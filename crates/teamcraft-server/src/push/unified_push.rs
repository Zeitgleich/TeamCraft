//! UnifiedPush: the distributor (for example a self-hosted ntfy) gives the app an endpoint and Web
//! Push keys, and the server sends exactly as for Web Push: the sealed wake-up encrypted with
//! RFC 8291, posted per RFC 8030, VAPID-signed when the server has a key (distributors may ignore
//! it). The endpoint is chosen by the client, so it gets the same checks as Web Push endpoints.

use std::time::Duration;

use super::sender::{PushError, PushMessage, PushSender, PushTarget};
use super::web_push::{Vapid, WebPushSubscription, build_client, post_encrypted};
use crate::auth::BoxFuture;

pub struct UnifiedPushSender {
  maybe_vapid: Option<Vapid>,
  allow_private_endpoints: bool,
  http: reqwest::Client,
}

impl UnifiedPushSender {
  pub fn new(
    maybe_vapid: Option<Vapid>,
    timeout: Duration,
    allow_private_endpoints: bool,
  ) -> Result<Self, reqwest::Error> {
    Ok(Self {
      maybe_vapid,
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
      // The registration is the endpoint with its keys, in the Web Push subscription format.
      let (subscription, keys) = WebPushSubscription::parse(&target.token, self.allow_private_endpoints)?;
      post_encrypted(
        &self.http,
        &subscription,
        &keys,
        self.maybe_vapid.as_ref(),
        message,
      )
      .await
    })
  }
}
