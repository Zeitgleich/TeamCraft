//! The occurrences of a repeating meeting inside a time range (docs/meetings.md): a series is stored
//! once, with the first occurrence's start; the list expands it so every occurrence can remind,
//! appear in the calendar view and be joined.
//!
//! Occurrences repeat on the organizer's wall clock: "every Monday at 09:00" stays at 09:00 local
//! time across a daylight-saving change, so the UTC instant moves by an hour.

use chrono::{Duration, LocalResult, Months, NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use teamcraft_proto::{MeetingRecurrence, RecurrenceFrequency};

/// Occurrences looked at per series at most, whatever the range: an open-ended daily series asked
/// for ten years stops here instead of looping on.
const MAX_STEPS: u32 = 20_000;

/// One occurrence: start and end in Unix milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Occurrence {
  pub starts_at_ms: i64,
  pub ends_at_ms: i64,
}

/// The series' occurrences that end after `from_ms` and start before `to_ms`, soonest first. The
/// first occurrence is `first_start_ms` itself; the length of each is `duration_ms`.
pub fn occurrences_in(
  first_start_ms: i64,
  duration_ms: i64,
  recurrence: &MeetingRecurrence,
  zone: Tz,
  from_ms: i64,
  to_ms: i64,
) -> Vec<Occurrence> {
  let Some(first_local) = zone
    .timestamp_millis_opt(first_start_ms)
    .single()
    .map(|start| start.naive_local())
  else {
    return Vec::new();
  };
  let interval = recurrence.interval.max(1);
  let count = recurrence.maybe_count.unwrap_or(u32::MAX);
  let mut found = Vec::new();
  for step in 0..count.min(MAX_STEPS) {
    let starts_at_ms = if step == 0 {
      first_start_ms
    } else {
      match local_start(first_local, &recurrence.frequency, interval, step, zone) {
        Some(start) => start,
        None => break,
      }
    };
    if starts_at_ms >= to_ms {
      break;
    }
    let ends_at_ms = starts_at_ms.saturating_add(duration_ms);
    if ends_at_ms > from_ms {
      found.push(Occurrence {
        starts_at_ms,
        ends_at_ms,
      });
    }
  }
  found
}

/// The start of occurrence `step` as a UTC instant: the first one's wall clock moved by whole days,
/// weeks or months. A day a month doesn't have becomes its last day; a wall-clock time the zone
/// skips (the spring change) moves an hour on; one it has twice (the autumn change) takes the
/// earlier.
fn local_start(
  first: NaiveDateTime,
  frequency: &RecurrenceFrequency,
  interval: u32,
  step: u32,
  zone: Tz,
) -> Option<i64> {
  let moved = i64::from(interval) * i64::from(step);
  let local = match frequency {
    RecurrenceFrequency::Daily => first.checked_add_signed(Duration::try_days(moved)?)?,
    RecurrenceFrequency::Weekly => first.checked_add_signed(Duration::try_weeks(moved)?)?,
    RecurrenceFrequency::Monthly => first.checked_add_months(Months::new(u32::try_from(moved).ok()?))?,
    RecurrenceFrequency::Unknown(_) => return None,
  };
  let resolved = match zone.from_local_datetime(&local) {
    LocalResult::Single(time) => time,
    LocalResult::Ambiguous(earlier, _) => earlier,
    LocalResult::None => zone
      .from_local_datetime(&local.checked_add_signed(Duration::try_hours(1)?)?)
      .earliest()?,
  };
  Some(resolved.timestamp_millis())
}

#[cfg(test)]
mod tests {
  use chrono::{Datelike, Timelike};

  use super::*;

  const HOUR: i64 = 3_600_000;
  const DAY: i64 = 24 * HOUR;

  mod ranges {
    use super::*;

    #[test]
    fn a_daily_series_expands_inside_the_range() {
      let found = occurrences_in(
        0,
        HOUR,
        &series(RecurrenceFrequency::Daily, 1, None),
        Tz::UTC,
        2 * DAY,
        5 * DAY,
      );
      let starts: Vec<i64> = found.iter().map(|o| o.starts_at_ms).collect();
      assert_eq!(starts, vec![2 * DAY, 3 * DAY, 4 * DAY]);
    }

    #[test]
    fn an_occurrence_still_running_at_the_start_of_the_range_counts() {
      let found = occurrences_in(
        0,
        2 * HOUR,
        &series(RecurrenceFrequency::Daily, 1, None),
        Tz::UTC,
        DAY + HOUR,
        DAY + 2 * HOUR,
      );
      assert_eq!(found.len(), 1);
      assert_eq!(found[0].starts_at_ms, DAY);
    }

    #[test]
    fn the_count_limits_the_series_and_includes_the_first() {
      let found = occurrences_in(
        0,
        HOUR,
        &series(RecurrenceFrequency::Weekly, 2, Some(3)),
        Tz::UTC,
        0,
        365 * DAY,
      );
      let starts: Vec<i64> = found.iter().map(|o| o.starts_at_ms).collect();
      assert_eq!(starts, vec![0, 14 * DAY, 28 * DAY]);
    }

    #[test]
    fn an_open_ended_series_in_a_huge_range_stops() {
      let found = occurrences_in(
        0,
        HOUR,
        &series(RecurrenceFrequency::Daily, 1, None),
        Tz::UTC,
        0,
        i64::MAX,
      );
      assert_eq!(found.len(), MAX_STEPS as usize);
    }
  }

  mod wall_clock {
    use super::*;

    #[test]
    fn a_weekly_meeting_keeps_its_local_time_across_the_spring_change() {
      let zone = Tz::Europe__Berlin;
      // Monday 2026-03-23 09:00 local (UTC+1); the clocks changed on 2026-03-29.
      let first = zone
        .with_ymd_and_hms(2026, 3, 23, 9, 0, 0)
        .unwrap()
        .timestamp_millis();
      let found = occurrences_in(
        first,
        HOUR,
        &series(RecurrenceFrequency::Weekly, 1, Some(2)),
        zone,
        0,
        i64::MAX,
      );
      assert_eq!(found.len(), 2);
      assert_eq!(found[1].starts_at_ms - found[0].starts_at_ms, 7 * DAY - HOUR);
      let local = zone.timestamp_millis_opt(found[1].starts_at_ms).unwrap();
      assert_eq!((local.hour(), local.minute()), (9, 0));
    }

    #[test]
    fn a_wall_clock_time_the_zone_skips_moves_an_hour_on() {
      let zone = Tz::Europe__Berlin;
      // Saturday 2026-03-28 02:30 local exists; Sunday 2026-03-29 02:30 does not.
      let first = zone
        .with_ymd_and_hms(2026, 3, 28, 2, 30, 0)
        .unwrap()
        .timestamp_millis();
      let found = occurrences_in(
        first,
        HOUR,
        &series(RecurrenceFrequency::Daily, 1, Some(2)),
        zone,
        0,
        i64::MAX,
      );
      let local = zone.timestamp_millis_opt(found[1].starts_at_ms).unwrap();
      assert_eq!((local.hour(), local.minute()), (3, 30));
    }

    #[test]
    fn a_monthly_meeting_on_the_31st_takes_the_last_day_of_shorter_months() {
      let first = Tz::UTC
        .with_ymd_and_hms(2026, 1, 31, 10, 0, 0)
        .unwrap()
        .timestamp_millis();
      let found = occurrences_in(
        first,
        HOUR,
        &series(RecurrenceFrequency::Monthly, 1, Some(2)),
        Tz::UTC,
        0,
        i64::MAX,
      );
      let local = Tz::UTC.timestamp_millis_opt(found[1].starts_at_ms).unwrap();
      assert_eq!((local.year(), local.month(), local.day()), (2026, 2, 28));
    }
  }

  fn series(frequency: RecurrenceFrequency, interval: u32, maybe_count: Option<u32>) -> MeetingRecurrence {
    MeetingRecurrence {
      frequency,
      interval,
      maybe_count,
    }
  }
}
