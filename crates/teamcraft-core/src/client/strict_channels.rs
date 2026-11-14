//! Strict channels in the app (e2ee.md §4.6.2, issue #1896): a private channel whose membership
//! changes only signed channel admins (and moderators, for plain members) may make. The rule and
//! the verdicts are `teamcraft_crypto`'s; this module is what the app adds around them:
//!
//! - **Stopping at a refused commit** (D4): a commit the rule refuses, or that can't be read at
//!   this device's own epoch, stops the device there. It is recorded with the group state, the
//!   device doesn't ask to be added back, passes over entries of later epochs and takes the next
//!   commit for its own epoch. Its outbox counts as caught up and messages keep the current content
//!   key; no commit of its own goes out meanwhile.
//! - **Continuing** from the stop: a channel admin or workspace admin's commit for the last good
//!   epoch goes to the server's rewind, which drops what was built on the refused commit.
//! - **Signing and committing the roles**: turning strict on (a workspace signer, D3), changing
//!   the roles, turning it off.
//! - **The roles a device knew** stay in the account data, so a welcome back after falling out is
//!   opened only from an admin of them; a member's welcome only on the user's word, after
//!   `strict_rejoin_wait`.

use serde::{Deserialize, Serialize};
use teamcraft_crypto::identity::IdentityPublicKey;
use teamcraft_crypto::membership::{IdentityDirectory as _, MembershipAction};
use teamcraft_crypto::mls::{MlsError, StagedJoin};
use teamcraft_crypto::strict_roles::{
  StrictHash, StrictRole, StrictRoleKind, StrictRoles, StrictRolesChange, StrictSignerRole,
};
use teamcraft_proto::Id as _;
use teamcraft_proto::{
  ConversationId, ConversationKind, ConversationResponse, ConversationVisibility, DeviceId, MlsMessageId,
  MlsMessageResponse, UserId,
};

use super::Client;
use crate::account_data::KnownStrictRoles;
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};
use crate::trust::{PinnedDirectory, WorkspaceAuthority};

/// Where this device stopped in a strict channel (e2ee.md §4.6.2, D4): the refused commit and why.
/// Kept with the group state; it holds while the group is still in `epoch`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrictStop {
  /// The epoch this device stays in: the last good one.
  pub epoch: u64,
  /// The refused commit's queue entry.
  pub commit_id: MlsMessageId,
  pub sender_device_id: DeviceId,
  /// Who the commit's signed change claims as its signer, unverified: whom "Remove {name} and
  /// continue" names. `None` for a commit without one.
  #[serde(rename = "signer", default, skip_serializing_if = "Option::is_none")]
  pub maybe_signer: Option<UserId>,
  /// The server's time of the refused commit.
  pub at_ms: i64,
  /// Why it was refused, for the logs and a bug report; never shown as the reason to act.
  pub reason: String,
}

/// A welcome back into a strict channel that wasn't opened: it came from somebody who isn't an
/// admin of the roles this device knew (e2ee.md §4.6.2, "Falling out in a strict channel"). After
/// `strict_rejoin_wait`, or at once when the user is the only admin this device knows of, the
/// user decides.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrictRejoinHold {
  /// Who made the last welcome that wasn't opened.
  pub sender: UserId,
  /// When the first such welcome came (this device's clock): the wait runs from here.
  pub first_held_at_ms: i64,
  /// Who is in that welcome's tree and wasn't in this device's last tree.
  pub joined: Vec<UserId>,
  /// Who was in this device's last tree and isn't in that welcome's.
  pub gone: Vec<UserId>,
  /// The user said to take the next member's welcome.
  #[serde(default)]
  pub is_accepted: bool,
}

/// What the app shows about a held welcome ([`Client::strict_rejoin_prompt`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrictRejoinPrompt {
  pub hold: StrictRejoinHold,
  /// Whether the user may decide now: the wait is over, or nobody else could answer.
  pub is_due: bool,
}

/// Whether a welcome is opened ([`Client::check_strict_welcome`]).
pub(super) enum StrictWelcome {
  Open,
  Refused(MlsError),
}

/// Whether the app follows strict channels (Cargo.toml, `strict-channels`; `strict_channels_off.rs`).
pub(super) const ENABLED: bool = true;

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Whether this device stopped in the conversation's strict channel ([`Self::strict_stop`]).
  pub(super) fn is_strict_stopped(&mut self, conversation: ConversationId) -> Result<bool, CoreError> {
    Ok(self.strict_stop(conversation)?.is_some())
  }

  /// Whether this device's next commit in the conversation continues its strict channel from the
  /// stop ([`Self::continue_strict_channel`]).
  pub(super) fn is_strict_rewind(&self, conversation: ConversationId) -> bool {
    self.maybe_strict_rewind == Some(conversation)
  }

  /// Where this device stopped in the conversation's strict channel, if it did and the group is
  /// still there (e2ee.md §4.6.2, D4). A commit for the stopped epoch that this device takes (an
  /// admin's continuation) ends the stop with the epoch.
  pub fn strict_stop(&mut self, conversation: ConversationId) -> Result<Option<StrictStop>, CoreError> {
    let Some(stop) =
      records::get_json::<StrictStop>(&self.store, Table::AppliedChanges, &stop_key(conversation))?
    else {
      return Ok(None);
    };
    Ok((self.mls_epoch(conversation)? == Some(stop.epoch)).then_some(stop))
  }

  /// Whether the conversation's group is strict: its context carries the roles (e2ee.md §4.6.2).
  pub fn is_strict(&mut self, conversation: ConversationId) -> Result<bool, CoreError> {
    Ok(self.group(conversation)?.is_some_and(|group| group.is_strict()))
  }

  /// The strict roles of the conversation's group as it stands, `None` if it isn't strict.
  pub fn strict_roles(&mut self, conversation: ConversationId) -> Result<Option<StrictRoles>, CoreError> {
    match self.group(conversation)? {
      Some(group) => Ok(group.strict_roles()?),
      None => Ok(None),
    }
  }

  /// Turns the channel strict, sets its roles or, with `roles` empty, turns strict off (e2ee.md
  /// §4.6.2, "Turning it on", "Role changes while strict"). Signed as an admin of the current roles
  /// when this user is one, otherwise as a workspace owner or admin (turning on always is, D3),
  /// mirroring the channel's role log head, and committed from this device. Only for a private
  /// channel this device is in; every device must have refreshed its leaf to turn it on.
  pub async fn set_strict_roles(
    &mut self,
    conversation: ConversationId,
    roles: &[(UserId, StrictRoleKind)],
  ) -> Result<(), CoreError> {
    let _step = crate::timing::step("set_strict_roles");
    if !roles.is_empty() {
      self.ensure_strict_capable(conversation)?;
    }
    let mut roles = roles.to_vec();
    roles.sort_by_key(|(user, _)| *user);
    roles.dedup_by_key(|(user, _)| *user);
    let me = self.account.user_id;
    let attempts = self.settings.max_commit_attempts.max(1);
    for _ in 0..attempts {
      if !self.has_group(conversation)? {
        return Err(CoreError::NoGroup(conversation));
      }
      if self.strict_stop(conversation)?.is_some() {
        return Err(CoreError::StrictStopped(conversation));
      }
      self.resolve_pending_commit(conversation).await?;
      // A workspace signer is judged on the newest roster, by the members as by this device.
      self.sync_workspace().await?;
      let mut users = self.group_users(conversation)?;
      users.extend(roles.iter().map(|(user, _)| *user));
      users.push(me);
      let directory = self.trusted_directory(users).await?;
      let group = self.group_or_error(conversation)?;
      let epoch = group.epoch();
      let maybe_current = group.strict_roles()?;
      let own_key = directory.pinned_identity(convert::user(me));
      let is_context_admin = maybe_current.as_ref().is_some_and(|current| {
        current
          .role_of(convert::user(me))
          .is_some_and(|role| role.role == StrictRoleKind::Admin && Some(role.identity_key) == own_key)
      });
      let (signer_role, roster_version) = if is_context_admin {
        (StrictSignerRole::ChannelAdmin, 0)
      } else {
        (StrictSignerRole::Workspace, self.workspace()?.roster.version)
      };
      let mut strict_roles = Vec::with_capacity(roles.len());
      for (user, role) in &roles {
        let identity_key: IdentityPublicKey = directory
          .pinned_identity(convert::user(*user))
          .ok_or(CoreError::UnknownIdentity(*user))?;
        strict_roles.push(StrictRole {
          user_id: convert::user(*user),
          role: *role,
          identity_key,
        });
      }
      let (version, prev_hash) = match &maybe_current {
        Some(current) => (
          current.version().saturating_add(1),
          current.hash().map_err(MlsError::from)?,
        ),
        None => (1, StrictHash::ZERO),
      };
      let (role_log_version, role_log_hash) = self.role_log_mirror(conversation)?;
      let signed = StrictRolesChange {
        conversation_id: convert::conversation(conversation),
        version,
        prev_hash,
        signed_for_epoch: epoch,
        signer_role,
        roster_version,
        role_log_version,
        role_log_hash,
        roles: strict_roles,
      }
      .sign(&self.account.device, &self.account.certificate)
      .map_err(MlsError::from)?;
      let authority = self.group_authority(conversation, self.now_ms())?;
      let group = self
        .groups
        .get_mut(&conversation)
        .ok_or(CoreError::NoGroup(conversation))?;
      let output = group.commit_strict_roles(
        &self.provider,
        &self.account.device,
        &signed,
        &directory,
        &authority,
      )?;
      if self.post_commit(conversation, output, &[]).await?.is_some() {
        self.note_known_strict_roles(conversation)?;
        return Ok(());
      }
      self.sync_mls(conversation).await?;
    }
    Err(CoreError::GaveUp {
      attempts,
      reason: "other commits kept winning the epoch".into(),
    })
  }

  /// Continues the conversation's strict channel from where this device stopped (e2ee.md §4.6.2,
  /// D4): "Remove {name} and continue" with `maybe_remove`, otherwise "Continue" with an empty
  /// commit. The commit is for the last good epoch and goes to the server's rewind, which drops the
  /// refused commit and everything built on it. For a channel admin of the roles (the server also
  /// asks `manage_channel_roles`) or a workspace admin; the members judge the commit like any.
  pub async fn continue_strict_channel(
    &mut self,
    conversation: ConversationId,
    maybe_remove: Option<UserId>,
  ) -> Result<(), CoreError> {
    let _step = crate::timing::step("continue_strict_channel");
    if self.strict_stop(conversation)?.is_none() {
      return Err(CoreError::StrictNotNow(conversation, "this device didn't stop"));
    }
    self.maybe_strict_rewind = Some(conversation);
    let result = match maybe_remove {
      Some(user) => Box::pin(self.commit_change(
        conversation,
        super::groups::ChangeSource::Own(MembershipAction::RemoveUser {
          user_id: convert::user(user),
        }),
      ))
      .await
      .and_then(|()| {
        // Nothing to remove (the user isn't in the tree at that epoch): continue without.
        Ok(self.strict_stop(conversation)?.is_some())
      }),
      None => Ok(true),
    };
    let result = match result {
      Ok(true) => self.commit_empty_continuation(conversation).await,
      Ok(false) => Ok(()),
      Err(error) => Err(error),
    };
    self.maybe_strict_rewind = None;
    result
  }

  /// The conversation's held welcome back into a strict channel, if one wasn't opened (e2ee.md
  /// §4.6.2, "Falling out in a strict channel"), and whether the user may take a member's welcome
  /// now.
  pub fn strict_rejoin_prompt(
    &mut self,
    conversation: ConversationId,
  ) -> Result<Option<StrictRejoinPrompt>, CoreError> {
    let Some(hold) =
      records::get_json::<StrictRejoinHold>(&self.store, Table::AppliedChanges, &hold_key(conversation))?
    else {
      return Ok(None);
    };
    let wait_ms = i64::try_from(self.settings.strict_rejoin_wait.as_millis()).unwrap_or(i64::MAX);
    let is_waited = self.now_ms().saturating_sub(hold.first_held_at_ms) >= wait_ms;
    let is_due = is_waited || self.is_only_known_admin(conversation)?;
    Ok(Some(StrictRejoinPrompt { hold, is_due }))
  }

  /// The user takes a member's welcome back into the strict channel, having seen who is in its
  /// tree ([`Self::strict_rejoin_prompt`]): this device asks to be added back again and opens the
  /// next welcome whoever made it.
  pub async fn accept_strict_rejoin(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let Some(prompt) = self.strict_rejoin_prompt(conversation)? else {
      return Err(CoreError::StrictNotNow(conversation, "no welcome is held"));
    };
    if !prompt.is_due {
      return Err(CoreError::StrictNotNow(
        conversation,
        "the wait for an admin isn't over",
      ));
    }
    let hold = StrictRejoinHold {
      is_accepted: true,
      ..prompt.hold
    };
    records::put_json(&self.store, Table::AppliedChanges, &hold_key(conversation), &hold)?;
    // Asked again now: the user decided, the backoff of the earlier request doesn't hold it back.
    self
      .store
      .delete(Table::AppliedChanges, &super::groups::rejoin_key(conversation))?;
    let query = teamcraft_proto::ListMlsMessagesQuery {
      maybe_after: None,
      maybe_limit: Some(1),
    };
    let server_epoch = self
      .api
      .list_mls_messages(conversation, &query)
      .await?
      .current_epoch;
    self.request_rejoin(conversation, server_epoch).await
  }

  /// Records the stop at a refused commit with the group state and the cursor past the commit
  /// (e2ee.md §4.6.2, D4), in one write.
  pub(super) fn persist_strict_stop_at(
    &mut self,
    message: &MlsMessageResponse,
    error: &MlsError,
  ) -> Result<(), CoreError> {
    let conversation = message.conversation_id;
    let maybe_signer = teamcraft_crypto::mls::users_named_in_change(message.payload.as_slice())
      .last()
      .copied()
      .map(convert::user_from_crypto)
      .transpose()?;
    let stop = StrictStop {
      epoch: message.epoch,
      commit_id: message.id,
      sender_device_id: message.sender_device_id,
      maybe_signer,
      at_ms: message.id.snowflake().unix_millis(),
      reason: error.to_string(),
    };
    tracing::warn!(conversation_id = %conversation, mls_message_id = %message.id, %error, "stopped at a membership change this strict channel refuses");
    self.atomically(|client| {
      records::put_json(
        &client.store,
        Table::AppliedChanges,
        &stop_key(conversation),
        &stop,
      )?;
      client.persist_group_at(conversation, message)
    })
  }

  /// Keeps the roles of the conversation's group in the account data when they changed (e2ee.md
  /// §4.6.2, "Falling out in a strict channel"); a group that was never strict leaves nothing.
  pub(super) fn note_known_strict_roles(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let Some(group) = self.group(conversation)? else {
      return Ok(());
    };
    let epoch = group.epoch();
    let maybe_roles = group
      .strict_roles()?
      .map(|roles| roles.to_bytes())
      .transpose()
      .map_err(MlsError::from)?
      .map(teamcraft_proto::Base64Bytes::new);
    let data = self.account_data();
    let maybe_known = data.known_strict_roles(conversation);
    let is_unchanged = match maybe_known {
      None => maybe_roles.is_none(),
      Some(known) => known.maybe_roles == maybe_roles,
    };
    if is_unchanged {
      return Ok(());
    }
    let known = KnownStrictRoles { epoch, maybe_roles };
    self.update_account_data(|data, _| data.note_strict_roles(conversation, known))
  }

  /// In a strict group, how much later than others this device answers a rejoin request: members
  /// who aren't admins of its roles (or workspace admins) wait `strict_rejoin_wait`, so the
  /// welcome back comes from an admin (e2ee.md §4.6.2, "Falling out in a strict channel").
  pub(super) fn strict_rejoin_delay_ms(&mut self, conversation: ConversationId) -> Result<i64, CoreError> {
    let Some(roles) = self.strict_roles(conversation)? else {
      return Ok(0);
    };
    let me = self.account.user_id;
    let is_admin = roles
      .role_of(convert::user(me))
      .is_some_and(|role| role.role == StrictRoleKind::Admin);
    let workspace = self.workspace()?;
    let (admins, _) = self.fresh_admins(&workspace, self.now_ms());
    if is_admin || admins.contains(&me) {
      return Ok(0);
    }
    Ok(i64::try_from(self.settings.strict_rejoin_wait.as_millis()).unwrap_or(i64::MAX))
  }

  /// Whether a welcome into the conversation is opened (e2ee.md §4.6.2, "Falling out in a strict
  /// channel"): if the user's devices knew it strict, only from an admin of those roles or a
  /// workspace signer, or on the user's word ([`Self::accept_strict_rejoin`]); a welcome without
  /// the roles only after this device saw strict turned off. Called after the welcome was staged,
  /// before it is completed.
  pub(super) async fn check_strict_welcome(
    &mut self,
    conversation: ConversationId,
    staged: &StagedJoin,
    directory: &PinnedDirectory,
  ) -> Result<StrictWelcome, CoreError> {
    let maybe_known = self
      .account_data()
      .known_strict_roles(conversation)
      .and_then(|known| known.maybe_roles.clone());
    let Some(known_bytes) = maybe_known else {
      return Ok(StrictWelcome::Open);
    };
    // Only a device that fell out: a further device of the user joins like any (any member adds
    // it, §4.6.2 "A device's further devices don't change"), and has nothing it could have refused.
    let has_fallen_out = self.has_group(conversation)? || self.has_requested_rejoin(conversation)?;
    if !has_fallen_out {
      return Ok(StrictWelcome::Open);
    }
    let maybe_hold =
      records::get_json::<StrictRejoinHold>(&self.store, Table::AppliedChanges, &hold_key(conversation))?;
    if maybe_hold.as_ref().is_some_and(|hold| hold.is_accepted) {
      tracing::info!(conversation_id = %conversation, "took a welcome back into a strict channel on the user's word");
      self
        .store
        .delete(Table::AppliedChanges, &hold_key(conversation))?;
      return Ok(StrictWelcome::Open);
    }
    let known = StrictRoles::from_bytes(known_bytes.as_slice()).map_err(MlsError::from)?;
    let verdict = if staged.strict_roles()?.is_none() {
      Err(MlsError::UnexpectedMessage(
        "a welcome without the strict roles into a channel known strict",
      ))
    } else {
      let mut verdict = staged.check_strict_sender(&known, directory, &self.welcome_authority()?);
      if verdict.as_ref().is_err_and(MlsError::is_strict_retry) {
        // A workspace signer's welcome: judged on the newest roster.
        self.sync_workspace().await?;
        verdict = staged.check_strict_sender(&known, directory, &self.welcome_authority()?);
      }
      verdict.map(|_| ())
    };
    let error = match verdict {
      Ok(()) => {
        self
          .store
          .delete(Table::AppliedChanges, &hold_key(conversation))?;
        return Ok(StrictWelcome::Open);
      },
      Err(error) => error,
    };
    let sender = convert::user_from_crypto(staged.sender()?.user_id)?;
    let mut tree = Vec::new();
    for user in staged.claimed_users()? {
      tree.push(convert::user_from_crypto(user)?);
    }
    let last = match self.group(conversation)? {
      Some(_) => self.group_users(conversation)?,
      None => Vec::new(),
    };
    let hold = StrictRejoinHold {
      sender,
      first_held_at_ms: maybe_hold.map_or(self.now_ms(), |hold| hold.first_held_at_ms),
      joined: tree.iter().copied().filter(|user| !last.contains(user)).collect(),
      gone: last.iter().copied().filter(|user| !tree.contains(user)).collect(),
      is_accepted: false,
    };
    records::put_json(&self.store, Table::AppliedChanges, &hold_key(conversation), &hold)?;
    tracing::warn!(conversation_id = %conversation, %error, "held a welcome back into a strict channel");
    Ok(StrictWelcome::Refused(error))
  }

  /// Turning strict on only for a private channel (e2ee.md §4.6.2): never a DM, a meeting chat,
  /// the notes or a public channel.
  pub(super) fn ensure_strict_capable(&self, conversation: ConversationId) -> Result<(), CoreError> {
    let maybe_response: Option<ConversationResponse> =
      records::get_json(&self.store, Table::Conversations, &conversation.to_string())?;
    let is_private_channel = maybe_response.is_some_and(|response| {
      response.kind == ConversationKind::Channel && response.visibility == ConversationVisibility::Private
    });
    if is_private_channel {
      Ok(())
    } else {
      Err(CoreError::NotStrictCapable(conversation))
    }
  }

  /// The empty commit of "Continue": a leaf refresh for the stopped epoch, sent to the rewind.
  async fn commit_empty_continuation(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self.resolve_pending_commit(conversation).await?;
    let group = self
      .groups
      .get_mut(&conversation)
      .ok_or(CoreError::NoGroup(conversation))?;
    let output = group.commit_leaf_refresh(&self.provider, &self.account.device)?;
    match self.post_commit(conversation, output, &[]).await? {
      Some(_) => Ok(()),
      None => Err(CoreError::StrictNotNow(
        conversation,
        "the server holds no later epoch to continue from",
      )),
    }
  }

  /// The channel role log's head this device accepted (§4.6.1), mirrored into a roles change:
  /// shown, never decided on.
  fn role_log_mirror(&self, conversation: ConversationId) -> Result<(u64, StrictHash), CoreError> {
    let Some(head) = self.channel_role_state(conversation)?.head() else {
      return Ok((0, StrictHash::ZERO));
    };
    let hash = <[u8; 32]>::try_from(head.hash.as_slice()).map_or(StrictHash::ZERO, StrictHash::from_bytes);
    Ok((head.version, hash))
  }

  /// Whether this user is the only admin of the roles this device knew for the conversation:
  /// then nobody else could answer its rejoin request.
  fn is_only_known_admin(&self, conversation: ConversationId) -> Result<bool, CoreError> {
    let Some(bytes) = self
      .account_data()
      .known_strict_roles(conversation)
      .and_then(|known| known.maybe_roles.clone())
    else {
      return Ok(false);
    };
    let known = StrictRoles::from_bytes(bytes.as_slice()).map_err(MlsError::from)?;
    let me = convert::user(self.account.user_id);
    let mut admins = known.admins();
    Ok(admins.next().is_some_and(|admin| admin.user_id == me) && admins.next().is_none())
  }

  /// The authority a welcome's sender is judged by when it isn't an admin of the known roles: a
  /// workspace owner or admin of a fresh roster.
  fn welcome_authority(&self) -> Result<WorkspaceAuthority, CoreError> {
    let workspace = self.workspace()?;
    let (admins, roster) = self.fresh_admins(&workspace, self.now_ms());
    Ok(
      WorkspaceAuthority::new(Vec::new(), admins, workspace.policy.admin_self_join.clone())
        .with_roster(roster),
    )
  }
}

fn stop_key(conversation: ConversationId) -> String {
  format!("strict-stop:{conversation}")
}

fn hold_key(conversation: ConversationId) -> String {
  format!("strict-hold:{conversation}")
}
