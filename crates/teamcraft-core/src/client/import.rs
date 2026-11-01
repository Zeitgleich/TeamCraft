//! The import account's side of an import from another chat (e2ee.md §4.10, admin.md §3.8): its
//! messages are encrypted like any send, with the origin inside the ciphertext, and stored by the
//! server below the conversation's live messages. Used by `teamcraft-import`.

use std::collections::BTreeSet;

use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use teamcraft_crypto::content_key::{ContentKey, ContentKeyRing};
use teamcraft_crypto::message::encrypt_message;
use teamcraft_proto::{
  AdminSelfJoin, Base64Bytes, ConversationId, ConversationResponse, Id, ImportDmRequest,
  ImportMemberStateRequest, ImportMessagesRequest, ImportedMessage, ListMessagesQuery, MessageId,
  ProvisionUsersRequest, ProvisionedUser, Snowflake, ThreadId, UserId,
};
use zeroize::Zeroizing;

use super::Client;
use super::key_history::key_hex;
use super::server_changes::Observed;
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::imported::imported_message_id;
use crate::records;
use crate::store::{LocalStore, MessageContent, Table};

/// Messages per page when checking the keys an earlier version noted ([`Client::check_legacy_import_keys`]).
const LEGACY_CHECK_PAGE_SIZE: u32 = 200;

/// One message to import: its text with [`crate::store::MessageMeta::imported`] set, and the thread it answers.
#[derive(Clone, Debug)]
pub struct ImportDraft {
  pub content: MessageContent,
  /// The server ID of the imported thread root this message answers.
  pub maybe_thread_root: Option<MessageId>,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Records the import grant of `conversation` (e2ee.md §4.10, "Guests in imported
  /// conversations"): the import made it, and the import's mapping names `users` as its members or
  /// participants, so this device may share its history with them even if they are guests. Adds to
  /// what was recorded under the same policy date; under another one it starts afresh. Only the
  /// import device acts on it, and only under the signed import policy.
  pub fn grant_imported_history(
    &mut self,
    conversation: ConversationId,
    users: &[UserId],
  ) -> Result<(), CoreError> {
    let key = import_grant_key(conversation);
    let maybe_joins_until_ms = self.workspace()?.policy.import.maybe_joins_until_ms;
    let mut grant: ImportGrant = records::get_json(&self.store, Table::SyncState, &key)?
      .filter(|grant: &ImportGrant| grant.maybe_joins_until_ms == maybe_joins_until_ms)
      .unwrap_or(ImportGrant {
        maybe_joins_until_ms,
        users: BTreeSet::new(),
      });
    let before = grant.users.len();
    grant
      .users
      .extend(users.iter().copied().filter(|user| *user != self.account.user_id));
    if grant.users.len() != before {
      records::put_json(&self.store, Table::SyncState, &key, &grant)?;
    }
    Ok(())
  }

  /// Records that the import's mapping names `users` as members of `conversation`, a channel the
  /// import merges into (it gets no import grant): those who aren't guests get its import key
  /// ([`Self::is_import_reader`]). Adds to what was recorded.
  pub fn name_import_readers(
    &mut self,
    conversation: ConversationId,
    users: &[UserId],
  ) -> Result<(), CoreError> {
    let key = import_readers_key(conversation);
    let mut readers: BTreeSet<UserId> =
      records::get_json(&self.store, Table::SyncState, &key)?.unwrap_or_default();
    let before = readers.len();
    readers.extend(users.iter().copied().filter(|user| *user != self.account.user_id));
    if readers.len() != before {
      records::put_json(&self.store, Table::SyncState, &key, &readers)?;
    }
    Ok(())
  }

  /// The keys of the import of `conversation`, as hex IDs: its import keys
  /// ([`Self::import_content_key`]) and the keys an earlier version noted that only this device's
  /// messages are under ([`Self::check_legacy_import_keys`]).
  pub(crate) fn import_keys(&self, conversation: ConversationId) -> Result<BTreeSet<String>, CoreError> {
    let maybe_record: Option<ImportKey> =
      records::get_json(&self.store, Table::SyncState, &import_key_key(conversation))?;
    Ok(
      maybe_record
        .map(|record| record.ids.into_iter().chain(record.legacy).collect())
        .unwrap_or_default(),
    )
  }

  /// Checks the keys an earlier version noted for the import of `conversation` (#1078: every key
  /// its ring held, live ones included) against the conversation's messages: a key stays one of
  /// the import's only while no message of another device is under it (e2ee.md §4.10, "Earlier
  /// versions"). The first call takes over the old note, keeping the keys the ring still holds;
  /// each call then reads the messages after the newest one checked, so a key a member writes
  /// under later drops out before it is shared again. A refused listing drops them all; a network
  /// error is returned, and the keys are checked again next time. Nothing to do without a group.
  pub(crate) async fn check_legacy_import_keys(
    &mut self,
    conversation: ConversationId,
  ) -> Result<(), CoreError> {
    if !self.has_group(conversation)? {
      return Ok(());
    }
    let record_key = import_key_key(conversation);
    let mut record: ImportKey =
      records::get_json(&self.store, Table::SyncState, &record_key)?.unwrap_or_default();
    let old_key = legacy_import_keys_key(conversation);
    if let Some(noted) = records::get_json::<BTreeSet<String>>(&self.store, Table::SyncState, &old_key)? {
      let held: BTreeSet<String> = self
        .ring_keys_hex(conversation)
        .into_iter()
        .filter(|id| noted.contains(id))
        .collect();
      record.legacy.extend(held);
      record.maybe_legacy_checked_through = None;
      records::put_json(&self.store, Table::SyncState, &record_key, &record)?;
      self.store.delete(Table::SyncState, &old_key)?;
    }
    if record.legacy.is_empty() {
      return Ok(());
    }
    let before = record.legacy.len();
    let mut maybe_after = record.maybe_legacy_checked_through;
    loop {
      let query = ListMessagesQuery {
        maybe_before: None,
        maybe_after: Some(maybe_after.unwrap_or(MessageId::from_snowflake(Snowflake::ZERO))),
        maybe_limit: Some(LEGACY_CHECK_PAGE_SIZE),
      };
      let page = match self.api.list_messages(conversation, &query).await {
        Ok(page) => page,
        Err(error) if error.is_retry_later() => return Err(error.into()),
        Err(error) => {
          tracing::warn!(conversation_id = %conversation, keys = record.legacy.len(), %error, "can't check the keys an earlier version noted for the import; they are not shared");
          record.legacy.clear();
          break;
        },
      };
      for message in &page.messages {
        if message.sender_device_id != self.account.device_id {
          let id: String = message
            .content_key_id
            .as_slice()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
          record.legacy.remove(&id);
        }
        maybe_after = maybe_after.max(Some(message.id));
      }
      if !page.has_more || page.messages.is_empty() || record.legacy.is_empty() {
        break;
      }
    }
    if record.legacy.len() < before {
      tracing::warn!(conversation_id = %conversation, keys = before - record.legacy.len(), "keys an earlier version noted for the import have members' messages under them; they are not shared");
    }
    record.maybe_legacy_checked_through = maybe_after;
    records::put_json(&self.store, Table::SyncState, &record_key, &record)?;
    Ok(())
  }

  /// The IDs of the keys of the conversation's ring, as hex.
  fn ring_keys_hex(&self, conversation: ConversationId) -> Vec<String> {
    self
      .account
      .key_bag
      .ring(convert::conversation(conversation))
      .map(|ring| ring.keys().iter().map(|key| key_hex(key.id())).collect())
      .unwrap_or_default()
  }

  /// Whether something other than the server names `user` as somebody who may read the import of
  /// `conversation`: the import's mapping (its grant, or the readers of a merged channel,
  /// [`Self::name_import_readers`]) or the verified roster. Guests among them are left out by the
  /// caller, on the server's word, which can only lower what is shared.
  pub(crate) fn is_import_reader(
    &self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<bool, CoreError> {
    let maybe_grant: Option<ImportGrant> =
      records::get_json(&self.store, Table::SyncState, &import_grant_key(conversation))?;
    if maybe_grant.is_some_and(|grant| grant.users.contains(&user)) {
      return Ok(true);
    }
    let maybe_readers: Option<BTreeSet<UserId>> =
      records::get_json(&self.store, Table::SyncState, &import_readers_key(conversation))?;
    if maybe_readers.is_some_and(|readers| readers.contains(&user)) {
      return Ok(true);
    }
    Ok(
      self
        .workspace()?
        .roster
        .members
        .iter()
        .any(|member| member.user_id == user),
    )
  }

  /// Whether this device may share the history of `conversation` with `user` although the user
  /// is a guest: this account is the import account of the verified policy, whose date hasn't
  /// passed, the verified workspace state is fresh (its roster younger than `workspace_max_age`)
  /// and raised no alarm, both by the later of this device's clock and the newest server time it
  /// saw, and the import grant of the conversation names the user under this very policy date
  /// ([`Self::grant_imported_history`]).
  pub(crate) fn is_import_granted(
    &self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<bool, CoreError> {
    let state = self.workspace()?;
    let policy = &state.policy.import;
    let newest_server_ms = self
      .sync
      .mls_cursors
      .values()
      .map(|cursor| cursor.snowflake().unix_millis())
      .max()
      .unwrap_or(i64::MIN);
    let now_ms = self.now_ms().max(newest_server_ms);
    let max_age_ms = i64::try_from(self.settings.workspace_max_age.as_millis()).unwrap_or(i64::MAX);
    if !policy.allows(self.account.user_id, now_ms)
      || state.maybe_alarm.is_some()
      || !state.is_roster_fresh(now_ms, max_age_ms)
    {
      return Ok(false);
    }
    let maybe_grant: Option<ImportGrant> =
      records::get_json(&self.store, Table::SyncState, &import_grant_key(conversation))?;
    Ok(maybe_grant.is_some_and(|grant| {
      grant.maybe_joins_until_ms == policy.maybe_joins_until_ms && grant.users.contains(&user)
    }))
  }

  /// Creates the accounts of `usernames` from the identity provider (admin.md §3.8).
  pub async fn provision_users(&self, usernames: Vec<String>) -> Result<Vec<ProvisionedUser>, CoreError> {
    let response = self
      .api
      .provision_users(&ProvisionUsersRequest { usernames })
      .await?;
    Ok(response.users)
  }

  /// The DM of `participants` (two people), or one person's notes, with this account as a member
  /// that is no participant. Stored like any conversation this device learns of.
  pub async fn open_import_dm(
    &mut self,
    participants: Vec<UserId>,
  ) -> Result<ConversationResponse, CoreError> {
    let response = self.api.import_dm(&ImportDmRequest { participants }).await?;
    self.store_conversation(&response, Observed::FromServer { maybe_head: None })?;
    Ok(response)
  }

  /// Sets up the encryption group of an import DM ([`Self::open_import_dm`]): founded by this
  /// device with the participants, who get the full history, unless somebody founded it already.
  /// A participant without a device yet waits as a pending addition (e2ee.md §4.3). `true` once
  /// this device is in the group.
  pub async fn set_up_import_dm(
    &mut self,
    conversation: ConversationResponse,
    participants: &[UserId],
  ) -> Result<bool, CoreError> {
    let id = self.found_participants_group(conversation, participants).await?;
    self.has_group(id)
  }

  /// Asks to be let into the group of a DM or notes the import didn't found (it existed before,
  /// e2ee.md §4.10): posts this account's signed self-addition, from the join on, which a
  /// participant's device commits like a workspace admin's self-join (admin.md §2.4: only under
  /// `admin_self_join = visible`). Posted again while nobody lets it in
  /// ([`Self::retry_due_self_adds`]). Nothing to do once this device is in the group.
  pub async fn request_import_join(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    if self.has_group(conversation)? {
      return Ok(());
    }
    let self_join = self.workspace()?.policy.admin_self_join.clone();
    if self_join != AdminSelfJoin::Visible {
      tracing::warn!(
        conversation_id = %conversation,
        policy = %self_join,
        "members' devices let the import account into an existing DM or notes only while admin_self_join is visible"
      );
    }
    self.post_self_add_change(conversation).await
  }

  /// Encrypts `drafts` with the conversation's import key ([`Self::import_content_key`]), sealed
  /// to the members first, and stores them as imported messages, oldest first; returns their
  /// server IDs in order. A draft sent before keeps its ID.
  /// Nothing is stored locally: the import device never reads its own history.
  pub async fn send_imported(
    &mut self,
    conversation: ConversationId,
    drafts: &[ImportDraft],
  ) -> Result<Vec<MessageId>, CoreError> {
    self.ensure_mls_current(conversation).await?;
    if !self.has_group(conversation)? {
      return Err(CoreError::InvalidRequest(
        "not in this conversation's group yet".into(),
      ));
    }
    let key = self.import_content_key(conversation)?;
    self.share_import_key(conversation).await?;
    // The import key is current only in this ring, which encrypts nothing else.
    let mut ring = ContentKeyRing::new(convert::conversation(conversation));
    ring.insert(key.duplicate())?;
    ring.set_current(key.id())?;
    let mut messages = Vec::with_capacity(drafts.len());
    for draft in drafts {
      let MessageContent::Text { meta, .. } = &draft.content else {
        return Err(CoreError::InvalidRequest(
          "only text messages are imported".into(),
        ));
      };
      let Some(origin) = &meta.maybe_imported else {
        return Err(CoreError::InvalidRequest(
          "an imported message needs its origin".into(),
        ));
      };
      let client_message_id = imported_message_id(conversation, origin.source, &origin.source_id);
      let plaintext = Zeroizing::new(
        serde_json::to_vec(&draft.content).map_err(|error| CoreError::Corrupt(error.to_string()))?,
      );
      let context = self.message_context(conversation, client_message_id);
      let encrypted = encrypt_message(&ring, &context, &plaintext, &self.account.device, &mut OsRng)?;
      messages.push(ImportedMessage {
        client_message_id,
        content_key_id: Base64Bytes::new(encrypted.content_key_id().as_bytes().to_vec()),
        ciphertext: Base64Bytes::new(encrypted.to_bytes()?),
        maybe_thread_id: draft
          .maybe_thread_root
          .map(|root| ThreadId::from_snowflake(root.snowflake())),
        sent_at_ms: origin.sent_at_ms,
      });
    }
    let response = self
      .api
      .import_messages(
        conversation,
        &ImportMessagesRequest {
          device_id: self.account.device_id,
          messages,
        },
      )
      .await?;
    Ok(response.messages.into_iter().map(|stored| stored.id).collect())
  }

  /// The content key the import of `conversation` encrypts with (e2ee.md §4.10, "The import
  /// key"): made by this device for the import alone and never anybody's current key, so nobody
  /// writes a live message under it. A guest the import grant names gets the import keys and no
  /// other one from before their join. A new one is made when the group changed (another epoch)
  /// since the last one was used: a device that was in the group meanwhile and left keeps that
  /// key, but reads nothing imported after it left. Also when the ring lost it. The earlier ones
  /// stay import keys.
  fn import_content_key(&mut self, conversation: ConversationId) -> Result<ContentKey, CoreError> {
    let epoch = self.group_or_error(conversation)?.epoch();
    let record_key = import_key_key(conversation);
    let mut record: ImportKey =
      records::get_json(&self.store, Table::SyncState, &record_key)?.unwrap_or_default();
    let ring_id = convert::conversation(conversation);
    let maybe_key = record.ids.last().and_then(|id| {
      self
        .account
        .key_bag
        .ring(ring_id)
        .and_then(|ring| ring.keys().iter().find(|key| key_hex(key.id()) == *id))
        .map(ContentKey::duplicate)
    });
    match maybe_key {
      Some(key) if record.epoch == epoch => return Ok(key),
      Some(_) => {
        tracing::info!(conversation_id = %conversation, "the group changed during the import: imported history goes on under a new import key");
      },
      None if !record.ids.is_empty() => {
        tracing::warn!(conversation_id = %conversation, "the import key is missing from the ring: imported history goes on under a new one");
      },
      None => {},
    }
    let fresh = ContentKey::generate(&mut OsRng);
    self.account.key_bag.ring_mut(ring_id).insert(fresh.duplicate())?;
    self.persist_keys()?;
    record.ids.push(key_hex(fresh.id()));
    record.sealed_to.clear();
    record.epoch = epoch;
    records::put_json(&self.store, Table::SyncState, &record_key, &record)?;
    Ok(fresh)
  }

  /// Seals the import key of `conversation` to the group's member devices that may read the
  /// import and don't have the key from this device yet ([`Self::seal_import_key`]), and notes
  /// them. Before every batch, when the import is finished and on every later pass, so a device
  /// that joined since gets it too. Nothing to do without an import key or a group.
  pub async fn share_import_key(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self.check_legacy_import_keys(conversation).await?;
    if !self.has_group(conversation)? {
      return Ok(());
    }
    let record_key = import_key_key(conversation);
    let Some(mut record) = records::get_json::<ImportKey>(&self.store, Table::SyncState, &record_key)? else {
      return Ok(());
    };
    let Some(key) = record.ids.last().and_then(|id| {
      self
        .account
        .key_bag
        .ring(convert::conversation(conversation))
        .and_then(|ring| ring.keys().iter().find(|key| key_hex(key.id()) == *id))
        .map(ContentKey::duplicate)
    }) else {
      return Ok(());
    };
    let sealed = self
      .seal_import_key(conversation, &key, &record.sealed_to)
      .await?;
    if !sealed.is_empty() {
      record.sealed_to.extend(sealed);
      records::put_json(&self.store, Table::SyncState, &record_key, &record)?;
    }
    Ok(())
  }

  /// A member's read position, favourite and mute from the old system.
  pub async fn set_import_member_state(
    &self,
    conversation: ConversationId,
    user: UserId,
    state: &ImportMemberStateRequest,
  ) -> Result<(), CoreError> {
    Ok(
      self
        .api
        .set_import_member_state(conversation, user, state)
        .await?,
    )
  }

  /// Marks the conversation's import as finished, so its members load its history's start again,
  /// and seals the import key to member devices that joined since the last batch
  /// ([`Self::share_import_key`]).
  pub async fn finish_import(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self.api.finish_import(conversation).await?;
    self.share_import_key(conversation).await
  }

  /// Leaves a conversation the import is done with, without a goodbye: a signed removal of this
  /// user that a member commits, then the server's membership (also of a DM or notes, e2ee.md
  /// §4.10). `Ok` while it waits for the members' commit; the pending queue finishes it.
  pub async fn leave_after_import(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let me = self.account.user_id;
    self.api.check_member_removal(conversation, me).await?;
    self.record_and_run_removal(conversation, me, None).await
  }
}

/// The import grant of a conversation ([`Client::grant_imported_history`]): the people the mapping
/// names there, under the import policy's date when it was recorded. A later policy (another date,
/// also a renewed one) starts a new grant, which only the import tool records again.
#[derive(Debug, Serialize, Deserialize)]
struct ImportGrant {
  #[serde(rename = "joins_until_ms")]
  maybe_joins_until_ms: Option<i64>,
  users: BTreeSet<UserId>,
}

/// The import keys of a conversation ([`Client::import_content_key`]) and who has the last one.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ImportKey {
  /// The keys' IDs in the ring, as hex, oldest first; the last one encrypts.
  ids: Vec<String>,
  /// Member devices (`user/device`) this device sealed the last key to.
  sealed_to: BTreeSet<String>,
  /// The group's epoch when the last key was made: in another one, the next batch takes a new key.
  epoch: u64,
  /// Keys an earlier version noted for the import, as hex, under which no message of another
  /// device was found so far ([`Client::check_legacy_import_keys`]).
  #[serde(default)]
  legacy: BTreeSet<String>,
  /// The newest message checked for [`Self::legacy`].
  #[serde(rename = "legacy_checked_through", default)]
  maybe_legacy_checked_through: Option<MessageId>,
}

/// Where the import keys of a conversation are kept ([`Client::import_content_key`]).
fn import_key_key(conversation: ConversationId) -> String {
  format!("import_key/{conversation}")
}

/// Where earlier versions noted every key of a conversation's ring at the end of its import (#1078),
/// taken over by [`Client::check_legacy_import_keys`].
fn legacy_import_keys_key(conversation: ConversationId) -> String {
  format!("import_keys/{conversation}")
}

/// Where the readers of a merged channel's import are kept ([`Client::name_import_readers`]).
fn import_readers_key(conversation: ConversationId) -> String {
  format!("import_readers/{conversation}")
}

/// Where the import grant of a conversation is kept ([`Client::grant_imported_history`]).
fn import_grant_key(conversation: ConversationId) -> String {
  format!("import_grant/{conversation}")
}
