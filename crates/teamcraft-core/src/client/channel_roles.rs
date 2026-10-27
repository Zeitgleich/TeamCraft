//! A channel's signed role log on this device (issue #380, e2ee.md §4.6.1): reading and verifying
//! it, signing the next entry, and comparing heads with the other members.

use std::collections::{BTreeMap, BTreeSet};

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
/// Pages of a role log read in one sync; a log longer than this is read on in the next one.
const MAX_ROLE_LOG_PAGES: usize = 100;
/// Claimed founders whose identity keys are pinned before a channel's first entries are verified.
const MAX_FOUNDER_CANDIDATES: usize = 16;

/// What became of a role change on this device's side of the signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoleSigning {
  /// Signed and stored: every member verifies it.
  Signed,
  /// Not signed: this user can't sign it in a way members would accept (a role that is the
  /// server's word only, a space admin, a custom scheme). The server's change stands, and members
  /// see the role as not signed.
  Unsigned,
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
    let entries = match self.fetch_role_log(conversation, state.cursor).await {
      Ok(entries) => entries,
      Err(CoreError::Api(error)) if error.is_not_found() || error.is_forbidden() => return Ok(()),
      Err(error) => return Err(error),
    };
    if !entries.is_empty() {
      // A workspace admin's entry is judged against the newest roster the server shows.
      if let Err(error) = self.sync_workspace().await {
        tracing::warn!(conversation_id = %conversation, %error, "verifying channel roles against the roster this device has");
      }
      // A founder must hold the key this device pins for them; pin before verifying, or the
      // entry would be skipped for good.
      let claimed = founder_candidates(&state, &entries);
      let pinned = self.pin_identities(claimed).await?;
      let workspace = self.workspace()?;
      let verifier = RoleVerifier {
        conversation,
        workspace: &workspace,
        roster_max_age_ms: self.roster_max_age_ms(),
        pinned: &pinned,
      };
      for error in state.apply_entries(entries, &verifier) {
        tracing::warn!(conversation_id = %conversation, %error, "skipped a channel role entry");
      }
    }
    state.check_ahead();
    if let Some(alarm) = &state.maybe_alarm {
      tracing::error!(conversation_id = %conversation, %alarm, "channel role log alarm");
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
    for attempt in 0..2 {
      self.sync_channel_roles(conversation).await?;
      let Some(request) = self.role_entry(conversation, changes, may_found).await? else {
        return Ok(RoleSigning::Unsigned);
      };
      match self.api.append_channel_role_log(conversation, &request).await {
        Ok(_) => {
          self.sync_channel_roles(conversation).await?;
          return Ok(RoleSigning::Signed);
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
      Ok(RoleSigning::Unsigned) => {
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
  ) -> Result<Vec<ChannelRoleLogEntryResponse>, CoreError> {
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
        break;
      }
    }
    Ok(entries)
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

  fn roster_max_age_ms(&self) -> i64 {
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

pub(crate) fn channel_roles_key(conversation: ConversationId) -> String {
  format!("{CHANNEL_ROLES_KEY_PREFIX}{conversation}")
}
