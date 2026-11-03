//! Web Push (RFC 8030) with VAPID (RFC 8292) and aes128gcm encryption (RFC 8291).

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::StatusCode;
use serde::Deserialize;
use url::{Host, Url};

use super::crypto::{SubscriptionKeys, VapidKey, encrypt_web_push};
use super::sender::{PushError, PushMessage, PushSender, PushTarget, Urgency, retry_after};
use crate::auth::BoxFuture;

/// Connecting to an endpoint a user chose may take this long at most; a working push service
/// answers far sooner.
const MAX_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

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
    let subscription: Self = serde_json::from_str(token)
      .map_err(|e| PushError::Invalid(format!("not a push subscription: {}", json_error(&e))))?;
    let keys = SubscriptionKeys::from_base64url(&subscription.keys.p256dh, &subscription.keys.auth)?;
    check_endpoint(&subscription.endpoint, allow_private_endpoints)?;
    Ok((subscription, keys))
  }
}

/// Where and what kind of JSON error, without serde's quote of the input (which can be a user's
/// endpoint or, for key files, a private key).
pub(super) fn json_error(error: &serde_json::Error) -> String {
  format!(
    "{:?} error at line {}, column {}",
    error.classify(),
    error.line(),
    error.column()
  )
}

pub(super) fn check_endpoint(endpoint: &str, allow_private_endpoints: bool) -> Result<(), PushError> {
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

/// Whether the address is not a public unicast address: loopback, private, link-local, shared
/// (CGNAT), documentation, benchmarking, multicast, reserved, and the IPv6 transition ranges
/// (NAT64, 6to4, Teredo, IPv4-compatible) that can carry a private IPv4 address inside.
fn is_private_ip(ip: IpAddr) -> bool {
  match ip {
    IpAddr::V4(ip) => {
      let [a, b, c, _] = ip.octets();
      ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        // 0.0.0.0/8 "this network"
        || a == 0
        // 100.64.0.0/10 shared address space
        || (a == 100 && (64..128).contains(&b))
        // 192.0.0.0/24 IETF protocol assignments, 198.18.0.0/15 benchmarking, 192.88.99.0/24
        // deprecated 6to4 relay anycast
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 88 && c == 99)
        || (a == 198 && (b == 18 || b == 19))
        // 192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24 documentation
        || (a == 192 && b == 0 && c == 2)
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        // 240.0.0.0/4 reserved
        || a >= 240
    },
    IpAddr::V6(ip) => {
      let segments = ip.segments();
      ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        // fc00::/7 unique local, fe80::/10 link-local, fec0::/10 site-local
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] & 0xffc0) == 0xfec0
        // 64:ff9b::/96 and 64:ff9b:1::/48 NAT64, 2002::/16 6to4, 2001::/32 Teredo
        || (segments[0] == 0x64 && segments[1] == 0xff9b)
        || segments[0] == 0x2002
        || (segments[0] == 0x2001 && segments[1] == 0)
        // 2001:db8::/32 and 3fff::/20 documentation, 2001:20::/28 ORCHIDv2, 100::/64 discard-only
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || (segments[0] & 0xfff0) == 0x3ff0
        || (segments[0] == 0x2001 && (segments[1] & 0xfff0) == 0x0020)
        || (segments[0] == 0x0100 && segments[1] == 0 && segments[2] == 0 && segments[3] == 0)
        // ::a.b.c.d IPv4-compatible (and ::ffff:a.b.c.d mapped, checked as IPv4)
        || (segments[..6] == [0; 6])
        // ::ffff:0:a.b.c.d IPv4-translated (SIIT)
        || (segments[..4] == [0; 4] && segments[4] == 0xffff && segments[5] == 0)
        || ip.to_ipv4_mapped().is_some_and(|v4| is_private_ip(IpAddr::V4(v4)))
    },
  }
}

/// Resolves names for the push client and drops every non-public address, so the connection goes to
/// an address that was checked right now (no gap in which a name could point somewhere else). A name
/// that only resolves to private addresses cannot be reached at all.
struct PublicOnlyResolver;

impl reqwest::dns::Resolve for PublicOnlyResolver {
  fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
    Box::pin(async move {
      let addresses: Vec<SocketAddr> = tokio::net::lookup_host((name.as_str(), 0))
        .await?
        .filter(|address| !is_private_ip(address.ip()))
        .collect();
      if addresses.is_empty() {
        return Err(std::io::Error::other("the name resolves only to private addresses").into());
      }
      Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs)
    })
  }
}

/// The HTTP client of the push sender: no redirects (a push service could send us to an internal
/// address) and, unless private endpoints are allowed for development, name resolution that refuses
/// private addresses.
pub(super) fn build_client(
  timeout: Duration,
  allow_private_endpoints: bool,
) -> Result<reqwest::Client, reqwest::Error> {
  // No proxy from the environment (HTTP(S)_PROXY): a proxy resolves names itself, past the
  // resolver below.
  let builder = reqwest::Client::builder()
    .timeout(timeout)
    .connect_timeout(timeout.min(MAX_CONNECT_TIMEOUT))
    .no_proxy()
    .redirect(reqwest::redirect::Policy::none());
  if allow_private_endpoints {
    builder.build()
  } else {
    builder.dns_resolver(Arc::new(PublicOnlyResolver)).build()
  }
}

/// VAPID credentials (RFC 8292) for the `Authorization` header.
pub struct Vapid {
  key: VapidKey,
  /// Contact for push services, `mailto:` or `https:`.
  subject: String,
}

impl Vapid {
  pub fn new(key: VapidKey, subject: String) -> Self {
    Self { key, subject }
  }
}

pub struct WebPushSender {
  vapid: Vapid,
  allow_private_endpoints: bool,
  http: reqwest::Client,
}

impl WebPushSender {
  pub fn new(vapid: Vapid, timeout: Duration, allow_private_endpoints: bool) -> Result<Self, reqwest::Error> {
    let http = build_client(timeout, allow_private_endpoints)?;
    Ok(Self {
      vapid,
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
      post_encrypted(&self.http, &subscription, &keys, Some(&self.vapid), message).await
    })
  }
}

/// Encrypts the sealed wake-up for the subscription (RFC 8291) and posts it to its endpoint
/// (RFC 8030), with a VAPID token if there is a key. Shared by Web Push and UnifiedPush, whose
/// distributors speak the same protocol.
pub(super) async fn post_encrypted(
  http: &reqwest::Client,
  subscription: &WebPushSubscription,
  keys: &SubscriptionKeys,
  maybe_vapid: Option<&Vapid>,
  message: &PushMessage,
) -> Result<(), PushError> {
  let body = encrypt_web_push(keys, &message.sealed)?;
  let urgency = match message.urgency {
    Urgency::Normal => "normal",
    Urgency::High => "high",
  };
  let mut request = http
    .post(&subscription.endpoint)
    .header("Content-Encoding", "aes128gcm")
    .header("Content-Type", "application/octet-stream")
    .header("TTL", message.ttl.as_secs().to_string())
    .header("Urgency", urgency);
  if let Some(vapid) = maybe_vapid {
    let audience =
      origin_of(&subscription.endpoint).ok_or_else(|| PushError::Invalid("endpoint is not a URL".into()))?;
    let now = jsonwebtoken::get_current_timestamp();
    request = request.header(
      "Authorization",
      vapid.key.authorization(&audience, &vapid.subject, now)?,
    );
  }
  let response = request
    .body(body)
    .send()
    .await
    .map_err(|e| PushError::unavailable(e.without_url().to_string()))?;
  let maybe_retry_after = retry_after(response.headers());
  match response.status() {
    status if status.is_success() => Ok(()),
    StatusCode::NOT_FOUND | StatusCode::GONE => Err(PushError::Gone),
    StatusCode::TOO_MANY_REQUESTS => Err(PushError::unavailable_for("rate limited", maybe_retry_after)),
    status if status.is_server_error() => Err(PushError::unavailable_for(
      format!("status {status}"),
      maybe_retry_after,
    )),
    status => Err(PushError::Rejected(status.as_u16())),
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
      "https://[2606:4700::1111]/p",
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
      "https://[::10.0.0.1]/x",
      "https://[64:ff9b::a00:1]/x",
      "https://[2002:a00:1::1]/x",
      "https://[fec0::1]/x",
      "https://[2001:db8::1]/x",
      "https://0.1.2.3/x",
      "https://198.18.0.1/x",
      "https://224.0.0.1/x",
      "https://240.0.0.1/x",
      "https://[ff02::1]/x",
      "https://192.88.99.1/x",
      "https://[::ffff:0:a00:1]/x",
      "https://[3fff::1]/x",
      "https://[2001:20::1]/x",
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

  #[tokio::test]
  async fn names_that_resolve_to_private_addresses_are_not_connected_to() {
    // A name the textual check lets through, resolving to the loopback address: the client must
    // fail without ever reaching the listener.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let client = build_client(Duration::from_secs(2), false).unwrap();
    let result = client
      .post(format!("https://localhost:{port}/push"))
      .body("x")
      .send()
      .await;
    assert!(result.is_err());
    let accepted = tokio::time::timeout(Duration::from_millis(300), listener.accept()).await;
    assert!(accepted.is_err(), "the server connected to a private address");

    // For development the same name is reachable.
    let permissive = build_client(Duration::from_secs(2), true).unwrap();
    let attempt = tokio::spawn(async move {
      permissive
        .post(format!("https://localhost:{port}/push"))
        .body("x")
        .send()
        .await
    });
    let accepted = tokio::time::timeout(Duration::from_secs(2), listener.accept()).await;
    assert!(accepted.is_ok(), "development mode should connect");
    attempt.abort();
  }

  #[test]
  fn public_addresses_stay_reachable() {
    for ip in ["8.8.8.8", "142.250.0.1", "2606:4700::1111", "2a00:1450:4001::1"] {
      assert!(!is_private_ip(ip.parse().unwrap()), "{ip}");
    }
  }

  #[test]
  fn parse_errors_do_not_quote_the_input() {
    let secret = "https://push.example.com/SECRET-PATH";
    let error = WebPushSubscription::parse(&format!("{secret:?}"), false).unwrap_err();
    assert!(!error.to_string().contains("SECRET"), "{error}");
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
