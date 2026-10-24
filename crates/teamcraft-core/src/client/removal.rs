//! Removing members, leaving and revoking devices as one recoverable operation (e2ee.md §4.6,
//! removal order): the MLS side first, the server's membership afterwards, with the progress kept
//! in the pending queue so a crash or a lost answer in between is finished on the next round.
//! When the remover commits, every intermediate state is safe: before the MLS step nothing
//! changed; after it the removed person has no key for the new epoch, even while the server still
//! lists them. A signed change only posted for others to commit is e2ee.md §4.6 and #231.

use teamcraft_crypto::membership::MembershipAction;
use teamcraft_crypto::mls::MAX_CHANGE_AGE_EPOCHS;
use teamcraft_proto::{ConversationId, DeviceId, UserId};

use super::Client;
use super::groups::ChangeSource;
use super::workspace::{PendingChange, RemovalStep, RevocationStep};
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{ConversationEvent, LocalStore, Table};

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Removes `user` from the MLS group, then on the server. The epoch key of the removal commit
  /// means the removed person can't read anything new (e2ee.md §4.1, §4.2).
  pub async fn remove_member(&mut self, conversation: ConversationId, user: UserId) -> Result<(), CoreError> {
    self.start_removal(conversation, user).await?;
    // After the key rotation, so the removed person doesn't read it.
    self
      .announce(conversation, ConversationEvent::MemberRemoved { user })
      .await
  }

  /// An admin removes `user` from a conversation, also one the admin isn't a member of: outside
  /// the group the signed change is posted into the group's queue for a member's client to commit
  /// (admin.md §2.2), then the server's membership is dropped. The admin can't see that commit; a
  /// change members refuse as stale leaves the user in the group (#231). Deactivating someone
  /// calls this per conversation; the server's own removal is only a hint to the clients (security
  /// review A8).
  pub async fn admin_remove_member(
    &mut self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<(), CoreError> {
    self.start_removal(conversation, user).await
  }

  /// Says goodbye, then leaves: this device posts a signed request to remove its user, which the
  /// remaining members commit (MLS doesn't let a member commit its own removal). Once a commit
  /// removed this device, the pending queue drops the server's membership; `Ok` before that.
  pub async fn leave_channel(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let me = self.account.user_id;
    // Asked first, so a leave the server refuses (the last channel admin) sends no goodbye.
    self.api.check_member_removal(conversation, me).await?;
    if self.has_group(conversation)? {
      self.announce(conversation, ConversationEvent::Left).await?;
    }
    self.record_and_run_removal(conversation, me).await
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

  /// Asks the server whether the removal is allowed, records it and runs it.
  async fn start_removal(&mut self, conversation: ConversationId, user: UserId) -> Result<(), CoreError> {
    // The server refuses before the group changes (a moderator removing an admin, the last channel
    // admin leaving), so the group never drops somebody the server keeps.
    self.api.check_member_removal(conversation, user).await?;
    self.record_and_run_removal(conversation, user).await
  }

  /// Records a removal the server allowed in the pending queue, then runs it. `Ok` also when a
  /// leave waits for the members' commit; the pending queue finishes it.
  async fn record_and_run_removal(
    &mut self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<(), CoreError> {
    self.store_removal(conversation, user, RemovalStep::Group, 0)?;
    let key = PendingChange::removal_key(conversation, user);
    match self.run_removal(conversation, user, RemovalStep::Group).await {
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
      step = self.remove_from_group(conversation, user).await?;
      self.store_removal(conversation, user, step, 0)?;
    }
    if let RemovalStep::AwaitCommit { signed_epoch } = step {
      if !self.is_own_removal_committed(conversation, signed_epoch).await? {
        return Ok(RemovalProgress::Waiting);
      }
      self.store_removal(conversation, user, RemovalStep::Server, 0)?;
    }
    match self.api.remove_member(conversation, user).await {
      Ok(()) => Ok(RemovalProgress::Done),
      // Gone already: somebody who left no longer sees the conversation.
      Err(error) if error.is_not_found() => Ok(RemovalProgress::Done),
      Err(error) => Err(error.into()),
    }
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
    } else {
      // An admin outside the group: the members commit the signed change (admin.md §2.2). The
      // admin can't see that commit; a change refused as stale is #231.
      self.post_admin_change(conversation, action).await?;
    }
    Ok(RemovalStep::Server)
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
      self.store_removal(
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
  /// server side) was done, due again after `removal_retry`; `None` for one that can simply be
  /// dropped (nothing changed yet).
  pub(crate) fn retry_later(&self, reached: PendingChange) -> Option<PendingChange> {
    let retry_ms = i64::try_from(self.settings.removal_retry.as_millis()).unwrap_or(i64::MAX);
    let not_before_ms = self.now_ms().saturating_add(retry_ms);
    match reached {
      PendingChange::Removal {
        conversation_id,
        user_id,
        step: step @ (RemovalStep::Server | RemovalStep::AwaitCommit { .. }),
        ..
      } => Some(PendingChange::Removal {
        conversation_id,
        user_id,
        step,
        not_before_ms,
      }),
      PendingChange::Revocation {
        device_id,
        step: RevocationStep::Groups,
        ..
      } => Some(PendingChange::Revocation {
        device_id,
        step: RevocationStep::Groups,
        not_before_ms,
      }),
      _ => None,
    }
  }

  fn store_removal(
    &self,
    conversation: ConversationId,
    user: UserId,
    step: RemovalStep,
    not_before_ms: i64,
  ) -> Result<(), CoreError> {
    self.queue_pending(PendingChange::Removal {
      conversation_id: conversation,
      user_id: user,
      step,
      not_before_ms,
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
  /// Leaving: waits for a member to commit this device's removal.
  Waiting,
}

/// Whether a removal or revocation that failed this way stays queued for the next round.
pub(crate) fn is_worth_retrying(error: &CoreError) -> bool {
  error.is_transient() || error.is_retry_later() || matches!(error, CoreError::GaveUp { .. })
}
