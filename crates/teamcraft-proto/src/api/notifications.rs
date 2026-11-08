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

/// Do-not-disturb schedule and overrides (PLAN.md §5). The server evaluates it to decide on pushes;
/// clients show it and check it again before they show a notification. Times are local to the user's
/// time zone: evaluators with a time zone database use `time_zone` at the moment in question, others
/// `utc_offset_minutes`, which clients keep current. The overrides apply to manual DND as well.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DndSchedule {
  pub utc_offset_minutes: i16,
  /// IANA time zone name, for example `Europe/Berlin`.
  #[serde(rename = "time_zone", default, skip_serializing_if = "Option::is_none")]
  pub maybe_time_zone: Option<String>,
  pub windows: Vec<DndWindow>,
  /// People whose messages still notify during DND (for example a manager).
  #[serde(default)]
  pub allow_user_ids: Vec<UserId>,
  /// Messages the sender marked urgent still notify during DND (where `urgent_scope` says).
  #[serde(default)]
  pub allows_urgent: bool,
  /// Where an urgent message may break through DND: only in DMs and group DMs unless the user
  /// widens it, so anybody in a big channel can't wake them up.
  #[serde(default)]
  pub urgent_scope: UrgentScope,
}

wire_enum! {
  /// Where urgent messages break through DND (with `allows_urgent`).
  #[derive(Default)]
  pub enum UrgentScope {
    /// DMs and group DMs only (the default).
    #[default]
    Direct => "direct",
    /// Every conversation, channels included.
    Everywhere => "everywhere",
  }
}

/// What DND's overrides need to know about one message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DndCandidate {
  pub sender: UserId,
  pub is_urgent: bool,
  /// A DM or group DM.
  pub is_direct: bool,
}

impl DndSchedule {
  /// Whether DND, scheduled or manual, lets this message through anyway: an allowed sender, or an
  /// urgent message where the user lets urgent ones through.
  pub fn lets_through(&self, message: DndCandidate) -> bool {
    let is_urgent_allowed = self.allows_urgent
      && message.is_urgent
      && (message.is_direct || self.urgent_scope == UrgentScope::Everywhere);
    self.allow_user_ids.contains(&message.sender) || is_urgent_allowed
  }

  /// Whether the windows silence the message at `unix_seconds`, given the user's UTC offset at that
  /// moment.
  pub fn silences(&self, unix_seconds: i64, offset_minutes: i16, message: DndCandidate) -> bool {
    !self.lets_through(message) && self.is_active_at(unix_seconds, offset_minutes)
  }

  /// Whether `unix_seconds` falls into one of the windows, given the user's UTC offset at that moment.
  pub fn is_active_at(&self, unix_seconds: i64, offset_minutes: i16) -> bool {
    windows_active_at(&self.windows, unix_seconds, offset_minutes)
  }
}

/// Whether `unix_seconds` falls into one of `windows`, given the UTC offset at that moment. A window
/// whose end is before its start runs past midnight and belongs to the day it starts on.
pub fn windows_active_at(windows: &[DndWindow], unix_seconds: i64, offset_minutes: i16) -> bool {
  let local = unix_seconds + i64::from(offset_minutes) * 60;
  let day = local.div_euclid(SECONDS_PER_DAY);
  let minute = u16::try_from(local.rem_euclid(SECONDS_PER_DAY) / 60).unwrap_or(0);
  // 1970-01-01 was a Thursday; Monday is index 0.
  let today = Weekday::from_monday_based_index(day + 3);
  let yesterday = Weekday::from_monday_based_index(day + 2);
  windows.iter().any(|window| {
    let (start, end) = (window.start_minute, window.end_minute);
    if start < end {
      window.weekdays.contains(&today) && (start..end).contains(&minute)
    } else {
      (window.weekdays.contains(&today) && minute >= start)
        || (window.weekdays.contains(&yesterday) && minute < end)
    }
  })
}

wire_enum! {
  /// Whether `@channel`, `@here` and `@all` count as a mention of the member in one conversation
  /// (notifications.md, "Per-conversation filters").
  #[derive(Default)]
  pub enum ChannelMentions {
    /// The member's general choice: on the desktop the setting "@channel notifies me"; the push
    /// routing, which can't see that device setting, doesn't count them.
    #[default]
    Default => "default",
    /// They count as a mention here, also for phone pushes.
    Notify => "notify",
    /// They don't count as a mention here.
    Ignore => "ignore",
  }
}

impl ChannelMentions {
  /// Whether a message to the whole conversation counts as a mention, given the member's general
  /// choice. A value from a newer version follows the general choice.
  pub fn counts(&self, general: bool) -> bool {
    match self {
      Self::Notify => true,
      Self::Ignore => false,
      Self::Default | Self::Unknown(_) => general,
    }
  }

  pub fn is_default(&self) -> bool {
    *self == Self::Default
  }
}

/// Times at which one conversation stays quiet as if it were muted (notifications.md,
/// "Per-conversation filters"), in the member's local time like [`DndSchedule`]: evaluators with a
/// time zone database use `time_zone` at the moment in question, others `utc_offset_minutes`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct QuietHours {
  pub utc_offset_minutes: i16,
  /// IANA time zone name, for example `Europe/Berlin`.
  #[serde(rename = "time_zone", default, skip_serializing_if = "Option::is_none")]
  pub maybe_time_zone: Option<String>,
  pub windows: Vec<DndWindow>,
}

impl QuietHours {
  /// Whether the conversation is quiet at `unix_seconds`, given the member's UTC offset then.
  pub fn is_active_at(&self, unix_seconds: i64, offset_minutes: i16) -> bool {
    windows_active_at(&self.windows, unix_seconds, offset_minutes)
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
  /// Desktop and web sessions report whether the user looks at them, so phone pushes wait while
  /// they do (notifications.md, "Attention"). Reads: whether that is in effect (the user's choice
  /// and the instance's switch); clients that read `false` report nothing. Writes: absent keeps the
  /// stored choice.
  #[serde(
    rename = "uses_desktop_attention",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_uses_desktop_attention: Option<bool>,
  /// Which messages notify in conversations the user never configured and whose space has no level
  /// (issue #1011). Reads: the user's own choice; absent: they never chose, `default_level` applies.
  /// Writes: absent keeps the stored choice.
  #[serde(rename = "level", default, skip_serializing_if = "Option::is_none")]
  pub maybe_level: Option<NotificationLevel>,
  /// Read only: the instance default for people who never chose a level (`[notifications]
  /// default_level`). Servers from before the setting send nothing.
  #[serde(rename = "default_level", default, skip_serializing_if = "Option::is_none")]
  pub maybe_default_level: Option<NotificationLevel>,
}

/// `GET`/`PUT /api/v1/conversations/{id}/notification-settings`; `PUT` replaces level and mute time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ConversationNotificationSettings {
  pub level: NotificationLevel,
  /// Muted until this time; past values mean not muted.
  #[serde(rename = "muted_until", default, skip_serializing_if = "Option::is_none")]
  pub maybe_muted_until: Option<UnixMillis>,
  /// Reads: the conversation follows its space's setting. Writes: `true` drops the conversation's
  /// own setting (level and mute time are ignored) so it follows the space again. The filters
  /// below are the conversation's own either way.
  #[serde(default)]
  pub is_inherited: bool,
  /// Reads: whether `@channel` counts as a mention here, absent for `default`. Writes: absent keeps
  /// the stored choice (clients from before the field).
  #[serde(rename = "channel_mentions", default, skip_serializing_if = "Option::is_none")]
  pub maybe_channel_mentions: Option<ChannelMentions>,
  /// Reads: the quiet hours, if there are any. Writes: absent keeps them, quiet hours without
  /// windows remove them.
  #[serde(rename = "quiet_hours", default, skip_serializing_if = "Option::is_none")]
  pub maybe_quiet_hours: Option<QuietHours>,
}

/// `GET`/`PUT /api/v1/spaces/{id}/notification-settings`: the caller's setting for a whole space.
/// Its channels follow it unless the member set a channel's own level or mute time. `PUT` replaces
/// both fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SpaceNotificationSettings {
  /// `None`: no space-level choice; every channel keeps its own level.
  #[serde(rename = "level", default, skip_serializing_if = "Option::is_none")]
  pub maybe_level: Option<NotificationLevel>,
  /// Muted until this time; past values mean not muted.
  #[serde(rename = "muted_until", default, skip_serializing_if = "Option::is_none")]
  pub maybe_muted_until: Option<UnixMillis>,
}

/// What a member's conversation settings come to once the space level is taken into account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveNotifications {
  pub level: NotificationLevel,
  pub maybe_muted_until: Option<UnixMillis>,
}

/// Per field: a conversation the member configured themselves keeps its own settings; any other
/// follows the space level and mute time, if the member set them.
pub fn effective_notifications(
  own: &ConversationNotificationSettings,
  maybe_space: Option<&SpaceNotificationSettings>,
) -> EffectiveNotifications {
  let Some(space) = maybe_space.filter(|_| own.is_inherited) else {
    return EffectiveNotifications {
      level: own.level.clone(),
      maybe_muted_until: own.maybe_muted_until,
    };
  };
  EffectiveNotifications {
    level: space.maybe_level.clone().unwrap_or_else(|| own.level.clone()),
    maybe_muted_until: own.maybe_muted_until.max(space.maybe_muted_until),
  }
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
      ..DndSchedule::default()
    }
  }

  #[test]
  fn windows_past_midnight_continue_into_the_next_day() {
    let schedule = night_shift(vec![Weekday::Monday]);
    assert!(schedule.is_active_at(MONDAY_MIDNIGHT + 23 * HOUR, schedule.utc_offset_minutes));
    assert!(schedule.is_active_at(
      MONDAY_MIDNIGHT + 24 * HOUR + 6 * HOUR,
      schedule.utc_offset_minutes
    ));
    assert!(!schedule.is_active_at(
      MONDAY_MIDNIGHT + 24 * HOUR + 7 * HOUR,
      schedule.utc_offset_minutes
    ));
    // Monday's morning belongs to Sunday's window.
    assert!(!schedule.is_active_at(MONDAY_MIDNIGHT + 6 * HOUR, schedule.utc_offset_minutes));
    assert!(!schedule.is_active_at(
      MONDAY_MIDNIGHT + 2 * 24 * HOUR + 23 * HOUR,
      schedule.utc_offset_minutes
    ));
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
      ..DndSchedule::default()
    };
    assert!(schedule.is_active_at(MONDAY_MIDNIGHT + 9 * HOUR, schedule.utc_offset_minutes));
    assert!(!schedule.is_active_at(MONDAY_MIDNIGHT + 10 * HOUR, schedule.utc_offset_minutes));
    assert!(!schedule.is_active_at(MONDAY_MIDNIGHT + 8 * HOUR, schedule.utc_offset_minutes));
  }

  #[test]
  fn the_offset_moves_the_window_to_local_time() {
    let mut schedule = night_shift(vec![Weekday::Monday]);
    schedule.utc_offset_minutes = 120;
    // 21:00 UTC is 23:00 local.
    assert!(schedule.is_active_at(MONDAY_MIDNIGHT + 21 * HOUR, schedule.utc_offset_minutes));
    assert!(!schedule.is_active_at(MONDAY_MIDNIGHT + 19 * HOUR, schedule.utc_offset_minutes));
    schedule.utc_offset_minutes = -300;
    // 03:00 UTC Tuesday is 22:00 local Monday.
    assert!(schedule.is_active_at(
      MONDAY_MIDNIGHT + 24 * HOUR + 3 * HOUR,
      schedule.utc_offset_minutes
    ));
  }

  #[test]
  fn allowed_senders_still_notify() {
    let manager = UserId::from_snowflake(Snowflake::new(7).unwrap());
    let other = UserId::from_snowflake(Snowflake::new(8).unwrap());
    let mut schedule = night_shift(vec![Weekday::Monday]);
    schedule.allow_user_ids = vec![manager];
    let night = MONDAY_MIDNIGHT + 23 * HOUR;
    assert!(!schedule.silences(night, 0, candidate(manager, false, false)));
    assert!(schedule.silences(night, 0, candidate(other, false, false)));
  }

  #[test]
  fn urgent_messages_break_through_only_when_allowed() {
    let sender = UserId::from_snowflake(Snowflake::new(8).unwrap());
    let mut schedule = night_shift(vec![Weekday::Monday]);
    let night = MONDAY_MIDNIGHT + 23 * HOUR;
    assert!(schedule.silences(night, 0, candidate(sender, true, true)));
    schedule.allows_urgent = true;
    assert!(!schedule.silences(night, 0, candidate(sender, true, true)));
    assert!(schedule.silences(night, 0, candidate(sender, false, true)));
    // In a channel only when the user widened it.
    assert!(schedule.silences(night, 0, candidate(sender, true, false)));
    schedule.urgent_scope = UrgentScope::Everywhere;
    assert!(!schedule.silences(night, 0, candidate(sender, true, false)));
  }

  fn candidate(sender: UserId, is_urgent: bool, is_direct: bool) -> DndCandidate {
    DndCandidate {
      sender,
      is_urgent,
      is_direct,
    }
  }

  #[test]
  fn schedule_wire_format_keeps_old_fields_and_reads_old_values() {
    let old: DndSchedule = serde_json::from_str(r#"{"utc_offset_minutes":60,"windows":[]}"#).unwrap();
    assert_eq!(old.maybe_time_zone, None);
    assert!(!old.allows_urgent);
    let schedule = DndSchedule {
      utc_offset_minutes: 60,
      maybe_time_zone: Some("Europe/Berlin".into()),
      allows_urgent: true,
      ..DndSchedule::default()
    };
    assert_eq!(
      serde_json::to_string(&schedule).unwrap(),
      r#"{"utc_offset_minutes":60,"time_zone":"Europe/Berlin","windows":[],"allow_user_ids":[],"allows_urgent":true,"urgent_scope":"direct"}"#
    );
  }

  mod conversation_filters {
    use super::*;

    #[test]
    fn channel_mentions_follow_the_general_choice_unless_set() {
      assert!(ChannelMentions::Default.counts(true));
      assert!(!ChannelMentions::Default.counts(false));
      assert!(ChannelMentions::Notify.counts(false));
      assert!(!ChannelMentions::Ignore.counts(true));
      let future: ChannelMentions = serde_json::from_str("\"only_admins\"").unwrap();
      assert_eq!(future, ChannelMentions::Unknown("only_admins".into()));
      assert!(future.counts(true));
      assert!(!future.counts(false));
    }

    #[test]
    fn quiet_hours_cross_midnight_like_the_dnd_schedule() {
      let quiet = QuietHours {
        windows: night_shift(vec![Weekday::Friday]).windows,
        ..QuietHours::default()
      };
      let friday = MONDAY_MIDNIGHT + 4 * 24 * HOUR;
      assert!(quiet.is_active_at(friday + 23 * HOUR, 0));
      assert!(quiet.is_active_at(friday + 24 * HOUR + 6 * HOUR, 0));
      assert!(!quiet.is_active_at(friday + 24 * HOUR + 7 * HOUR, 0));
      assert!(!quiet.is_active_at(friday + 21 * HOUR, 0));
      // Sunday 22:00 is not in a Friday window.
      assert!(!quiet.is_active_at(friday + 2 * 24 * HOUR + 22 * HOUR, 0));
    }

    #[test]
    fn a_window_ending_at_midnight_ends_with_the_day() {
      let quiet = QuietHours {
        windows: vec![DndWindow {
          weekdays: vec![Weekday::Monday],
          start_minute: 18 * 60,
          end_minute: 0,
        }],
        ..QuietHours::default()
      };
      assert!(quiet.is_active_at(MONDAY_MIDNIGHT + 23 * HOUR + 59 * 60, 0));
      assert!(!quiet.is_active_at(MONDAY_MIDNIGHT + 24 * HOUR, 0));
    }

    #[test]
    fn settings_from_older_clients_leave_the_filters_out() {
      let old: ConversationNotificationSettings =
        serde_json::from_str(r#"{"level":"mentions","is_inherited":false}"#).unwrap();
      assert_eq!(old.maybe_channel_mentions, None);
      assert_eq!(old.maybe_quiet_hours, None);
      let new = ConversationNotificationSettings {
        maybe_channel_mentions: Some(ChannelMentions::Notify),
        maybe_quiet_hours: Some(QuietHours {
          utc_offset_minutes: 60,
          maybe_time_zone: Some("Europe/Berlin".into()),
          windows: vec![DndWindow {
            weekdays: vec![Weekday::Saturday, Weekday::Sunday],
            start_minute: 600,
            end_minute: 1200,
          }],
        }),
        ..old
      };
      assert_eq!(
        serde_json::to_string(&new).unwrap(),
        r#"{"level":"mentions","is_inherited":false,"channel_mentions":"notify","quiet_hours":{"utc_offset_minutes":60,"time_zone":"Europe/Berlin","windows":[{"weekdays":["sat","sun"],"start_minute":600,"end_minute":1200}]}}"#
      );
    }
  }

  #[test]
  fn settings_wire_format_is_stable() {
    let settings = NotificationSettings {
      auto_follow_mentioned_threads: true,
      push_mode: PushMode::AwayOnly,
      send_read_receipts: false,
      maybe_dnd_schedule: None,
      maybe_uses_desktop_attention: None,
      maybe_level: None,
      maybe_default_level: None,
    };
    assert_eq!(
      serde_json::to_string(&settings).unwrap(),
      r#"{"auto_follow_mentioned_threads":true,"push_mode":"away_only","send_read_receipts":false}"#
    );
    let with_levels = NotificationSettings {
      maybe_level: Some(NotificationLevel::Mentions),
      maybe_default_level: Some(NotificationLevel::All),
      ..settings
    };
    assert_eq!(
      serde_json::to_string(&with_levels).unwrap(),
      r#"{"auto_follow_mentioned_threads":true,"push_mode":"away_only","send_read_receipts":false,"level":"mentions","default_level":"all"}"#
    );
    let unknown: PushMode = serde_json::from_str("\"sometimes\"").unwrap();
    assert_eq!(unknown, PushMode::Unknown("sometimes".into()));
  }
}
