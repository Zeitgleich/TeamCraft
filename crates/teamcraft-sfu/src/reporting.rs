//! Tells `teamcraft-server` who joined and left a call, so it can start the call, ring DMs and
//! show the roster (`POST /api/internal/v1/calls/participants/{joined,left}`).
//!
//! Reports are fire-and-forget: a failed report is logged and media keeps flowing. The server
//! authenticates the SFU with its registration token.

use std::fmt;
use std::sync::Arc;

use teamcraft_proto::SfuParticipantReport;
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

/// Reports to `teamcraft-server` over HTTP.
pub struct HttpReporter {
  client: reqwest::Client,
  base_url: String,
  registration_token: Arc<str>,
}

impl ParticipantReporter for NoReporter {
  fn joined(&self, _grant: JoinGrant) {}
  fn left(&self, _grant: JoinGrant) {}
}

impl HttpReporter {
  /// `base_url`: the server's base URL, e.g. `https://chat.example.com`.
  pub fn new(base_url: impl Into<String>, registration_token: impl Into<Arc<str>>) -> Self {
    Self {
      client: reqwest::Client::new(),
      base_url: base_url.into().trim_end_matches('/').to_owned(),
      registration_token: registration_token.into(),
    }
  }

  fn report(&self, kind: &'static str, grant: JoinGrant) {
    let url = format!("{}/api/internal/v1/calls/participants/{kind}", self.base_url);
    let request = self
      .client
      .post(url)
      .bearer_auth(&*self.registration_token)
      .json(&SfuParticipantReport {
        call_id: grant.call_id,
        user_id: grant.user_id,
        device_id: grant.device_id,
      });
    tokio::spawn(async move {
      match request.send().await {
        Ok(response) if response.status().is_success() => debug!(kind, "participant reported"),
        Ok(response) => warn!(kind, status = %response.status(), "server rejected participant report"),
        Err(error) => warn!(kind, %error, "participant report failed"),
      }
    });
  }
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
