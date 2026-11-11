//! The server says a conversation was deleted (e2ee.md §4.6, "Deleting a channel"). Without a
//! signature that is its word alone, and a device's local copy may be the last copy there is. So the
//! claim hides the conversation and drops its MLS secrets, as a removal does, but the messages stay
//! for `deleted_conversation_retention` and only then go. Only a deletion the user confirmed on this
//! device, or one signed by somebody this device verifies may delete the channel
//! ([`crate::signed_deletion`]), goes at once. Notes to self are never purged on a claim. Many claims
//! at once are suspect, and the user is told once. The server listing the conversation again lifts
//! the quarantine.

use serde::{Deserialize, Serialize};
use teamcraft_crypto::channel_anchor::name_hash;
use teamcraft_proto::{
  ConversationId, ConversationKind, ConversationResponse, DeleteConversationRequest, SignedChannelDeletion,
  SpaceId,
};

use super::Client;
use super::workspace::current_epoch_of;
use crate::api::{ApiError, ServerApi};
use crate::error::CoreError;
use crate::records;
use crate::signed_deletion::{
  DeletionVerifier, GroupFacts, can_sign_deletion, is_told_epoch_plausible, sign_channel_deletion,
  verify_signed_deletion,
};
use crate::store::{LocalStore, MessageFilter, Table};

const QUARANTINE_PREFIX: &str = "quarantine/";
const OWN_DELETION_PREFIX: &str = "own_deletion/";
const NOTES_PREFIX: &str = "notes_to_self/";
const DIRECT_PREFIX: &str = "direct/";
/// When deletions verified here purged, within `suspect_deletion_window`: they leave no other mark.
const SIGNED_PURGES_KEY: &str = "signed_purges";
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
        Ok(true) => {
          self.note_signed_purge()?;
          // The same mark as the user's own deletion: a purge cut off is finished by the next claim,
          // though the group that made it verifiable is gone by then.
          self.note_own_deletion(conversation)?;
          self.note_server_deletion(conversation)?;
          // A real signature on many channels at once is worth telling too.
          return self.note_suspect_deletions();
        },
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
  /// signed-deleted channel's log for that. If the read fails, or the server leaves entries out, the
  /// device judges on what it has: a promotion it missed refuses, and a demotion it missed still
  /// accepts (e2ee.md §4.6, residual risks).
  async fn verifies_signed_deletion(
    &mut self,
    conversation: ConversationId,
    signed: &SignedChannelDeletion,
  ) -> Result<bool, CoreError> {
    if !self.has_group(conversation)?
      || self.is_pinned(&notes_key(conversation))?
      || self.is_pinned(&direct_key(conversation))?
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
    let held = self.held_anchor(conversation).await?;
    let maybe_listed_name = maybe_known.as_ref().and_then(|known| known.maybe_name.as_deref());
    let anchor = held.facts(maybe_listed_name);
    let verifier = DeletionVerifier {
      conversation,
      is_channel: maybe_known
        .as_ref()
        .is_some_and(|known| known.kind == ConversationKind::Channel),
      maybe_group: epoch.map(|epoch| GroupFacts {
        epoch,
        users: &group_users,
      }),
      roles: &roles,
      workspace: &workspace,
      roster_max_age_ms: self.roster_max_age_ms(),
      max_age_ms: i64::try_from(self.settings.signed_deletion_max_age.as_millis()).unwrap_or(i64::MAX),
      clock_skew_ms: i64::try_from(self.settings.signed_deletion_clock_skew.as_millis()).unwrap_or(i64::MAX),
      now_ms: self.now_ms(),
      channel_roles_max_age_ms: i64::try_from(self.settings.channel_roles_max_age.as_millis())
        .unwrap_or(i64::MAX),
      newest_server_ms: self.newest_server_ms(),
      anchor,
    };
    let verified = match verify_signed_deletion(signed, &verifier) {
      Ok(verified) => verified,
      Err(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "a signed deletion doesn't hold; it stays the server's word");
        return Ok(false);
      },
    };
    // The certificate alone outlives a removal of the device: its user's signed list decides. No
    // list, or none readable, refuses (e2ee.md §4.6).
    let maybe_list = self.fetch_device_list(verified.signer, &verified.identity).await;
    if !matches!(&maybe_list, Ok(Some(list)) if list.is_active(&verified.device)) {
      tracing::warn!(conversation_id = %conversation, signer = %verified.signer, "a signed deletion comes from a device its user's list doesn't name; it stays the server's word");
      return Ok(false);
    }
    tracing::info!(
      conversation_id = %conversation,
      signer = %verified.signer,
      is_workspace_admin = verified.is_workspace_admin,
      "a signed deletion holds; the history goes at once"
    );
    Ok(true)
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
    let is_channel = !self.is_pinned(&direct_key(conversation)).unwrap_or(true)
      && (self.is_known_channel(conversation)
        || self
          .api
          .conversation(conversation)
          .await
          .is_ok_and(|response| response.kind == ConversationKind::Channel));
    if is_channel {
      self.refresh_deletion_authority(conversation).await;
    }
    let maybe_own_epoch = self.group_epoch(conversation).ok().flatten();
    let mut epoch = maybe_own_epoch.unwrap_or_default();
    let mut may_sign = is_channel;
    let mut attempts = 1;
    loop {
      let request = DeleteConversationRequest {
        maybe_signed_deletion: if may_sign {
          self.signed_deletion_if_allowed(conversation, epoch)
        } else {
          None
        },
      };
      let answer = self.api.delete_conversation(conversation, &request).await;
      match answer {
        Err(error) if error.is_epoch_conflict() && attempts < DELETION_ATTEMPTS => {
          let Some(told) = current_epoch_of(&error) else {
            return Err(error);
          };
          // A member knows about where its group is, and signs nothing far from it: the signature
          // would be good until the group got there. Unsigned, the deletion takes the retention.
          if maybe_own_epoch.is_some_and(|own| !is_told_epoch_plausible(own, told)) {
            tracing::warn!(conversation_id = %conversation, told, "the server names an epoch far from this device's group; the deletion stays unsigned");
            may_sign = false;
          }
          epoch = told;
          attempts += 1;
        },
        answer => return answer,
      }
    }
  }

  /// A signature of this device on a channel's deletion: what it would send if it was allowed to,
  /// without any check. What members make of it is up to them.
  pub fn sign_channel_deletion(
    &self,
    conversation: ConversationId,
    epoch: u64,
  ) -> Result<SignedChannelDeletion, CoreError> {
    // The name this user was shown: the server's listing (e2ee.md §4.6.5, D3). Members compare it
    // with their anchor; a swapped one takes the retention there.
    let maybe_name_hash = records::get_json::<ConversationResponse>(
      &self.store,
      Table::Conversations,
      &conversation.to_string(),
    )?
    .and_then(|known| known.maybe_name)
    .map(|name| name_hash(&name));
    sign_channel_deletion(
      &self.account.device,
      &self.account.certificate,
      conversation,
      epoch,
      self.now_ms(),
      maybe_name_hash,
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

  /// The user deleted a space here and the server took it. Which channels were in it is
  /// `maybe_space_id`, the server's word: nothing signed binds a channel to its space yet (#1171),
  /// and channels move between spaces. A server could relabel any conversation into the space being
  /// deleted, so purging what it lists there at once would let it destroy the deleter's copies
  /// (#1162). So they are claims here too: hidden, their history kept for the retention, and counted
  /// towards the mass notice, where a relabel shows as more than the user deleted. What this device
  /// saw as direct stays as it is: no space holds one.
  pub(crate) fn note_space_deletion(&mut self, space: SpaceId) -> Result<(), CoreError> {
    let listed: Vec<ConversationId> = self
      .cached_conversations()?
      .into_values()
      .filter(|known| known.maybe_space_id == Some(space) && known.kind == ConversationKind::Channel)
      .map(|known| known.id)
      .collect();
    for channel in listed {
      if self.is_pinned(&direct_key(channel))? {
        tracing::warn!(conversation_id = %channel, "the server lists a direct conversation in a deleted space; left alone");
        continue;
      }
      self.note_server_deletion(channel)?;
    }
    Ok(())
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

  /// The server lists these conversations: a quarantine of them was wrong, or is over. So is a mark
  /// a purge cut off left: it must not make a later claim purge at once.
  pub(crate) fn lift_quarantines(&mut self, listed: &[ConversationResponse]) -> Result<(), CoreError> {
    for (key, _) in self.store.list(Table::AppliedChanges)? {
      let Some(id) = key
        .strip_prefix(QUARANTINE_PREFIX)
        .or_else(|| key.strip_prefix(OWN_DELETION_PREFIX))
      else {
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

  /// Keeps that a conversation is direct (anything but a channel) from the first time this device
  /// sees it so: `kind` is the server's word and may change later, and no signature purges one.
  pub(crate) fn pin_if_direct(&self, conversation: &ConversationResponse) -> Result<(), CoreError> {
    if conversation.kind == ConversationKind::Channel {
      return Ok(());
    }
    let key = direct_key(conversation.id);
    if !self.is_pinned(&key)? {
      self.store.put(Table::AppliedChanges, &key, b"1")?;
    }
    Ok(())
  }

  fn is_pinned(&self, key: &str) -> Result<bool, CoreError> {
    Ok(self.store.get(Table::AppliedChanges, key)?.is_some())
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

  /// A verified purge leaves nothing behind that [`Self::note_suspect_deletions`] could count.
  fn note_signed_purge(&mut self) -> Result<(), CoreError> {
    let mut purges = self.recent_signed_purges()?;
    purges.push(self.now_ms());
    records::put_json(&self.store, Table::AppliedChanges, SIGNED_PURGES_KEY, &purges)
  }

  /// Bounded by the window: older ones are dropped with each write.
  fn recent_signed_purges(&self) -> Result<Vec<i64>, CoreError> {
    let window_ms = i64::try_from(self.settings.suspect_deletion_window.as_millis()).unwrap_or(i64::MAX);
    let now = self.now_ms();
    let mut purges: Vec<i64> =
      records::get_json(&self.store, Table::AppliedChanges, SIGNED_PURGES_KEY)?.unwrap_or_default();
    purges.retain(|at_ms| now.saturating_sub(*at_ms) <= window_ms);
    Ok(purges)
  }

  /// Counts over a window, not per sync, so claims one by one on the gateway add up too, and signed
  /// purges with them.
  fn note_suspect_deletions(&mut self) -> Result<(), CoreError> {
    let window_ms = i64::try_from(self.settings.suspect_deletion_window.as_millis()).unwrap_or(i64::MAX);
    let now = self.now_ms();
    let recent = self
      .quarantined()?
      .iter()
      .filter(|entry| now.saturating_sub(entry.since_ms) <= window_ms)
      .count()
      .saturating_add(self.recent_signed_purges()?.len());
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

fn direct_key(conversation: ConversationId) -> String {
  format!("{DIRECT_PREFIX}{conversation}")
}

/// The markers a purge removes with the conversation.
pub(crate) fn purge_markers(conversation: ConversationId) -> [String; 3] {
  [
    quarantine_key(conversation),
    own_deletion_key(conversation),
    direct_key(conversation),
  ]
}
