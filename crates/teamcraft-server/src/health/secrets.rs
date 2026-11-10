//! When the secrets the server holds expire, where that can be known (#1647):
//!
//! - the FCM service account key: Google publishes a certificate for each of the account's keys,
//!   valid until the key expires; read twice a day (one small public GET);
//! - the identity provider's client secret of the server's service account: Keycloak's secret
//!   rotation keeps the expiry in the client's attributes, read with every sync run
//!   (`services::oidc`).
//!
//! The APNs key (`.p8`) does not expire (Apple revokes, it doesn't time out), and the push gateway
//! key is the server's own: neither is counted. The check `secret_expiry` shows the days left of
//! the soonest one.

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tracing::{debug, warn};

use crate::push::FcmKeyExpiry;
use crate::state::AppState;

/// How often the FCM key's certificate is read: an expiry is set weeks ahead.
const FCM_READ_INTERVAL: Duration = Duration::from_secs(12 * 3600);
/// After a failed read.
const FCM_RETRY_INTERVAL: Duration = Duration::from_secs(3600);
const FCM_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// What is known about one secret's expiry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SecretReading {
  /// Not read yet, or the read failed.
  #[default]
  Unknown,
  /// The secret has no expiry.
  NoExpiry,
  /// Valid until then (Unix seconds).
  ExpiresAt(i64),
  /// The provider no longer knows it: as good as expired.
  Gone,
}

/// The FCM key's reading; the client secret's is kept by the sync ([`crate::services::oidc`]).
#[derive(Debug, Default)]
pub struct SecretExpiries {
  fcm: Mutex<SecretReading>,
}

impl SecretExpiries {
  pub fn fcm(&self) -> SecretReading {
    *self.lock()
  }

  fn set_fcm(&self, reading: SecretReading) {
    *self.lock() = reading;
  }

  fn lock(&self) -> MutexGuard<'_, SecretReading> {
    self.fcm.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

/// Reads the FCM key's expiry now and then, when FCM is sent directly with a key whose expiry can
/// be read.
pub fn spawn(state: AppState) {
  let Some(key) = state.push.fcm_key().cloned() else {
    return;
  };
  tokio::spawn(async move {
    let http = match reqwest::Client::builder()
      .timeout(FCM_READ_TIMEOUT)
      .redirect(reqwest::redirect::Policy::none())
      .build()
    {
      Ok(http) => http,
      Err(error) => {
        warn!(%error, "no HTTP client for reading the FCM key's expiry");
        return;
      },
    };
    loop {
      let wait = match key.read_expiry(&http).await {
        Ok(expiry) => {
          state.health.secrets().set_fcm(match expiry {
            FcmKeyExpiry::ExpiresAt(at) => SecretReading::ExpiresAt(at),
            FcmKeyExpiry::Gone => SecretReading::Gone,
          });
          FCM_READ_INTERVAL
        },
        Err(error) => {
          // The last reading stays: an expiry doesn't move because Google didn't answer once.
          debug!(%error, "reading the FCM key's expiry failed");
          FCM_RETRY_INTERVAL
        },
      };
      tokio::time::sleep(wait).await;
    }
  });
}
