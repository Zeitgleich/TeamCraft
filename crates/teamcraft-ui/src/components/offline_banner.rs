//! A banner while the app can't reach the server: the device is offline, the server is unreachable
//! ("online but disconnected": server down, a proxy blocking WebSockets) or the session expired.
//! It follows the core's connection state, not only the browser's idea of connectivity. Messages
//! wait in the outbox meanwhile.
//!
//! The page also reports what the core can't see itself: the network coming back, the tab becoming
//! visible again or being resumed after it was frozen. Each one makes the core check its
//! connection at once instead of after its backoff.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::remote::{CORE_CHANGED_EVENT, CoreConnection};

use crate::auth::tauri;
use crate::clock::now_ms;
use crate::components::app::{on_document, on_window};
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::reactive::{SetIfAlive, use_interval_while};
use crate::remote_backend;
use crate::session::use_session;

/// "Connecting" only shows when it takes longer than this; a quick reconnect needs no banner.
const CONNECTING_GRACE_MS: i64 = 3_000;
/// How often the countdown to the next attempt updates.
const COUNTDOWN_MS: u64 = 1_000;

/// What the banner says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Notice {
  /// The device itself has no network.
  DeviceOffline,
  Connecting,
  /// Online, but the server isn't reachable; the next attempt is in this many seconds.
  Unreachable {
    retry_in_secs: i64,
  },
  SignInRequired,
}

/// Shown at the top while the app can't reach the server.
#[component]
pub fn OfflineBanner() -> impl IntoView {
  let i18n = use_i18n();
  let is_online = RwSignal::new(navigator_online());
  let connection = RwSignal::new(None::<CoreConnection>);
  let connecting_since = RwSignal::new(None::<i64>);
  let now = RwSignal::new(now_ms());
  let maybe_session = use_session();

  let refresh = move || {
    spawn_local(async move {
      // No core (not signed in yet, demo): nothing to show.
      let Ok(state) = remote_backend::connection_state().await else {
        return;
      };
      if state == CoreConnection::Connecting {
        if connecting_since.try_get_untracked() == Some(None) {
          connecting_since.set_if_alive(Some(now_ms()));
        }
      } else {
        connecting_since.set_if_alive(None);
      }
      if connection
        .try_get_untracked()
        .is_some_and(|shown| shown != Some(state))
      {
        connection.set_if_alive(Some(state));
      }
    });
  };
  refresh();
  tauri::listen(CORE_CHANGED_EVENT, refresh);
  // The clock only matters while a countdown or the connecting grace period can show; a connected
  // app must not wake up every second for it.
  use_interval_while(
    COUNTDOWN_MS,
    move || {
      matches!(
        connection.get(),
        Some(CoreConnection::Connecting | CoreConnection::Offline { .. })
      )
    },
    move || now.set(now_ms()),
  );
  report_network_changes(is_online);

  let notice = Memo::new(move |_| {
    notice_for(
      is_online.get(),
      connection.get(),
      connecting_since.get(),
      now.get(),
    )
  });
  move || {
    notice.get().map(|notice| {
      let text = match notice {
        Notice::DeviceOffline => i18n.t("offline-banner"),
        Notice::Connecting => i18n.t("connection-banner-connecting"),
        Notice::Unreachable { retry_in_secs } => i18n.t_with(
          "connection-banner-unreachable",
          &[("seconds", &retry_in_secs.to_string())],
        ),
        Notice::SignInRequired => i18n.t("connection-banner-sign-in"),
      };
      let action = match notice {
        Notice::Unreachable { .. } => Some(
          view! {
            <button class="link-btn" on:click=move |_| network_changed()>
              {move || i18n.t("connection-banner-retry")}
            </button>
          }
          .into_any(),
        ),
        Notice::SignInRequired => Some(
          view! {
            <button
              class="link-btn"
              on:click=move |_| {
                if let Some(control) = maybe_session {
                  control.sign_out();
                }
              }
            >
              {move || i18n.t("connection-banner-sign-in-action")}
            </button>
          }
          .into_any(),
        ),
        Notice::DeviceOffline | Notice::Connecting => None,
      };
      view! {
        <div class="offline-banner" role="status">
          <Icon name=IconName::Alert />
          <span>{text}</span>
          {action}
        </div>
      }
    })
  }
}

/// The browser (or the desktop webview) saw the network come back, the page become visible again
/// or resume after being frozen or restored from the back/forward cache.
fn report_network_changes(is_online: RwSignal<bool>) {
  let listener = window_event_listener(ev::online, move |_| {
    is_online.set(true);
    network_changed();
  });
  // Removed with the component: a late event must not reach its disposed signals.
  on_cleanup(move || listener.remove());
  let listener = window_event_listener(ev::offline, move |_| is_online.set(false));
  // Removed with the component: a late event must not reach its disposed signals.
  on_cleanup(move || listener.remove());
  on_window("pageshow", network_changed);
  on_document("resume", network_changed);
  on_document("visibilitychange", || {
    let is_visible = web_sys::window()
      .and_then(|window| window.document())
      .is_some_and(|document| !document.hidden());
    if is_visible {
      network_changed();
    }
  });
}

fn network_changed() {
  spawn_local(async {
    if let Err(error) = remote_backend::network_changed().await {
      tracing::debug!(%error, "could not report the network change");
    }
  });
}

/// The device's own connectivity wins: without a network the server can't be the problem.
fn notice_for(
  is_online: bool,
  maybe_connection: Option<CoreConnection>,
  maybe_connecting_since: Option<i64>,
  now: i64,
) -> Option<Notice> {
  if !is_online {
    return Some(Notice::DeviceOffline);
  }
  match maybe_connection? {
    CoreConnection::Connected => None,
    CoreConnection::Connecting => maybe_connecting_since
      .filter(|since| now.saturating_sub(*since) >= CONNECTING_GRACE_MS)
      .map(|_| Notice::Connecting),
    CoreConnection::Offline { retry_at_ms } => Some(Notice::Unreachable {
      retry_in_secs: retry_at_ms.saturating_sub(now).max(0).saturating_add(999) / 1000,
    }),
    CoreConnection::SignInRequired => Some(Notice::SignInRequired),
  }
}

fn navigator_online() -> bool {
  web_sys::window().is_none_or(|window| window.navigator().on_line())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn being_offline_wins_over_the_connection_state() {
    assert_eq!(
      notice_for(false, Some(CoreConnection::Connected), None, 0),
      Some(Notice::DeviceOffline)
    );
  }

  #[test]
  fn a_connected_or_unknown_core_shows_nothing() {
    assert_eq!(notice_for(true, Some(CoreConnection::Connected), None, 0), None);
    assert_eq!(notice_for(true, None, None, 0), None);
  }

  #[test]
  fn connecting_shows_only_after_a_grace_period() {
    let connecting = Some(CoreConnection::Connecting);
    assert_eq!(notice_for(true, connecting, Some(1_000), 2_000), None);
    assert_eq!(
      notice_for(true, connecting, Some(1_000), 1_000 + CONNECTING_GRACE_MS),
      Some(Notice::Connecting)
    );
  }

  #[test]
  fn online_but_unreachable_counts_down_to_the_next_attempt() {
    let offline = Some(CoreConnection::Offline { retry_at_ms: 10_500 });
    assert_eq!(
      notice_for(true, offline, None, 1_000),
      Some(Notice::Unreachable { retry_in_secs: 10 })
    );
    assert_eq!(
      notice_for(true, offline, None, 20_000),
      Some(Notice::Unreachable { retry_in_secs: 0 })
    );
  }

  #[test]
  fn a_refused_token_asks_to_sign_in() {
    assert_eq!(
      notice_for(true, Some(CoreConnection::SignInRequired), None, 0),
      Some(Notice::SignInRequired)
    );
  }
}
