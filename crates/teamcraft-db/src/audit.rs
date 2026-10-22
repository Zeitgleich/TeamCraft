use teamcraft_proto::{Snowflake, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db_opt, to_db};

/// An audit entry: metadata only, never message content (admin.md §4).
#[derive(Debug, Clone)]
pub struct AuditEntry {
  pub id: Snowflake,
  pub maybe_actor: Option<UserId>,
  pub action: String,
  pub maybe_target_kind: Option<String>,
  pub maybe_target_id: Option<i64>,
  /// JSON text.
  pub detail: String,
  pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct NewAuditEntry<'a> {
  pub id: Snowflake,
  pub maybe_actor: Option<UserId>,
  pub action: &'a str,
  pub maybe_target_kind: Option<&'a str>,
  pub maybe_target_id: Option<i64>,
  /// JSON text.
  pub detail: &'a str,
}

/// Filters of an audit log query; newest first.
#[derive(Debug, Clone, Default)]
pub struct AuditQuery<'a> {
  pub maybe_actor: Option<UserId>,
  pub maybe_action: Option<&'a str>,
  pub maybe_target_kind: Option<&'a str>,
  pub maybe_target_id: Option<i64>,
  pub maybe_since: Option<OffsetDateTime>,
  pub maybe_until: Option<OffsetDateTime>,
  pub maybe_before: Option<Snowflake>,
  pub limit: i64,
}

impl Database {
  pub async fn record_audit(&self, entry: &NewAuditEntry<'_>) -> Result<(), DbError> {
    sqlx::query!(
      r#"INSERT INTO audit_log (id, actor_user_id, action, target_kind, target_id, detail)
         VALUES ($1, $2, $3, $4, $5, $6::text::jsonb)"#,
      entry.id.get(),
      entry.maybe_actor.map(to_db),
      entry.action,
      entry.maybe_target_kind,
      entry.maybe_target_id,
      entry.detail,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Newest first; `limit + 1` rows so callers can tell if more exist.
  pub async fn list_audit(&self, query: &AuditQuery<'_>) -> Result<Vec<AuditEntry>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT id, actor_user_id, action, target_kind, target_id, detail::text AS "detail!", created_at
         FROM audit_log
         WHERE ($1::bigint IS NULL OR actor_user_id = $1)
           AND ($2::text IS NULL OR action = $2)
           AND ($3::text IS NULL OR target_kind = $3)
           AND ($4::bigint IS NULL OR target_id = $4)
           AND ($5::timestamptz IS NULL OR created_at >= $5)
           AND ($6::timestamptz IS NULL OR created_at < $6)
           AND ($7::bigint IS NULL OR id < $7)
         ORDER BY id DESC LIMIT $8"#,
      query.maybe_actor.map(to_db),
      query.maybe_action,
      query.maybe_target_kind,
      query.maybe_target_id,
      query.maybe_since,
      query.maybe_until,
      query.maybe_before.map(Snowflake::get),
      query.limit + 1,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(AuditEntry {
          id: Snowflake::new(row.id).map_err(|e| DbError::InvalidStoredValue(e.to_string()))?,
          maybe_actor: from_db_opt(row.actor_user_id)?,
          action: row.action,
          maybe_target_kind: row.target_kind,
          maybe_target_id: row.target_id,
          detail: row.detail,
          created_at: row.created_at,
        })
      })
      .collect()
  }
}
