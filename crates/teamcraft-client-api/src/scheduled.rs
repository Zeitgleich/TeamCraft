//! "Send later" as the clients see it (docs/scheduled-messages.md): a message waits in the user's
//! encrypted account data until one of their devices is online at or after its time, and that device
//! sends it like any other message. The rules every client shows and the core applies live here.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{ClientMessageId, ScheduledMessagePolicy};

use crate::actions::MessagePriority;
use crate::model::{ConversationId, MessageId, ThreadId, TimestampMs};

/// Messages one user can have waiting. They travel in the key bag, which is uploaded as a whole.
pub const MAX_SCHEDULED_MESSAGES: usize = 50;
/// The longest text of a scheduled message: with [`MAX_SCHEDULED_MESSAGES`] they stay well below the
/// key bag's default limit of 1 MiB, also base64-encoded and encrypted.
pub const MAX_SCHEDULED_BODY_BYTES: usize = 8 * 1024;

const MS_PER_HOUR: i64 = 3_600_000;
const MS_PER_DAY: i64 = 24 * MS_PER_HOUR;

/// A message waiting for its time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledMessage {
  /// Also the ID the message goes out with.
  pub id: ClientMessageId,
  pub conversation: ConversationId,
  /// Empty when this device doesn't know the conversation ([`ScheduledStatus::Unavailable`]).
  #[serde(default)]
  pub conversation_name: String,
  #[serde(default)]
  pub maybe_thread: Option<ThreadId>,
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

/// Where a scheduled message stands. Each one has its own line in every client: nothing waits
/// silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledStatus {
  /// Its time hasn't come.
  Waiting,
  /// Its time has come: it goes out as soon as one of the user's devices has a connection.
  Due,
  /// None of the user's devices was online for longer than the workspace's late limit after its
  /// time. It may be stale by now, so it doesn't go out by itself: send it, change it or cancel it.
  Missed,
  /// The workspace switched scheduling off after it was written. Send it or cancel it.
  PolicyOff,
  /// This device doesn't know its conversation (the user left it, or it isn't loaded here yet).
  Unavailable,
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
  BodyTooLong,
  /// The time is not in the future.
  NotInFuture,
  /// The time is further ahead than the workspace allows.
  BeyondHorizon,
  /// [`MAX_SCHEDULED_MESSAGES`] are waiting already.
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
    if self.is_new && self.waiting >= MAX_SCHEDULED_MESSAGES {
      found.push(ScheduleRefusal::TooMany);
    }
    if self.body.trim().is_empty() {
      found.push(ScheduleRefusal::EmptyBody);
    } else if self.body.len() > MAX_SCHEDULED_BODY_BYTES {
      found.push(ScheduleRefusal::BodyTooLong);
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

/// Where a message with this time stands. `is_known` says whether this device knows its conversation.
pub fn status_of(
  due_at: TimestampMs,
  server_now: TimestampMs,
  policy: &ScheduledMessagePolicy,
  is_known: bool,
) -> ScheduledStatus {
  if !is_known {
    return ScheduledStatus::Unavailable;
  }
  if !policy.is_enabled {
    return ScheduledStatus::PolicyOff;
  }
  if due_at > server_now {
    return ScheduledStatus::Waiting;
  }
  let late_limit_ms = i64::from(policy.late_limit_hours).saturating_mul(MS_PER_HOUR);
  if server_now.saturating_sub(due_at) > late_limit_ms {
    ScheduledStatus::Missed
  } else {
    ScheduledStatus::Due
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const NOW: TimestampMs = 1_800_000_000_000;

  mod status {
    use super::*;

    #[test]
    fn follows_the_time() {
      let policy = policy();
      assert_eq!(status_of(NOW + 1, NOW, &policy, true), ScheduledStatus::Waiting);
      assert_eq!(status_of(NOW, NOW, &policy, true), ScheduledStatus::Due);
      assert_eq!(
        status_of(NOW - 24 * MS_PER_HOUR, NOW, &policy, true),
        ScheduledStatus::Due
      );
      assert_eq!(
        status_of(NOW - 24 * MS_PER_HOUR - 1, NOW, &policy, true),
        ScheduledStatus::Missed
      );
    }

    #[test]
    fn a_policy_that_went_off_holds_it_back_whatever_the_time() {
      let policy = ScheduledMessagePolicy {
        is_enabled: false,
        ..policy()
      };
      assert_eq!(status_of(NOW - 1, NOW, &policy, true), ScheduledStatus::PolicyOff);
      assert_eq!(status_of(NOW + 1, NOW, &policy, true), ScheduledStatus::PolicyOff);
    }

    #[test]
    fn an_unknown_conversation_says_so_first() {
      let policy = policy();
      assert_eq!(
        status_of(NOW - 1, NOW, &policy, false),
        ScheduledStatus::Unavailable
      );
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
    fn the_text_has_a_limit() {
      let long = "x".repeat(MAX_SCHEDULED_BODY_BYTES + 1);
      assert_eq!(
        check(&long, NOW + 1).refusals(&policy()),
        [ScheduleRefusal::BodyTooLong]
      );
    }

    #[test]
    fn only_a_new_one_counts_against_the_cap() {
      let full = ScheduleCheck {
        waiting: MAX_SCHEDULED_MESSAGES,
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
