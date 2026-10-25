//! Workspace roster and policy (verified client-side), admin actions on groups the admin isn't in,
//! and membership changes the server asks for (removed users and devices).

use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use teamcraft_crypto::membership::{MembershipAction, MembershipChange, SignedMembershipChange};
use teamcraft_crypto::workspace::{WorkspaceLog, sign_log_entry};
use teamcraft_proto::{
  AddConversationMemberRequest, AppendWorkspaceLogRequest, Base64Bytes, ClientMessageId, ConversationId,
  DeviceId, ErrorCode, HistoryPolicy, Id, ListWorkspaceLogQuery, MlsMessageKind, PostMlsMessageRequest,
  PresenceState, UserId, WorkspaceLogEntryResponse, WorkspaceLogKind, WorkspacePolicy,
};

use super::Client;
use super::groups::{ADMIN_CHANGE_MAGIC, ChangeSource};
use super::pending_add::UserAddWait;
use super::removal::{RemovalProgress, RemovalRun, is_worth_retrying};
use crate::api::{ApiError, ServerApi};
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{ConversationEvent, LocalStore, Table};
use crate::workspace::{
  ChainedPolicyDocument, RosterDocument, RosterMember, RosterRole, WorkspaceHead, WorkspaceState,
};

pub(crate) const WORKSPACE_KEY: &str = "state";
/// Where [`PendingChange::RevocationCheck`]s wait in the pending queue, before the device ID.
pub(crate) const REVOCATION_CHECK_KEY_PREFIX: &str = "revocation_check/";

/// A membership change waiting to be committed by this device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum PendingChange {
  /// An admin's signed change (admin.md §2.2).
  Delegated {
    conversation_id: ConversationId,
    change: Base64Bytes,
  },
  /// Queued by earlier versions when the server reported a removal; dropped unsigned now
  /// (security review A8).
  RemoveUser {
    conversation_id: ConversationId,
    user_id: UserId,
  },
  /// Queued by earlier versions when the server reported a revoked device; dropped now.
  RemoveDevice {
    conversation_id: ConversationId,
    user_id: UserId,
    device_id: DeviceId,
  },
  /// A member registered a new device (`device_add`). Every online member queues this, staggered
  /// by a rank derived from the device IDs, so normally only the first one commits; the others find
  /// the device already added. Retried while the device has no key package yet.
  AddDevice {
    conversation_id: ConversationId,
    user_id: UserId,
    device_id: DeviceId,
    not_before_ms: i64,
    attempts: u32,
  },
  /// A device fell out of the group and asked, with its signed request, to be added back
  /// (e2ee.md §4.6). Staggered like [`Self::AddDevice`]; done once any member answered.
  Rejoin {
    conversation_id: ConversationId,
    user_id: UserId,
    device_id: DeviceId,
    request: Base64Bytes,
    not_before_ms: i64,
    attempts: u32,
  },
  /// Content keys this device still owes a member it added (the commit's answer was lost, or
  /// sending them failed).
  ShareHistory {
    conversation_id: ConversationId,
    share: SharedKeys,
  },
  /// A further device of a member joined without this device's commit (by external commit, or a
  /// colleague added it): its key bag may lack the current key (#178). Staggered like
  /// [`Self::AddDevice`]; dropped once any member sent a current key to the group.
  ShareCurrentKey {
    conversation_id: ConversationId,
    device_id: DeviceId,
    not_before_ms: i64,
  },
  /// A removal (or leave) this device started: the MLS side first, then the server's membership
  /// (e2ee.md §4.6, removal order). Kept until both are done.
  Removal {
    conversation_id: ConversationId,
    user_id: UserId,
    step: RemovalStep,
    /// Not tried again before this (server time of this device's clock) after the server refused
    /// the server side; 0 for right away.
    #[serde(default)]
    not_before_ms: i64,
    /// Definite refusals so far (or, while watching, re-signings); each pushes the next try
    /// further out, up to `removal_retry_max`.
    #[serde(default)]
    attempts: u32,
    #[serde(default)]
    state: RemovalState,
    /// The goodbye or the announcement of the removal, with the message ID chosen up front, so a
    /// retry after a crash sends it once (sent before the group step for a goodbye, after it for
    /// an announcement).
    #[serde(default)]
    maybe_announcement: Option<PendingAnnouncement>,
  },
  /// A revocation of another device of this user: the server first, then the signed device list
  /// and the groups. Kept until all are done.
  Revocation {
    device_id: DeviceId,
    step: RevocationStep,
    /// Not tried again before this after a group refused the removal; 0 for right away.
    #[serde(default)]
    not_before_ms: i64,
  },
  /// The server reported another device of this user revoked (`device_remove`). Only the signed
  /// device list decides: once it names the device revoked, this device removes it from the groups
  /// they share, in case the revoking device couldn't (#232, e2ee.md §3). Dropped after
  /// `removal_attempts` checks that found nothing signed.
  RevocationCheck {
    device_id: DeviceId,
    not_before_ms: i64,
    #[serde(default)]
    attempts: u32,
  },
  /// A user this device added who had no device to add yet (none set up, or none with a key
  /// package left); see [`super::pending_add`]. The server lists them already.
  AddUser {
    conversation_id: ConversationId,
    user_id: UserId,
    history: HistoryPolicy,
    wait: UserAddWait,
    /// When to try next; `None` waits for the user's first device (`device_add`).
    #[serde(default)]
    maybe_not_before_ms: Option<i64>,
    #[serde(default)]
    attempts: u32,
  },
}

/// The next step of a [`PendingChange::Removal`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RemovalStep {
  /// Remove the user from the MLS group (commit, or a signed change the members commit).
  Group,
  /// Leaving: the signed removal of this user, signed for `signed_epoch`, is in the group's queue;
  /// wait until a member's commit removed this device before dropping the server's membership.
  AwaitCommit { signed_epoch: u64 },
  /// The group side is done; drop the server's membership.
  Server,
  /// An admin outside the group posted the signed removal for `signed_epoch`: drop the server's
  /// membership, then watch whether the change gets too old to be committed.
  ServerThenWatch { signed_epoch: u64 },
  /// An admin outside the group can't see the members' commit: until `removal_watch` after
  /// `since_ms` (the first signing), it checks the group's epoch (the epoch-conflict answer tells it) and signs
  /// the removal again before members would refuse it as too old (`MAX_CHANGE_AGE_EPOCHS`).
  /// Signing again is harmless when the first one was committed: members find nobody to remove.
  Watch {
    signed_epoch: u64,
    since_ms: i64,
    /// Checks so far, for the backoff between them.
    probes: u32,
  },
}

/// Whether a [`PendingChange::Removal`] is still being tried.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RemovalState {
  #[default]
  Retrying,
  /// Refused `removal_attempts` times: shown as failed until the user tries again.
  Failed,
  /// Can't be confirmed from here (an admin outside the group: the server lists the user again,
  /// or the server step resumed later, when somebody may have added them back): waits for the
  /// admin's "remove" or "keep", never settled on the server's word.
  Unconfirmed,
}

/// A message that belongs to a removal: the leaver's goodbye or the remover's announcement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PendingAnnouncement {
  pub(crate) client_message_id: ClientMessageId,
  pub(crate) event: ConversationEvent,
}

impl PendingAnnouncement {
  /// A goodbye goes out before the removal (the leaver can't write afterwards); an announcement
  /// after the key rotation, so the removed person doesn't read it.
  pub(crate) fn is_before_group_step(&self) -> bool {
    matches!(self.event, ConversationEvent::Left)
  }
}

/// The next step of a [`PendingChange::Revocation`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RevocationStep {
  /// Revoke the device on the server (ends its sessions).
  Server,
  /// Publish the device list without it and remove it from every shared group.
  Groups,
}

/// Which content keys a new member gets after the commit that added them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SharedKeys {
  /// Every key of the conversation ("share history").
  FullHistory,
  /// The current key only (history from the join on).
  CurrentKey,
  /// To a device added back, sealed to it: the keys of the epochs it was a member in (e2ee.md
  /// §4.6).
  MissedKeys { user_id: UserId, device_id: DeviceId },
}

/// What became of a pending change in this round.
pub(crate) enum PendingOutcome {
  Done,
  /// Keep it, possibly updated (not due yet, or retried later).
  Later(PendingChange),
}

impl PendingChange {
  /// Where a device's rejoin request waits ([`PendingChange::Rejoin`]).
  pub(crate) fn rejoin_key(conversation: ConversationId, device: DeviceId) -> String {
    format!("rejoin/{conversation}/{device}")
  }

  /// Where the addition of a user without a device waits ([`PendingChange::AddUser`]).
  pub(crate) fn user_add_key(conversation: ConversationId, user: UserId) -> String {
    format!("add_user/{conversation}/{user}")
  }

  /// Where the hand-over of the current key to a further device waits
  /// ([`PendingChange::ShareCurrentKey`]).
  pub(crate) fn current_key_share_key(conversation: ConversationId, device: DeviceId) -> String {
    format!("{}{device}", Self::current_key_share_prefix(conversation))
  }

  pub(crate) fn current_key_share_prefix(conversation: ConversationId) -> String {
    format!("share_current/{conversation}/")
  }

  /// Where a removal this device started waits ([`PendingChange::Removal`]).
  pub(crate) fn removal_key(conversation: ConversationId, user: UserId) -> String {
    format!("removal/{conversation}/{user}")
  }

  /// Where a revocation this device started waits ([`PendingChange::Revocation`]).
  pub(crate) fn revocation_key(device: DeviceId) -> String {
    format!("revocation/{device}")
  }

  /// Where the check of a revocation the server reported waits ([`PendingChange::RevocationCheck`]).
  pub(crate) fn revocation_check_key(device: DeviceId) -> String {
    format!("{REVOCATION_CHECK_KEY_PREFIX}{device}")
  }
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// The workspace roster and policy as verified so far (defaults before the first sync).
  pub fn workspace(&self) -> Result<WorkspaceState, CoreError> {
    Ok(records::get_json(&self.store, Table::Workspace, WORKSPACE_KEY)?.unwrap_or_default())
  }

  /// Fingerprint of the workspace root of trust (the first owner's identity key), for the user to
  /// compare with a colleague or the admin (admin.md §2.3: "shown once, can be verified").
  pub fn workspace_root_fingerprint(&self) -> Result<Option<String>, CoreError> {
    Ok(self.workspace()?.maybe_root.map(|root| {
      teamcraft_crypto::identity::IdentityPublicKey::from_bytes(root.identity_key)
        .fingerprint()
        .to_string()
    }))
  }

  /// The owners of the verified roster and the console's warning lead (admin.md §2.3).
  #[cfg(feature = "ui")]
  pub fn owner_roster_status(&self) -> Option<teamcraft_client_api::admin::OwnerRosterStatus> {
    crate::workspace::owner_roster_status(
      &self.store,
      self.settings.workspace_max_age,
      self.settings.workspace_expiry_warning,
    )
  }

  /// The verified workspace policy; the default policy if none was signed yet.
  pub fn workspace_policy(&self) -> Result<WorkspacePolicy, CoreError> {
    Ok(self.workspace()?.policy)
  }

  /// Fetches new roster and policy entries and verifies them. A rejected entry stops the chain; the
  /// last verified state stays in force and [`WorkspaceState::maybe_rejection`] says why.
  pub async fn sync_workspace(&mut self) -> Result<(), CoreError> {
    let mut state = self.workspace()?;
    // A new device takes the root another device of the user verified, so a forged genesis
    // roster is refused here too (security review A2).
    if state.maybe_root.is_none()
      && let Some(root) = self.account_data().maybe_workspace_root
      && let Ok(identity_key) = <[u8; 32]>::try_from(root.public_key.as_slice())
    {
      state.maybe_root = Some(crate::workspace::RootOfTrust {
        user_id: root.user_id,
        identity_key,
      });
    }
    let mut entries = self
      .fetch_log(&WorkspaceLogKind::Roster, state.roster.version)
      .await?;
    entries.extend(
      self
        .fetch_log(&WorkspaceLogKind::Policy, state.policy_version)
        .await?,
    );
    if let Err(error) = state.apply_entries(entries) {
      tracing::error!(%error, "rejected workspace log entry");
    }
    state.check_ahead();
    if let Some(alarm) = &state.maybe_alarm {
      tracing::error!(%alarm, "workspace log alarm");
    }
    records::put_json(&self.store, Table::Workspace, WORKSPACE_KEY, &state)
  }

  /// Compares a member's workspace head (from a message) with this device's.
  pub(crate) fn note_workspace_head(&self, head: &WorkspaceHead) -> Result<(), CoreError> {
    let mut state = self.workspace()?;
    state.note_head(head);
    records::put_json(&self.store, Table::Workspace, WORKSPACE_KEY, &state)
  }

  /// Syncs the logs if a member showed a newer head; afterwards the state says whether this device
  /// is being shown less than its colleagues.
  pub async fn check_workspace_head(&mut self) -> Result<(), CoreError> {
    if self.workspace()?.maybe_ahead.is_some() {
      self.sync_workspace().await?;
    }
    Ok(())
  }

  /// Signs and publishes a new roster (owners only). Identity keys come from what this device
  /// pinned; the first roster makes this user the first owner and the workspace root of trust.
  pub async fn publish_roster(&mut self, roles: &[(UserId, RosterRole)]) -> Result<(), CoreError> {
    self.sync_workspace().await?;
    let directory_users: Vec<UserId> = roles.iter().map(|(user, _)| *user).collect();
    self.pin_identities(directory_users).await?;
    let pinned = records::pinned_identities(&self.store)?;
    let mut members = Vec::new();
    for (user, role) in roles {
      let identity = pinned.get(user).ok_or(CoreError::UnknownIdentity(*user))?;
      members.push(RosterMember {
        user_id: *user,
        role: role.clone(),
        identity_key: Base64Bytes::new(identity.public_key.to_vec()),
      });
    }
    let mut document = self.workspace()?.roster;
    document.members = members;
    self.append_roster(document).await
  }

  /// [`Self::publish_roster`] only if the verified roster is still `base_version`, the one the
  /// console's editor started from: an edit of an older roster would silently undo another
  /// owner's change.
  pub async fn publish_roster_based_on(
    &mut self,
    base_version: u64,
    roles: &[(UserId, RosterRole)],
  ) -> Result<(), CoreError> {
    self.sync_workspace().await?;
    if self.workspace()?.roster.version != base_version {
      return Err(CoreError::RosterBaseMismatch);
    }
    self.publish_roster(roles).await
  }

  /// Publishes the current roster with a new org recovery key (owners only).
  pub(crate) async fn publish_org_recovery_key(&mut self, key: &[u8; 32]) -> Result<(), CoreError> {
    self.sync_workspace().await?;
    let mut document = self.workspace()?.roster;
    document.maybe_org_recovery_key = Some(Base64Bytes::new(key.to_vec()));
    self.append_roster(document).await
  }

  /// Signs `policy` only if it was edited from the policy this device itself verified. The console
  /// shows what the server says; a compromised server could alter fields there, and signing that
  /// draft would make the owner vouch for them. `base` is the policy the editor started from.
  pub async fn publish_policy_based_on(
    &mut self,
    base: &WorkspacePolicy,
    policy: WorkspacePolicy,
  ) -> Result<(), CoreError> {
    self.sync_workspace().await?;
    if self.workspace()?.policy != *base {
      return Err(CoreError::PolicyBaseMismatch);
    }
    self.publish_policy(policy).await
  }

  /// [`Self::publish_policy_based_on`], first signing the genesis roster if this device verified
  /// none yet: the signer as owner and root of trust, plus the `owners` and `admins` the user
  /// confirmed with their IDs and keys (admin.md §2.3, #265). The server's role holders never go
  /// in unconfirmed; once a roster is verified, `owners` and `admins` are ignored.
  pub async fn publish_policy_with_genesis(
    &mut self,
    base: &WorkspacePolicy,
    policy: WorkspacePolicy,
    owners: &[UserId],
    admins: &[UserId],
  ) -> Result<(), CoreError> {
    self.sync_workspace().await?;
    if self.workspace()?.roster.version == 0 {
      let roles = crate::workspace::genesis_roles(self.account.user_id, owners, admins);
      self.publish_roster(&roles).await?;
    }
    self.publish_policy_based_on(base, policy).await
  }

  /// Signs and publishes a new workspace policy (owners only).
  pub async fn publish_policy(&mut self, policy: WorkspacePolicy) -> Result<(), CoreError> {
    self.sync_workspace().await?;
    let state = self.workspace()?;
    if state.roster.version > 0 && !self.is_verified_owner(&state) {
      return Err(CoreError::NotAnOwner);
    }
    let document = ChainedPolicyDocument {
      version: state.policy_version + 1,
      policy,
      roster_version: state.roster.version,
      maybe_prev_hash: Some(head_or_genesis(state.heads.maybe_policy_hash)),
      maybe_signed_at_ms: Some(self.now_ms()),
    };
    let version = document.version;
    self
      .append_log(WorkspaceLogKind::Policy, WorkspaceLog::Policy, version, &document)
      .await
  }

  /// Whether this user is an owner in the roster this device verified.
  fn is_verified_owner(&self, state: &WorkspaceState) -> bool {
    state
      .roster
      .members
      .iter()
      .any(|member| member.user_id == self.account.user_id && member.role == RosterRole::Owner)
  }

  /// Owners: signs the roster again when it is half as old as clients accept (security review A6).
  /// Cheap when nothing is due; the runner calls it regularly.
  pub async fn refresh_workspace_signatures(&mut self) -> Result<(), CoreError> {
    let state = self.workspace()?;
    let is_owner = state
      .roster
      .members
      .iter()
      .any(|member| member.user_id == self.account.user_id && member.role == RosterRole::Owner);
    let max_age_ms = i64::try_from(self.settings.workspace_max_age.as_millis()).unwrap_or(i64::MAX);
    if !is_owner || state.is_roster_fresh(self.now_ms(), max_age_ms / 2) && state.heads.is_roster_chained {
      return Ok(());
    }
    self.sync_workspace().await?;
    let document = self.workspace()?.roster;
    self.append_roster(document).await
  }

  /// Signs `document` as the next roster version, then the current policy again under it, so a
  /// policy signed under an older roster (by an owner removed since) is refused (A7). A roster
  /// without an owner is never signed: nobody could change it again (admin.md §2.3).
  async fn append_roster(&mut self, mut document: RosterDocument) -> Result<(), CoreError> {
    if document.owner_count() == 0 {
      return Err(CoreError::LastOwner);
    }
    let state = self.workspace()?;
    // After the genesis only an owner of the verified roster can sign the next one; anything
    // else would be refused by every client anyway.
    if state.roster.version > 0 && !self.is_verified_owner(&state) {
      return Err(CoreError::NotAnOwner);
    }
    document.version = state.roster.version + 1;
    document.maybe_prev_hash = Some(head_or_genesis(state.heads.maybe_roster_hash));
    document.maybe_signed_at_ms = Some(self.now_ms());
    let version = document.version;
    self
      .append_log(WorkspaceLogKind::Roster, WorkspaceLog::Roster, version, &document)
      .await?;
    let state = self.workspace()?;
    if state.policy_version > 0 {
      self.publish_policy(state.policy).await?;
    }
    Ok(())
  }

  /// An admin adds `user` to a conversation they aren't a member of: the server membership changes
  /// now, the signed change waits in the group's queue until a member's client commits it
  /// (admin.md §2.2). Adding oneself is subject to the policy's `admin_self_join`.
  pub async fn admin_add_member(
    &mut self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<(), CoreError> {
    self.cancel_removal_on_add(conversation, user)?;
    self
      .api
      .add_member(
        conversation,
        &AddConversationMemberRequest {
          user_id: user,
          maybe_role: None,
        },
      )
      .await?;
    let history = self.implicit_history_in(conversation, user).await?;
    self
      .post_admin_change(
        conversation,
        MembershipAction::AddUser {
          user_id: convert::user(user),
          history,
        },
      )
      .await
      .map(|_| ())
  }

  /// Commits what's waiting in the pending queue. Changes that turned out moot or invalid are
  /// dropped (and logged); ones that failed for network reasons stay for the next round, as do
  /// changes that are not due yet.
  pub async fn apply_pending_changes(&mut self) -> Result<(), CoreError> {
    for (key, value) in self.store.list(Table::PendingChanges)? {
      let change: PendingChange =
        serde_json::from_slice(&value).map_err(|error| CoreError::Corrupt(error.to_string()))?;
      match self.apply_pending(&change).await {
        Ok(PendingOutcome::Done) => self.store.delete(Table::PendingChanges, &key)?,
        Ok(PendingOutcome::Later(updated)) => {
          records::put_json(&self.store, Table::PendingChanges, &key, &updated)?;
        },
        Err(error) if error.is_transient() => return Err(error),
        Err(error) => {
          tracing::warn!(%error, "dropped a pending membership change");
          self.store.delete(Table::PendingChanges, &key)?;
        },
      }
    }
    Ok(())
  }

  pub(crate) fn queue_pending(&self, change: PendingChange) -> Result<(), CoreError> {
    let key = match &change {
      PendingChange::Delegated {
        conversation_id,
        change,
      } => format!("delegated/{conversation_id}/{}", change_id_hex(change.as_slice())),
      PendingChange::RemoveUser {
        conversation_id,
        user_id,
      } => format!("remove_user/{conversation_id}/{user_id}"),
      PendingChange::RemoveDevice {
        conversation_id,
        device_id,
        ..
      } => format!("remove_device/{conversation_id}/{device_id}"),
      PendingChange::AddDevice {
        conversation_id,
        device_id,
        ..
      } => format!("add_device/{conversation_id}/{device_id}"),
      PendingChange::Rejoin {
        conversation_id,
        device_id,
        ..
      } => PendingChange::rejoin_key(*conversation_id, *device_id),
      PendingChange::ShareHistory {
        conversation_id,
        share,
      } => format!("share_history/{conversation_id}/{share:?}"),
      PendingChange::ShareCurrentKey {
        conversation_id,
        device_id,
        ..
      } => PendingChange::current_key_share_key(*conversation_id, *device_id),
      PendingChange::Removal {
        conversation_id,
        user_id,
        ..
      } => PendingChange::removal_key(*conversation_id, *user_id),
      PendingChange::Revocation { device_id, .. } => PendingChange::revocation_key(*device_id),
      PendingChange::RevocationCheck { device_id, .. } => PendingChange::revocation_check_key(*device_id),
      PendingChange::AddUser {
        conversation_id,
        user_id,
        ..
      } => PendingChange::user_add_key(*conversation_id, *user_id),
    };
    records::put_json(&self.store, Table::PendingChanges, &key, &change)
  }

  /// Queues adding a member's new device to every group this device shares with that member. The
  /// commit is delayed by this device's rank among the group's devices, so one member commits and
  /// the rest only check. Devices of users who are online rank first (an offline device at rank 0
  /// would hold everybody up); the delay is capped, so a new device waits at most
  /// `device_add_max_rank` × `device_add_stagger`.
  pub(crate) fn queue_device_addition(&mut self, user: UserId, device: DeviceId) -> Result<(), CoreError> {
    // A user this device added before they had a device can be added now.
    self.remember_has_devices(user)?;
    self.wake_user_additions(user)?;
    let conversations: Vec<ConversationId> = self.cached_conversations()?.into_keys().collect();
    for conversation in conversations {
      self.queue_device_addition_in(conversation, user, device)?;
    }
    Ok(())
  }

  /// [`Self::queue_device_addition`] for one conversation: if this device is in its group, the
  /// user has a device there and `device` isn't one yet.
  pub(crate) fn queue_device_addition_in(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    device: DeviceId,
  ) -> Result<(), CoreError> {
    if device == self.account.device_id || !self.has_group(conversation)? {
      return Ok(());
    }
    let crypto_user = convert::user(user);
    let members = self.group_or_error(conversation)?.members()?;
    if !members.iter().any(|member| member.user_id == crypto_user) {
      return Ok(());
    }
    let mut candidates = Vec::with_capacity(members.len());
    for member in &members {
      let member_device = convert::device_from_crypto(member.device_id)?;
      if member_device == device {
        // Already in the group.
        return Ok(());
      }
      let member_user = convert::user_from_crypto(member.user_id)?;
      candidates.push((member_device, self.is_probably_online(member_user)?));
    }
    let now = self.now_ms();
    let stagger_ms = i64::try_from(self.settings.device_add_stagger.as_millis()).unwrap_or(i64::MAX);
    let rank =
      commit_rank(&candidates, self.account.device_id, device).min(self.settings.device_add_max_rank);
    self.queue_pending(PendingChange::AddDevice {
      conversation_id: conversation,
      user_id: user,
      device_id: device,
      not_before_ms: now.saturating_add(stagger_ms.saturating_mul(i64::from(rank))),
      attempts: 0,
    })
  }

  /// How long this device waits before it adds back `device` (a rejoin): ranked among the group's
  /// other devices like [`Self::queue_device_addition`], so normally one member commits.
  pub(crate) fn commit_delay_ms(
    &mut self,
    conversation: ConversationId,
    device: DeviceId,
  ) -> Result<i64, CoreError> {
    let members = self.group_or_error(conversation)?.members()?;
    let mut candidates = Vec::with_capacity(members.len());
    for member in &members {
      let member_device = convert::device_from_crypto(member.device_id)?;
      if member_device != device {
        let member_user = convert::user_from_crypto(member.user_id)?;
        candidates.push((member_device, self.is_probably_online(member_user)?));
      }
    }
    let rank =
      commit_rank(&candidates, self.account.device_id, device).min(self.settings.device_add_max_rank);
    let stagger_ms = i64::try_from(self.settings.device_add_stagger.as_millis()).unwrap_or(i64::MAX);
    Ok(stagger_ms.saturating_mul(i64::from(rank)))
  }

  /// From the last presence the gateway reported; this device's own user counts as online.
  fn is_probably_online(&self, user: UserId) -> Result<bool, CoreError> {
    if user == self.account.user_id {
      return Ok(true);
    }
    let maybe_state: Option<PresenceState> =
      records::get_json(&self.store, Table::Presence, &user.to_string())?;
    Ok(matches!(
      maybe_state,
      Some(PresenceState::Online | PresenceState::Away | PresenceState::DoNotDisturb)
    ))
  }

  async fn apply_pending(&mut self, change: &PendingChange) -> Result<PendingOutcome, CoreError> {
    let source = match change {
      PendingChange::Delegated {
        conversation_id,
        change,
      } => {
        let signed = SignedMembershipChange::from_bytes(change.as_slice())?;
        (*conversation_id, ChangeSource::Delegated(Box::new(signed)))
      },
      PendingChange::RemoveUser { .. } | PendingChange::RemoveDevice { .. } => {
        tracing::info!("dropped a removal the server asked for without a signed change");
        return Ok(PendingOutcome::Done);
      },
      PendingChange::AddDevice { .. } => return self.apply_device_addition(change).await,
      PendingChange::Removal {
        conversation_id,
        user_id,
        step,
        not_before_ms,
        state,
        ..
      } => {
        // A failed one waits for the user's "retry" ([`Client::retry_removal`]).
        if matches!(state, RemovalState::Failed | RemovalState::Unconfirmed) || self.now_ms() < *not_before_ms
        {
          return Ok(PendingOutcome::Later(change.clone()));
        }
        let key = PendingChange::removal_key(*conversation_id, *user_id);
        // Dropped earlier in this round (the user was added again).
        if self.stored_pending(&key)?.is_none() {
          return Ok(PendingOutcome::Done);
        }
        let result = self
          .run_removal(*conversation_id, *user_id, *step, RemovalRun::Resumed)
          .await;
        return self.removal_outcome(&key, change, result);
      },
      PendingChange::Revocation {
        device_id,
        step,
        not_before_ms,
      } => {
        if self.now_ms() < *not_before_ms {
          return Ok(PendingOutcome::Later(change.clone()));
        }
        let result = self.run_revocation(*device_id, *step).await;
        let key = PendingChange::revocation_key(*device_id);
        return self.removal_outcome(&key, change, result.map(|()| RemovalProgress::Done));
      },
      PendingChange::RevocationCheck { .. } => return self.apply_revocation_check(change).await,
      PendingChange::Rejoin { .. } => return self.apply_rejoin(change).await,
      PendingChange::AddUser { .. } => return self.apply_user_addition(change).await,
      PendingChange::ShareHistory {
        conversation_id,
        share,
      } => {
        if self.has_group(*conversation_id)? {
          self.share_history(*conversation_id, *share).await?;
        }
        return Ok(PendingOutcome::Done);
      },
      PendingChange::ShareCurrentKey { .. } => return self.apply_current_key_share(change).await,
    };
    self.commit_change(source.0, source.1).await?;
    Ok(PendingOutcome::Done)
  }

  /// What a removal or revocation round leads to: done, or kept with the step it reached when it
  /// waits for a commit, may work later, or failed after its group side was done (dropping it
  /// then would leave the server listing somebody the group removed; it is tried again later).
  fn removal_outcome(
    &mut self,
    key: &str,
    change: &PendingChange,
    result: Result<RemovalProgress, CoreError>,
  ) -> Result<PendingOutcome, CoreError> {
    let reached = self.stored_pending(key)?.unwrap_or_else(|| change.clone());
    match result {
      Ok(RemovalProgress::Done) => Ok(PendingOutcome::Done),
      Ok(RemovalProgress::Waiting) => Ok(PendingOutcome::Later(reached)),
      Err(error) if is_worth_retrying(&error) => {
        tracing::debug!(%error, "a removal waits for the next round");
        Ok(PendingOutcome::Later(reached))
      },
      Err(error) => match self.retry_later(reached) {
        Some(later) => {
          tracing::warn!(%error, "the server refused a removal whose group side is done; trying again later");
          Ok(PendingOutcome::Later(later))
        },
        None => Err(error),
      },
    }
  }

  /// Adds a member's new device once due. Done when the device is in the group (added here or by
  /// another member); retried later while the device has no key package or another commit won.
  async fn apply_device_addition(&mut self, change: &PendingChange) -> Result<PendingOutcome, CoreError> {
    let PendingChange::AddDevice {
      conversation_id,
      user_id,
      device_id,
      not_before_ms,
      attempts,
    } = change
    else {
      return Ok(PendingOutcome::Done);
    };
    let conversation = *conversation_id;
    let now = self.now_ms();
    if now < *not_before_ms {
      return Ok(PendingOutcome::Later(change.clone()));
    }
    if !self.has_group(conversation)? {
      return Ok(PendingOutcome::Done);
    }
    // Commits that arrived meanwhile may already contain the device.
    self.sync_mls(conversation).await?;
    if self.group_device_ids(conversation)?.contains(device_id) {
      return Ok(PendingOutcome::Done);
    }
    match self.add_missing_devices(conversation).await {
      Ok(_) => {},
      Err(CoreError::GaveUp { .. }) => {
        tracing::debug!(conversation_id = %conversation, "another member committed first")
      },
      Err(error) => return Err(error),
    }
    if self.group_device_ids(conversation)?.contains(device_id) {
      return Ok(PendingOutcome::Done);
    }
    let attempts = attempts.saturating_add(1);
    if attempts >= self.settings.device_add_attempts {
      tracing::warn!(conversation_id = %conversation, "gave up adding a new device (no key package)");
      return Ok(PendingOutcome::Done);
    }
    let retry_ms = i64::try_from(self.settings.device_add_stagger.as_millis()).unwrap_or(i64::MAX);
    Ok(PendingOutcome::Later(PendingChange::AddDevice {
      conversation_id: conversation,
      user_id: *user_id,
      device_id: *device_id,
      not_before_ms: now.saturating_add(retry_ms),
      attempts,
    }))
  }

  /// Adds back a device that asked for it once due; retried while it has no key package or
  /// another commit won, given up after `device_add_attempts`.
  async fn apply_rejoin(&mut self, change: &PendingChange) -> Result<PendingOutcome, CoreError> {
    let PendingChange::Rejoin {
      conversation_id,
      user_id,
      device_id,
      request,
      not_before_ms,
      attempts,
    } = change
    else {
      return Ok(PendingOutcome::Done);
    };
    let now = self.now_ms();
    if now < *not_before_ms {
      return Ok(PendingOutcome::Later(change.clone()));
    }
    if self
      .answer_rejoin(*conversation_id, *user_id, *device_id, request.as_slice())
      .await?
    {
      return Ok(PendingOutcome::Done);
    }
    let attempts = attempts.saturating_add(1);
    if attempts >= self.settings.device_add_attempts {
      tracing::warn!(conversation_id = %conversation_id, "gave up adding back a device that fell out of the group");
      return Ok(PendingOutcome::Done);
    }
    let retry_ms = i64::try_from(self.settings.device_add_stagger.as_millis()).unwrap_or(i64::MAX);
    Ok(PendingOutcome::Later(PendingChange::Rejoin {
      conversation_id: *conversation_id,
      user_id: *user_id,
      device_id: *device_id,
      request: request.clone(),
      not_before_ms: now.saturating_add(retry_ms),
      attempts,
    }))
  }

  /// Posts a signed membership change for the group's members to commit; returns the epoch it
  /// was signed for.
  pub(crate) async fn post_admin_change(
    &mut self,
    conversation: ConversationId,
    action: MembershipAction,
  ) -> Result<u64, CoreError> {
    // A non-member can't read the group's queue, so it learns the epoch from the conflict answer;
    // the change is signed for that epoch (security review A3). A member (leaving) starts from its
    // own.
    let mut epoch = match self.group(conversation)? {
      Some(group) => group.epoch(),
      None => 0,
    };
    for _ in 0..2 {
      let signed = MembershipChange::new(convert::conversation(conversation), epoch, action, &mut OsRng)
        .sign(&self.account.device, self.account.certificate.clone())?;
      let mut payload = ADMIN_CHANGE_MAGIC.to_vec();
      payload.extend_from_slice(&signed.to_bytes()?);
      let request = PostMlsMessageRequest {
        device_id: self.account.device_id,
        epoch,
        kind: MlsMessageKind::Proposal,
        payload: Base64Bytes::new(payload),
      };
      match self.api.post_mls_message(conversation, &request).await {
        Ok(_) => return Ok(epoch),
        Err(error) if error.is_epoch_conflict() => match current_epoch_of(&error) {
          Some(current) => epoch = current,
          None => return Err(error.into()),
        },
        Err(error) => return Err(error.into()),
      }
    }
    Err(CoreError::GaveUp {
      attempts: 2,
      reason: "the group's epoch kept changing".into(),
    })
  }

  async fn fetch_log(
    &self,
    kind: &WorkspaceLogKind,
    after: u64,
  ) -> Result<Vec<WorkspaceLogEntryResponse>, CoreError> {
    let mut entries = Vec::new();
    let mut cursor = after;
    loop {
      let page = self
        .api
        .list_workspace_log(
          kind,
          &ListWorkspaceLogQuery {
            maybe_after: Some(cursor),
            maybe_limit: None,
          },
        )
        .await?;
      let has_more = page.has_more;
      if let Some(last) = page.entries.last() {
        cursor = last.version;
      }
      let is_empty = page.entries.is_empty();
      entries.extend(page.entries);
      if !has_more || is_empty {
        return Ok(entries);
      }
    }
  }

  async fn append_log(
    &mut self,
    kind: WorkspaceLogKind,
    log: WorkspaceLog,
    version: u64,
    document: &impl Serialize,
  ) -> Result<(), CoreError> {
    let payload = serde_json::to_vec(document).map_err(|error| CoreError::Corrupt(error.to_string()))?;
    let signature = sign_log_entry(
      &self.account.device,
      &self.account.certificate,
      log,
      version,
      &payload,
    )?;
    let request = AppendWorkspaceLogRequest {
      expected_version: version - 1,
      payload: Base64Bytes::new(payload),
      signature: Base64Bytes::new(signature),
      signer_device_id: self.account.device_id,
    };
    self
      .api
      .append_workspace_log(&kind, &request)
      .await
      .map_err(|error| match error.error_code() {
        // The server's sanity check (an older client of this user may have built the roster).
        Some(ErrorCode::LastOwner) => CoreError::LastOwner,
        _ => error.into(),
      })?;
    self.sync_workspace().await
  }
}

/// The previous entry's hash for a new document; zeros for a log's first chained entry.
fn head_or_genesis(maybe_head: Option<Base64Bytes>) -> Base64Bytes {
  maybe_head.unwrap_or_else(|| Base64Bytes::new(vec![0; 32]))
}

/// Reads "the group is in epoch N" from an `epoch_conflict` answer (the body has no field for it
/// yet; crate README, server gaps).
pub(crate) fn current_epoch_of(error: &ApiError) -> Option<u64> {
  let ApiError::Status {
    maybe_body: Some(body),
    ..
  } = error
  else {
    return None;
  };
  body
    .message
    .split_whitespace()
    .find_map(|word| word.trim_matches(|c: char| !c.is_ascii_digit()).parse().ok())
}

/// The change ID of an encoded signed change (it follows the version byte): the queue key.
fn change_id_hex(bytes: &[u8]) -> String {
  bytes
    .iter()
    .skip(1)
    .take(16)
    .map(|byte| format!("{byte:02x}"))
    .collect()
}

/// This device's position among the group's devices for committing `new_device`: devices of online
/// users first, then by ID XOR the new device's ID, so the same member isn't always first. Every
/// member computes the same order from the same group and presence.
fn commit_rank(candidates: &[(DeviceId, bool)], own: DeviceId, new_device: DeviceId) -> u32 {
  let order = |device: DeviceId, is_online: bool| {
    (
      !is_online,
      device.snowflake().get() ^ new_device.snowflake().get(),
    )
  };
  let own_key = order(own, true);
  let ahead = candidates
    .iter()
    .filter(|(device, is_online)| *device != own && order(*device, *is_online) < own_key)
    .count();
  u32::try_from(ahead).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::Snowflake;

  use super::*;

  #[test]
  fn online_devices_rank_before_offline_ones_and_order_varies_per_new_device() {
    let (a, b, c) = (device(1), device(2), device(3));
    let all_online = [(a, true), (b, true), (c, true)];
    let ranks: Vec<u32> = [a, b, c]
      .iter()
      .map(|own| commit_rank(&all_online, *own, device(8)))
      .collect();
    let mut sorted = ranks.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, vec![0, 1, 2], "every device gets its own rank");

    // a would be first, but its user is offline: b and c go ahead of it.
    let a_offline = [(a, false), (b, true), (c, true)];
    assert_eq!(
      commit_rank(&a_offline, b, device(8)).min(commit_rank(&a_offline, c, device(8))),
      0
    );

    // Another new device mixes the order differently.
    assert_ne!(
      commit_rank(&all_online, a, device(8)),
      commit_rank(&all_online, a, device(10))
    );
  }

  fn device(raw: i64) -> DeviceId {
    DeviceId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
