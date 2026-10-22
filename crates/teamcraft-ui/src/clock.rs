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
    for (key, value) in [
      ("hour", "2-digit"),
      ("minute", "2-digit"),
      ("second", "2-digit"),
      ("hour12", "false"),
    ] {
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

/// "Tuesday, October 21" in the user's locale and time zone.
pub fn format_date(ts: TimestampMs, locale: &str) -> String {
  #[cfg(target_arch = "wasm32")]
  {
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ts as f64));
    let options = js_sys::Object::new();
    for (key, value) in [("weekday", "long"), ("month", "long"), ("day", "numeric")] {
      // Setting a plain data property on a fresh object cannot fail.
      let _ = js_sys::Reflect::set(&options, &key.into(), &value.into());
    }
    String::from(date.to_locale_date_string(locale, &options))
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    let _ = locale;
    format!("day {}", ts.div_euclid(MS_PER_DAY))
  }
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn native_fallback_formats_utc() {
    assert_eq!(format_time(14 * 3_600_000 + 5 * MS_PER_MINUTE, "en"), "14:05");
    assert_eq!(local_day(MS_PER_DAY + 1), 1);
    assert_eq!(local_day(-1), -1);
  }
}
