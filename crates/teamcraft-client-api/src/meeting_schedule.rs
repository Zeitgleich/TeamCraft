//! Until when the organizer's device signs who may let guests in (e2ee.md §4.6, "Who is a guest,
//! and until when"): from the schedule the organizer set on this device, never from a later end the
//! server reports (#1919). Desktop and web (`teamcraft-ui`), the mobile apps (through
//! teamcraft-ffi) and the core use these functions; none computes the end on its own.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{CreateMeetingRequest, MeetingRecurrence, MeetingResponse, UpdateMeetingRequest};

use crate::meeting_edits::ADMITTERS_LEAD_MS;
use crate::meeting_invitees::GUEST_ACCESS_GRACE_MS;

/// Until when the organizer's signed admitters of a meeting last, by this device's word: the end of
/// the meeting (or of the occurrence the server lists) and the guests' grace.
///
/// With `maybe_recorded`, the schedule the organizer set on this device, the server's end counts
/// only where it is earlier than the recorded one: a server that lengthens a meeting gets no longer
/// signature. Without a record (a meeting handed to this user, or scheduled on another device) the
/// server's end is all the device knows.
pub fn admitters_until(server: MeetingTimes, maybe_recorded: Option<&MeetingSchedule>, now_ms: i64) -> i64 {
  let end = match maybe_recorded {
    Some(recorded) => server
      .ends_at_ms
      .min(recorded.occurrence_end(server.starts_at_ms, now_ms)),
    None => server.ends_at_ms,
  };
  end.saturating_add(GUEST_ACCESS_GRACE_MS)
}

/// A meeting's (or one occurrence's) start and end, in milliseconds since the epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingTimes {
  pub starts_at_ms: i64,
  pub ends_at_ms: i64,
}

/// The schedule the organizer set for a meeting on this device: what the core keeps as the
/// device's own record and signs the admitters' validity from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingSchedule {
  /// The meeting, or the first occurrence of a series.
  pub times: MeetingTimes,
  #[serde(rename = "recurrence", default, skip_serializing_if = "Option::is_none")]
  pub maybe_recurrence: Option<MeetingRecurrence>,
}

/// What this device just set of a meeting's schedule, for its record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleNote {
  /// A meeting scheduled here: the whole schedule.
  Scheduled { schedule: MeetingSchedule },
  /// A meeting started here ("Meet now", a call that grows into a meeting): the request names no
  /// times, so the server's answer is all there is; the record bounds it
  /// ([`MeetingSchedule::started`]).
  Started { server_times: MeetingTimes },
  /// An edit saved here: only what it changed.
  Edited { edit: ScheduleEdit },
}

/// The parts of the schedule an edit on this device changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleEdit {
  /// The new start and end, when the meeting moved.
  #[serde(rename = "times", default, skip_serializing_if = "Option::is_none")]
  pub maybe_times: Option<MeetingTimes>,
  /// The new repeat rule, when it changed.
  #[serde(rename = "recurrence", default, skip_serializing_if = "Option::is_none")]
  pub maybe_recurrence: Option<MeetingRecurrence>,
  /// The rule as the server had it before the edit: taken only when the device has no record
  /// and the edit moved the meeting. It shapes which occurrence counts, never how long one lasts.
  #[serde(
    rename = "listed_recurrence",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_listed_recurrence: Option<MeetingRecurrence>,
}

impl MeetingTimes {
  /// The times as the server lists `meeting` (one occurrence of a series).
  pub fn of(meeting: &MeetingResponse) -> Self {
    Self {
      starts_at_ms: meeting.starts_at.0,
      ends_at_ms: meeting.ends_at.0,
    }
  }
}

impl MeetingSchedule {
  /// The schedule of a meeting this device asks the server to create.
  pub fn scheduled(request: &CreateMeetingRequest) -> Self {
    Self {
      times: MeetingTimes {
        starts_at_ms: request.starts_at.0,
        ends_at_ms: request.ends_at.0,
      },
      maybe_recurrence: request.maybe_recurrence.clone(),
    }
  }

  /// The schedule of a meeting started at `now_ms` ("Meet now"): the server's answer, but ending
  /// no later than `max_length_ms` from now, whatever end the server answered. Later answers no
  /// longer lengthen it.
  pub fn started(server_times: MeetingTimes, now_ms: i64, max_length_ms: i64) -> Self {
    let latest_end = now_ms.saturating_add(max_length_ms.max(0));
    Self {
      times: MeetingTimes {
        starts_at_ms: server_times.starts_at_ms.min(now_ms),
        ends_at_ms: server_times.ends_at_ms.min(latest_end),
      },
      maybe_recurrence: None,
    }
  }

  /// The record after `note`: a new meeting replaces it, an edit changes what it changed. An edit
  /// that moved nothing leaves a device without a record without one (the server's end applies).
  /// `now_ms` and `max_instant_ms` bound a meeting started here ([`Self::started`]).
  pub fn after(
    maybe_recorded: Option<Self>,
    note: ScheduleNote,
    now_ms: i64,
    max_instant_ms: i64,
  ) -> Option<Self> {
    match note {
      ScheduleNote::Scheduled { schedule } => Some(schedule),
      ScheduleNote::Started { server_times } => Some(Self::started(server_times, now_ms, max_instant_ms)),
      ScheduleNote::Edited { edit } => match maybe_recorded {
        Some(recorded) => Some(Self {
          times: edit.maybe_times.unwrap_or(recorded.times),
          maybe_recurrence: edit.maybe_recurrence.or(recorded.maybe_recurrence),
        }),
        None => edit.maybe_times.map(|times| Self {
          times,
          maybe_recurrence: edit.maybe_recurrence.or(edit.maybe_listed_recurrence),
        }),
      },
    }
  }

  /// The end of the occurrence the server says starts at `server_start_ms`, by this record. A
  /// single meeting ends at its recorded end. A series lasts the recorded duration from the
  /// occurrence's start, which counts only between the first start and the rule's `until`, and no
  /// further ahead than the upkeep signs ([`ADMITTERS_LEAD_MS`]): a server that names a far-off
  /// occurrence gets no long signature. The days of the rule are not expanded on clients (that
  /// needs the instance's time zone), so the start is bounded, not checked against them.
  fn occurrence_end(&self, server_start_ms: i64, now_ms: i64) -> i64 {
    let Some(recurrence) = &self.maybe_recurrence else {
      return self.times.ends_at_ms;
    };
    let first_start = self.times.starts_at_ms;
    let duration = self.times.ends_at_ms.saturating_sub(first_start).max(0);
    let mut latest_start = now_ms.saturating_add(ADMITTERS_LEAD_MS);
    if let Some(until) = recurrence.maybe_until {
      latest_start = latest_start.min(until.0);
    }
    let start = server_start_ms.min(latest_start).max(first_start);
    start.saturating_add(duration)
  }
}

impl ScheduleEdit {
  /// What `request`, an edit of `before` (the meeting as the server had it), changes of the
  /// schedule; `None` when it changes neither the times nor the rule.
  pub fn of(before: &MeetingResponse, request: &UpdateMeetingRequest) -> Option<Self> {
    let maybe_times = match (request.maybe_starts_at, request.maybe_ends_at) {
      (Some(starts_at), Some(ends_at)) => Some(MeetingTimes {
        starts_at_ms: starts_at.0,
        ends_at_ms: ends_at.0,
      }),
      _ => None,
    };
    if maybe_times.is_none() && request.maybe_recurrence.is_none() {
      return None;
    }
    Some(Self {
      maybe_times,
      maybe_recurrence: request.maybe_recurrence.clone(),
      maybe_listed_recurrence: before.maybe_recurrence.clone(),
    })
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{RecurrenceFrequency, UnixMillis};

  use super::*;

  const HOUR_MS: i64 = 60 * 60_000;
  const DAY_MS: i64 = 24 * HOUR_MS;
  const START_MS: i64 = 1_000 * DAY_MS;
  const END_MS: i64 = START_MS + HOUR_MS;

  mod single {
    use super::*;

    #[test]
    fn a_server_that_lengthens_the_meeting_gets_the_recorded_end() {
      let recorded = schedule(None);
      let lengthened = times(START_MS, END_MS + 10 * DAY_MS);
      assert_eq!(
        admitters_until(lengthened, Some(&recorded), START_MS),
        END_MS + GUEST_ACCESS_GRACE_MS
      );
    }

    #[test]
    fn an_earlier_server_end_counts() {
      let recorded = schedule(None);
      let shortened = times(START_MS, END_MS - HOUR_MS / 2);
      assert_eq!(
        admitters_until(shortened, Some(&recorded), START_MS),
        END_MS - HOUR_MS / 2 + GUEST_ACCESS_GRACE_MS
      );
    }

    #[test]
    fn without_a_record_the_server_end_counts() {
      let handed_over = times(START_MS, END_MS + DAY_MS);
      assert_eq!(
        admitters_until(handed_over, None, START_MS),
        END_MS + DAY_MS + GUEST_ACCESS_GRACE_MS
      );
    }
  }

  mod series {
    use super::*;

    #[test]
    fn an_occurrence_lasts_the_recorded_duration() {
      let recorded = schedule(Some(daily(None)));
      let third = times(START_MS + 2 * DAY_MS, START_MS + 2 * DAY_MS + 5 * HOUR_MS);
      assert_eq!(
        admitters_until(third, Some(&recorded), START_MS + 2 * DAY_MS),
        START_MS + 2 * DAY_MS + HOUR_MS + GUEST_ACCESS_GRACE_MS
      );
    }

    #[test]
    fn an_occurrence_starts_no_further_ahead_than_the_upkeep_signs() {
      let recorded = schedule(Some(daily(None)));
      let now = START_MS + DAY_MS;
      let far_off = times(START_MS + 400 * DAY_MS, START_MS + 400 * DAY_MS + HOUR_MS);
      assert_eq!(
        admitters_until(far_off, Some(&recorded), now),
        now + ADMITTERS_LEAD_MS + HOUR_MS + GUEST_ACCESS_GRACE_MS
      );
    }

    #[test]
    fn an_occurrence_starts_no_later_than_the_recorded_until() {
      let until = START_MS + 3 * DAY_MS;
      let recorded = schedule(Some(daily(Some(until))));
      let after_until = times(START_MS + 5 * DAY_MS, START_MS + 5 * DAY_MS + HOUR_MS);
      assert_eq!(
        admitters_until(after_until, Some(&recorded), START_MS + 5 * DAY_MS),
        until + HOUR_MS + GUEST_ACCESS_GRACE_MS
      );
    }

    #[test]
    fn the_first_occurrence_signed_at_scheduling_lasts_until_its_end() {
      let recorded = schedule(Some(daily(None)));
      let now = START_MS - 30 * DAY_MS;
      assert_eq!(
        admitters_until(times(START_MS, END_MS), Some(&recorded), now),
        END_MS + GUEST_ACCESS_GRACE_MS
      );
    }
  }

  mod record {
    use super::*;

    #[test]
    fn an_edit_changes_what_it_moved() {
      let moved = times(START_MS + DAY_MS, END_MS + DAY_MS + HOUR_MS);
      let edit = ScheduleEdit {
        maybe_times: Some(moved),
        maybe_recurrence: None,
        maybe_listed_recurrence: Some(daily(Some(START_MS))),
      };
      let after = MeetingSchedule::after(
        Some(schedule(Some(daily(None)))),
        ScheduleNote::Edited { edit },
        0,
        0,
      );
      assert_eq!(
        after,
        Some(MeetingSchedule {
          times: moved,
          maybe_recurrence: Some(daily(None))
        })
      );
    }

    #[test]
    fn an_edit_without_a_record_records_only_a_move() {
      let edit = ScheduleEdit {
        maybe_times: None,
        maybe_recurrence: Some(daily(None)),
        maybe_listed_recurrence: None,
      };
      assert_eq!(
        MeetingSchedule::after(None, ScheduleNote::Edited { edit }, 0, 0),
        None
      );
      let moved = times(START_MS, END_MS);
      let edit = ScheduleEdit {
        maybe_times: Some(moved),
        maybe_recurrence: None,
        maybe_listed_recurrence: Some(daily(None)),
      };
      assert_eq!(
        MeetingSchedule::after(None, ScheduleNote::Edited { edit }, 0, 0),
        Some(MeetingSchedule {
          times: moved,
          maybe_recurrence: Some(daily(None))
        })
      );
    }

    #[test]
    fn scheduling_replaces_the_record() {
      let note = ScheduleNote::Scheduled {
        schedule: schedule(None),
      };
      assert_eq!(
        MeetingSchedule::after(Some(schedule(Some(daily(None)))), note, 0, 0),
        Some(schedule(None))
      );
    }

    #[test]
    fn the_note_keeps_its_wire_shape() {
      let note = ScheduleNote::Edited {
        edit: ScheduleEdit {
          maybe_times: Some(times(1, 2)),
          maybe_recurrence: None,
          maybe_listed_recurrence: None,
        },
      };
      let json = serde_json::to_string(&note).unwrap();
      assert_eq!(
        json,
        r#"{"kind":"edited","edit":{"times":{"starts_at_ms":1,"ends_at_ms":2}}}"#
      );
      assert_eq!(serde_json::from_str::<ScheduleNote>(&json).unwrap(), note);
    }
  }

  mod started {
    use super::*;

    const MAX_MS: i64 = 8 * HOUR_MS;

    #[test]
    fn a_far_server_end_is_bounded_by_the_maximum_length() {
      let note = ScheduleNote::Started {
        server_times: times(START_MS, START_MS + 90 * DAY_MS),
      };
      let recorded = MeetingSchedule::after(None, note, START_MS, MAX_MS).unwrap();
      let lengthened = times(START_MS, START_MS + 90 * DAY_MS);
      assert_eq!(
        admitters_until(lengthened, Some(&recorded), START_MS),
        START_MS + MAX_MS + GUEST_ACCESS_GRACE_MS
      );
    }

    #[test]
    fn a_shorter_server_end_is_kept() {
      let started = MeetingSchedule::started(times(START_MS, START_MS + HOUR_MS), START_MS, MAX_MS);
      assert_eq!(started.times, times(START_MS, START_MS + HOUR_MS));
    }

    #[test]
    fn a_start_in_the_future_counts_from_now() {
      let started =
        MeetingSchedule::started(times(START_MS + DAY_MS, START_MS + 2 * DAY_MS), START_MS, MAX_MS);
      assert_eq!(started.times, times(START_MS, START_MS + MAX_MS));
    }
  }

  fn times(starts_at_ms: i64, ends_at_ms: i64) -> MeetingTimes {
    MeetingTimes {
      starts_at_ms,
      ends_at_ms,
    }
  }

  fn schedule(maybe_recurrence: Option<MeetingRecurrence>) -> MeetingSchedule {
    MeetingSchedule {
      times: times(START_MS, END_MS),
      maybe_recurrence,
    }
  }

  fn daily(maybe_until_ms: Option<i64>) -> MeetingRecurrence {
    MeetingRecurrence {
      frequency: RecurrenceFrequency::Daily,
      interval: 1,
      weekdays: Vec::new(),
      maybe_count: None,
      maybe_until: maybe_until_ms.map(UnixMillis),
      skips_holidays: false,
    }
  }
}
