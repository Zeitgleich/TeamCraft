//! System events of conversations.

use teamcraft_proto::{ConversationId, SystemEventId, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, from_db_opt, to_db};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSystemEvent {
  pub id: SystemEventId,
  pub conversation: ConversationId,
  pub kind: String,
  pub maybe_actor: Option<UserId>,
  pub maybe_subject: Option<UserId>,
  pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct NewSystemEvent<'a> {
  pub id: SystemEventId,
  pub conversation: ConversationId,
  pub kind: &'a str,
  pub maybe_actor: Option<UserId>,
  pub maybe_subject: Option<UserId>,
}

impl Database {
  pub async fn record_system_event(&self, new: &NewSystemEvent<'_>) -> Result<StoredSystemEvent, DbError> {
    let row = sqlx::query!(
      r#"INSERT INTO system_events (id, conversation_id, kind, actor_user_id, subject_user_id)
         VALUES ($1, $2, $3, $4, $5) RETURNING created_at"#,
      to_db(new.id),
      to_db(new.conversation),
      new.kind,
      new.maybe_actor.map(to_db),
      new.maybe_subject.map(to_db),
    )
    .fetch_one(self.pool())
    .await?;
    Ok(StoredSystemEvent {
      id: new.id,
      conversation: new.conversation,
      kind: new.kind.to_owned(),
      maybe_actor: new.maybe_actor,
      maybe_subject: new.maybe_subject,
      created_at: row.created_at,
    })
  }

  /// Events of the conversation with IDs after `after` (ascending), at most `limit + 1`.
  pub async fn list_system_events(
    &self,
    conversation: ConversationId,
    maybe_after: Option<SystemEventId>,
    limit: i64,
  ) -> Result<Vec<StoredSystemEvent>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT id, kind, actor_user_id, subject_user_id, created_at FROM system_events
         WHERE conversation_id = $1 AND ($2::bigint IS NULL OR id > $2)
         ORDER BY id LIMIT $3"#,
      to_db(conversation),
      maybe_after.map(to_db),
      limit + 1,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(StoredSystemEvent {
          id: from_db(row.id)?,
          conversation,
          kind: row.kind,
          maybe_actor: from_db_opt(row.actor_user_id)?,
          maybe_subject: from_db_opt(row.subject_user_id)?,
          created_at: row.created_at,
        })
      })
      .collect()
  }
}
