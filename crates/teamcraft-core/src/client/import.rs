//! The import account's side of an import from another chat (e2ee.md §4.10, admin.md §3.8): its
//! messages are encrypted like any send, with the origin inside the ciphertext, and stored by the
//! server below the conversation's live messages. Used by `teamcraft-import`.

use std::collections::BTreeSet;

use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use teamcraft_crypto::message::encrypt_message;
use teamcraft_proto::{
  AdminSelfJoin, Base64Bytes, ConversationId, ConversationResponse, Id, ImportDmRequest,
  ImportMemberStateRequest, ImportMessagesRequest, ImportedMessage, MessageId, ProvisionUsersRequest,
  ProvisionedUser, ThreadId, UserId,
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

  /// Notes the keys of the import of `conversation`: those the ring holds now, once its history is
  /// in (at [`Self::finish_import`], or for a conversation an earlier version finished, on the next
  /// pass). A guest the import grant names gets these and nothing older than their join besides
  /// (e2ee.md §4.10). Kept once noted.
  pub fn note_import_keys(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let key = import_keys_key(conversation);
    if records::get_json::<BTreeSet<String>>(&self.store, Table::SyncState, &key)?.is_some() {
      return Ok(());
    }
    let keys: BTreeSet<String> = self
      .account
      .key_bag
      .ring(convert::conversation(conversation))
      .map(|ring| ring.keys().iter().map(|key| key_hex(key.id())).collect())
      .unwrap_or_default();
    records::put_json(&self.store, Table::SyncState, &key, &keys)?;
    Ok(())
  }

  /// The keys noted by [`Self::note_import_keys`], as hex IDs; empty when none were.
  pub(crate) fn import_keys(&self, conversation: ConversationId) -> Result<BTreeSet<String>, CoreError> {
    Ok(records::get_json(&self.store, Table::SyncState, &import_keys_key(conversation))?.unwrap_or_default())
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

  /// Encrypts `drafts` with the conversation's current content key and stores them as imported
  /// messages, oldest first; returns their server IDs in order. A draft sent before keeps its ID.
  /// Nothing is stored locally: the import device never reads its own history.
  pub async fn send_imported(
    &mut self,
    conversation: ConversationId,
    drafts: &[ImportDraft],
  ) -> Result<Vec<MessageId>, CoreError> {
    self.ensure_mls_current(conversation).await?;
    let Some(ring) = self
      .account
      .key_bag
      .ring(convert::conversation(conversation))
      .filter(|ring| ring.current().is_some())
    else {
      return Err(CoreError::InvalidRequest(
        "no content key for this conversation yet".into(),
      ));
    };
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
      let encrypted = encrypt_message(ring, &context, &plaintext, &self.account.device, &mut OsRng)?;
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
  /// notes the keys of the import ([`Self::note_import_keys`]) and, alone in the group, writes on
  /// with a fresh key ([`Self::rotate_after_import`]).
  pub async fn finish_import(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self.api.finish_import(conversation).await?;
    self.note_import_keys(conversation)?;
    self.rotate_after_import(conversation)
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

/// Where the keys of a conversation's import are kept ([`Client::note_import_keys`]).
fn import_keys_key(conversation: ConversationId) -> String {
  format!("import_keys/{conversation}")
}

/// Where the import grant of a conversation is kept ([`Client::grant_imported_history`]).
fn import_grant_key(conversation: ConversationId) -> String {
  format!("import_grant/{conversation}")
}
