//! Web Push (RFC 8030) with VAPID (RFC 8292) and aes128gcm encryption (RFC 8291).

use std::net::IpAddr;
use std::time::Duration;

use reqwest::StatusCode;
use serde::Deserialize;
use url::{Host, Url};

use super::crypto::{SubscriptionKeys, VapidKey, encrypt_web_push};
use super::sender::{PushError, PushMessage, PushSender, PushTarget, Urgency};
use crate::auth::BoxFuture;

/// A browser's `PushSubscription` as the client registers it.
#[derive(Debug, Clone, Deserialize)]
pub struct WebPushSubscription {
  pub endpoint: String,
  pub keys: WebPushSubscriptionKeys,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WebPushSubscriptionKeys {
  pub p256dh: String,
  pub auth: String,
}

impl WebPushSubscription {
  /// Parses and validates the registration token. The endpoint is chosen by the client and the server
  /// will POST to it, so it must be an https URL that doesn't point into private networks (unless
  /// `allow_private_endpoints` is set for development).
  pub fn parse(token: &str, allow_private_endpoints: bool) -> Result<(Self, SubscriptionKeys), PushError> {
    let subscription: Self =
      serde_json::from_str(token).map_err(|e| PushError::Invalid(format!("not a push subscription: {e}")))?;
    let keys = SubscriptionKeys::from_base64url(&subscription.keys.p256dh, &subscription.keys.auth)?;
    check_endpoint(&subscription.endpoint, allow_private_endpoints)?;
    Ok((subscription, keys))
  }
}

fn check_endpoint(endpoint: &str, allow_private_endpoints: bool) -> Result<(), PushError> {
  let url: Url = endpoint
    .parse()
    .map_err(|_| PushError::Invalid("endpoint is not a URL".into()))?;
  if allow_private_endpoints {
    return if matches!(url.scheme(), "http" | "https") {
      Ok(())
    } else {
      Err(PushError::Invalid("endpoint scheme".into()))
    };
  }
  if url.scheme() != "https" {
    return Err(PushError::Invalid("endpoint must use https".into()));
  }
  if !url.username().is_empty() || url.password().is_some() {
    return Err(PushError::Invalid("endpoint must not contain credentials".into()));
  }
  let is_private = match url.host() {
    Some(Host::Domain(name)) => {
      let name = name.to_ascii_lowercase();
      name == "localhost"
        || name.ends_with(".localhost")
        || name.ends_with(".local")
        || name.ends_with(".internal")
    },
    Some(Host::Ipv4(ip)) => is_private_ip(IpAddr::V4(ip)),
    Some(Host::Ipv6(ip)) => is_private_ip(IpAddr::V6(ip)),
    None => true,
  };
  if is_private {
    Err(PushError::Invalid("endpoint points to a private address".into()))
  } else {
    Ok(())
  }
}

/// `scheme://host[:port]` of a URL, the VAPID audience (RFC 8292 §2).
fn origin_of(endpoint: &str) -> Option<String> {
  let url: Url = endpoint.parse().ok()?;
  let host = url.host_str()?;
  let port = url.port().map(|port| format!(":{port}")).unwrap_or_default();
  Some(format!("{}://{host}{port}", url.scheme()))
}

fn is_private_ip(ip: IpAddr) -> bool {
  match ip {
    IpAddr::V4(ip) => {
      ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.octets()[0] == 100 && (64..128).contains(&ip.octets()[1])
    },
    IpAddr::V6(ip) => {
      ip.is_loopback()
        || ip.is_unspecified()
        || (ip.segments()[0] & 0xfe00) == 0xfc00
        || (ip.segments()[0] & 0xffc0) == 0xfe80
        || ip
          .to_ipv4_mapped()
          .is_some_and(|v4| is_private_ip(IpAddr::V4(v4)))
    },
  }
}

pub struct WebPushSender {
  vapid: VapidKey,
  subject: String,
  allow_private_endpoints: bool,
  http: reqwest::Client,
}

impl WebPushSender {
  pub fn new(
    vapid: VapidKey,
    subject: String,
    timeout: Duration,
    allow_private_endpoints: bool,
  ) -> Result<Self, reqwest::Error> {
    // Redirects would let a push service send us to internal addresses; push services don't need them.
    let http = reqwest::Client::builder()
      .timeout(timeout)
      .redirect(reqwest::redirect::Policy::none())
      .build()?;
    Ok(Self {
      vapid,
      subject,
      allow_private_endpoints,
      http,
    })
  }
}

impl PushSender for WebPushSender {
  fn send<'a>(
    &'a self,
    target: &'a PushTarget,
    message: &'a PushMessage,
  ) -> BoxFuture<'a, Result<(), PushError>> {
    Box::pin(async move {
      let (subscription, keys) = WebPushSubscription::parse(&target.token, self.allow_private_endpoints)?;
      let body = encrypt_web_push(&keys, &message.sealed)?;
      let now = jsonwebtoken::get_current_timestamp();
      let audience = origin_of(&subscription.endpoint)
        .ok_or_else(|| PushError::Invalid("endpoint is not a URL".into()))?;
      let authorization = self.vapid.authorization(&audience, &self.subject, now)?;
      let urgency = match message.urgency {
        Urgency::Normal => "normal",
        Urgency::High => "high",
      };

      let response = self
        .http
        .post(&subscription.endpoint)
        .header("Authorization", authorization)
        .header("Content-Encoding", "aes128gcm")
        .header("Content-Type", "application/octet-stream")
        .header("TTL", message.ttl.as_secs().to_string())
        .header("Urgency", urgency)
        .body(body)
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

#[cfg(test)]
mod tests {
  use super::*;

  const P256DH: &str =
    "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
  const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";

  fn token(endpoint: &str) -> String {
    format!(r#"{{"endpoint":"{endpoint}","keys":{{"p256dh":"{P256DH}","auth":"{AUTH}"}}}}"#)
  }

  #[test]
  fn the_vapid_audience_is_the_origin_of_the_endpoint() {
    assert_eq!(
      origin_of("https://push.example.com:8443/send/abc?x=1").as_deref(),
      Some("https://push.example.com:8443")
    );
    assert_eq!(
      origin_of("https://fcm.googleapis.com/fcm/send/abc").as_deref(),
      Some("https://fcm.googleapis.com")
    );
    assert_eq!(origin_of("not a url"), None);
  }

  #[test]
  fn public_https_endpoints_are_accepted() {
    for endpoint in [
      "https://fcm.googleapis.com/fcm/send/abc",
      "https://updates.push.services.mozilla.com/wpush/v2/x",
      "https://[2001:db8::1]/p",
    ] {
      assert!(
        WebPushSubscription::parse(&token(endpoint), false).is_ok(),
        "{endpoint}"
      );
    }
  }

  #[test]
  fn endpoints_into_private_networks_are_refused() {
    for endpoint in [
      "http://push.example.com/x",
      "https://localhost/x",
      "https://service.internal/x",
      "https://127.0.0.1/x",
      "https://10.1.2.3/x",
      "https://192.168.0.5/x",
      "https://169.254.169.254/latest/meta-data",
      "https://100.64.0.1/x",
      "https://[::1]/x",
      "https://[fd00::1]/x",
      "https://[::ffff:10.0.0.1]/x",
      "https://user:pw@push.example.com/x",
      "ftp://push.example.com/x",
      "not a url",
    ] {
      assert!(
        WebPushSubscription::parse(&token(endpoint), false).is_err(),
        "{endpoint}"
      );
    }
  }

  #[test]
  fn development_may_use_local_endpoints() {
    assert!(WebPushSubscription::parse(&token("http://127.0.0.1:9000/push"), true).is_ok());
    assert!(WebPushSubscription::parse(&token("ftp://127.0.0.1/push"), true).is_err());
  }

  #[test]
  fn malformed_subscriptions_are_refused() {
    for bad in [
      "{}",
      "not json",
      r#"{"endpoint":"https://a.example.com","keys":{"p256dh":"AAAA","auth":"AAAA"}}"#,
    ] {
      assert!(WebPushSubscription::parse(bad, false).is_err(), "{bad}");
    }
  }
}
