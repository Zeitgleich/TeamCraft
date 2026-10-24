//! Helpers for reading signals from code that runs both inside and outside reactive scopes.

use leptos::prelude::*;
use leptos::reactive::graph::Observer;

/// Writes from async tasks and timers (after an `.await`, in a callback of the core), which can run
/// after the component that owns the signal is gone. Plain `set`/`update` panic then ("reactive
/// value already disposed"), which kills the wasm app (issue #322); these do nothing instead.
/// `scripts/check-async-signals.py` makes sure spawned tasks use them.
pub trait SetIfAlive: Set {
  fn set_if_alive(&self, value: Self::Value) {
    // A disposed signal hands the value back; nobody is left to show it.
    let _ = self.try_set(value);
  }
}

impl<S: Set> SetIfAlive for S {}

/// See [`SetIfAlive`].
pub trait UpdateIfAlive: Update {
  fn update_if_alive(&self, change: impl FnOnce(&mut Self::Value)) {
    let _ = self.try_update(change);
  }
}

impl<S: Update> UpdateIfAlive for S {}

/// Reads a signal, subscribing the caller only when there is one. Component bodies and event handlers
/// have no observer; reading there is intentional and must not trigger the debug-build warning.
pub fn get_if_observed<T: Clone + Send + Sync + 'static>(signal: RwSignal<T>) -> T {
  if Observer::get().is_some() {
    signal.get()
  } else {
    signal.get_untracked()
  }
}

/// Whether the page is visible: the browser tab in front, the desktop window not minimized or
/// hidden to the tray (the webview reports both as `visibilitychange`). Timers whose result only
/// shows on screen pause while it is hidden.
pub fn use_page_visible() -> Signal<bool> {
  let is_visible = RwSignal::new(is_document_visible());
  // `visibilitychange` bubbles from the document to the window.
  let handle = window_event_listener_untyped("visibilitychange", move |_| {
    is_visible.set(is_document_visible());
  });
  on_cleanup(move || handle.remove());
  is_visible.into()
}

/// Like [`use_interval_while`], for timers whose result only shows on screen: they pause while the
/// page is hidden and run once right when it becomes visible again.
pub fn use_interval_while_visible(interval_ms: u64, callback: impl Fn() + Clone + 'static) {
  let is_visible = use_page_visible();
  use_interval_while(interval_ms, move || is_visible.get(), callback);
}

fn is_document_visible() -> bool {
  web_sys::window()
    .and_then(|window| window.document())
    .is_none_or(|document| !document.hidden())
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

/// A copy of `source` that follows it once it stopped changing for `delay_ms`: a search box asks
/// the server when the user paused typing, not on every key.
pub fn use_debounced(source: RwSignal<String>, delay_ms: u64) -> ReadSignal<String> {
  let (value, set_value) = signal(source.get_untracked());
  let generation = StoredValue::new(0_u64);
  Effect::new(move |_| {
    let next = source.get();
    generation.update_value(|current| *current += 1);
    let mine = generation.get_value();
    set_timeout(
      move || {
        // A newer key press restarted the wait; a closed view ignores it (`try_*`).
        if generation.try_get_value() == Some(mine) {
          let _ = set_value.try_set(next);
        }
      },
      std::time::Duration::from_millis(delay_ms),
    );
  });
  value
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Issue #322: an upload or a core answer finishes after its component was unmounted.
  #[test]
  fn writes_after_the_owner_is_gone_do_nothing() {
    let owner = Owner::new();
    let (value, list) = owner.with(|| (RwSignal::new(1), RwSignal::new(Vec::<u32>::new())));
    owner.cleanup();
    value.set_if_alive(2);
    list.update_if_alive(|list| list.push(3));
    assert_eq!(value.try_get_untracked(), None);
  }

  #[test]
  fn writes_while_the_owner_lives_go_through() {
    let owner = Owner::new();
    owner.with(|| {
      let list = RwSignal::new(Vec::<u32>::new());
      list.update_if_alive(|list| list.push(3));
      list.set_if_alive(vec![4]);
      assert_eq!(list.get_untracked(), vec![4]);
    });
  }
}
