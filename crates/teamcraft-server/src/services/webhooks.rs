//! Outgoing webhooks in the admin console (docs/webhooks.md): create, change, delete, rotate the
//! secret, test, and read the delivery log. The secret is generated here, shown once, and stored
//! sealed with the settings key.

use std::time::Instant;

use rand_core::OsRng;
use serde_json::json;
use teamcraft_crypto::webhook::WebhookSecret;
use teamcraft_db::{NewWebhook, NewWebhookLogEntry, StoredWebhook, StoredWebhookLogEntry, WebhookChange};
use teamcraft_proto::{
  AdminWebhook, AdminWebhooksResponse, CreateWebhookRequest, Id as _, MAX_WEBHOOK_DESCRIPTION_CHARS,
  MAX_WEBHOOK_URL_CHARS, MAX_WEBHOOKS, Permission, SettingsTestResponse, Target, UpdateWebhookRequest,
  WebhookDeliveriesResponse, WebhookDeliveryEntry, WebhookDeliveryOutcome, WebhookEventData, WebhookEventId,
  WebhookEventKind, WebhookHealth, WebhookId, WebhookPayload, WebhookSecretResponse, WebhookSecretText,
};
use time::OffsetDateTime;
use tracing::warn;

use super::access::require;
use super::audit::{self, Target as AuditTarget};
use super::convert::unix_millis;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;
use crate::webhooks::{AttemptResult, check_target_url, open_secret, secret_name};

pub async fn list_webhooks(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<AdminWebhooksResponse, ApiError> {
  require_admin(state, caller)?;
  let webhooks = state.database.list_webhooks().await?;
  Ok(AdminWebhooksResponse {
    webhooks: webhooks.iter().map(admin_webhook).collect(),
    can_store_secrets: state.maybe_settings_key.is_some(),
    allows_private_targets: state.webhooks.allows_private(),
  })
}

/// Creates a webhook with a fresh secret, which the answer carries this one time.
pub async fn create_webhook(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: CreateWebhookRequest,
) -> Result<WebhookSecretResponse, ApiError> {
  require_admin(state, caller)?;
  let fields = validate(state, &request.url, &request.description, &request.events)?;
  if state.database.count_webhooks().await? >= MAX_WEBHOOKS as u64 {
    return Err(ApiError::Conflict(format!("at most {MAX_WEBHOOKS} webhooks")));
  }
  let id: WebhookId = state.next_id()?;
  let secret = WebhookSecret::generate(&mut OsRng);
  let sealed = seal(state, id, &secret)?;
  state
    .database
    .create_webhook(&NewWebhook {
      id,
      url: &fields.url,
      description: &fields.description,
      events: &fields.events,
      is_enabled: request.is_enabled,
      sealed_secret: &sealed,
    })
    .await?;
  audit::record(
    state,
    Some(caller.user.id),
    "webhook_created",
    Some(AuditTarget::new("webhook", id)),
    &audit_detail(&fields, request.is_enabled),
  )
  .await?;
  Ok(WebhookSecretResponse {
    webhook: find(state, id).await?,
    secret: WebhookSecretText::new(secret.to_text().to_string()),
  })
}

/// Changes URL, description, events or the enabled flag; the secret stays.
pub async fn update_webhook(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: WebhookId,
  request: UpdateWebhookRequest,
) -> Result<AdminWebhook, ApiError> {
  require_admin(state, caller)?;
  let fields = validate(state, &request.url, &request.description, &request.events)?;
  let is_found = state
    .database
    .update_webhook(
      id,
      &WebhookChange {
        url: &fields.url,
        description: &fields.description,
        events: &fields.events,
        is_enabled: request.is_enabled,
      },
    )
    .await?;
  if !is_found {
    return Err(ApiError::webhook_not_found());
  }
  audit::record(
    state,
    Some(caller.user.id),
    "webhook_updated",
    Some(AuditTarget::new("webhook", id)),
    &audit_detail(&fields, request.is_enabled),
  )
  .await?;
  find(state, id).await
}

pub async fn delete_webhook(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: WebhookId,
) -> Result<(), ApiError> {
  require_admin(state, caller)?;
  if !state.database.delete_webhook(id).await? {
    return Err(ApiError::webhook_not_found());
  }
  audit::record(
    state,
    Some(caller.user.id),
    "webhook_deleted",
    Some(AuditTarget::new("webhook", id)),
    &json!({}),
  )
  .await?;
  Ok(())
}

/// Replaces the secret; the old one stops working at once. The answer carries the new one.
pub async fn rotate_secret(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: WebhookId,
) -> Result<WebhookSecretResponse, ApiError> {
  require_admin(state, caller)?;
  let secret = WebhookSecret::generate(&mut OsRng);
  let sealed = seal(state, id, &secret)?;
  if !state.database.replace_webhook_secret(id, &sealed).await? {
    return Err(ApiError::webhook_not_found());
  }
  audit::record(
    state,
    Some(caller.user.id),
    "webhook_secret_rotated",
    Some(AuditTarget::new("webhook", id)),
    &json!({}),
  )
  .await?;
  Ok(WebhookSecretResponse {
    webhook: find(state, id).await?,
    secret: WebhookSecretText::new(secret.to_text().to_string()),
  })
}

/// Sends a `ping` event now, outside the queue, and reports how it went. Logged like a delivery,
/// but it neither changes the webhook's health nor backs it off.
pub async fn test_webhook(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: WebhookId,
) -> Result<SettingsTestResponse, ApiError> {
  require_admin(state, caller)?;
  let (url, sealed) = state
    .database
    .webhook_target(id)
    .await?
    .ok_or_else(ApiError::webhook_not_found)?;
  let secret = open_secret(state, id, &sealed).ok_or_else(|| {
    ApiError::Unavailable("the webhook secret cannot be read with this settings key".into())
  })?;
  let event_id: WebhookEventId = state.next_id()?;
  let payload = WebhookPayload {
    id: event_id.to_string(),
    kind: WebhookEventKind::Ping,
    created_at: unix_millis(OffsetDateTime::now_utc()),
    data: WebhookEventData::default(),
  };
  let body = serde_json::to_string(&payload).map_err(|error| ApiError::Internal(error.to_string()))?;
  let started = Instant::now();
  let result = state.webhooks.send(&url, &secret, &payload.id, &body).await;
  let (outcome, maybe_status_code, maybe_error) = match result {
    AttemptResult::Delivered { status } => (WebhookDeliveryOutcome::Delivered, Some(status), None),
    AttemptResult::Temporary {
      maybe_status, error, ..
    }
    | AttemptResult::Permanent { maybe_status, error } => {
      (WebhookDeliveryOutcome::Failed, maybe_status, Some(error))
    },
  };
  let log_id: WebhookEventId = state.next_id()?;
  let entry = NewWebhookLogEntry {
    id: log_id.snowflake().get(),
    webhook_id: id,
    event_id: event_id.snowflake().get(),
    event_kind: WebhookEventKind::Ping,
    attempt: 1,
    outcome: outcome.clone(),
    maybe_status_code,
    maybe_error: maybe_error.clone(),
    duration: started.elapsed(),
  };
  let keep = state.settings.get().webhooks.log_entries_per_webhook;
  if let Err(error) = state.database.log_webhook_attempt(&entry, keep).await {
    warn!(%error, webhook_id = %id, "logging a webhook test failed");
  }
  audit::record(
    state,
    Some(caller.user.id),
    "webhook_test",
    Some(AuditTarget::new("webhook", id)),
    &json!({"is_ok": outcome == WebhookDeliveryOutcome::Delivered}),
  )
  .await?;
  Ok(SettingsTestResponse {
    is_ok: outcome == WebhookDeliveryOutcome::Delivered,
    maybe_error,
  })
}

/// The latest attempts, newest first.
pub async fn list_deliveries(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: WebhookId,
) -> Result<WebhookDeliveriesResponse, ApiError> {
  require_admin(state, caller)?;
  if state.database.webhook_target(id).await?.is_none() {
    return Err(ApiError::webhook_not_found());
  }
  let keep = state.settings.get().webhooks.log_entries_per_webhook;
  let entries = state.database.list_webhook_log(id, keep).await?;
  Ok(WebhookDeliveriesResponse {
    entries: entries.iter().map(delivery_entry).collect(),
  })
}

fn require_admin(state: &AppState, caller: &AuthenticatedUser) -> Result<(), ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)
}

/// The checked fields of a create or update request.
struct ValidFields {
  url: String,
  description: String,
  events: Vec<WebhookEventKind>,
}

fn validate(
  state: &AppState,
  url: &str,
  description: &str,
  events: &[WebhookEventKind],
) -> Result<ValidFields, ApiError> {
  let url = url.trim();
  if url.chars().count() > MAX_WEBHOOK_URL_CHARS {
    return Err(ApiError::InvalidRequest(format!(
      "url must be at most {MAX_WEBHOOK_URL_CHARS} characters"
    )));
  }
  check_target_url(url, state.webhooks.allows_private())
    .map_err(|refusal| ApiError::InvalidRequest(format!("url {refusal}")))?;
  let description = description.trim();
  if description.chars().count() > MAX_WEBHOOK_DESCRIPTION_CHARS || description.chars().any(char::is_control)
  {
    return Err(ApiError::InvalidRequest(format!(
      "description must be one line of at most {MAX_WEBHOOK_DESCRIPTION_CHARS} characters"
    )));
  }
  let mut checked: Vec<WebhookEventKind> = Vec::with_capacity(events.len());
  for event in events {
    if !event.is_subscribable() {
      return Err(ApiError::InvalidRequest(format!("unknown event {event}")));
    }
    if !checked.contains(event) {
      checked.push(event.clone());
    }
  }
  if checked.is_empty() {
    return Err(ApiError::InvalidRequest("pick at least one event".into()));
  }
  Ok(ValidFields {
    url: url.to_owned(),
    description: description.to_owned(),
    events: checked,
  })
}

fn seal(state: &AppState, id: WebhookId, secret: &WebhookSecret) -> Result<Vec<u8>, ApiError> {
  let key = state.maybe_settings_key.as_ref().ok_or_else(|| {
    ApiError::Unavailable(
      "webhook secrets can only be stored with a settings key ([secrets] in the configuration)".into(),
    )
  })?;
  key
    .seal(&secret_name(id), secret.expose_secret(), &mut OsRng)
    .map_err(|error| ApiError::Internal(error.to_string()))
}

async fn find(state: &AppState, id: WebhookId) -> Result<AdminWebhook, ApiError> {
  state
    .database
    .list_webhooks()
    .await?
    .iter()
    .find(|stored| stored.id == id)
    .map(admin_webhook)
    .ok_or_else(ApiError::webhook_not_found)
}

/// The audit detail: the target's host only, since a URL's path or query may hold a token.
fn audit_detail(fields: &ValidFields, is_enabled: bool) -> serde_json::Value {
  let host = url::Url::parse(&fields.url)
    .ok()
    .and_then(|url| url.host_str().map(str::to_owned))
    .unwrap_or_default();
  let events: Vec<&str> = fields.events.iter().map(WebhookEventKind::as_str).collect();
  json!({"host": host, "events": events, "is_enabled": is_enabled})
}

fn admin_webhook(stored: &StoredWebhook) -> AdminWebhook {
  AdminWebhook {
    id: stored.id,
    url: stored.url.clone(),
    description: stored.description.clone(),
    events: stored.events.clone(),
    is_enabled: stored.is_enabled,
    created_at: unix_millis(stored.created_at),
    health: WebhookHealth {
      maybe_last_success_at: stored.maybe_last_success_at.map(unix_millis),
      maybe_last_failure_at: stored.maybe_last_failure_at.map(unix_millis),
      maybe_last_error: stored.maybe_last_error.clone(),
      consecutive_failures: stored.consecutive_failures,
      maybe_paused_until: stored
        .maybe_paused_until
        .filter(|until| *until > OffsetDateTime::now_utc())
        .map(unix_millis),
      queued: stored.queued_count,
      dropped: stored.dropped_count,
    },
  }
}

fn delivery_entry(stored: &StoredWebhookLogEntry) -> WebhookDeliveryEntry {
  WebhookDeliveryEntry {
    event_id: stored.event_id.to_string(),
    event: stored.event_kind.clone(),
    attempt: stored.attempt,
    outcome: stored.outcome.clone(),
    maybe_status_code: stored.maybe_status_code,
    maybe_error: stored.maybe_error.clone(),
    duration_ms: stored.duration_ms,
    at: unix_millis(stored.at),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_audit_detail_names_the_host_but_not_the_path() {
    let detail = audit_detail(
      &ValidFields {
        url: "https://hooks.example.com/services/T000/secret-token".into(),
        description: String::new(),
        events: vec![WebhookEventKind::MemberJoined],
      },
      true,
    );
    let text = detail.to_string();
    assert!(text.contains("hooks.example.com"));
    assert!(!text.contains("secret-token"));
  }
}
