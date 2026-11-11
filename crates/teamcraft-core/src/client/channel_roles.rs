//! A channel's signed role log on this device (issue #380, e2ee.md §4.6.1): reading and verifying
//! it, signing the next entry, and comparing heads with the other members.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use teamcraft_crypto::channel_anchor::AnchorFloor;
use teamcraft_crypto::workspace::{WorkspaceLog, claimed_signer, sign_log_entry};
use teamcraft_proto::{
  AppendChannelRoleLogRequest, Base64Bytes, ChannelRole, ChannelRoleLogEntryResponse, ChannelRolesDocument,
  ConversationId, ConversationKind, ListChannelRoleLogQuery, UserId,
};

use super::Client;
use crate::api::ServerApi;
#[cfg(feature = "ui")]
use crate::channel_roles::RoleKeyStanding;
use crate::channel_roles::{
  ChannelRoleState, ChannelRolesHead, CheckedRoleKey, MessageEntry, RoleFounding, RoleKeyEvidence,
  RoleVerifier, SignerAuthority,
};
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, MessageContent, Table};
use crate::trust::PinnedDirectory;

/// Where a channel's verified roles are kept, in [`Table::Workspace`].
const CHANNEL_ROLES_KEY_PREFIX: &str = "channel_roles/";
/// The channels where a member showed a newer head than this device's, in [`Table::Workspace`].
const CHANNEL_ROLES_AHEAD_KEY: &str = "channel_roles_ahead";
/// The channels whose role logs are read after an admin left the roster, in [`Table::Workspace`].
const CHANNEL_ROLES_DUE_KEY: &str = "channel_roles_due";
/// Counts how often [`CHANNEL_ROLES_DUE_KEY`] was queued afresh, in [`Table::Workspace`].
const CHANNEL_ROLES_DUE_GENERATION_KEY: &str = "channel_roles_due_generation";
/// Set once the channels whose state has no read mark were queued for a read, in
/// [`Table::Workspace`]: a state from before the mark was recorded gets it from that read.
const CHANNEL_ROLES_MARKS_QUEUED_KEY: &str = "channel_roles_marks_queued";
/// The roster's `admin_departures` the due reads were last queued for, in [`Table::Workspace`].
const CHANNEL_ROLES_QUEUED_DEPARTURES_KEY: &str = "channel_roles_queued_departures";
/// How many of those are read in one upkeep pass, so a removal is no burst.
const ROLE_LOG_READS_PER_PASS: usize = 4;
/// How long a failed one waits before it is read again, doubling up to the cap: one channel that
/// keeps failing neither holds up the others nor asks the server every pass.
const ROLE_LOG_READ_RETRY_MS: i64 = 5 * 60 * 1000;
const ROLE_LOG_READ_RETRY_MAX_MS: i64 = 6 * 60 * 60 * 1000;
/// Role entries held at most between reading channels' logs and judging them on one roster read: a
/// pass over many channels with long logs judges them in parts, so memory stays bounded.
const MAX_HELD_ROLE_ENTRIES: usize = 2_000;
/// Pages of a role log read in one sync; a log longer than this is read on in the next one.
const MAX_ROLE_LOG_PAGES: usize = 100;
/// Claimed founders whose identity keys are pinned before a channel's first entries are verified.
const MAX_FOUNDER_CANDIDATES: usize = 16;
/// The channels without a log this device waits with founding, in [`Table::Workspace`].
const CHANNEL_ROLES_FIRST_SIGHT_KEY: &str = "channel_roles_first_sight";
/// The channels where a member's device and this one disagree, in [`Table::Workspace`].
const CHANNEL_ROLES_ALARMED_KEY: &str = "channel_roles_alarmed";
/// What a workspace admin's device can check of a channel's listed keys, in [`Table::Workspace`].
#[cfg(feature = "ui")]
const ROLE_KEY_STANDINGS_KEY_PREFIX: &str = "channel_role_keys/";
/// How many channels are founded in one upkeep pass, so an update is no burst.
const FOUNDINGS_PER_PASS: usize = 4;
/// How long the conversations aren't looked through again once none is left to found. Founding
/// asks nothing of a device that isn't a channel's admin; this only bounds the local read.
const FIRST_SIGHT_SWEEP_MS: i64 = 60 * 60 * 1000;

/// What a signature is to say.
enum RolePlan<'a> {
  /// This device's snapshot with some changes: only where it can vouch for the rest.
  Changes {
    changes: &'a [(UserId, ChannelRole)],
    may_found: bool,
  },
  /// Exactly these roles, which a workspace admin looked at while the log stood at `at_cursor`.
  Reviewed {
    roles: &'a [(UserId, ChannelRole)],
    at_cursor: u64,
  },
  /// A person acting on `changes`, in the app: a channel still without a log is founded on the way.
  Action { changes: &'a [(UserId, ChannelRole)] },
  /// Nobody acting: found the channel if it has no log and the server lists this user its admin.
  FirstSight,
}

/// When to ask an anchored channel's anchor for its founder again, after it gave none (a device
/// list it couldn't fetch, a fork), in [`Table::Workspace`].
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct FoundingWait {
  not_before_ms: i64,
  failures: u32,
  /// The pin and floor at the time: a changed one asks again at once.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  maybe_floor: Option<AnchorFloor>,
}

/// How far the channels without a log are, for the upkeep.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct FirstSightSweep {
  /// When the conversations are looked through again.
  #[serde(default)]
  not_before_ms: i64,
  /// Channels where founding failed or was refused, each with when it is tried again.
  #[serde(default)]
  waiting: BTreeMap<ConversationId, DueRoleRead>,
}

/// A channel's log read due after a removal, with how it went so far.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct DueRoleRead {
  #[serde(default)]
  failures: u32,
  #[serde(default)]
  not_before_ms: i64,
}

/// A channel's role log as read, before its new entries are judged.
struct RoleLogRead {
  conversation: ConversationId,
  state: ChannelRoleState,
  entries: Vec<ChannelRoleLogEntryResponse>,
  is_read_to_end: bool,
  /// The roster version this device held before the read.
  roster_at_read: u64,
}

/// What became of a role change on this device's side of the signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoleSigning {
  /// Signed and stored: every member verifies it.
  Signed,
  /// Not signed: this user can't sign it in a way members would accept (a role that is the
  /// server's word only, a space admin, a custom scheme). The server's change stands; where a signed
  /// role is needed, the person is told when they act.
  Unsigned,
  /// Not signed: the log moved on since the workspace admin looked at this list, so another admin's
  /// change may be missing from it. They look again; it is never signed for them.
  ReviewOutdated,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// The channel's roles as this device verified them (nothing signed yet: the default).
  pub fn channel_role_state(&self, conversation: ConversationId) -> Result<ChannelRoleState, CoreError> {
    let mut state: ChannelRoleState =
      records::get_json(&self.store, Table::Workspace, &channel_roles_key(conversation))?.unwrap_or_default();
    // Once per channel after the update (issue #1264, option 1B); stored, so never again.
    if state.has_legacy_recent_signers {
      state.forget_legacy_recent_signers(&self.workspace()?);
      self.put_channel_role_state(conversation, &state)?;
    }
    Ok(state)
  }

  /// The signed role of `user` in the channel (member when nothing signed names them). Decisions
  /// that must be the same on every member, like accepting a commit, use this and nothing the
  /// server lists; issue #776's owner approval is one.
  pub fn verified_channel_role(
    &self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<ChannelRole, CoreError> {
    Ok(self.channel_role_state(conversation)?.role_of(user))
  }

  /// Before a commit is judged: under `owner_approval` it may add an admin, which only a signed
  /// channel admin may do (e2ee.md §4.6.1), and the admin's role entry may be newer than this
  /// device's copy. Reads the log then; a failure only means the copy stays as it is.
  pub(crate) async fn refresh_channel_roles_for_commit(&mut self, conversation: ConversationId) {
    let is_owner_approval = self.workspace().is_ok_and(|workspace| {
      workspace.policy.admin_self_join == teamcraft_proto::AdminSelfJoin::OwnerApproval
    });
    if !is_owner_approval {
      return;
    }
    if let Err(error) = self.sync_channel_roles(conversation).await {
      tracing::warn!(conversation_id = %conversation, %error, "could not read the channel roles before judging a commit");
    }
  }

  /// Reads the channel's new role entries and verifies them. Skipped entries are logged; the state
  /// keeps the last accepted snapshot. A server that refuses the log (not a member any more, an
  /// older server) changes nothing.
  pub async fn sync_channel_roles(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let read: Vec<RoleLogRead> = self.read_role_log(conversation).await?.into_iter().collect();
    self.judge_role_logs(read).await
  }

  /// Signs the channel's roles after `changes` (`Member` drops a role) if this user can do so in a
  /// way every member accepts: as an admin of the fresh workspace roster, as a signed channel admin,
  /// or, for a channel nothing was signed for yet, as its founder (`may_found`). A changed log in
  /// between (another admin signed) is read and the entry built again once.
  pub async fn sign_channel_roles(
    &mut self,
    conversation: ConversationId,
    changes: &[(UserId, ChannelRole)],
    may_found: bool,
  ) -> Result<RoleSigning, CoreError> {
    let plan = RolePlan::Changes { changes, may_found };
    self.sign_role_plan(conversation, &plan).await
  }

  /// Signs exactly `roles`, which a workspace admin looked at and confirmed: the way out of an
  /// unverified stretch and off a snapshot resting on somebody who left, where this device won't
  /// vouch for what it has (`ChannelRoleState::must_review_before_vouching`). `at_cursor` is where
  /// the log stood when they looked ([`ChannelRoleState::cursor`]); `ReviewOutdated` if it moved, or
  /// moves on the way to the server. `Unsigned` for anybody who isn't an admin of the fresh roster.
  pub async fn sign_reviewed_channel_roles(
    &mut self,
    conversation: ConversationId,
    roles: &[(UserId, ChannelRole)],
    at_cursor: u64,
  ) -> Result<RoleSigning, CoreError> {
    let plan = RolePlan::Reviewed { roles, at_cursor };
    self.sign_role_plan(conversation, &plan).await
  }

  /// Whether this user signs roles here as a workspace admin or a signed channel admin, so that a role
  /// change not signed is something they should hear about: this device can't vouch for the snapshot
  /// or the key. Anybody else's change was the server's word all along.
  pub fn could_sign_channel_roles(&self, conversation: ConversationId) -> Result<bool, CoreError> {
    let state = self.channel_role_state(conversation)?;
    let workspace = self.workspace()?;
    Ok(
      state
        .signing_authorities(
          self.account.user_id,
          &workspace,
          self.now_ms(),
          self.roster_max_age_ms(),
        )
        .iter()
        .any(|authority| *authority != SignerAuthority::Founder),
    )
  }

  /// Signs a role somebody just changed in the app. A channel still without a log is founded on
  /// the way, from the roles the server lists now ([`Self::first_sight_roles`]).
  pub(crate) async fn sign_action_roles(
    &mut self,
    conversation: ConversationId,
    changes: &[(UserId, ChannelRole)],
  ) -> Result<RoleSigning, CoreError> {
    Box::pin(self.sign_role_plan(conversation, &RolePlan::Action { changes })).await
  }

  /// Founds the channel if its log is empty and the server lists this user its admin (e2ee.md
  /// §4.6.1, "Founding at first sight"). `Unsigned` whenever nothing was founded here, also because
  /// somebody else got there first.
  pub(crate) async fn found_at_first_sight(
    &mut self,
    conversation: ConversationId,
  ) -> Result<RoleSigning, CoreError> {
    Box::pin(self.sign_role_plan(conversation, &RolePlan::FirstSight)).await
  }

  /// Founds the logs of channels from before signed roles that the server lists this user the admin
  /// of, a few per upkeep pass. Local reads only while there is nothing to do, at most once in
  /// [`FIRST_SIGHT_SWEEP_MS`]; a channel that fails or is refused waits, later and later.
  pub async fn found_listed_channels(&mut self) -> Result<(), CoreError> {
    let mut sweep: FirstSightSweep =
      records::get_json(&self.store, Table::Workspace, CHANNEL_ROLES_FIRST_SIGHT_KEY)?.unwrap_or_default();
    let now = self.now_ms();
    if sweep.not_before_ms > now {
      return Ok(());
    }
    let due = self.channels_to_found(&sweep, now)?;
    let is_all_of_them = due.len() <= FOUNDINGS_PER_PASS;
    for conversation in due.into_iter().take(FOUNDINGS_PER_PASS) {
      let outcome = self.found_at_first_sight(conversation).await;
      if let Err(error) = &outcome {
        tracing::debug!(conversation_id = %conversation, %error, "channel not founded yet");
      }
      // Whoever founded it, there is a log now; anything else is tried again later.
      if self.channel_role_state(conversation)?.is_signed() {
        sweep.waiting.remove(&conversation);
      } else {
        let wait = sweep.waiting.entry(conversation).or_default();
        wait.failures = wait.failures.saturating_add(1);
        wait.not_before_ms = now.saturating_add(role_read_retry_ms(wait.failures));
      }
    }
    if is_all_of_them {
      sweep.not_before_ms = now.saturating_add(FIRST_SIGHT_SWEEP_MS);
    }
    records::put_json(
      &self.store,
      Table::Workspace,
      CHANNEL_ROLES_FIRST_SIGHT_KEY,
      &sweep,
    )
  }

  /// The open channels the server lists this user the admin of, that this device has no log of, is
  /// in the encryption group of, and isn't waiting with.
  fn channels_to_found(
    &mut self,
    sweep: &FirstSightSweep,
    now: i64,
  ) -> Result<Vec<ConversationId>, CoreError> {
    let mut due = Vec::new();
    for conversation in self.cached_conversations()?.into_values() {
      let is_listed_admin = conversation
        .maybe_my_membership
        .as_ref()
        .is_some_and(|membership| membership.role == ChannelRole::Admin);
      if conversation.kind != ConversationKind::Channel || conversation.is_archived {
        continue;
      }
      let id = conversation.id;
      let is_waiting = sweep
        .waiting
        .get(&id)
        .is_some_and(|wait| wait.not_before_ms > now);
      if is_waiting || !self.channel_role_state(id)?.is_unfounded() || !self.has_group(id)? {
        continue;
      }
      // An anchored channel's founding is the anchor's, never one at first sight (e2ee.md §4.6.5):
      // only the founder's own app signs it, if it was lost at creation, whatever the server lists.
      let is_due = if self.is_anchored_channel(id)? {
        self.anchor_claims_founder(id, self.account.user_id)?
      } else {
        is_listed_admin
      };
      if is_due {
        due.push(id);
      }
    }
    Ok(due)
  }

  async fn sign_role_plan(
    &mut self,
    conversation: ConversationId,
    plan: &RolePlan<'_>,
  ) -> Result<RoleSigning, CoreError> {
    for attempt in 0..2 {
      self.sync_channel_roles(conversation).await?;
      let maybe_request = match *plan {
        RolePlan::Changes { changes, may_found } => {
          self.role_entry(conversation, changes, may_found, false).await?
        },
        RolePlan::Reviewed { roles, at_cursor } => {
          if self.channel_role_state(conversation)?.cursor != at_cursor {
            return Ok(RoleSigning::ReviewOutdated);
          }
          self.reviewed_role_entry(conversation, roles).await?
        },
        RolePlan::Action { changes } => self.action_role_entry(conversation, changes).await?,
        RolePlan::FirstSight => {
          if self.channel_role_state(conversation)?.is_unfounded() {
            self.action_role_entry(conversation, &[]).await?
          } else {
            None
          }
        },
      };
      let Some(request) = maybe_request else {
        return Ok(RoleSigning::Unsigned);
      };
      // A founding is no role change, and a server can get a second one signed by itself: it
      // travels only in the log (e2ee.md §4.6.3, "Foundings stay out").
      let is_founding = !self.channel_role_state(conversation)?.is_signed();
      match self.api.append_channel_role_log(conversation, &request).await {
        Ok(_) if is_founding => {
          self.sync_channel_roles(conversation).await?;
          return Ok(RoleSigning::Signed);
        },
        // Taken here and queued for the members before the log is read again: what the log shows
        // afterwards can neither stop the copy nor hide that it leaves the entry out (review of
        // #1416). A failed read only means the next one compares.
        Ok(response) => {
          // What this device signed, not what the server says it stored; only its time is the
          // server's, as in the log.
          let appended = ChannelRoleLogEntryResponse {
            version: request.expected_version + 1,
            payload: request.payload.clone(),
            signature: request.signature.clone(),
            maybe_signer_device_id: Some(request.signer_device_id),
            created_at: response.created_at,
          };
          self.take_channel_roles_entry(conversation, &appended)?;
          self.send_channel_roles_entry(conversation, &appended).await?;
          if let Err(error) = self.sync_channel_roles(conversation).await {
            tracing::warn!(conversation_id = %conversation, %error, "could not read the channel roles after signing");
          }
          return Ok(RoleSigning::Signed);
        },
        // What somebody reviewed isn't signed again on a log they didn't see: read it, they look.
        Err(error) if error.is_version_conflict() && matches!(plan, RolePlan::Reviewed { .. }) => {
          self.sync_channel_roles(conversation).await?;
          return Ok(RoleSigning::ReviewOutdated);
        },
        Err(error) if error.is_version_conflict() && attempt == 0 => continue,
        // A server from before signed channel roles: the server's change stands, unsigned.
        Err(error) if error.is_not_found() => {
          tracing::info!(conversation_id = %conversation, "the server keeps no channel role log");
          return Ok(RoleSigning::Unsigned);
        },
        Err(error) => return Err(error.into()),
      }
    }
    Err(CoreError::GaveUp {
      attempts: 2,
      reason: "the channel's roles kept changing".into(),
    })
  }

  /// The signed entry for what somebody just did. On a channel without a log it founds the log, with
  /// the roles the server lists now and `changes`, or nothing if this device can't check that list;
  /// otherwise it is an ordinary entry, founding only for an admin the server lists.
  async fn action_role_entry(
    &mut self,
    conversation: ConversationId,
    changes: &[(UserId, ChannelRole)],
  ) -> Result<Option<AppendChannelRoleLogRequest>, CoreError> {
    let me = self.account.user_id;
    let state = self.channel_role_state(conversation)?;
    if !state.is_unfounded() {
      // There is a log this device took nothing from, or it was told of one: signing here would found
      // a second one, and a workspace admin's signature would say more than anybody saw. Only a list
      // a workspace admin looked at ends this (`role_entry` refuses too).
      if !state.is_signed() {
        return Ok(None);
      }
      return self.role_entry(conversation, changes, false, false).await;
    }
    // Nobody founds an anchored channel at first sight: a device at cursor 0 shown an empty log
    // signs nothing (e2ee.md §4.6.5), and the change stays the server's word. Only the anchor's
    // founder signs the founding, if it was lost at creation: themselves as admin and the change,
    // nothing the server lists.
    if self.is_anchored_channel(conversation)? {
      if !self.may_found_roles(conversation, &state).await? {
        tracing::info!(conversation_id = %conversation, "an anchored channel's roles are founded by its anchor's founder only");
        return Ok(None);
      }
      return self.role_entry(conversation, changes, true, false).await;
    }
    let Some(mut roles) = self.first_sight_roles(conversation).await? else {
      tracing::info!(conversation_id = %conversation, "not in this channel's group: left to an admin who is");
      return Ok(None);
    };
    let is_listed_admin = self.server_channel_role(conversation, me)? == Some(ChannelRole::Admin);
    // Nothing to do on anybody's behalf unless the server lists this user the channel's admin.
    if changes.is_empty() && !is_listed_admin {
      return Ok(None);
    }
    roles.retain(|(user, _)| !changes.iter().any(|(changed, _)| changed == user));
    roles.extend_from_slice(changes);
    self.role_entry(conversation, &roles, is_listed_admin, true).await
  }

  /// Who the server lists above member, of the people in this device's encryption group of the
  /// channel, which the server can't add anybody to by itself; `None` without a group. Members only:
  /// a guest stays a member whatever is signed, a deactivated account signs nothing.
  async fn first_sight_roles(
    &mut self,
    conversation: ConversationId,
  ) -> Result<Option<Vec<(UserId, ChannelRole)>>, CoreError> {
    if !self.has_group(conversation)? {
      return Ok(None);
    }
    self.refresh_members(conversation).await?;
    let in_group: BTreeSet<UserId> = self.group_users(conversation)?.into_iter().collect();
    let stored: Option<teamcraft_proto::ListConversationMembersResponse> =
      records::get_json(&self.store, Table::Members, &conversation.to_string())?;
    let listed: Vec<(UserId, ChannelRole)> = stored
      .into_iter()
      .flat_map(|list| list.members)
      .filter(|member| {
        matches!(member.role, ChannelRole::Admin | ChannelRole::Moderator)
          && member.user.kind == teamcraft_proto::UserKind::Member
          && !member.user.is_deactivated
          && !member.user.is_deleted
          && in_group.contains(&member.user.id)
      })
      .map(|member| (member.user.id, member.role))
      .take(teamcraft_proto::MAX_CHANNEL_ROLE_ASSIGNMENTS)
      .collect();
    // Somebody whose key this device can't check is left out, not everybody with them.
    let state = self.channel_role_state(conversation)?;
    let users: Vec<UserId> = listed.iter().map(|(user, _)| *user).collect();
    let keys = self.checked_role_keys(conversation, &state, &users, true).await?;
    Ok(Some(
      listed
        .into_iter()
        .filter(|(user, _)| keys.contains_key(user))
        .collect(),
    ))
  }

  /// Signs the first entry of a channel this device just created: its creator as admin.
  pub(crate) async fn found_channel_roles(&mut self, conversation: ConversationId) {
    let me = self.account.user_id;
    match self
      .sign_channel_roles(conversation, &[(me, ChannelRole::Admin)], true)
      .await
    {
      Ok(RoleSigning::Signed) => {},
      Ok(RoleSigning::Unsigned | RoleSigning::ReviewOutdated) => {
        tracing::info!(conversation_id = %conversation, "the new channel's roles stay unsigned for now");
      },
      // The channel works without; its admin's upkeep founds the log (`found_listed_channels`).
      Err(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "could not sign the new channel's roles");
      },
    }
  }

  /// The channel's signed head, to send along inside an encrypted message (`None` while nothing is
  /// signed, or for direct messages). Marked when this device is stuck on the channel's roles, so a
  /// workspace admin in the channel is offered the review (issue #1264, option 2A).
  pub(crate) fn channel_roles_head(
    &self,
    conversation: ConversationId,
  ) -> Result<Option<ChannelRolesHead>, CoreError> {
    Ok(self.channel_role_state(conversation)?.head_to_send())
  }

  /// Compares a member's head of the channel's roles (from a message the server dated
  /// `sent_at_ms`) with this device's, and takes the sender's request for a review if it carries
  /// one (issue #1264, option 2A).
  pub(crate) fn note_channel_roles_head(
    &self,
    conversation: ConversationId,
    head: &ChannelRolesHead,
    sender: UserId,
    sent_at_ms: i64,
  ) -> Result<(), CoreError> {
    let mut state = self.channel_role_state(conversation)?;
    let before = state.clone();
    state.note_head(head);
    // This device knows of its own state; its echo asks nobody.
    if head.needs_review && sender != self.account.user_id {
      state.note_review_request(sender, sent_at_ms);
    }
    if state == before {
      return Ok(());
    }
    self.put_channel_role_state(conversation, &state)?;
    if state.maybe_ahead.is_some() {
      let mut ahead = self.channel_roles_ahead()?;
      if ahead.insert(conversation) {
        records::put_json(&self.store, Table::Workspace, CHANNEL_ROLES_AHEAD_KEY, &ahead)?;
      }
    }
    Ok(())
  }

  /// Sends the entry this device just appended to the channel's members as a hidden message, so it
  /// reaches them even if the log leaves it out (e2ee.md §4.6.3, part 1). Through the outbox: it is
  /// kept and goes once the device is online again. Only from inside the group; whatever the log
  /// shows afterwards.
  async fn send_channel_roles_entry(
    &mut self,
    conversation: ConversationId,
    appended: &ChannelRoleLogEntryResponse,
  ) -> Result<(), CoreError> {
    if !self.has_group(conversation)? {
      return Ok(());
    }
    let content = MessageContent::ChannelRolesEntry {
      version: appended.version,
      payload: appended.payload.clone(),
      signature: appended.signature.clone(),
    };
    self.enqueue(conversation, None, content)?;
    // Queued: a failure here only means it goes with the next flush.
    if let Err(error) = self.flush_outbox().await {
      tracing::debug!(conversation_id = %conversation, %error, "channel role entry queued for later");
    }
    Ok(())
  }

  /// Takes a role entry a member sent in the channel (e2ee.md §4.6.3, part 1), checked against the
  /// stored roster and pins: receiving reads nothing from the server. An entry it can't verify
  /// yet, or one it accepted, has the log read at the next upkeep, which compares the two.
  pub(crate) fn take_channel_roles_entry(
    &self,
    conversation: ConversationId,
    entry: &ChannelRoleLogEntryResponse,
  ) -> Result<(), CoreError> {
    let mut state = self.channel_role_state(conversation)?;
    let workspace = self.workspace()?;
    let pinned = PinnedDirectory::new(
      &records::pinned_identities(&self.store)?,
      &workspace.roster_identities(),
    );
    let verifier = RoleVerifier {
      conversation,
      workspace: &workspace,
      roster_max_age_ms: self.roster_max_age_ms(),
      now_ms: self.now_ms(),
      tenure_clock_skew_ms: i64::try_from(self.settings.role_tenure_clock_skew.as_millis())
        .unwrap_or(i64::MAX),
      pinned: &pinned,
      // A member's message never carries a founding (`take_message_entry` ignores it while nothing
      // is accepted), so the anchor decides nothing here.
      founding: RoleFounding::Withheld,
    };
    let outcome = state.take_message_entry(entry, &verifier);
    match outcome {
      MessageEntry::Known | MessageEntry::Ignored => return Ok(()),
      MessageEntry::Fork => {
        tracing::error!(conversation_id = %conversation, version = entry.version, "two valid channel role entries for one version")
      },
      MessageEntry::Accepted | MessageEntry::Ahead => {},
    }
    self.put_channel_role_state(conversation, &state)?;
    if matches!(outcome, MessageEntry::Accepted | MessageEntry::Ahead) {
      let mut ahead = self.channel_roles_ahead()?;
      if ahead.insert(conversation) {
        records::put_json(&self.store, Table::Workspace, CHANNEL_ROLES_AHEAD_KEY, &ahead)?;
      }
    }
    Ok(())
  }

  /// Syncs the role logs of channels where a member showed a newer head; afterwards their state
  /// says whether this device is shown less than its colleagues. One read when nothing is due. The
  /// logs are read first and judged together, so the workspace roster is read once for all of them
  /// (#1706), not once per channel.
  pub async fn check_channel_roles_heads(&mut self) -> Result<(), CoreError> {
    let ahead = self.channel_roles_ahead()?;
    if ahead.is_empty() {
      return Ok(());
    }
    let mut reads = Vec::new();
    let mut held_entries = 0;
    for conversation in &ahead {
      if let Some(read) = self.read_role_log(*conversation).await? {
        held_entries += read.entries.len();
        reads.push(read);
      }
      if held_entries >= MAX_HELD_ROLE_ENTRIES {
        self.judge_role_logs(std::mem::take(&mut reads)).await?;
        held_entries = 0;
      }
    }
    self.judge_role_logs(reads).await?;
    let mut still_open = BTreeSet::new();
    for conversation in &ahead {
      // A log too long for one read hasn't shown the entries members sent yet: read on next time.
      if !self.channel_role_state(*conversation)?.unconfirmed.is_empty() {
        still_open.insert(*conversation);
      }
    }
    records::put_json(
      &self.store,
      Table::Workspace,
      CHANNEL_ROLES_AHEAD_KEY,
      &still_open,
    )
  }

  /// After an admin left the roster: reads each channel's role log this device knows, a few per
  /// upkeep pass, so entries the server shows later can't pass for ones from before the removal
  /// (e2ee.md §4.6.1, "Reading the log late"). A local read while nothing is due, which is nearly
  /// always. A failure stays due, later and later, and doesn't hold up the other channels.
  pub async fn read_due_channel_roles(&mut self) -> Result<(), CoreError> {
    self.queue_reads_after_departures(self.workspace()?.admin_departures)?;
    self.queue_unmarked_channel_role_reads()?;
    let mut due: BTreeMap<ConversationId, DueRoleRead> =
      records::get_json(&self.store, Table::Workspace, CHANNEL_ROLES_DUE_KEY)?.unwrap_or_default();
    if due.is_empty() {
      return Ok(());
    }
    let now = self.now_ms();
    let batch: Vec<ConversationId> = due
      .iter()
      .filter(|(_, read)| read.not_before_ms <= now)
      .map(|(conversation, _)| *conversation)
      .take(ROLE_LOG_READS_PER_PASS)
      .collect();
    // Each log is read on its own, so one channel that fails holds up no other; they are judged
    // together, on one read of the roster.
    let mut reads = Vec::new();
    let mut read_channels = Vec::new();
    for conversation in batch {
      match self.read_role_log(conversation).await {
        Ok(maybe_read) => {
          reads.extend(maybe_read);
          read_channels.push(conversation);
        },
        Err(error) => postpone_due_read(&mut due, conversation, now, &error),
      }
    }
    let queued_before = self.due_queue_generation()?;
    let judged = self.judge_role_logs(reads).await;
    // The roster read on the way ended another admin's tenure: every channel was queued afresh, and
    // these logs were read before that roster, so they stay due with the rest.
    if self.due_queue_generation()? != queued_before {
      if let Err(error) = judged {
        tracing::debug!(%error, "channel role logs due after a removal not read yet");
      }
      return Ok(());
    }
    match judged {
      Ok(()) => {
        for conversation in read_channels {
          due.remove(&conversation);
        }
      },
      Err(error) => {
        for conversation in read_channels {
          postpone_due_read(&mut due, conversation, now, &error);
        }
      },
    }
    records::put_json(&self.store, Table::Workspace, CHANNEL_ROLES_DUE_KEY, &due)
  }

  /// Marks every channel this device has role state for as due. Runs when a verified roster ends an
  /// admin's tenure, which is rare.
  pub(crate) fn queue_channel_role_reads(&self) -> Result<(), CoreError> {
    let due: BTreeMap<ConversationId, DueRoleRead> = self
      .store
      .list_prefix(Table::Workspace, CHANNEL_ROLES_KEY_PREFIX)?
      .into_iter()
      .filter_map(|(key, _)| key.strip_prefix(CHANNEL_ROLES_KEY_PREFIX)?.parse().ok())
      .map(|conversation| (conversation, DueRoleRead::default()))
      .collect();
    records::put_json(&self.store, Table::Workspace, CHANNEL_ROLES_DUE_KEY, &due)?;
    let generation = self.due_queue_generation()?.wrapping_add(1);
    records::put_json(
      &self.store,
      Table::Workspace,
      CHANNEL_ROLES_DUE_GENERATION_KEY,
      &generation,
    )
  }

  /// Queues the reads for every removal the verified roster counted (`admin_departures`) that no
  /// queue covers yet. The count is stored with the roster before this runs, so a crash in between
  /// only means the next sync or upkeep pass queues them (review of #1715).
  pub(crate) fn queue_reads_after_departures(&self, departures: u64) -> Result<(), CoreError> {
    let queued: u64 =
      records::get_json(&self.store, Table::Workspace, CHANNEL_ROLES_QUEUED_DEPARTURES_KEY)?.unwrap_or(0);
    if queued == departures {
      return Ok(());
    }
    self.queue_channel_role_reads()?;
    records::put_json(
      &self.store,
      Table::Workspace,
      CHANNEL_ROLES_QUEUED_DEPARTURES_KEY,
      &departures,
    )
  }

  /// Once per device: a channel state from before the read mark was recorded can't place an entry
  /// by somebody who left, and gets the mark only from a read to the log's end, which used to wait
  /// for the next role change or removal. Queues those channels with the reads after a removal, so
  /// the mark is set within a few passes of the update (issue #1264). Each channel already due keeps
  /// its wait.
  fn queue_unmarked_channel_role_reads(&self) -> Result<(), CoreError> {
    let is_done: Option<bool> =
      records::get_json(&self.store, Table::Workspace, CHANNEL_ROLES_MARKS_QUEUED_KEY)?;
    if is_done == Some(true) {
      return Ok(());
    }
    let mut due: BTreeMap<ConversationId, DueRoleRead> =
      records::get_json(&self.store, Table::Workspace, CHANNEL_ROLES_DUE_KEY)?.unwrap_or_default();
    for (key, bytes) in self
      .store
      .list_prefix(Table::Workspace, CHANNEL_ROLES_KEY_PREFIX)?
    {
      let Some(conversation) = key
        .strip_prefix(CHANNEL_ROLES_KEY_PREFIX)
        .and_then(|id| id.parse::<ConversationId>().ok())
      else {
        continue;
      };
      // One state this device can't read mustn't keep the marker unset, or every pass would fail
      // here before the reads due after a removal.
      match serde_json::from_slice::<ChannelRoleState>(&bytes) {
        Ok(state) if state.maybe_read_at_roster_version.is_none() => {
          due.entry(conversation).or_default();
        },
        Ok(_) => {},
        Err(error) => {
          tracing::warn!(conversation_id = %conversation, %error, "unreadable channel role state not queued");
        },
      }
    }
    records::put_json(&self.store, Table::Workspace, CHANNEL_ROLES_DUE_KEY, &due)?;
    records::put_json(
      &self.store,
      Table::Workspace,
      CHANNEL_ROLES_MARKS_QUEUED_KEY,
      &true,
    )
  }

  /// How often the due reads were queued afresh, so a pass can tell that the queue it holds a copy
  /// of was replaced while it ran.
  fn due_queue_generation(&self) -> Result<u64, CoreError> {
    Ok(records::get_json(&self.store, Table::Workspace, CHANNEL_ROLES_DUE_GENERATION_KEY)?.unwrap_or(0))
  }

  fn channel_roles_ahead(&self) -> Result<BTreeSet<ConversationId>, CoreError> {
    Ok(records::get_json(&self.store, Table::Workspace, CHANNEL_ROLES_AHEAD_KEY)?.unwrap_or_default())
  }

  /// The signed request for the next entry, or `None` when this user can't sign one that members
  /// accept. Only channels have roles.
  async fn role_entry(
    &mut self,
    conversation: ConversationId,
    changes: &[(UserId, ChannelRole)],
    may_found: bool,
    is_first_sight: bool,
  ) -> Result<Option<AppendChannelRoleLogRequest>, CoreError> {
    let maybe_response: Option<teamcraft_proto::ConversationResponse> =
      records::get_json(&self.store, Table::Conversations, &conversation.to_string())?;
    if maybe_response.is_some_and(|response| response.kind != ConversationKind::Channel) {
      return Ok(None);
    }
    let me = self.account.user_id;
    let state = self.channel_role_state(conversation)?;
    if !state.is_signed() && state.cursor > 0 {
      tracing::info!(conversation_id = %conversation, "this channel has a log this device took nothing from: not signing on it");
      return Ok(None);
    }
    let workspace = self.workspace()?;
    let mut authorities = state.signing_authorities(me, &workspace, self.now_ms(), self.roster_max_age_ms());
    // While an anchored channel's founding is open, members take only the founder's founding
    // (e2ee.md §4.6.5): a workspace admin's entry, the founder's own included, would be junk on every
    // device and take the version the founding needs.
    if !state.is_signed() && self.is_anchored_channel(conversation)? {
      authorities.retain(|authority| *authority == SignerAuthority::Founder);
    }
    // Every member takes this user's signature as a workspace admin's, whatever authority it is
    // signed on, and it would vouch for the whole snapshot. Nobody asked for that here, and a device
    // that nobody is looking at (an import) has nobody to ask: it stays unsigned.
    if state.must_review_before_vouching() && authorities.iter().any(is_workspace_authority) {
      tracing::info!(conversation_id = %conversation, "not vouching for roles nobody reviewed");
      return Ok(None);
    }
    // ... and it would anchor the snapshot. On one that isn't yet, nothing says the keys in it are
    // right: a first pin from the server can make a forged founding look fine to this device.
    let is_vouching = authorities.iter().any(is_workspace_authority);
    if is_vouching
      && !state.is_workspace_anchored
      && !self.can_check_kept_keys(conversation, &state, changes).await?
    {
      tracing::info!(conversation_id = %conversation, "not vouching for keys this device can't check");
      return Ok(None);
    }
    // In an anchored channel only the anchor's founder founds, under its key (e2ee.md §4.6.5):
    // anybody else's founding would be junk on every member's device.
    let may_found = may_found
      && (!authorities.contains(&SignerAuthority::Founder)
        || self.may_found_roles(conversation, &state).await?);
    let mut maybe_refusal = None;
    for authority in authorities {
      if authority == SignerAuthority::Founder && !may_found {
        continue;
      }
      // Nothing is signed yet as far as this device knows: a workspace admin's ordinary entry would
      // anchor it, so it comes only with a founding this device can stand behind.
      if is_workspace_authority(&authority) && !state.is_signed() && !may_found && !is_first_sight {
        continue;
      }
      let mut changes = changes.to_vec();
      if authority == SignerAuthority::Founder && !changes.iter().any(|(user, _)| *user == me) {
        changes.push((me, ChannelRole::Admin));
      }
      let given_a_role: Vec<UserId> = changes
        .iter()
        .filter(|(_, role)| *role != ChannelRole::Member)
        .map(|(user, _)| *user)
        .collect();
      // A workspace admin's signature vouches for the key too: the snapshot's only if it is anchored.
      let may_go_by_snapshot = !is_vouching || state.is_workspace_anchored;
      let checked = self
        .checked_role_keys(conversation, &state, &given_a_role, may_go_by_snapshot)
        .await?;
      if given_a_role.iter().any(|user| !checked.contains_key(user)) {
        tracing::info!(conversation_id = %conversation, "not signing a role for somebody whose key this device can't check");
        return Ok(None);
      }
      let keys = checked
        .into_iter()
        .map(|(user, found)| (user, found.key))
        .collect();
      let mut document = state.next_document(conversation, &changes, &keys, self.now_ms())?;
      document.has_server_listed_roles |= is_first_sight;
      match state.check_document(&document, conversation, me, authority) {
        Ok(()) => return Ok(Some(self.signed_role_entry(&document)?)),
        Err(error) => maybe_refusal = Some(error),
      }
    }
    if let Some(error) = maybe_refusal {
      tracing::info!(conversation_id = %conversation, %error, "not signing a role change members would refuse");
    }
    Ok(None)
  }

  /// The signed request for exactly `roles`, or `None` when this user isn't an admin of the fresh
  /// roster: only such a signature is a checkpoint.
  async fn reviewed_role_entry(
    &mut self,
    conversation: ConversationId,
    roles: &[(UserId, ChannelRole)],
  ) -> Result<Option<AppendChannelRoleLogRequest>, CoreError> {
    let me = self.account.user_id;
    let state = self.channel_role_state(conversation)?;
    let workspace = self.workspace()?;
    let maybe_authority = state
      .signing_authorities(me, &workspace, self.now_ms(), self.roster_max_age_ms())
      .into_iter()
      .find(is_workspace_authority);
    let Some(authority) = maybe_authority else {
      return Ok(None);
    };
    // A reviewed list vouches for every key in it, so an unanchored snapshot's are no reason.
    let given_a_role: Vec<UserId> = roles
      .iter()
      .filter(|(_, role)| *role != ChannelRole::Member)
      .map(|(user, _)| *user)
      .collect();
    let checked = self
      .checked_role_keys(conversation, &state, &given_a_role, state.is_workspace_anchored)
      .await?;
    if given_a_role.iter().any(|user| !checked.contains_key(user)) {
      tracing::info!(conversation_id = %conversation, "not confirming a role for somebody whose key this device can't check");
      return Ok(None);
    }
    let keys: BTreeMap<UserId, [u8; 32]> = checked
      .into_iter()
      .map(|(user, found)| (user, found.key))
      .collect();
    let document = state.reviewed_document(conversation, roles, &keys, self.now_ms())?;
    state.check_document(&document, conversation, me, authority)?;
    Ok(Some(self.signed_role_entry(&document)?))
  }

  fn signed_role_entry(
    &self,
    document: &ChannelRolesDocument,
  ) -> Result<AppendChannelRoleLogRequest, CoreError> {
    let payload = serde_json::to_vec(document).map_err(|error| CoreError::Corrupt(error.to_string()))?;
    let signature = sign_log_entry(
      &self.account.device,
      &self.account.certificate,
      WorkspaceLog::ChannelRoles,
      document.version,
      &payload,
    )?;
    Ok(AppendChannelRoleLogRequest {
      expected_version: document.version - 1,
      payload: Base64Bytes::new(payload),
      signature: Base64Bytes::new(signature),
      signer_device_id: self.account.device_id,
    })
  }

  /// Whether this device can check the key of everybody the next entry keeps from the snapshot, and
  /// that it is the one the snapshot names: the entry repeats it, so it vouches for it.
  async fn can_check_kept_keys(
    &mut self,
    conversation: ConversationId,
    state: &ChannelRoleState,
    changes: &[(UserId, ChannelRole)],
  ) -> Result<bool, CoreError> {
    let kept: Vec<UserId> = state
      .assignments
      .iter()
      .map(|assignment| assignment.user_id)
      .filter(|user| !changes.iter().any(|(changed, _)| changed == user))
      .collect();
    let checked = self.checked_role_keys(conversation, state, &kept, false).await?;
    Ok(kept.iter().all(|user| {
      checked
        .get(user)
        .is_some_and(|found| state.snapshot_key_of(*user) == Some(found.key))
    }))
  }

  /// The keys this device stands behind for `users`, without those it can't (e2ee.md §4.6.1, "Whose
  /// key a role names"). `may_go_by_snapshot`: whether what the accepted snapshot names counts.
  async fn checked_role_keys(
    &mut self,
    conversation: ConversationId,
    state: &ChannelRoleState,
    users: &[UserId],
    may_go_by_snapshot: bool,
  ) -> Result<BTreeMap<UserId, CheckedRoleKey>, CoreError> {
    Ok(
      self
        .role_key_evidence(conversation, state, users, may_go_by_snapshot)
        .await?
        .into_iter()
        .filter_map(|(user, evidence)| Some((user, evidence.checked()?)))
        .collect(),
    )
  }

  /// What this device knows of each of `users`' keys. A first pin is whatever the server says, so
  /// nobody is pinned for this unless an attestation can then check it.
  async fn role_key_evidence(
    &mut self,
    conversation: ConversationId,
    state: &ChannelRoleState,
    users: &[UserId],
    may_go_by_snapshot: bool,
  ) -> Result<BTreeMap<UserId, RoleKeyEvidence>, CoreError> {
    if self.attestation_settings().is_some() {
      self.pin_identities(users.iter().copied()).await?;
    }
    let in_group: BTreeSet<UserId> = if self.has_group(conversation)? {
      self.group_users(conversation)?.into_iter().collect()
    } else {
      BTreeSet::new()
    };
    let pinned = records::pinned_identities(&self.store)?;
    let workspace = self.workspace()?;
    let me = self.account.user_id;
    let own_key = *self.account.identity_public_key().as_bytes();
    Ok(
      users
        .iter()
        .map(|user| {
          let maybe_pin = pinned.get(user);
          let maybe_roster_key = workspace
            .roster
            .members
            .iter()
            .find(|member| member.user_id == *user)
            .and_then(|member| <[u8; 32]>::try_from(member.identity_key.as_slice()).ok());
          let evidence = RoleKeyEvidence {
            maybe_snapshot: state.snapshot_key_of(*user).filter(|_| may_go_by_snapshot),
            maybe_pinned: maybe_pin.map(|pin| pin.public_key),
            maybe_signed: if *user == me {
              Some(own_key)
            } else {
              maybe_roster_key
            },
            is_attested: maybe_pin.is_some_and(|pin| pin.is_attested),
            is_in_group: in_group.contains(user),
            is_said_to_have_changed: maybe_pin.is_some_and(|pin| pin.maybe_changed_to.is_some()),
          };
          (*user, evidence)
        })
        .collect(),
    )
  }

  /// For a workspace admin's look at the channel's roles: what this device can check of the keys of
  /// everybody the server lists. A device that can't sign for the workspace leaves nothing here.
  #[cfg(feature = "ui")]
  pub(crate) async fn note_role_key_standings(
    &mut self,
    conversation: ConversationId,
  ) -> Result<(), CoreError> {
    let state = self.channel_role_state(conversation)?;
    let workspace = self.workspace()?;
    let can_vouch = state
      .signing_authorities(
        self.account.user_id,
        &workspace,
        self.now_ms(),
        self.roster_max_age_ms(),
      )
      .iter()
      .any(is_workspace_authority);
    if !can_vouch {
      return Ok(());
    }
    let stored: Option<teamcraft_proto::ListConversationMembersResponse> =
      records::get_json(&self.store, Table::Members, &conversation.to_string())?;
    let listed: Vec<UserId> = stored
      .into_iter()
      .flat_map(|list| list.members)
      .filter(|member| member.role != ChannelRole::Member)
      .map(|member| member.user.id)
      .collect();
    let standings: BTreeMap<UserId, RoleKeyStanding> = self
      .role_key_evidence(conversation, &state, &listed, state.is_workspace_anchored)
      .await?
      .into_iter()
      .map(|(user, evidence)| (user, evidence.standing()))
      .collect();
    records::put_json(
      &self.store,
      Table::Workspace,
      &role_key_standings_key(conversation),
      &standings,
    )
  }

  /// Reads the channel's new role entries, without judging them yet. `None` when the server refuses
  /// the log (not a member any more, an older server).
  async fn read_role_log(&self, conversation: ConversationId) -> Result<Option<RoleLogRead>, CoreError> {
    let mut state = self.channel_role_state(conversation)?;
    state.prepare_read();
    // An anchored channel whose anchor gave no founder lately: its log would be read from the start
    // and left unread again (e2ee.md §4.6.5).
    if !state.is_signed() && self.is_founding_waiting(conversation)? {
      return Ok(None);
    }
    // Before the read, so an entry that arrives while it runs isn't judged by a newer roster.
    let roster_at_read = self.workspace()?.roster.version;
    let (entries, is_read_to_end) = match self.fetch_role_log(conversation, state.log_read_from()).await {
      Ok(read) => read,
      Err(CoreError::Api(error)) if error.is_not_found() || error.is_forbidden() => return Ok(None),
      Err(error) => return Err(error),
    };
    // Entries members sent in the channel: the log must show the same (e2ee.md §4.6.3).
    state.compare_log(&entries, is_read_to_end);
    Ok(Some(RoleLogRead {
      conversation,
      state,
      entries,
      is_read_to_end,
      roster_at_read,
    }))
  }

  /// Verifies the new entries of logs read before and stores each channel's state. The workspace
  /// roster is read once for all of them, after the last log was read, and only if one has new
  /// entries: every entry is then judged against a roster at least as new as the one it was signed
  /// on (e2ee.md §4.6.1). A failed roster read stores nothing, so every entry stays unread.
  async fn judge_role_logs(&mut self, reads: Vec<RoleLogRead>) -> Result<(), CoreError> {
    let has_new_entries = reads
      .iter()
      .any(|read| read.entries.iter().any(|entry| entry.version > read.state.cursor));
    if has_new_entries {
      // Entries are judged against the newest roster the server shows, with its history. A failed
      // read leaves them unread, to be judged next time: judged on an old roster they would be
      // refused for good, and could end up as a stretch only a workspace admin can end.
      self.sync_workspace().await?;
      self.complete_admin_history().await?;
    }
    for read in reads {
      self.judge_role_log(read).await?;
    }
    Ok(())
  }

  /// One channel's part of [`Self::judge_role_logs`], on the roster read for all of them.
  async fn judge_role_log(&mut self, read: RoleLogRead) -> Result<(), CoreError> {
    let RoleLogRead {
      conversation,
      mut state,
      entries,
      is_read_to_end,
      roster_at_read,
    } = read;
    // Without new entries the anchor isn't asked (it fetches device lists): an anchored channel
    // still counts as one whose founding may be open, so the read isn't marked at the roster.
    let mut founding = if self.is_anchored_channel(conversation)? {
      RoleFounding::Withheld
    } else {
      RoleFounding::Claimed
    };
    let has_new_entries = entries.iter().any(|entry| entry.version > state.cursor);
    if has_new_entries {
      founding = self.founding_for(conversation, &state).await?;
      // A founder must hold the key this device pins for them; pin before verifying, or the
      // entry would be skipped for good.
      let claimed = founder_candidates(&state, &entries);
      let pinned = self.pin_identities(claimed).await?;
      let workspace = self.workspace()?;
      let verifier = RoleVerifier {
        conversation,
        workspace: &workspace,
        roster_max_age_ms: self.roster_max_age_ms(),
        now_ms: self.now_ms(),
        tenure_clock_skew_ms: i64::try_from(self.settings.role_tenure_clock_skew.as_millis())
          .unwrap_or(i64::MAX),
        pinned: &pinned,
        founding,
      };
      for error in state.apply_entries(entries, &verifier) {
        tracing::warn!(conversation_id = %conversation, %error, "skipped a channel role entry");
      }
    }
    // Entries left unread for the anchor weren't seen at this roster.
    if is_read_to_end && !state.is_founding_open(founding) {
      state.note_read(roster_at_read);
    }
    if founding != RoleFounding::Claimed {
      if !state.is_founding_open(founding) {
        self.clear_founding_wait(conversation)?;
      } else if has_new_entries && matches!(founding, RoleFounding::Anchored { .. }) {
        // The log shows entries but no founding this device takes: reading it again from its start
        // waits too, or a server could have every sync verify the whole log anew.
        self.note_founding_wait(conversation)?;
      }
    }
    state.check_ahead();
    if let Some(alarm) = &state.maybe_alarm {
      tracing::error!(conversation_id = %conversation, %alarm, "channel role log alarm");
    }
    if let Some(from) = state.maybe_unverified_from {
      tracing::warn!(conversation_id = %conversation, from, "this device can't verify a stretch of the channel's role log");
    }
    self.put_channel_role_state(conversation, &state)
  }

  /// Whether this user's founding would found the channel's roles on members' devices: as before
  /// anchors, or as the founder the anchor names, under this account's identity key.
  async fn may_found_roles(
    &mut self,
    conversation: ConversationId,
    state: &ChannelRoleState,
  ) -> Result<bool, CoreError> {
    let me = self.account.user_id;
    let own_key = *self.account.identity_public_key().as_bytes();
    Ok(match self.founding_for(conversation, state).await? {
      RoleFounding::Claimed => true,
      RoleFounding::Anchored {
        founder,
        identity_key,
      } => founder == me && identity_key == own_key,
      RoleFounding::Withheld => false,
    })
  }

  /// Who may found the channel's roles, for a read of its log: asked of the anchor only while
  /// nothing is accepted, as it decides nothing after that. An anchor this device can't judge now
  /// (a device list it can't fetch) withholds the founding: the entries wait for the next read.
  async fn founding_for(
    &mut self,
    conversation: ConversationId,
    state: &ChannelRoleState,
  ) -> Result<RoleFounding, CoreError> {
    if !self.is_anchored_channel(conversation)? {
      return Ok(RoleFounding::Claimed);
    }
    if state.is_signed() {
      return Ok(RoleFounding::Withheld);
    }
    if self.is_founding_waiting(conversation)? {
      return Ok(RoleFounding::Withheld);
    }
    let founding = match Box::pin(self.role_founding(conversation)).await {
      Ok(founding) => founding,
      Err(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "could not judge the channel's anchor; its role log waits");
        RoleFounding::Withheld
      },
    };
    if founding == RoleFounding::Withheld {
      self.note_founding_wait(conversation)?;
    }
    Ok(founding)
  }

  /// Whether an anchor that gave no founder last time is asked again only later: it fetches device
  /// lists, and the log would be read from its start for nothing. A changed pin or floor asks at once.
  fn is_founding_waiting(&self, conversation: ConversationId) -> Result<bool, CoreError> {
    let maybe_wait: Option<FoundingWait> =
      records::get_json(&self.store, Table::Workspace, &founding_wait_key(conversation))?;
    let now = self.now_ms();
    Ok(maybe_wait.is_some_and(|wait| {
      // A wait set while this clock was ahead lasts no longer than the longest one.
      let not_before_ms = wait
        .not_before_ms
        .min(now.saturating_add(ROLE_LOG_READ_RETRY_MAX_MS));
      not_before_ms > now && wait.maybe_floor == self.account_data().anchor_floor(conversation)
    }))
  }

  /// Records when to ask the anchor and read the log again after this device found no founding to
  /// take (later and later).
  fn note_founding_wait(&self, conversation: ConversationId) -> Result<(), CoreError> {
    let key = founding_wait_key(conversation);
    let maybe_wait: Option<FoundingWait> = records::get_json(&self.store, Table::Workspace, &key)?;
    let failures = maybe_wait.map_or(0, |wait| wait.failures).saturating_add(1);
    let wait = FoundingWait {
      not_before_ms: self.now_ms().saturating_add(role_read_retry_ms(failures)),
      failures,
      maybe_floor: self.account_data().anchor_floor(conversation),
    };
    records::put_json(&self.store, Table::Workspace, &key, &wait)
  }

  /// Forgets the wait once the founding is accepted.
  fn clear_founding_wait(&self, conversation: ConversationId) -> Result<(), CoreError> {
    let key = founding_wait_key(conversation);
    if self.store.get(Table::Workspace, &key)?.is_some() {
      self.store.delete(Table::Workspace, &key)?;
    }
    Ok(())
  }

  async fn fetch_role_log(
    &self,
    conversation: ConversationId,
    after: u64,
  ) -> Result<(Vec<ChannelRoleLogEntryResponse>, bool), CoreError> {
    let mut entries = Vec::new();
    let mut cursor = after;
    for _ in 0..MAX_ROLE_LOG_PAGES {
      let page = self
        .api
        .list_channel_role_log(
          conversation,
          &ListChannelRoleLogQuery {
            maybe_after: Some(cursor),
            maybe_limit: None,
          },
        )
        .await?;
      if let Some(last) = page.entries.last() {
        cursor = last.version;
      }
      let is_done = !page.has_more || page.entries.is_empty();
      entries.extend(page.entries);
      if is_done {
        return Ok((entries, true));
      }
    }
    Ok((entries, false))
  }

  fn put_channel_role_state(
    &self,
    conversation: ConversationId,
    state: &ChannelRoleState,
  ) -> Result<(), CoreError> {
    records::put_json(
      &self.store,
      Table::Workspace,
      &channel_roles_key(conversation),
      state,
    )?;
    let mut alarmed: BTreeSet<ConversationId> =
      records::get_json(&self.store, Table::Workspace, CHANNEL_ROLES_ALARMED_KEY)?.unwrap_or_default();
    let is_changed = if state.maybe_alarm.is_some() {
      alarmed.insert(conversation)
    } else {
      alarmed.remove(&conversation)
    };
    if is_changed {
      records::put_json(&self.store, Table::Workspace, CHANNEL_ROLES_ALARMED_KEY, &alarmed)?;
    }
    Ok(())
  }

  /// The role the stored member list (the server's word) gives `user`.
  pub(crate) fn server_channel_role(
    &self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<Option<ChannelRole>, CoreError> {
    let stored: Option<teamcraft_proto::ListConversationMembersResponse> =
      records::get_json(&self.store, Table::Members, &conversation.to_string())?;
    Ok(
      stored
        .into_iter()
        .flat_map(|list| list.members)
        .find(|member| member.user.id == user)
        .map(|member| member.role),
    )
  }

  pub(crate) fn roster_max_age_ms(&self) -> i64 {
    i64::try_from(self.settings.workspace_max_age.as_millis()).unwrap_or(i64::MAX)
  }
}

/// While nothing is accepted yet, who the new entries claim signed them (unverified): one of them
/// may be the founder, whose key must be pinned before the entry is verified. Bounded, so a server
/// can't make this device look up everybody.
fn founder_candidates(state: &ChannelRoleState, entries: &[ChannelRoleLogEntryResponse]) -> BTreeSet<UserId> {
  if state.is_signed() {
    return BTreeSet::new();
  }
  entries
    .iter()
    .filter_map(|entry| claimed_signer(entry.signature.as_slice()).ok())
    .filter_map(|user| crate::convert::user_from_crypto(user).ok())
    .take(MAX_FOUNDER_CANDIDATES)
    .collect()
}

/// A due role log read failed: it waits longer, and the other channels get their turn meanwhile.
fn postpone_due_read(
  due: &mut BTreeMap<ConversationId, DueRoleRead>,
  conversation: ConversationId,
  now_ms: i64,
  error: &CoreError,
) {
  tracing::debug!(conversation_id = %conversation, %error, "channel role log due after a removal not read yet");
  if let Some(read) = due.get_mut(&conversation) {
    read.failures = read.failures.saturating_add(1);
    read.not_before_ms = now_ms.saturating_add(role_read_retry_ms(read.failures));
  }
}

/// How long a role log read waits after its `failures`th failure.
fn role_read_retry_ms(failures: u32) -> i64 {
  let doublings = failures.saturating_sub(1).min(16);
  ROLE_LOG_READ_RETRY_MS
    .saturating_mul(1 << doublings)
    .min(ROLE_LOG_READ_RETRY_MAX_MS)
}

fn is_workspace_authority(authority: &SignerAuthority) -> bool {
  matches!(authority, SignerAuthority::Workspace { .. })
}

/// Where the channels with an alarm about their role logs are kept, for the UI.
#[cfg(feature = "ui")]
pub(crate) fn channel_roles_alarmed_key() -> &'static str {
  CHANNEL_ROLES_ALARMED_KEY
}

#[cfg(feature = "ui")]
pub(crate) fn role_key_standings_key(conversation: ConversationId) -> String {
  format!("{ROLE_KEY_STANDINGS_KEY_PREFIX}{conversation}")
}

fn founding_wait_key(conversation: ConversationId) -> String {
  format!("channel_roles_founding_wait/{conversation}")
}

pub(crate) fn channel_roles_key(conversation: ConversationId) -> String {
  format!("{CHANNEL_ROLES_KEY_PREFIX}{conversation}")
}
