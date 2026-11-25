//! Reports the page's thread being held up (#2397). A web page whose thread is blocked for long
//! stops reacting to clicks and scrolling, and left no trace before: no error, no crash, nothing in
//! GlitchTip. The browser's own record of long tasks (`long-animation-frame` where it has it, else
//! `longtask`) names them; one of at least [`REPORTED_FROM_MS`] goes out as a handled warning with
//! the redacted debug log, which says what the app was doing (crash-report consent and scrubbing
//! as for every report, docs/crash-reports.md). Shorter ones are the measurements' business
//! (`tests/e2e` `perf.rs`, docs/performance.md).

use std::cell::Cell;

use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::Closure;
use web_sys::{PerformanceObserver, PerformanceObserverEntryList};

/// A task at least this long is reported: the page stood still for half a second.
const REPORTED_FROM_MS: f64 = 500.0;
/// Reports per page load at most: one frozen start shouldn't send dozens.
const MAX_REPORTS: u32 = 3;
/// The entry types asked for, the more telling first.
const ENTRY_TYPES: [&str; 2] = ["long-animation-frame", "longtask"];

thread_local! {
  static REPORTS: Cell<u32> = const { Cell::new(0) };
}

/// Starts watching the page's long tasks; a browser that records neither entry type isn't watched.
pub fn install() {
  let Some(entry_type) = ENTRY_TYPES
    .into_iter()
    .find(|entry_type| is_supported(entry_type))
  else {
    return;
  };
  let on_entries =
    Closure::<dyn FnMut(PerformanceObserverEntryList)>::new(|list: PerformanceObserverEntryList| {
      for entry in list.get_entries().iter() {
        if let Ok(entry) = entry.dyn_into::<web_sys::PerformanceEntry>() {
          note(entry.duration());
        }
      }
    });
  let Ok(observer) = PerformanceObserver::new(on_entries.as_ref().unchecked_ref()) else {
    return;
  };
  // `{ type, buffered }`, which web-sys's `PerformanceObserverInit` (only `entryTypes`) lacks:
  // `buffered` takes the long tasks of the start, before this ran.
  let options = js_sys::Object::new();
  let is_set = js_sys::Reflect::set(&options, &"type".into(), &entry_type.into()).is_ok()
    && js_sys::Reflect::set(&options, &"buffered".into(), &JsValue::TRUE).is_ok();
  let observe = js_sys::Reflect::get(&observer, &"observe".into())
    .ok()
    .and_then(|observe| observe.dyn_into::<js_sys::Function>().ok());
  if let Some(observe) = observe.filter(|_| is_set)
    && observe.call1(&observer, &options).is_ok()
  {
    // Lives as long as the page.
    on_entries.forget();
  }
}

/// Whether the browser records `entry_type` (`PerformanceObserver.supportedEntryTypes`).
fn is_supported(entry_type: &str) -> bool {
  js_sys::Reflect::get(&js_sys::global(), &"PerformanceObserver".into())
    .and_then(|class| js_sys::Reflect::get(&class, &"supportedEntryTypes".into()))
    .ok()
    .filter(|types| types.is_object())
    .map(|types| js_sys::Array::from(&types))
    .is_some_and(|types| types.includes(&entry_type.into(), 0))
}

fn note(duration_ms: f64) {
  if duration_ms < REPORTED_FROM_MS {
    return;
  }
  let is_due = REPORTS.with(|reports| {
    let sent = reports.get();
    reports.set(sent.saturating_add(1));
    sent < MAX_REPORTS
  });
  if is_due {
    // In steps, so the same kind of freeze groups as one issue whatever its exact length.
    let step = match duration_ms {
      ms if ms >= 10_000.0 => "over 10 s",
      ms if ms >= 2_000.0 => "over 2 s",
      _ => "over 0.5 s",
    };
    crate::crash_reports::capture_handled(
      "main_thread_blocked",
      format!("the page's thread was busy for {step} at once"),
    );
  }
}
