//! Keeps the address bar and the history in step with the app, both ways (issue #235).
//!
//! - On start and on Back/Forward (`popstate`) the URL's [`Route`] is shown; what hasn't loaded
//!   yet is waited for, anything unknown or not allowed falls back to Home with a notice.
//! - Whenever the app goes somewhere, the URL follows: `pushState` for a new place,
//!   `replaceState` for the same place spelled differently, another settings tab or a redirect.
//! - Closing the settings dialog goes back to the entry it was opened over, so it leaves no
//!   extra entry behind.
//! - The desktop app uses the same paths inside its webview. Its webviews don't all map the mouse's
//!   back/forward buttons and Alt+←/→ to the history, so the app does that itself there.
//!
//! Own module instead of `leptos_router`: navigation state lives in `AppState` signals that many
//! components set directly, and overlays (settings) have routes too. The router would have to
//! become the source of truth for all of that; this keeps the state as it is and mirrors it.

use std::time::Duration;

use leptos::prelude::*;
use teamcraft_client_api::remote::DEEP_LINK_EVENT;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;

use crate::auth::tauri;
use crate::components::app::on_window;
use crate::i18n::I18n;
use crate::route::{HistoryStep, Route};
use crate::state::{AppState, RouteOutcome};

/// How long a URL that names something not loaded yet is waited for before Home shows instead.
const ROUTE_WAIT: Duration = Duration::from_secs(20);
/// `MouseEvent.button` of the mouse's back and forward buttons.
const MOUSE_BACK: i16 = 3;
const MOUSE_FORWARD: i16 = 4;

/// A step through the history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Travel {
  Back,
  Forward,
}

/// What the address bar shows and where closing the settings dialog returns to.
#[derive(Clone, Copy)]
struct AddressBar {
  shown: StoredValue<Route>,
  /// The page the settings dialog was opened over, while its entry is the one below.
  maybe_below_settings: StoredValue<Option<Route>>,
  /// The app's route when a waiting route was given; going elsewhere meanwhile drops the wait.
  maybe_waiting_on: StoredValue<Option<Route>>,
}

/// Shows the URL's place and keeps the URL in step from now on.
pub fn start(state: AppState, i18n: I18n) {
  let bar = AddressBar {
    shown: StoredValue::new(Route::Start),
    maybe_below_settings: StoredValue::new(None),
    maybe_waiting_on: StoredValue::new(None),
  };
  // Following a URL reads and sets state outside any reactive scope, like an event handler does.
  untrack(|| follow_location(state, bar));
  let route = Memo::new(move |_| state.route());
  Effect::new(move |previous: Option<()>| {
    let next = route.get();
    let is_waiting = state.pending_route.get().is_some();
    sync_address_bar(state, bar, next, is_waiting, previous.is_none());
  });
  Effect::new(move |previous: Option<u64>| {
    let count = state.unavailable_routes.get();
    if count > previous.unwrap_or(0) {
      state.show_toast(i18n.t("route-unavailable"));
    }
    count
  });
  on_window("popstate", move || untrack(|| follow_location(state, bar)));
  if tauri::is_available() {
    handle_mouse_buttons();
    tauri::listen_text(DEEP_LINK_EVENT, move |link| {
      if let Some(route) = Route::from_permalink(&link) {
        untrack(|| follow(state, bar, route));
      }
    });
  }
}

/// One step back or forward, as the browser's own buttons do.
pub fn travel(travel: Travel) {
  let Some(history) = web_sys::window().and_then(|window| window.history().ok()) else {
    return;
  };
  let result = match travel {
    Travel::Back => history.back(),
    Travel::Forward => history.forward(),
  };
  if result.is_err() {
    tracing::debug!(?travel, "could not move through the history");
  }
}

/// Shows the route of the current URL (start, Back, Forward).
fn follow_location(state: AppState, bar: AddressBar) {
  let path = web_sys::window()
    .and_then(|window| window.location().pathname().ok())
    .unwrap_or_else(|| "/".to_owned());
  let Some(route) = Route::parse(&path) else {
    bar.shown.set_value(Route::Start);
    state.reject_route();
    return;
  };
  bar.shown.set_value(route);
  if route == state.route_untracked() {
    return;
  }
  follow(state, bar, route);
}

fn follow(state: AppState, bar: AddressBar, route: Route) {
  bar.maybe_waiting_on.set_value(None);
  if state.follow_route(route) == RouteOutcome::Waiting {
    bar.maybe_waiting_on.set_value(Some(state.route_untracked()));
    set_timeout(move || untrack(|| state.give_up_route(route)), ROUTE_WAIT);
  }
}

/// Moves the address bar to `next`, the app's current route.
fn sync_address_bar(state: AppState, bar: AddressBar, next: Route, is_waiting: bool, is_first: bool) {
  let mut is_redirect = state.take_follows_url() || is_first;
  if is_waiting {
    // The URL keeps naming what is being waited for, unless the user went somewhere else.
    let is_still_there = bar
      .maybe_waiting_on
      .get_value()
      .is_some_and(|waiting_on| waiting_on.is_same_page(&next));
    if is_still_there {
      return;
    }
    state.cancel_pending_route();
    is_redirect = true;
  }
  let shown = bar.shown.get_value();
  if matches!(shown, Route::Settings(_))
    && !matches!(next, Route::Settings(_))
    && bar.maybe_below_settings.get_value() == Some(next)
  {
    bar.maybe_below_settings.set_value(None);
    bar.shown.set_value(next);
    travel(Travel::Back);
    return;
  }
  let step = HistoryStep::between(&shown, &next, is_redirect);
  if step == HistoryStep::Push {
    let below_settings =
      (matches!(next, Route::Settings(_)) && !matches!(shown, Route::Settings(_))).then_some(shown);
    bar.maybe_below_settings.set_value(below_settings);
  }
  write_address_bar(step, &next.path());
  bar.shown.set_value(next);
}

fn write_address_bar(step: HistoryStep, path: &str) {
  let Some(history) = web_sys::window().and_then(|window| window.history().ok()) else {
    return;
  };
  let result = match step {
    HistoryStep::Keep => return,
    HistoryStep::Push => history.push_state_with_url(&JsValue::NULL, "", Some(path)),
    HistoryStep::Replace => history.replace_state_with_url(&JsValue::NULL, "", Some(path)),
  };
  if result.is_err() {
    tracing::debug!(?step, "could not update the address bar");
  }
}

/// Desktop: the mouse's back and forward buttons move through the history. The webview's own
/// handling (where there is one) is cancelled, so a click never moves twice.
fn handle_mouse_buttons() {
  let Some(window) = web_sys::window() else { return };
  let listener = Closure::<dyn Fn(web_sys::MouseEvent)>::new(|event: web_sys::MouseEvent| {
    let travel_to = match event.button() {
      MOUSE_BACK => Travel::Back,
      MOUSE_FORWARD => Travel::Forward,
      _ => return,
    };
    event.prevent_default();
    travel(travel_to);
  });
  if window
    .add_event_listener_with_callback("mouseup", listener.as_ref().unchecked_ref())
    .is_ok()
  {
    // Lives as long as the page.
    listener.forget();
  }
}
