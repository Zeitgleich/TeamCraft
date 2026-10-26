//! A banner while the app can't reach the server: the device is offline, the server is unreachable
//! ("online but disconnected": server down, a proxy blocking WebSockets) or the session expired.
//! It follows the core's connection state, not only the browser's idea of connectivity. Messages
//! wait in the outbox meanwhile.
//!
//! The page also reports what the core can't see itself: the network coming back, the tab becoming
//! visible or focused again or being resumed after it was frozen. Each one makes the core check its
//! connection at once instead of after its backoff, and gives the reconnect a grace period before
//! the banner shows: coming back from the camera or another app is silent when it reconnects quickly.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::remote::{CORE_CHANGED_EVENT, CoreConnection, NetworkInterception};
use teamcraft_proto::SignInReason;

use crate::auth::tauri;
use crate::clock::now_ms;
use crate::components::app::{on_document, on_window};
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::reactive::{SetIfAlive, use_interval_while};
use crate::remote_backend;
use crate::session::use_session;

/// "Connecting" and "unreachable" only show when the connection stays down longer than this, counted
/// from the page's return when it was hidden or frozen: a quick reconnect needs no banner.
const CONNECTING_GRACE_MS: i64 = 10_000;
/// How often the countdown to the next attempt updates.
const COUNTDOWN_MS: u64 = 1_000;

/// What the banner says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Notice {
  /// The device itself has no network.
  DeviceOffline,
  Connecting,
  /// Online, but the server isn't reachable; the next attempt is in this many seconds. Names what
  /// answered instead, if the core found out (a captive portal, a TLS-intercepting proxy).
  Unreachable {
    retry_in_secs: i64,
    maybe_interception: Option<NetworkInterception>,
  },
  /// The token was refused; says why when the server did (issue #667).
  SignInRequired(SignInReason),
}

/// Since when the connection is down, for the grace period. A page that comes back (from the
/// camera, another app, a frozen tab) starts it again: the time it was away doesn't count, and the
/// reconnect it starts gets the whole grace period.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Unsettled {
  maybe_since_ms: Option<i64>,
}

/// Shown at the top while the app can't reach the server.
#[component]
pub fn OfflineBanner() -> impl IntoView {
  let i18n = use_i18n();
  let is_online = RwSignal::new(navigator_online());
  let connection = RwSignal::new(None::<CoreConnection>);
  let unsettled = RwSignal::new(Unsettled::default());
  let now = RwSignal::new(now_ms());
  let maybe_session = use_session();

  let refresh = move || {
    spawn_local(async move {
      // No core (not signed in yet, demo): nothing to show.
      let Ok(state) = remote_backend::connection_state().await else {
        return;
      };
      if let Some(current) = unsettled.try_get_untracked() {
        let observed = current.observe(state, now_ms());
        if observed != current {
          unsettled.set_if_alive(observed);
        }
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
  report_network_changes(is_online, unsettled);

  let notice = Memo::new(move |_| notice_for(is_online.get(), connection.get(), unsettled.get(), now.get()));
  move || {
    notice.get().map(|notice| {
      let text = match notice {
        Notice::DeviceOffline => i18n.t("offline-banner"),
        Notice::Connecting => i18n.t("connection-banner-connecting"),
        Notice::Unreachable {
          retry_in_secs,
          maybe_interception,
        } => i18n.t_with(
          match maybe_interception {
            None => "connection-banner-unreachable",
            Some(NetworkInterception::CaptivePortal) => "connection-banner-captive-portal",
            Some(NetworkInterception::UntrustedCertificate) => "connection-banner-untrusted-certificate",
          },
          &[("seconds", &retry_in_secs.to_string())],
        ),
        Notice::SignInRequired(reason) => i18n.t(match reason {
          SignInReason::SessionEnded => "connection-banner-session-ended",
          SignInReason::SessionTimedOut => "connection-banner-session-timed-out",
          SignInReason::Unknown => "connection-banner-sign-in",
        }),
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
        Notice::SignInRequired(_) => Some(
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
fn report_network_changes(is_online: RwSignal<bool>, unsettled: RwSignal<Unsettled>) {
  let returned = move || {
    // The listeners live as long as the page; the banner may be gone (signed out).
    unsettled.try_update(|current| *current = current.page_returned(now_ms()));
    network_changed();
  };
  let listener = window_event_listener(ev::online, move |_| {
    is_online.set(true);
    returned();
  });
  // Removed with the component: a late event must not reach its disposed signals.
  on_cleanup(move || listener.remove());
  let listener = window_event_listener(ev::offline, move |_| is_online.set(false));
  // Removed with the component: a late event must not reach its disposed signals.
  on_cleanup(move || listener.remove());
  on_window("pageshow", returned);
  on_document("resume", returned);
  on_window("focus", returned);
  on_document("visibilitychange", move || {
    let is_visible = web_sys::window()
      .and_then(|window| window.document())
      .is_some_and(|document| !document.hidden());
    if is_visible {
      returned();
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

impl Unsettled {
  /// Follows the connection: the grace period starts when it goes down and ends when it is up (or
  /// waits for the user to sign in).
  fn observe(self, connection: CoreConnection, now_ms: i64) -> Self {
    let maybe_since_ms = match connection {
      CoreConnection::Connecting | CoreConnection::Offline { .. } => {
        Some(self.maybe_since_ms.unwrap_or(now_ms))
      },
      CoreConnection::Connected | CoreConnection::SignInRequired { .. } => None,
    };
    Self { maybe_since_ms }
  }

  /// The page is back: a connection that is down gets the grace period from now on.
  fn page_returned(self, now_ms: i64) -> Self {
    Self {
      maybe_since_ms: self.maybe_since_ms.map(|_| now_ms),
    }
  }

  /// Down for longer than the grace period. A clock that went back counts as just now.
  fn is_past_grace(self, now_ms: i64) -> bool {
    self
      .maybe_since_ms
      .is_some_and(|since| now_ms.saturating_sub(since) >= CONNECTING_GRACE_MS)
  }
}

/// The device's own connectivity wins: without a network the server can't be the problem.
fn notice_for(
  is_online: bool,
  maybe_connection: Option<CoreConnection>,
  unsettled: Unsettled,
  now: i64,
) -> Option<Notice> {
  if !is_online {
    return Some(Notice::DeviceOffline);
  }
  match maybe_connection? {
    CoreConnection::Connected => None,
    CoreConnection::Connecting | CoreConnection::Offline { .. } if !unsettled.is_past_grace(now) => None,
    CoreConnection::Connecting => Some(Notice::Connecting),
    CoreConnection::Offline {
      retry_at_ms,
      maybe_interception,
    } => Some(Notice::Unreachable {
      retry_in_secs: retry_at_ms.saturating_sub(now).max(0).saturating_add(999) / 1000,
      maybe_interception,
    }),
    CoreConnection::SignInRequired { reason } => Some(Notice::SignInRequired(reason)),
  }
}

fn navigator_online() -> bool {
  web_sys::window().is_none_or(|window| window.navigator().on_line())
}

#[cfg(test)]
mod tests {
  use super::*;

  const OFFLINE: CoreConnection = CoreConnection::Offline {
    retry_at_ms: 10_500,
    maybe_interception: None,
  };

  #[test]
  fn being_offline_wins_over_the_connection_state() {
    assert_eq!(
      notice_for(false, Some(CoreConnection::Connected), Unsettled::default(), 0),
      Some(Notice::DeviceOffline)
    );
  }

  #[test]
  fn a_connected_or_unknown_core_shows_nothing() {
    assert_eq!(
      notice_for(true, Some(CoreConnection::Connected), Unsettled::default(), 0),
      None
    );
    assert_eq!(notice_for(true, None, Unsettled::default(), 0), None);
  }

  #[test]
  fn a_refused_token_asks_to_sign_in() {
    assert_eq!(
      notice_for(
        true,
        Some(CoreConnection::SignInRequired {
          reason: SignInReason::SessionEnded
        }),
        Unsettled::default(),
        0
      ),
      Some(Notice::SignInRequired(SignInReason::SessionEnded))
    );
  }

  mod grace {
    use super::*;

    #[test]
    fn connecting_shows_only_after_the_grace_period() {
      let connecting = CoreConnection::Connecting;
      let unsettled = Unsettled::default().observe(connecting, 1_000);
      assert_eq!(notice_for(true, Some(connecting), unsettled, 2_000), None);
      assert_eq!(
        notice_for(true, Some(connecting), unsettled, 1_000 + CONNECTING_GRACE_MS),
        Some(Notice::Connecting)
      );
    }

    #[test]
    fn a_lost_connection_is_not_unreachable_before_the_grace_period() {
      let unsettled = Unsettled::default().observe(OFFLINE, 1_000);
      assert_eq!(notice_for(true, Some(OFFLINE), unsettled, 2_500), None);
    }

    #[test]
    fn the_grace_period_runs_on_from_lost_to_connecting() {
      let unsettled = Unsettled::default()
        .observe(OFFLINE, 1_000)
        .observe(CoreConnection::Connecting, 2_000);
      assert_eq!(
        notice_for(
          true,
          Some(CoreConnection::Connecting),
          unsettled,
          1_000 + CONNECTING_GRACE_MS
        ),
        Some(Notice::Connecting)
      );
    }

    #[test]
    fn a_page_back_from_the_camera_reconnects_silently() {
      // Down before the tab was frozen; the page comes back much later.
      let connecting = CoreConnection::Connecting;
      let before_freeze = Unsettled::default().observe(connecting, 1_000);
      let back = before_freeze.page_returned(60_000);
      assert_eq!(notice_for(true, Some(connecting), back, 60_000), None);
      assert_eq!(notice_for(true, Some(connecting), back, 62_500), None);
      assert_eq!(
        notice_for(
          true,
          Some(CoreConnection::Connected),
          back.observe(CoreConnection::Connected, 62_500),
          62_500
        ),
        None
      );
      // Still down after the grace period: now it says so.
      assert_eq!(
        notice_for(true, Some(connecting), back, 60_000 + CONNECTING_GRACE_MS),
        Some(Notice::Connecting)
      );
    }

    #[test]
    fn a_return_while_connected_starts_no_grace_period() {
      let unsettled = Unsettled::default().page_returned(5_000);
      assert_eq!(unsettled, Unsettled::default());
    }

    #[test]
    fn being_connected_ends_the_grace_period() {
      let unsettled = Unsettled::default()
        .observe(CoreConnection::Connecting, 1_000)
        .observe(CoreConnection::Connected, 1_500)
        .observe(CoreConnection::Connecting, 10_000);
      assert_eq!(
        notice_for(true, Some(CoreConnection::Connecting), unsettled, 11_000),
        None
      );
    }

    #[test]
    fn a_clock_that_went_back_shows_nothing_yet() {
      let unsettled = Unsettled::default().observe(CoreConnection::Connecting, 10_000);
      assert_eq!(
        notice_for(true, Some(CoreConnection::Connecting), unsettled, 1_000),
        None
      );
    }
  }

  mod unreachable {
    use super::*;

    #[test]
    fn online_but_unreachable_counts_down_to_the_next_attempt() {
      let unsettled = Unsettled::default().observe(OFFLINE, 0);
      assert_eq!(
        notice_for(true, Some(OFFLINE), unsettled, CONNECTING_GRACE_MS),
        Some(Notice::Unreachable {
          retry_in_secs: 8,
          maybe_interception: None
        })
      );
      assert_eq!(
        notice_for(true, Some(OFFLINE), unsettled, 20_000),
        Some(Notice::Unreachable {
          retry_in_secs: 0,
          maybe_interception: None
        })
      );
    }

    #[test]
    fn a_captive_portal_is_named_in_the_banner() {
      let portal = CoreConnection::Offline {
        retry_at_ms: 5_000,
        maybe_interception: Some(NetworkInterception::CaptivePortal),
      };
      let unsettled = Unsettled::default().observe(portal, 0);
      assert_eq!(
        notice_for(true, Some(portal), unsettled, CONNECTING_GRACE_MS),
        Some(Notice::Unreachable {
          retry_in_secs: 2,
          maybe_interception: Some(NetworkInterception::CaptivePortal)
        })
      );
    }
  }
}
