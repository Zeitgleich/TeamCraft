//! `/remind`: reminders kept on this device (local storage) and shown while the app is open.

use serde::{Deserialize, Serialize};

use crate::model::TimestampMs;
use crate::slash::ReminderTime;

const MS_PER_MINUTE: i64 = 60_000;
const MS_PER_DAY: i64 = 24 * 60 * MS_PER_MINUTE;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reminder {
  pub due_at: TimestampMs,
  pub text: String,
}

/// When a reminder fires. `start_of_today` is local midnight, so `At` means the next such time.
pub fn due_time(when: ReminderTime, now: TimestampMs, start_of_today: TimestampMs) -> TimestampMs {
  match when {
    ReminderTime::In { minutes } => now.saturating_add(i64::from(minutes) * MS_PER_MINUTE),
    ReminderTime::At { minute_of_day } => {
      let today = start_of_today + i64::from(minute_of_day) * MS_PER_MINUTE;
      if today > now { today } else { today + MS_PER_DAY }
    },
  }
}

/// Splits the list into reminders that are due at `now` and the ones still waiting.
pub fn take_due(all: Vec<Reminder>, now: TimestampMs) -> (Vec<Reminder>, Vec<Reminder>) {
  all.into_iter().partition(|reminder| reminder.due_at <= now)
}

pub fn to_json(list: &[Reminder]) -> String {
  serde_json::to_string(list).unwrap_or_else(|_| "[]".to_owned())
}

pub fn from_json(text: &str) -> Vec<Reminder> {
  serde_json::from_str(text).unwrap_or_default()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn relative_and_clock_times_resolve_to_the_next_occurrence() {
    let midnight = 1_000 * MS_PER_DAY;
    let now = midnight + 15 * 60 * MS_PER_MINUTE;
    assert_eq!(
      due_time(ReminderTime::In { minutes: 10 }, now, midnight),
      now + 10 * MS_PER_MINUTE
    );
    assert_eq!(
      due_time(
        ReminderTime::At {
          minute_of_day: 16 * 60
        },
        now,
        midnight
      ),
      midnight + 16 * 60 * MS_PER_MINUTE
    );
    assert_eq!(
      due_time(
        ReminderTime::At {
          minute_of_day: 9 * 60
        },
        now,
        midnight
      ),
      midnight + MS_PER_DAY + 9 * 60 * MS_PER_MINUTE
    );
  }

  #[test]
  fn due_reminders_are_taken_and_the_rest_waits() {
    let list = vec![
      Reminder {
        due_at: 10,
        text: "a".into(),
      },
      Reminder {
        due_at: 99,
        text: "b".into(),
      },
    ];
    let (due, waiting) = take_due(list.clone(), 50);
    assert_eq!(due, vec![list[0].clone()]);
    assert_eq!(waiting, vec![list[1].clone()]);
    assert_eq!(from_json(&to_json(&list)), list);
    assert!(from_json("nonsense").is_empty());
  }
}
