//! Opting a private channel in to strict (e2ee.md §4.6.2, "Turning it on", steps 2 and 3, issue
//! #1924), and a workspace owner or admin outside the group signing a strict channel's roles.
//!
//! - **The ready step** (step 2, D6): a workspace owner or admin in the group asks ([`StrictAsk`],
//!   a proposal in the group's queue like a device-add request). Every device of this version
//!   answers with a [`SignedStrictReady`], signed with the key its leaf signs MLS with. The asker's
//!   app lists the devices whose leaf lacks the capability and that haven't answered
//!   ([`Client::strict_readiness`]), with removing a stale one on offer
//!   ([`Client::remove_strict_blocker`]). Older apps pass over both entries.
//! - **The leaf refresh** (step 3): once every such device answered, each one refreshes its own
//!   leaf in an empty commit, ranked like a device addition (`device_add_stagger`). Never before:
//!   older apps refuse an empty commit (`MissingSignedChange`).
//! - **A workspace signer outside the group**: the server's hint gives the group's epoch and the
//!   current roles' version and hash ([`ServerApi::mls_strict_hint`]); the signed change goes into
//!   the queue as a proposal, and a member commits it like any roles change, ranked so one member
//!   does. The committer tells the server the new hint with the commit.
//!
//! Nothing here runs unless this build's leaves announce strict channels
//! ([`teamcraft_crypto::mls::announces_strict_roles`]): a device that doesn't implement the rule
//! never says it is ready.

use std::collections::BTreeSet;

use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use teamcraft_crypto::identity::IdentityPublicKey;
use teamcraft_crypto::membership::{IdentityDirectory as _, MembershipAction};
use teamcraft_crypto::mls::{MlsError, SignedStrictReady, StrictAsk, announces_strict_roles};
use teamcraft_crypto::strict_roles::{
  STRICT_CHANGE_WINDOW_EPOCHS, SignedStrictRolesChange, StrictHash, StrictRole, StrictRoleKind,
  StrictRolesChange, StrictRolesError, StrictSignerRole,
};
use teamcraft_proto::Id as _;
use teamcraft_proto::{
  Base64Bytes, ConversationId, ConversationKind, ConversationVisibility, DeviceId, MlsMessageKind,
  MlsMessageResponse, PostMlsMessageRequest, StrictChannelHint, UserId,
};

use super::Client;
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::retry::{RetryPolicy, millis};
use crate::store::{LocalStore, Table};

/// Marks an MLS "proposal" entry that asks the group's devices whether they are ready for strict
/// channels ([`StrictAsk`]).
pub(crate) const STRICT_ASK_MAGIC: &[u8] = b"TCSA";
/// Marks an MLS "proposal" entry with a device's signed answer ([`SignedStrictReady`]).
pub(crate) const STRICT_READY_MAGIC: &[u8] = b"TCSY";
/// Marks an MLS "proposal" entry with a strict roles change a workspace owner or admin outside the
/// group signed, for a member to commit ([`SignedStrictRolesChange`]).
pub(crate) const STRICT_ROLES_PROPOSAL_MAGIC: &[u8] = b"TCSP";

/// Where the conversations with an open step of this module are listed.
const OPEN_KEY: &str = "strict-open";

/// How far a channel is on its way to strict, as this device sees it ([`Client::strict_readiness`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrictReadiness {
  /// The newest ask this device took, if any.
  pub maybe_ask: Option<StrictAskStatus>,
  /// Devices whose leaf doesn't announce strict channels and that haven't answered the ask: an
  /// older app, or not online since (D6). Each blocks turning strict on until it answers or is
  /// removed ([`Client::remove_strict_blocker`]).
  pub waiting: Vec<(UserId, DeviceId)>,
  /// Devices that answered and haven't refreshed their leaf yet: they will, once nobody waits.
  pub refreshing: Vec<(UserId, DeviceId)>,
  /// Every leaf announces strict channels: the channel can be turned strict now.
  pub is_ready: bool,
}

/// An ask this device took ([`StrictReadiness::maybe_ask`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrictAskStatus {
  pub asker: UserId,
  /// The server's time of the ask.
  pub asked_at_ms: i64,
}

/// The newest ask of a conversation and who answered it, kept per device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ReadyRecord {
  /// The encoded [`StrictAsk`]: this device answers it, and answers name it.
  ask: Base64Bytes,
  asker: UserId,
  asked_at_ms: i64,
  /// This device posted its answer.
  #[serde(default)]
  is_answered: bool,
  /// The devices whose signed answer verified against their leaf.
  #[serde(default)]
  ready: BTreeSet<DeviceId>,
  /// When this device refreshes its leaf: set once every device that lacks the capability
  /// answered, ranked among them.
  #[serde(rename = "refresh_at_ms", default, skip_serializing_if = "Option::is_none")]
  maybe_refresh_at_ms: Option<i64>,
}

/// A roles change a workspace signer outside the group posted, waiting for this device to commit
/// it unless another member does first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct DelegatedRoles {
  change: Base64Bytes,
  not_before_ms: i64,
  /// Tries that left it for later, for the backoff.
  #[serde(default)]
  attempts: u32,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Asks the channel's devices whether they are ready for strict channels (e2ee.md §4.6.2, step
  /// 2): a workspace owner or admin (D3) whose device is in the group. Nothing changes yet; the
  /// devices answer, and once all did they refresh their leaves. [`Self::strict_readiness`] shows
  /// how far they are.
  pub async fn ask_strict_ready(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let _step = crate::timing::step("ask_strict_ready");
    self.ensure_strict_capable(conversation)?;
    if !announces_strict_roles() {
      return Err(CoreError::StrictNotNow(
        conversation,
        "this app doesn't implement strict channels",
      ));
    }
    if !self.has_group(conversation)? {
      return Err(CoreError::NoGroup(conversation));
    }
    if self.strict_stop(conversation)?.is_some() {
      return Err(CoreError::StrictStopped(conversation));
    }
    if self.is_strict(conversation)? {
      return Err(CoreError::StrictNotNow(
        conversation,
        "the channel is strict already",
      ));
    }
    self.sync_workspace().await?;
    let me = self.account.user_id;
    let workspace = self.workspace()?;
    if !self.fresh_admins(&workspace, self.now_ms()).0.contains(&me) {
      return Err(CoreError::StrictNotNow(
        conversation,
        "only a workspace owner or admin asks",
      ));
    }
    let ask = StrictAsk::new(convert::conversation(conversation), convert::user(me), &mut OsRng);
    let bytes = ask.to_bytes()?;
    let attempts = self.settings.max_commit_attempts.max(1);
    for _ in 0..attempts {
      let epoch = self.group_or_error(conversation)?.epoch();
      let mut payload = STRICT_ASK_MAGIC.to_vec();
      payload.extend_from_slice(&bytes);
      let request = PostMlsMessageRequest {
        device_id: self.account.device_id,
        epoch,
        kind: MlsMessageKind::Proposal,
        payload: Base64Bytes::new(payload),
      };
      match self.api.post_mls_message(conversation, &request).await {
        Ok(posted) => {
          let record = ReadyRecord {
            ask: Base64Bytes::new(bytes),
            asker: me,
            asked_at_ms: posted.id.snowflake().unix_millis(),
            is_answered: false,
            ready: BTreeSet::new(),
            maybe_refresh_at_ms: None,
          };
          self.put_ready_record(conversation, &record)?;
          tracing::info!(conversation_id = %conversation, "asked the channel's devices whether they are ready for strict");
          return Box::pin(self.run_strict_tasks()).await;
        },
        Err(error) if error.is_epoch_conflict() => self.sync_mls(conversation).await?,
        Err(error) => return Err(error.into()),
      }
    }
    Err(CoreError::GaveUp {
      attempts,
      reason: "the group kept moving on".into(),
    })
  }

  /// How far the channel is on its way to strict (e2ee.md §4.6.2, steps 2 and 3, D6): who still
  /// has to answer, who still has to refresh, and whether it can be turned on. For a device in the
  /// group.
  pub fn strict_readiness(&mut self, conversation: ConversationId) -> Result<StrictReadiness, CoreError> {
    let lacking = self.leaves_lacking_strict(conversation)?;
    let maybe_record = self.ready_record(conversation)?;
    let ready = maybe_record
      .as_ref()
      .map(|record| record.ready.clone())
      .unwrap_or_default();
    let (refreshing, waiting) = lacking
      .iter()
      .copied()
      .partition(|(_, device)| ready.contains(device));
    Ok(StrictReadiness {
      maybe_ask: maybe_record.map(|record| StrictAskStatus {
        asker: record.asker,
        asked_at_ms: record.asked_at_ms,
      }),
      waiting,
      refreshing,
      is_ready: lacking.is_empty(),
    })
  }

  /// Removes a device that blocks turning strict on ([`StrictReadiness::waiting`], D6): an older
  /// app or one not online since the ask. A signed removal of that device, judged by the members
  /// like any; the user's other devices stay.
  pub async fn remove_strict_blocker(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    device: DeviceId,
  ) -> Result<(), CoreError> {
    let _step = crate::timing::step("remove_strict_blocker");
    let is_waiting = self
      .strict_readiness(conversation)?
      .waiting
      .contains(&(user, device));
    if !is_waiting {
      return Err(CoreError::StrictNotNow(
        conversation,
        "the device doesn't block turning strict on",
      ));
    }
    Box::pin(self.commit_change(
      conversation,
      super::groups::ChangeSource::Own(MembershipAction::RemoveDevice {
        user_id: convert::user(user),
        device_id: convert::device(device),
      }),
    ))
    .await?;
    self.schedule_strict_refresh(conversation)
  }

  /// A strict channel's roles change signed by this workspace owner or admin, who has no device in
  /// the group (e2ee.md §4.6.2, "Role changes while strict"): built on the server's hint of the
  /// current roles and the group's epoch, and posted for a member to commit. A wrong hint only gets
  /// the change refused by the members.
  pub(super) async fn post_strict_roles_from_outside(
    &mut self,
    conversation: ConversationId,
    roles: &[(UserId, StrictRoleKind)],
  ) -> Result<(), CoreError> {
    let response = self.api.conversation(conversation).await?;
    let is_private_channel =
      response.kind == ConversationKind::Channel && response.visibility == ConversationVisibility::Private;
    if !is_private_channel && !roles.is_empty() {
      return Err(CoreError::NotStrictCapable(conversation));
    }
    self.sync_workspace().await?;
    let me = self.account.user_id;
    let workspace = self.workspace()?;
    let (admins, roster) = self.fresh_admins(&workspace, self.now_ms());
    if !admins.contains(&me) {
      return Err(CoreError::StrictNotNow(
        conversation,
        "only a workspace owner or admin signs from outside the group",
      ));
    }
    let directory = self
      .trusted_directory(roles.iter().map(|(user, _)| *user).chain([me]))
      .await?;
    let mut strict_roles = Vec::with_capacity(roles.len());
    for (user, role) in roles {
      let identity_key: IdentityPublicKey = directory
        .pinned_identity(convert::user(*user))
        .ok_or(CoreError::UnknownIdentity(*user))?;
      strict_roles.push(StrictRole {
        user_id: convert::user(*user),
        role: *role,
        identity_key,
      });
    }
    let (role_log_version, role_log_hash) = self.role_log_mirror(conversation)?;
    let attempts = self.settings.max_commit_attempts.max(1);
    for _ in 0..attempts {
      let hint = self.api.mls_strict_hint(conversation).await?;
      let (version, prev_hash) = match &hint.maybe_strict {
        Some(current) if current.is_strict => (current.version.saturating_add(1), hint_hash(current)?),
        // Turning on starts a chain at any version (above the last one the server names).
        Some(last) if !roles.is_empty() => (last.version.saturating_add(1), StrictHash::ZERO),
        None if !roles.is_empty() => (1, StrictHash::ZERO),
        _ => return Err(CoreError::StrictNotNow(conversation, "the channel isn't strict")),
      };
      let signed = StrictRolesChange {
        conversation_id: convert::conversation(conversation),
        version,
        prev_hash,
        signed_for_epoch: hint.epoch,
        signer_role: StrictSignerRole::Workspace,
        roster_version: roster.version,
        role_log_version,
        role_log_hash,
        roles: strict_roles.clone(),
      }
      .sign(&self.account.device, &self.account.certificate)
      .map_err(MlsError::from)?;
      let mut payload = STRICT_ROLES_PROPOSAL_MAGIC.to_vec();
      payload.extend_from_slice(&signed.to_bytes().map_err(MlsError::from)?);
      let request = PostMlsMessageRequest {
        device_id: self.account.device_id,
        epoch: hint.epoch,
        kind: MlsMessageKind::Proposal,
        payload: Base64Bytes::new(payload),
      };
      match self.api.post_mls_message(conversation, &request).await {
        Ok(_) => {
          tracing::info!(conversation_id = %conversation, version, "posted a strict roles change for the members to commit");
          return Ok(());
        },
        Err(error) if error.is_epoch_conflict() => {},
        Err(error) => return Err(error.into()),
      }
    }
    Err(CoreError::GaveUp {
      attempts,
      reason: "the group kept moving on".into(),
    })
  }

  /// Takes a strict ask, answer or outside signer's change from the group's queue (e2ee.md §4.6.2).
  /// Nothing here stops the queue: an entry that doesn't check out is logged and passed over; only
  /// a network error holds it for the next pass.
  pub(super) async fn note_strict_proposal(&mut self, message: &MlsMessageResponse) -> Result<(), CoreError> {
    if !announces_strict_roles() {
      return Ok(());
    }
    let payload = message.payload.as_slice();
    let result = if let Some(ask) = payload.strip_prefix(STRICT_ASK_MAGIC) {
      self.note_strict_ask(message, ask).await
    } else if let Some(ready) = payload.strip_prefix(STRICT_READY_MAGIC) {
      self.note_strict_ready(message, ready)
    } else if let Some(change) = payload.strip_prefix(STRICT_ROLES_PROPOSAL_MAGIC) {
      self.note_delegated_strict_roles(message, change)
    } else {
      return Ok(());
    };
    match result {
      Err(error) if error.is_transient() => Err(error),
      Err(error) => {
        tracing::warn!(conversation_id = %message.conversation_id, mls_message_id = %message.id, %error, "passed over a strict channel entry");
        Ok(())
      },
      Ok(()) => Ok(()),
    }
  }

  /// Runs what this module has open: answering an ask, refreshing this device's leaf when due, and
  /// committing an outside signer's roles change when due. Called with the pending changes.
  pub(super) async fn run_strict_tasks(&mut self) -> Result<(), CoreError> {
    if !announces_strict_roles() {
      return Ok(());
    }
    let open: BTreeSet<ConversationId> =
      records::get_json(&self.store, Table::AppliedChanges, OPEN_KEY)?.unwrap_or_default();
    for conversation in open {
      let is_open = match Box::pin(self.run_strict_tasks_in(conversation)).await {
        Ok(is_open) => is_open,
        Err(error) if error.is_transient() => return Err(error),
        Err(error) => {
          tracing::warn!(conversation_id = %conversation, %error, "dropped a strict channel step");
          false
        },
      };
      if !is_open {
        self.close_strict_tasks(conversation)?;
      }
    }
    Ok(())
  }

  /// The hint this device's roles commit tells the server (e2ee.md §4.6.2), taken once.
  pub(super) fn take_strict_hint(&mut self, conversation: ConversationId) -> Option<StrictChannelHint> {
    match self.maybe_strict_hint.take() {
      Some((hinted, hint)) if hinted == conversation => Some(hint),
      other => {
        self.maybe_strict_hint = other;
        None
      },
    }
  }

  /// Keeps the hint of a roles change for the commit this device is about to post.
  pub(super) fn hint_strict_roles(
    &mut self,
    conversation: ConversationId,
    signed: &SignedStrictRolesChange,
  ) -> Result<(), CoreError> {
    let change = signed.change();
    let hash = change.hash().map_err(MlsError::from)?;
    self.maybe_strict_hint = Some((
      conversation,
      StrictChannelHint {
        is_strict: !change.roles.is_empty(),
        version: change.version,
        hash: Base64Bytes::new(hash.as_bytes().to_vec()),
      },
    ));
    Ok(())
  }

  /// An ask in the queue: taken if this device is in the group of a private channel, and the asker
  /// is a workspace owner or admin of a fresh roster (D3) whose signed device list names the device
  /// that posted it. The newest ask wins; its answers start over.
  async fn note_strict_ask(&mut self, message: &MlsMessageResponse, bytes: &[u8]) -> Result<(), CoreError> {
    let conversation = message.conversation_id;
    if !self.has_group(conversation)? || self.ensure_strict_capable(conversation).is_err() {
      return Ok(());
    }
    let ask = StrictAsk::from_bytes(bytes, convert::conversation(conversation))?;
    let asked_at_ms = message.id.snowflake().unix_millis();
    if let Some(known) = self.ready_record(conversation)?
      && (known.ask.as_slice() == bytes || known.asked_at_ms > asked_at_ms)
    {
      return Ok(());
    }
    let asker = convert::user_from_crypto(ask.asker())?;
    let directory = self.trusted_directory([asker]).await?;
    let is_listed = self.stored_device_lists(&directory, &[asker]).iter().any(|list| {
      list
        .devices()
        .iter()
        .any(|entry| entry.device_id == convert::device(message.sender_device_id))
    });
    if !is_listed {
      return Err(CoreError::StrictNotNow(
        conversation,
        "an ask from a device its asker doesn't list",
      ));
    }
    if !self.is_fresh_workspace_admin(asker, asked_at_ms)? {
      // The roster may be behind: read it once.
      self.sync_workspace().await?;
      if !self.is_fresh_workspace_admin(asker, asked_at_ms)? {
        return Err(CoreError::StrictNotNow(
          conversation,
          "an ask from somebody who isn't a workspace owner or admin",
        ));
      }
    }
    let record = ReadyRecord {
      ask: Base64Bytes::new(bytes.to_vec()),
      asker,
      asked_at_ms,
      is_answered: false,
      ready: BTreeSet::new(),
      maybe_refresh_at_ms: None,
    };
    self.put_ready_record(conversation, &record)?;
    tracing::info!(conversation_id = %conversation, "a workspace admin asked whether this channel's devices are ready for strict");
    Ok(())
  }

  /// A device's answer in the queue: counted if it answers the newest ask and verifies against
  /// the signature key of that device's leaf.
  fn note_strict_ready(&mut self, message: &MlsMessageResponse, bytes: &[u8]) -> Result<(), CoreError> {
    let conversation = message.conversation_id;
    let Some(mut record) = self.ready_record(conversation)? else {
      return Ok(());
    };
    if !self.has_group(conversation)? {
      return Ok(());
    }
    let signed = SignedStrictReady::from_bytes(bytes)?;
    let ask = StrictAsk::from_bytes(record.ask.as_slice(), convert::conversation(conversation))?;
    if signed.unverified().ask_id() != ask.ask_id() {
      // An answer to an earlier ask.
      return Ok(());
    }
    let ready = self.group_or_error(conversation)?.verify_strict_ready(&signed)?;
    let device = convert::device_from_crypto(ready.device().device_id)?;
    if record.ready.insert(device) {
      self.put_ready_record(conversation, &record)?;
      self.schedule_strict_refresh(conversation)?;
    }
    Ok(())
  }

  /// An outside signer's roles change in the queue: kept for this device to commit after its rank
  /// among the group's devices, so normally one member commits and the rest find it done.
  fn note_delegated_strict_roles(
    &mut self,
    message: &MlsMessageResponse,
    bytes: &[u8],
  ) -> Result<(), CoreError> {
    let conversation = message.conversation_id;
    if !self.has_group(conversation)? {
      return Ok(());
    }
    let signed = SignedStrictRolesChange::from_bytes(bytes).map_err(MlsError::from)?;
    if signed.change().conversation_id != convert::conversation(conversation) {
      return Err(CoreError::StrictNotNow(
        conversation,
        "a roles change of another channel",
      ));
    }
    let delay_ms = self.commit_delay_ms(conversation, message.sender_device_id)?;
    let delegated = DelegatedRoles {
      change: Base64Bytes::new(bytes.to_vec()),
      not_before_ms: self.now_ms().saturating_add(delay_ms),
      attempts: 0,
    };
    records::put_json(
      &self.store,
      Table::AppliedChanges,
      &delegated_key(conversation),
      &delegated,
    )?;
    self.open_strict_tasks(conversation)
  }

  /// One conversation's open steps; whether any is left.
  async fn run_strict_tasks_in(&mut self, conversation: ConversationId) -> Result<bool, CoreError> {
    if !self.has_group(conversation)? || self.strict_stop(conversation)?.is_some() {
      // Out of the group, or stopped (D4): nothing of this goes out.
      return self.has_group(conversation);
    }
    let mut is_open = false;
    if let Some(record) = self.ready_record(conversation)? {
      if !record.is_answered {
        self.answer_strict_ask(conversation, record).await?;
      }
      is_open |= self.refresh_leaf_if_due(conversation).await?;
    }
    is_open |= self.commit_delegated_roles_if_due(conversation).await?;
    Ok(is_open)
  }

  /// Posts this device's signed answer to the newest ask; the group moved on: next pass.
  async fn answer_strict_ask(
    &mut self,
    conversation: ConversationId,
    mut record: ReadyRecord,
  ) -> Result<(), CoreError> {
    let ask = StrictAsk::from_bytes(record.ask.as_slice(), convert::conversation(conversation))?;
    let answer = ask.answer(&self.account.device);
    let mut payload = STRICT_READY_MAGIC.to_vec();
    payload.extend_from_slice(&answer.to_bytes()?);
    let request = PostMlsMessageRequest {
      device_id: self.account.device_id,
      epoch: self.group_or_error(conversation)?.epoch(),
      kind: MlsMessageKind::Proposal,
      payload: Base64Bytes::new(payload),
    };
    match self.api.post_mls_message(conversation, &request).await {
      Ok(_) => {},
      Err(error) if error.is_epoch_conflict() => return Ok(()),
      Err(error) => return Err(error.into()),
    }
    record.is_answered = true;
    // The own answer counts at once; the queue brings it again, and it verifies the same.
    record.ready.insert(self.account.device_id);
    self.put_ready_record(conversation, &record)?;
    tracing::info!(conversation_id = %conversation, "answered that this device is ready for strict");
    self.schedule_strict_refresh(conversation)
  }

  /// Sets when this device refreshes its leaf (step 3): only when its leaf lacks the capability
  /// and every device whose leaf lacks it answered the newest ask; ranked among those devices by
  /// ID, so they don't all commit for the same epoch.
  pub(super) fn schedule_strict_refresh(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let Some(mut record) = self.ready_record(conversation)? else {
      return Ok(());
    };
    let lacking = self.leaves_lacking_strict(conversation)?;
    let own = self.account.device_id;
    let is_lacking = lacking.iter().any(|(_, device)| *device == own);
    let is_everybody_ready = lacking.iter().all(|(_, device)| record.ready.contains(device));
    let maybe_refresh_at_ms = if is_lacking && is_everybody_ready {
      let rank = lacking.iter().filter(|(_, device)| *device < own).count();
      let rank = u32::try_from(rank)
        .unwrap_or(u32::MAX)
        .min(self.settings.device_add_max_rank);
      let stagger_ms = i64::try_from(self.settings.device_add_stagger.as_millis()).unwrap_or(i64::MAX);
      let due_ms = self
        .now_ms()
        .saturating_add(stagger_ms.saturating_mul(i64::from(rank)));
      Some(
        record
          .maybe_refresh_at_ms
          .map_or(due_ms, |known| known.min(due_ms)),
      )
    } else {
      None
    };
    if record.maybe_refresh_at_ms != maybe_refresh_at_ms {
      record.maybe_refresh_at_ms = maybe_refresh_at_ms;
      self.put_ready_record(conversation, &record)?;
    }
    // An answer came in or the tree changed: the asker's list of waiting devices moves.
    self.note_strict_view(conversation)
  }

  /// Refreshes this device's leaf when due; whether a refresh is still to come.
  async fn refresh_leaf_if_due(&mut self, conversation: ConversationId) -> Result<bool, CoreError> {
    self.schedule_strict_refresh(conversation)?;
    let Some(record) = self.ready_record(conversation)? else {
      return Ok(false);
    };
    let Some(due_ms) = record.maybe_refresh_at_ms else {
      return Ok(!record.is_answered);
    };
    if self.now_ms() < due_ms {
      return Ok(true);
    }
    self.resolve_pending_commit(conversation).await?;
    let group = self
      .groups
      .get_mut(&conversation)
      .ok_or(CoreError::NoGroup(conversation))?;
    let output = group.commit_leaf_refresh(&self.provider, &self.account.device)?;
    if self.post_commit(conversation, output, &[]).await?.is_some() {
      tracing::info!(conversation_id = %conversation, "refreshed this device's leaf for strict channels");
      self.schedule_strict_refresh(conversation)?;
      return Ok(false);
    }
    // Another commit won the epoch: catch up and try again after the stagger.
    self.sync_mls(conversation).await?;
    let stagger_ms = i64::try_from(self.settings.device_add_stagger.as_millis()).unwrap_or(i64::MAX);
    if let Some(mut record) = self.ready_record(conversation)?
      && record.maybe_refresh_at_ms.is_some()
    {
      record.maybe_refresh_at_ms = Some(self.now_ms().saturating_add(stagger_ms));
      self.put_ready_record(conversation, &record)?;
    }
    Ok(true)
  }

  /// Commits an outside signer's roles change when due, unless the group has it already;
  /// whether it is still to come.
  async fn commit_delegated_roles_if_due(&mut self, conversation: ConversationId) -> Result<bool, CoreError> {
    let Some(delegated) =
      records::get_json::<DelegatedRoles>(&self.store, Table::AppliedChanges, &delegated_key(conversation))?
    else {
      return Ok(false);
    };
    if self.now_ms() < delegated.not_before_ms {
      return Ok(true);
    }
    let signed = SignedStrictRolesChange::from_bytes(delegated.change.as_slice()).map_err(MlsError::from)?;
    let outcome = Box::pin(self.commit_delegated_roles(conversation, &signed)).await;
    let is_kept = match outcome {
      Ok(is_kept) => is_kept,
      Err(error) if error.is_transient() => return Err(error),
      Err(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "dropped a workspace admin's strict roles change");
        false
      },
    };
    if is_kept {
      // Backed off from `device_add_stagger` up to `rejoin_retry_max`: a channel whose devices
      // aren't ready yet doesn't cost a commit attempt at every pass.
      let policy = RetryPolicy::new(self.settings.device_add_stagger, self.settings.rejoin_retry_max);
      let attempts = delegated.attempts.saturating_add(1);
      let later = DelegatedRoles {
        not_before_ms: self
          .now_ms()
          .saturating_add(millis(policy.backoff(attempts.saturating_sub(1)))),
        attempts,
        ..delegated
      };
      records::put_json(
        &self.store,
        Table::AppliedChanges,
        &delegated_key(conversation),
        &later,
      )?;
    } else {
      self
        .store
        .delete(Table::AppliedChanges, &delegated_key(conversation))?;
    }
    Ok(is_kept)
  }

  /// One try at committing an outside signer's change: `Ok(false)` once it is in the group (by
  /// this commit or another member's) or can't be any more, `Ok(true)` to try again later (the
  /// epoch was taken, the roster has to be read, or a leaf still lacks the capability while the
  /// change is fresh).
  async fn commit_delegated_roles(
    &mut self,
    conversation: ConversationId,
    signed: &SignedStrictRolesChange,
  ) -> Result<bool, CoreError> {
    let change = signed.change();
    let maybe_current = self.strict_roles(conversation)?;
    let is_done = match &maybe_current {
      Some(current) => current.version() >= change.version,
      None => change.roles.is_empty(),
    };
    if is_done {
      return Ok(false);
    }
    self.resolve_pending_commit(conversation).await?;
    let epoch = self.group_or_error(conversation)?.epoch();
    if epoch.saturating_sub(change.signed_for_epoch) > STRICT_CHANGE_WINDOW_EPOCHS {
      return Err(
        MlsError::from(StrictRolesError::Stale {
          signed_epoch: change.signed_for_epoch,
          current_epoch: epoch,
        })
        .into(),
      );
    }
    let mut users = self.group_users(conversation)?;
    for role in &change.roles {
      users.push(convert::user_from_crypto(role.user_id)?);
    }
    let signer = signed
      .claimed_signer()
      .map_err(|error| MlsError::from(StrictRolesError::Signature(error)))?;
    users.push(convert::user_from_crypto(signer)?);
    let directory = self.trusted_directory(users).await?;
    let authority = self.group_authority(conversation, self.now_ms())?;
    let group = self
      .groups
      .get_mut(&conversation)
      .ok_or(CoreError::NoGroup(conversation))?;
    let output = match group.commit_strict_roles(
      &self.provider,
      &self.account.device,
      signed,
      &directory,
      &authority,
    ) {
      Ok(output) => output,
      Err(error) if error.is_strict_retry() => {
        self.sync_workspace().await?;
        return Ok(true);
      },
      // openmls refuses the commit while a leaf lacks the capability: kept while it is fresh.
      Err(error @ MlsError::Openmls { .. }) => {
        tracing::info!(conversation_id = %conversation, %error, "a workspace admin's strict roles change waits");
        return Ok(true);
      },
      Err(error) => return Err(error.into()),
    };
    self.hint_strict_roles(conversation, signed)?;
    if self.post_commit(conversation, output, &[]).await?.is_some() {
      self.note_known_strict_roles(conversation)?;
      tracing::info!(conversation_id = %conversation, version = change.version, "committed a workspace admin's strict roles change");
      return Ok(false);
    }
    self.sync_mls(conversation).await?;
    Ok(true)
  }

  /// The devices in the conversation's group whose leaf doesn't announce strict channels.
  fn leaves_lacking_strict(
    &mut self,
    conversation: ConversationId,
  ) -> Result<Vec<(UserId, DeviceId)>, CoreError> {
    let mut lacking = Vec::new();
    for member in self.group_or_error(conversation)?.leaves_without_strict()? {
      lacking.push((
        convert::user_from_crypto(member.user_id)?,
        convert::device_from_crypto(member.device_id)?,
      ));
    }
    lacking.sort_by_key(|(_, device)| *device);
    Ok(lacking)
  }

  fn is_fresh_workspace_admin(&self, user: UserId, at_ms: i64) -> Result<bool, CoreError> {
    let workspace = self.workspace()?;
    Ok(self.fresh_admins(&workspace, at_ms).0.contains(&user))
  }

  fn ready_record(&self, conversation: ConversationId) -> Result<Option<ReadyRecord>, CoreError> {
    records::get_json(&self.store, Table::AppliedChanges, &ready_key(conversation))
  }

  fn put_ready_record(
    &mut self,
    conversation: ConversationId,
    record: &ReadyRecord,
  ) -> Result<(), CoreError> {
    records::put_json(
      &self.store,
      Table::AppliedChanges,
      &ready_key(conversation),
      record,
    )?;
    self.open_strict_tasks(conversation)
  }

  fn open_strict_tasks(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let mut open: BTreeSet<ConversationId> =
      records::get_json(&self.store, Table::AppliedChanges, OPEN_KEY)?.unwrap_or_default();
    if open.insert(conversation) {
      records::put_json(&self.store, Table::AppliedChanges, OPEN_KEY, &open)?;
    }
    Ok(())
  }

  /// Nothing more to do in the conversation: its ready record stays for the status, the list of
  /// open conversations forgets it.
  fn close_strict_tasks(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let mut open: BTreeSet<ConversationId> =
      records::get_json(&self.store, Table::AppliedChanges, OPEN_KEY)?.unwrap_or_default();
    if open.remove(&conversation) {
      records::put_json(&self.store, Table::AppliedChanges, OPEN_KEY, &open)?;
    }
    Ok(())
  }
}

/// The hash a hint names, as the chain's type; a malformed hint gets nothing signed.
fn hint_hash(hint: &StrictChannelHint) -> Result<StrictHash, CoreError> {
  <[u8; 32]>::try_from(hint.hash.as_slice())
    .map(StrictHash::from_bytes)
    .map_err(|_| CoreError::Corrupt("a strict hint whose hash isn't 32 bytes".into()))
}

fn ready_key(conversation: ConversationId) -> String {
  format!("strict-ready:{conversation}")
}

fn delegated_key(conversation: ConversationId) -> String {
  format!("strict-delegated:{conversation}")
}
