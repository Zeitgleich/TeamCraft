//! Wall-clock access and locale-aware time formatting. Browser APIs are used in the wasm build; the
//! native build (tests, clippy) falls back to plain UTC arithmetic.

use crate::model::TimestampMs;

const MS_PER_MINUTE: i64 = 60_000;
const MS_PER_DAY: i64 = 86_400_000;

/// Current time in milliseconds since the Unix epoch.
pub fn now_ms() -> TimestampMs {
  #[cfg(target_arch = "wasm32")]
  {
    js_sys::Date::now() as TimestampMs
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    1_792_540_800_000 + 12 * 3_600_000
  }
}

/// Day number (days since the epoch) in the local time zone; used to place date dividers.
pub fn local_day(ts: TimestampMs) -> i64 {
  (ts + local_offset_ms(ts)).div_euclid(MS_PER_DAY)
}

/// Midnight in the user's time zone that starts the local day `days_back` days before `ts`.
pub fn local_day_start(ts: TimestampMs, days_back: i64) -> TimestampMs {
  let day = local_day(ts) - days_back;
  // The offset is read around midday of that day, so a daylight saving change at night cannot
  // shift it onto the wrong side.
  let midday = day * MS_PER_DAY + MS_PER_DAY / 2 - local_offset_ms(ts);
  day * MS_PER_DAY - local_offset_ms(midday)
}

/// Offset of the user's time zone from UTC in minutes (east is positive).
pub fn local_utc_offset_minutes(ts: TimestampMs) -> i16 {
  i16::try_from(local_offset_ms(ts) / MS_PER_MINUTE).unwrap_or(0)
}

/// The next time the local clock shows `minute_of_day` on the following day ("tomorrow 08:00").
pub fn tomorrow_at(ts: TimestampMs, minute_of_day: u16) -> TimestampMs {
  let midnight = end_of_local_day(ts);
  let guess = midnight + i64::from(minute_of_day) * MS_PER_MINUTE;
  // A daylight saving change between midnight and then moves the wall clock.
  guess - (local_offset_ms(guess) - local_offset_ms(midnight))
}

/// Start of the next local day (midnight in the user's time zone).
pub fn end_of_local_day(ts: TimestampMs) -> TimestampMs {
  (local_day(ts) + 1) * MS_PER_DAY - local_offset_ms(ts)
}

/// Start of the next local Monday, i.e. the end of the current week.
pub fn end_of_local_week(ts: TimestampMs) -> TimestampMs {
  // 1970-01-01 was a Thursday; Monday is index 0.
  let weekday = (local_day(ts) + 3).rem_euclid(7);
  end_of_local_day(ts) + (6 - weekday) * MS_PER_DAY
}

/// "14:05" in the user's locale and time zone.
pub fn format_time(ts: TimestampMs, locale: &str) -> String {
  #[cfg(target_arch = "wasm32")]
  {
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ts as f64));
    let options = js_sys::Object::new();
    for (key, value) in [("hour", "2-digit"), ("minute", "2-digit")] {
      // Setting a plain data property on a fresh object cannot fail.
      let _ = js_sys::Reflect::set(&options, &key.into(), &value.into());
    }
    String::from(date.to_locale_time_string_with_options(locale, &options))
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    let _ = locale;
    let minutes = ts.div_euclid(MS_PER_MINUTE).rem_euclid(1440);
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
  }
}

/// "14:05:09" in the user's locale and time zone; used by the compact log view.
pub fn format_time_seconds(ts: TimestampMs, locale: &str) -> String {
  #[cfg(target_arch = "wasm32")]
  {
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ts as f64));
    let options = js_sys::Object::new();
    // The locale decides 12 or 24 hours, as for `format_time` (#702). No `hour12` option: set as
    // the string "false" it is truthy and forced a 12-hour clock with AM/PM on every locale.
    for (key, value) in [("hour", "2-digit"), ("minute", "2-digit"), ("second", "2-digit")] {
      // Setting a plain data property on a fresh object cannot fail.
      let _ = js_sys::Reflect::set(&options, &key.into(), &value.into());
    }
    String::from(date.to_locale_time_string_with_options(locale, &options))
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    let _ = locale;
    let seconds = ts.div_euclid(1000).rem_euclid(86_400);
    format!(
      "{:02}:{:02}:{:02}",
      seconds / 3600,
      seconds % 3600 / 60,
      seconds % 60
    )
  }
}

/// "Tuesday, October 21" in the user's locale and time zone, with the year for a day outside the
/// current one: "Wednesday, July 24, 2024" (#1086). The current year is read from the clock on
/// every call, so a label made after New Year's Eve has the year without any timer.
pub fn format_date(ts: TimestampMs, locale: &str) -> String {
  format_date_at(ts, now_ms(), locale)
}

fn format_date_at(ts: TimestampMs, now: TimestampMs, locale: &str) -> String {
  let has_year = shows_year(ts, now);
  #[cfg(target_arch = "wasm32")]
  {
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ts as f64));
    let options = js_sys::Object::new();
    let year = has_year.then_some(("year", "numeric"));
    for (key, value) in [("weekday", "long"), ("month", "long"), ("day", "numeric")]
      .into_iter()
      .chain(year)
    {
      // Setting a plain data property on a fresh object cannot fail.
      let _ = js_sys::Reflect::set(&options, &key.into(), &value.into());
    }
    String::from(date.to_locale_date_string(locale, &options))
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    let _ = locale;
    let day = local_day(ts);
    match has_year {
      true => format!("day {day} of {}", local_year(ts)),
      false => format!("day {day}"),
    }
  }
}

/// Whether a date label for `ts` names its year: only when it is not the year of `now`.
fn shows_year(ts: TimestampMs, now: TimestampMs) -> bool {
  local_year(ts) != local_year(now)
}

/// The calendar year of `ts` in the user's time zone.
fn local_year(ts: TimestampMs) -> i64 {
  civil_from_days(local_day(ts)).0
}

/// "5 minutes ago" / "in 3 days" in the user's locale, relative to `now`.
pub fn format_relative(ts: TimestampMs, now: TimestampMs, locale: &str) -> String {
  format_relative_in(ts, now, locale, "long")
}

/// "5 min. ago" / "in 3 days": the abbreviated form, for tight places like the channel header.
pub fn format_relative_short(ts: TimestampMs, now: TimestampMs, locale: &str) -> String {
  format_relative_in(ts, now, locale, "short")
}

/// `style` is an `Intl.RelativeTimeFormat` style: "long", "short" or "narrow".
fn format_relative_in(ts: TimestampMs, now: TimestampMs, locale: &str, style: &str) -> String {
  let delta = ts - now;
  let (value, unit) = relative_parts(delta);
  #[cfg(target_arch = "wasm32")]
  {
    let options = js_sys::Object::new();
    // Setting a plain data property on a fresh object cannot fail.
    let _ = js_sys::Reflect::set(&options, &"numeric".into(), &"auto".into());
    let _ = js_sys::Reflect::set(&options, &"style".into(), &style.into());
    let formatter = js_sys::Intl::RelativeTimeFormat::new(&js_sys::Array::of1(&locale.into()), &options);
    formatter.format(value as f64, unit).into()
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    let _ = (locale, style);
    if value < 0 {
      format!("{} {unit}s ago", -value)
    } else {
      format!("in {value} {unit}s")
    }
  }
}

/// "21/10/2026, 14:05" in the user's locale and time zone.
pub fn format_date_time(ts: TimestampMs, locale: &str) -> String {
  #[cfg(target_arch = "wasm32")]
  {
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ts as f64));
    let options = js_sys::Object::new();
    for (key, value) in [("dateStyle", "medium"), ("timeStyle", "short")] {
      // Setting a plain data property on a fresh object cannot fail.
      let _ = js_sys::Reflect::set(&options, &key.into(), &value.into());
    }
    date.to_locale_string(locale, &options).into()
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    let _ = locale;
    format!("{} {}", ts.div_euclid(MS_PER_DAY), format_time(ts, "en"))
  }
}

/// Picks the largest unit that keeps the number small: (-5, "minute"), (3, "day").
fn relative_parts(delta_ms: i64) -> (i64, &'static str) {
  const UNITS: [(&str, i64); 4] = [
    ("day", MS_PER_DAY),
    ("hour", 3_600_000),
    ("minute", MS_PER_MINUTE),
    ("second", 1000),
  ];
  for (unit, size) in UNITS {
    if delta_ms.abs() >= size {
      return (delta_ms / size, unit);
    }
  }
  (0, "second")
}

/// Reads the value of an `<input type="datetime-local">` (`2026-10-22T14:30`) as a point in time in
/// the user's time zone.
pub fn parse_local_datetime(text: &str) -> Option<TimestampMs> {
  let (date, time) = text.split_once('T')?;
  let mut date_parts = date.split('-');
  let (year, month, day) = (
    date_parts.next()?.parse::<i64>().ok()?,
    date_parts.next()?.parse::<i64>().ok()?,
    date_parts.next()?.parse::<i64>().ok()?,
  );
  let mut time_parts = time.split(':');
  let (hour, minute) = (
    time_parts.next()?.parse::<i64>().ok()?,
    time_parts.next()?.parse::<i64>().ok()?,
  );
  if !(1..=12).contains(&month)
    || !(1..=31).contains(&day)
    || !(0..24).contains(&hour)
    || !(0..60).contains(&minute)
  {
    return None;
  }
  let as_if_utc = days_from_civil(year, month, day) * MS_PER_DAY + (hour * 60 + minute) * MS_PER_MINUTE;
  // The offset depends on the date (summer time), so ask for it at the date in question.
  Some(as_if_utc - local_offset_ms(as_if_utc))
}

/// The value for an `<input type="datetime-local">` showing `ts` in the user's time zone.
pub fn format_local_datetime_input(ts: TimestampMs) -> String {
  let local = ts + local_offset_ms(ts);
  let (year, month, day) = civil_from_days(local.div_euclid(MS_PER_DAY));
  let minutes = local.rem_euclid(MS_PER_DAY) / MS_PER_MINUTE;
  format!(
    "{year:04}-{month:02}-{day:02}T{:02}:{:02}",
    minutes / 60,
    minutes % 60
  )
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
  let year = if month <= 2 { year - 1 } else { year };
  let era = year.div_euclid(400);
  let year_of_era = year.rem_euclid(400);
  let month_index = (month + 9) % 12;
  let day_of_year = (153 * month_index + 2) / 5 + day - 1;
  let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
  era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
  let days = days + 719_468;
  let era = days.div_euclid(146_097);
  let day_of_era = days.rem_euclid(146_097);
  let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
  let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
  let month_index = (5 * day_of_year + 2) / 153;
  let day = day_of_year - (153 * month_index + 2) / 5 + 1;
  let month = if month_index < 10 {
    month_index + 3
  } else {
    month_index - 9
  };
  let year = year_of_era + era * 400 + i64::from(month <= 2);
  (year, month, day)
}

fn local_offset_ms(ts: TimestampMs) -> i64 {
  #[cfg(target_arch = "wasm32")]
  {
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ts as f64));
    -(date.get_timezone_offset() as i64) * MS_PER_MINUTE
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    let _ = ts;
    0
  }
}

/// The time of day at `ts` in an IANA time zone (`Europe/Berlin`), formatted for `locale`. `None`
/// when the zone is unknown or the platform has no time zone data.
pub fn time_in_zone(ts: TimestampMs, zone: &str, locale: &str) -> Option<String> {
  #[cfg(target_arch = "wasm32")]
  {
    use wasm_bindgen::{JsCast, JsValue};
    let options = js_sys::Object::new();
    for (key, value) in [("hour", "2-digit"), ("minute", "2-digit"), ("timeZone", zone)] {
      js_sys::Reflect::set(&options, &key.into(), &value.into()).ok()?;
    }
    // An unknown zone makes the constructor throw a RangeError; building it through `Reflect` turns
    // that into a plain `None` (js-sys' own constructor would abort).
    let global = js_sys::global();
    let intl = js_sys::Reflect::get(&global, &"Intl".into()).ok()?;
    let constructor: js_sys::Function = js_sys::Reflect::get(&intl, &"DateTimeFormat".into())
      .ok()?
      .dyn_into()
      .ok()?;
    let args = js_sys::Array::of2(&JsValue::from_str(locale), &options);
    let format = js_sys::Reflect::construct(&constructor, &args).ok()?;
    let format_fn: js_sys::Function = js_sys::Reflect::get(&format, &"format".into())
      .ok()?
      .dyn_into()
      .ok()?;
    let date = js_sys::Date::new(&JsValue::from_f64(ts as f64));
    format_fn.call1(&format, &date).ok()?.as_string()
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    let _ = (ts, zone, locale);
    None
  }
}

/// The time zone the browser or system uses, as an IANA name.
pub fn local_time_zone() -> Option<String> {
  #[cfg(target_arch = "wasm32")]
  {
    use wasm_bindgen::JsCast;
    let intl = js_sys::Reflect::get(&js_sys::global(), &"Intl".into()).ok()?;
    let constructor: js_sys::Function = js_sys::Reflect::get(&intl, &"DateTimeFormat".into())
      .ok()?
      .dyn_into()
      .ok()?;
    let format = js_sys::Reflect::construct(&constructor, &js_sys::Array::new()).ok()?;
    let resolve: js_sys::Function = js_sys::Reflect::get(&format, &"resolvedOptions".into())
      .ok()?
      .dyn_into()
      .ok()?;
    let resolved = resolve.call0(&format).ok()?;
    js_sys::Reflect::get(&resolved, &"timeZone".into())
      .ok()?
      .as_string()
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    None
  }
}

/// IANA time zone names the platform knows, for the profile form.
pub fn time_zone_names() -> Vec<String> {
  #[cfg(target_arch = "wasm32")]
  {
    use wasm_bindgen::JsCast;
    let names = || -> Option<Vec<String>> {
      let intl = js_sys::Reflect::get(&js_sys::global(), &"Intl".into()).ok()?;
      let supported: js_sys::Function = js_sys::Reflect::get(&intl, &"supportedValuesOf".into())
        .ok()?
        .dyn_into()
        .ok()?;
      let list = supported.call1(&intl, &"timeZone".into()).ok()?;
      Some(
        js_sys::Array::from(&list)
          .iter()
          .filter_map(|name| name.as_string())
          .collect(),
      )
    };
    names().unwrap_or_default()
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    Vec::new()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn tomorrow_morning_is_on_the_next_day() {
    let afternoon = 1_792_828_800_000 + 7 * 3_600_000;
    assert_eq!(tomorrow_at(afternoon, 8 * 60), 1_792_828_800_000 + 24 * 3_600_000);
  }

  #[test]
  fn day_starts_are_midnights_counted_back_from_today() {
    let afternoon = 1_792_828_800_000 + 7 * 3_600_000;
    let today = local_day_start(afternoon, 0);
    assert_eq!(local_day(today), local_day(afternoon));
    assert!(today <= afternoon && afternoon - today < MS_PER_DAY);
    assert_eq!(local_day(local_day_start(afternoon, 1)), local_day(afternoon) - 1);
    assert_eq!(local_day(local_day_start(afternoon, 7)), local_day(afternoon) - 7);
  }

  mod date_labels {
    use super::*;

    #[test]
    fn a_day_of_the_current_year_has_no_year() {
      let now = utc_ms("2026-10-22T14:30");
      assert!(!shows_year(utc_ms("2026-01-01T00:00"), now));
      assert!(!shows_year(utc_ms("2026-07-24T09:00"), now));
      assert_eq!(format_date_at(now, now, "en"), format!("day {}", local_day(now)));
    }

    #[test]
    fn a_day_of_the_previous_year_has_its_year() {
      let now = utc_ms("2026-10-22T14:30");
      let then = utc_ms("2024-07-24T09:00");
      assert!(shows_year(utc_ms("2025-12-31T23:59"), now));
      assert_eq!(
        format_date_at(then, now, "en"),
        format!("day {} of 2024", local_day(then))
      );
    }

    #[test]
    fn a_day_of_a_future_year_has_its_year() {
      let now = utc_ms("2026-10-22T14:30");
      assert!(shows_year(utc_ms("2027-01-01T00:00"), now));
    }

    #[test]
    fn new_years_eve_gains_its_year_at_midnight() {
      let eve = utc_ms("2026-12-31T23:59");
      assert!(!shows_year(eve, utc_ms("2026-12-31T00:00")));
      assert!(shows_year(eve, utc_ms("2027-01-01T00:00")));
      assert!(shows_year(utc_ms("2027-01-01T00:00"), eve));
    }

    /// The native build's clock is UTC, so a local date-time input is a UTC instant.
    fn utc_ms(text: &str) -> TimestampMs {
      parse_local_datetime(text).expect("valid date-time")
    }
  }

  #[test]
  fn relative_units_scale_with_distance() {
    assert_eq!(relative_parts(-5 * MS_PER_MINUTE), (-5, "minute"));
    assert_eq!(relative_parts(-3 * MS_PER_DAY), (-3, "day"));
    assert_eq!(relative_parts(2 * 3_600_000), (2, "hour"));
    assert_eq!(relative_parts(400), (0, "second"));
  }

  #[test]
  fn datetime_inputs_round_trip() {
    for text in [
      "2026-10-22T14:30",
      "1970-01-01T00:00",
      "2024-02-29T23:59",
      "2000-03-01T00:01",
    ] {
      let ts = parse_local_datetime(text).expect("parses");
      assert_eq!(format_local_datetime_input(ts), text);
    }
    assert_eq!(parse_local_datetime("1970-01-02T00:00"), Some(MS_PER_DAY));
  }

  #[test]
  fn malformed_datetime_inputs_are_rejected() {
    for bad in [
      "",
      "2026-10-22",
      "2026-13-01T10:00",
      "2026-10-22T24:00",
      "2026-10-22T10:60",
      "x-y-zT10:00",
    ] {
      assert_eq!(parse_local_datetime(bad), None, "{bad}");
    }
  }

  #[test]
  fn native_fallback_formats_utc() {
    assert_eq!(format_time(14 * 3_600_000 + 5 * MS_PER_MINUTE, "en"), "14:05");
    assert_eq!(local_day(MS_PER_DAY + 1), 1);
    assert_eq!(local_day(-1), -1);
  }
}
