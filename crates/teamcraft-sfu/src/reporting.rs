//! Tells `teamcraft-server` who joined and left a call, so it can start the call, ring DMs and
//! show the roster (`POST /api/internal/v1/calls/participants/{joined,left}`).
//!
//! Reports never hold up media: they go into a bounded queue that one task sends in order, so a
//! leave never overtakes the join before it (a quick rejoin of the same device would otherwise
//! leave a stale entry on the server). A report the server couldn't take (network error, 5xx, 429)
//! is retried with back-off. The server authenticates the SFU with its registration token.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use reqwest::StatusCode;
use teamcraft_proto::SfuParticipantReport;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::auth::JoinGrant;

/// Receives join and leave notifications.
pub trait ParticipantReporter: Send + Sync {
  fn joined(&self, grant: JoinGrant);
  fn left(&self, grant: JoinGrant);
}

/// Reports nothing (development without a server, tests).
#[derive(Debug, Default)]
pub struct NoReporter;

/// Reports waiting to be sent; beyond this the oldest news is no longer worth keeping memory for.
const QUEUE_CAPACITY: usize = 4096;
/// Tries per report, the first included.
const MAX_ATTEMPTS: u32 = 6;
const FIRST_RETRY_DELAY: Duration = Duration::from_millis(500);

/// Reports to `teamcraft-server` over HTTP.
pub struct HttpReporter {
  queue: mpsc::Sender<(&'static str, SfuParticipantReport)>,
  base_url: String,
}

/// Sends the queued reports one after the other.
struct ReportSender {
  client: reqwest::Client,
  base_url: String,
  registration_token: Arc<str>,
}

impl ParticipantReporter for NoReporter {
  fn joined(&self, _grant: JoinGrant) {}
  fn left(&self, _grant: JoinGrant) {}
}

impl HttpReporter {
  /// `base_url`: the server's base URL, e.g. `https://chat.example.com`. Starts the sending task,
  /// so it must be called inside the Tokio runtime.
  pub fn new(base_url: impl Into<String>, registration_token: impl Into<Arc<str>>) -> Self {
    let base_url = base_url.into().trim_end_matches('/').to_owned();
    let (queue, reports) = mpsc::channel(QUEUE_CAPACITY);
    let sender = ReportSender {
      client: reqwest::Client::new(),
      base_url: base_url.clone(),
      registration_token: registration_token.into(),
    };
    tokio::spawn(sender.run(reports));
    Self { queue, base_url }
  }

  fn report(&self, kind: &'static str, grant: JoinGrant) {
    let report = SfuParticipantReport {
      call_id: grant.call_id,
      user_id: grant.user_id,
      device_id: grant.device_id,
    };
    if let Err(error) = self.queue.try_send((kind, report)) {
      warn!(kind, %error, "participant report dropped: the queue is full or closed");
    }
  }
}

impl ReportSender {
  async fn run(self, mut reports: mpsc::Receiver<(&'static str, SfuParticipantReport)>) {
    while let Some((kind, report)) = reports.recv().await {
      self.send(kind, &report).await;
    }
  }

  async fn send(&self, kind: &'static str, report: &SfuParticipantReport) {
    let url = format!("{}/api/internal/v1/calls/participants/{kind}", self.base_url);
    let mut delay = FIRST_RETRY_DELAY;
    for attempt in 1..=MAX_ATTEMPTS {
      let result = self
        .client
        .post(&url)
        .bearer_auth(&*self.registration_token)
        .json(report)
        .send()
        .await;
      match result {
        Ok(response) if response.status().is_success() => {
          debug!(kind, "participant reported");
          return;
        },
        Ok(response) if !is_worth_retrying(response.status()) => {
          warn!(kind, status = %response.status(), "server rejected participant report");
          return;
        },
        Ok(response) => warn!(kind, attempt, status = %response.status(), "participant report failed"),
        Err(error) => warn!(kind, attempt, %error, "participant report failed"),
      }
      if attempt < MAX_ATTEMPTS {
        tokio::time::sleep(delay).await;
        delay *= 2;
      }
    }
    warn!(kind, "participant report given up");
  }
}

/// Server errors and rate limiting may pass; anything else the server will answer the same again.
fn is_worth_retrying(status: StatusCode) -> bool {
  status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS
}

impl ParticipantReporter for HttpReporter {
  fn joined(&self, grant: JoinGrant) {
    self.report("joined", grant);
  }

  fn left(&self, grant: JoinGrant) {
    self.report("left", grant);
  }
}

// The registration token is a secret.
impl fmt::Debug for HttpReporter {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("HttpReporter")
      .field("base_url", &self.base_url)
      .finish_non_exhaustive()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_server_errors_and_rate_limits_are_retried() {
    assert!(is_worth_retrying(StatusCode::BAD_GATEWAY));
    assert!(is_worth_retrying(StatusCode::SERVICE_UNAVAILABLE));
    assert!(is_worth_retrying(StatusCode::TOO_MANY_REQUESTS));
    assert!(!is_worth_retrying(StatusCode::NOT_FOUND));
    assert!(!is_worth_retrying(StatusCode::UNAUTHORIZED));
  }
}
