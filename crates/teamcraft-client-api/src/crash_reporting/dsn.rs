//! The DSN: where a backend takes reports, as the backend shows it
//! (`https://<public key>@errors.example.com/<project>`).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::{Host, Url};

/// Envelope protocol version sent with every request.
const PROTOCOL_VERSION: u32 = 7;
/// Longest DSN accepted, in bytes.
const MAX_DSN_BYTES: usize = 512;

/// A parsed DSN. Its public key only allows sending reports, so it is not a secret: the instance
/// serves it to every app. A legacy secret key (`key:secret@`) is dropped while parsing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Dsn {
  /// `scheme://host[:port][/path]`, without a trailing slash.
  base: String,
  public_key: String,
  project_id: String,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DsnError {
  #[error("not a URL")]
  NotUrl,
  #[error("the DSN is longer than 512 characters")]
  TooLong,
  #[error("the DSN must use https (http only for localhost)")]
  InsecureScheme,
  #[error("the DSN has no public key before the host")]
  MissingPublicKey,
  #[error("the DSN does not end with a numeric project ID")]
  MissingProjectId,
  #[error("the DSN must not have a query or fragment")]
  HasQuery,
}

impl Dsn {
  /// The envelope endpoint, with the public key as query parameters (as the browser SDKs send it,
  /// which avoids a CORS preflight for a custom header).
  pub fn envelope_url(&self, client_version: &str) -> String {
    format!(
      "{}/api/{}/envelope/?sentry_key={}&sentry_version={PROTOCOL_VERSION}&sentry_client=teamcraft%2F{}",
      self.base,
      self.project_id,
      self.public_key,
      url_safe(client_version)
    )
  }

  pub fn project_id(&self) -> &str {
    &self.project_id
  }
}

impl FromStr for Dsn {
  type Err = DsnError;

  fn from_str(text: &str) -> Result<Self, Self::Err> {
    let text = text.trim();
    if text.len() > MAX_DSN_BYTES {
      return Err(DsnError::TooLong);
    }
    let url = Url::parse(text).map_err(|_| DsnError::NotUrl)?;
    let host = url.host().ok_or(DsnError::NotUrl)?;
    match url.scheme() {
      "https" => {},
      "http" if is_loopback(&host) => {},
      _ => return Err(DsnError::InsecureScheme),
    }
    if url.query().is_some() || url.fragment().is_some() {
      return Err(DsnError::HasQuery);
    }
    let public_key = url.username();
    if public_key.is_empty() || !public_key.chars().all(|c| c.is_ascii_alphanumeric()) {
      return Err(DsnError::MissingPublicKey);
    }
    let path = url.path().trim_end_matches('/');
    let (prefix, project_id) = path.rsplit_once('/').ok_or(DsnError::MissingProjectId)?;
    if project_id.is_empty() || !project_id.chars().all(|c| c.is_ascii_digit()) {
      return Err(DsnError::MissingProjectId);
    }
    let port = url.port().map(|port| format!(":{port}")).unwrap_or_default();
    Ok(Self {
      base: format!("{}://{host}{port}{prefix}", url.scheme()),
      public_key: public_key.to_owned(),
      project_id: project_id.to_owned(),
    })
  }
}

impl fmt::Display for Dsn {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let (scheme, rest) = self.base.split_once("://").unwrap_or(("https", &self.base));
    write!(f, "{scheme}://{}@{rest}/{}", self.public_key, self.project_id)
  }
}

impl TryFrom<String> for Dsn {
  type Error = DsnError;

  fn try_from(text: String) -> Result<Self, Self::Error> {
    text.parse()
  }
}

impl From<Dsn> for String {
  fn from(dsn: Dsn) -> Self {
    dsn.to_string()
  }
}

fn is_loopback(host: &Host<&str>) -> bool {
  match host {
    Host::Domain(name) => *name == "localhost" || name.ends_with(".localhost"),
    Host::Ipv4(address) => address.is_loopback(),
    Host::Ipv6(address) => address.is_loopback(),
  }
}

/// Keeps characters that need no escaping in a query value.
fn url_safe(text: &str) -> String {
  text
    .chars()
    .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_a_dsn_and_builds_the_envelope_url() {
    let dsn: Dsn = "https://abc123@errors.example.com/42".parse().unwrap();
    assert_eq!(dsn.project_id(), "42");
    assert_eq!(
      dsn.envelope_url("1.0.9"),
      "https://errors.example.com/api/42/envelope/?sentry_key=abc123&sentry_version=7&sentry_client=teamcraft%2F1.0.9"
    );
    assert_eq!(dsn.to_string(), "https://abc123@errors.example.com/42");
  }

  #[test]
  fn keeps_a_port_and_a_path_prefix() {
    let dsn: Dsn = "https://abc@example.com:8443/glitchtip/3/".parse().unwrap();
    assert!(
      dsn
        .envelope_url("1")
        .starts_with("https://example.com:8443/glitchtip/api/3/envelope/?")
    );
    assert_eq!(dsn.to_string(), "https://abc@example.com:8443/glitchtip/3");
  }

  #[test]
  fn drops_a_legacy_secret_key() {
    let dsn: Dsn = "https://public:secret@errors.example.com/1".parse().unwrap();
    assert!(!dsn.to_string().contains("secret"));
    assert!(!dsn.envelope_url("1").contains("secret"));
  }

  #[test]
  fn allows_http_only_for_loopback() {
    assert!("http://key@localhost:8000/1".parse::<Dsn>().is_ok());
    assert!("http://key@[::1]:8000/1".parse::<Dsn>().is_ok());
    assert_eq!(
      "http://key@errors.example.com/1".parse::<Dsn>(),
      Err(DsnError::InsecureScheme)
    );
  }

  #[test]
  fn rejects_malformed_dsns() {
    assert_eq!("".parse::<Dsn>(), Err(DsnError::NotUrl));
    assert_eq!("errors.example.com/1".parse::<Dsn>(), Err(DsnError::NotUrl));
    assert_eq!(
      "https://errors.example.com/1".parse::<Dsn>(),
      Err(DsnError::MissingPublicKey)
    );
    assert_eq!(
      "https://key@errors.example.com/".parse::<Dsn>(),
      Err(DsnError::MissingProjectId)
    );
    assert_eq!(
      "https://key@errors.example.com/project".parse::<Dsn>(),
      Err(DsnError::MissingProjectId)
    );
    assert_eq!(
      "https://key@errors.example.com/1?x=y".parse::<Dsn>(),
      Err(DsnError::HasQuery)
    );
    assert_eq!(
      format!("https://key@errors.example.com/{}", "1".repeat(600)).parse::<Dsn>(),
      Err(DsnError::TooLong)
    );
  }
}
