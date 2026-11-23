//! The connection state while the app can't reach the server: the device is offline, the server is unreachable
//! ("online but disconnected": server down, a proxy blocking WebSockets) or the session expired.
//! It follows the core's connection state, not only the browser's idea of connectivity. Messages
//! wait in the outbox meanwhile.
//!
//! The page also reports what the core can't see itself: the network coming back, the tab becoming
//! visible or focused again or being resumed after it was frozen. Each one makes the core check its
//! connection at once instead of after its backoff, and gives the reconnect a grace period before
//! the banner shows: coming back from the camera or another app is silent when it reconnects quickly.
//!
//! Reconnecting is the app's job, not the user's (#718): while it works on it the state is one quiet
//! line of text above the composer ([`ConnectionNote`], connection_note.rs), never a toast. A
//! request that finds the server down (a proxy answering 502 while it restarts) shows the same line
//! at once and makes the core check its connection. The line goes away by itself once the server
//! answers. Only when the user has to act (sign in again, sign in to the network, a certificate to
//! sort out) does it become a notice in the banner row, with the action.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::remote::{CORE_CHANGED_EVENT, CoreConnection, NetworkInterception};
use teamcraft_proto::SignInReason;

use crate::auth::tauri;
use crate::clock::now_ms;
use crate::components::app::{on_document, on_window};
use crate::components::connection_note::{ConnectionNote, ConnectionNoteSlot};
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::reactive::{SetIfAlive, use_interval_while};
use crate::remote_backend;
use crate::session::{SessionControl, use_session};

/// "Connecting" and "unreachable" only show when the connection stays down longer than this: a
/// short blip stays invisible.
const CONNECTING_GRACE_MS: i64 = 2_000;
/// The grace period after the page came back (from the camera, another app, a frozen tab): the
/// reconnect it starts gets this long before anything shows.
const RETURN_GRACE_MS: i64 = 10_000;
/// How long a request that found the server unreachable keeps the note up while the connection
/// itself looks fine; another such failure renews it, the connection coming back ends it.
const REQUEST_HOLD_MS: i64 = 15_000;
/// How often the countdown to the next attempt updates.
const COUNTDOWN_MS: u64 = 1_000;
/// A connection that stays down this long (counted like the grace period) offers "Try now".
const ESCALATE_AFTER_MS: i64 = 60_000;
/// While the app runs from the cached account and the server hasn't answered yet (issue #778) the
/// grace period is this short: the chats are on screen, and the user should soon see that they may
/// be out of date. Long enough for a normal start not to flash the hint.
const UNCONFIRMED_GRACE_MS: i64 = 1_500;

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
  /// Connected, but the server answers late: messages still go out, only slower (issue #1064).
  Slow,
}

/// Where a notice shows (#718).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prominence {
  /// The app is on it: the quiet line above the composer, nothing to do.
  Note(ConnectionNote),
  /// The user must do something: a card in the banner row with the full text and the action.
  Notice,
}

/// Since when the connection is down, for the grace period, and whether an attempt failed since
/// (then "connecting" again is still "unreachable", not a new state every few seconds). A page that
/// comes back (from the camera, another app, a frozen tab) starts the grace period again, the longer
/// [`RETURN_GRACE_MS`]: the time it was away doesn't count.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Unsettled {
  maybe_since_ms: Option<i64>,
  has_failed: bool,
  is_after_return: bool,
  /// When a request last found the server unreachable while the connection looked fine.
  maybe_request_failed_ms: Option<i64>,
}

/// Shown while the app can't reach the server: a hint along the top bar, or a notice to act on.
#[component]
pub fn OfflineBanner() -> impl IntoView {
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
      if let Some(shown) = connection.try_get_untracked()
        && shown != Some(state)
      {
        if let Some(previous) = shown {
          crate::perf::connection_changed(previous, state);
        }
        connection.set_if_alive(Some(state));
        // The identity provider refused the sign-in for good: back to the sign-in screen, which
        // says why (issue #883). Nothing the user could do here would help.
        if state
          == (CoreConnection::SignInRequired {
            reason: SignInReason::SignInExpired,
          })
          && let Some(control) = maybe_session
        {
          control.end_expired_session();
        }
      }
    });
  };
  refresh();
  tauri::listen(CORE_CHANGED_EVENT, refresh);
  // The clock only matters while a countdown, the connecting grace period or a failed request's
  // note can show; a connected app must not wake up every second for it.
  use_interval_while(
    COUNTDOWN_MS,
    move || {
      matches!(
        connection.get(),
        Some(CoreConnection::Connecting | CoreConnection::Offline { .. })
      ) || unsettled.with(|current| current.maybe_request_failed_ms.is_some())
    },
    move || {
      let tick = now_ms();
      now.set(tick);
      if let Some(current) = unsettled.try_get_untracked() {
        let expired = current.expire_request_failure(tick);
        if expired != current {
          unsettled.set_if_alive(expired);
        }
      }
    },
  );
  report_network_changes(is_online, unsettled, maybe_session);

  follow_request_failures(unsettled);

  let notice = Memo::new(move |_| {
    let grace_ms = match maybe_session.is_some_and(|control| control.is_account_unconfirmed.get()) {
      true => UNCONFIRMED_GRACE_MS,
      false => CONNECTING_GRACE_MS,
    };
    notice_for(
      is_online.get(),
      connection.get(),
      unsettled.get(),
      now.get(),
      grace_ms,
    )
  });
  let prominence = Memo::new(move |_| {
    notice
      .get()
      .map(|notice| prominence_of(notice, unsettled.get(), now.get()))
  });
  // The quiet line lives above the composer (connection_note.rs); this only feeds it.
  if let Some(slot) = use_context::<ConnectionNoteSlot>() {
    Effect::new(move |_| {
      let maybe_note = match prominence.get() {
        Some(Prominence::Note(note)) => Some(note),
        Some(Prominence::Notice) | None => None,
      };
      slot.set_note(maybe_note);
    });
    on_cleanup(move || slot.set_note(None));
  }
  move || {
    let notice = notice.get()?;
    (prominence.get()? == Prominence::Notice)
      .then(|| view! { <ConnectionNotice notice=notice maybe_session=maybe_session /> })
  }
}

/// Requests that find the server unreachable (feedback.rs) show the note at once, also while the
/// gateway still looks connected, and make the core check its connection now instead of after its
/// heartbeat.
fn follow_request_failures(unsettled: RwSignal<Unsettled>) {
  crate::components::connection_note::set_request_failure_sink(move || {
    let now = now_ms();
    let Some(current) = unsettled.try_get_untracked() else {
      return;
    };
    let must_probe = current.is_probe_due(now);
    unsettled.set_if_alive(current.request_unreachable(now));
    if must_probe {
      network_changed();
    }
  });
}

/// The card when the user can do something about it: what happened, and the action.
#[component]
fn ConnectionNotice(notice: Notice, maybe_session: Option<SessionControl>) -> impl IntoView {
  let i18n = use_i18n();
  let text = move || match notice {
    Notice::Unreachable {
      retry_in_secs,
      maybe_interception: Some(interception),
    } => i18n.t_with(
      match interception {
        NetworkInterception::CaptivePortal => "connection-banner-captive-portal",
        NetworkInterception::UntrustedCertificate => "connection-banner-untrusted-certificate",
        NetworkInterception::ProofRefused => "connection-banner-proof-refused",
      },
      &[("seconds", &retry_in_secs.to_string())],
    ),
    Notice::SignInRequired(reason) => i18n.t(match reason {
      SignInReason::SessionEnded => "connection-banner-session-ended",
      SignInReason::SessionTimedOut => "connection-banner-session-timed-out",
      SignInReason::DeviceBindingRequired => "connection-banner-device-binding-required",
      SignInReason::DeviceKeyLost => "connection-banner-device-key-lost",
      SignInReason::NoTeamcraftRole => "connection-banner-no-role-left",
      SignInReason::SignInExpired | SignInReason::Unknown => "connection-banner-sign-in",
    }),
    // Notes, never a card (prominence_of).
    Notice::DeviceOffline | Notice::Connecting | Notice::Slow | Notice::Unreachable { .. } => String::new(),
  };
  let action = match notice {
    Notice::Unreachable { .. } => Some(
      view! {
        <button class="btn btn-small" on:click=move |_| network_changed()>
          {move || i18n.t("connection-banner-retry")}
        </button>
      }
      .into_any(),
    ),
    Notice::SignInRequired(_) => Some(
      view! {
        <button
          class="btn btn-small btn-primary"
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
    Notice::DeviceOffline | Notice::Connecting | Notice::Slow => None,
  };
  view! {
    <div class="connection-notice" role="status">
      <Icon name=IconName::Alert />
      <span class="connection-notice-text">{text}</span>
      {action}
    </div>
  }
}

/// The browser (or the desktop webview) saw the network come back, the page become visible again
/// or resume after being frozen or restored from the back/forward cache.
fn report_network_changes(
  is_online: RwSignal<bool>,
  unsettled: RwSignal<Unsettled>,
  maybe_session: Option<SessionControl>,
) {
  let returned = move || {
    // The listeners live as long as the page; the banner may be gone (signed out).
    unsettled.try_update(|current| *current = current.page_returned(now_ms()));
    network_changed();
    if let Some(control) = maybe_session {
      control.check_account_now();
    }
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

pub(crate) fn network_changed() {
  spawn_local(async {
    if let Err(error) = remote_backend::network_changed().await {
      tracing::debug!(%error, "could not report the network change");
    }
  });
}

impl Unsettled {
  /// Follows the connection: the grace period starts when it goes down and ends when it is up (or
  /// waits for the user to sign in). Being connected again also ends a failed request's note: the
  /// server answers.
  fn observe(self, connection: CoreConnection, now_ms: i64) -> Self {
    match connection {
      CoreConnection::Connecting => Self {
        maybe_since_ms: Some(self.maybe_since_ms.unwrap_or(now_ms)),
        ..self
      },
      CoreConnection::Offline { .. } => Self {
        maybe_since_ms: Some(self.maybe_since_ms.unwrap_or(now_ms)),
        has_failed: true,
        ..self
      },
      // Back from being down: the server answers again. Still up: a failed request's note stays
      // until its hold is over (this runs on every change of the core, not only on reconnects).
      CoreConnection::Connected | CoreConnection::Slow if self.maybe_since_ms.is_some() => Self::default(),
      CoreConnection::Connected | CoreConnection::Slow | CoreConnection::SignInRequired { .. } => Self {
        maybe_request_failed_ms: self.maybe_request_failed_ms,
        ..Self::default()
      },
    }
  }

  /// The page is back: a connection that is down gets the longer grace period from now on.
  fn page_returned(self, now_ms: i64) -> Self {
    match self.maybe_since_ms {
      Some(_) => Self {
        maybe_since_ms: Some(now_ms),
        is_after_return: true,
        ..self
      },
      None => self,
    }
  }

  /// A request found the server unreachable.
  fn request_unreachable(self, now_ms: i64) -> Self {
    Self {
      maybe_request_failed_ms: Some(now_ms),
      ..self
    }
  }

  /// Whether a request failure should make the core check its connection: the first one, not every
  /// one of a burst (a scroll through history while the server is down).
  fn is_probe_due(self, now_ms: i64) -> bool {
    !self.is_request_failure_live(now_ms)
  }

  /// Forgets a failed request's note once its hold is over.
  fn expire_request_failure(self, now_ms: i64) -> Self {
    match self.is_request_failure_live(now_ms) {
      true => self,
      false => Self {
        maybe_request_failed_ms: None,
        ..self
      },
    }
  }

  /// A request found the server unreachable within [`REQUEST_HOLD_MS`]. A clock that went back
  /// counts as just now.
  fn is_request_failure_live(self, now_ms: i64) -> bool {
    self
      .maybe_request_failed_ms
      .is_some_and(|at| now_ms.saturating_sub(at) < REQUEST_HOLD_MS)
  }

  /// Down for longer than the grace period. A clock that went back counts as just now.
  fn is_past_grace(self, now_ms: i64, grace_ms: i64) -> bool {
    let grace_ms = match self.is_after_return {
      true => grace_ms.max(RETURN_GRACE_MS),
      false => grace_ms,
    };
    self
      .maybe_since_ms
      .is_some_and(|since| now_ms.saturating_sub(since) >= grace_ms)
  }
}

/// The device's own connectivity wins: without a network the server can't be the problem.
fn notice_for(
  is_online: bool,
  maybe_connection: Option<CoreConnection>,
  unsettled: Unsettled,
  now: i64,
  grace_ms: i64,
) -> Option<Notice> {
  if !is_online {
    return Some(Notice::DeviceOffline);
  }
  // A request found the server down although the connection looks fine (or is still in its grace
  // period): the user's action waits, so this shows at once.
  let request_failed = unsettled
    .is_request_failure_live(now)
    .then_some(Notice::Unreachable {
      retry_in_secs: 0,
      maybe_interception: None,
    });
  match maybe_connection {
    None | Some(CoreConnection::Connected) => request_failed,
    // Still connected: a quiet note, no grace period needed (the core doesn't flip it back and forth).
    Some(CoreConnection::Slow) => request_failed.or(Some(Notice::Slow)),
    Some(CoreConnection::Connecting | CoreConnection::Offline { .. })
      if !unsettled.is_past_grace(now, grace_ms) =>
    {
      request_failed
    },
    // Reconnecting after a failed attempt is still "unreachable", not a new state every few seconds.
    Some(CoreConnection::Connecting) if unsettled.has_failed => Some(Notice::Unreachable {
      retry_in_secs: 0,
      maybe_interception: None,
    }),
    Some(CoreConnection::Connecting) => Some(Notice::Connecting),
    Some(CoreConnection::Offline {
      retry_at_ms,
      maybe_interception,
    }) => Some(Notice::Unreachable {
      retry_in_secs: retry_at_ms.saturating_sub(now).max(0).saturating_add(999) / 1000,
      maybe_interception,
    }),
    Some(CoreConnection::SignInRequired { reason }) => Some(Notice::SignInRequired(reason)),
  }
}

/// A notice where the user has to act (sign in again, sign in to the network, a certificate); the
/// quiet note while the app reconnects by itself, with "Try now" once the server stayed away long.
fn prominence_of(notice: Notice, unsettled: Unsettled, now_ms: i64) -> Prominence {
  let is_down_long = unsettled
    .maybe_since_ms
    .is_some_and(|since| now_ms.saturating_sub(since) >= ESCALATE_AFTER_MS);
  match notice {
    Notice::SignInRequired(_)
    | Notice::Unreachable {
      maybe_interception: Some(_),
      ..
    } => Prominence::Notice,
    Notice::DeviceOffline => Prominence::Note(ConnectionNote::DeviceOffline),
    Notice::Connecting => Prominence::Note(ConnectionNote::Connecting),
    Notice::Unreachable { .. } => Prominence::Note(ConnectionNote::ServerUnreachable {
      can_retry: is_down_long,
    }),
    Notice::Slow => Prominence::Note(ConnectionNote::Slow),
  }
}

fn navigator_online() -> bool {
  web_sys::window().is_none_or(|window| window.navigator().on_line())
}

#[cfg(test)]
mod tests {
  use super::*;

  const OFFLINE: CoreConnection = CoreConnection::Offline {
    retry_at_ms: 17_500,
    maybe_interception: None,
  };
  const UNREACHABLE: Notice = Notice::Unreachable {
    retry_in_secs: 0,
    maybe_interception: None,
  };

  /// The usual grace period; see [`a_start_from_the_cached_account_says_so_sooner`].
  fn notice_for(
    is_online: bool,
    maybe_connection: Option<CoreConnection>,
    unsettled: Unsettled,
    now: i64,
  ) -> Option<Notice> {
    super::notice_for(is_online, maybe_connection, unsettled, now, CONNECTING_GRACE_MS)
  }

  /// What the quiet line above the composer says at `now`, if anything.
  fn note_at(
    maybe_connection: Option<CoreConnection>,
    unsettled: Unsettled,
    now: i64,
  ) -> Option<ConnectionNote> {
    match prominence_of(
      notice_for(true, maybe_connection, unsettled, now)?,
      unsettled,
      now,
    ) {
      Prominence::Note(note) => Some(note),
      Prominence::Notice => None,
    }
  }

  /// The chats are on screen before the server answered (issue #778): the user soon sees that they
  /// may be out of date, but a normal start doesn't flash the note.
  #[test]
  fn a_start_from_the_cached_account_says_so_sooner() {
    let unsettled = Unsettled::default().observe(CoreConnection::Connecting, 1_000);
    let notice = |now| {
      super::notice_for(
        true,
        Some(CoreConnection::Connecting),
        unsettled,
        now,
        UNCONFIRMED_GRACE_MS,
      )
    };
    assert_eq!(notice(1_000 + UNCONFIRMED_GRACE_MS - 1), None);
    let shown = notice(1_000 + UNCONFIRMED_GRACE_MS);
    assert_eq!(shown, Some(Notice::Connecting));
    assert_eq!(
      shown.map(|shown| prominence_of(shown, unsettled, 1_000 + UNCONFIRMED_GRACE_MS)),
      Some(Prominence::Note(ConnectionNote::Connecting))
    );
  }

  #[test]
  fn being_offline_wins_over_the_connection_state() {
    assert_eq!(
      notice_for(false, Some(CoreConnection::Connected), Unsettled::default(), 0),
      Some(Notice::DeviceOffline)
    );
    assert_eq!(
      prominence_of(Notice::DeviceOffline, Unsettled::default(), 0),
      Prominence::Note(ConnectionNote::DeviceOffline)
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
  fn a_slow_link_is_a_note_not_unreachable() {
    assert_eq!(
      note_at(Some(CoreConnection::Slow), Unsettled::default(), 0),
      Some(ConnectionNote::Slow)
    );
  }

  #[test]
  fn a_refused_token_asks_to_sign_in_on_a_card() {
    let notice = notice_for(
      true,
      Some(CoreConnection::SignInRequired {
        reason: SignInReason::SessionEnded,
      }),
      Unsettled::default(),
      0,
    );
    assert_eq!(notice, Some(Notice::SignInRequired(SignInReason::SessionEnded)));
    assert_eq!(
      notice.map(|notice| prominence_of(notice, Unsettled::default(), 0)),
      Some(Prominence::Notice)
    );
  }

  #[test]
  fn a_lost_last_role_is_named_not_shown_as_an_expired_session() {
    let notice = notice_for(
      true,
      Some(CoreConnection::SignInRequired {
        reason: SignInReason::NoTeamcraftRole,
      }),
      Unsettled::default(),
      0,
    );
    assert_eq!(
      notice,
      Some(Notice::SignInRequired(SignInReason::NoTeamcraftRole))
    );
  }

  mod grace {
    use super::*;

    #[test]
    fn a_short_blip_stays_invisible() {
      let unsettled = Unsettled::default().observe(OFFLINE, 1_000);
      assert_eq!(note_at(Some(OFFLINE), unsettled, 1_000), None);
      assert_eq!(
        note_at(Some(OFFLINE), unsettled, 1_000 + CONNECTING_GRACE_MS - 1),
        None
      );
      // Back within the grace period: nothing ever showed.
      let back = unsettled.observe(CoreConnection::Connected, 2_500);
      assert_eq!(
        note_at(Some(CoreConnection::Connected), back, 1_000 + CONNECTING_GRACE_MS),
        None
      );
    }

    #[test]
    fn the_server_down_longer_shows_the_note_until_it_answers() {
      let unsettled = Unsettled::default().observe(OFFLINE, 1_000);
      assert_eq!(
        note_at(Some(OFFLINE), unsettled, 1_000 + CONNECTING_GRACE_MS),
        Some(ConnectionNote::ServerUnreachable { can_retry: false })
      );
      let back = unsettled.observe(CoreConnection::Connected, 30_000);
      assert_eq!(note_at(Some(CoreConnection::Connected), back, 30_000), None);
    }

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
    fn reconnecting_after_a_failed_attempt_stays_unreachable() {
      let unsettled = Unsettled::default()
        .observe(OFFLINE, 1_000)
        .observe(CoreConnection::Connecting, 2_000);
      assert_eq!(
        note_at(
          Some(CoreConnection::Connecting),
          unsettled,
          1_000 + CONNECTING_GRACE_MS
        ),
        Some(ConnectionNote::ServerUnreachable { can_retry: false })
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
      // Still down after the longer grace period: now it says so.
      assert_eq!(
        notice_for(true, Some(connecting), back, 60_000 + RETURN_GRACE_MS),
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

  mod failed_requests {
    use super::*;

    #[test]
    fn a_request_that_finds_the_server_down_shows_the_note_at_once() {
      let unsettled = Unsettled::default().request_unreachable(5_000);
      assert_eq!(
        notice_for(true, Some(CoreConnection::Connected), unsettled, 5_000),
        Some(UNREACHABLE)
      );
      // Also while the connection is down but still within its grace period.
      let down = Unsettled::default()
        .observe(OFFLINE, 5_000)
        .request_unreachable(5_500);
      assert_eq!(
        note_at(Some(OFFLINE), down, 5_500),
        Some(ConnectionNote::ServerUnreachable { can_retry: false })
      );
    }

    #[test]
    fn the_note_goes_once_requests_stop_failing() {
      let unsettled = Unsettled::default().request_unreachable(5_000);
      let later = 5_000 + REQUEST_HOLD_MS;
      assert_eq!(
        notice_for(true, Some(CoreConnection::Connected), unsettled, later - 1),
        Some(UNREACHABLE)
      );
      assert_eq!(
        notice_for(true, Some(CoreConnection::Connected), unsettled, later),
        None
      );
      assert_eq!(unsettled.expire_request_failure(later), Unsettled::default());
      assert_eq!(unsettled.expire_request_failure(later - 1), unsettled);
    }

    #[test]
    fn another_failure_renews_the_note() {
      let unsettled = Unsettled::default()
        .request_unreachable(5_000)
        .request_unreachable(15_000);
      assert_eq!(
        notice_for(
          true,
          Some(CoreConnection::Connected),
          unsettled,
          5_000 + REQUEST_HOLD_MS
        ),
        Some(UNREACHABLE)
      );
    }

    #[test]
    fn the_connection_coming_back_ends_it() {
      let unsettled = Unsettled::default()
        .observe(OFFLINE, 1_000)
        .request_unreachable(2_000)
        .observe(CoreConnection::Connected, 3_000);
      assert_eq!(
        notice_for(true, Some(CoreConnection::Connected), unsettled, 3_000),
        None
      );
    }

    #[test]
    fn other_changes_while_connected_keep_it() {
      let unsettled = Unsettled::default()
        .request_unreachable(2_000)
        .observe(CoreConnection::Connected, 2_500);
      assert_eq!(
        notice_for(true, Some(CoreConnection::Connected), unsettled, 2_500),
        Some(UNREACHABLE)
      );
    }

    #[test]
    fn only_the_first_failure_of_a_burst_probes_the_connection() {
      let unsettled = Unsettled::default();
      assert!(unsettled.is_probe_due(1_000));
      let failed = unsettled.request_unreachable(1_000);
      assert!(!failed.is_probe_due(1_500));
      assert!(failed.is_probe_due(1_000 + REQUEST_HOLD_MS));
    }
  }

  mod prominence {
    use super::*;

    #[test]
    fn reconnecting_by_itself_is_only_a_note() {
      let unsettled = Unsettled::default().observe(OFFLINE, 0);
      assert_eq!(
        prominence_of(Notice::DeviceOffline, unsettled, 30_000),
        Prominence::Note(ConnectionNote::DeviceOffline)
      );
      assert_eq!(
        prominence_of(Notice::Connecting, unsettled, 30_000),
        Prominence::Note(ConnectionNote::Connecting)
      );
      assert_eq!(
        prominence_of(UNREACHABLE, unsettled, 30_000),
        Prominence::Note(ConnectionNote::ServerUnreachable { can_retry: false })
      );
    }

    #[test]
    fn what_the_user_must_act_on_is_a_notice() {
      let unsettled = Unsettled::default().observe(OFFLINE, 0);
      assert_eq!(
        prominence_of(
          Notice::Unreachable {
            retry_in_secs: 5,
            maybe_interception: Some(NetworkInterception::CaptivePortal)
          },
          unsettled,
          0
        ),
        Prominence::Notice
      );
    }

    #[test]
    fn a_server_away_long_offers_to_try_now_but_stays_quiet() {
      let unsettled = Unsettled::default().observe(OFFLINE, 1_000);
      assert_eq!(
        prominence_of(UNREACHABLE, unsettled, ESCALATE_AFTER_MS),
        Prominence::Note(ConnectionNote::ServerUnreachable { can_retry: false })
      );
      assert_eq!(
        prominence_of(UNREACHABLE, unsettled, 1_000 + ESCALATE_AFTER_MS),
        Prominence::Note(ConnectionNote::ServerUnreachable { can_retry: true })
      );
    }
  }

  mod unreachable {
    use super::*;

    #[test]
    fn online_but_unreachable_counts_down_to_the_next_attempt() {
      let unsettled = Unsettled::default().observe(OFFLINE, 0);
      assert_eq!(
        notice_for(true, Some(OFFLINE), unsettled, 9_500),
        Some(Notice::Unreachable {
          retry_in_secs: 8,
          maybe_interception: None
        })
      );
    }

    #[test]
    fn a_captive_portal_is_named_in_the_banner() {
      let portal = CoreConnection::Offline {
        retry_at_ms: 12_000,
        maybe_interception: Some(NetworkInterception::CaptivePortal),
      };
      let unsettled = Unsettled::default().observe(portal, 0);
      assert_eq!(
        notice_for(true, Some(portal), unsettled, 10_000),
        Some(Notice::Unreachable {
          retry_in_secs: 2,
          maybe_interception: Some(NetworkInterception::CaptivePortal)
        })
      );
    }
  }
}
