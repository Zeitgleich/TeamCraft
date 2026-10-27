//! What brings the app back after a wake-up or a network change (issue #883): the token refresh
//! loop, the "network is back" signal and what it resets, and a probe for platforms that have no
//! such signal.
//!
//! Sources of the signal: logind `PrepareForSleep` and NetworkManager `StateChanged` on Linux
//! (`freedesktop.rs`), the native resume and network hooks on macOS and Windows
//! (`teamcraft-desktop-native`), the webview's `online` event (`core_network_changed`), and
//! [`probe_while_offline`] where no OS signal for the network could be hooked.

use std::sync::Arc;
use std::time::Duration;

use tauri::Manager;
use teamcraft_core::connection::ConnectionState;
use teamcraft_core::retry::{Jitter, RetryPolicy};
use tokio::sync::Notify;

use crate::auth::{AuthError, AuthService, KeyringStore};
use crate::calls::CallHost;
use crate::core::commands::CoreHost;

/// How often the login session's access token is checked and refreshed before it runs out.
const TOKEN_REFRESH: Duration = Duration::from_secs(2);
/// The refresh loop's backoff while the identity provider can't be reached: from the normal check
/// up to a minute, so an outage isn't hammered every two seconds.
const REFRESH_BACKOFF: RetryPolicy = RetryPolicy::new(TOKEN_REFRESH, Duration::from_secs(60));
/// How often [`probe_while_offline`] looks whether the server can be reached again, while the
/// connection is down. Short, because it replaces a network-up signal; it does nothing otherwise.
const PROBE_INTERVAL: Duration = Duration::from_secs(3);
/// A TCP connection to the server that takes longer than this counts as "not yet".
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Wakes the token refresh loop early, after a wake-up or when the network is back.
#[derive(Clone, Default)]
pub struct RefreshNow(Arc<Notify>);

/// When the refresh loop checks the token next.
struct RefreshPacing {
  failures: u32,
  jitter: Jitter,
}

/// Whether the server answered the last probe; the signal fires on the change from no to yes.
#[derive(Default)]
struct OfflineProbe {
  maybe_was_reachable: Option<bool>,
}

impl RefreshNow {
  fn notify(&self) {
    self.0.notify_one();
  }
}

/// The network may be back (a wake-up, NetworkManager, the webview, the probe): pooled HTTP
/// connections are dropped (a suspend leaves them dead, and a request on one hangs until the
/// keep-alive gives up), the token is checked against the wall clock and refreshed if it ran out
/// meanwhile, and the gateway and a running call reconnect at once instead of after the backoff.
pub fn network_back(app: &tauri::AppHandle) {
  app.state::<Arc<AuthService<KeyringStore>>>().drop_connections();
  app.state::<RefreshNow>().notify();
  app.state::<CoreHost>().network_changed();
  app.state::<Arc<CallHost>>().network_changed();
}

/// Keeps the session's access token fresh: checked every [`TOKEN_REFRESH`] (it refreshes close to
/// expiry; the core reads the result from the session), at once on [`RefreshNow`], and with a
/// growing pause while the identity provider can't be reached. A refused refresh token ends the
/// session in [`AuthService`]; nothing here retries a refusal.
pub fn keep_tokens_fresh(service: Arc<AuthService<KeyringStore>>, now: RefreshNow) {
  tauri::async_runtime::spawn(async move {
    let mut pacing = RefreshPacing::new(seed());
    loop {
      let result = service.server_bearer_token().await;
      let delay = pacing.after(result.as_ref().map(|_| ()));
      if let Err(error) = &result
        && pacing.failures > 0
      {
        tracing::debug!(
          kind = error.kind(),
          ?delay,
          "token refresh failed; trying again later"
        );
      }
      tokio::select! {
        () = tokio::time::sleep(delay) => {},
        // A wake-up or a network change: a pause from before no longer says anything.
        () = now.0.notified() => pacing.failures = 0,
      }
    }
  });
}

/// While the gateway is offline, tries a TCP connection to the server every [`PROBE_INTERVAL`] and
/// reports [`network_back`] when it works again after it failed: the network-up signal where the
/// OS gives none (Linux without NetworkManager, a native hook that failed to install), so the
/// reconnect doesn't wait for the backoff. Connected or signed out, it only reads the state.
pub fn probe_while_offline(app: tauri::AppHandle) {
  tauri::async_runtime::spawn(async move {
    let mut probe = OfflineProbe::default();
    let mut ticks = tokio::time::interval(PROBE_INTERVAL);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
      ticks.tick().await;
      let is_offline = matches!(
        app.state::<CoreHost>().connection_state(),
        Some(ConnectionState::Offline { .. })
      );
      let maybe_server = app
        .state::<Arc<AuthService<KeyringStore>>>()
        .active_server()
        .map(|server| server.server_url);
      let (true, Some(server_url)) = (is_offline, maybe_server) else {
        probe.reset();
        continue;
      };
      if probe.observe(is_reachable(&server_url).await) {
        tracing::info!("the server is reachable again");
        network_back(&app);
      }
    }
  });
}

impl RefreshPacing {
  fn new(seed: u64) -> Self {
    Self {
      failures: 0,
      jitter: Jitter::new(seed),
    }
  }

  /// The wait after a check that ended with `result`. Signed out, or a refused token, is the
  /// normal check again (nothing to retry); no answer from the provider backs off.
  fn after(&mut self, result: Result<(), &AuthError>) -> Duration {
    match result {
      Err(AuthError::Network(_) | AuthError::InvalidResponse(_)) => {
        let delay = REFRESH_BACKOFF.delay(self.failures, &mut self.jitter, None);
        self.failures = self.failures.saturating_add(1);
        delay
      },
      Ok(()) | Err(_) => {
        self.failures = 0;
        TOKEN_REFRESH
      },
    }
  }
}

impl OfflineProbe {
  /// Records a probe; `true` when the server is reachable again after a failed probe. The first
  /// probe never fires: reachable right away means the network was not the problem.
  fn observe(&mut self, is_reachable: bool) -> bool {
    let is_back = self.maybe_was_reachable == Some(false) && is_reachable;
    self.maybe_was_reachable = Some(is_reachable);
    is_back
  }

  fn reset(&mut self) {
    self.maybe_was_reachable = None;
  }
}

/// Whether a TCP connection to the server's host and port opens within [`PROBE_TIMEOUT`].
async fn is_reachable(server_url: &str) -> bool {
  let Ok(url) = url::Url::parse(server_url) else {
    return false;
  };
  let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else {
    return false;
  };
  let host = host.trim_start_matches('[').trim_end_matches(']');
  matches!(
    tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect((host, port))).await,
    Ok(Ok(_))
  )
}

/// Spread for the backoff, not a secret.
fn seed() -> u64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(1, |elapsed| u64::from(elapsed.subsec_nanos()))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn an_unreachable_provider_backs_off_and_a_success_resets() {
    let mut pacing = RefreshPacing::new(7);
    let network = AuthError::Network("no route".into());
    let first = pacing.after(Err(&network));
    let second = pacing.after(Err(&network));
    assert!(first >= TOKEN_REFRESH / 2 && first <= TOKEN_REFRESH, "{first:?}");
    assert!(
      second >= TOKEN_REFRESH && second <= TOKEN_REFRESH * 2,
      "{second:?}"
    );
    for _ in 0..20 {
      assert!(pacing.after(Err(&network)) <= Duration::from_secs(60));
    }
    assert_eq!(pacing.after(Ok(())), TOKEN_REFRESH);
    assert_eq!(pacing.failures, 0);
  }

  #[test]
  fn signed_out_or_refused_is_no_reason_to_back_off() {
    let mut pacing = RefreshPacing::new(7);
    assert_eq!(pacing.after(Err(&AuthError::NotSignedIn)), TOKEN_REFRESH);
    assert_eq!(
      pacing.after(Err(&AuthError::Denied("invalid_grant".into()))),
      TOKEN_REFRESH
    );
    assert_eq!(pacing.failures, 0);
  }

  #[test]
  fn the_probe_fires_once_when_the_server_comes_back() {
    let mut probe = OfflineProbe::default();
    assert!(!probe.observe(true), "reachable at once: not a network problem");
    assert!(!probe.observe(false));
    assert!(!probe.observe(false));
    assert!(probe.observe(true));
    assert!(!probe.observe(true), "only the change counts");
    probe.reset();
    assert!(!probe.observe(true));
  }

  #[tokio::test]
  async fn a_closed_port_is_not_reachable_and_an_open_one_is() {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, 0))
      .await
      .or(tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await)
      .unwrap();
    let address = listener.local_addr().unwrap();
    let url = match address {
      std::net::SocketAddr::V6(v6) => format!("http://[::1]:{}", v6.port()),
      std::net::SocketAddr::V4(v4) => format!("http://127.0.0.1:{}", v4.port()),
    };
    assert!(is_reachable(&url).await);
    drop(listener);
    assert!(!is_reachable(&url).await);
  }
}
