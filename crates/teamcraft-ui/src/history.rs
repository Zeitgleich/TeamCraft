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
use crate::clock::now_ms;
use crate::components::app::on_window;
use crate::i18n::I18n;
use crate::model::TimestampMs;
use crate::route::{HistoryStep, Route};
use crate::state::{AppState, RouteOutcome};

/// How long a URL that names something not loaded yet is waited for before Home shows instead.
const ROUTE_WAIT: Duration = Duration::from_secs(20);
/// `MouseEvent.button` of the mouse's back and forward buttons.
const MOUSE_BACK: i16 = 3;
const MOUSE_FORWARD: i16 = 4;
/// How long after the release of a mouse back/forward click the webview's own step is waited for
/// before the app takes it.
const WEBVIEW_TRAVEL_GRACE: Duration = Duration::from_millis(100);

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

/// Goes back to the view before the current one when that entry is the app's own, otherwise Home.
/// A page opened by a link (or a fresh tab) has nothing in-app to go back to.
pub fn back_or_home(state: AppState) {
  if depth_of_current_entry() > 0 {
    travel(Travel::Back);
  } else {
    state.open_home();
  }
}

/// How many entries the app itself pushed before the current one, kept in the entry's state;
/// `0` for the entry the page was opened on.
fn depth_of_current_entry() -> u32 {
  web_sys::window()
    .and_then(|window| window.history().ok())
    .and_then(|history| history.state().ok())
    .and_then(|value| value.as_f64())
    .map_or(0, |depth| depth as u32)
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
  let depth = depth_of_current_entry();
  let result = match step {
    HistoryStep::Keep => return,
    HistoryStep::Push => {
      history.push_state_with_url(&JsValue::from_f64(f64::from(depth + 1)), "", Some(path))
    },
    HistoryStep::Replace => {
      history.replace_state_with_url(&JsValue::from_f64(f64::from(depth)), "", Some(path))
    },
  };
  if result.is_err() {
    tracing::debug!(?step, "could not update the address bar");
  }
}

/// Desktop: the mouse's back and forward buttons move through the history, exactly one step per
/// click. Webviews differ in what they do on their own: WebView2 (Chromium) goes back on the
/// release unless the page cancels it, WebKitGTK builds may go back on the press or not at all, and
/// whether `preventDefault` stops them varies. So the default action is cancelled on press, release
/// and `auxclick`, and the app takes its own step [`WEBVIEW_TRAVEL_GRACE`] after the release, only
/// if no `popstate` came since the press (the webview moved on its own).
fn handle_mouse_buttons() {
  let Some(window) = web_sys::window() else { return };
  let clicks = Rc::new(Cell::new(MouseClicks::default()));
  let on_press = clicks.clone();
  listen_mouse(&window, "mousedown", move |travel_to, event| {
    event.prevent_default();
    update(&on_press, |clicks| clicks.press(travel_to));
  });
  let on_release = clicks.clone();
  listen_mouse(&window, "mouseup", move |travel_to, event| {
    event.prevent_default();
    let now = now_ms();
    if let Some(earlier) = update(&on_release, |clicks| clicks.release(travel_to, now)) {
      travel(earlier);
    }
    let deadline = now + grace_ms();
    let on_grace_end = on_release.clone();
    set_timeout(
      move || {
        if let Some(travel_to) = update(&on_grace_end, |clicks| clicks.settle(deadline)) {
          travel(travel_to);
        }
      },
      WEBVIEW_TRAVEL_GRACE,
    );
  });
  listen_mouse(&window, "auxclick", |_, event| event.prevent_default());
  on_window("popstate", move || {
    let now = now_ms();
    update(&clicks, |clicks| clicks.history_moved(now));
  });
}

fn update<T>(clicks: &Cell<MouseClicks>, change: impl FnOnce(&mut MouseClicks) -> T) -> T {
  let mut next = clicks.get();
  let result = change(&mut next);
  clicks.set(next);
  result
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

fn grace_ms() -> TimestampMs {
  TimestampMs::try_from(WEBVIEW_TRAVEL_GRACE.as_millis()).unwrap_or(TimestampMs::MAX)
}

/// The clicks of the back and forward buttons and the app's own steps for them. Every method that
/// returns a [`Travel`] means the app takes that step now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct MouseClicks {
  click: MouseClick,
  /// When the app last took a step itself; its `popstate` is not the webview's.
  maybe_own_step_at: Option<TimestampMs>,
}

/// Where a click stands, from its press until the app's own step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum MouseClick {
  #[default]
  Idle,
  Pressed(Travel),
  /// The history moved while the button was down: the webview went there on its own.
  MovedByWebview,
  /// Released; the app steps at `deadline` unless the webview moves first.
  Released {
    travel: Travel,
    deadline: TimestampMs,
  },
}

impl MouseClicks {
  /// The button went down. A step still waiting from a click just before is kept for its release.
  fn press(&mut self, travel_to: Travel) {
    if !matches!(self.click, MouseClick::Released { .. }) {
      self.click = MouseClick::Pressed(travel_to);
    }
  }

  /// The button came up at `now`. Returns the step of a click before this one that is still
  /// waiting: it is taken now, so quick clicks each move once.
  fn release(&mut self, travel_to: Travel, now: TimestampMs) -> Option<Travel> {
    let waiting = MouseClick::Released {
      travel: travel_to,
      deadline: now + grace_ms(),
    };
    match self.click {
      MouseClick::MovedByWebview => {
        self.click = MouseClick::Idle;
        None
      },
      MouseClick::Released { travel, .. } => {
        self.click = waiting;
        self.maybe_own_step_at = Some(now);
        Some(travel)
      },
      // A release without a press seen (a webview that doesn't report it) still moves once.
      MouseClick::Idle | MouseClick::Pressed(_) => {
        self.click = waiting;
        None
      },
    }
  }

  /// The grace of the release with `deadline` ended: the app's own step, unless the webview moved
  /// meanwhile or a newer click took over.
  fn settle(&mut self, deadline: TimestampMs) -> Option<Travel> {
    match self.click {
      MouseClick::Released {
        travel,
        deadline: waiting_for,
      } if waiting_for == deadline => {
        self.click = MouseClick::Idle;
        self.maybe_own_step_at = Some(deadline);
        Some(travel)
      },
      _ => None,
    }
  }

  /// A `popstate` arrived at `now`. The answer to the app's own recent step is skipped; any other
  /// while a click is under way is the webview's own step, so the app doesn't take one. One after
  /// the grace (a throttled timer) is not the webview's.
  fn history_moved(&mut self, now: TimestampMs) {
    if let Some(own_step_at) = self.maybe_own_step_at.take()
      && now <= own_step_at + grace_ms()
    {
      return;
    }
    self.click = match self.click {
      MouseClick::Pressed(_) => MouseClick::MovedByWebview,
      MouseClick::Released { deadline, .. } if now <= deadline => MouseClick::Idle,
      other => other,
    };
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const T0: TimestampMs = 1_000_000;

  mod mouse_buttons {
    use super::*;

    #[test]
    fn a_click_moves_once_after_the_grace() {
      let mut clicks = clicked(Travel::Back);
      assert_eq!(clicks.settle(T0 + grace_ms()), Some(Travel::Back));
      clicks.history_moved(T0 + grace_ms() + 3);
      assert_eq!(clicks, MouseClicks::default(), "its own popstate changes nothing");
    }

    #[test]
    fn a_webview_that_moved_on_the_press_gets_no_second_step() {
      let mut clicks = MouseClicks::default();
      clicks.press(Travel::Forward);
      clicks.history_moved(T0 - 5);
      assert_eq!(clicks.release(Travel::Forward, T0), None);
      assert_eq!(clicks.settle(T0 + grace_ms()), None);
    }

    #[test]
    fn a_webview_that_moved_on_the_release_gets_no_second_step() {
      let mut clicks = clicked(Travel::Back);
      clicks.history_moved(T0 + grace_ms() / 2);
      assert_eq!(clicks.settle(T0 + grace_ms()), None);
      assert_eq!(clicks, MouseClicks::default());
    }

    #[test]
    fn a_history_move_after_the_grace_is_not_the_webviews() {
      let mut clicks = clicked(Travel::Back);
      clicks.history_moved(T0 + grace_ms() + 1);
      assert_eq!(clicks.settle(T0 + grace_ms()), Some(Travel::Back));
    }

    #[test]
    fn quick_clicks_each_move_once() {
      let mut clicks = clicked(Travel::Back);
      clicks.press(Travel::Back);
      let second_release = T0 + 30;
      assert_eq!(
        clicks.release(Travel::Back, second_release),
        Some(Travel::Back),
        "the first click's step is taken now"
      );
      clicks.history_moved(second_release + 2);
      assert_eq!(clicks.settle(T0 + grace_ms()), None, "the first timer is stale");
      assert_eq!(
        clicks.settle(second_release + grace_ms()),
        Some(Travel::Back),
        "the first step's popstate was not taken for the webview's"
      );
    }

    #[test]
    fn history_moves_without_a_click_change_nothing() {
      let mut clicks = MouseClicks::default();
      clicks.history_moved(T0);
      assert_eq!(clicks, MouseClicks::default());
    }

    #[test]
    fn a_release_without_a_press_still_moves() {
      let mut clicks = MouseClicks::default();
      assert_eq!(clicks.release(Travel::Forward, T0), None);
      assert_eq!(clicks.settle(T0 + grace_ms()), Some(Travel::Forward));
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

  /// Pressed and released at `T0`, waiting for the grace to end.
  fn clicked(travel_to: Travel) -> MouseClicks {
    let mut clicks = MouseClicks::default();
    clicks.press(travel_to);
    assert_eq!(
      clicks.release(travel_to, T0),
      None,
      "nothing happens on the release itself"
    );
    clicks
  }
}
