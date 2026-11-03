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

/// People checked again per full sync at most ([`Client::recheck_waiting_additions`]).
const MAX_WAITING_CHECKS: usize = 50;

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
  /// The user had no device for `pending_addition_expiry`: the wait is over and the conversation
  /// says they never set up the app. Their first device, should it ever appear, still gets them
  /// in (`device_add` makes the addition due again).
  NeverSetUp,
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

  /// After a full sync (at start, and when the gateway's resume window had expired): asks the
  /// server again whether people waiting for a device have one by now, since their `device_add`
  /// may have gone by while this device was offline. One device list per person, at most
  /// [`MAX_WAITING_CHECKS`] people, and not again within `rejoin_retry`. Returns how many woke.
  pub async fn recheck_waiting_additions(&mut self) -> Result<usize, CoreError> {
    let now = self.now_ms();
    let interval_ms = i64::try_from(self.settings.rejoin_retry.as_millis()).unwrap_or(i64::MAX);
    if self
      .maybe_waiting_checked_at_ms
      .is_some_and(|checked| now.saturating_sub(checked) < interval_ms)
    {
      return Ok(0);
    }
    let mut waiting: Vec<UserId> = records::list_json::<PendingChange>(&self.store, Table::PendingChanges)?
      .into_iter()
      .filter_map(|change| match change {
        PendingChange::AddUser {
          user_id,
          wait: UserAddWait::NoDevice | UserAddWait::Unverifiable | UserAddWait::NeverSetUp,
          maybe_not_before_ms: None,
          ..
        } => Some(user_id),
        _ => None,
      })
      .collect();
    waiting.sort();
    waiting.dedup();
    waiting.truncate(MAX_WAITING_CHECKS);
    let mut woken = 0;
    for user in waiting {
      if !self.api.list_user_devices(user).await?.devices.is_empty() {
        self.wake_user_additions(user)?;
        woken += 1;
      }
    }
    // Only after all answered: a sync cut short checks again next time.
    self.maybe_waiting_checked_at_ms = Some(now);
    Ok(woken)
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
        maybe_waiting_since_ms,
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
          maybe_waiting_since_ms,
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
      wait: previous_wait,
      maybe_waiting_since_ms,
    } = change
    else {
      return Ok(PendingOutcome::Done);
    };
    let (conversation, user) = (*conversation_id, *user_id);
    let since_ms = maybe_waiting_since_ms.unwrap_or_else(|| self.now_ms());
    if *previous_wait == UserAddWait::NoDevice && self.is_wait_expired(since_ms) {
      tracing::info!(conversation_id = %conversation, "the added user never set up the app; stopped waiting");
      self.forget_waiting_dm_setup(conversation)?;
      return Ok(PendingOutcome::Later(self.user_addition(
        conversation,
        user,
        history.clone(),
        UserAddWait::NeverSetUp,
        0,
        Some(since_ms),
      )));
    }
    let is_due = maybe_not_before_ms.is_some_and(|not_before| self.now_ms() >= not_before);
    if !is_due {
      // Stamped, so a record from before the expiry existed starts its wait now.
      let mut waiting = change.clone();
      if let PendingChange::AddUser {
        maybe_waiting_since_ms,
        ..
      } = &mut waiting
      {
        maybe_waiting_since_ms.get_or_insert(since_ms);
      }
      return Ok(PendingOutcome::Later(waiting));
    }
    if !self.has_group(conversation)? {
      return Ok(PendingOutcome::Done);
    }
    if let Some(others) = self.resume_dm_setup(conversation)? {
      return self
        .resume_dm_addition(conversation, user, history, &others, Some(since_ms))
        .await;
    }
    self.sync_mls(conversation).await?;
    if self.group_users(conversation)?.contains(&user) {
      let epoch = self.group_or_error(conversation)?.epoch();
      if *history == HistoryPolicy::Full
        && self.effective_history(user, HistoryShare::Full).await? == HistoryShare::Full
      {
        // Somebody else let the person's first device in, maybe from the join on (a member
        // committed its signed self-add): this device decided to share the history with them,
        // so it seals the history to their devices like a new member's (e2ee.md §4.10).
        self.send_history(conversation, user, epoch).await?;
        tracing::info!(conversation_id = %conversation, "sealed the history to a waiting user who joined meanwhile");
      } else if self.is_import_granted(conversation, user)? {
        // A guest the import grant names gets the import's keys (e2ee.md §4.10).
        self.send_import_history(conversation, user, epoch).await?;
      }
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
      Some(since_ms),
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
    since_ms: Option<i64>,
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
        since_ms,
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
    let since_ms = Some(self.now_ms());
    let change = self.user_addition(conversation, user, history, wait, attempts, since_ms);
    self.queue_pending(change)
  }

  fn user_addition(
    &self,
    conversation: ConversationId,
    user: UserId,
    history: HistoryPolicy,
    wait: UserAddWait,
    attempts: u32,
    maybe_waiting_since_ms: Option<i64>,
  ) -> PendingChange {
    PendingChange::AddUser {
      conversation_id: conversation,
      user_id: user,
      history,
      wait,
      maybe_not_before_ms: self.next_try_ms(wait, attempts),
      attempts,
      maybe_waiting_since_ms,
    }
  }

  /// Whether a wait for a first device that began at `since_ms` is over.
  fn is_wait_expired(&self, since_ms: i64) -> bool {
    let expiry_ms = i64::try_from(self.settings.pending_addition_expiry.as_millis()).unwrap_or(i64::MAX);
    self.now_ms().saturating_sub(since_ms) >= expiry_ms
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
      UserAddWait::NoDevice | UserAddWait::Unverifiable | UserAddWait::NeverSetUp => None,
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
