//! Removing members, leaving and revoking devices as one recoverable operation (e2ee.md §4.6,
//! removal order): the MLS side first, the server's membership afterwards, with the progress kept
//! in the pending queue so a crash or a lost answer in between is finished on the next round.
//! When the remover commits, every intermediate state is safe: before the MLS step nothing
//! changed; after it the removed person has no key for the new epoch, even while the server still
//! lists them. An admin outside the group watches its posted change and signs it again before it
//! gets too old to be committed; a refused server step is retried with a growing delay and shown
//! as failed after `removal_attempts`.

use teamcraft_crypto::membership::MembershipAction;
use teamcraft_crypto::mls::MAX_CHANGE_AGE_EPOCHS;
use teamcraft_proto::{Base64Bytes, ConversationId, DeviceId, MlsMessageKind, PostMlsMessageRequest, UserId};

use super::Client;
use super::groups::{ADMIN_CHANGE_MAGIC, ChangeSource};
use super::workspace::{
  PendingAnnouncement, PendingChange, PendingOutcome, REVOCATION_CHECK_KEY_PREFIX, RemovalState, RemovalStep,
  RevocationStep, current_epoch_of,
};
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{ConversationEvent, LocalStore, MessageContent, Table};

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Removes `user` from the MLS group, then on the server. The epoch key of the removal commit
  /// means the removed person can't read anything new (e2ee.md §4.1, §4.2). The announcement
  /// follows the key rotation, so the removed person doesn't read it; it belongs to the pending
  /// removal and goes out once, also when a later round finishes the removal.
  pub async fn remove_member(&mut self, conversation: ConversationId, user: UserId) -> Result<(), CoreError> {
    self.api.check_member_removal(conversation, user).await?;
    let maybe_event = self
      .has_group(conversation)?
      .then_some(ConversationEvent::MemberRemoved { user });
    self.record_and_run_removal(conversation, user, maybe_event).await
  }

  /// An admin removes `user` from a conversation, also one the admin isn't a member of: outside
  /// the group the signed change is posted into the group's queue for a member's client to commit
  /// (admin.md §2.2), then the server's membership is dropped, and the admin's client watches the
  /// change until it can't get too old any more ([`RemovalStep::Watch`]). Deactivating someone
  /// calls this per conversation; the server's own removal is only a hint to the clients (security
  /// review A8).
  pub async fn admin_remove_member(
    &mut self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<(), CoreError> {
    self.api.check_member_removal(conversation, user).await?;
    self.record_and_run_removal(conversation, user, None).await
  }

  /// "Remove from all chats" for a deactivated user (admin.md §3.2.1): this admin's device signs
  /// the user's removal from every channel they are in ([`Self::admin_remove_member`]; members
  /// commit it with a key rotation), and only when all of them are on their way asks the server to
  /// drop the rest (devices, direct messages, spaces). A channel that fails leaves the server step
  /// out, so "remove" again finishes it; channels already done are no longer listed.
  pub async fn admin_remove_from_all_chats(&mut self, user: UserId) -> Result<(), CoreError> {
    self.sign_removals_from_all_chats(user).await?;
    self.api.admin_remove_from_chats(user).await?;
    Ok(())
  }

  /// "Delete account" (admin.md §3.2.1, issue #630): the same signed removals as
  /// [`Self::admin_remove_from_all_chats`] first (the server never fakes a membership change), then
  /// the server deletes the account (`DELETE /admin/users/{id}`): devices, key material and
  /// memberships go, direct messages with them are deleted, a "Deleted user" tombstone stays. A
  /// channel that fails leaves the server step out, so deleting again finishes it; both steps are
  /// idempotent.
  pub async fn admin_delete_user(&mut self, user: UserId) -> Result<(), CoreError> {
    if user == self.account.user_id {
      return Err(CoreError::InvalidRequest(
        "admins can't delete their own account".into(),
      ));
    }
    self.sign_removals_from_all_chats(user).await?;
    self.remove_from_own_group_dms(user).await?;
    self.api.admin_delete_user(user).await?;
    Ok(())
  }

  /// Group DMs stay after a deletion when more than one other person remains, and admins don't
  /// sign changes in DMs from outside (admin.md §3.2.1): where this admin is in such a group with
  /// the user, its own device commits the removal, like any member's. 1:1 DMs are deleted anyway.
  async fn remove_from_own_group_dms(&mut self, user: UserId) -> Result<(), CoreError> {
    let groups: Vec<ConversationId> = self
      .cached_conversations()?
      .into_values()
      .filter(|conversation| conversation.kind == teamcraft_proto::ConversationKind::Group)
      .map(|conversation| conversation.id)
      .collect();
    for conversation in groups {
      if !self.has_group(conversation)? || !self.group_users(conversation)?.contains(&user) {
        continue;
      }
      let action = MembershipAction::RemoveUser {
        user_id: convert::user(user),
      };
      self
        .commit_change(conversation, ChangeSource::Own(action))
        .await?;
    }
    Ok(())
  }

  /// "Remove from the group" on a leftover the admin console lists (issue #734): somebody still in
  /// the encryption group of a conversation this device is in, whom fresh member lists no longer
  /// show (confirmed twice, [`Self::check_group_leftovers`]). This admin decides, so this device
  /// signs and commits the removal itself, which rotates the keys; the server has nothing left to
  /// drop. Only users recorded as leftovers are removed this way, never on the server's word alone
  /// (security review A8). Done when they are no longer in the group.
  pub async fn admin_remove_group_leftover(
    &mut self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<(), CoreError> {
    let me = self.account.user_id;
    if !self.workspace()?.is_admin(me) {
      return Err(CoreError::NotAnAdmin(me));
    }
    if !super::group_health::group_leftovers(&self.store, conversation)?.contains(&user) {
      return Err(CoreError::InvalidRequest(
        "the user is not left over in this group".into(),
      ));
    }
    if self.has_group(conversation)? && self.group_users(conversation)?.contains(&user) {
      let action = MembershipAction::RemoveUser {
        user_id: convert::user(user),
      };
      self
        .commit_change(conversation, ChangeSource::Own(action))
        .await?;
    }
    self.forget_leftover(conversation, user)
  }

  /// This admin's device signs the user's removal from every channel the server lists them in
  /// ([`Self::admin_remove_member`]; members commit it, keys rotate) and commits it in every group
  /// of its own where the user is left over. A failure doesn't stop the others; the first one is
  /// returned after all were tried, and channels already done are no longer listed.
  async fn sign_removals_from_all_chats(&mut self, user: UserId) -> Result<(), CoreError> {
    let me = self.account.user_id;
    if !self.workspace()?.is_admin(me) {
      return Err(CoreError::NotAnAdmin(me));
    }
    let mut maybe_failure = None;
    for channel in self.api.admin_channels_of(user).await? {
      if let Err(error) = self.admin_remove_member(channel, user).await {
        tracing::warn!(conversation_id = %channel, %error, "signing a removal from all chats failed for a channel");
        maybe_failure.get_or_insert(error);
      }
    }
    let leftovers: Vec<ConversationId> = super::group_health::all_group_leftovers(&self.store)?
      .into_iter()
      .filter(|(_, leftover)| *leftover == user)
      .map(|(conversation, _)| conversation)
      .collect();
    for conversation in leftovers {
      if let Err(error) = self.admin_remove_group_leftover(conversation, user).await {
        tracing::warn!(conversation_id = %conversation, %error, "removing a leftover from the group failed");
        maybe_failure.get_or_insert(error);
      }
    }
    match maybe_failure {
      Some(failure) => Err(failure),
      None => Ok(()),
    }
  }

  /// Says goodbye, then leaves: this device posts a signed request to remove its user, which the
  /// remaining members commit (MLS doesn't let a member commit its own removal). Once a commit
  /// removed this device, the pending queue drops the server's membership; `Ok` before that. The
  /// goodbye is part of the pending leave, so leaving again after a crash doesn't repeat it.
  pub async fn leave_channel(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let me = self.account.user_id;
    // Asked first, so a leave the server refuses (the last channel admin) sends no goodbye.
    self.api.check_member_removal(conversation, me).await?;
    let maybe_event = self.has_group(conversation)?.then_some(ConversationEvent::Left);
    self.record_and_run_removal(conversation, me, maybe_event).await
  }

  /// "Retry" on a removal or leave shown as failed, or "remove" on an admin's unconfirmed one
  /// (e2ee.md §4.6): this user decides and signs it, never the server's word (security review
  /// A8). Somebody the server merely stopped listing is never offered for removal (#661).
  pub async fn retry_removal(&mut self, conversation: ConversationId, user: UserId) -> Result<(), CoreError> {
    let key = PendingChange::removal_key(conversation, user);
    if self.stored_pending(&key)?.is_none() {
      return Err(CoreError::InvalidRequest(
        "no removal of this user to retry".into(),
      ));
    }
    self.record_and_run_removal(conversation, user, None).await
  }

  /// Revokes one of this user's other devices: on the server first, so a lost laptop loses its
  /// session at once, then in the signed device list and every group this device shares with it.
  /// A crash or a lost answer in between is finished on the next round.
  pub async fn revoke_device(&mut self, device: DeviceId) -> Result<(), CoreError> {
    if device == self.account.device_id {
      return Err(CoreError::Corrupt("a device can't revoke itself".into()));
    }
    let change = PendingChange::Revocation {
      device_id: device,
      step: RevocationStep::Server,
      not_before_ms: 0,
    };
    self.queue_pending(change)?;
    let result = self.run_revocation(device, RevocationStep::Server).await;
    self.settle_pending(&PendingChange::revocation_key(device), result)
  }

  /// Records a removal the server allowed in the pending queue, then runs it. A removal started
  /// before (its answer lost, the app killed, or shown as failed) is continued from the step it
  /// reached, with the announcement it already has. `Ok` also when a leave waits for the members'
  /// commit or an admin watches its change; the pending queue finishes those.
  async fn record_and_run_removal(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    maybe_event: Option<ConversationEvent>,
  ) -> Result<(), CoreError> {
    let key = PendingChange::removal_key(conversation, user);
    let (step, maybe_announcement) = match self.stored_pending(&key)? {
      // "Remove" on an unconfirmed one: the admin decides now, so it is signed and sent afresh.
      Some(PendingChange::Removal {
        state: RemovalState::Unconfirmed,
        maybe_announcement,
        ..
      }) => (RemovalStep::Group, maybe_announcement),
      Some(PendingChange::Removal {
        step,
        maybe_announcement,
        ..
      }) => (step, maybe_announcement),
      _ => (
        RemovalStep::Group,
        maybe_event.map(|event| PendingAnnouncement {
          client_message_id: records::new_client_message_id(),
          event,
        }),
      ),
    };
    self.queue_pending(PendingChange::Removal {
      conversation_id: conversation,
      user_id: user,
      step,
      not_before_ms: 0,
      attempts: 0,
      state: RemovalState::Retrying,
      maybe_announcement,
    })?;
    match self
      .run_removal(conversation, user, step, RemovalRun::Decided)
      .await
    {
      Ok(RemovalProgress::Done) => self.store.delete(Table::PendingChanges, &key).map_err(Into::into),
      Ok(RemovalProgress::Waiting) => Ok(()),
      Err(error) => {
        if !is_worth_retrying(&error) {
          let reached = self.stored_pending(&key)?;
          match reached.and_then(|reached| self.retry_later(reached)) {
            Some(later) => self.queue_pending(later)?,
            None => self.store.delete(Table::PendingChanges, &key)?,
          }
        }
        Err(error)
      },
    }
  }

  /// Runs a removal from `step` on; also continues one from the pending queue (`run`). Each
  /// finished step is stored before the next starts.
  pub(crate) async fn run_removal(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    mut step: RemovalStep,
    mut run: RemovalRun,
  ) -> Result<RemovalProgress, CoreError> {
    if step == RemovalStep::Group {
      // Signed just now: what follows rests on this decision.
      run = RemovalRun::Decided;
      self.send_announcement(conversation, user, true).await?;
      step = self.remove_from_group(conversation, user).await?;
      self.set_removal_step(conversation, user, step, 0)?;
    }
    self.send_announcement(conversation, user, false).await?;
    if let RemovalStep::AwaitCommit { signed_epoch } = step {
      if !self.is_own_removal_committed(conversation, signed_epoch).await? {
        return Ok(RemovalProgress::Waiting);
      }
      step = RemovalStep::Server;
      self.set_removal_step(conversation, user, step, 0)?;
    }
    if let RemovalStep::Watch {
      signed_epoch,
      since_ms,
      probes,
    } = step
    {
      return self
        .watch_removal(conversation, user, signed_epoch, since_ms, probes, run)
        .await;
    }
    if run == RemovalRun::Resumed && matches!(step, RemovalStep::ServerThenWatch { .. }) {
      // An admin outside the group can't see whether somebody added the user back since it
      // signed; the server's list can't tell either. The admin decides ("remove" or "keep").
      tracing::warn!(conversation_id = %conversation, "an admin's removal resumed later is unconfirmed; waiting for the admin");
      self.mark_unconfirmed(conversation, user)?;
      return Ok(RemovalProgress::Waiting);
    }
    if self.is_back_in_group(conversation, user)? {
      // Added again since the removal (its commit removed them, or it was refused as stale): the
      // server step, maybe days later, must not drop the new membership.
      tracing::info!(conversation_id = %conversation, "the user is in the group again; dropped the removal");
      return Ok(RemovalProgress::Done);
    }
    match self.api.remove_member(conversation, user).await {
      Ok(()) => {},
      // Gone already: somebody who left no longer sees the conversation.
      Err(error) if error.is_not_found() => {},
      Err(error) => return Err(error.into()),
    }
    match step {
      // An epoch-0 group has nobody who could have committed anything yet; nothing to watch.
      RemovalStep::ServerThenWatch { signed_epoch } if signed_epoch > 0 => {
        let now = self.now_ms();
        let watch = RemovalStep::Watch {
          signed_epoch,
          since_ms: now,
          probes: 0,
        };
        self.set_removal_step(conversation, user, watch, now.saturating_add(self.backoff_ms(1)))?;
        Ok(RemovalProgress::Waiting)
      },
      _ => Ok(RemovalProgress::Done),
    }
  }

  /// Puts the removal's goodbye (`is_before_group_step`) or announcement into the outbox, with
  /// the message ID chosen when the removal was recorded: a second round finds the message in
  /// the store and doesn't write it again, and the server deduplicates on the ID. A goodbye has
  /// to reach the server before the removal goes on (the leaver can't write afterwards).
  async fn send_announcement(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    is_before_group_step: bool,
  ) -> Result<(), CoreError> {
    let key = PendingChange::removal_key(conversation, user);
    let Some(PendingChange::Removal {
      step,
      not_before_ms,
      attempts,
      state,
      maybe_announcement: Some(announcement),
      ..
    }) = self.stored_pending(&key)?
    else {
      return Ok(());
    };
    if announcement.is_before_group_step() != is_before_group_step {
      return Ok(());
    }
    let id = announcement.client_message_id;
    if self.store.message(conversation, id)?.is_none() {
      let draft = records::Draft {
        conversation_id: conversation,
        maybe_thread_id: None,
        content: MessageContent::Event {
          event: announcement.event.clone(),
        },
      };
      records::enqueue_message_as(&self.store, self.account.user_id, self.now_ms(), draft, id)?;
    }
    if let Err(error) = self.flush_outbox().await {
      tracing::debug!(conversation_id = %conversation, %error, "announcement waits in the outbox");
    }
    let outbox_key = records::outbox_key(conversation, id);
    let is_waiting = records::get_json::<records::OutboxEntry>(&self.store, Table::Outbox, &outbox_key)?
      .is_some_and(|entry| entry.maybe_last_error.is_none());
    if is_before_group_step && is_waiting {
      return Err(CoreError::GoodbyePending(conversation));
    }
    // In the outbox (or sent): from there it goes out once, whatever happens to the removal.
    self.queue_pending(PendingChange::Removal {
      conversation_id: conversation,
      user_id: user,
      step,
      not_before_ms,
      attempts,
      state,
      maybe_announcement: None,
    })
  }

  /// The MLS side of a removal, and the step that follows. Idempotent: a user no longer in the
  /// group is skipped.
  async fn remove_from_group(
    &mut self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<RemovalStep, CoreError> {
    let action = MembershipAction::RemoveUser {
      user_id: convert::user(user),
    };
    if user == self.account.user_id {
      // Leaving: posted also without a group here, for this user's other devices in it. The
      // members commit it (moot if none of them is in the group).
      if self.has_group(conversation)? && !self.has_other_users(conversation)? {
        // Nobody else in the group to commit it, or to read anything after.
        return Ok(RemovalStep::Server);
      }
      let signed_epoch = self.post_admin_change(conversation, action).await?;
      return Ok(if self.has_group(conversation)? {
        RemovalStep::AwaitCommit { signed_epoch }
      } else {
        // Nothing here to watch the commit with; the user's other devices drop the group when
        // the server's removal reaches them.
        RemovalStep::Server
      });
    }
    if self.has_group(conversation)? {
      self
        .commit_change(conversation, ChangeSource::Own(action))
        .await?;
      return Ok(RemovalStep::Server);
    }
    // An admin outside the group: the members commit the signed change (admin.md §2.2). The
    // admin can't see that commit, so it watches the change afterwards.
    let signed_epoch = self.post_admin_change(conversation, action).await?;
    Ok(RemovalStep::ServerThenWatch { signed_epoch })
  }

  /// One check of an admin's posted removal ([`RemovalStep::Watch`]): signs it again once the
  /// group got half as many epochs past it as members accept, so it is never refused as too old.
  /// Done after `removal_watch`, after `removal_attempts` signings, or once the admin's own group
  /// shows the user gone.
  async fn watch_removal(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    signed_epoch: u64,
    since_ms: i64,
    probes: u32,
    run: RemovalRun,
  ) -> Result<RemovalProgress, CoreError> {
    let now = self.now_ms();
    if self.has_group(conversation)? {
      // The admin joined meanwhile and sees the group itself: somebody still in it whom the
      // server doesn't list shows up as left in the group; one in it and listed may have been
      // added again, which this removal must not undo.
      return Ok(RemovalProgress::Done);
    }
    let watch_ms = millis(self.settings.removal_watch);
    // Measured from the first signing: signing again doesn't extend it.
    if now.saturating_sub(since_ms) > watch_ms {
      return Ok(RemovalProgress::Done);
    }
    let current = self.probe_epoch(conversation, signed_epoch).await?;
    let key = PendingChange::removal_key(conversation, user);
    let attempts = match self.stored_pending(&key)? {
      Some(PendingChange::Removal { attempts, .. }) => attempts,
      _ => 0,
    };
    if current.saturating_sub(signed_epoch) < MAX_CHANGE_AGE_EPOCHS / 2 {
      let probes = probes.saturating_add(1);
      let watch = RemovalStep::Watch {
        signed_epoch,
        since_ms,
        probes,
      };
      self.set_removal_step(
        conversation,
        user,
        watch,
        now.saturating_add(self.backoff_ms(probes)),
      )?;
      return Ok(RemovalProgress::Waiting);
    }
    let attempts = attempts.saturating_add(1);
    if attempts > self.settings.removal_attempts {
      tracing::warn!(conversation_id = %conversation, "stopped signing a removal again; members' devices show whether it went through");
      return Ok(RemovalProgress::Done);
    }
    if run == RemovalRun::Resumed && self.is_listed_again(conversation, user).await? {
      // Added back, or a server that wants the removal forgotten while it holds the change back:
      // not signed again, not dropped either; the admin decides.
      tracing::warn!(conversation_id = %conversation, "the server lists a removed user again; waiting for the admin");
      self.mark_unconfirmed(conversation, user)?;
      return Ok(RemovalProgress::Waiting);
    }
    tracing::info!(conversation_id = %conversation, signed_epoch, current_epoch = current, "signing an admin removal again before members would refuse it as too old");
    let action = MembershipAction::RemoveUser {
      user_id: convert::user(user),
    };
    let signed_epoch = self.post_admin_change(conversation, action).await?;
    self.queue_pending(PendingChange::Removal {
      conversation_id: conversation,
      user_id: user,
      step: RemovalStep::Watch {
        signed_epoch,
        since_ms,
        probes: 0,
      },
      not_before_ms: now.saturating_add(self.backoff_ms(1)),
      attempts,
      state: RemovalState::Retrying,
      maybe_announcement: None,
    })?;
    Ok(RemovalProgress::Waiting)
  }

  /// The group's current epoch, for a device outside it (which can't read the queue): a post for
  /// an epoch the group has left for sure (epochs only grow) is answered with an epoch conflict
  /// that names the current one, and nothing is stored. `post_admin_change` relies on the same.
  async fn probe_epoch(&mut self, conversation: ConversationId, signed_epoch: u64) -> Result<u64, CoreError> {
    let request = PostMlsMessageRequest {
      device_id: self.account.device_id,
      epoch: signed_epoch.saturating_sub(1),
      kind: MlsMessageKind::Proposal,
      // Nothing a member would act on, should a broken server store it after all.
      payload: Base64Bytes::new(ADMIN_CHANGE_MAGIC.to_vec()),
    };
    match self.api.post_mls_message(conversation, &request).await {
      Err(error) if error.is_epoch_conflict() => match current_epoch_of(&error) {
        Some(current) => Ok(current),
        None => Err(error.into()),
      },
      Err(error) => Err(error.into()),
      Ok(_) => Err(CoreError::GaveUp {
        attempts: 1,
        reason: "the server took a post for a past epoch".into(),
      }),
    }
  }

  /// Whether a member committed this device's removal, which drops the group here. Looks at the
  /// local state only (the commit arrives over the gateway like any other); posts the signed
  /// removal again when the one waiting got too old for members to commit.
  async fn is_own_removal_committed(
    &mut self,
    conversation: ConversationId,
    signed_epoch: u64,
  ) -> Result<bool, CoreError> {
    let Some(epoch) = self.mls_epoch(conversation)? else {
      return Ok(true);
    };
    if !self.has_other_users(conversation)? {
      // The others left meanwhile: nobody is left to commit it.
      return Ok(true);
    }
    if epoch.saturating_sub(signed_epoch) >= MAX_CHANGE_AGE_EPOCHS / 2 {
      let action = MembershipAction::RemoveUser {
        user_id: convert::user(self.account.user_id),
      };
      let signed_epoch = self.post_admin_change(conversation, action).await?;
      self.set_removal_step(
        conversation,
        self.account.user_id,
        RemovalStep::AwaitCommit { signed_epoch },
        0,
      )?;
    }
    Ok(false)
  }

  /// A signed addition of `user` (this device's own, or one it saw committed) is newer than any
  /// removal of them this device started: that removal is dropped, so its later steps (signing
  /// again, the server step) don't undo the addition.
  pub(crate) fn cancel_removal_on_add(
    &mut self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<(), CoreError> {
    let key = PendingChange::removal_key(conversation, user);
    if user != self.account.user_id && self.stored_pending(&key)?.is_some() {
      tracing::info!(conversation_id = %conversation, "the user was added again; dropped the pending removal");
      self.store.delete(Table::PendingChanges, &key)?;
    }
    Ok(())
  }

  /// Drops a pending removal on the user's "keep" (an unconfirmed or failed one).
  pub async fn discard_removal(
    &mut self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<(), CoreError> {
    self.store.delete(
      Table::PendingChanges,
      &PendingChange::removal_key(conversation, user),
    )?;
    Ok(())
  }

  /// Keeps a removal, with its step, until the user says "remove" or "keep".
  fn mark_unconfirmed(&self, conversation: ConversationId, user: UserId) -> Result<(), CoreError> {
    if let Some(PendingChange::Removal {
      step,
      not_before_ms,
      attempts,
      maybe_announcement,
      ..
    }) = self.stored_pending(&PendingChange::removal_key(conversation, user))?
    {
      self.queue_pending(PendingChange::Removal {
        conversation_id: conversation,
        user_id: user,
        step,
        not_before_ms,
        attempts,
        state: RemovalState::Unconfirmed,
        maybe_announcement,
      })?;
    }
    Ok(())
  }

  /// Whether somebody else this device removed is in its group again (added back since).
  fn is_back_in_group(&mut self, conversation: ConversationId, user: UserId) -> Result<bool, CoreError> {
    Ok(
      user != self.account.user_id
        && self.has_group(conversation)?
        && self.group_users(conversation)?.contains(&user),
    )
  }

  /// For an admin outside the group whose removal's server step is done: the server listing the
  /// user again means somebody added them back. Not trusting it can only mean not signing.
  async fn is_listed_again(&mut self, conversation: ConversationId, user: UserId) -> Result<bool, CoreError> {
    match self.api.list_members(conversation).await {
      Ok(members) => Ok(members.members.iter().any(|member| member.user.id == user)),
      // Not allowed to see the members (not an instance admin): nothing to go by.
      Err(error) if error.is_forbidden() || error.is_not_found() => Ok(false),
      Err(error) => Err(error.into()),
    }
  }

  /// Whether the group has a device of a user other than this one.
  fn has_other_users(&mut self, conversation: ConversationId) -> Result<bool, CoreError> {
    let me = self.account.user_id;
    Ok(self.group_users(conversation)?.iter().any(|user| *user != me))
  }

  /// Runs a revocation from `step` on; also continues one from the pending queue. A group whose
  /// commit fails doesn't keep the device in the others: the first error is returned after all
  /// were tried, and the next round tries again.
  pub(crate) async fn run_revocation(
    &mut self,
    device: DeviceId,
    step: RevocationStep,
  ) -> Result<(), CoreError> {
    if step == RevocationStep::Server {
      match self.api.revoke_device(device).await {
        Ok(()) => {},
        // Revoked already (the answer to an earlier try was lost).
        Err(error) if error.is_not_found() => {},
        Err(error) => return Err(error.into()),
      }
      self.queue_pending(PendingChange::Revocation {
        device_id: device,
        step: RevocationStep::Groups,
        not_before_ms: 0,
      })?;
    }
    // A list that names the device revoked already (published before a crash, or by the device
    // whose revocation this one finishes) needn't be signed again.
    if !self.is_revoked_in_stored_list(device)? {
      self.publish_device_list(&[device]).await?;
    }
    let conversations: Vec<_> = self.cached_conversations()?.into_keys().collect();
    let mut maybe_first_error = None;
    for conversation in conversations {
      if !self.has_group(conversation)? || !self.group_device_ids(conversation)?.contains(&device) {
        continue;
      }
      let removal = ChangeSource::Own(MembershipAction::RemoveDevice {
        user_id: convert::user(self.account.user_id),
        device_id: convert::device(device),
      });
      if let Err(error) = self.commit_change(conversation, removal).await {
        tracing::warn!(conversation_id = %conversation, %error, "a revoked device is still in a group");
        maybe_first_error.get_or_insert(error);
      }
    }
    maybe_first_error.map_or(Ok(()), Err)
  }

  /// The server reported `device`, another device of this user, revoked. Its word alone changes
  /// nothing: after `revocation_check_delay` this device reads the user's signed device list and,
  /// only if that names the device revoked, finishes what the revoking device may not have, its
  /// removal from every group they share (#232, e2ee.md §3). Nothing to check when this device is
  /// the one revoking it, for a device it doesn't know (in no shared group and not in the last
  /// verified list: the server may make IDs up), for one checked already, or beyond
  /// `max_pending_revocation_checks`.
  pub(crate) fn queue_revocation_check(&mut self, device: DeviceId) -> Result<(), CoreError> {
    let check_key = PendingChange::revocation_check_key(device);
    let is_own_revocation = self
      .stored_pending(&PendingChange::revocation_key(device))?
      .is_some();
    if device == self.account.device_id || is_own_revocation || self.stored_pending(&check_key)?.is_some() {
      return Ok(());
    }
    let is_in_shared_group = self.shares_group_with(device)?;
    let me = self.account.user_id;
    let maybe_list = self.stored_device_list(me, &self.account.identity_public_key())?;
    let crypto_device = convert::device(device);
    let is_revoked_in_list = maybe_list
      .as_ref()
      .is_some_and(|list| list.is_revoked(crypto_device));
    // Finished already (by this device, or the list it stored names it and no group has it).
    if is_revoked_in_list && !is_in_shared_group {
      return Ok(());
    }
    let is_listed = maybe_list.as_ref().is_some_and(|list| {
      list
        .devices()
        .iter()
        .any(|entry| entry.device_id == crypto_device)
    });
    if !is_in_shared_group && !is_listed {
      tracing::debug!(device_id = %device, "the server reported an unknown device revoked; ignored");
      return Ok(());
    }
    if self.pending_revocation_checks()? >= self.settings.max_pending_revocation_checks {
      tracing::warn!(device_id = %device, "too many revocation checks waiting; this one is not queued");
      return Ok(());
    }
    let delay_ms = millis(self.settings.revocation_check_delay);
    self.queue_pending(PendingChange::RevocationCheck {
      device_id: device,
      not_before_ms: self.now_ms().saturating_add(delay_ms),
      attempts: 0,
    })
  }

  /// How many [`PendingChange::RevocationCheck`]s wait in the queue.
  fn pending_revocation_checks(&self) -> Result<usize, CoreError> {
    let pending = self.store.list(Table::PendingChanges)?;
    Ok(
      pending
        .iter()
        .filter(|(key, _)| key.starts_with(REVOCATION_CHECK_KEY_PREFIX))
        .count(),
    )
  }

  /// Runs a [`PendingChange::RevocationCheck`] that is due: finishes the revocation if the signed
  /// list names the device revoked, else checks again later, up to `removal_attempts` times.
  pub(crate) async fn apply_revocation_check(
    &mut self,
    change: &PendingChange,
  ) -> Result<PendingOutcome, CoreError> {
    let PendingChange::RevocationCheck {
      device_id,
      not_before_ms,
      attempts,
    } = change
    else {
      return Ok(PendingOutcome::Done);
    };
    if self.now_ms() < *not_before_ms {
      return Ok(PendingOutcome::Later(change.clone()));
    }
    if self.is_revoked_in_own_list(*device_id).await? {
      self.finish_listed_revocation(*device_id).await?;
      return Ok(PendingOutcome::Done);
    }
    let attempts = attempts.saturating_add(1);
    if attempts >= self.settings.removal_attempts {
      tracing::info!(device_id = %device_id, "no signed revocation of the device the server reported; nothing removed");
      return Ok(PendingOutcome::Done);
    }
    Ok(PendingOutcome::Later(PendingChange::RevocationCheck {
      device_id: *device_id,
      not_before_ms: self.now_ms().saturating_add(self.backoff_ms(attempts)),
      attempts,
    }))
  }

  /// After a full resync (the gateway events may be gone): finishes every revocation the signed
  /// device list names whose device is still in one of this device's groups.
  pub(crate) async fn finish_listed_revocations(&mut self) -> Result<(), CoreError> {
    let me = self.account.user_id;
    let Some(list) = self
      .fetch_device_list(me, &self.account.identity_public_key())
      .await?
    else {
      return Ok(());
    };
    let mut revoked = std::collections::BTreeSet::new();
    for conversation in self.cached_conversations()?.into_keys() {
      if !self.has_group(conversation)? {
        continue;
      }
      for member in self.group_or_error(conversation)?.members()? {
        let device = convert::device_from_crypto(member.device_id)?;
        if member.user_id == convert::user(me)
          && device != self.account.device_id
          && list.is_revoked(member.device_id)
        {
          revoked.insert(device);
        }
      }
    }
    for device in revoked {
      self.finish_listed_revocation(device).await?;
    }
    Ok(())
  }

  /// Removes `device`, which the verified list names revoked, from the groups this device shares
  /// with it; a group that refuses is retried through the pending queue like an own revocation.
  async fn finish_listed_revocation(&mut self, device: DeviceId) -> Result<(), CoreError> {
    let key = PendingChange::revocation_key(device);
    if self.stored_pending(&key)?.is_some() {
      // Already under way (this device's own, or an earlier check's).
      return Ok(());
    }
    tracing::info!(device_id = %device, "finishing a revocation the signed device list names");
    self.queue_pending(PendingChange::Revocation {
      device_id: device,
      step: RevocationStep::Groups,
      not_before_ms: 0,
    })?;
    let result = self.run_revocation(device, RevocationStep::Groups).await;
    match self.settle_pending(&key, result) {
      Err(error) if error.is_transient() => Err(error),
      // Kept in the queue for the next round (`settle_pending`).
      Err(error) => {
        tracing::warn!(device_id = %device, %error, "a revoked device is still in a group");
        Ok(())
      },
      Ok(()) => Ok(()),
    }
  }

  /// Whether one of this device's groups has `device`.
  fn shares_group_with(&mut self, device: DeviceId) -> Result<bool, CoreError> {
    for conversation in self.cached_conversations()?.into_keys() {
      if self.has_group(conversation)? && self.group_device_ids(conversation)?.contains(&device) {
        return Ok(true);
      }
    }
    Ok(false)
  }

  /// Whether this user's signed device list, fetched and verified with the identity key (never
  /// older than the stored one), names `device` revoked.
  async fn is_revoked_in_own_list(&mut self, device: DeviceId) -> Result<bool, CoreError> {
    let me = self.account.user_id;
    let maybe_list = self
      .fetch_device_list(me, &self.account.identity_public_key())
      .await?;
    Ok(maybe_list.is_some_and(|list| list.is_revoked(convert::device(device))))
  }

  /// [`Self::is_revoked_in_own_list`] with the list this device stored last, without a request.
  fn is_revoked_in_stored_list(&self, device: DeviceId) -> Result<bool, CoreError> {
    let me = self.account.user_id;
    let maybe_list = self.stored_device_list(me, &self.account.identity_public_key())?;
    Ok(maybe_list.is_some_and(|list| list.is_revoked(convert::device(device))))
  }

  /// Drops a pending revocation once it is done; keeps it when it may work later, or, after the
  /// server already revoked the device, retries its groups after `removal_retry`.
  fn settle_pending(&mut self, key: &str, result: Result<(), CoreError>) -> Result<(), CoreError> {
    match &result {
      Ok(()) => self.store.delete(Table::PendingChanges, key)?,
      Err(error) if is_worth_retrying(error) => {},
      Err(_) => match self
        .stored_pending(key)?
        .and_then(|reached| self.retry_later(reached))
      {
        Some(later) => self.queue_pending(later)?,
        None => self.store.delete(Table::PendingChanges, key)?,
      },
    }
    result
  }

  /// A removal or revocation that failed for good after its group side (for a revocation: the
  /// server side) was done, due again later; `None` for one that can simply be dropped (nothing
  /// changed yet). A removal waits twice as long after each refusal, up to `removal_retry_max`,
  /// and shows as failed after `removal_attempts` refusals, until the user tries again.
  pub(crate) fn retry_later(&self, reached: PendingChange) -> Option<PendingChange> {
    let now = self.now_ms();
    match reached {
      PendingChange::Removal {
        conversation_id,
        user_id,
        step:
          step @ (RemovalStep::Server | RemovalStep::AwaitCommit { .. } | RemovalStep::ServerThenWatch { .. }),
        attempts,
        maybe_announcement,
        ..
      } => {
        let attempts = attempts.saturating_add(1);
        let state = if attempts >= self.settings.removal_attempts {
          tracing::warn!(conversation_id = %conversation_id, attempts, "a removal failed; waiting for the user to try again");
          RemovalState::Failed
        } else {
          RemovalState::Retrying
        };
        Some(PendingChange::Removal {
          conversation_id,
          user_id,
          step,
          not_before_ms: now.saturating_add(self.backoff_ms(attempts)),
          attempts,
          state,
          maybe_announcement,
        })
      },
      PendingChange::Revocation {
        device_id,
        step: RevocationStep::Groups,
        ..
      } => Some(PendingChange::Revocation {
        device_id,
        step: RevocationStep::Groups,
        not_before_ms: now.saturating_add(millis(self.settings.removal_retry)),
      }),
      _ => None,
    }
  }

  /// `removal_retry` doubled for each try after the first, at most `removal_retry_max`.
  fn backoff_ms(&self, tries: u32) -> i64 {
    let base = millis(self.settings.removal_retry);
    let ceiling = millis(self.settings.removal_retry_max).max(base);
    let doublings = tries.saturating_sub(1).min(32);
    base.saturating_mul(1_i64 << doublings).min(ceiling)
  }

  /// Moves a stored removal to `step`, keeping its tries and announcement.
  fn set_removal_step(
    &self,
    conversation: ConversationId,
    user: UserId,
    step: RemovalStep,
    not_before_ms: i64,
  ) -> Result<(), CoreError> {
    let (attempts, state, maybe_announcement) =
      match self.stored_pending(&PendingChange::removal_key(conversation, user))? {
        Some(PendingChange::Removal {
          attempts,
          state,
          maybe_announcement,
          ..
        }) => (attempts, state, maybe_announcement),
        _ => (0, RemovalState::Retrying, None),
      };
    self.queue_pending(PendingChange::Removal {
      conversation_id: conversation,
      user_id: user,
      step,
      not_before_ms,
      attempts,
      state,
      maybe_announcement,
    })
  }

  /// The pending change stored under `key`, as the last finished step left it.
  pub(crate) fn stored_pending(&self, key: &str) -> Result<Option<PendingChange>, CoreError> {
    records::get_json(&self.store, Table::PendingChanges, key)
  }
}

/// Whether a removal runs because the user decided it now, or resumed from the pending queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemovalRun {
  Decided,
  Resumed,
}

/// How far a removal got in one round.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RemovalProgress {
  Done,
  /// Leaving: waits for a member to commit this device's removal; or an admin outside the group
  /// watches its change.
  Waiting,
}

/// Whether a removal or revocation that failed this way stays queued for the next round.
pub(crate) fn is_worth_retrying(error: &CoreError) -> bool {
  error.is_transient() || error.is_retry_later() || matches!(error, CoreError::GaveUp { .. })
}

fn millis(duration: std::time::Duration) -> i64 {
  i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}
