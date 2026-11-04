//! A channel's signed role log on this device (issue #380, e2ee.md §4.6.1): reading and verifying
//! it, signing the next entry, and comparing heads with the other members.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use teamcraft_crypto::workspace::{WorkspaceLog, claimed_signer, sign_log_entry};
use teamcraft_proto::{
  AppendChannelRoleLogRequest, Base64Bytes, ChannelRole, ChannelRoleLogEntryResponse, ChannelRolesDocument,
  ConversationId, ConversationKind, ListChannelRoleLogQuery, UserId,
};

use super::Client;
use crate::api::ServerApi;
use crate::channel_roles::{ChannelRoleState, ChannelRolesHead, RoleVerifier, SignerAuthority};
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

/// Where a channel's verified roles are kept, in [`Table::Workspace`].
const CHANNEL_ROLES_KEY_PREFIX: &str = "channel_roles/";
/// The channels where a member showed a newer head than this device's, in [`Table::Workspace`].
const CHANNEL_ROLES_AHEAD_KEY: &str = "channel_roles_ahead";
/// The channels whose role logs are read after an admin left the roster, in [`Table::Workspace`].
const CHANNEL_ROLES_DUE_KEY: &str = "channel_roles_due";
/// How many of those are read in one upkeep pass, so a removal is no burst.
const ROLE_LOG_READS_PER_PASS: usize = 4;
/// How long a failed one waits before it is read again, doubling up to the cap: one channel that
/// keeps failing neither holds up the others nor asks the server every pass.
const ROLE_LOG_READ_RETRY_MS: i64 = 5 * 60 * 1000;
const ROLE_LOG_READ_RETRY_MAX_MS: i64 = 6 * 60 * 60 * 1000;
/// Pages of a role log read in one sync; a log longer than this is read on in the next one.
const MAX_ROLE_LOG_PAGES: usize = 100;
/// Claimed founders whose identity keys are pinned before a channel's first entries are verified.
const MAX_FOUNDER_CANDIDATES: usize = 16;

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
}

/// A channel's log read due after a removal, with how it went so far.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct DueRoleRead {
  #[serde(default)]
  failures: u32,
  #[serde(default)]
  not_before_ms: i64,
}

/// What became of a role change on this device's side of the signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoleSigning {
  /// Signed and stored: every member verifies it.
  Signed,
  /// Not signed: this user can't sign it in a way members would accept (a role that is the
  /// server's word only, a space admin, a custom scheme). The server's change stands, and members
  /// see the role as not signed.
  Unsigned,
  /// Not signed: the log moved on since the workspace admin looked at this list, so another admin's
  /// change may be missing from it. They look again; it is never signed for them.
  ReviewOutdated,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// The channel's roles as this device verified them (nothing signed yet: the default).
  pub fn channel_role_state(&self, conversation: ConversationId) -> Result<ChannelRoleState, CoreError> {
    Ok(
      records::get_json(&self.store, Table::Workspace, &channel_roles_key(conversation))?.unwrap_or_default(),
    )
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
    let mut state = self.channel_role_state(conversation)?;
    state.prepare_read();
    // Before the read, so an entry that arrives while it runs isn't judged by a newer roster.
    let roster_at_read = self.workspace()?.roster.version;
    let (entries, is_read_to_end) = match self.fetch_role_log(conversation, state.cursor).await {
      Ok(read) => read,
      Err(CoreError::Api(error)) if error.is_not_found() || error.is_forbidden() => return Ok(()),
      Err(error) => return Err(error),
    };
    if !entries.is_empty() {
      // Entries are judged against the newest roster the server shows, with its history. A failed
      // read leaves them unread, to be judged next time: judged on an old roster they would be
      // refused for good, and could end up as a stretch only a workspace admin can end.
      self.sync_workspace().await?;
      self.complete_admin_history().await?;
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
      };
      for error in state.apply_entries(entries, &verifier) {
        tracing::warn!(conversation_id = %conversation, %error, "skipped a channel role entry");
      }
    }
    if is_read_to_end {
      state.note_read(roster_at_read);
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

  async fn sign_role_plan(
    &mut self,
    conversation: ConversationId,
    plan: &RolePlan<'_>,
  ) -> Result<RoleSigning, CoreError> {
    for attempt in 0..2 {
      self.sync_channel_roles(conversation).await?;
      let maybe_request = match *plan {
        RolePlan::Changes { changes, may_found } => self.role_entry(conversation, changes, may_found).await?,
        RolePlan::Reviewed { roles, at_cursor } => {
          if self.channel_role_state(conversation)?.cursor != at_cursor {
            return Ok(RoleSigning::ReviewOutdated);
          }
          self.reviewed_role_entry(conversation, roles).await?
        },
      };
      let Some(request) = maybe_request else {
        return Ok(RoleSigning::Unsigned);
      };
      match self.api.append_channel_role_log(conversation, &request).await {
        Ok(_) => {
          self.sync_channel_roles(conversation).await?;
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
      // The channel works without; its first admin who changes a role founds the log.
      Err(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "could not sign the new channel's roles");
      },
    }
  }

  /// The channel's signed head, to send along inside an encrypted message (`None` while nothing is
  /// signed, or for direct messages).
  pub(crate) fn channel_roles_head(
    &self,
    conversation: ConversationId,
  ) -> Result<Option<ChannelRolesHead>, CoreError> {
    Ok(self.channel_role_state(conversation)?.head())
  }

  /// Compares a member's head of the channel's roles (from a message) with this device's.
  pub(crate) fn note_channel_roles_head(
    &self,
    conversation: ConversationId,
    head: &ChannelRolesHead,
  ) -> Result<(), CoreError> {
    let mut state = self.channel_role_state(conversation)?;
    let before = state.clone();
    state.note_head(head);
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

  /// Syncs the role logs of channels where a member showed a newer head; afterwards their state
  /// says whether this device is shown less than its colleagues. One read when nothing is due.
  pub async fn check_channel_roles_heads(&mut self) -> Result<(), CoreError> {
    let ahead = self.channel_roles_ahead()?;
    if ahead.is_empty() {
      return Ok(());
    }
    for conversation in &ahead {
      self.sync_channel_roles(*conversation).await?;
    }
    records::put_json(
      &self.store,
      Table::Workspace,
      CHANNEL_ROLES_AHEAD_KEY,
      &BTreeSet::<ConversationId>::new(),
    )
  }

  /// After an admin left the roster: reads each channel's role log this device knows, a few per
  /// upkeep pass, so entries the server shows later can't pass for ones from before the removal
  /// (e2ee.md §4.6.1, "Reading the log late"). A local read while nothing is due, which is nearly
  /// always. A failure stays due, later and later, and doesn't hold up the other channels.
  pub async fn read_due_channel_roles(&mut self) -> Result<(), CoreError> {
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
    for conversation in batch {
      if let Err(error) = self.sync_channel_roles(conversation).await {
        tracing::debug!(conversation_id = %conversation, %error, "channel role log due after a removal not read yet");
        if let Some(read) = due.get_mut(&conversation) {
          read.failures = read.failures.saturating_add(1);
          read.not_before_ms = now.saturating_add(role_read_retry_ms(read.failures));
        }
      } else {
        due.remove(&conversation);
      }
      records::put_json(&self.store, Table::Workspace, CHANNEL_ROLES_DUE_KEY, &due)?;
    }
    Ok(())
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
    records::put_json(&self.store, Table::Workspace, CHANNEL_ROLES_DUE_KEY, &due)
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
  ) -> Result<Option<AppendChannelRoleLogRequest>, CoreError> {
    let maybe_response: Option<teamcraft_proto::ConversationResponse> =
      records::get_json(&self.store, Table::Conversations, &conversation.to_string())?;
    if maybe_response.is_some_and(|response| response.kind != ConversationKind::Channel) {
      return Ok(None);
    }
    let me = self.account.user_id;
    let state = self.channel_role_state(conversation)?;
    let workspace = self.workspace()?;
    let authorities = state.signing_authorities(me, &workspace, self.now_ms(), self.roster_max_age_ms());
    // Every member takes this user's signature as a workspace admin's, whatever authority it is
    // signed on, and it would vouch for the whole snapshot. Nobody asked for that here, and a device
    // that nobody is looking at (an import) has nobody to ask: it stays unsigned.
    if state.must_review_before_vouching() && authorities.iter().any(is_workspace_authority) {
      tracing::info!(conversation_id = %conversation, "not vouching for roles nobody reviewed");
      return Ok(None);
    }
    let mut maybe_refusal = None;
    for authority in authorities {
      if authority == SignerAuthority::Founder && !may_found {
        continue;
      }
      let mut changes = changes.to_vec();
      if authority == SignerAuthority::Founder && !changes.iter().any(|(user, _)| *user == me) {
        changes.push((me, ChannelRole::Admin));
      }
      let keys = self.identity_keys(changes.iter().map(|(user, _)| *user)).await?;
      let document = state.next_document(conversation, &changes, &keys, self.now_ms())?;
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
    let keys = self.identity_keys(roles.iter().map(|(user, _)| *user)).await?;
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

  /// Identity keys as this device pins them (pinning first where needed).
  async fn identity_keys(
    &self,
    users: impl IntoIterator<Item = UserId>,
  ) -> Result<BTreeMap<UserId, [u8; 32]>, CoreError> {
    let users: Vec<UserId> = users.into_iter().collect();
    self.pin_identities(users.iter().copied()).await?;
    let pinned = records::pinned_identities(&self.store)?;
    Ok(
      users
        .into_iter()
        .filter_map(|user| Some((user, pinned.get(&user)?.public_key)))
        .collect(),
    )
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

pub(crate) fn channel_roles_key(conversation: ConversationId) -> String {
  format!("{CHANNEL_ROLES_KEY_PREFIX}{conversation}")
}
