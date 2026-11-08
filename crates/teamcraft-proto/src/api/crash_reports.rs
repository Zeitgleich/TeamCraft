//! Crash reports (docs/crash-reports.md, issue #1508): the instance setting the admin console
//! edits, what `GET /api/v1/server-info` tells the apps, and the DSN both sides validate the same
//! way.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::{Host, Url};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;

/// `POST`: the server relays one envelope to the instance's backend (docs/crash-reports.md).
pub const CRASH_REPORTS_PATH: &str = "/api/v1/crash-reports";
/// Largest envelope the relay takes, in bytes; the apps' reports are far smaller.
pub const MAX_CRASH_ENVELOPE_BYTES: usize = 256 * 1024;
/// Longest environment name, in characters.
pub const MAX_CRASH_ENVIRONMENT_CHARS: usize = 64;

/// Administration → Crash reports: where the apps send crash and error reports. By default to the
/// publisher's service, the backend of whoever built the apps and the server (like the push
/// gateway); an instance can send to a backend of its own instead, or turn reports off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CrashReportSettings {
  pub mode: CrashReportsMode,
  /// The own backend's DSN (`https://<public key>@errors.example.com/<project>`); only used, and
  /// only required, with [`CrashReportsMode::Own`].
  pub dsn: String,
  /// Percentage of handled errors that are sent; crashes are always sent.
  pub sample_percent: u8,
  /// Percentage of measured operations (start, channel switch, send, sync) whose performance data
  /// (`transaction` items) is sent, issue #1521. 100 while the team gathers data; meant to be
  /// lowered as instances grow. Absent from older rows: 100.
  #[serde(default = "all_percent")]
  pub trace_percent: u8,
  /// The environment the backend groups reports by (`production`, `staging`).
  pub environment: String,
  /// People may turn reports off in their privacy settings. Off: reports are mandatory on this
  /// instance and the switch shows that the organization requires them. Absent from older rows: on.
  #[serde(default = "is_on")]
  pub allows_opt_out: bool,
  /// The apps say once, on the first start with reports on, that they send them. Off for an
  /// operator who informs people another way (privacy policy, onboarding). Absent: on.
  #[serde(default = "is_on")]
  pub shows_notice: bool,
}

wire_enum! {
  /// Where an instance's crash reports go.
  pub enum CrashReportsMode {
    /// The publisher's service, built into the server (`TEAMCRAFT_CRASH_REPORTS_DSN` at build
    /// time); a build without one sends nothing. The default.
    Publisher => "publisher",
    /// The instance's own backend, [`CrashReportSettings::dsn`].
    Own => "own",
    Off => "off",
  }
}

/// The part of [`CrashReportSettings`] the apps get in `GET /api/v1/server-info`, only while
/// reports are on. Public by design: the DSN's key only allows sending reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CrashReportsInfo {
  pub dsn: String,
  pub sample_percent: u8,
  /// Share of measured operations sent as performance data. Absent (an older server): none.
  #[serde(default)]
  pub trace_percent: u8,
  pub environment: String,
  #[serde(default = "is_on")]
  pub allows_opt_out: bool,
  #[serde(default = "is_on")]
  pub shows_notice: bool,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum CrashReportSettingsError {
  #[error("dsn: {0}")]
  Dsn(#[from] DsnError),
  #[error("sample_percent must be between 0 and 100")]
  SamplePercent,
  #[error("trace_percent must be between 0 and 100")]
  TracePercent,
  #[error("environment must be 1 to 64 lowercase letters, digits, '-', '_' or '.'")]
  Environment,
  #[error("mode must be publisher, own or off")]
  Mode,
}

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

impl Default for CrashReportSettings {
  fn default() -> Self {
    Self {
      mode: CrashReportsMode::Publisher,
      dsn: String::new(),
      sample_percent: 100,
      trace_percent: 100,
      environment: "production".to_owned(),
      allows_opt_out: true,
      shows_notice: true,
    }
  }
}

impl CrashReportSettings {
  /// Checks the values; a DSN is only required for an own backend.
  pub fn check(&self) -> Result<(), CrashReportSettingsError> {
    if matches!(self.mode, CrashReportsMode::Unknown(_)) {
      return Err(CrashReportSettingsError::Mode);
    }
    if self.mode == CrashReportsMode::Own || !self.dsn.trim().is_empty() {
      self.dsn.parse::<Dsn>()?;
    }
    if self.sample_percent > 100 {
      return Err(CrashReportSettingsError::SamplePercent);
    }
    if self.trace_percent > 100 {
      return Err(CrashReportSettingsError::TracePercent);
    }
    if !is_crash_environment(&self.environment) {
      return Err(CrashReportSettingsError::Environment);
    }
    Ok(())
  }

  /// The backend in force: the publisher's (`maybe_publisher`, built into the server) or the own
  /// one; `None` while reports are off, the build has no publisher service, or the values don't
  /// check.
  pub fn backend(&self, maybe_publisher: Option<&Dsn>) -> Option<Dsn> {
    if self.check().is_err() {
      return None;
    }
    match self.mode {
      CrashReportsMode::Publisher => maybe_publisher.cloned(),
      CrashReportsMode::Own => self.dsn.parse().ok(),
      CrashReportsMode::Off | CrashReportsMode::Unknown(_) => None,
    }
  }

  /// What the apps get in `GET /api/v1/server-info`; `None` while no backend is in force.
  pub fn info(&self, maybe_publisher: Option<&Dsn>) -> Option<CrashReportsInfo> {
    self.backend(maybe_publisher).map(|dsn| CrashReportsInfo {
      dsn: dsn.to_string(),
      sample_percent: self.sample_percent,
      trace_percent: self.trace_percent,
      environment: self.environment.clone(),
      allows_opt_out: self.allows_opt_out,
      shows_notice: self.shows_notice,
    })
  }
}

fn is_on() -> bool {
  true
}

fn all_percent() -> u8 {
  100
}

/// Lowercase ASCII letters, digits, `-`, `_` and `.`, 1 to [`MAX_CRASH_ENVIRONMENT_CHARS`].
pub fn is_crash_environment(name: &str) -> bool {
  !name.is_empty()
    && name.len() <= MAX_CRASH_ENVIRONMENT_CHARS
    && name
      .chars()
      .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
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

  /// Host (and port) of the backend, for the console to say where reports go.
  pub fn host(&self) -> &str {
    let after_scheme = self
      .base
      .split_once("://")
      .map_or(self.base.as_str(), |(_, rest)| rest);
    after_scheme.split('/').next().unwrap_or(after_scheme)
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
  fn the_publisher_service_is_the_default() {
    let publisher: Dsn = "https://pub@crashes.example.com/1".parse().unwrap();
    let settings = CrashReportSettings::default();
    assert!(settings.check().is_ok());
    assert_eq!(
      settings.info(Some(&publisher)),
      Some(CrashReportsInfo {
        dsn: "https://pub@crashes.example.com/1".to_owned(),
        sample_percent: 100,
        trace_percent: 100,
        environment: "production".to_owned(),
        allows_opt_out: true,
        shows_notice: true,
      })
    );
    // A build without a publisher service sends nothing by default.
    assert_eq!(settings.info(None), None);
  }

  #[test]
  fn an_own_backend_needs_its_dsn_and_off_is_off() {
    let publisher: Dsn = "https://pub@crashes.example.com/1".parse().unwrap();
    let mut settings = CrashReportSettings {
      mode: CrashReportsMode::Own,
      ..CrashReportSettings::default()
    };
    assert!(matches!(settings.check(), Err(CrashReportSettingsError::Dsn(_))));
    assert_eq!(settings.info(Some(&publisher)), None);
    settings.dsn = " https://abc@errors.example.com/2 ".to_owned();
    settings.sample_percent = 25;
    let info = settings.info(Some(&publisher)).unwrap();
    assert_eq!(info.dsn, "https://abc@errors.example.com/2");
    assert_eq!(info.sample_percent, 25);
    settings.mode = CrashReportsMode::Off;
    assert_eq!(settings.info(Some(&publisher)), None);
    settings.mode = CrashReportsMode::Unknown("later".into());
    assert_eq!(settings.check(), Err(CrashReportSettingsError::Mode));
  }

  #[test]
  fn bad_values_send_nothing() {
    let publisher: Dsn = "https://pub@crashes.example.com/1".parse().unwrap();
    let mut settings = CrashReportSettings {
      sample_percent: 101,
      ..CrashReportSettings::default()
    };
    assert_eq!(settings.check(), Err(CrashReportSettingsError::SamplePercent));
    assert_eq!(settings.info(Some(&publisher)), None);
    settings.sample_percent = 0;
    settings.trace_percent = 101;
    assert_eq!(settings.check(), Err(CrashReportSettingsError::TracePercent));
    settings.trace_percent = 5;
    settings.environment = "Prod".to_owned();
    assert_eq!(settings.check(), Err(CrashReportSettingsError::Environment));
  }

  #[test]
  fn settings_keep_their_wire_names() {
    assert_eq!(
      serde_json::to_value(CrashReportSettings::default()).unwrap(),
      serde_json::json!({
        "mode": "publisher",
        "dsn": "",
        "sample_percent": 100,
        "trace_percent": 100,
        "environment": "production",
        "allows_opt_out": true,
        "shows_notice": true,
      })
    );
  }

  #[test]
  fn older_rows_and_servers_get_their_trace_defaults() {
    let row: CrashReportSettings = serde_json::from_value(serde_json::json!({
      "mode": "publisher", "dsn": "", "sample_percent": 10, "environment": "production",
    }))
    .unwrap();
    assert_eq!(row.trace_percent, 100);
    let info: CrashReportsInfo = serde_json::from_value(serde_json::json!({
      "dsn": "https://pub@crashes.example.com/1", "sample_percent": 10, "environment": "production",
    }))
    .unwrap();
    assert_eq!(
      info.trace_percent, 0,
      "an older server asks for no performance data"
    );
  }

  #[test]
  fn parses_a_dsn_and_builds_the_envelope_url() {
    let dsn: Dsn = "https://abc123@errors.example.com/42".parse().unwrap();
    assert_eq!(dsn.project_id(), "42");
    assert_eq!(dsn.host(), "errors.example.com");
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
