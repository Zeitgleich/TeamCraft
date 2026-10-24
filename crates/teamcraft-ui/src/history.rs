//! Keeps the address bar and the history in step with the app, both ways (issue #235).
//!
//! - On start and on Back/Forward (`popstate`) the URL's [`Route`] is shown; what hasn't loaded
//!   yet is waited for, anything unknown or not allowed falls back to Home with a notice.
//! - Whenever the app goes somewhere, the URL follows: `pushState` for a new place,
//!   `replaceState` for the same place spelled differently, another settings tab or a redirect.
//! - Closing the settings dialog goes back to the entry it was opened over, so it leaves no
//!   extra entry behind.
//! - The desktop app uses the same paths inside its webview. Its webviews don't all map the mouse's
//!   back/forward buttons and Alt+←/→ to the history, so the app does that itself there, and
//!   makes sure a click never moves twice (`handle_mouse_buttons`).
//!
//! Own module instead of `leptos_router`: navigation state lives in `AppState` signals that many
//! components set directly, and overlays (settings) have routes too. The router would have to
//! become the source of truth for all of that; this keeps the state as it is and mirrors it.

use std::cell::Cell;
use std::rc::Rc;
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

/// Desktop: the mouse's back and forward buttons move through the history, exactly one step per
/// click. Webviews differ in what they do on their own: WebView2 (Chromium) goes back on the
/// release unless the page cancels it, WebKitGTK builds may go back on the press or not at all. So
/// the default action is cancelled on press, release and `auxclick`, the app moves on the release,
/// and if the webview moved on the press anyway (a `popstate` in between) the app doesn't move again.
fn handle_mouse_buttons() {
  let Some(window) = web_sys::window() else { return };
  let press = Rc::new(Cell::new(MouseButtonPress::Idle));
  let on_press = press.clone();
  listen_mouse(&window, "mousedown", move |travel_to, event| {
    event.prevent_default();
    on_press.set(MouseButtonPress::Pressed(travel_to));
  });
  let on_release = press.clone();
  listen_mouse(&window, "mouseup", move |travel_to, event| {
    event.prevent_default();
    let (next, maybe_travel) = on_release.get().release(travel_to);
    on_release.set(next);
    if let Some(travel_to) = maybe_travel {
      travel(travel_to);
    }
  });
  listen_mouse(&window, "auxclick", |_, event| event.prevent_default());
  on_window("popstate", move || press.set(press.get().history_moved()));
}

/// Calls `handler` for the mouse's back and forward buttons only.
fn listen_mouse(
  window: &web_sys::Window,
  event: &str,
  handler: impl Fn(Travel, &web_sys::MouseEvent) + 'static,
) {
  let listener = Closure::<dyn Fn(web_sys::MouseEvent)>::new(move |event: web_sys::MouseEvent| {
    if let Some(travel_to) = mouse_travel(event.button()) {
      handler(travel_to, &event);
    }
  });
  if window
    .add_event_listener_with_callback(event, listener.as_ref().unchecked_ref())
    .is_ok()
  {
    // Lives as long as the page.
    listener.forget();
  }
}

fn mouse_travel(button: i16) -> Option<Travel> {
  match button {
    MOUSE_BACK => Some(Travel::Back),
    MOUSE_FORWARD => Some(Travel::Forward),
    _ => None,
  }
}

/// Where a click of the back or forward button stands, between its press and its release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MouseButtonPress {
  Idle,
  Pressed(Travel),
  /// The history moved while the button was down: the webview went there on its own.
  MovedByWebview,
}

impl MouseButtonPress {
  /// A `popstate` arrived.
  fn history_moved(self) -> Self {
    match self {
      Self::Pressed(_) => Self::MovedByWebview,
      other => other,
    }
  }

  /// The button was released: the next state and the step the app takes, if any. A release
  /// without a press seen (a webview that doesn't report it) still moves once.
  fn release(self, travel_to: Travel) -> (Self, Option<Travel>) {
    match self {
      Self::MovedByWebview => (Self::Idle, None),
      Self::Idle | Self::Pressed(_) => (Self::Idle, Some(travel_to)),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_click_moves_once() {
    let (next, maybe_travel) = MouseButtonPress::Pressed(Travel::Back).release(Travel::Back);
    assert_eq!((next, maybe_travel), (MouseButtonPress::Idle, Some(Travel::Back)));
  }

  #[test]
  fn a_webview_that_moved_on_the_press_gets_no_second_step() {
    let press = MouseButtonPress::Pressed(Travel::Forward).history_moved();
    assert_eq!(press.release(Travel::Forward), (MouseButtonPress::Idle, None));
  }

  #[test]
  fn history_moves_without_a_press_change_nothing() {
    assert_eq!(MouseButtonPress::Idle.history_moved(), MouseButtonPress::Idle);
  }

  #[test]
  fn a_release_without_a_press_still_moves() {
    assert_eq!(
      MouseButtonPress::Idle.release(Travel::Back),
      (MouseButtonPress::Idle, Some(Travel::Back))
    );
  }

  #[test]
  fn only_the_back_and_forward_buttons_travel() {
    assert_eq!(mouse_travel(MOUSE_BACK), Some(Travel::Back));
    assert_eq!(mouse_travel(MOUSE_FORWARD), Some(Travel::Forward));
    for button in [0, 1, 2, 5] {
      assert_eq!(mouse_travel(button), None);
    }
  }
}
