//! Which HTTP proxy a native connection goes through (issue #157), the same answer for REST
//! (reqwest) and the WebSockets (gateway, device-link relay), so a mandatory company proxy doesn't
//! leave REST working and realtime dead.
//!
//! Sources, first match wins: the `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` / `NO_PROXY`
//! environment variables, then the operating system's settings where hyper-util reads them (macOS
//! System Settings, the Windows registry). Android and iOS hand theirs to [`set_system_proxy`],
//! since the platform keeps them out of reach of native code. REST takes `http://` and `https://`
//! proxies; the WebSockets tunnel with `CONNECT` through `http://` ones only. SOCKS proxies are not
//! used (a warning, once, and the connection goes direct). PAC scripts are never evaluated: the shells
//! pass no proxy for a network that only has one.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hyper_util::client::proxy::matcher::Matcher;
use url::Url;

/// A proxy the platform reports (Android `ProxyInfo`, iOS `CFNetworkCopySystemProxySettings`).
#[derive(Clone, PartialEq, Eq)]
pub struct SystemProxy {
  pub host: String,
  pub port: u16,
  /// Hosts that bypass the proxy, in `NO_PROXY` syntax (`example.com`, `.example.com`, `10.0.0.0/8`).
  pub exclusions: Vec<String>,
}

/// Replaces the platform's proxy (`None`: there is none); later connections use it. The
/// environment variables still win, as everywhere else.
pub fn set_system_proxy(maybe_proxy: Option<SystemProxy>) {
  let matcher = match maybe_proxy {
    Some(proxy) if !has_proxy_environment() => {
      let host = match proxy.host.parse::<std::net::Ipv6Addr>() {
        Ok(_) => format!("[{}]", proxy.host),
        Err(_) => proxy.host.clone(),
      };
      Matcher::builder()
        .all(format!("http://{host}:{}", proxy.port))
        .no(no_proxy_list(&proxy.exclusions))
        .build()
    },
    _ => Matcher::from_system(),
  };
  let mut current = matchers()
    .write()
    .unwrap_or_else(std::sync::PoisonError::into_inner);
  *current = Arc::new(matcher);
}

/// The exclusions in `NO_PROXY` syntax: platforms write `*.example.com` (Android, iOS) where
/// `NO_PROXY` says `.example.com`, and iOS shortens networks to `169.254/16`.
fn no_proxy_list(exclusions: &[String]) -> String {
  exclusions
    .iter()
    .map(|entry| entry.trim())
    .filter(|entry| !entry.is_empty())
    .map(|entry| {
      let entry = entry
        .strip_prefix('*')
        .filter(|rest| rest.starts_with('.'))
        .unwrap_or(entry);
      full_ipv4_network(entry).unwrap_or_else(|| entry.to_owned())
    })
    .collect::<Vec<_>>()
    .join(",")
}

/// `169.254/16` as `169.254.0.0/16`; `None` for anything that isn't a shortened IPv4 network.
fn full_ipv4_network(entry: &str) -> Option<String> {
  let (address, prefix) = entry.split_once('/')?;
  prefix.parse::<u8>().ok().filter(|bits| *bits <= 32)?;
  let octets: Vec<&str> = address.split('.').collect();
  let is_octet = |octet: &&str| octet.parse::<u8>().is_ok();
  if octets.len() >= 4 || !octets.iter().all(is_octet) {
    return None;
  }
  let mut full = octets.join(".");
  for _ in octets.len()..4 {
    full.push_str(".0");
  }
  Some(format!("{full}/{prefix}"))
}

/// The proxy for an HTTP request to `target` (`http` or `https`), with its credentials in the URL's
/// user info; `None` to connect directly.
pub(crate) fn proxy_for(target: &Url) -> Option<Url> {
  let matcher = matchers()
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone();
  route(&matcher, target)
}

/// The proxy a WebSocket to `target` (`ws` or `wss`) tunnels through: only `http://` proxies speak
/// `CONNECT` here; with an `https://` one the WebSocket goes direct (REST still uses it).
pub(crate) fn tunnel_proxy_for(target: &Url) -> Option<Url> {
  tunnel_only(proxy_for(target)?)
}

fn tunnel_only(proxy: Url) -> Option<Url> {
  if proxy.scheme() == "http" {
    return Some(proxy);
  }
  static WARNED: AtomicBool = AtomicBool::new(false);
  if !WARNED.swap(true, Ordering::Relaxed) {
    tracing::warn!(
      scheme = proxy.scheme(),
      "WebSockets tunnel through http:// proxies only; connecting them directly"
    );
  }
  None
}

fn route(matcher: &Matcher, target: &Url) -> Option<Url> {
  let scheme = match target.scheme() {
    "https" | "wss" => "https",
    _ => "http",
  };
  let host = target.host_str()?;
  let uri: http::Uri = format!("{scheme}://{host}/").parse().ok()?;
  let intercept = matcher.intercept(&uri)?;
  let mut proxy = Url::parse(&intercept.uri().to_string()).ok()?;
  if !matches!(proxy.scheme(), "http" | "https") {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
      tracing::warn!(
        scheme = proxy.scheme(),
        "only http:// and https:// proxies are supported; connecting directly"
      );
    }
    return None;
  }
  let maybe_credentials = match intercept.raw_auth() {
    Some((user, password)) => Some((user.to_owned(), password.to_owned())),
    None => intercept.basic_auth().and_then(basic_credentials),
  };
  if let Some((user, password)) = maybe_credentials {
    // A URL with a host always takes user info.
    let _ = proxy.set_username(&user);
    let _ = proxy.set_password(Some(&password));
  }
  Some(proxy)
}

/// hyper-util keeps the `user:password@` of an `http://` proxy only as a ready `Basic` header.
fn basic_credentials(header: &http::HeaderValue) -> Option<(String, String)> {
  let encoded = header.to_str().ok()?.strip_prefix("Basic ")?;
  let decoded = STANDARD.decode(encoded).ok()?;
  let text = String::from_utf8(decoded).ok()?;
  let (user, password) = text.split_once(':')?;
  Some((user.to_owned(), password.to_owned()))
}

fn matchers() -> &'static RwLock<Arc<Matcher>> {
  static MATCHER: OnceLock<RwLock<Arc<Matcher>>> = OnceLock::new();
  MATCHER.get_or_init(|| RwLock::new(Arc::new(Matcher::from_system())))
}

fn has_proxy_environment() -> bool {
  [
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "ALL_PROXY",
    "all_proxy",
  ]
  .iter()
  .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

impl std::fmt::Debug for SystemProxy {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("SystemProxy")
      .field("host", &self.host)
      .field("port", &self.port)
      .field("exclusions", &self.exclusions.len())
      .finish()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn https_and_wss_go_through_the_https_proxy_and_exclusions_go_direct() {
    let matcher = Matcher::builder()
      .https("http://proxy.example.com:3128")
      .no("intranet.example.com")
      .build();
    for target in [
      "https://chat.example.com/api/v1/me",
      "wss://chat.example.com/api/v1/gateway",
    ] {
      let proxy = route(&matcher, &Url::parse(target).unwrap()).unwrap();
      assert_eq!(proxy.as_str(), "http://proxy.example.com:3128/", "{target}");
    }
    let excluded = Url::parse("wss://intranet.example.com/api/v1/gateway").unwrap();
    assert_eq!(route(&matcher, &excluded), None);
    // Only an https proxy is configured: plain http goes direct.
    let plain = Url::parse("ws://chat.example.com/api/v1/gateway").unwrap();
    assert_eq!(route(&matcher, &plain), None);
  }

  #[test]
  fn proxy_credentials_survive_the_route() {
    let matcher = Matcher::builder()
      .all("http://user:secret@proxy.example.com:8080")
      .build();
    let proxy = route(&matcher, &Url::parse("wss://chat.example.com/").unwrap()).unwrap();
    assert_eq!(proxy.username(), "user");
    assert_eq!(proxy.password(), Some("secret"));
    assert_eq!(proxy.host_str(), Some("proxy.example.com"));
  }

  #[test]
  fn platform_exclusions_bypass_the_proxy() {
    let exclusions = vec![
      "*.intranet.example.com".to_owned(),
      " localhost ".to_owned(),
      String::new(),
    ];
    assert_eq!(no_proxy_list(&exclusions), ".intranet.example.com,localhost");
    let matcher = Matcher::builder()
      .all("http://proxy.example.com:3128")
      .no(no_proxy_list(&exclusions))
      .build();
    let inside = Url::parse("wss://chat.intranet.example.com/").unwrap();
    assert_eq!(route(&matcher, &inside), None);
    assert!(route(&matcher, &Url::parse("wss://chat.example.com/").unwrap()).is_some());
  }

  #[test]
  fn https_proxies_serve_rest_but_not_the_websocket_tunnel() {
    let matcher = Matcher::builder().all("https://proxy.example.com:3129").build();
    let proxy = route(
      &matcher,
      &Url::parse("https://chat.example.com/api/v1/me").unwrap(),
    )
    .unwrap();
    assert_eq!(proxy.scheme(), "https");
    assert_eq!(tunnel_only(proxy), None);
    let plain = Url::parse("http://proxy.example.com:3128").unwrap();
    assert_eq!(tunnel_only(plain.clone()), Some(plain));
  }

  #[test]
  fn shortened_ipv4_networks_are_completed() {
    let exclusions = ["169.254/16", "10/8", "192.168.1.0/24", "fe80::/10", "1.2.3/40"].map(String::from);
    assert_eq!(
      no_proxy_list(&exclusions),
      "169.254.0.0/16,10.0.0.0/8,192.168.1.0/24,fe80::/10,1.2.3/40"
    );
    let matcher = Matcher::builder()
      .all("http://proxy.example.com:3128")
      .no(no_proxy_list(&exclusions))
      .build();
    assert_eq!(
      route(&matcher, &Url::parse("https://169.254.10.1/").unwrap()),
      None
    );
  }

  #[test]
  fn socks_proxies_are_not_used() {
    let matcher = Matcher::builder().all("socks5://proxy.example.com:1080").build();
    assert_eq!(
      route(&matcher, &Url::parse("wss://chat.example.com/").unwrap()),
      None
    );
  }

  #[test]
  fn the_debug_output_names_no_exclusions() {
    let proxy = SystemProxy {
      host: "proxy.example.com".into(),
      port: 3128,
      exclusions: vec!["secret-host.example.com".into()],
    };
    assert!(!format!("{proxy:?}").contains("secret-host"));
  }
}
