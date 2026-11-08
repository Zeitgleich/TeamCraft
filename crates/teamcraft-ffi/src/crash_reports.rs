//! Crash reports of the mobile apps (docs/crash-reports.md, "The Android SDK" and "iOS"): the
//! instance's setting from `GET /api/v1/server-info`, checked the way the web and desktop apps
//! check it, for the platform SDKs. Scrubbing and sending stay in the SDKs' `beforeSend`.

use teamcraft_client_api::crash_reporting::CrashReportingConfig;
use teamcraft_proto::ServerInfoResponse;

/// What the platform crash SDK is started with while the instance has crash reports on.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct CrashReportsConfig {
  /// The backend project's DSN, normalised (a legacy secret key dropped).
  pub dsn: String,
  /// Share of handled errors that are sent, 0 to 1; crashes are always sent.
  pub sample_rate: f64,
  /// The environment the backend groups reports by (`production`).
  pub environment: String,
  /// The user's switch counts; off: the instance requires reports.
  pub allows_opt_out: bool,
  /// Say once, on the first start with reports on, that the app sends them.
  pub shows_notice: bool,
}

/// The crash reports setting in a `GET /api/v1/server-info` answer (`response_json`, status 200).
/// `None` while the instance has them off, and for an answer or a value that doesn't check: then
/// reports stay off rather than trusting it.
#[uniffi::export]
pub fn crash_reports_config(response_json: String) -> Option<CrashReportsConfig> {
  let info: ServerInfoResponse = serde_json::from_str(&response_json).ok()?;
  let config = CrashReportingConfig::from_info(info.maybe_crash_reports.as_ref()?).ok()?;
  Some(CrashReportsConfig {
    dsn: config.dsn.to_string(),
    sample_rate: config.sample_rate.get(),
    environment: config.environment.as_str().to_owned(),
    allows_opt_out: config.allows_opt_out,
    shows_notice: config.shows_notice,
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  const DSN: &str = "https://0123456789abcdef0123456789abcdef@errors.example.com/7";

  #[test]
  fn an_instance_with_reports_on_gives_the_checked_config() {
    let config = crash_reports_config(answer(&format!(
      r#"{{"dsn":"{DSN}","sample_percent":25,"environment":"production","allows_opt_out":false,"shows_notice":true}}"#
    )))
    .expect("config");
    assert_eq!(config.dsn, DSN);
    assert!((config.sample_rate - 0.25).abs() < f64::EPSILON);
    assert_eq!(config.environment, "production");
    assert!(!config.allows_opt_out);
    assert!(config.shows_notice);
  }

  #[test]
  fn reports_off_or_an_invalid_value_gives_none() {
    assert_eq!(crash_reports_config(answer("null")), None);
    assert_eq!(
      crash_reports_config(answer(
        r#"{"dsn":"http://key@errors.example.com/7","sample_percent":10,"environment":"production"}"#
      )),
      None
    );
    assert_eq!(
      crash_reports_config(answer(&format!(
        r#"{{"dsn":"{DSN}","sample_percent":10,"environment":"Prod uction"}}"#
      ))),
      None
    );
    assert_eq!(crash_reports_config("not json".to_owned()), None);
  }

  /// A minimal server-info answer with `crash_reports` set to `crash_reports` (JSON).
  fn answer(crash_reports: &str) -> String {
    serde_json::json!({
      "product": "teamcraft",
      "server_version": "1.0.0",
      "minimum_client_version": "1.0.0",
      "brand": { "id": "example", "display_name": "Example Chat", "origin": "https://team.example.com" },
      "sign_in": {
        "issuer": "https://auth.example.com/realms/company",
        "client_id": "teamcraft",
        "scopes": ["openid"],
      },
      "crash_reports": serde_json::from_str::<serde_json::Value>(crash_reports).expect("json"),
    })
    .to_string()
  }
}
