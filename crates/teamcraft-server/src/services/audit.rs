//! The audit log (admin.md §4): who did what to whom, metadata only.

use teamcraft_db::NewAuditEntry;
use teamcraft_proto::{Id, UserId};

use crate::error::ApiError;
use crate::state::AppState;

/// What an audit entry is about.
pub struct Target<'a> {
  pub kind: &'a str,
  pub id: i64,
}

impl<'a> Target<'a> {
  pub fn new(kind: &'a str, id: impl Id) -> Self {
    Self {
      kind,
      id: id.snowflake().get(),
    }
  }
}

/// Records an action. `detail` must never contain message content, keys or tokens.
pub async fn record(
  state: &AppState,
  maybe_actor: Option<UserId>,
  action: &str,
  maybe_target: Option<Target<'_>>,
  detail: &serde_json::Value,
) -> Result<(), ApiError> {
  let id = state
    .ids
    .next_snowflake()
    .map_err(|e| ApiError::Internal(e.to_string()))?;
  state
    .database
    .record_audit(&NewAuditEntry {
      id,
      maybe_actor,
      action,
      maybe_target_kind: maybe_target.as_ref().map(|t| t.kind),
      maybe_target_id: maybe_target.as_ref().map(|t| t.id),
      detail: &detail.to_string(),
    })
    .await?;
  Ok(())
}
