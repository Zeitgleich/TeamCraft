//! "Send later" as the clients see it (docs/scheduled-messages.md): the server holds a message sealed
//! by the sender's device and releases it at its time, on time unless the group changed and all the
//! sender's devices stayed off. The server enforces the rules; these are the same rules for showing
//! why before asking it, and what a held message's state means.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{ScheduledHoldReason, ScheduledMessageId, ScheduledMessagePolicy};

use crate::actions::MessagePriority;
use crate::model::{ConversationId, MessageId, ThreadId, TimestampMs};

const MS_PER_DAY: i64 = 24 * 3_600_000;

/// A message waiting for its time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledMessage {
  pub id: ScheduledMessageId,
  pub conversation: ConversationId,
  /// Empty when this device doesn't know the conversation.
  #[serde(default)]
  pub conversation_name: String,
  #[serde(default)]
  pub maybe_thread: Option<ThreadId>,
  /// Empty when this device can't read it yet ([`ScheduledStatus::Unreadable`]).
  pub body: String,
  /// Server time.
  pub due_at: TimestampMs,
  pub status: ScheduledStatus,
  #[serde(default)]
  pub maybe_reply_to: Option<MessageId>,
  #[serde(default)]
  pub priority: MessagePriority,
  #[serde(default)]
  pub wants_acknowledgement: bool,
  #[serde(default)]
  pub is_silent: bool,
  #[serde(default)]
  pub also_send_to_channel: bool,
}

/// Where a held message stands. Each one has its own line in every client: nothing waits silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledStatus {
  /// The server sends it at its time, whether the user's devices are on or not.
  Waiting,
  /// The group changed since it was sealed and its time hasn't come. A device of the user seals it
  /// again as soon as one is online; if none is by then, it goes out when one next is.
  NeedsSealing,
  /// Its time has come but the group changed: it goes out when a device of the user is next online.
  Stale,
  /// The workspace turned scheduling off. Send it or cancel it.
  PolicyOff,
  /// The user is no longer in the conversation, or their account is off: it won't go out.
  NotAMember,
  /// The user may no longer post there: it won't go out.
  NoPermission,
  /// The conversation was deleted.
  ConversationGone,
  /// The thread it answers is gone, or threads were turned off.
  ThreadGone,
  /// This device holds no key for it yet, and its epoch is current: the server sends it at its time.
  Unreadable,
  /// From a newer version.
  #[serde(other)]
  Unknown,
}

/// What the "Send later" screens need: the list, the rule they apply, and the time they compute
/// from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledOverview {
  /// Soonest first.
  pub messages: Vec<ScheduledMessage>,
  pub policy: ScheduledMessagePolicy,
  /// The server's clock as far as this device knows it, so "in 1 hour" means an hour from now also
  /// on a device whose clock is wrong.
  pub server_now: TimestampMs,
}

impl Default for ScheduledOverview {
  fn default() -> Self {
    Self {
      messages: Vec::new(),
      // A backend that can't keep them offers nothing.
      policy: ScheduledMessagePolicy {
        is_enabled: false,
        ..ScheduledMessagePolicy::default()
      },
      server_now: 0,
    }
  }
}

/// Why a message can't be scheduled like this.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleRefusal {
  /// The workspace doesn't allow scheduling.
  Disabled,
  EmptyBody,
  /// The time is not in the future.
  NotInFuture,
  /// The time is further ahead than the workspace allows.
  BeyondHorizon,
  /// The user has as many waiting as the workspace allows.
  TooMany,
}

/// A change to a waiting message: its text and its time. `is_new` also counts it against the cap.
#[derive(Clone, Copy, Debug)]
pub struct ScheduleCheck<'a> {
  pub body: &'a str,
  pub due_at: TimestampMs,
  pub server_now: TimestampMs,
  pub waiting: usize,
  pub is_new: bool,
}

impl ScheduleCheck<'_> {
  /// Every refusal, in the order of the form's fields; empty means it can be scheduled.
  pub fn refusals(&self, policy: &ScheduledMessagePolicy) -> Vec<ScheduleRefusal> {
    let mut found = Vec::new();
    if !policy.is_enabled {
      found.push(ScheduleRefusal::Disabled);
    }
    if self.is_new && self.waiting >= policy.max_per_user as usize {
      found.push(ScheduleRefusal::TooMany);
    }
    if self.body.trim().is_empty() {
      found.push(ScheduleRefusal::EmptyBody);
    }
    if self.due_at <= self.server_now {
      found.push(ScheduleRefusal::NotInFuture);
    } else if self.due_at > latest_due(policy, self.server_now) {
      found.push(ScheduleRefusal::BeyondHorizon);
    }
    found
  }
}

/// The latest time a message may be scheduled for right now.
pub fn latest_due(policy: &ScheduledMessagePolicy, server_now: TimestampMs) -> TimestampMs {
  server_now.saturating_add(i64::from(policy.max_horizon_days).saturating_mul(MS_PER_DAY))
}

/// Where a held message stands, from what the server says and whether this device can read it.
pub fn status_of(
  maybe_hold: Option<&ScheduledHoldReason>,
  is_epoch_current: bool,
  is_readable: bool,
) -> ScheduledStatus {
  match maybe_hold {
    Some(ScheduledHoldReason::StaleEpoch) => ScheduledStatus::Stale,
    Some(ScheduledHoldReason::PolicyOff) => ScheduledStatus::PolicyOff,
    Some(ScheduledHoldReason::NotAMember | ScheduledHoldReason::SenderInactive) => {
      ScheduledStatus::NotAMember
    },
    Some(ScheduledHoldReason::NoPermission) => ScheduledStatus::NoPermission,
    Some(ScheduledHoldReason::ConversationGone) => ScheduledStatus::ConversationGone,
    Some(ScheduledHoldReason::ThreadGone) => ScheduledStatus::ThreadGone,
    Some(ScheduledHoldReason::Unknown(_)) => ScheduledStatus::Unknown,
    // Before "can't read": a message under an old epoch needs a device of the user whatever this one
    // can read, so "it still goes out" would be wrong.
    None if !is_epoch_current => ScheduledStatus::NeedsSealing,
    None if !is_readable => ScheduledStatus::Unreadable,
    None => ScheduledStatus::Waiting,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const NOW: TimestampMs = 1_800_000_000_000;

  mod status {
    use super::*;

    #[test]
    fn waiting_needs_a_current_epoch_and_a_key() {
      assert_eq!(status_of(None, true, true), ScheduledStatus::Waiting);
      assert_eq!(status_of(None, false, true), ScheduledStatus::NeedsSealing);
      assert_eq!(status_of(None, true, false), ScheduledStatus::Unreadable);
      assert_eq!(
        status_of(None, false, false),
        ScheduledStatus::NeedsSealing,
        "a stale one without the key doesn't claim to go out"
      );
    }

    #[test]
    fn every_hold_has_its_status() {
      for (hold, status) in [
        (ScheduledHoldReason::StaleEpoch, ScheduledStatus::Stale),
        (ScheduledHoldReason::PolicyOff, ScheduledStatus::PolicyOff),
        (ScheduledHoldReason::NotAMember, ScheduledStatus::NotAMember),
        (ScheduledHoldReason::SenderInactive, ScheduledStatus::NotAMember),
        (ScheduledHoldReason::NoPermission, ScheduledStatus::NoPermission),
        (
          ScheduledHoldReason::ConversationGone,
          ScheduledStatus::ConversationGone,
        ),
        (ScheduledHoldReason::ThreadGone, ScheduledStatus::ThreadGone),
      ] {
        assert_eq!(status_of(Some(&hold), false, true), status);
      }
    }

    #[test]
    fn a_status_of_a_newer_version_is_read_as_unknown() {
      let status: ScheduledStatus = serde_json::from_str("\"on_the_moon\"").unwrap();
      assert_eq!(status, ScheduledStatus::Unknown);
    }
  }

  mod refusals {
    use super::*;

    #[test]
    fn a_good_one_passes() {
      assert!(check("Good morning", NOW + 1).refusals(&policy()).is_empty());
    }

    #[test]
    fn the_time_must_lie_ahead() {
      assert_eq!(
        check("text", NOW).refusals(&policy()),
        [ScheduleRefusal::NotInFuture]
      );
    }

    #[test]
    fn the_horizon_ends_exactly_at_its_day() {
      let latest = latest_due(&policy(), NOW);
      assert!(check("text", latest).refusals(&policy()).is_empty());
      assert_eq!(
        check("text", latest + 1).refusals(&policy()),
        [ScheduleRefusal::BeyondHorizon]
      );
    }

    #[test]
    fn every_problem_is_named_at_once() {
      let policy = ScheduledMessagePolicy {
        is_enabled: false,
        ..policy()
      };
      assert_eq!(
        check("  ", NOW).refusals(&policy),
        [
          ScheduleRefusal::Disabled,
          ScheduleRefusal::EmptyBody,
          ScheduleRefusal::NotInFuture
        ]
      );
    }

    #[test]
    fn only_a_new_one_counts_against_the_cap() {
      let full = ScheduleCheck {
        waiting: policy().max_per_user as usize,
        ..check("text", NOW + 1)
      };
      assert_eq!(full.refusals(&policy()), [ScheduleRefusal::TooMany]);
      let edit = ScheduleCheck {
        is_new: false,
        ..full
      };
      assert!(edit.refusals(&policy()).is_empty());
    }

    fn check(body: &str, due_at: TimestampMs) -> ScheduleCheck<'_> {
      ScheduleCheck {
        body,
        due_at,
        server_now: NOW,
        waiting: 0,
        is_new: true,
      }
    }
  }

  fn policy() -> ScheduledMessagePolicy {
    ScheduledMessagePolicy::default()
  }
}
