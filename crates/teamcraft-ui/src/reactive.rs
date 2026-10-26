//! Helpers for reading signals from code that runs both inside and outside reactive scopes.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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

/// Whether the reactive owner current at creation (usually the signed-in shell, or a component) is
/// still alive. Listeners that live as long as the page (window and document events, service
/// worker messages, desktop shell events) check it: after a sign-out the next shell registers its
/// own, and the old ones must go quiet instead of reaching their disposed signals (a Back press
/// after signing in again panicked in the old shell's `popstate` handler). Always alive without an
/// owner.
#[derive(Clone)]
pub struct OwnerLiveness(Arc<AtomicBool>);

impl OwnerLiveness {
  pub fn of_current_owner() -> Self {
    let is_alive = Arc::new(AtomicBool::new(true));
    let flag = is_alive.clone();
    on_cleanup(move || flag.store(false, Ordering::Relaxed));
    Self(is_alive)
  }

  pub fn is_alive(&self) -> bool {
    self.0.load(Ordering::Relaxed)
  }
}

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

/// Whether the page is visible right now (not reactive; see [`use_page_visible`]).
pub fn is_document_visible() -> bool {
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
pub fn use_debounced(source: impl Into<Signal<String>>, delay_ms: u64) -> ReadSignal<String> {
  let source = source.into();
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

/// The items of a list by key, for rows that are built once and then follow their item: a `<For>`
/// keyed by id alone keeps its rows, and each row reads its current item with [`keyed_item`].
/// Keying a `<For>` by what a row shows instead builds the row anew (its pictures and animations
/// included) whenever that changes; so does a list rendered inside a closure (issue #687).
pub fn items_by_key<K, T>(list: Signal<Vec<T>>, key_of: fn(&T) -> K) -> Memo<HashMap<K, T>>
where
  K: Eq + Hash + Send + Sync + 'static,
  T: Clone + PartialEq + Send + Sync + 'static,
{
  Memo::new(move |_| list.with(|items| items.iter().map(|item| (key_of(item), item.clone())).collect()))
}

/// One row's item of [`items_by_key`]: changes only when that item does. Once the key left the list
/// the row is about to go, and keeps showing its last item until then.
pub fn keyed_item<K, T>(items: Memo<HashMap<K, T>>, key: K, initial: T) -> Memo<T>
where
  K: Eq + Hash + Send + Sync + 'static,
  T: Clone + PartialEq + Send + Sync + 'static,
{
  Memo::new(move |previous: Option<&T>| {
    items
      .with(|items| items.get(&key).cloned())
      .or_else(|| previous.cloned())
      .unwrap_or_else(|| initial.clone())
  })
}

/// Waits on the page's event loop, without holding up anything else.
pub async fn pause(duration: std::time::Duration) {
  let (done, wait) = futures_channel::oneshot::channel::<()>();
  leptos::prelude::set_timeout(
    move || {
      // The waiter may be gone (its view closed): nothing to tell then.
      let _ = done.send(());
    },
    duration,
  );
  let _ = wait.await;
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

  /// A Back press after signing in again reached the signed-out shell's `popstate` handler.
  #[test]
  fn page_listeners_go_quiet_with_their_owner() {
    let shell = Owner::new();
    let (liveness, shown) = shell.with(|| (OwnerLiveness::of_current_owner(), StoredValue::new(1)));
    let on_popstate = move || {
      if liveness.is_alive() {
        shown.set_value(2);
      }
    };
    on_popstate();
    assert_eq!(shown.try_get_value(), Some(2));
    shell.cleanup();
    on_popstate();
    assert_eq!(shown.try_get_value(), None, "disposed, and not touched");
    assert!(
      OwnerLiveness::of_current_owner().is_alive(),
      "without an owner: always"
    );
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
