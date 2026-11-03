//! The server says a conversation was deleted (e2ee.md §4.6, "Deleting a channel"). Without a
//! signature that is its word alone, and a device's local copy may be the last copy there is. So the
//! claim hides the conversation and drops its MLS secrets, as a removal does, but the messages stay
//! for `deleted_conversation_retention` and only then go. Only a deletion the user confirmed on this
//! device, or one signed by somebody this device verifies may delete the channel
//! ([`crate::signed_deletion`]), goes at once. Notes to self are never purged on a claim. Many claims
//! at once are suspect, and the user is told once. The server listing the conversation again lifts
//! the quarantine.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{
  ConversationId, ConversationKind, ConversationResponse, DeleteConversationRequest, DeleteSpaceRequest,
  MAX_SIGNED_DELETIONS, SignedChannelDeletion, SpaceId,
};

use super::Client;
use super::workspace::current_epoch_of;
use crate::api::{ApiError, ServerApi};
use crate::error::CoreError;
use crate::records;
use crate::signed_deletion::{
  DeletionVerifier, GroupFacts, can_sign_deletion, sign_channel_deletion, verify_signed_deletion,
};
use crate::store::{LocalStore, MessageFilter, Table};

const QUARANTINE_PREFIX: &str = "quarantine/";
const OWN_DELETION_PREFIX: &str = "own_deletion/";
const NOTES_PREFIX: &str = "notes_to_self/";
pub(crate) const SUSPECT_DELETIONS_KEY: &str = "suspect_deletions";
/// Tries with fresh epochs of a signed deletion: a commit may come in between, and an admin outside
/// a group learns its epoch from the first refusal.
const DELETION_ATTEMPTS: usize = 3;

/// A conversation the server claimed deleted, hidden here until its retention ran out.
#[derive(Debug, Serialize, Deserialize)]
struct Quarantined {
  conversation: ConversationId,
  since_ms: i64,
}

/// Many claims at once: what the user is told once.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SuspectDeletions {
  pub count: u32,
  pub at_ms: i64,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// The gateway's `conversation_delete`. A claim about a conversation the server still shows this
  /// user is forged or replayed, signed or not, and changes nothing.
  pub(crate) async fn note_deletion_event(
    &mut self,
    conversation: ConversationId,
    maybe_signed: Option<SignedChannelDeletion>,
  ) -> Result<(), CoreError> {
    if !self.has_own_deletion(conversation)? {
      match self.api.conversation(conversation).await {
        Ok(response) if response.maybe_my_membership.is_some() => {
          tracing::warn!(conversation_id = %conversation, "the server claims a deletion of a conversation it still lists; ignored");
          return Ok(());
        },
        // Not reachable now: hiding it is safe, and the next sync lifts it if it was wrong.
        _ => {},
      }
    }
    if maybe_signed.is_some()
      && let Err(error) = self.sync_workspace().await
    {
      // A sync has just read it; here the event may have come long after the last read.
      tracing::warn!(conversation_id = %conversation, %error, "judging a signed deletion on the roster this device has");
    }
    self
      .note_claimed_deletion(conversation, maybe_signed.as_ref())
      .await
  }

  /// A deletion the server claims, with the signature it came with, if any. One this device verifies
  /// makes it purge at once; anything else stays a claim ([`Self::note_server_deletion`]). Replays
  /// cost nothing: a device that no longer has the group judges nothing, and asks nobody.
  pub(crate) async fn note_claimed_deletion(
    &mut self,
    conversation: ConversationId,
    maybe_signed: Option<&SignedChannelDeletion>,
  ) -> Result<(), CoreError> {
    if let Some(signed) = maybe_signed
      && !self.has_own_deletion(conversation)?
    {
      match self.verifies_signed_deletion(conversation, signed).await {
        // The same mark as the user's own deletion: a purge cut off is finished by the next claim,
        // though the group that made it verifiable is gone by then.
        Ok(true) => self.note_own_deletion(conversation)?,
        Ok(false) => {},
        // A group this device can't read decides nothing, and doesn't stop the sync.
        Err(error) => {
          tracing::warn!(conversation_id = %conversation, %error, "could not judge a signed deletion; it stays the server's word");
        },
      }
    }
    self.note_server_deletion(conversation)
  }

  /// Whether a signed deletion holds on this device (`crate::signed_deletion`). Reads the channel's
  /// role log first: the signer's role may be newer than this device's copy, and the server keeps a
  /// signed-deleted channel's log for that. If the read fails the device judges on what it has, which
  /// can only refuse: a real deletion then takes the retention.
  async fn verifies_signed_deletion(
    &mut self,
    conversation: ConversationId,
    signed: &SignedChannelDeletion,
  ) -> Result<bool, CoreError> {
    if !self.has_group(conversation)?
      || self
        .store
        .get(Table::AppliedChanges, &notes_key(conversation))?
        .is_some()
    {
      return Ok(false);
    }
    let maybe_known: Option<ConversationResponse> =
      records::get_json(&self.store, Table::Conversations, &conversation.to_string())?;
    if let Err(error) = self.sync_channel_roles(conversation).await {
      tracing::warn!(conversation_id = %conversation, %error, "judging a signed deletion on the roles this device has");
    }
    let roles = self.channel_role_state(conversation)?;
    let workspace = self.workspace()?;
    let group_users = self.group_users(conversation)?;
    let epoch = self.group_epoch(conversation)?;
    let verifier = DeletionVerifier {
      conversation,
      is_channel: maybe_known.is_some_and(|known| known.kind == ConversationKind::Channel),
      maybe_group: epoch.map(|epoch| GroupFacts {
        epoch,
        users: &group_users,
      }),
      roles: &roles,
      workspace: &workspace,
      roster_max_age_ms: self.roster_max_age_ms(),
      now_ms: self.now_ms(),
    };
    match verify_signed_deletion(signed, &verifier) {
      Ok(verified) => {
        tracing::info!(
          conversation_id = %conversation,
          signer = %verified.signer,
          is_workspace_admin = verified.is_workspace_admin,
          "a signed deletion holds; the history goes at once"
        );
        Ok(true)
      },
      Err(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "a signed deletion doesn't hold; it stays the server's word");
        Ok(false)
      },
    }
  }

  /// Asks the server to delete the channel, signed if this user can sign in a way members accept. The
  /// signature names the group's current epoch, which the server tells this device if it doesn't
  /// know it (it's outside the group, or behind). Anything that goes wrong with signing leaves the
  /// deletion unsigned: the user asked for it, and the members' devices just keep the history longer.
  pub(crate) async fn request_channel_deletion(
    &mut self,
    conversation: ConversationId,
  ) -> Result<(), ApiError> {
    // An admin outside the channel knows it only from the server; the members' devices check.
    let is_channel = self.is_known_channel(conversation)
      || self
        .api
        .conversation(conversation)
        .await
        .is_ok_and(|response| response.kind == ConversationKind::Channel);
    if is_channel {
      self.refresh_deletion_authority(conversation).await;
    }
    let mut epoch = self.known_group_epoch(conversation);
    let mut attempts = 1;
    loop {
      let request = DeleteConversationRequest {
        maybe_signed_deletion: if is_channel {
          self.signed_deletion_if_allowed(conversation, epoch)
        } else {
          None
        },
      };
      let answer = self.api.delete_conversation(conversation, &request).await;
      match answer {
        Err(error) if error.is_epoch_conflict() && attempts < DELETION_ATTEMPTS => {
          let Some(current) = current_epoch_of(&error) else {
            return Err(error);
          };
          epoch = current;
          attempts += 1;
        },
        answer => return answer,
      }
    }
  }

  /// [`Self::request_channel_deletion`] for a space. `channels`: those this device knows in it. Only
  /// they are signed, never what the server names: it could name any channel of the workspace, or a
  /// direct message, to a workspace admin. Others go as the server's word.
  pub(crate) async fn request_space_deletion(
    &mut self,
    space: SpaceId,
    channels: &[ConversationId],
  ) -> Result<(), ApiError> {
    let mut epochs = Vec::with_capacity(channels.len());
    for channel in channels {
      if !self.is_known_channel(*channel) {
        continue;
      }
      self.refresh_deletion_authority(*channel).await;
      epochs.push((*channel, self.known_group_epoch(*channel)));
    }
    self.read_space_epochs(space, &mut epochs).await;
    let mut attempts = 1;
    loop {
      let signed_deletions = epochs
        .iter()
        .filter_map(|(channel, epoch)| self.signed_deletion_if_allowed(*channel, *epoch))
        .take(MAX_SIGNED_DELETIONS)
        .collect();
      let request = DeleteSpaceRequest { signed_deletions };
      let answer = self.api.delete_space(space, &request).await;
      match answer {
        Err(error) if error.is_epoch_conflict() && attempts < DELETION_ATTEMPTS => {
          self.read_space_epochs(space, &mut epochs).await;
          attempts += 1;
        },
        answer => return answer,
      }
    }
  }

  /// The groups' epochs as the server says, for the channels to sign. An older server has no such
  /// read, and takes no signatures either; a failed read shows in the request that follows.
  async fn read_space_epochs(&self, space: SpaceId, epochs: &mut [(ConversationId, u64)]) {
    match self.api.space_channel_epochs(space).await {
      Ok(response) => {
        for told in response.channels {
          if let Some((_, epoch)) = epochs
            .iter_mut()
            .find(|(channel, _)| *channel == told.conversation_id)
          {
            *epoch = told.epoch;
          }
        }
      },
      Err(error) => {
        tracing::debug!(space_id = %space, %error, "could not read the groups' epochs before deleting a space");
      },
    }
  }

  /// A signature of this device on a channel's deletion: what it would send if it was allowed to,
  /// without any check. What members make of it is up to them.
  pub fn sign_channel_deletion(
    &self,
    conversation: ConversationId,
    epoch: u64,
  ) -> Result<SignedChannelDeletion, CoreError> {
    sign_channel_deletion(
      &self.account.device,
      &self.account.certificate,
      conversation,
      epoch,
      self.now_ms(),
    )
  }

  /// The roster and the channel's roles may be newer on the server when this device can't sign yet.
  /// Failing to read them only leaves the deletion unsigned.
  async fn refresh_deletion_authority(&mut self, conversation: ConversationId) {
    if self.can_sign_deletion_of(conversation) {
      return;
    }
    if let Err(error) = self.sync_workspace().await {
      tracing::debug!(conversation_id = %conversation, %error, "could not read the roster before signing a deletion");
    }
    if self.can_sign_deletion_of(conversation) {
      return;
    }
    if let Err(error) = self.sync_channel_roles(conversation).await {
      tracing::debug!(conversation_id = %conversation, %error, "could not read the roles before signing a deletion");
    }
  }

  fn signed_deletion_if_allowed(
    &self,
    conversation: ConversationId,
    epoch: u64,
  ) -> Option<SignedChannelDeletion> {
    if !self.can_sign_deletion_of(conversation) {
      tracing::info!(conversation_id = %conversation, "this user holds no signed authority here; the deletion stays unsigned");
      return None;
    }
    match self.sign_channel_deletion(conversation, epoch) {
      Ok(signed) => Some(signed),
      Err(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "could not sign a deletion; it stays unsigned");
        None
      },
    }
  }

  fn is_known_channel(&self, conversation: ConversationId) -> bool {
    records::get_json::<ConversationResponse>(&self.store, Table::Conversations, &conversation.to_string())
      .ok()
      .flatten()
      .is_some_and(|known| known.kind == ConversationKind::Channel)
  }

  /// Whether this user can sign the channel's deletion in a way every member can check.
  fn can_sign_deletion_of(&self, conversation: ConversationId) -> bool {
    let (Ok(roles), Ok(workspace)) = (self.channel_role_state(conversation), self.workspace()) else {
      return false;
    };
    let authorities = roles.signing_authorities(
      self.account.user_id,
      &workspace,
      self.now_ms(),
      self.roster_max_age_ms(),
    );
    can_sign_deletion(&authorities)
  }

  /// The group's epoch as this device knows it; 0 without a group, and the server's refusal says.
  fn known_group_epoch(&mut self, conversation: ConversationId) -> u64 {
    self.group_epoch(conversation).ok().flatten().unwrap_or_default()
  }

  /// The epoch of this device's group of the conversation, if it has one.
  pub fn group_epoch(&mut self, conversation: ConversationId) -> Result<Option<u64>, CoreError> {
    Ok(self.group(conversation)?.map(|group| group.epoch()))
  }

  /// A deletion the server claims with no signature that holds, from the gateway or a sync's
  /// `deleted_conversation_ids`. Replays change nothing: the retention runs from the first claim.
  pub(crate) fn note_server_deletion(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    if self.has_own_deletion(conversation)? {
      return self.purge_conversation(conversation);
    }
    if self
      .store
      .get(Table::AppliedChanges, &notes_key(conversation))?
      .is_some()
    {
      tracing::warn!(conversation_id = %conversation, "the server claims the notes to self were deleted; they stay");
      return Ok(());
    }
    let key = quarantine_key(conversation);
    if self.store.get(Table::AppliedChanges, &key)?.is_some() {
      return Ok(());
    }
    let is_known = self
      .store
      .get(Table::Conversations, &conversation.to_string())?
      .is_some()
      || !self
        .store
        .messages(conversation, MessageFilter::TopLevel)?
        .is_empty();
    if !is_known {
      return Ok(());
    }
    let entry = Quarantined {
      conversation,
      since_ms: self.now_ms(),
    };
    records::put_json(&self.store, Table::AppliedChanges, &key, &entry)?;
    self.forget_conversation(conversation)?;
    self.drop_pending_work(conversation)?;
    tracing::info!(conversation_id = %conversation, "the server says this conversation was deleted; hidden, its messages stay for now");
    self.note_suspect_deletions()
  }

  /// Marks that this conversation may purge at once: the user confirmed deleting it here (set before
  /// the request: the answer can come on the gateway first), or a signed deletion held here.
  pub(crate) fn note_own_deletion(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self
      .store
      .put(Table::AppliedChanges, &own_deletion_key(conversation), b"1")?;
    Ok(())
  }

  pub(crate) fn has_own_deletion(&self, conversation: ConversationId) -> Result<bool, CoreError> {
    Ok(
      self
        .store
        .get(Table::AppliedChanges, &own_deletion_key(conversation))?
        .is_some(),
    )
  }

  /// The server lists these conversations: a quarantine of them was wrong, or is over.
  pub(crate) fn lift_quarantines(&mut self, listed: &[ConversationResponse]) -> Result<(), CoreError> {
    for (key, _) in self.store.list(Table::AppliedChanges)? {
      let Some(id) = key.strip_prefix(QUARANTINE_PREFIX) else {
        continue;
      };
      if listed
        .iter()
        .any(|conversation| conversation.id.to_string() == id)
      {
        self.store.delete(Table::AppliedChanges, &key)?;
      }
    }
    Ok(())
  }

  /// Purges what stayed hidden past its retention. Run with a full sync.
  pub(crate) fn purge_expired_quarantines(&mut self) -> Result<(), CoreError> {
    let retention_ms =
      i64::try_from(self.settings.deleted_conversation_retention.as_millis()).unwrap_or(i64::MAX);
    let now = self.now_ms();
    for entry in self.quarantined()? {
      if now.saturating_sub(entry.since_ms) >= retention_ms {
        self.purge_conversation(entry.conversation)?;
      }
    }
    Ok(())
  }

  /// Keeps the fact that a conversation is the user's notes: the server can't take it back later by
  /// making the record go first.
  pub(crate) fn remember_notes_to_self(&self, conversation: ConversationId) -> Result<(), CoreError> {
    let key = notes_key(conversation);
    if self.store.get(Table::AppliedChanges, &key)?.is_none() {
      self.store.put(Table::AppliedChanges, &key, b"1")?;
    }
    Ok(())
  }

  pub fn dismiss_suspect_deletions(&mut self) -> Result<(), CoreError> {
    Ok(self.store.delete(Table::AppliedChanges, SUSPECT_DELETIONS_KEY)?)
  }

  fn quarantined(&self) -> Result<Vec<Quarantined>, CoreError> {
    let mut found = Vec::new();
    for (key, bytes) in self.store.list(Table::AppliedChanges)? {
      if key.starts_with(QUARANTINE_PREFIX)
        && let Ok(entry) = serde_json::from_slice::<Quarantined>(&bytes)
      {
        found.push(entry);
      }
    }
    Ok(found)
  }

  /// Counts over a window, not per sync, so claims one by one on the gateway add up too.
  fn note_suspect_deletions(&mut self) -> Result<(), CoreError> {
    let window_ms = i64::try_from(self.settings.suspect_deletion_window.as_millis()).unwrap_or(i64::MAX);
    let now = self.now_ms();
    let recent = self
      .quarantined()?
      .iter()
      .filter(|entry| now.saturating_sub(entry.since_ms) <= window_ms)
      .count();
    let total = self
      .store
      .list(Table::Conversations)?
      .len()
      .saturating_add(recent);
    let is_suspect = recent > self.settings.suspect_deletion_count
      || (recent >= 2
        && recent.saturating_mul(100) > total.saturating_mul(self.settings.suspect_deletion_percent));
    if !is_suspect {
      return Ok(());
    }
    tracing::warn!(
      recent,
      total,
      "the server claims many conversations were deleted at once; kept hidden"
    );
    records::put_json(
      &self.store,
      Table::AppliedChanges,
      SUSPECT_DELETIONS_KEY,
      &SuspectDeletions {
        count: u32::try_from(recent).unwrap_or(u32::MAX),
        at_ms: now,
      },
    )
  }
}

fn quarantine_key(conversation: ConversationId) -> String {
  format!("{QUARANTINE_PREFIX}{conversation}")
}

fn own_deletion_key(conversation: ConversationId) -> String {
  format!("{OWN_DELETION_PREFIX}{conversation}")
}

fn notes_key(conversation: ConversationId) -> String {
  format!("{NOTES_PREFIX}{conversation}")
}

/// The markers a purge removes with the conversation.
pub(crate) fn purge_markers(conversation: ConversationId) -> [String; 2] {
  [quarantine_key(conversation), own_deletion_key(conversation)]
}
