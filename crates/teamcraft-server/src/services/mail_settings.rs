//! The mail server of the server's own mail (admin.md §6.2): set in the console, else
//! `[mail.smtp]` (or `[meetings.smtp]`) of the configuration file. The password is write-only: it
//! is sealed with the settings key before it reaches the database and never sent back.

use std::sync::Arc;

use rand_core::OsRng;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::json;
use teamcraft_db::StoredSetting;
use teamcraft_proto::{
  AdminMailSettingsResponse, Permission, SecretUpdate, SendTestMailRequest, SettingSource,
  SettingsTestResponse, SmtpSecurityMode, SmtpSettings, Target, UpdateMailSettingsRequest,
};
use tracing::{error, warn};

use super::access::require;
use super::audit;
use crate::auth::AuthenticatedUser;
use crate::config::{SmtpConfig, SmtpSecurity};
use crate::error::ApiError;
use crate::meetings::ical::Method;
use crate::meetings::mailer::{Mailer, OutgoingMail, SmtpMailer};
use crate::runtime::mail_status::TrackedMailer;
use crate::state::AppState;

/// Row in `server_settings`; also the associated data of the sealed password.
const SETTING_NAME: &str = "mail.smtp";
const DEFAULT_TIMEOUT_SECS: u64 = 20;
const MAX_TEXT_CHARS: usize = 200;
const MAX_PASSWORD_BYTES: usize = 1024;

/// The mail server as stored, without the password.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredSmtp {
  host: String,
  port: u16,
  security: SmtpSecurityMode,
  #[serde(rename = "username", default, skip_serializing_if = "Option::is_none")]
  maybe_username: Option<String>,
  from_address: String,
  #[serde(rename = "from_name", default, skip_serializing_if = "Option::is_none")]
  maybe_from_name: Option<String>,
}

/// Puts the mail server in force: the stored one, else the configuration's. At startup and after
/// every change.
pub async fn load(state: &AppState) -> Result<(), ApiError> {
  let maybe_stored = state.database.server_setting(SETTING_NAME).await?;
  let maybe_config = match &maybe_stored {
    Some(row) => stored_config(state, row),
    None => state.settings.get().maybe_mail_seed.clone(),
  };
  let maybe_mailer = match maybe_config {
    Some(config) => match SmtpMailer::new(&config) {
      Ok(mailer) => Some(Arc::new(TrackedMailer::new(
        Arc::new(mailer),
        Arc::clone(&state.mail_status),
      )) as Arc<dyn Mailer>),
      Err(error) => {
        error!(%error, "the mail server settings are unusable; no mail is sent");
        None
      },
    },
    None => None,
  };
  state.mailer.set(maybe_mailer);
  Ok(())
}

pub async fn get_mail_settings(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<AdminMailSettingsResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  response(state).await
}

/// Replaces the mail server.
pub async fn put_mail_settings(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: UpdateMailSettingsRequest,
) -> Result<AdminMailSettingsResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let stored = validate(&request)?;
  {
    let _guard = state.settings_lock.lock().await;
    let maybe_sealed = sealed_password(state, &request.password).await?;
    let value = serde_json::to_string(&stored).map_err(|error| ApiError::Internal(error.to_string()))?;
    state
      .database
      .put_server_setting(SETTING_NAME, &value, maybe_sealed.as_deref(), caller.user.id)
      .await?;
    state.mail_status.clear();
    load(state).await?;
  }
  audit::record(
    state,
    Some(caller.user.id),
    "mail_settings_updated",
    None,
    &json!({"host": stored.host, "port": stored.port, "password": password_change(&request.password)}),
  )
  .await?;
  response(state).await
}

/// Goes back to the configuration file's mail server.
pub async fn reset_mail_settings(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<AdminMailSettingsResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let is_deleted = {
    let _guard = state.settings_lock.lock().await;
    let is_deleted = state.database.delete_server_setting(SETTING_NAME).await?;
    if is_deleted {
      state.mail_status.clear();
    }
    load(state).await?;
    is_deleted
  };
  if is_deleted {
    audit::record(
      state,
      Some(caller.user.id),
      "mail_settings_reset",
      None,
      &json!({}),
    )
    .await?;
  }
  response(state).await
}

/// Sends a short mail through the mail server in force, to the caller unless an address is given.
pub async fn send_test_mail(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: SendTestMailRequest,
) -> Result<SettingsTestResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let mailer = state
    .mailer
    .get()
    .ok_or_else(|| ApiError::Unavailable("no mail server is configured".into()))?;
  let to_address = match request.maybe_to_address {
    Some(address) => checked_address(address.trim())?,
    None => state
      .database
      .get_user_emails(&[caller.user.id])
      .await?
      .remove(&caller.user.id)
      .ok_or_else(|| ApiError::InvalidRequest("you have no e-mail address; give one".into()))?,
  };
  let brands = state.brands.current();
  let brand = brands.for_email(&to_address);
  let mail = OutgoingMail {
    to_address,
    maybe_to_name: None,
    subject: format!("Test mail from {}", brand.display_name),
    text: format!(
      "This is a test mail from {}. The mail settings work.\n",
      brand.display_name
    ),
    method: Method::Request,
    ics: String::new(),
    maybe_from_name: None,
    maybe_from_address: None,
  }
  .as_brand(brand);
  let result = mailer.send(&mail).await;
  if let Err(error) = &result {
    warn!(%error, "the test mail failed");
  }
  audit::record(
    state,
    Some(caller.user.id),
    "mail_test_sent",
    None,
    &json!({"is_ok": result.is_ok()}),
  )
  .await?;
  Ok(SettingsTestResponse {
    is_ok: result.is_ok(),
    maybe_error: result.err().map(|error| error.to_string()),
  })
}

async fn response(state: &AppState) -> Result<AdminMailSettingsResponse, ApiError> {
  let maybe_stored = state.database.server_setting(SETTING_NAME).await?;
  let (source, maybe_smtp) = match &maybe_stored {
    Some(row) => (
      SettingSource::Database,
      parse_stored(row).map(|stored| SmtpSettings {
        host: stored.host,
        port: stored.port,
        security: stored.security,
        maybe_username: stored.maybe_username,
        has_password: row.maybe_sealed_secret.is_some(),
        from_address: stored.from_address,
        maybe_from_name: stored.maybe_from_name,
      }),
    ),
    None => (
      SettingSource::Config,
      state
        .settings
        .get()
        .maybe_mail_seed
        .as_ref()
        .map(|config| SmtpSettings {
          host: config.host.clone(),
          port: config.port,
          security: security_mode(config.security),
          maybe_username: config.username.clone(),
          has_password: config.password.is_some(),
          from_address: config.from_address.clone(),
          maybe_from_name: config.from_name.clone(),
        }),
    ),
  };
  Ok(AdminMailSettingsResponse {
    source,
    maybe_smtp,
    can_store_secrets: state.maybe_settings_key.is_some(),
    status: state.mail_status.snapshot(),
  })
}

/// The password to store with the new settings: the new one, none, or the one in force (which may
/// come from the configuration file and is then sealed for the database).
async fn sealed_password(state: &AppState, update: &SecretUpdate) -> Result<Option<Vec<u8>>, ApiError> {
  let maybe_plain: Option<SecretString> = match update {
    SecretUpdate::Clear => return Ok(None),
    SecretUpdate::Set(secret) => {
      let secret = secret.expose_secret();
      if secret.is_empty() || secret.len() > MAX_PASSWORD_BYTES {
        return Err(ApiError::InvalidRequest(format!(
          "the password must be 1 to {MAX_PASSWORD_BYTES} bytes"
        )));
      }
      Some(SecretString::from(secret.to_owned()))
    },
    SecretUpdate::Keep => match state.database.server_setting(SETTING_NAME).await? {
      // Stored already: keep the sealed bytes as they are.
      Some(row) => return Ok(row.maybe_sealed_secret),
      None => state
        .settings
        .get()
        .maybe_mail_seed
        .as_ref()
        .and_then(|config| config.password.clone()),
    },
  };
  let Some(plain) = maybe_plain else {
    return Ok(None);
  };
  let key = state.maybe_settings_key.as_ref().ok_or_else(|| {
    ApiError::Unavailable(
      "passwords can only be stored with a settings key ([secrets] in the configuration)".into(),
    )
  })?;
  key
    .seal(SETTING_NAME, plain.expose_secret().as_bytes(), &mut OsRng)
    .map(Some)
    .map_err(|error| ApiError::Internal(error.to_string()))
}

/// The stored settings as a transport configuration; the password is opened with the settings key.
fn stored_config(state: &AppState, row: &StoredSetting) -> Option<SmtpConfig> {
  let stored = parse_stored(row)?;
  let password = match (&row.maybe_sealed_secret, &state.maybe_settings_key) {
    (None, _) => None,
    (Some(sealed), Some(key)) => match key.open(SETTING_NAME, sealed) {
      Ok(plain) => String::from_utf8(plain.expose_secret().to_vec())
        .ok()
        .map(SecretString::from),
      Err(_) => {
        error!("the stored mail server password cannot be decrypted with this settings key");
        None
      },
    },
    (Some(_), None) => {
      error!("a mail server password is stored but no settings key is configured ([secrets])");
      None
    },
  };
  let timeout_secs = state
    .settings
    .get()
    .maybe_mail_seed
    .as_ref()
    .map_or(DEFAULT_TIMEOUT_SECS, |config| config.timeout_secs);
  Some(SmtpConfig {
    host: stored.host,
    port: stored.port,
    security: match stored.security {
      SmtpSecurityMode::Tls => SmtpSecurity::Tls,
      SmtpSecurityMode::Plain => SmtpSecurity::None,
      SmtpSecurityMode::StartTls | SmtpSecurityMode::Unknown(_) => SmtpSecurity::StartTls,
    },
    username: stored.maybe_username,
    password,
    from_address: stored.from_address,
    from_name: stored.maybe_from_name,
    timeout_secs,
  })
}

fn parse_stored(row: &StoredSetting) -> Option<StoredSmtp> {
  serde_json::from_str(&row.value)
    .inspect_err(|error| warn!(%error, "the stored mail server settings cannot be read"))
    .ok()
}

fn validate(request: &UpdateMailSettingsRequest) -> Result<StoredSmtp, ApiError> {
  let host = request.host.trim().to_ascii_lowercase();
  let is_host_ok = !host.is_empty()
    && host.len() <= 253
    && host
      .bytes()
      .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'));
  if !is_host_ok {
    return Err(ApiError::InvalidRequest(
      "host must be a host name or address".into(),
    ));
  }
  if request.port == 0 {
    return Err(ApiError::InvalidRequest(
      "port must be between 1 and 65535".into(),
    ));
  }
  match &request.security {
    SmtpSecurityMode::Unknown(_) => {
      return Err(ApiError::InvalidRequest("unknown security mode".into()));
    },
    // Without encryption the password would cross the network in plain text.
    SmtpSecurityMode::Plain if !is_loopback(&host) => {
      return Err(ApiError::InvalidRequest(
        "a mail server without encryption is only allowed on this machine (localhost)".into(),
      ));
    },
    _ => {},
  }
  let maybe_username = optional_text(request.maybe_username.as_deref(), "username")?;
  let maybe_from_name = optional_text(request.maybe_from_name.as_deref(), "from_name")?;
  Ok(StoredSmtp {
    host,
    port: request.port,
    security: request.security.clone(),
    maybe_username,
    from_address: checked_address(request.from_address.trim())?,
    maybe_from_name,
  })
}

fn optional_text(maybe_text: Option<&str>, name: &str) -> Result<Option<String>, ApiError> {
  let Some(text) = maybe_text.map(str::trim).filter(|text| !text.is_empty()) else {
    return Ok(None);
  };
  if text.chars().count() > MAX_TEXT_CHARS || text.chars().any(char::is_control) {
    return Err(ApiError::InvalidRequest(format!(
      "{name} must be one line of at most {MAX_TEXT_CHARS} characters"
    )));
  }
  Ok(Some(text.to_owned()))
}

/// A plain address (no display name, no line breaks), as lettre parses it.
fn checked_address(address: &str) -> Result<String, ApiError> {
  match address.parse::<lettre::Address>() {
    Ok(parsed) if parsed.to_string() == address => Ok(address.to_owned()),
    _ => Err(ApiError::InvalidRequest(format!(
      "{address:?} is not an e-mail address"
    ))),
  }
}

fn is_loopback(host: &str) -> bool {
  let bare = host.trim_start_matches('[').trim_end_matches(']');
  host == "localhost"
    || host.ends_with(".localhost")
    || bare.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn security_mode(security: SmtpSecurity) -> SmtpSecurityMode {
  match security {
    SmtpSecurity::StartTls => SmtpSecurityMode::StartTls,
    SmtpSecurity::Tls => SmtpSecurityMode::Tls,
    SmtpSecurity::None => SmtpSecurityMode::Plain,
  }
}

/// What happened to the password, for the audit log (never the password itself).
fn password_change(update: &SecretUpdate) -> &'static str {
  match update {
    SecretUpdate::Keep => "kept",
    SecretUpdate::Clear => "cleared",
    SecretUpdate::Set(_) => "changed",
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::WriteOnlySecret;

  use super::*;

  fn request() -> UpdateMailSettingsRequest {
    UpdateMailSettingsRequest {
      host: "SMTP.example.com".into(),
      port: 587,
      security: SmtpSecurityMode::StartTls,
      maybe_username: Some(" teamcraft ".into()),
      password: SecretUpdate::Keep,
      from_address: "no-reply@example.com".into(),
      maybe_from_name: Some("".into()),
    }
  }

  #[test]
  fn valid_settings_are_normalized() {
    let stored = validate(&request()).unwrap();
    assert_eq!(stored.host, "smtp.example.com");
    assert_eq!(stored.maybe_username.as_deref(), Some("teamcraft"));
    assert_eq!(stored.maybe_from_name, None);
  }

  #[test]
  fn plain_text_mail_is_only_allowed_on_this_machine() {
    let mut plain = request();
    plain.security = SmtpSecurityMode::Plain;
    assert!(validate(&plain).is_err());
    for host in ["localhost", "127.0.0.1", "[::1]", "mail.localhost"] {
      plain.host = host.into();
      assert!(validate(&plain).is_ok(), "{host}");
    }
  }

  #[test]
  fn broken_values_are_refused() {
    for change in [
      |r: &mut UpdateMailSettingsRequest| r.host = "smtp example.com".into(),
      |r: &mut UpdateMailSettingsRequest| r.host = String::new(),
      |r: &mut UpdateMailSettingsRequest| r.port = 0,
      |r: &mut UpdateMailSettingsRequest| r.from_address = "not an address".into(),
      |r: &mut UpdateMailSettingsRequest| r.from_address = "a@example.com\r\nBcc: b@example.com".into(),
      |r: &mut UpdateMailSettingsRequest| r.maybe_from_name = Some("A\nB".into()),
      |r: &mut UpdateMailSettingsRequest| r.security = SmtpSecurityMode::Unknown("ssl3".into()),
    ] {
      let mut broken = request();
      change(&mut broken);
      assert!(validate(&broken).is_err(), "{broken:?}");
    }
  }

  #[test]
  fn the_audit_names_the_password_change_but_never_the_password() {
    let set = SecretUpdate::Set(WriteOnlySecret::new("hunter2".into()));
    assert_eq!(password_change(&set), "changed");
    assert!(!format!("{set:?}").contains("hunter2"));
  }
}
