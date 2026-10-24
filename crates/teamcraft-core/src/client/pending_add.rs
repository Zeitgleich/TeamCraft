//! Adding somebody who has no device to add yet: a colleague who hasn't set up the app, whose
//! devices have no key package left, or whose devices don't pass the checks. The conversation is
//! made (and the person is a member on the server) right away; the encryption group gets them once
//! a device can be added. Waiting costs no network: the person's next device (`device_add`) makes
//! the addition due; a missing key package is retried with a growing delay.
//!
//! A DM keeps its setup record ([`Client::finish_dm_setup`]) for founding races and retries a
//! missing key package itself; only the wait for a first device is handed over to here.
//!
//! Only the device that added the person keeps the addition (docs/robustness-audit.md, F2).

use serde::{Deserialize, Serialize};
use teamcraft_crypto::membership::{HistoryShare, MembershipAction};
use teamcraft_proto::{ConversationId, HistoryPolicy, UserId};

use super::Client;
use super::groups::ChangeSource;
use super::workspace::{PendingChange, PendingOutcome};
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

/// Why a user isn't in the encryption group yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserAddWait {
  /// The user has no device: they haven't set up the app yet.
  NoDevice,
  /// The user's devices have no key package left on the server; they upload new ones when they
  /// come online.
  NoKeyPackage,
  /// None of the user's devices passed the checks (signed device list, credential, revocation):
  /// not retried; their next device makes it due again.
  Unverifiable,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// After this device added `user` to a channel: queues the addition for later when the group
  /// still lacks them, else drops one queued before.
  pub(crate) async fn queue_user_addition_if_missing(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    history: HistoryShare,
  ) -> Result<(), CoreError> {
    let key = PendingChange::user_add_key(conversation, user);
    if !self.has_group(conversation)? || self.group_users(conversation)?.contains(&user) {
      self.store.delete(Table::PendingChanges, &key)?;
      return Ok(());
    }
    let wait = self.why_not_addable(user).await?;
    self.queue_user_addition(conversation, user, history_policy(history), wait, 0)
  }

  /// A DM's setup found `user` without a device: queues their addition for their first device and
  /// returns `true`; `false` (nothing queued) when they have devices, so the setup goes on.
  pub(crate) async fn wait_for_first_device(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    history: HistoryShare,
  ) -> Result<bool, CoreError> {
    if self.why_not_addable(user).await? != UserAddWait::NoDevice {
      return Ok(false);
    }
    self.queue_user_addition(
      conversation,
      user,
      history_policy(history),
      UserAddWait::NoDevice,
      0,
    )?;
    Ok(true)
  }

  /// `user` registered a device: their waiting additions are due now.
  pub(crate) fn wake_user_additions(&mut self, user: UserId) -> Result<(), CoreError> {
    let now = self.now_ms();
    for change in records::list_json::<PendingChange>(&self.store, Table::PendingChanges)? {
      if let PendingChange::AddUser {
        conversation_id,
        user_id,
        history,
        wait,
        ..
      } = change
        && user_id == user
      {
        self.queue_pending(PendingChange::AddUser {
          conversation_id,
          user_id,
          history,
          wait,
          maybe_not_before_ms: Some(now),
          attempts: 0,
        })?;
      }
    }
    Ok(())
  }

  /// Tries a due [`PendingChange::AddUser`]. Done once the user is in the group (or no longer a
  /// member at all, or a DM's setup took over); back to waiting while they have no device or none
  /// passes the checks; retried later while their devices have no key package.
  pub(crate) async fn apply_user_addition(
    &mut self,
    change: &PendingChange,
  ) -> Result<PendingOutcome, CoreError> {
    let PendingChange::AddUser {
      conversation_id,
      user_id,
      history,
      attempts,
      maybe_not_before_ms,
      ..
    } = change
    else {
      return Ok(PendingOutcome::Done);
    };
    let (conversation, user) = (*conversation_id, *user_id);
    let is_due = maybe_not_before_ms.is_some_and(|not_before| self.now_ms() >= not_before);
    if !is_due {
      return Ok(PendingOutcome::Later(change.clone()));
    }
    if !self.has_group(conversation)? {
      return Ok(PendingOutcome::Done);
    }
    if let Some(others) = self.resume_dm_setup(conversation)? {
      return self
        .resume_dm_addition(conversation, user, history, &others)
        .await;
    }
    self.sync_mls(conversation).await?;
    if self.group_users(conversation)?.contains(&user) {
      return Ok(PendingOutcome::Done);
    }
    let members = self.api.list_members(conversation).await?;
    if !members.members.iter().any(|member| member.user.id == user) {
      // Removed (or left) meanwhile: nobody to add any more.
      return Ok(PendingOutcome::Done);
    }
    let mut wait = self.why_not_addable(user).await?;
    if wait != UserAddWait::NoDevice {
      let added = self
        .commit_change(
          conversation,
          ChangeSource::Own(MembershipAction::AddUser {
            user_id: convert::user(user),
            history: history_share(history),
          }),
        )
        .await;
      match added {
        Ok(()) if self.group_users(conversation)?.contains(&user) => {
          tracing::info!(conversation_id = %conversation, "added a user who had no device to add before");
          return Ok(PendingOutcome::Done);
        },
        Ok(()) => wait = UserAddWait::NoKeyPackage,
        Err(error) if error.is_transient() => return Err(error),
        Err(error) => {
          tracing::warn!(conversation_id = %conversation, %error, "no device of the added user passed the checks");
          wait = UserAddWait::Unverifiable;
        },
      }
    }
    let attempts = if wait == UserAddWait::NoKeyPackage {
      attempts.saturating_add(1)
    } else {
      0
    };
    Ok(PendingOutcome::Later(self.user_addition(
      conversation,
      user,
      history.clone(),
      wait,
      attempts,
    )))
  }

  /// A DM: its setup adds the user (and settles a founding race with the user's new device). The
  /// wait stays here only while the user still has no device; anything else is the setup's to
  /// retry.
  async fn resume_dm_addition(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    history: &HistoryPolicy,
    others: &[UserId],
  ) -> Result<PendingOutcome, CoreError> {
    self.finish_dm_setup(conversation, others).await?;
    if self.has_group(conversation)? && self.group_users(conversation)?.contains(&user) {
      return Ok(PendingOutcome::Done);
    }
    if self.why_not_addable(user).await? == UserAddWait::NoDevice {
      return Ok(PendingOutcome::Later(self.user_addition(
        conversation,
        user,
        history.clone(),
        UserAddWait::NoDevice,
        0,
      )));
    }
    Ok(PendingOutcome::Done)
  }

  fn queue_user_addition(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    history: HistoryPolicy,
    wait: UserAddWait,
    attempts: u32,
  ) -> Result<(), CoreError> {
    tracing::info!(conversation_id = %conversation, ?wait, "the added user waits for a device");
    let change = self.user_addition(conversation, user, history, wait, attempts);
    self.queue_pending(change)
  }

  fn user_addition(
    &self,
    conversation: ConversationId,
    user: UserId,
    history: HistoryPolicy,
    wait: UserAddWait,
    attempts: u32,
  ) -> PendingChange {
    PendingChange::AddUser {
      conversation_id: conversation,
      user_id: user,
      history,
      wait,
      maybe_not_before_ms: self.next_try_ms(wait, attempts),
      attempts,
    }
  }

  /// No device at all, or devices that can't be added (no key package, or none passes the
  /// checks: told apart by the commit). The device list is cheap; claiming to find out would waste
  /// the packages of a user who has some.
  async fn why_not_addable(&self, user: UserId) -> Result<UserAddWait, CoreError> {
    let devices = self.api.list_user_devices(user).await?;
    Ok(if devices.devices.is_empty() {
      UserAddWait::NoDevice
    } else {
      UserAddWait::NoKeyPackage
    })
  }

  /// `None` while waiting for a device (`device_add` wakes it); for a missing key package the
  /// delay starts at `device_add_stagger` and doubles up to `rejoin_retry_max`.
  fn next_try_ms(&self, wait: UserAddWait, attempts: u32) -> Option<i64> {
    match wait {
      UserAddWait::NoDevice | UserAddWait::Unverifiable => None,
      UserAddWait::NoKeyPackage => {
        let base_ms = i64::try_from(self.settings.device_add_stagger.as_millis()).unwrap_or(i64::MAX);
        let max_ms = i64::try_from(self.settings.rejoin_retry_max.as_millis()).unwrap_or(i64::MAX);
        let delay_ms = base_ms
          .saturating_mul(1_i64.checked_shl(attempts.min(30)).unwrap_or(i64::MAX))
          .min(max_ms);
        Some(self.now_ms().saturating_add(delay_ms))
      },
    }
  }
}

/// Users of the conversation this device waits to add to the encryption group, and why (the
/// conversation's banner).
pub fn pending_user_additions(
  store: &impl LocalStore,
  conversation: ConversationId,
) -> Result<Vec<(UserId, UserAddWait)>, CoreError> {
  let mut waiting = Vec::new();
  for change in records::list_json::<PendingChange>(store, Table::PendingChanges)? {
    if let PendingChange::AddUser {
      conversation_id,
      user_id,
      wait,
      ..
    } = change
      && conversation_id == conversation
    {
      waiting.push((user_id, wait));
    }
  }
  Ok(waiting)
}

fn history_policy(history: HistoryShare) -> HistoryPolicy {
  match history {
    HistoryShare::FromJoin => HistoryPolicy::FromJoin,
    HistoryShare::Full => HistoryPolicy::Full,
  }
}

/// Anything but an explicit "full" shares only what is sent from the join on.
fn history_share(history: &HistoryPolicy) -> HistoryShare {
  match history {
    HistoryPolicy::Full => HistoryShare::Full,
    _ => HistoryShare::FromJoin,
  }
}
