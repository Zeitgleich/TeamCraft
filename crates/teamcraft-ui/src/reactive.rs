//! Helpers for reading signals from code that runs both inside and outside reactive scopes.

use leptos::prelude::*;
use leptos::reactive::graph::Observer;

/// Reads a signal, subscribing the caller only when there is one. Component bodies and event handlers
/// have no observer; reading there is intentional and must not trigger the debug-build warning.
pub fn get_if_observed<T: Clone + Send + Sync + 'static>(signal: RwSignal<T>) -> T {
  if Observer::get().is_some() {
    signal.get()
  } else {
    signal.get_untracked()
  }
}

/// Runs `callback` every `interval_ms` until the current scope is dropped.
pub fn use_interval(interval_ms: u64, callback: impl Fn() + 'static) {
  use std::time::Duration;

  use leptos::leptos_dom::helpers::set_interval_with_handle;

  match set_interval_with_handle(callback, Duration::from_millis(interval_ms)) {
    Ok(handle) => on_cleanup(move || handle.clear()),
    Err(_) => tracing::debug!("could not start interval"),
  }
}

/// Like [`use_interval`], but only while the reactive condition `is_active` holds (and once right
/// when it starts to): an idle page shouldn't wake up every second for something nobody sees.
pub fn use_interval_while(
  interval_ms: u64,
  is_active: impl Fn() -> bool + 'static,
  callback: impl Fn() + Clone + 'static,
) {
  use std::time::Duration;

  use leptos::leptos_dom::helpers::{IntervalHandle, set_interval_with_handle};

  let handle = StoredValue::new(None::<IntervalHandle>);
  Effect::new(move |_| {
    let should_run = is_active();
    let is_running = handle.with_value(Option::is_some);
    if should_run && !is_running {
      callback();
      match set_interval_with_handle(callback.clone(), Duration::from_millis(interval_ms)) {
        Ok(started) => handle.set_value(Some(started)),
        Err(_) => tracing::debug!("could not start interval"),
      }
    } else if !should_run && let Some(running) = handle.get_value() {
      running.clear();
      handle.set_value(None);
    }
  });
  on_cleanup(move || {
    if let Some(running) = handle.try_get_value().flatten() {
      running.clear();
    }
  });
}
