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
//!   `strict_rejoin_wait`. Before judging such a welcome, the device carries those roles forward
//!   along the server's chain of the signed changes it missed (`GET …/mls/strict?after=`,
//!   [`teamcraft_crypto::strict_roles::verify_chain`]): an admin appointed meanwhile brings it back
//!   at once, and after a verified "off" any member's welcome without the roles opens. A chain
//!   that can't be read or doesn't verify changes nothing: the roles it knew judge.

use serde::{Deserialize, Serialize};
use teamcraft_crypto::identity::IdentityPublicKey;
use teamcraft_crypto::membership::{IdentityDirectory as _, MembershipAction};
use teamcraft_crypto::mls::{MlsError, StagedJoin};
use teamcraft_crypto::strict_roles::{
  ChainEnd, ChainLink, SignedStrictRolesChange, StrictHash, StrictRole, StrictRoleKind, StrictRoles,
  StrictRolesChange, StrictRolesError, StrictSignerRole, verify_chain,
};
use teamcraft_proto::Id as _;
use teamcraft_proto::{
  Base64Bytes, ConversationId, ConversationKind, ConversationResponse, ConversationVisibility, DeviceId,
  MlsMessageId, MlsMessageResponse, MlsStrictHintQuery, StrictChainChange, UserId,
};

use super::Client;
use crate::account_data::KnownStrictRoles;
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{AwayChange, ConversationEvent, LocalStore, MessageContent, StoredMessage, Table};
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

/// Whether a welcome is opened ([`Client::check_strict_welcome`]): with the lines for who is new
/// or gone since this device's last tree, after an add-back into a channel it knew strict.
pub(super) enum StrictWelcome {
  Open(Vec<(UserId, AwayChange)>),
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

  /// The refused commit a rewind from this device replaces: where it stopped. A continuation
  /// another admin got in first is then an epoch conflict, and this device catches up (e2ee.md
  /// §4.6.2, D4, "Epoch conflicts and convergence").
  pub(super) fn strict_rewind_replaces(
    &mut self,
    conversation: ConversationId,
  ) -> Result<Option<MlsMessageId>, CoreError> {
    Ok(self.strict_stop(conversation)?.map(|stop| stop.commit_id))
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
    let mut roles = roles.to_vec();
    roles.sort_by_key(|(user, _)| *user);
    roles.dedup_by_key(|(user, _)| *user);
    if !self.has_group(conversation)? {
      // A workspace owner or admin outside the group: the members commit it (e2ee.md §4.6.2). The
      // channel's kind is read from the server, which this device has no copy of.
      return self.post_strict_roles_from_outside(conversation, &roles).await;
    }
    if !roles.is_empty() {
      self.ensure_strict_capable(conversation)?;
    }
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
        None => (
          self.next_strict_chain_version(conversation).await?,
          StrictHash::ZERO,
        ),
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
      self.hint_strict_roles(conversation, &signed)?;
      if self.post_commit(conversation, output, &[]).await?.is_some() {
        self.note_known_strict_roles(conversation, Some(version))?;
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
    self.note_strict_view(conversation)?;
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
    })?;
    self.note_strict_view(conversation)
  }

  /// Keeps the roles of the conversation's group in the account data when they changed (e2ee.md
  /// §4.6.2, "Falling out in a strict channel"); a group that was never strict leaves nothing.
  /// `maybe_change_version`: the version of the roles change the commit just made, if it made one
  /// (what an "off" is noted with; roles carry their own).
  pub(super) fn note_known_strict_roles(
    &mut self,
    conversation: ConversationId,
    maybe_change_version: Option<u64>,
  ) -> Result<(), CoreError> {
    // The group changed: a stop may have ended, roles or readiness moved (the UI's view).
    self.note_strict_view(conversation)?;
    if !self.has_group(conversation)? {
      return Ok(());
    }
    // The tree changed: a device that blocked the opt-in may have refreshed or gone (step 3).
    self.schedule_strict_refresh(conversation)?;
    let Some(group) = self.group(conversation)? else {
      return Ok(());
    };
    let epoch = group.epoch();
    let maybe_strict = group.strict_roles()?;
    let maybe_version = match &maybe_strict {
      Some(roles) => Some(roles.version()),
      None => maybe_change_version,
    };
    let maybe_roles = maybe_strict
      .map(|roles| roles.to_bytes())
      .transpose()
      .map_err(MlsError::from)?
      .map(Base64Bytes::new);
    let data = self.account_data();
    let maybe_known = data.known_strict_roles(conversation);
    let is_unchanged = match maybe_known {
      None => maybe_roles.is_none(),
      Some(known) => known.maybe_roles == maybe_roles,
    };
    if is_unchanged {
      return Ok(());
    }
    let known = KnownStrictRoles {
      epoch,
      maybe_version,
      maybe_roles,
    };
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
  /// channel"): if the user's devices knew it strict, only from an admin of those roles, carried
  /// forward along the server's chain of signed changes ([`Self::strict_chain_end`]), or from a
  /// workspace signer, or on the user's word ([`Self::accept_strict_rejoin`]); a welcome without
  /// the roles only after the device saw strict turned off, in its group or at the chain's end.
  /// Called after the welcome was staged, before it is completed.
  /// `is_rejoin`: this device asked to be added back (its group was dropped for the welcome
  /// already, and with it the record of the request).
  pub(super) async fn check_strict_welcome(
    &mut self,
    conversation: ConversationId,
    staged: &StagedJoin,
    directory: &PinnedDirectory,
    is_rejoin: bool,
  ) -> Result<StrictWelcome, CoreError> {
    let maybe_known = self
      .account_data()
      .known_strict_roles(conversation)
      .and_then(|known| known.maybe_roles.clone());
    let Some(known_bytes) = maybe_known else {
      return Ok(StrictWelcome::Open(Vec::new()));
    };
    // Only a device that fell out: a further device of the user joins like any (any member adds
    // it, §4.6.2 "A device's further devices don't change"), and has nothing it could have refused.
    let has_fallen_out =
      is_rejoin || self.has_group(conversation)? || self.has_requested_rejoin(conversation)?;
    if !has_fallen_out {
      return Ok(StrictWelcome::Open(Vec::new()));
    }
    let mut tree = Vec::new();
    for user in staged.claimed_users()? {
      tree.push(convert::user_from_crypto(user)?);
    }
    let maybe_last = self.strict_last_tree(conversation)?;
    let maybe_hold =
      records::get_json::<StrictRejoinHold>(&self.store, Table::AppliedChanges, &hold_key(conversation))?;
    if maybe_hold.as_ref().is_some_and(|hold| hold.is_accepted) {
      tracing::info!(conversation_id = %conversation, "took a welcome back into a strict channel on the user's word");
      self
        .store
        .delete(Table::AppliedChanges, &hold_key(conversation))?;
      return Ok(StrictWelcome::Open(self.away_lines(maybe_last.as_deref(), &tree)));
    }
    let known = StrictRoles::from_bytes(known_bytes.as_slice()).map_err(MlsError::from)?;
    let is_strict_welcome = staged.strict_roles()?.is_some();
    let maybe_end = self.strict_chain_end(conversation, &known).await;
    if let Some(end) = &maybe_end
      && end.version() > known.version()
      // An "off" against a strict welcome: the chain lacks the turning on again, so the "off"
      // isn't kept (a device that knows "off" opens any welcome).
      && (end.maybe_roles().is_some() || !is_strict_welcome)
      && let Err(error) = self.note_strict_chain_end(conversation, end)
    {
      tracing::warn!(conversation_id = %conversation, %error, "couldn't keep a strict channel's roles carried forward");
    }
    let verdict = self
      .judge_strict_welcome(staged, is_strict_welcome, &known, maybe_end.as_ref(), directory)
      .await?;
    let error = match verdict {
      Ok(()) => {
        self
          .store
          .delete(Table::AppliedChanges, &hold_key(conversation))?;
        return Ok(StrictWelcome::Open(self.away_lines(maybe_last.as_deref(), &tree)));
      },
      Err(error) => error,
    };
    let sender = convert::user_from_crypto(staged.sender()?.user_id)?;
    let last = maybe_last.unwrap_or_default();
    let hold = StrictRejoinHold {
      sender,
      first_held_at_ms: maybe_hold.map_or(self.now_ms(), |hold| hold.first_held_at_ms),
      joined: tree.iter().copied().filter(|user| !last.contains(user)).collect(),
      gone: last.iter().copied().filter(|user| !tree.contains(user)).collect(),
      is_accepted: false,
    };
    records::put_json(&self.store, Table::AppliedChanges, &hold_key(conversation), &hold)?;
    tracing::warn!(conversation_id = %conversation, %error, "held a welcome back into a strict channel");
    self.note_strict_view(conversation)?;
    Ok(StrictWelcome::Refused(error))
  }

  /// The verdict on a welcome back into a channel this user's devices knew strict (`known`), by the
  /// end of the verified chain after it when there is one:
  ///
  /// - without the roles: opened only after a verified "off", from any member, as the channel
  ///   isn't strict any more;
  /// - with the roles: its sender must be an admin of the chain's end (an admin appointed while
  ///   the device was away brings it back), or a workspace signer. Without a verified chain, or
  ///   one ending in "off" while the welcome is strict (its turning on again isn't in the chain),
  ///   the known roles judge, as without a chain.
  async fn judge_strict_welcome(
    &mut self,
    staged: &StagedJoin,
    is_strict_welcome: bool,
    known: &StrictRoles,
    maybe_end: Option<&ChainEnd>,
    directory: &PinnedDirectory,
  ) -> Result<Result<(), MlsError>, CoreError> {
    if !is_strict_welcome {
      return Ok(match maybe_end {
        Some(ChainEnd::Off { .. }) => Ok(()),
        _ => Err(MlsError::UnexpectedMessage(
          "a welcome without the strict roles into a channel known strict",
        )),
      });
    }
    let judged_by = maybe_end.and_then(ChainEnd::maybe_roles).unwrap_or(known);
    let mut verdict = staged.check_strict_sender(judged_by, directory, &self.welcome_authority()?);
    if verdict.as_ref().is_err_and(MlsError::is_strict_retry) {
      // A workspace signer's welcome: judged on the newest roster.
      self.sync_workspace().await?;
      verdict = staged.check_strict_sender(judged_by, directory, &self.welcome_authority()?);
    }
    Ok(verdict.map(|_| ()))
  }

  /// The roles this user's devices knew (`known`), carried forward along the signed roles changes
  /// the server kept after them (e2ee.md §4.6.2, "Falling out in a strict channel"). `None` when
  /// the chain can't be read, is longer than `max_strict_chain_changes` or doesn't verify, a roster
  /// or device list it needs included: then the known roles judge, as without a chain, and the next
  /// welcome tries again.
  async fn strict_chain_end(
    &mut self,
    conversation: ConversationId,
    known: &StrictRoles,
  ) -> Option<ChainEnd> {
    let result = match self.read_strict_chain(conversation, known.version()).await {
      Ok(Some(links)) if links.is_empty() => return Some(ChainEnd::Roles(known.clone())),
      Ok(Some(links)) => self.verify_strict_chain(known, &links).await,
      Ok(None) => {
        tracing::warn!(conversation_id = %conversation, max = self.settings.max_strict_chain_changes, "a strict channel's chain is too long to carry the known roles forward");
        return None;
      },
      Err(error) => Err(error),
    };
    let end = match result {
      Ok(Ok(end)) => end,
      Ok(Err(error @ (StrictRolesError::RosterUnknown(_) | StrictRolesError::DeviceListUnknown(_)))) => {
        tracing::info!(conversation_id = %conversation, %error, "a strict channel's chain can't be verified yet");
        return None;
      },
      Ok(Err(error)) => {
        tracing::warn!(conversation_id = %conversation, %error, "refused the server's chain of a strict channel's roles changes");
        return None;
      },
      Err(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "couldn't read a strict channel's chain");
        return None;
      },
    };
    tracing::info!(conversation_id = %conversation, from = known.version(), to = end.version(), is_strict = end.maybe_roles().is_some(), "carried a strict channel's known roles forward along its chain");
    Some(end)
  }

  /// The signed roles changes the server kept after `after`, ascending, read page by page; `None`
  /// past `max_strict_chain_changes`.
  async fn read_strict_chain(
    &mut self,
    conversation: ConversationId,
    after: u64,
  ) -> Result<Option<Vec<StrictChainChange>>, CoreError> {
    let max = self.settings.max_strict_chain_changes;
    let mut links: Vec<StrictChainChange> = Vec::new();
    let mut after = after;
    loop {
      let query = MlsStrictHintQuery {
        maybe_after: Some(after),
      };
      let page = self.api.mls_strict_hint(conversation, &query).await?;
      let Some(last) = page.changes.last().map(|change| change.version) else {
        return Ok(Some(links));
      };
      if last <= after {
        // Pages that don't move forward: verifying what came refuses it.
        links.extend(page.changes);
        return Ok(Some(links));
      }
      links.extend(page.changes);
      if links.len() > max {
        return Ok(None);
      }
      if !page.has_more {
        return Ok(Some(links));
      }
      after = last;
    }
  }

  /// Verifies the chain after `known` ([`verify_chain`]): on a fresh roster when a workspace signer
  /// signed a link, with the device lists of every signer read.
  async fn verify_strict_chain(
    &mut self,
    known: &StrictRoles,
    links: &[StrictChainChange],
  ) -> Result<Result<ChainEnd, StrictRolesError>, CoreError> {
    let mut signers = Vec::new();
    let mut has_workspace_signer = false;
    for link in links {
      // A link that doesn't parse is refused by the verification below.
      let Ok(signed) = SignedStrictRolesChange::from_bytes(link.change.as_slice()) else {
        continue;
      };
      has_workspace_signer |= signed.change().signer_role == StrictSignerRole::Workspace;
      if let Some(user) = signed
        .claimed_signer()
        .ok()
        .and_then(|user| convert::user_from_crypto(user).ok())
        && !signers.contains(&user)
      {
        signers.push(user);
      }
    }
    if has_workspace_signer {
      self.sync_workspace().await?;
    }
    let directory = self.trusted_directory(signers).await?;
    let authority = self.welcome_authority()?;
    let chain = links.iter().map(|link| ChainLink {
      version: link.version,
      change: link.change.as_slice(),
    });
    Ok(verify_chain(known, chain, &directory, &authority))
  }

  /// Keeps the end of a verified chain as the roles this user's devices know (e2ee.md §4.6.2), by
  /// its version; the chain names no epoch, so the replaced entry's stays.
  fn note_strict_chain_end(&mut self, conversation: ConversationId, end: &ChainEnd) -> Result<(), CoreError> {
    let maybe_roles = end
      .maybe_roles()
      .map(StrictRoles::to_bytes)
      .transpose()
      .map_err(MlsError::from)?
      .map(Base64Bytes::new);
    let epoch = self
      .account_data()
      .known_strict_roles(conversation)
      .map_or(0, |known| known.epoch);
    let known = KnownStrictRoles {
      epoch,
      maybe_version: Some(end.version()),
      maybe_roles,
    };
    self.update_account_data(|data, _| data.note_strict_roles(conversation, known))
  }

  /// The version a change turning the channel strict signs: one above the newest this user's
  /// devices or the server's hint know, so the server's chain stays one sequence after an "off"
  /// (`check_change` takes any version for turning on).
  async fn next_strict_chain_version(&mut self, conversation: ConversationId) -> Result<u64, CoreError> {
    let maybe_known = self
      .account_data()
      .known_strict_roles(conversation)
      .and_then(KnownStrictRoles::version);
    let hint = self
      .api
      .mls_strict_hint(conversation, &MlsStrictHintQuery::default())
      .await?;
    let maybe_hinted = hint.maybe_strict.map(|hint| hint.version);
    let newest = maybe_known.into_iter().chain(maybe_hinted).max().unwrap_or(0);
    Ok(newest.saturating_add(1))
  }

  /// Keeps the users of the conversation's group before it is dropped for a welcome back, if this
  /// user's devices knew the channel strict: what the device shows who is new or gone against once
  /// it is back (e2ee.md §4.6.2, "Falling out in a strict channel"). Kept until a welcome is
  /// opened, also across held ones.
  pub(super) fn keep_strict_last_tree(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let is_known_strict = self
      .account_data()
      .known_strict_roles(conversation)
      .is_some_and(|known| known.maybe_roles.is_some());
    if !is_known_strict || !self.has_group(conversation)? {
      return Ok(());
    }
    let users = self.group_users(conversation)?;
    records::put_json(
      &self.store,
      Table::AppliedChanges,
      &last_tree_key(conversation),
      &users,
    )
  }

  /// Writes a line for each of `lines` after this device was added back into the conversation
  /// with `welcome` (e2ee.md §4.6.2): derived here from the welcome it opened, its ID made from the
  /// welcome and the user, so the line is written once. Forgets the last tree.
  pub(super) fn note_changed_while_away(
    &mut self,
    conversation: ConversationId,
    welcome: MlsMessageId,
    lines: &[(UserId, AwayChange)],
  ) -> Result<(), CoreError> {
    self
      .store
      .delete(Table::AppliedChanges, &last_tree_key(conversation))?;
    for &(user, change) in lines {
      self.store.upsert_message(&StoredMessage {
        conversation_id: conversation,
        client_message_id: away_line_id(welcome, user),
        maybe_server_id: None,
        sender_user_id: user,
        maybe_thread_id: None,
        sent_at_ms: welcome.snowflake().unix_millis(),
        content: MessageContent::Event {
          event: ConversationEvent::ChangedWhileAway { user, change },
        },
        is_edited: false,
        edits: Vec::new(),
        maybe_edit_id: None,
        is_pinned: false,
        maybe_verified_commit: Some(welcome),
        maybe_server_change: None,
        maybe_unreadable: None,
      })?;
    }
    if !lines.is_empty() {
      tracing::info!(conversation_id = %conversation, mls_message_id = %welcome, lines = lines.len(), "noted who is new or gone since this device's last tree");
    }
    Ok(())
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
    if self.post_commit(conversation, output, &[]).await?.is_some() {
      return Ok(());
    }
    // Another admin continued first (two at once, §4.6.2 "Epoch conflicts and convergence"):
    // their commit for the stopped epoch ends this device's stop once it is merged.
    self.sync_mls(conversation).await?;
    match self.strict_stop(conversation)? {
      None => Ok(()),
      Some(_) => Err(CoreError::StrictNotNow(
        conversation,
        "the server holds no later epoch to continue from",
      )),
    }
  }

  /// The channel role log's head this device accepted (§4.6.1), mirrored into a roles change:
  /// shown, never decided on.
  pub(super) fn role_log_mirror(&self, conversation: ConversationId) -> Result<(u64, StrictHash), CoreError> {
    let Some(head) = self.channel_role_state(conversation)?.head() else {
      return Ok((0, StrictHash::ZERO));
    };
    let hash = <[u8; 32]>::try_from(head.hash.as_slice()).map_or(StrictHash::ZERO, StrictHash::from_bytes);
    Ok((head.version, hash))
  }

  /// The users of the group this device held before it was dropped for a welcome back
  /// ([`Self::keep_strict_last_tree`]), or of the group it still holds.
  fn strict_last_tree(&mut self, conversation: ConversationId) -> Result<Option<Vec<UserId>>, CoreError> {
    if let Some(users) =
      records::get_json::<Vec<UserId>>(&self.store, Table::AppliedChanges, &last_tree_key(conversation))?
    {
      return Ok(Some(users));
    }
    match self.group(conversation)? {
      Some(_) => Ok(Some(self.group_users(conversation)?)),
      None => Ok(None),
    }
  }

  /// Who is new in `tree` and who is gone from `maybe_last`, this user left out; nothing when
  /// this device doesn't know its last tree (a line for everybody would say nothing).
  fn away_lines(&self, maybe_last: Option<&[UserId]>, tree: &[UserId]) -> Vec<(UserId, AwayChange)> {
    let Some(last) = maybe_last else {
      return Vec::new();
    };
    let me = self.account.user_id;
    let joined = tree
      .iter()
      .filter(|user| **user != me && !last.contains(user))
      .map(|user| (*user, AwayChange::Joined));
    let gone = last
      .iter()
      .filter(|user| **user != me && !tree.contains(user))
      .map(|user| (*user, AwayChange::Gone));
    joined.chain(gone).collect()
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

pub(super) fn hold_key(conversation: ConversationId) -> String {
  format!("strict-hold:{conversation}")
}

fn last_tree_key(conversation: ConversationId) -> String {
  format!("strict-last-tree:{conversation}")
}

/// A line's ID made from the welcome and the user ([`Client::note_changed_while_away`]).
fn away_line_id(welcome: MlsMessageId, user: UserId) -> teamcraft_proto::ClientMessageId {
  let mut bytes = [0u8; 16];
  bytes[..8].copy_from_slice(&welcome.snowflake().get().to_be_bytes());
  bytes[8..].copy_from_slice(&user.snowflake().get().to_be_bytes());
  teamcraft_proto::ClientMessageId::from_bytes(bytes)
}
