//! The CalDAV calendar meetings are written to (admin.md §6.2): set in the console, else
//! `[meetings.caldav]` of the configuration file. Needs meetings to be configured. The password is
//! write-only and sealed with the settings key like the mail server's.

use std::sync::Arc;

use rand_core::OsRng;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::json;
use teamcraft_db::StoredSetting;
use teamcraft_proto::{
  AdminCalendarSettingsResponse, CalDavSettings, Permission, SecretUpdate, SettingSource,
  SettingsTestResponse, Target, UpdateCalendarSettingsRequest,
};
use tracing::{error, warn};

use super::access::require;
use super::audit;
use crate::auth::AuthenticatedUser;
use crate::config::CalDavConfig;
use crate::error::ApiError;
use crate::meetings::MeetingService;
use crate::meetings::caldav::{CalDavStore, CalendarStore};
use crate::state::AppState;

/// Row in `server_settings`; also the associated data of the sealed password.
const SETTING_NAME: &str = "calendar.caldav";
const DEFAULT_TIMEOUT_SECS: u64 = 10;
const MAX_URL_CHARS: usize = 2000;
const MAX_TEXT_CHARS: usize = 200;
const MAX_PASSWORD_BYTES: usize = 1024;

/// The calendar as stored, without the password.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredCalDav {
  calendar_url: String,
  username: String,
  #[serde(
    rename = "free_busy_url_template",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  maybe_free_busy_url_template: Option<String>,
}

/// Puts the calendar in force: the stored one, else the configuration's. At startup and after
/// every change; nothing to do without meetings.
pub async fn load(state: &AppState) -> Result<(), ApiError> {
  let Some(meetings) = &state.maybe_meetings else {
    return Ok(());
  };
  let maybe_config = match state.database.server_setting(SETTING_NAME).await? {
    Some(row) => stored_config(state, &row),
    None => state.settings.get().maybe_calendar_seed.clone(),
  };
  let maybe_store = match maybe_config {
    Some(config) => match CalDavStore::new(&config) {
      Ok(store) => Some(Arc::new(store) as Arc<dyn CalendarStore>),
      Err(error) => {
        error!(%error, "the calendar settings are unusable; meetings are not stored in a calendar");
        None
      },
    },
    None => None,
  };
  meetings.calendar_slot().set(maybe_store);
  Ok(())
}

pub async fn get_calendar_settings(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<AdminCalendarSettingsResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  response(state).await
}

/// Replaces the calendar.
pub async fn put_calendar_settings(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: UpdateCalendarSettingsRequest,
) -> Result<AdminCalendarSettingsResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  meetings_of(state)?;
  let stored = validate(&request)?;
  {
    let _guard = state.settings_lock.lock().await;
    let sealed = sealed_password(state, &request.password).await?;
    let value = serde_json::to_string(&stored).map_err(|error| ApiError::Internal(error.to_string()))?;
    state
      .database
      .put_server_setting(SETTING_NAME, &value, Some(&sealed), caller.user.id)
      .await?;
    load(state).await?;
  }
  audit::record(
    state,
    Some(caller.user.id),
    "calendar_settings_updated",
    None,
    &json!({"password": if matches!(request.password, SecretUpdate::Set(_)) { "changed" } else { "kept" }}),
  )
  .await?;
  response(state).await
}

/// Goes back to the configuration file's calendar.
pub async fn reset_calendar_settings(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<AdminCalendarSettingsResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let is_deleted = {
    let _guard = state.settings_lock.lock().await;
    let is_deleted = state.database.delete_server_setting(SETTING_NAME).await?;
    load(state).await?;
    is_deleted
  };
  if is_deleted {
    audit::record(
      state,
      Some(caller.user.id),
      "calendar_settings_reset",
      None,
      &json!({}),
    )
    .await?;
  }
  response(state).await
}

/// Asks the calendar in force whether it can be reached with its account.
pub async fn test_calendar(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<SettingsTestResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let calendar = meetings_of(state)?
    .calendar()
    .ok_or_else(|| ApiError::Unavailable("no calendar is configured".into()))?;
  let result = calendar.check().await;
  if let Err(error) = &result {
    warn!(%error, "the calendar connection test failed");
  }
  audit::record(
    state,
    Some(caller.user.id),
    "calendar_test",
    None,
    &json!({"is_ok": result.is_ok()}),
  )
  .await?;
  Ok(SettingsTestResponse {
    is_ok: result.is_ok(),
    maybe_error: result.err().map(|error| error.to_string()),
  })
}

async fn response(state: &AppState) -> Result<AdminCalendarSettingsResponse, ApiError> {
  let maybe_row = state.database.server_setting(SETTING_NAME).await?;
  let (source, maybe_caldav) = match &maybe_row {
    Some(row) => (
      SettingSource::Database,
      parse_stored(row).map(|stored| CalDavSettings {
        calendar_url: stored.calendar_url,
        username: stored.username,
        has_password: row.maybe_sealed_secret.is_some(),
        maybe_free_busy_url_template: stored.maybe_free_busy_url_template,
      }),
    ),
    None => (
      SettingSource::Config,
      state
        .settings
        .get()
        .maybe_calendar_seed
        .as_ref()
        .map(|config| CalDavSettings {
          calendar_url: config.calendar_url.clone(),
          username: config.username.clone(),
          has_password: !config.password.expose_secret().is_empty(),
          maybe_free_busy_url_template: config.maybe_free_busy_url_template.clone(),
        }),
    ),
  };
  Ok(AdminCalendarSettingsResponse {
    is_available: state.maybe_meetings.is_some(),
    source,
    maybe_caldav,
    can_store_secrets: state.maybe_settings_key.is_some(),
  })
}

fn meetings_of(state: &AppState) -> Result<&Arc<MeetingService>, ApiError> {
  state
    .maybe_meetings
    .as_ref()
    .ok_or_else(|| ApiError::Unavailable("meetings are not configured".into()))
}

/// The password to store: the new one or the one in force (a calendar account needs one).
async fn sealed_password(state: &AppState, update: &SecretUpdate) -> Result<Vec<u8>, ApiError> {
  let plain: SecretString = match update {
    SecretUpdate::Clear => {
      return Err(ApiError::InvalidRequest(
        "the calendar account needs a password".into(),
      ));
    },
    SecretUpdate::Set(secret) => {
      let secret = secret.expose_secret();
      if secret.is_empty() || secret.len() > MAX_PASSWORD_BYTES {
        return Err(ApiError::InvalidRequest(format!(
          "the password must be 1 to {MAX_PASSWORD_BYTES} bytes"
        )));
      }
      SecretString::from(secret.to_owned())
    },
    SecretUpdate::Keep => {
      if let Some(sealed) = state
        .database
        .server_setting(SETTING_NAME)
        .await?
        .and_then(|row| row.maybe_sealed_secret)
      {
        return Ok(sealed);
      }
      state
        .settings
        .get()
        .maybe_calendar_seed
        .as_ref()
        .map(|config| config.password.clone())
        .ok_or_else(|| ApiError::InvalidRequest("the calendar account needs a password".into()))?
    },
  };
  let key = state.maybe_settings_key.as_ref().ok_or_else(|| {
    ApiError::Unavailable(
      "passwords can only be stored with a settings key ([secrets] in the configuration)".into(),
    )
  })?;
  key
    .seal(SETTING_NAME, plain.expose_secret().as_bytes(), &mut OsRng)
    .map_err(|error| ApiError::Internal(error.to_string()))
}

fn stored_config(state: &AppState, row: &StoredSetting) -> Option<CalDavConfig> {
  let stored = parse_stored(row)?;
  let password = match (&row.maybe_sealed_secret, &state.maybe_settings_key) {
    (Some(sealed), Some(key)) => match key.open(SETTING_NAME, sealed) {
      Ok(plain) => String::from_utf8(plain.expose_secret().to_vec()).ok()?,
      Err(_) => {
        error!("the stored calendar password cannot be decrypted with this settings key");
        return None;
      },
    },
    _ => {
      error!("the stored calendar settings have no usable password (is [secrets] configured?)");
      return None;
    },
  };
  let request_timeout_secs = state
    .settings
    .get()
    .maybe_calendar_seed
    .as_ref()
    .map_or(DEFAULT_TIMEOUT_SECS, |config| config.request_timeout_secs);
  Some(CalDavConfig {
    calendar_url: stored.calendar_url,
    username: stored.username,
    password: SecretString::from(password),
    request_timeout_secs,
    maybe_free_busy_url_template: stored.maybe_free_busy_url_template,
  })
}

fn parse_stored(row: &StoredSetting) -> Option<StoredCalDav> {
  serde_json::from_str(&row.value)
    .inspect_err(|error| warn!(%error, "the stored calendar settings cannot be read"))
    .ok()
}

fn validate(request: &UpdateCalendarSettingsRequest) -> Result<StoredCalDav, ApiError> {
  let calendar_url = checked_url(request.calendar_url.trim(), "calendar_url")?;
  let maybe_free_busy_url_template = match request
    .maybe_free_busy_url_template
    .as_deref()
    .map(str::trim)
    .filter(|template| !template.is_empty())
  {
    Some(template) => {
      if !template.contains("{email}") {
        return Err(ApiError::InvalidRequest(
          "free_busy_url_template must contain {email}".into(),
        ));
      }
      checked_url(
        &template.replace("{email}", "someone%40example.com"),
        "free_busy_url_template",
      )?;
      Some(template.to_owned())
    },
    None => None,
  };
  let username = request.username.trim();
  if username.is_empty()
    || username.chars().count() > MAX_TEXT_CHARS
    || username.chars().any(char::is_control)
  {
    return Err(ApiError::InvalidRequest(format!(
      "username must be one line of 1 to {MAX_TEXT_CHARS} characters"
    )));
  }
  Ok(StoredCalDav {
    calendar_url,
    username: username.to_owned(),
    maybe_free_busy_url_template,
  })
}

/// An `https` URL without credentials; `http` only for this machine (a password would cross the
/// network in plain text).
fn checked_url(raw: &str, name: &str) -> Result<String, ApiError> {
  let invalid = || ApiError::InvalidRequest(format!("{name} must be an https URL without credentials"));
  if raw.chars().count() > MAX_URL_CHARS {
    return Err(invalid());
  }
  let url = url::Url::parse(raw).map_err(|_| invalid())?;
  let is_local = url.host().is_some_and(|host| match host {
    url::Host::Domain(domain) => domain == "localhost" || domain.ends_with(".localhost"),
    url::Host::Ipv4(ip) => ip.is_loopback(),
    url::Host::Ipv6(ip) => ip.is_loopback(),
  });
  let is_scheme_ok = url.scheme() == "https" || (url.scheme() == "http" && is_local);
  if !is_scheme_ok || url.host().is_none() || !url.username().is_empty() || url.password().is_some() {
    return Err(invalid());
  }
  Ok(raw.to_owned())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn request() -> UpdateCalendarSettingsRequest {
    UpdateCalendarSettingsRequest {
      calendar_url: "https://cal.example.com/dav/calendars/teamcraft/meetings/".into(),
      username: "teamcraft".into(),
      password: SecretUpdate::Keep,
      maybe_free_busy_url_template: Some("https://cal.example.com/dav/calendars/{email}/personal/".into()),
    }
  }

  #[test]
  fn a_valid_calendar_is_accepted() {
    let stored = validate(&request()).unwrap();
    assert_eq!(stored.username, "teamcraft");
    assert!(stored.maybe_free_busy_url_template.is_some());
  }

  #[test]
  fn plain_http_only_on_this_machine_and_never_with_credentials() {
    for (url, is_ok) in [
      ("http://cal.example.com/dav/", false),
      ("http://127.0.0.1:5232/dav/", true),
      ("http://localhost:5232/dav/", true),
      ("https://user:pw@cal.example.com/dav/", false),
      ("ftp://cal.example.com/", false),
      ("not a url", false),
    ] {
      let mut changed = request();
      changed.calendar_url = url.into();
      assert_eq!(validate(&changed).is_ok(), is_ok, "{url}");
    }
  }

  #[test]
  fn the_free_busy_template_needs_the_placeholder() {
    let mut changed = request();
    changed.maybe_free_busy_url_template = Some("https://cal.example.com/dav/calendars/x/".into());
    assert!(validate(&changed).is_err());
    changed.maybe_free_busy_url_template = Some("  ".into());
    assert_eq!(validate(&changed).unwrap().maybe_free_busy_url_template, None);
  }
}
