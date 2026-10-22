use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{ConversationId, MessageId, NotificationLevel, UnixMillis, UserId};

/// Minutes in a day; window bounds are minutes since local midnight.
pub const MINUTES_PER_DAY: u16 = 24 * 60;

const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

wire_enum! {
  /// When the server pushes to a user's devices while one of them is active.
  pub enum PushMode {
    /// No push while a device of the user is active (they see the message there).
    AwayOnly => "away_only",
    /// Push to devices without a live connection even if another device is active.
    Always => "always",
  }
}

wire_enum! {
  pub enum Weekday {
    Monday => "mon",
    Tuesday => "tue",
    Wednesday => "wed",
    Thursday => "thu",
    Friday => "fri",
    Saturday => "sat",
    Sunday => "sun",
  }
}

impl Weekday {
  fn from_monday_based_index(index: i64) -> Self {
    match index.rem_euclid(7) {
      0 => Self::Monday,
      1 => Self::Tuesday,
      2 => Self::Wednesday,
      3 => Self::Thursday,
      4 => Self::Friday,
      5 => Self::Saturday,
      _ => Self::Sunday,
    }
  }
}

/// A recurring do-not-disturb window in the user's local time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DndWindow {
  /// Days on which the window starts.
  pub weekdays: Vec<Weekday>,
  /// Minutes since local midnight, `0..1440`.
  pub start_minute: u16,
  /// Minutes since local midnight. An end before the start means the window runs past midnight
  /// into the next day.
  pub end_minute: u16,
}

/// Do-not-disturb schedule (PLAN.md §5). The server evaluates it to decide on pushes; clients
/// show it. Times are local; `utc_offset_minutes` is the user's current offset, which the client
/// updates when daylight saving time changes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DndSchedule {
  pub utc_offset_minutes: i16,
  pub windows: Vec<DndWindow>,
  /// People whose messages still notify during DND (for example a manager).
  #[serde(default)]
  pub allow_user_ids: Vec<UserId>,
}

impl DndSchedule {
  /// Whether a message from `sender` at `unix_seconds` is silenced.
  pub fn silences(&self, unix_seconds: i64, sender: UserId) -> bool {
    !self.allow_user_ids.contains(&sender) && self.is_active_at(unix_seconds)
  }

  /// Whether `unix_seconds` falls into one of the windows.
  pub fn is_active_at(&self, unix_seconds: i64) -> bool {
    let local = unix_seconds + i64::from(self.utc_offset_minutes) * 60;
    let day = local.div_euclid(SECONDS_PER_DAY);
    let minute = u16::try_from(local.rem_euclid(SECONDS_PER_DAY) / 60).unwrap_or(0);
    // 1970-01-01 was a Thursday; Monday is index 0.
    let today = Weekday::from_monday_based_index(day + 3);
    let yesterday = Weekday::from_monday_based_index(day + 2);
    self.windows.iter().any(|window| {
      let (start, end) = (window.start_minute, window.end_minute);
      if start < end {
        window.weekdays.contains(&today) && (start..end).contains(&minute)
      } else {
        (window.weekdays.contains(&today) && minute >= start)
          || (window.weekdays.contains(&yesterday) && minute < end)
      }
    })
  }
}

/// `GET`/`PUT /api/v1/me/notification-settings`: the caller's notification preferences. `PUT`
/// replaces all of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct NotificationSettings {
  /// Follow threads in which the user is mentioned.
  pub auto_follow_mentioned_threads: bool,
  pub push_mode: PushMode,
  /// Tell others in DMs and group DMs which messages were read. Reciprocal: whoever turns it off
  /// does not see the others' receipts either.
  pub send_read_receipts: bool,
  #[serde(rename = "dnd_schedule", default, skip_serializing_if = "Option::is_none")]
  pub maybe_dnd_schedule: Option<DndSchedule>,
}

/// `GET`/`PUT /api/v1/conversations/{id}/notification-settings`; `PUT` replaces both fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ConversationNotificationSettings {
  pub level: NotificationLevel,
  /// Muted until this time; past values mean not muted.
  #[serde(rename = "muted_until", default, skip_serializing_if = "Option::is_none")]
  pub maybe_muted_until: Option<UnixMillis>,
}

/// One member's read position in a DM or group DM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReadReceipt {
  pub user_id: UserId,
  /// The newest message the member has read.
  pub message_id: MessageId,
}

/// `GET /api/v1/conversations/{id}/read-receipts`: who has read up to where. Receipts exist only in
/// DMs and group DMs and are reciprocal: the list is empty for a caller who turned receipts off,
/// and members who turned them off are not listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReadReceiptsResponse {
  pub conversation_id: ConversationId,
  pub receipts: Vec<ReadReceipt>,
}

/// Somebody acknowledged an urgent message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MessageAcknowledgement {
  pub user_id: UserId,
  pub acknowledged_at: UnixMillis,
}

/// `GET /api/v1/conversations/{id}/messages/{message_id}/acknowledgements`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListAcknowledgementsResponse {
  pub message_id: MessageId,
  pub acknowledgements: Vec<MessageAcknowledgement>,
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{Id, Snowflake};

  // 2026-01-05 (Monday) 00:00:00 UTC.
  const MONDAY_MIDNIGHT: i64 = 1_767_571_200;
  const HOUR: i64 = 3600;

  fn night_shift(weekdays: Vec<Weekday>) -> DndSchedule {
    DndSchedule {
      utc_offset_minutes: 0,
      windows: vec![DndWindow {
        weekdays,
        start_minute: 22 * 60,
        end_minute: 7 * 60,
      }],
      allow_user_ids: Vec::new(),
    }
  }

  #[test]
  fn windows_past_midnight_continue_into_the_next_day() {
    let schedule = night_shift(vec![Weekday::Monday]);
    assert!(schedule.is_active_at(MONDAY_MIDNIGHT + 23 * HOUR));
    assert!(schedule.is_active_at(MONDAY_MIDNIGHT + 24 * HOUR + 6 * HOUR));
    assert!(!schedule.is_active_at(MONDAY_MIDNIGHT + 24 * HOUR + 7 * HOUR));
    // Monday's morning belongs to Sunday's window.
    assert!(!schedule.is_active_at(MONDAY_MIDNIGHT + 6 * HOUR));
    assert!(!schedule.is_active_at(MONDAY_MIDNIGHT + 2 * 24 * HOUR + 23 * HOUR));
  }

  #[test]
  fn daytime_windows_end_exclusive() {
    let schedule = DndSchedule {
      utc_offset_minutes: 0,
      windows: vec![DndWindow {
        weekdays: vec![Weekday::Monday],
        start_minute: 9 * 60,
        end_minute: 10 * 60,
      }],
      allow_user_ids: Vec::new(),
    };
    assert!(schedule.is_active_at(MONDAY_MIDNIGHT + 9 * HOUR));
    assert!(!schedule.is_active_at(MONDAY_MIDNIGHT + 10 * HOUR));
    assert!(!schedule.is_active_at(MONDAY_MIDNIGHT + 8 * HOUR));
  }

  #[test]
  fn the_offset_moves_the_window_to_local_time() {
    let mut schedule = night_shift(vec![Weekday::Monday]);
    schedule.utc_offset_minutes = 120;
    // 21:00 UTC is 23:00 local.
    assert!(schedule.is_active_at(MONDAY_MIDNIGHT + 21 * HOUR));
    assert!(!schedule.is_active_at(MONDAY_MIDNIGHT + 19 * HOUR));
    schedule.utc_offset_minutes = -300;
    // 03:00 UTC Tuesday is 22:00 local Monday.
    assert!(schedule.is_active_at(MONDAY_MIDNIGHT + 24 * HOUR + 3 * HOUR));
  }

  #[test]
  fn allowed_senders_still_notify() {
    let manager = UserId::from_snowflake(Snowflake::new(7).unwrap());
    let other = UserId::from_snowflake(Snowflake::new(8).unwrap());
    let mut schedule = night_shift(vec![Weekday::Monday]);
    schedule.allow_user_ids = vec![manager];
    let night = MONDAY_MIDNIGHT + 23 * HOUR;
    assert!(!schedule.silences(night, manager));
    assert!(schedule.silences(night, other));
  }

  #[test]
  fn settings_wire_format_is_stable() {
    let settings = NotificationSettings {
      auto_follow_mentioned_threads: true,
      push_mode: PushMode::AwayOnly,
      send_read_receipts: false,
      maybe_dnd_schedule: None,
    };
    assert_eq!(
      serde_json::to_string(&settings).unwrap(),
      r#"{"auto_follow_mentioned_threads":true,"push_mode":"away_only","send_read_receipts":false}"#
    );
    let unknown: PushMode = serde_json::from_str("\"sometimes\"").unwrap();
    assert_eq!(unknown, PushMode::Unknown("sometimes".into()));
  }
}
