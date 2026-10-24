//! Removing members, leaving and revoking devices as one recoverable operation (e2ee.md §4.6,
//! removal order): the MLS side first, the server's membership afterwards, with the progress kept
//! in the pending queue so a crash or a lost answer in between is finished on the next round.
//! Every intermediate state is safe: before the MLS step nothing changed; after it the removed
//! person has no key for the new epoch, even while the server still lists them.

use teamcraft_crypto::membership::MembershipAction;
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
  /// the group the signed change waits in the group's queue until a member's client commits it
  /// (admin.md §2.2), and only then is the server's membership dropped. Deactivating someone calls
  /// this per conversation; the server's own removal is only a hint to the clients (security
  /// review A8).
  pub async fn admin_remove_member(
    &mut self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<(), CoreError> {
    self.start_removal(conversation, user).await
  }

  /// Says goodbye, then leaves: this device posts a signed request to remove its user, which the
  /// remaining members commit (MLS doesn't let a member commit its own removal), then the server
  /// drops the membership.
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

  /// Records a removal the server allowed in the pending queue, then runs it.
  async fn record_and_run_removal(
    &mut self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<(), CoreError> {
    self.queue_pending(PendingChange::Removal {
      conversation_id: conversation,
      user_id: user,
      step: RemovalStep::Group,
    })?;
    let result = self.run_removal(conversation, user, RemovalStep::Group).await;
    self.settle_pending(&PendingChange::removal_key(conversation, user), result)
  }

  /// Runs a removal from `step` on; also continues one from the pending queue.
  pub(crate) async fn run_removal(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    step: RemovalStep,
  ) -> Result<(), CoreError> {
    if step == RemovalStep::Group {
      self.remove_from_group(conversation, user).await?;
      self.queue_pending(PendingChange::Removal {
        conversation_id: conversation,
        user_id: user,
        step: RemovalStep::Server,
      })?;
    }
    match self.api.remove_member(conversation, user).await {
      Ok(()) => Ok(()),
      // Gone already: somebody who left no longer sees the conversation.
      Err(error) if error.is_not_found() => Ok(()),
      Err(error) => Err(error.into()),
    }
  }

  /// The MLS side of a removal. Idempotent: a user no longer in the group is skipped.
  async fn remove_from_group(&mut self, conversation: ConversationId, user: UserId) -> Result<(), CoreError> {
    let action = MembershipAction::RemoveUser {
      user_id: convert::user(user),
    };
    let is_me = user == self.account.user_id;
    match (is_me, self.has_group(conversation)?) {
      // This device never joined the group: nothing to give up.
      (true, false) => Ok(()),
      // Leaving, or an admin outside the group: the members commit the signed change
      // (admin.md §2.2).
      (true, true) | (false, false) => self.post_admin_change(conversation, action).await,
      (false, true) => self.commit_change(conversation, ChangeSource::Own(action)).await,
    }
  }

  /// Runs a revocation from `step` on; also continues one from the pending queue.
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
      })?;
    }
    self.publish_device_list(&[device]).await?;
    let conversations: Vec<_> = self.cached_conversations()?.into_keys().collect();
    for conversation in conversations {
      if self.has_group(conversation)? && self.group_device_ids(conversation)?.contains(&device) {
        self
          .commit_change(
            conversation,
            ChangeSource::Own(MembershipAction::RemoveDevice {
              user_id: convert::user(self.account.user_id),
              device_id: convert::device(device),
            }),
          )
          .await?;
      }
    }
    Ok(())
  }

  /// Drops a pending removal or revocation once it is done or failed for good; keeps it when it
  /// may work later (offline, signed out, other commits kept winning).
  fn settle_pending(&mut self, key: &str, result: Result<(), CoreError>) -> Result<(), CoreError> {
    match &result {
      Err(error) if is_worth_retrying(error) => {},
      _ => self.store.delete(Table::PendingChanges, key)?,
    }
    result
  }

  /// The pending change stored under `key`, as the last finished step left it.
  pub(crate) fn stored_pending(&self, key: &str) -> Result<Option<PendingChange>, CoreError> {
    records::get_json(&self.store, Table::PendingChanges, key)
  }
}

/// Whether a removal or revocation that failed this way stays queued for the next round.
pub(crate) fn is_worth_retrying(error: &CoreError) -> bool {
  error.is_transient() || error.is_retry_later() || matches!(error, CoreError::GaveUp { .. })
}
