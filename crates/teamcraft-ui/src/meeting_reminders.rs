//! Meeting reminders (notifications.md, "Meeting reminders"): a notification some minutes before a
//! meeting the user is invited to, and one when it starts, each with a Join action.
//!
//! Pure scheduling on the meetings the client knows; the wiring in `components::app` refreshes the
//! list, checks it once more right before a reminder is due (so a meeting cancelled or moved a minute
//! ago does not remind) and shows what [`ReminderSchedule::due`] returns. Times are the server's:
//! the list carries the server clock, and [`ServerClock`] applies the measured offset, so a device
//! whose clock is off still reminds on time. Each reminder is remembered by meeting, start time and
//! kind ([`FiredReminders`], kept in local storage), so a restart or a second tab never repeats it
//! and a moved meeting reminds again for its new time. Each device reminds once; the server does not
//! coordinate devices.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use teamcraft_proto::{MeetingId, MeetingKind, MeetingResponse, MeetingStatus, RsvpStatus};

use crate::meetings::my_rsvp;
use crate::model::{TimestampMs, UserId};

const MS_PER_MINUTE: i64 = 60_000;
const MS_PER_HOUR: i64 = 60 * MS_PER_MINUTE;
/// A start reminder still comes this long after the start (the device slept through it), never later.
const START_GRACE_MS: i64 = 10 * MS_PER_MINUTE;
/// Fired reminders are forgotten once their meeting started this long ago.
const FORGET_AFTER_MS: i64 = 24 * 60 * MS_PER_MINUTE;
/// How far ahead the next wake-up is planned at most: a safety net for a timer that browsers or the
/// system dropped, not a refresh schedule (changes arrive as gateway `meeting_update` events).
const MAX_SLEEP_MS: i64 = 6 * MS_PER_HOUR;

/// The user's reminder settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReminderSettings {
  /// Minutes before the start; `None`: no reminder before the meeting.
  pub maybe_minutes_before: Option<u16>,
  pub notifies_at_start: bool,
}

/// One reminder that is due now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DueReminder {
  pub meeting: MeetingId,
  pub kind: ReminderKind,
  pub title: String,
  pub starts_at: TimestampMs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderKind {
  Before,
  AtStart,
}

/// The server's clock, estimated from the meeting list's `server_now`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServerClock {
  /// Server time minus local time; zero until the server said.
  pub offset_ms: i64,
}

/// Reminders already shown on this device, by meeting, start time and kind.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FiredReminders(BTreeSet<ReminderKey>);

/// A reminder by meeting, start time and kind.
type ReminderKey = (MeetingId, TimestampMs, ReminderKind);

/// The meetings and settings reminders are planned from.
pub struct ReminderSchedule<'a> {
  pub meetings: &'a [MeetingResponse],
  pub me: UserId,
  pub settings: ReminderSettings,
}

impl ServerClock {
  /// The offset from one answer: the server's time against the middle of the request.
  pub fn measured(server_now: TimestampMs, sent_at: TimestampMs, received_at: TimestampMs) -> Self {
    let local_middle = sent_at + (received_at - sent_at) / 2;
    Self {
      offset_ms: server_now - local_middle,
    }
  }

  pub fn now(&self, local_now: TimestampMs) -> TimestampMs {
    local_now.saturating_add(self.offset_ms)
  }

  /// The local time at which the server's clock shows `server_time`.
  pub fn local_time_of(&self, server_time: TimestampMs) -> TimestampMs {
    server_time.saturating_sub(self.offset_ms)
  }
}

impl FiredReminders {
  pub fn from_json(text: &str) -> Self {
    serde_json::from_str(text).unwrap_or_default()
  }

  pub fn to_json(&self) -> String {
    serde_json::to_string(self).unwrap_or_else(|_| "[]".to_owned())
  }

  pub fn contains(&self, reminder: &DueReminder) -> bool {
    self
      .0
      .contains(&(reminder.meeting, reminder.starts_at, reminder.kind))
  }

  pub fn insert(&mut self, reminder: &DueReminder) {
    self
      .0
      .insert((reminder.meeting, reminder.starts_at, reminder.kind));
  }

  /// Drops entries of meetings that started long ago, so the stored list stays small.
  pub fn forget_old(&mut self, server_now: TimestampMs) {
    self
      .0
      .retain(|(_, starts_at, _)| server_now.saturating_sub(*starts_at) < FORGET_AFTER_MS);
  }
}

impl ReminderSchedule<'_> {
  /// Reminders due at `server_now` that this device has not shown, soonest meeting first. When the
  /// device slept past the "before" time and the meeting already started, only the start reminder
  /// comes.
  pub fn due(&self, server_now: TimestampMs, fired: &FiredReminders) -> Vec<DueReminder> {
    self
      .reminding_meetings()
      .filter_map(|meeting| self.due_for(meeting, server_now))
      .filter(|reminder| !fired.contains(reminder))
      .collect()
  }

  /// When (server time) the next reminder comes due, capped at [`MAX_SLEEP_MS`] from now.
  pub fn next_due_at(&self, server_now: TimestampMs, fired: &FiredReminders) -> TimestampMs {
    let cap = server_now + MAX_SLEEP_MS;
    self
      .reminding_meetings()
      .flat_map(|meeting| self.reminder_times(meeting))
      .filter(|(at, key)| *at > server_now && !fired.0.contains(key))
      .map(|(at, _)| at)
      .min()
      .map_or(cap, |at| at.min(cap))
  }

  /// Whether `reminder` still stands on this (fresh) list: the meeting is still on, at that time.
  pub fn still_due(&self, reminder: &DueReminder) -> bool {
    self
      .reminding_meetings()
      .any(|meeting| meeting.id == reminder.meeting && meeting.starts_at.0 == reminder.starts_at)
  }

  /// Meetings the user goes to: planned (not "meet now"), still on and not declined.
  fn reminding_meetings(&self) -> impl Iterator<Item = &MeetingResponse> {
    self.meetings.iter().filter(|meeting| {
      meeting.kind == MeetingKind::Scheduled
        && meeting.status == MeetingStatus::Scheduled
        && my_rsvp(meeting, self.me) != Some(RsvpStatus::Declined)
    })
  }

  fn due_for(&self, meeting: &MeetingResponse, server_now: TimestampMs) -> Option<DueReminder> {
    let starts_at = meeting.starts_at.0;
    let reminder = |kind| DueReminder {
      meeting: meeting.id,
      kind,
      title: meeting.title.clone(),
      starts_at,
    };
    let is_running =
      server_now >= starts_at && server_now < starts_at + START_GRACE_MS && server_now < meeting.ends_at.0;
    if is_running {
      return self
        .settings
        .notifies_at_start
        .then(|| reminder(ReminderKind::AtStart));
    }
    let minutes = self.settings.maybe_minutes_before?;
    let before_at = starts_at - i64::from(minutes) * MS_PER_MINUTE;
    (minutes > 0 && server_now >= before_at && server_now < starts_at).then(|| reminder(ReminderKind::Before))
  }

  /// Every reminder time of a meeting under the settings (server time), with its key.
  fn reminder_times(&self, meeting: &MeetingResponse) -> Vec<(TimestampMs, ReminderKey)> {
    let starts_at = meeting.starts_at.0;
    let mut times = Vec::new();
    if let Some(minutes) = self.settings.maybe_minutes_before.filter(|minutes| *minutes > 0) {
      times.push((
        starts_at - i64::from(minutes) * MS_PER_MINUTE,
        (meeting.id, starts_at, ReminderKind::Before),
      ));
    }
    if self.settings.notifies_at_start {
      times.push((starts_at, (meeting.id, starts_at, ReminderKind::AtStart)));
    }
    times
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{ConversationId, MeetingAttendeeResponse, UnixMillis};

  use super::*;
  use crate::model::id_from_raw;

  const NOON: TimestampMs = 1_792_540_800_000 + 12 * 60 * MS_PER_MINUTE;
  const FIVE_MINUTES: ReminderSettings = ReminderSettings {
    maybe_minutes_before: Some(5),
    notifies_at_start: true,
  };

  mod timing {
    use super::*;

    #[test]
    fn nothing_is_due_before_the_reminder_time() {
      let meetings = [meeting(1, NOON)];
      let schedule = schedule(&meetings, FIVE_MINUTES);
      assert!(
        schedule
          .due(NOON - 5 * MS_PER_MINUTE - 1, &FiredReminders::default())
          .is_empty()
      );
    }

    #[test]
    fn the_reminder_comes_minutes_before_and_again_at_the_start() {
      let meetings = [meeting(1, NOON)];
      let schedule = schedule(&meetings, FIVE_MINUTES);
      let mut fired = FiredReminders::default();
      let before = schedule.due(NOON - 5 * MS_PER_MINUTE, &fired);
      assert_eq!(kinds(&before), vec![ReminderKind::Before]);
      fired.insert(&before[0]);
      assert!(schedule.due(NOON - MS_PER_MINUTE, &fired).is_empty());
      let start = schedule.due(NOON, &fired);
      assert_eq!(kinds(&start), vec![ReminderKind::AtStart]);
    }

    #[test]
    fn a_device_that_slept_past_the_start_only_gets_the_start_reminder() {
      let meetings = [meeting(1, NOON)];
      let schedule = schedule(&meetings, FIVE_MINUTES);
      let due = schedule.due(NOON + 3 * MS_PER_MINUTE, &FiredReminders::default());
      assert_eq!(kinds(&due), vec![ReminderKind::AtStart]);
    }

    #[test]
    fn a_start_long_past_does_not_remind() {
      let meetings = [meeting(1, NOON)];
      let schedule = schedule(&meetings, FIVE_MINUTES);
      assert!(
        schedule
          .due(NOON + START_GRACE_MS, &FiredReminders::default())
          .is_empty()
      );
    }

    #[test]
    fn the_next_wake_up_is_the_next_reminder_or_the_cap() {
      let meetings = [meeting(1, NOON)];
      let schedule = schedule(&meetings, FIVE_MINUTES);
      let fired = FiredReminders::default();
      assert_eq!(
        schedule.next_due_at(NOON - 10 * MS_PER_MINUTE, &fired),
        NOON - 5 * MS_PER_MINUTE
      );
      assert_eq!(schedule.next_due_at(NOON - 2 * MS_PER_MINUTE, &fired), NOON);
      // A reminder hours away is waited for as such, not woken for every quarter of an hour.
      assert_eq!(
        schedule.next_due_at(NOON - 60 * MS_PER_MINUTE, &fired),
        NOON - 5 * MS_PER_MINUTE
      );
      assert_eq!(
        schedule.next_due_at(NOON - 10 * MS_PER_HOUR, &fired),
        NOON - 10 * MS_PER_HOUR + MAX_SLEEP_MS
      );
    }
  }

  mod settings {
    use super::*;

    #[test]
    fn reminders_before_can_be_turned_off() {
      let meetings = [meeting(1, NOON)];
      let only_start = ReminderSettings {
        maybe_minutes_before: None,
        notifies_at_start: true,
      };
      let schedule = schedule(&meetings, only_start);
      assert!(
        schedule
          .due(NOON - MS_PER_MINUTE, &FiredReminders::default())
          .is_empty()
      );
      assert_eq!(
        kinds(&schedule.due(NOON, &FiredReminders::default())),
        vec![ReminderKind::AtStart]
      );
    }

    #[test]
    fn everything_off_never_reminds() {
      let meetings = [meeting(1, NOON)];
      let off = ReminderSettings {
        maybe_minutes_before: None,
        notifies_at_start: false,
      };
      let schedule = schedule(&meetings, off);
      for at in [NOON - 5 * MS_PER_MINUTE, NOON, NOON + MS_PER_MINUTE] {
        assert!(schedule.due(at, &FiredReminders::default()).is_empty());
      }
      assert_eq!(
        schedule.next_due_at(NOON - MS_PER_MINUTE, &FiredReminders::default()),
        NOON - MS_PER_MINUTE + MAX_SLEEP_MS
      );
    }
  }

  mod changes {
    use super::*;

    #[test]
    fn cancelled_meetings_declined_invitations_and_instant_meetings_never_remind() {
      let mut cancelled = meeting(1, NOON);
      cancelled.status = MeetingStatus::Cancelled;
      let mut declined = meeting(2, NOON);
      declined.attendees = vec![attendee(RsvpStatus::Declined)];
      let mut instant = meeting(3, NOON);
      instant.kind = MeetingKind::Instant;
      let meetings = [cancelled, declined, instant];
      let schedule = schedule(&meetings, FIVE_MINUTES);
      assert!(schedule.due(NOON, &FiredReminders::default()).is_empty());
    }

    #[test]
    fn a_moved_meeting_reminds_again_for_its_new_time() {
      let mut fired = FiredReminders::default();
      let meetings = [meeting(1, NOON)];
      let before = schedule(&meetings, FIVE_MINUTES).due(NOON - 5 * MS_PER_MINUTE, &fired);
      fired.insert(&before[0]);
      let later = NOON + 60 * MS_PER_MINUTE;
      let moved = [meeting(1, later)];
      let schedule = schedule(&moved, FIVE_MINUTES);
      assert!(!schedule.still_due(&before[0]));
      assert_eq!(
        kinds(&schedule.due(later - 5 * MS_PER_MINUTE, &fired)),
        vec![ReminderKind::Before]
      );
    }

    #[test]
    fn a_reminder_is_checked_against_the_fresh_list_before_it_shows() {
      let meetings = [meeting(1, NOON)];
      let due = schedule(&meetings, FIVE_MINUTES).due(NOON, &FiredReminders::default());
      let mut cancelled = meeting(1, NOON);
      cancelled.status = MeetingStatus::Cancelled;
      let fresh = [cancelled];
      assert!(!schedule(&fresh, FIVE_MINUTES).still_due(&due[0]));
      assert!(schedule(&meetings, FIVE_MINUTES).still_due(&due[0]));
    }
  }

  mod restarts {
    use super::*;

    #[test]
    fn fired_reminders_survive_a_restart_through_storage() {
      let meetings = [meeting(1, NOON)];
      let schedule = schedule(&meetings, FIVE_MINUTES);
      let mut fired = FiredReminders::default();
      for reminder in schedule.due(NOON, &fired) {
        fired.insert(&reminder);
      }
      let restored = FiredReminders::from_json(&fired.to_json());
      assert_eq!(restored, fired);
      assert!(schedule.due(NOON + MS_PER_MINUTE, &restored).is_empty());
      assert_eq!(FiredReminders::from_json("garbage"), FiredReminders::default());
    }

    #[test]
    fn old_entries_are_forgotten() {
      let meetings = [meeting(1, NOON)];
      let mut fired = FiredReminders::default();
      for reminder in schedule(&meetings, FIVE_MINUTES).due(NOON, &fired) {
        fired.insert(&reminder);
      }
      fired.forget_old(NOON + FORGET_AFTER_MS - 1);
      assert_ne!(fired, FiredReminders::default());
      fired.forget_old(NOON + FORGET_AFTER_MS);
      assert_eq!(fired, FiredReminders::default());
    }
  }

  mod clock_skew {
    use super::*;

    #[test]
    fn the_server_clock_decides_not_the_device_clock() {
      // The device runs ten minutes behind: the request left at 11:44 local, the server said 11:55.
      let local_sent = NOON - 16 * MS_PER_MINUTE;
      let clock = ServerClock::measured(NOON - 5 * MS_PER_MINUTE, local_sent, local_sent + 2_000);
      assert_eq!(clock.offset_ms, 11 * MS_PER_MINUTE - 1_000);
      let meetings = [meeting(1, NOON)];
      let schedule = schedule(&meetings, FIVE_MINUTES);
      let local_now = local_sent + 2_000;
      assert_eq!(
        kinds(&schedule.due(clock.now(local_now), &FiredReminders::default())),
        vec![ReminderKind::Before]
      );
      assert_eq!(clock.local_time_of(clock.now(local_now)), local_now);
    }
  }

  fn schedule(meetings: &[MeetingResponse], settings: ReminderSettings) -> ReminderSchedule<'_> {
    ReminderSchedule {
      meetings,
      me: id_from_raw(1),
      settings,
    }
  }

  fn kinds(due: &[DueReminder]) -> Vec<ReminderKind> {
    due.iter().map(|reminder| reminder.kind).collect()
  }

  fn attendee(rsvp: RsvpStatus) -> MeetingAttendeeResponse {
    MeetingAttendeeResponse {
      maybe_user_id: Some(id_from_raw(1)),
      maybe_email: None,
      maybe_name: None,
      rsvp,
    }
  }

  fn meeting(id: u64, starts_at: TimestampMs) -> MeetingResponse {
    MeetingResponse {
      id: id_from_raw(id),
      title: "Weekly sync".into(),
      maybe_description: None,
      starts_at: UnixMillis(starts_at),
      ends_at: UnixMillis(starts_at + 30 * MS_PER_MINUTE),
      kind: MeetingKind::Scheduled,
      status: MeetingStatus::Scheduled,
      has_lobby: false,
      organizer_user_id: id_from_raw(2),
      conversation_id: id_from_raw::<ConversationId>(9),
      join_url: "https://chat.example.com/meeting/1".into(),
      maybe_recurrence: None,
      is_occurrence_cancelled: false,
      attendees: vec![attendee(RsvpStatus::Accepted)],
      undelivered_invitations: Vec::new(),
    }
  }
}
