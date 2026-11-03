//! The occurrences of a repeating meeting inside a time range (docs/meetings.md): a series is stored
//! once, with the first occurrence's start; the list expands it so every occurrence can remind,
//! appear in the calendar view and be joined.
//!
//! Occurrences repeat on the organizer's wall clock: "every Monday at 09:00" stays at 09:00 local
//! time across a daylight-saving change, so the UTC instant moves by an hour. The first occurrence is
//! always the stored start (RFC 5545: DTSTART is the first instance), also when it falls on a day the
//! rule doesn't name; `count` includes it.

use std::iter;

use chrono::{Datelike, Duration, LocalResult, Months, NaiveDate, NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use teamcraft_proto::{MeetingRecurrence, RecurrenceFrequency, Weekday};

/// Occurrences looked at per series at most, whatever the range: an open-ended daily series asked
/// for ten years stops here instead of looping on.
const MAX_STEPS: u32 = 20_000;
const DAYS_PER_WEEK: u32 = 7;

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
  series_starts(first_start_ms, recurrence, zone)
    .take_while(|starts_at_ms| *starts_at_ms < to_ms)
    .map(|starts_at_ms| Occurrence {
      starts_at_ms,
      ends_at_ms: starts_at_ms.saturating_add(duration_ms),
    })
    .filter(|occurrence| occurrence.ends_at_ms > from_ms)
    .collect()
}

/// Whether the series has an occurrence that starts exactly at `at_ms`.
pub fn is_occurrence_start(
  first_start_ms: i64,
  recurrence: &MeetingRecurrence,
  zone: Tz,
  at_ms: i64,
) -> bool {
  series_starts(first_start_ms, recurrence, zone)
    .take_while(|starts_at_ms| *starts_at_ms <= at_ms)
    .any(|starts_at_ms| starts_at_ms == at_ms)
}

/// The day of the week as the wire names it, Monday first; `None` for one this version doesn't know.
pub fn weekday_index(weekday: &Weekday) -> Option<u32> {
  match weekday {
    Weekday::Monday => Some(0),
    Weekday::Tuesday => Some(1),
    Weekday::Wednesday => Some(2),
    Weekday::Thursday => Some(3),
    Weekday::Friday => Some(4),
    Weekday::Saturday => Some(5),
    Weekday::Sunday => Some(6),
    Weekday::Unknown(_) => None,
  }
}

/// Every start of the series in order, as UTC instants, with the rule's count and end applied.
fn series_starts(first_start_ms: i64, recurrence: &MeetingRecurrence, zone: Tz) -> impl Iterator<Item = i64> {
  let maybe_first = zone
    .timestamp_millis_opt(first_start_ms)
    .single()
    .map(|start| start.naive_local());
  let count = recurrence.maybe_count.unwrap_or(u32::MAX);
  let until_ms = recurrence.maybe_until.map_or(i64::MAX, |until| until.0);
  let later = maybe_first
    .into_iter()
    .flat_map(move |first| local_starts(first, recurrence).filter(move |start| *start > first))
    .filter_map(move |local| resolve(local, zone));
  maybe_first
    .map(|_| first_start_ms)
    .into_iter()
    .chain(later)
    .take(usize::try_from(count.min(MAX_STEPS)).unwrap_or(usize::MAX))
    .take_while(move |starts_at_ms| *starts_at_ms <= until_ms)
}

/// The wall-clock starts the rule produces from `first` on (it may produce `first` itself), at most
/// [`MAX_STEPS`] candidates.
fn local_starts<'a>(
  first: NaiveDateTime,
  recurrence: &'a MeetingRecurrence,
) -> Box<dyn Iterator<Item = NaiveDateTime> + 'a> {
  let interval = recurrence.interval.max(1);
  let days = weekday_set(&recurrence.weekdays);
  match recurrence.frequency {
    RecurrenceFrequency::Daily => Box::new(
      (0..MAX_STEPS)
        .map_while(move |step| {
          first.checked_add_signed(Duration::try_days(i64::from(interval) * i64::from(step))?)
        })
        .filter(move |start| days.is_none_or(|days| days[weekday_of(start.date())])),
    ),
    RecurrenceFrequency::Weekly => {
      let days = days.unwrap_or_else(|| {
        let mut only = [false; 7];
        only[weekday_of(first.date())] = true;
        only
      });
      let week_start = first.date() - Duration::days(i64::from(first.weekday().num_days_from_monday()));
      Box::new(
        (0..MAX_STEPS / DAYS_PER_WEEK)
          .map_while(move |week| {
            week_start.checked_add_signed(Duration::try_weeks(i64::from(interval) * i64::from(week))?)
          })
          .flat_map(move |monday| {
            (0..DAYS_PER_WEEK)
              .filter(move |day| days[*day as usize])
              .filter_map(move |day| monday.checked_add_signed(Duration::days(i64::from(day))))
          })
          .map(move |date| date.and_time(first.time())),
      )
    },
    RecurrenceFrequency::Monthly => Box::new(
      (0..MAX_STEPS)
        .map_while(move |step| first.checked_add_months(Months::new(interval.checked_mul(step)?))),
    ),
    RecurrenceFrequency::Unknown(_) => Box::new(iter::empty()),
  }
}

/// The chosen days, Monday first; `None` when the rule names none (or only unknown ones).
fn weekday_set(weekdays: &[Weekday]) -> Option<[bool; 7]> {
  let mut days = [false; 7];
  for index in weekdays.iter().filter_map(weekday_index) {
    days[index as usize] = true;
  }
  days.contains(&true).then_some(days)
}

fn weekday_of(date: NaiveDate) -> usize {
  date.weekday().num_days_from_monday() as usize
}

/// A wall-clock time as a UTC instant. A time the zone skips (the spring change) moves an hour on;
/// one it has twice (the autumn change) takes the earlier.
fn resolve(local: NaiveDateTime, zone: Tz) -> Option<i64> {
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
  use teamcraft_proto::UnixMillis;

  use super::*;

  const HOUR: i64 = 3_600_000;
  const DAY: i64 = 24 * HOUR;
  const WORK_WEEK: [Weekday; 5] = [
    Weekday::Monday,
    Weekday::Tuesday,
    Weekday::Wednesday,
    Weekday::Thursday,
    Weekday::Friday,
  ];

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
      assert_eq!(starts(&found), vec![2 * DAY, 3 * DAY, 4 * DAY]);
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
      assert_eq!(starts(&found), vec![0, 14 * DAY, 28 * DAY]);
    }

    #[test]
    fn until_ends_the_series_and_includes_an_occurrence_starting_then() {
      let mut daily = series(RecurrenceFrequency::Daily, 1, None);
      daily.maybe_until = Some(UnixMillis(3 * DAY));
      let found = occurrences_in(0, HOUR, &daily, Tz::UTC, 0, 365 * DAY);
      assert_eq!(starts(&found), vec![0, DAY, 2 * DAY, 3 * DAY]);
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

    #[test]
    fn only_real_starts_are_occurrences() {
      let daily = series(RecurrenceFrequency::Daily, 2, None);
      assert!(is_occurrence_start(0, &daily, Tz::UTC, 4 * DAY));
      assert!(!is_occurrence_start(0, &daily, Tz::UTC, 3 * DAY));
      assert!(!is_occurrence_start(0, &daily, Tz::UTC, 4 * DAY + 1));
      assert!(!is_occurrence_start(0, &daily, Tz::UTC, -2 * DAY));
    }
  }

  mod weekdays {
    use super::*;

    #[test]
    fn a_daily_series_on_work_days_skips_the_weekend() {
      // Thursday 2026-10-29 09:00 UTC.
      let first = utc(2026, 10, 29, 9);
      let mut daily = series(RecurrenceFrequency::Daily, 1, Some(4));
      daily.weekdays = WORK_WEEK.to_vec();
      let found = occurrences_in(first, HOUR, &daily, Tz::UTC, 0, i64::MAX);
      assert_eq!(days(&found), vec![29, 30, 2, 3]);
    }

    #[test]
    fn a_weekly_series_meets_on_each_chosen_day() {
      // Monday 2026-11-02, every second week on Monday and Thursday.
      let first = utc(2026, 11, 2, 9);
      let mut weekly = series(RecurrenceFrequency::Weekly, 2, Some(5));
      weekly.weekdays = vec![Weekday::Thursday, Weekday::Monday];
      let found = occurrences_in(first, HOUR, &weekly, Tz::UTC, 0, i64::MAX);
      assert_eq!(days(&found), vec![2, 5, 16, 19, 30]);
    }

    #[test]
    fn days_of_the_first_week_before_the_start_are_left_out() {
      // Wednesday 2026-11-04; Monday of that week is before the start.
      let first = utc(2026, 11, 4, 9);
      let mut weekly = series(RecurrenceFrequency::Weekly, 1, Some(3));
      weekly.weekdays = vec![Weekday::Monday, Weekday::Wednesday];
      let found = occurrences_in(first, HOUR, &weekly, Tz::UTC, 0, i64::MAX);
      assert_eq!(days(&found), vec![4, 9, 11]);
    }

    #[test]
    fn a_first_occurrence_off_the_chosen_days_still_counts() {
      // Saturday 2026-10-31, then work days only.
      let first = utc(2026, 10, 31, 9);
      let mut daily = series(RecurrenceFrequency::Daily, 1, Some(3));
      daily.weekdays = WORK_WEEK.to_vec();
      let found = occurrences_in(first, HOUR, &daily, Tz::UTC, 0, i64::MAX);
      assert_eq!(days(&found), vec![31, 2, 3]);
    }

    #[test]
    fn days_that_an_interval_never_reaches_end_the_series_without_looping() {
      // Every seventh day from a Monday, but only on Tuesdays: never after the first.
      let first = utc(2026, 11, 2, 9);
      let mut daily = series(RecurrenceFrequency::Daily, 7, None);
      daily.weekdays = vec![Weekday::Tuesday];
      let found = occurrences_in(first, HOUR, &daily, Tz::UTC, 0, i64::MAX);
      assert_eq!(found.len(), 1);
    }

    #[test]
    fn work_days_are_the_organizers_not_utc() {
      // Monday 2026-11-02 00:30 in Berlin is Sunday 23:30 UTC; Friday's meeting is the last of the week.
      let zone = Tz::Europe__Berlin;
      let first = zone
        .with_ymd_and_hms(2026, 11, 2, 0, 30, 0)
        .unwrap()
        .timestamp_millis();
      let mut daily = series(RecurrenceFrequency::Daily, 1, Some(6));
      daily.weekdays = WORK_WEEK.to_vec();
      let found = occurrences_in(first, HOUR, &daily, zone, 0, i64::MAX);
      let local_days: Vec<u32> = found
        .iter()
        .map(|occurrence| zone.timestamp_millis_opt(occurrence.starts_at_ms).unwrap().day())
        .collect();
      assert_eq!(local_days, vec![2, 3, 4, 5, 6, 9]);
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
    fn a_work_day_meeting_keeps_its_local_time_across_the_autumn_change() {
      let zone = Tz::Europe__Berlin;
      // Friday 2026-10-23 09:00 local (UTC+2); the clocks went back on Sunday 2026-10-25.
      let first = zone
        .with_ymd_and_hms(2026, 10, 23, 9, 0, 0)
        .unwrap()
        .timestamp_millis();
      let mut daily = series(RecurrenceFrequency::Daily, 1, Some(2));
      daily.weekdays = WORK_WEEK.to_vec();
      let found = occurrences_in(first, HOUR, &daily, zone, 0, i64::MAX);
      assert_eq!(found[1].starts_at_ms - found[0].starts_at_ms, 3 * DAY + HOUR);
      let local = zone.timestamp_millis_opt(found[1].starts_at_ms).unwrap();
      assert_eq!((local.day(), local.hour()), (26, 9));
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
      let first = utc(2026, 1, 31, 10);
      let found = occurrences_in(
        first,
        HOUR,
        &series(RecurrenceFrequency::Monthly, 1, Some(3)),
        Tz::UTC,
        0,
        i64::MAX,
      );
      let dates: Vec<(u32, u32)> = found
        .iter()
        .map(|occurrence| {
          let local = Tz::UTC.timestamp_millis_opt(occurrence.starts_at_ms).unwrap();
          (local.month(), local.day())
        })
        .collect();
      assert_eq!(dates, vec![(1, 31), (2, 28), (3, 31)]);
    }
  }

  fn series(frequency: RecurrenceFrequency, interval: u32, maybe_count: Option<u32>) -> MeetingRecurrence {
    MeetingRecurrence {
      frequency,
      interval,
      weekdays: Vec::new(),
      maybe_count,
      maybe_until: None,
    }
  }

  fn utc(year: i32, month: u32, day: u32, hour: u32) -> i64 {
    Tz::UTC
      .with_ymd_and_hms(year, month, day, hour, 0, 0)
      .unwrap()
      .timestamp_millis()
  }

  fn starts(found: &[Occurrence]) -> Vec<i64> {
    found.iter().map(|occurrence| occurrence.starts_at_ms).collect()
  }

  fn days(found: &[Occurrence]) -> Vec<u32> {
    found
      .iter()
      .map(|occurrence| {
        Tz::UTC
          .timestamp_millis_opt(occurrence.starts_at_ms)
          .unwrap()
          .day()
      })
      .collect()
  }
}
