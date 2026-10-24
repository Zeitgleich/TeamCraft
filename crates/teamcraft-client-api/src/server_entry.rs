//! The server entry of the native apps (docs/operations.md "Server address in the apps"): what the
//! user typed becomes a server origin, and the server's `GET /api/v1/server-info` answer becomes
//! the sign-in settings of an account, after strict checks.
//!
//! The apps never switch servers on a server's word: the origin comes from the user (or the
//! device's managed configuration), and everything stored for an account is keyed by it. A server
//! only tells the app how to sign in to *itself*.
//!
//! Pure functions without I/O, shared by the desktop shell, the web client and, through
//! `teamcraft-ffi`, the mobile apps. The callers fetch; transport failures (unreachable, TLS) are
//! theirs to report, everything about the answer is decided here.

use std::cmp::Ordering;

use semver::Version;
use serde::{Deserialize, Serialize};
use teamcraft_proto::{BrandResponse, SERVER_INFO_PRODUCT, ServerInfoResponse, SignInInfo};
use url::{Host, Url};

/// Where a client asks a server about itself.
pub const SERVER_INFO_PATH: &str = "/api/v1/server-info";
/// The oldest server the apps of this release work with.
pub const MINIMUM_SERVER_VERSION: &str = "1.0.0";
/// The scope every sign-in asks for.
const OPENID_SCOPE: &str = "openid";
const MAX_INPUT_CHARS: usize = 2048;
const MAX_CLIENT_ID_CHARS: usize = 200;
const MAX_SCOPES: usize = 20;
const MAX_SCOPE_CHARS: usize = 100;
const MAX_DISPLAY_NAME_CHARS: usize = 64;
/// Host names that are this device; plain `http` to them stays on the device.
const LOOPBACK_NAMES: [&str; 2] = ["localhost", "localhost."];

/// The origin of a TeamCraft server: `https://host[:port]`, lower case, nothing after it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ServerOrigin(String);

/// Whether plain `http` is accepted for servers and identity providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpPolicy {
  /// Release builds: `https` only, plain `http` on loopback addresses (a server on this device).
  LoopbackOnly,
  /// Development builds: plain `http` anywhere (an emulator reaches its host at `10.0.2.2`).
  Allowed,
}

/// An account's server and how to sign in to it, taken from the server's answer and stored with
/// the account. One active account is enough for now; several later (multi-account) are several
/// of these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerProfile {
  pub origin: ServerOrigin,
  pub issuer: String,
  pub client_id: String,
  /// Always contains `openid`.
  pub scopes: Vec<String>,
  /// The brand name the user confirmed, shown in account lists.
  pub display_name: String,
  pub server_version: String,
  #[serde(default)]
  pub has_local_accounts: bool,
}

/// A checked answer: the profile to store, the brand to show for confirmation and the sign-in
/// page settings (`sign_in_page::SignInPage::from_info`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedServer {
  pub profile: ServerProfile,
  pub brand: BrandResponse,
  /// Where the user will enter their password; shown on the confirmation.
  pub identity_provider_host: String,
  /// The identity provider lies outside the server's domain: the confirmation warns.
  pub is_identity_provider_elsewhere: bool,
  pub sign_in: SignInInfo,
}

/// The server's answer to `GET /api/v1/server-info`, as the caller received it. Callers must not
/// follow redirects: a redirect is reported, not obeyed.
#[derive(Debug, Clone, Copy)]
pub struct ServerAnswer<'a> {
  pub status: u16,
  pub maybe_content_type: Option<&'a str>,
  pub body: &'a [u8],
}

/// What the user typed is not a usable server address.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServerAddressError {
  #[error("no server address")]
  Empty,
  #[error("not a web address")]
  Invalid,
  #[error("only https addresses are supported")]
  UnsupportedScheme,
  #[error("plain http is only allowed for this device")]
  InsecureHttp,
  #[error("the address contains a user name or password")]
  HasCredentials,
}

/// The identity provider's discovery document can't be trusted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiscoveryError {
  #[error("the discovery document is for another issuer")]
  IssuerMismatch,
  #[error("the discovery document names an endpoint that is not https")]
  UnsafeEndpoint,
}

/// The address answered, but not as a usable TeamCraft server.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServerCheckError {
  /// Some other web server, or a TeamCraft server that does not serve this host name.
  #[error("not a TeamCraft server")]
  NotTeamCraft,
  /// The network answered instead of the server: an HTML page or a redirect, typically a hotel
  /// or train Wi-Fi login.
  #[error("the network asks for a sign-in first")]
  CaptivePortal,
  /// The server is there but failing (5xx).
  #[error("the server is not available (HTTP {0})")]
  Unavailable(u16),
  #[error("this app is too old for the server (needs {minimum})")]
  ClientTooOld { minimum: String },
  #[error("the server is too old for this app (version {version})")]
  ServerTooOld { version: String },
  /// The server's sign-in settings are unusable (an `http` issuer, an invalid client ID, ...).
  #[error("the server's sign-in settings are invalid: {0}")]
  InvalidSignIn(String),
  /// The server lists the redirect URIs it allows, and this app's is not among them.
  #[error("the server is not set up for this app")]
  RedirectNotAllowed,
}

impl ServerOrigin {
  /// Normalises what a user typed or pasted: `team.example.com`, a full URL, or any link into
  /// the server (an invite, a permalink) all give the server's origin.
  pub fn parse(input: &str, policy: HttpPolicy) -> Result<Self, ServerAddressError> {
    let trimmed = input
      .trim()
      .trim_matches(|ch: char| matches!(ch, '<' | '>' | '"' | '\''));
    if trimmed.is_empty() {
      return Err(ServerAddressError::Empty);
    }
    if trimmed.chars().count() > MAX_INPUT_CHARS || trimmed.chars().any(char::is_whitespace) {
      return Err(ServerAddressError::Invalid);
    }
    let with_scheme = if trimmed.contains("://") {
      trimmed.to_owned()
    } else {
      format!("https://{trimmed}")
    };
    let mut url = Url::parse(&with_scheme).map_err(|_| ServerAddressError::Invalid)?;
    // `team.example.com.` (fully qualified) is the same server; one origin per server.
    if let Some(Host::Domain(name)) = url.host()
      && let Some(stripped) = name.strip_suffix('.')
      && !stripped.is_empty()
    {
      let stripped = stripped.to_owned();
      url
        .set_host(Some(&stripped))
        .map_err(|_| ServerAddressError::Invalid)?;
    }
    Self::from_url(&url, policy)
  }

  /// The origin as text, e.g. `https://team.example.com`.
  pub fn as_str(&self) -> &str {
    &self.0
  }

  /// Host and port without the scheme, for showing to the user.
  pub fn host(&self) -> &str {
    self.0.split_once("://").map_or(self.0.as_str(), |(_, host)| host)
  }

  /// A file and key name for everything stored for this server: letters, digits, `.`, `-` and
  /// `_` only, and different for different origins. Any other character (`:` of a port, `[` `]`
  /// of an IPv6 address, `_`) becomes `_` and its two hex digits; `http` origins get a prefix so
  /// they never share storage with `https` ones.
  pub fn storage_key(&self) -> String {
    let mut host = String::new();
    for byte in self.host().bytes() {
      if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-') {
        host.push(char::from(byte));
      } else {
        host.push_str(&format!("_{byte:02x}"));
      }
    }
    if self.0.starts_with("http://") {
      format!("http_{host}")
    } else {
      host
    }
  }

  /// `{origin}/api/v1/server-info`.
  pub fn server_info_url(&self) -> String {
    format!("{}{SERVER_INFO_PATH}", self.0)
  }

  fn from_url(url: &Url, policy: HttpPolicy) -> Result<Self, ServerAddressError> {
    let is_http = match url.scheme() {
      "https" => false,
      "http" => true,
      _ => return Err(ServerAddressError::UnsupportedScheme),
    };
    if !url.username().is_empty() || url.password().is_some() {
      return Err(ServerAddressError::HasCredentials);
    }
    let Some(host) = url.host() else {
      return Err(ServerAddressError::Invalid);
    };
    if matches!(host, Host::Domain(name) if name.is_empty() || name.starts_with('.')) {
      return Err(ServerAddressError::Invalid);
    }
    if is_http && policy == HttpPolicy::LoopbackOnly && !is_loopback(&host) {
      return Err(ServerAddressError::InsecureHttp);
    }
    // `Url` lower-cases and IDNA-encodes the host and drops default ports.
    let origin = url.origin().ascii_serialization();
    if origin == "null" {
      return Err(ServerAddressError::Invalid);
    }
    Ok(Self(origin))
  }
}

impl TryFrom<String> for ServerOrigin {
  type Error = ServerAddressError;

  /// Stored origins are checked again on load; they were written by this code, so only an
  /// exact origin is accepted.
  fn try_from(text: String) -> Result<Self, Self::Error> {
    let origin = Self::parse(&text, HttpPolicy::Allowed)?;
    if origin.0 == text {
      Ok(origin)
    } else {
      Err(ServerAddressError::Invalid)
    }
  }
}

impl From<ServerOrigin> for String {
  fn from(origin: ServerOrigin) -> Self {
    origin.0
  }
}

impl std::fmt::Display for ServerOrigin {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    formatter.write_str(&self.0)
  }
}

impl ServerAddressError {
  /// The i18n message for the user (`server-entry-*` in the Fluent files, the same names with `_`
  /// in the mobile apps).
  pub fn i18n_key(&self) -> &'static str {
    match self {
      Self::Empty => "server-entry-empty",
      Self::Invalid => "server-entry-invalid-address",
      Self::UnsupportedScheme | Self::InsecureHttp => "server-entry-https-only",
      Self::HasCredentials => "server-entry-has-credentials",
    }
  }
}

impl ServerCheckError {
  /// The i18n message for the user, as [`ServerAddressError::i18n_key`].
  pub fn i18n_key(&self) -> &'static str {
    match self {
      Self::NotTeamCraft => "server-entry-not-teamcraft",
      Self::CaptivePortal => "server-entry-captive-portal",
      Self::Unavailable(_) => "server-entry-unavailable",
      Self::ClientTooOld { .. } => "server-entry-app-too-old",
      Self::ServerTooOld { .. } => "server-entry-server-too-old",
      Self::InvalidSignIn(_) => "server-entry-invalid-sign-in",
      Self::RedirectNotAllowed => "server-entry-redirect-not-allowed",
    }
  }
}

/// Checks the answer of `origin` to `GET /api/v1/server-info` and turns it into the profile of an
/// account. `client_version` is this app's version; `maybe_redirect_uri` the redirect URI it signs
/// in with (`None`: the caller signs in on the same origin, like the web client).
pub fn check_server_info(
  origin: &ServerOrigin,
  answer: ServerAnswer<'_>,
  client_version: &str,
  maybe_redirect_uri: Option<&str>,
  policy: HttpPolicy,
) -> Result<CheckedServer, ServerCheckError> {
  let info = parse_answer(answer)?;
  check_versions(&info, client_version)?;
  let issuer = check_issuer(&info.sign_in.issuer, origin, policy)?;
  let client_id = check_client_id(&info.sign_in.client_id)?;
  let scopes = check_scopes(&info.sign_in.scopes)?;
  if let (Some(own), Some(listed)) = (maybe_redirect_uri, &info.sign_in.maybe_redirect_uris)
    && !listed.iter().any(|allowed| redirect_matches(allowed, own))
  {
    return Err(ServerCheckError::RedirectNotAllowed);
  }
  let display_name = clean_display_name(&info.brand.display_name);
  let (identity_provider_host, is_identity_provider_elsewhere) = identity_provider(origin, &issuer);
  Ok(CheckedServer {
    identity_provider_host,
    is_identity_provider_elsewhere,
    profile: ServerProfile {
      origin: origin.clone(),
      issuer,
      client_id,
      scopes,
      display_name: if display_name.is_empty() {
        origin.host().to_owned()
      } else {
        display_name
      },
      server_version: info.server_version.clone(),
      has_local_accounts: info.sign_in.has_local_accounts,
    },
    brand: info.brand,
    sign_in: info.sign_in,
  })
}

fn parse_answer(answer: ServerAnswer<'_>) -> Result<ServerInfoResponse, ServerCheckError> {
  let looks_like_html = answer
    .maybe_content_type
    .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/html"))
    || answer
      .body
      .iter()
      .find(|byte| !byte.is_ascii_whitespace())
      .is_some_and(|byte| *byte == b'<');
  match answer.status {
    200 if looks_like_html => return Err(ServerCheckError::CaptivePortal),
    200 => {},
    // RFC 6585 "Network Authentication Required", and redirects to a login page.
    300..=399 | 511 => return Err(ServerCheckError::CaptivePortal),
    500..=599 => return Err(ServerCheckError::Unavailable(answer.status)),
    _ => return Err(ServerCheckError::NotTeamCraft),
  }
  let info: ServerInfoResponse =
    serde_json::from_slice(answer.body).map_err(|_| ServerCheckError::NotTeamCraft)?;
  if info.product != SERVER_INFO_PRODUCT {
    return Err(ServerCheckError::NotTeamCraft);
  }
  Ok(info)
}

fn check_versions(info: &ServerInfoResponse, client_version: &str) -> Result<(), ServerCheckError> {
  let parse = |text: &str| Version::parse(text.trim()).ok();
  let (Some(server), Some(minimum_client)) =
    (parse(&info.server_version), parse(&info.minimum_client_version))
  else {
    return Err(ServerCheckError::NotTeamCraft);
  };
  // Invariant: a constant of this file that the tests parse.
  let minimum_server = parse(MINIMUM_SERVER_VERSION).unwrap_or(Version::new(1, 0, 0));
  if compare_release(&server, &minimum_server) == Ordering::Less {
    return Err(ServerCheckError::ServerTooOld {
      version: info.server_version.clone(),
    });
  }
  if let Some(client) = parse(client_version)
    && compare_release(&client, &minimum_client) == Ordering::Less
  {
    return Err(ServerCheckError::ClientTooOld {
      minimum: info.minimum_client_version.clone(),
    });
  }
  Ok(())
}

/// Compares major, minor and patch only: release candidates of a version count as that version,
/// so a `1.0.0-rc.9` server works with `1.0.0` apps and the other way round.
fn compare_release(left: &Version, right: &Version) -> Ordering {
  (left.major, left.minor, left.patch).cmp(&(right.major, right.minor, right.patch))
}

fn check_issuer(issuer: &str, origin: &ServerOrigin, policy: HttpPolicy) -> Result<String, ServerCheckError> {
  let invalid = |reason: &str| ServerCheckError::InvalidSignIn(format!("issuer: {reason}"));
  let url = Url::parse(issuer.trim()).map_err(|_| invalid("not a URL"))?;
  let Some(host) = url.host() else {
    return Err(invalid("no host"));
  };
  // A plain-http issuer on this device only goes with a server on this device (development).
  let is_server_local = Url::parse(origin.as_str())
    .ok()
    .is_some_and(|server| server.host().is_some_and(|server_host| is_loopback(&server_host)));
  match url.scheme() {
    "https" => {},
    "http" if policy == HttpPolicy::Allowed || (is_loopback(&host) && is_server_local) => {},
    _ => return Err(invalid("not https")),
  }
  if !url.username().is_empty() || url.password().is_some() {
    return Err(invalid("contains credentials"));
  }
  if url.query().is_some() || url.fragment().is_some() {
    return Err(invalid("has a query or fragment"));
  }
  // The issuer must equal the tokens' `iss` exactly; keep it as the server wrote it.
  Ok(issuer.trim().to_owned())
}

/// The brand name as shown for confirmation: no control or bidirectional formatting characters
/// (which could make a name look like another), at most 64 characters.
fn clean_display_name(name: &str) -> String {
  name
    .chars()
    .filter(|ch| !ch.is_control() && !is_bidi_control(*ch))
    .take(MAX_DISPLAY_NAME_CHARS)
    .collect::<String>()
    .trim()
    .to_owned()
}

/// Bidirectional formatting and zero-width characters: invisible, but they change how a name reads.
fn is_bidi_control(ch: char) -> bool {
  matches!(
    ch,
    '\u{061C}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}'
  )
}

/// The identity provider's host, and whether it lies outside the server's own domain. Signing in
/// hands that provider's access token to this server; a provider elsewhere is shown with a
/// warning on the confirmation (a server copying another instance's settings would show the real
/// provider of that instance). "The same domain" means the same registrable domain (one label
/// below the public suffix, from the Public Suffix List: `sso.example.com` is inside for
/// `team.example.com`, `auth.victim.co.uk` is not for `evil.co.uk`). IP addresses only match
/// themselves.
fn identity_provider(origin: &ServerOrigin, issuer: &str) -> (String, bool) {
  let host_of = |url: &str| {
    Url::parse(url)
      .ok()
      .and_then(|url| url.host().map(|host| host.to_owned()))
  };
  let maybe_provider = host_of(issuer);
  let maybe_server = host_of(origin.as_str());
  let shown = maybe_provider
    .as_ref()
    .map(|host| normalized_host(&host.to_string()))
    .unwrap_or_default();
  let is_inside = match (maybe_provider, maybe_server) {
    (Some(Host::Domain(provider)), Some(Host::Domain(server))) => {
      registrable_domain(&provider) == registrable_domain(&server)
    },
    (Some(provider), Some(server)) => provider == server,
    _ => false,
  };
  (shown, !is_inside)
}

/// Lower case, without the trailing dot of a fully qualified name.
fn normalized_host(host: &str) -> String {
  host.trim_end_matches('.').to_ascii_lowercase()
}

/// The registrable domain (eTLD+1) of a host name; a name that is itself a public suffix, or has
/// no known suffix structure (`localhost`), stands for itself.
fn registrable_domain(host: &str) -> String {
  let host = normalized_host(host);
  psl::domain_str(&host).map_or_else(|| host.clone(), str::to_owned)
}

/// Whether an endpoint of the identity provider's discovery document may be used: `https`
/// (plain `http` only on loopback, or anywhere in development builds), no credentials. Anything
/// else (`file:`, `javascript:`, `search-ms:`, ...) must never reach a browser or the OS opener.
pub fn is_provider_endpoint(endpoint: &str, policy: HttpPolicy) -> bool {
  let Ok(url) = Url::parse(endpoint) else {
    return false;
  };
  let Some(host) = url.host() else {
    return false;
  };
  let is_scheme_ok = match url.scheme() {
    "https" => true,
    "http" => policy == HttpPolicy::Allowed || is_loopback(&host),
    _ => false,
  };
  is_scheme_ok && url.username().is_empty() && url.password().is_none()
}

/// Checks a discovery document's `issuer` and endpoints (RFC 8414 §3.3: the `issuer` must equal
/// the configured one exactly).
pub fn check_discovery(
  expected_issuer: &str,
  discovered_issuer: &str,
  endpoints: &[&str],
  policy: HttpPolicy,
) -> Result<(), DiscoveryError> {
  if discovered_issuer != expected_issuer {
    return Err(DiscoveryError::IssuerMismatch);
  }
  // A plain-http loopback endpoint only goes with a plain-http loopback issuer (development).
  let is_issuer_local_http = Url::parse(expected_issuer)
    .ok()
    .is_some_and(|url| url.scheme() == "http" && url.host().is_some_and(|host| is_loopback(&host)));
  let is_allowed = |endpoint: &str| {
    is_provider_endpoint(endpoint, policy)
      && (policy == HttpPolicy::Allowed || is_issuer_local_http || endpoint.starts_with("https://"))
  };
  if !endpoints.iter().all(|endpoint| is_allowed(endpoint)) {
    return Err(DiscoveryError::UnsafeEndpoint);
  }
  Ok(())
}

fn check_client_id(client_id: &str) -> Result<String, ServerCheckError> {
  let is_valid = !client_id.is_empty()
    && client_id.chars().count() <= MAX_CLIENT_ID_CHARS
    && client_id.chars().all(|ch| ch.is_ascii_graphic());
  if is_valid {
    Ok(client_id.to_owned())
  } else {
    Err(ServerCheckError::InvalidSignIn("client_id".to_owned()))
  }
}

/// Scope tokens of RFC 6749 §3.3; `openid` is added when missing.
fn check_scopes(scopes: &[String]) -> Result<Vec<String>, ServerCheckError> {
  if scopes.len() > MAX_SCOPES {
    return Err(ServerCheckError::InvalidSignIn("too many scopes".to_owned()));
  }
  let mut checked = vec![OPENID_SCOPE.to_owned()];
  for scope in scopes {
    let is_valid = !scope.is_empty()
      && scope.len() <= MAX_SCOPE_CHARS
      && scope
        .bytes()
        .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\');
    if !is_valid {
      return Err(ServerCheckError::InvalidSignIn(format!("scope {scope:?}")));
    }
    if !checked.contains(scope) {
      checked.push(scope.clone());
    }
  }
  Ok(checked)
}

/// Whether `own` is allowed by the listed redirect URI `allowed`. A loopback redirect matches
/// whatever its port (RFC 8252 §7.3: the desktop app picks a free port per sign-in).
fn redirect_matches(allowed: &str, own: &str) -> bool {
  if allowed == own {
    return true;
  }
  let (Ok(allowed), Ok(own)) = (Url::parse(allowed), Url::parse(own)) else {
    return false;
  };
  let is_loopback_http =
    |url: &Url| url.scheme() == "http" && url.host().is_some_and(|host| is_loopback(&host));
  is_loopback_http(&allowed)
    && is_loopback_http(&own)
    && allowed.host() == own.host()
    && allowed.path() == own.path()
}

fn is_loopback(host: &Host<&str>) -> bool {
  match host {
    Host::Domain(name) => LOOPBACK_NAMES.contains(name),
    Host::Ipv4(address) => address.is_loopback(),
    Host::Ipv6(address) => address.is_loopback(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const ORIGIN: &str = "https://team.example.com";

  mod addresses {
    use super::*;

    #[test]
    fn host_names_urls_and_links_become_the_origin() {
      for input in [
        "team.example.com",
        "team.example.com/",
        "  team.example.com  ",
        " team.example.com/ ",
        "Team.Example.COM",
        "TEAM.EXAMPLE.COM/",
        "team.example.com.",
        "https://Team.Example.com/some/page",
        "https://team.example.com",
        "https://team.example.com/",
        "https://team.example.com:443/",
        "team.example.com/invite/abc?x=1#y",
        "https://team.example.com/#/c/123/m/456",
        "<https://team.example.com/s/abc>",
      ] {
        assert_eq!(origin(input).as_str(), ORIGIN, "{input}");
      }
    }

    #[test]
    fn ports_ip_literals_and_idn_are_kept_in_canonical_form() {
      assert_eq!(
        origin("team.example.com:8443").as_str(),
        "https://team.example.com:8443"
      );
      assert_eq!(origin("[2001:db8::1]").as_str(), "https://[2001:db8::1]");
      assert_eq!(origin("192.0.2.10:8443").as_str(), "https://192.0.2.10:8443");
      assert_eq!(origin("bücher.example").as_str(), "https://xn--bcher-kva.example");
    }

    #[test]
    fn plain_http_only_reaches_this_device_in_release_builds() {
      for loopback in [
        "http://127.0.0.1:8081",
        "http://localhost:8081",
        "http://[::1]:8081",
      ] {
        assert!(
          ServerOrigin::parse(loopback, HttpPolicy::LoopbackOnly).is_ok(),
          "{loopback}"
        );
      }
      // Never a silent fallback: a bare domain is https, an explicit http one is refused.
      assert_eq!(
        ServerOrigin::parse("http://team.example.com", HttpPolicy::LoopbackOnly),
        Err(ServerAddressError::InsecureHttp)
      );
      assert_eq!(
        ServerOrigin::parse("localhost:8081", HttpPolicy::LoopbackOnly)
          .unwrap()
          .as_str(),
        "https://localhost:8081"
      );
      assert_eq!(
        ServerOrigin::parse("http://10.0.2.2:8081", HttpPolicy::Allowed)
          .unwrap()
          .as_str(),
        "http://10.0.2.2:8081"
      );
    }

    #[test]
    fn unusable_input_is_rejected_with_a_reason() {
      let cases = [
        ("", ServerAddressError::Empty),
        ("   ", ServerAddressError::Empty),
        ("team example.com", ServerAddressError::Invalid),
        ("https://", ServerAddressError::Invalid),
        ("ftp://team.example.com", ServerAddressError::UnsupportedScheme),
        ("javascript://alert(1)", ServerAddressError::UnsupportedScheme),
        ("file:///etc/passwd", ServerAddressError::UnsupportedScheme),
        (
          "https://user:pw@team.example.com",
          ServerAddressError::HasCredentials,
        ),
        (
          "https://user@team.example.com",
          ServerAddressError::HasCredentials,
        ),
      ];
      for (input, expected) in cases {
        assert_eq!(
          ServerOrigin::parse(input, HttpPolicy::LoopbackOnly),
          Err(expected),
          "{input:?}"
        );
      }
      let oversized = format!("{}.example.com", "a".repeat(MAX_INPUT_CHARS));
      assert_eq!(
        ServerOrigin::parse(&oversized, HttpPolicy::LoopbackOnly),
        Err(ServerAddressError::Invalid)
      );
    }

    #[test]
    fn storage_keys_are_file_names_and_separate_http_from_https() {
      assert_eq!(
        origin("team.example.com:8443").storage_key(),
        "team.example.com_3a8443"
      );
      assert_eq!(origin("[::1]").storage_key(), "_5b_3a_3a1_5d");
      let http = ServerOrigin::parse("http://localhost:8081", HttpPolicy::LoopbackOnly).unwrap();
      assert_eq!(http.storage_key(), "http_localhost_3a8081");
      // Different origins never share a key.
      assert_ne!(origin("[::1]:2").storage_key(), origin("[::1:2]").storage_key());
      assert_ne!(
        origin("a.example.com:8443").storage_key(),
        origin("a.example.com").storage_key()
      );
    }

    #[test]
    fn stored_origins_round_trip_and_tampered_ones_are_refused() {
      let json = serde_json::to_string(&origin("team.example.com")).unwrap();
      assert_eq!(json, "\"https://team.example.com\"");
      assert_eq!(
        serde_json::from_str::<ServerOrigin>(&json).unwrap(),
        origin("team.example.com")
      );
      for bad in [
        "\"team.example.com\"",
        "\"https://team.example.com/x\"",
        "\"ftp://x\"",
      ] {
        assert!(serde_json::from_str::<ServerOrigin>(bad).is_err(), "{bad}");
      }
    }
  }

  mod answers {
    use super::*;

    #[test]
    fn a_valid_answer_becomes_a_profile_and_a_brand() {
      let checked = check(200, &info_json(|_| {})).unwrap();
      assert_eq!(checked.profile.origin.as_str(), ORIGIN);
      assert_eq!(checked.profile.issuer, "https://auth.example.com/realms/company");
      assert_eq!(checked.profile.client_id, "teamcraft");
      assert_eq!(checked.profile.scopes, ["openid", "profile"]);
      assert_eq!(checked.profile.display_name, "Example Chat");
      assert_eq!(checked.brand.display_name, "Example Chat");
    }

    #[test]
    fn other_servers_and_networks_are_told_apart() {
      let html = "<!doctype html><title>Wi-Fi login</title>";
      assert_eq!(check(200, html), Err(ServerCheckError::CaptivePortal));
      assert_eq!(check(302, ""), Err(ServerCheckError::CaptivePortal));
      assert_eq!(check(511, ""), Err(ServerCheckError::CaptivePortal));
      assert_eq!(
        check(404, "<html>not found</html>"),
        Err(ServerCheckError::NotTeamCraft)
      );
      assert_eq!(check(421, "{}"), Err(ServerCheckError::NotTeamCraft));
      assert_eq!(
        check(200, "{\"status\":\"ok\"}"),
        Err(ServerCheckError::NotTeamCraft)
      );
      assert_eq!(check(200, ""), Err(ServerCheckError::NotTeamCraft));
      assert_eq!(check(502, ""), Err(ServerCheckError::Unavailable(502)));
      let other_product = info_json(|info| info["product"] = "other".into());
      assert_eq!(check(200, &other_product), Err(ServerCheckError::NotTeamCraft));
    }

    #[test]
    fn versions_are_checked_both_ways() {
      let newer_minimum = info_json(|info| info["minimum_client_version"] = "2.0.0".into());
      assert_eq!(
        check(200, &newer_minimum),
        Err(ServerCheckError::ClientTooOld {
          minimum: "2.0.0".into()
        })
      );
      let old_server = info_json(|info| info["server_version"] = "0.9.0".into());
      assert_eq!(
        check(200, &old_server),
        Err(ServerCheckError::ServerTooOld {
          version: "0.9.0".into()
        })
      );
      // A release candidate is its release.
      let candidate = info_json(|info| info["server_version"] = "1.0.0-rc.9".into());
      assert!(check(200, &candidate).is_ok());
      let candidate_app = check_server_info(
        &origin(ORIGIN),
        answer(200, &info_json(|_| {})),
        "1.0.0-rc.1",
        None,
        HttpPolicy::LoopbackOnly,
      );
      assert!(candidate_app.is_ok());
      let garbage = info_json(|info| info["server_version"] = "latest".into());
      assert_eq!(check(200, &garbage), Err(ServerCheckError::NotTeamCraft));
    }

    #[test]
    fn insecure_or_malformed_sign_in_settings_are_refused() {
      for issuer in [
        "http://auth.example.com/realms/company",
        "https://user:pw@auth.example.com/",
        "https://auth.example.com/?client_secret=x",
        "https://auth.example.com/#x",
        "not a url",
        "javascript:alert(1)",
      ] {
        let json = info_json(|info| info["sign_in"]["issuer"] = issuer.into());
        assert!(
          matches!(check(200, &json), Err(ServerCheckError::InvalidSignIn(_))),
          "{issuer}"
        );
      }
      for client_id in ["", "two words", "x\u{0}"] {
        let json = info_json(|info| info["sign_in"]["client_id"] = client_id.into());
        assert!(
          matches!(check(200, &json), Err(ServerCheckError::InvalidSignIn(_))),
          "{client_id:?}"
        );
      }
      let bad_scope = info_json(|info| info["sign_in"]["scopes"] = serde_json::json!(["openid profile"]));
      assert!(matches!(
        check(200, &bad_scope),
        Err(ServerCheckError::InvalidSignIn(_))
      ));
    }

    #[test]
    fn a_loopback_issuer_needs_a_local_server_and_development_builds_allow_http() {
      let json = info_json(|info| info["sign_in"]["issuer"] = "http://127.0.0.1:8080/realms/dev".into());
      // A remote server must not send this device's sign-in to a local plain-http provider.
      assert!(check(200, &json).is_err());
      let local = ServerOrigin::parse("http://127.0.0.1:8081", HttpPolicy::LoopbackOnly).unwrap();
      let checked = check_server_info(
        &local,
        answer(200, &json),
        "1.0.0",
        None,
        HttpPolicy::LoopbackOnly,
      );
      assert!(checked.is_ok());
      let json = info_json(|info| info["sign_in"]["issuer"] = "http://10.0.2.2:8080/realms/dev".into());
      assert!(check(200, &json).is_err());
      let development = check_server_info(
        &origin(ORIGIN),
        answer(200, &json),
        "1.0.0",
        None,
        HttpPolicy::Allowed,
      );
      assert!(development.is_ok());
    }

    #[test]
    fn listed_redirect_uris_must_contain_the_apps_one() {
      let json = info_json(|info| {
        info["sign_in"]["redirect_uris"] = serde_json::json!([
          "http://127.0.0.1/callback",
          "de.zeitgleich.teamcraft:/oauth/callback"
        ]);
      });
      let with_redirect = |redirect: &str| {
        check_server_info(
          &origin(ORIGIN),
          answer(200, &json),
          "1.0.0",
          Some(redirect),
          HttpPolicy::LoopbackOnly,
        )
      };
      assert!(with_redirect("http://127.0.0.1:53817/callback").is_ok());
      assert!(with_redirect("de.zeitgleich.teamcraft:/oauth/callback").is_ok());
      assert_eq!(
        with_redirect("com.example.selfbuild:/oauth/callback"),
        Err(ServerCheckError::RedirectNotAllowed)
      );
      assert_eq!(
        with_redirect("http://127.0.0.1:53817/other"),
        Err(ServerCheckError::RedirectNotAllowed)
      );
    }

    #[test]
    fn the_identity_provider_is_shown_and_flagged_when_elsewhere() {
      let checked = check(200, &info_json(|_| {})).unwrap();
      assert_eq!(checked.identity_provider_host, "auth.example.com");
      assert!(!checked.is_identity_provider_elsewhere);
      let other = info_json(|info| info["sign_in"]["issuer"] = "https://sso.other.example/realms/x".into());
      let checked = check(200, &other).unwrap();
      assert_eq!(checked.identity_provider_host, "sso.other.example");
      assert!(checked.is_identity_provider_elsewhere);
      let same_host = info_json(|info| info["sign_in"]["issuer"] = "https://team.example.com/auth".into());
      assert!(!check(200, &same_host).unwrap().is_identity_provider_elsewhere);
      let lookalike = info_json(|info| info["sign_in"]["issuer"] = "https://evilexample.com/".into());
      assert!(check(200, &lookalike).unwrap().is_identity_provider_elsewhere);
    }

    #[test]
    fn the_server_domain_follows_the_public_suffix_list() {
      let elsewhere = |server: &str, issuer: &str| {
        let origin = ServerOrigin::parse(server, HttpPolicy::LoopbackOnly).unwrap();
        identity_provider(&origin, issuer).1
      };
      // The usual setup: a provider on a sibling host of the same company domain.
      assert!(!elsewhere("team.example.com", "https://sso.example.com/realms/x"));
      assert!(!elsewhere(
        "team.example.com",
        "https://SSO.Example.COM./realms/x"
      ));
      assert!(!elsewhere("chat.example.co.uk", "https://auth.example.co.uk/"));
      // A public suffix is never the shared domain.
      assert!(elsewhere("evil.co.uk", "https://auth.victim.co.uk/"));
      assert!(elsewhere("evil.com.au", "https://auth.victim.com.au/"));
      assert!(elsewhere("evil.github.io", "https://victim.github.io/"));
      // IP addresses match only themselves.
      assert!(elsewhere("1.2.3.4", "https://2.3.4.5/"));
      assert!(!elsewhere("1.2.3.4", "https://1.2.3.4:8443/"));
      assert!(elsewhere("[2001:db8::1]", "https://[2001:db8::2]/"));
      assert!(!elsewhere("[2001:db8::1]", "https://[2001:db8::1]/realms/x"));
      assert!(elsewhere("1.2.3.4", "https://auth.example.com/"));
    }

    #[test]
    fn brand_names_lose_control_and_bidi_characters_and_are_capped() {
      let json = info_json(|info| info["brand"]["display_name"] = "Bank\u{202E}moc.elpmaxe\u{0007}".into());
      assert_eq!(check(200, &json).unwrap().profile.display_name, "Bankmoc.elpmaxe");
      let hidden = info_json(|info| info["brand"]["display_name"] = "Ex\u{200B}am\u{2060}ple\u{FEFF}".into());
      assert_eq!(check(200, &hidden).unwrap().profile.display_name, "Example");
      let long = info_json(|info| info["brand"]["display_name"] = "x".repeat(500).into());
      assert_eq!(
        check(200, &long).unwrap().profile.display_name.chars().count(),
        64
      );
    }

    #[test]
    fn discovery_documents_must_match_the_issuer_and_use_https() {
      let issuer = "https://auth.example.com/realms/company";
      let good = ["https://auth.example.com/auth", "https://auth.example.com/token"];
      assert!(check_discovery(issuer, issuer, &good, HttpPolicy::LoopbackOnly).is_ok());
      assert_eq!(
        check_discovery(issuer, "https://evil.example", &good, HttpPolicy::LoopbackOnly),
        Err(DiscoveryError::IssuerMismatch)
      );
      for bad in [
        "file://host/share/x.lnk",
        "search-ms:query=x",
        "ms-officecmd:x",
        "javascript:alert(1)",
        "http://auth.example.com/auth",
        "https://user:pw@auth.example.com/auth",
        "not a url",
      ] {
        assert_eq!(
          check_discovery(issuer, issuer, &[bad], HttpPolicy::LoopbackOnly),
          Err(DiscoveryError::UnsafeEndpoint),
          "{bad}"
        );
      }
      assert!(is_provider_endpoint(
        "http://127.0.0.1:8080/auth",
        HttpPolicy::LoopbackOnly
      ));
      // A plain-http loopback endpoint needs a plain-http loopback issuer.
      let local = ["http://127.0.0.1:8080/realms/dev/auth"];
      assert_eq!(
        check_discovery(issuer, issuer, &local, HttpPolicy::LoopbackOnly),
        Err(DiscoveryError::UnsafeEndpoint)
      );
      let dev_issuer = "http://127.0.0.1:8080/realms/dev";
      assert!(check_discovery(dev_issuer, dev_issuer, &local, HttpPolicy::LoopbackOnly).is_ok());
    }

    #[test]
    fn every_error_has_an_i18n_key() {
      let errors = [
        ServerCheckError::NotTeamCraft,
        ServerCheckError::CaptivePortal,
        ServerCheckError::Unavailable(503),
        ServerCheckError::ClientTooOld {
          minimum: "2.0.0".into(),
        },
        ServerCheckError::ServerTooOld {
          version: "0.9.0".into(),
        },
        ServerCheckError::InvalidSignIn("x".into()),
        ServerCheckError::RedirectNotAllowed,
      ];
      for error in errors {
        assert!(error.i18n_key().starts_with("server-entry-"), "{error:?}");
      }
      assert_eq!(
        ServerAddressError::InsecureHttp.i18n_key(),
        "server-entry-https-only"
      );
    }
  }

  fn origin(input: &str) -> ServerOrigin {
    ServerOrigin::parse(input, HttpPolicy::LoopbackOnly).unwrap()
  }

  fn answer(status: u16, body: &str) -> ServerAnswer<'_> {
    ServerAnswer {
      status,
      maybe_content_type: Some("application/json"),
      body: body.as_bytes(),
    }
  }

  fn check(status: u16, body: &str) -> Result<CheckedServer, ServerCheckError> {
    check_server_info(
      &origin(ORIGIN),
      answer(status, body),
      "1.0.0",
      None,
      HttpPolicy::LoopbackOnly,
    )
  }

  fn info_json(change: impl FnOnce(&mut serde_json::Value)) -> String {
    let mut info = serde_json::json!({
      "product": "teamcraft",
      "server_version": "1.0.0",
      "minimum_client_version": "1.0.0",
      "brand": {
        "id": "example",
        "display_name": "Example Chat",
        "origin": ORIGIN,
      },
      "sign_in": {
        "issuer": "https://auth.example.com/realms/company",
        "client_id": "teamcraft",
        "scopes": ["openid", "profile"],
      },
    });
    change(&mut info);
    info.to_string()
  }
}
