//! A brand's own mail server (admin.md §6.2): for brands that belong to organizations with their
//! own relay, SPF and DKIM. Stored without the password, which is sealed with the settings key like
//! the instance's and never sent back. Only database brands have one; the others are read-only.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use serde_json::json;
use teamcraft_db::StoredBrandMailServer;
use teamcraft_proto::{
  AdminBrandsResponse, Permission, SecretUpdate, SendTestMailRequest, SettingsTestResponse, SmtpSecurityMode,
  Target, UpdateBrandMailServerRequest,
};
use time::OffsetDateTime;
use tracing::{error, warn};

use super::access::require;
use super::audit;
use super::brands::{database_index, response};
use super::mail_settings::{
  check_new_password, checked_address, checked_server, optional_text, password_change, seal_password,
  timeout_secs, transport_security,
};
use crate::auth::AuthenticatedUser;
use crate::brand::Brand;
use crate::config::SmtpConfig;
use crate::error::ApiError;
use crate::meetings::ical::Method;
use crate::meetings::mailer::{MailError, Mailer, OutgoingMail, SmtpMailer};
use crate::runtime::brand_mail::{BrandServerView, BrandTransport, BrokenMailer};
use crate::state::AppState;

/// The associated data of a brand's sealed password starts with this, then the brand id: a sealed
/// password can't be moved to another brand or setting.
const SEAL_NAME_PREFIX: &str = "mail.smtp.brand.";

/// A brand's mail server as stored, without the password.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredServer {
  host: String,
  port: u16,
  security: SmtpSecurityMode,
  #[serde(rename = "username", default, skip_serializing_if = "Option::is_none")]
  maybe_username: Option<String>,
}

/// Builds the transports of all brands and puts them in force, at startup and after every change.
/// A server that can't be used still gets a transport, one that fails with the reason: its mail
/// must never move to the instance's server unnoticed.
pub async fn load(state: &AppState) -> Result<(), ApiError> {
  let rows = state.database.stored_brand_mail_servers().await?;
  let registry = state.brands.current();
  let transports: HashMap<String, Arc<BrandTransport>> = rows
    .iter()
    .map(|row| {
      let maybe_brand = registry.by_id(&row.brand_id).map(Arc::as_ref);
      (row.brand_id.clone(), Arc::new(transport(state, row, maybe_brand)))
    })
    .collect();
  state.brand_mail.install(transports);
  Ok(())
}

/// Sets, replaces or (with an empty host) removes a brand's own mail server.
pub async fn put_mail_server(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: &str,
  request: UpdateBrandMailServerRequest,
) -> Result<AdminBrandsResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  if request.host.trim().is_empty() {
    return remove_mail_server(state, caller, id).await;
  }
  let stored = validate(&request)?;
  {
    let _guard = state.settings_lock.lock().await;
    let database = state.brands.database_brands();
    let index = database_index(state, &database, id)?;
    if database[index].config.mail.maybe_from_address.is_none() {
      return Err(ApiError::InvalidRequest(
        "the brand needs a sender address before it can have its own mail server".into(),
      ));
    }
    let maybe_sealed = sealed_password(state, id, &request.password).await?;
    let value = serde_json::to_string(&stored).map_err(|error| ApiError::Internal(error.to_string()))?;
    state
      .database
      .put_brand_mail_server(id, &value, maybe_sealed.as_deref(), caller.user.id)
      .await?;
    state.brand_mail.status_of(id).clear();
    load(state).await?;
  }
  audit::record(
    state,
    Some(caller.user.id),
    "brand_mail_server_updated",
    None,
    &json!({
      "brand_id": id,
      "host": stored.host,
      "port": stored.port,
      "password": password_change(&request.password),
    }),
  )
  .await?;
  Ok(response(state))
}

/// Removes a brand's own mail server; its mail goes through the instance's again.
pub async fn remove_mail_server(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: &str,
) -> Result<AdminBrandsResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let is_deleted = {
    let _guard = state.settings_lock.lock().await;
    let database = state.brands.database_brands();
    database_index(state, &database, id)?;
    let is_deleted = state.database.delete_brand_mail_server(id).await?;
    if is_deleted {
      state.brand_mail.status_of(id).clear();
      load(state).await?;
    }
    is_deleted
  };
  if is_deleted {
    audit::record(
      state,
      Some(caller.user.id),
      "brand_mail_server_removed",
      None,
      &json!({"brand_id": id}),
    )
    .await?;
  }
  Ok(response(state))
}

/// Sends a short mail through the brand's own server, to the caller unless an address is given. It
/// asks the server even right after a failure, and reports what happened.
pub async fn send_test_mail(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: &str,
  request: SendTestMailRequest,
) -> Result<SettingsTestResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let brands = state.brands.current();
  let brand = brands.by_id(id).ok_or_else(ApiError::brand_not_found)?;
  let transport = state
    .brand_mail
    .transport(id)
    .ok_or_else(|| ApiError::Conflict("this brand has no mail server of its own".into()))?;
  let to_address = match request.maybe_to_address {
    Some(address) => checked_address(address.trim())?,
    None => state
      .database
      .get_user_emails(&[caller.user.id])
      .await?
      .remove(&caller.user.id)
      .ok_or_else(|| ApiError::InvalidRequest("you have no e-mail address; give one".into()))?,
  };
  let mail = OutgoingMail {
    to_address,
    maybe_to_name: None,
    subject: format!("Test mail from {}", brand.display_name),
    text: format!(
      "This is a test mail from {}. Its own mail server works.\n",
      brand.display_name
    ),
    method: Method::Request,
    ics: String::new(),
    maybe_from_name: None,
    maybe_from_address: None,
  }
  .as_brand(brand);
  let result = transport.attempt(&mail).await;
  if let Err(error) = &result {
    warn!(%error, brand_id = %id, "the brand's test mail failed");
  }
  audit::record(
    state,
    Some(caller.user.id),
    "brand_mail_test_sent",
    None,
    &json!({"brand_id": id, "is_ok": result.is_ok()}),
  )
  .await?;
  Ok(SettingsTestResponse {
    is_ok: result.is_ok(),
    maybe_error: result.err().map(|error| error.to_string()),
  })
}

fn validate(request: &UpdateBrandMailServerRequest) -> Result<StoredServer, ApiError> {
  let host = checked_server(&request.host, request.port, &request.security)?;
  Ok(StoredServer {
    host,
    port: request.port,
    security: request.security.clone(),
    maybe_username: optional_text(request.maybe_username.as_deref(), "username")?,
  })
}

/// The password to store with the new settings: the new one, none, or the stored one.
async fn sealed_password(
  state: &AppState,
  id: &str,
  update: &SecretUpdate,
) -> Result<Option<Vec<u8>>, ApiError> {
  match update {
    SecretUpdate::Clear => Ok(None),
    SecretUpdate::Keep => Ok(
      state
        .database
        .brand_mail_server(id)
        .await?
        .and_then(|row| row.maybe_sealed_secret),
    ),
    SecretUpdate::Set(secret) => {
      let secret = secret.expose_secret();
      check_new_password(secret)?;
      seal_password(state, &seal_name(id), secret).map(Some)
    },
  }
}

/// The transport of one stored server: a working one, or one that fails with the reason.
fn transport(state: &AppState, row: &StoredBrandMailServer, maybe_brand: Option<&Brand>) -> BrandTransport {
  let status = state.brand_mail.status_of(&row.brand_id);
  let timeout = timeout_secs(state);
  let view_of = |stored: &StoredServer| BrandServerView {
    host: stored.host.clone(),
    port: stored.port,
    security: stored.security.clone(),
    maybe_username: stored.maybe_username.clone(),
    has_password: row.maybe_sealed_secret.is_some(),
  };
  let (view, built) = match serde_json::from_str::<StoredServer>(&row.value) {
    Ok(stored) => (
      view_of(&stored),
      smtp_mailer(state, row, &stored, maybe_brand, timeout),
    ),
    Err(parse_error) => {
      error!(brand_id = %row.brand_id, error = %parse_error, "a brand's stored mail server cannot be read");
      (
        BrandServerView {
          host: String::new(),
          port: 0,
          security: SmtpSecurityMode::StartTls,
          maybe_username: None,
          has_password: row.maybe_sealed_secret.is_some(),
        },
        Err("the stored mail server settings cannot be read".to_owned()),
      )
    },
  };
  let mailer: Arc<dyn Mailer> = match built {
    Ok(mailer) => Arc::new(mailer),
    Err(reason) => {
      error!(brand_id = %row.brand_id, %reason, "a brand's mail server is unusable; its mail fails until it is fixed");
      // Visible at once, not only after the next mail fails.
      status.record(&Err(MailError::Build(reason.clone())), OffsetDateTime::now_utc());
      Arc::new(BrokenMailer::new(reason))
    },
  };
  BrandTransport::new(view, mailer, status, Duration::from_secs(timeout))
}

fn smtp_mailer(
  state: &AppState,
  row: &StoredBrandMailServer,
  stored: &StoredServer,
  maybe_brand: Option<&Brand>,
  timeout_secs: u64,
) -> Result<SmtpMailer, String> {
  let brand = maybe_brand.ok_or_else(|| "the brand is not in force".to_owned())?;
  let from_address = brand
    .mail
    .maybe_from_address
    .clone()
    .ok_or_else(|| "the brand has no sender address".to_owned())?;
  let config = SmtpConfig {
    host: stored.host.clone(),
    port: stored.port,
    security: transport_security(&stored.security),
    username: stored.maybe_username.clone(),
    password: opened_password(state, row)?,
    from_address,
    from_name: brand.mail.maybe_sender_name.clone(),
    timeout_secs,
  };
  SmtpMailer::new(&config).map_err(|error| error.to_string())
}

/// The stored password; a failure is one, since sending without it would only be refused later.
fn opened_password(state: &AppState, row: &StoredBrandMailServer) -> Result<Option<SecretString>, String> {
  let Some(sealed) = &row.maybe_sealed_secret else {
    return Ok(None);
  };
  let key = state
    .maybe_settings_key
    .as_ref()
    .ok_or_else(|| "a password is stored but no settings key is configured ([secrets])".to_owned())?;
  let plain = key
    .open(&seal_name(&row.brand_id), sealed)
    .map_err(|_| "the stored password cannot be decrypted with this settings key".to_owned())?;
  String::from_utf8(plain.expose_secret().to_vec())
    .map(|text| Some(SecretString::from(text)))
    .map_err(|_| "the stored password cannot be decrypted with this settings key".to_owned())
}

fn seal_name(id: &str) -> String {
  format!("{SEAL_NAME_PREFIX}{id}")
}

#[cfg(test)]
mod tests {
  use super::*;

  fn request() -> UpdateBrandMailServerRequest {
    UpdateBrandMailServerRequest {
      host: " SMTP.Brand-a.example.com ".into(),
      port: 587,
      security: SmtpSecurityMode::StartTls,
      maybe_username: Some(" mailer ".into()),
      password: SecretUpdate::Keep,
    }
  }

  #[test]
  fn valid_settings_are_normalized() {
    let stored = validate(&request()).unwrap();
    assert_eq!(stored.host, "smtp.brand-a.example.com");
    assert_eq!(stored.maybe_username.as_deref(), Some("mailer"));
  }

  #[test]
  fn the_instance_guard_applies() {
    for (index, change) in [
      |r: &mut UpdateBrandMailServerRequest| r.host = "smtp example.com".into(),
      |r: &mut UpdateBrandMailServerRequest| r.port = 0,
      |r: &mut UpdateBrandMailServerRequest| r.security = SmtpSecurityMode::Plain,
      |r: &mut UpdateBrandMailServerRequest| r.security = SmtpSecurityMode::Unknown("ssl3".into()),
      |r: &mut UpdateBrandMailServerRequest| r.maybe_username = Some("a\nb".into()),
    ]
    .into_iter()
    .enumerate()
    {
      let mut broken = request();
      change(&mut broken);
      assert!(validate(&broken).is_err(), "broken case {index}");
    }
  }

  #[test]
  fn sealed_passwords_are_bound_to_their_brand() {
    assert_ne!(seal_name("a"), seal_name("b"));
  }
}
