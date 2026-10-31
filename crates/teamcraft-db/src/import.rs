//! Imported history (e2ee.md §4.10, admin.md §3.8): imported messages are numbered below a
//! conversation's live ones, on the import worker, and marked so they count as unread for nobody.

use teamcraft_proto::{
  ClientMessageId, ConversationId, DeviceId, MessageId, ThreadId, UserId, next_import_id,
};

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, from_db_opt, to_db};

/// One imported message to store.
#[derive(Debug, Clone)]
pub struct NewImportedMessage<'a> {
  pub client_message_id: ClientMessageId,
  pub maybe_thread_id: Option<ThreadId>,
  pub sent_at_ms: i64,
  pub content_key_id: &'a [u8],
  pub ciphertext: &'a [u8],
}

/// What [`Database::store_imported_messages`] did. Nothing is stored unless it is `Stored`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
  /// The IDs in the request's order; a client message ID stored before keeps its ID.
  Stored(Vec<(ClientMessageId, MessageId)>),
  /// No ID is left below the conversation's first live message.
  WindowFull,
  /// A message names a thread root that isn't a top-level message of the conversation.
  UnknownThread(ClientMessageId),
  /// The conversation doesn't exist.
  ConversationGone,
}

impl Database {
  /// Stores `messages` of `sender` in `conversation`, in order, in one transaction. Each new one gets
  /// [`next_import_id`] after the conversation's last imported message, which must stay below its
  /// first message that wasn't imported.
  pub async fn store_imported_messages(
    &self,
    conversation: ConversationId,
    sender: UserId,
    device: DeviceId,
    messages: &[NewImportedMessage<'_>],
  ) -> Result<ImportOutcome, DbError> {
    let mut tx = self.pool().begin().await?;
    let Some(row) = sqlx::query!(
      "SELECT import_last_message_id FROM conversations WHERE id = $1 FOR UPDATE",
      to_db(conversation),
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
      return Ok(ImportOutcome::ConversationGone);
    };
    let mut maybe_last: Option<MessageId> = from_db_opt(row.import_last_message_id)?;
    // Live messages all lie above the imported ones, so the first after the last imported is the
    // first live one.
    let maybe_first_live: Option<MessageId> = from_db_opt(
      sqlx::query_scalar!(
        r#"SELECT id FROM messages
           WHERE conversation_id = $1 AND id > $2 AND NOT is_imported
           ORDER BY id LIMIT 1"#,
        to_db(conversation),
        maybe_last.map_or(0, to_db),
      )
      .fetch_optional(&mut *tx)
      .await?,
    )?;

    let mut stored = Vec::with_capacity(messages.len());
    for message in messages {
      let client_id = message.client_message_id.as_bytes().as_slice();
      let maybe_existing = sqlx::query_scalar!(
        "SELECT message_id FROM message_client_ids WHERE conversation_id = $1 AND client_message_id = $2",
        to_db(conversation),
        client_id,
      )
      .fetch_optional(&mut *tx)
      .await?;
      if let Some(existing) = maybe_existing {
        stored.push((message.client_message_id, from_db(existing)?));
        continue;
      }
      if let Some(thread) = message.maybe_thread_id {
        let is_root = sqlx::query_scalar!(
          r#"SELECT EXISTS (SELECT 1 FROM messages
                            WHERE id = $1 AND conversation_id = $2 AND thread_id IS NULL) AS "is_root!""#,
          to_db(thread),
          to_db(conversation),
        )
        .fetch_one(&mut *tx)
        .await?;
        if !is_root {
          return Ok(ImportOutcome::UnknownThread(message.client_message_id));
        }
      }
      let Some(id) = next_import_id(maybe_last, message.sent_at_ms) else {
        return Ok(ImportOutcome::WindowFull);
      };
      if maybe_first_live.is_some_and(|first_live| id >= first_live) {
        return Ok(ImportOutcome::WindowFull);
      }
      sqlx::query!(
        r#"INSERT INTO message_client_ids (conversation_id, client_message_id, message_id)
           VALUES ($1, $2, $3)"#,
        to_db(conversation),
        client_id,
        to_db(id),
      )
      .execute(&mut *tx)
      .await?;
      sqlx::query!(
        r#"INSERT INTO messages (id, conversation_id, thread_id, sender_device_id, sender_user_id, content_key_id,
                                 mentioned_user_ids, ciphertext, client_message_id, is_urgent, is_imported)
           VALUES ($1, $2, $3, $4, $5, $6, '{}', $7, $8, false, true)"#,
        to_db(id),
        to_db(conversation),
        message.maybe_thread_id.map(to_db),
        to_db(device),
        to_db(sender),
        message.content_key_id,
        message.ciphertext,
        client_id,
      )
      .execute(&mut *tx)
      .await?;
      maybe_last = Some(id);
      stored.push((message.client_message_id, id));
    }
    sqlx::query!(
      "UPDATE conversations SET import_last_message_id = $2 WHERE id = $1",
      to_db(conversation),
      maybe_last.map(to_db),
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(ImportOutcome::Stored(stored))
  }

  /// Marks the conversation's import as finished up to its newest imported message, which it
  /// returns (`None`: nothing was imported, or no such conversation).
  pub async fn finish_import(&self, conversation: ConversationId) -> Result<Option<MessageId>, DbError> {
    let maybe_through = sqlx::query_scalar!(
      r#"UPDATE conversations SET imported_through_message_id = import_last_message_id
         WHERE id = $1 RETURNING imported_through_message_id"#,
      to_db(conversation),
    )
    .fetch_optional(self.pool())
    .await?
    .flatten();
    from_db_opt(maybe_through)
  }

  /// Makes `importer` a member of the participants' conversation `conversation` who is no
  /// participant (idempotent).
  pub async fn add_importer(&self, conversation: ConversationId, importer: UserId) -> Result<(), DbError> {
    let mut tx = self.pool().begin().await?;
    sqlx::query!(
      r#"INSERT INTO memberships (conversation_id, user_id, role) VALUES ($1, $2, 'member')
         ON CONFLICT DO NOTHING"#,
      to_db(conversation),
      to_db(importer),
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
      "UPDATE conversations SET importer_user_id = $2 WHERE id = $1",
      to_db(conversation),
      to_db(importer),
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
  }

  /// Drops `importer`'s membership of `conversation` it holds as the importer; `false` when it
  /// holds none (already dropped, or never added).
  pub async fn remove_importer(
    &self,
    conversation: ConversationId,
    importer: UserId,
  ) -> Result<bool, DbError> {
    let mut tx = self.pool().begin().await?;
    let cleared = sqlx::query!(
      "UPDATE conversations SET importer_user_id = NULL WHERE id = $1 AND importer_user_id = $2",
      to_db(conversation),
      to_db(importer),
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if cleared == 0 {
      return Ok(false);
    }
    sqlx::query!(
      "DELETE FROM memberships WHERE conversation_id = $1 AND user_id = $2",
      to_db(conversation),
      to_db(importer),
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
  }
}
