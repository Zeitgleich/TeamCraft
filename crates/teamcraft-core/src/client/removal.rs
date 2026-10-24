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
  PendingAnnouncement, PendingChange, RemovalState, RemovalStep, RevocationStep, current_epoch_of,
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

  /// "Retry" on a removal or leave shown as failed, or "remove" on somebody the server no longer
  /// lists who is still in the group (e2ee.md §4.6): this user decides and signs it, never the
  /// server's word (security review A8).
  pub async fn retry_removal(&mut self, conversation: ConversationId, user: UserId) -> Result<(), CoreError> {
    let key = PendingChange::removal_key(conversation, user);
    if self.stored_pending(&key)?.is_some() {
      return self.record_and_run_removal(conversation, user, None).await;
    }
    if !self.group_leftovers(conversation)?.contains(&user) {
      return Err(CoreError::InvalidRequest(
        "no removal of this user to retry".into(),
      ));
    }
    // The server's word is only why the user is asked; ask it again before signing anything.
    let members = self.api.list_members(conversation).await?;
    if members.members.iter().any(|member| member.user.id == user) {
      return self.note_group_leftovers(conversation);
    }
    let maybe_event = self
      .has_group(conversation)?
      .then_some(ConversationEvent::MemberRemoved { user });
    let result = self.record_and_run_removal(conversation, user, maybe_event).await;
    self.note_group_leftovers(conversation)?;
    result
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
    match self.run_removal(conversation, user, step).await {
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

  /// Runs a removal from `step` on; also continues one from the pending queue. Each finished step
  /// is stored before the next starts.
  pub(crate) async fn run_removal(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    mut step: RemovalStep,
  ) -> Result<RemovalProgress, CoreError> {
    if step == RemovalStep::Group {
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
      signed_at_ms,
      probes,
    } = step
    {
      return self
        .watch_removal(conversation, user, signed_epoch, signed_at_ms, probes)
        .await;
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
          signed_at_ms: now,
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
    signed_at_ms: i64,
    probes: u32,
  ) -> Result<RemovalProgress, CoreError> {
    let now = self.now_ms();
    if self.has_group(conversation)? {
      // The admin joined meanwhile and sees the group itself.
      if self.group_users(conversation)?.contains(&user) {
        let action = MembershipAction::RemoveUser {
          user_id: convert::user(user),
        };
        self
          .commit_change(conversation, ChangeSource::Own(action))
          .await?;
      }
      return Ok(RemovalProgress::Done);
    }
    let watch_ms = millis(self.settings.removal_watch);
    if now.saturating_sub(signed_at_ms) > watch_ms {
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
        signed_at_ms,
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
        signed_at_ms: now,
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
    self.publish_device_list(&[device]).await?;
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
