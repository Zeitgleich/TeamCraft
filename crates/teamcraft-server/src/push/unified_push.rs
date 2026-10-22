//! UnifiedPush: a plain POST of the sealed wake-up to the endpoint the distributor (for example a
//! self-hosted ntfy) gave the app. The endpoint is chosen by the client, so it gets the same
//! checks as Web Push endpoints.

use std::time::Duration;

use reqwest::StatusCode;

use super::sender::{PushError, PushMessage, PushSender, PushTarget, Urgency};
use super::web_push::{build_client, check_endpoint};
use crate::auth::BoxFuture;

pub struct UnifiedPushSender {
  allow_private_endpoints: bool,
  http: reqwest::Client,
}

impl UnifiedPushSender {
  pub fn new(timeout: Duration, allow_private_endpoints: bool) -> Result<Self, reqwest::Error> {
    Ok(Self {
      allow_private_endpoints,
      http: build_client(timeout, allow_private_endpoints)?,
    })
  }
}

impl PushSender for UnifiedPushSender {
  fn send<'a>(
    &'a self,
    target: &'a PushTarget,
    message: &'a PushMessage,
  ) -> BoxFuture<'a, Result<(), PushError>> {
    Box::pin(async move {
      check_endpoint(&target.token, self.allow_private_endpoints)?;
      let urgency = match message.urgency {
        Urgency::Normal => "normal",
        Urgency::High => "high",
      };
      let response = self
        .http
        .post(&target.token)
        .header("Content-Type", "application/octet-stream")
        .header("TTL", message.ttl.as_secs().to_string())
        .header("Urgency", urgency)
        .body(message.sealed.clone())
        .send()
        .await
        .map_err(|e| PushError::Unavailable(e.without_url().to_string()))?;
      match response.status() {
        status if status.is_success() => Ok(()),
        StatusCode::NOT_FOUND | StatusCode::GONE => Err(PushError::Gone),
        StatusCode::TOO_MANY_REQUESTS => Err(PushError::Unavailable("rate limited".into())),
        status if status.is_server_error() => Err(PushError::Unavailable(format!("status {status}"))),
        status => Err(PushError::Rejected(status.as_u16())),
      }
    })
  }
}
