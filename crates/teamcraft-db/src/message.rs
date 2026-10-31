use teamcraft_proto::{ClientMessageId, ConversationId, DeviceId, MessageId, ThreadId, UserId};

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, from_db_all, from_db_opt, to_db, to_db_all};

/// A stored message. `ciphertext` is opaque to the server.
#[derive(Debug, Clone)]
pub struct StoredMessage {
  pub id: MessageId,
  pub client_message_id: ClientMessageId,
  pub conversation_id: ConversationId,
  pub sender_user_id: UserId,
  pub sender_device_id: DeviceId,
  pub maybe_thread_id: Option<ThreadId>,
  pub mentioned_user_ids: Vec<UserId>,
  pub is_urgent: bool,
  pub content_key_id: Vec<u8>,
  pub ciphertext: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct NewMessage<'a> {
  /// Server-assigned Snowflake; only used if the client ID is new.
  pub id: MessageId,
  pub client_message_id: ClientMessageId,
  pub conversation_id: ConversationId,
  pub sender_user_id: UserId,
  pub sender_device_id: DeviceId,
  pub maybe_thread_id: Option<ThreadId>,
  pub mentioned_user_ids: &'a [UserId],
  pub is_urgent: bool,
  pub content_key_id: &'a [u8],
  pub ciphertext: &'a [u8],
}

#[derive(Debug, Clone)]
pub struct StoreOutcome {
  pub message: StoredMessage,
  /// `false`: the client ID was already stored and the existing message is returned.
  pub is_new: bool,
}

/// Direction and bounds of a history page.
#[derive(Debug, Clone)]
pub struct HistoryQuery {
  pub conversation: ConversationId,
  pub maybe_before: Option<MessageId>,
  pub maybe_after: Option<MessageId>,
  /// Messages with a smaller ID are invisible to the caller (history policy, guests).
  pub maybe_not_before: Option<MessageId>,
  /// Imported messages are visible whatever `maybe_not_before` says (a DM's participants, who get
  /// its imported history, e2ee.md §4.10).
  pub sees_imported: bool,
  pub limit: i64,
}

impl Database {
  /// Stores a message once per client message ID.
  pub async fn store_message(&self, new: &NewMessage<'_>) -> Result<StoreOutcome, DbError> {
    let mut tx = self.pool().begin().await?;
    let inserted = sqlx::query_scalar!(
      r#"INSERT INTO message_client_ids (conversation_id, client_message_id, message_id)
         VALUES ($1, $2, $3) ON CONFLICT DO NOTHING RETURNING message_id"#,
      to_db(new.conversation_id),
      new.client_message_id.as_bytes().as_slice(),
      to_db(new.id),
    )
    .fetch_optional(&mut *tx)
    .await?;

    if inserted.is_none() {
      let existing = sqlx::query_scalar!(
        "SELECT message_id FROM message_client_ids WHERE conversation_id = $1 AND client_message_id = $2",
        to_db(new.conversation_id),
        new.client_message_id.as_bytes().as_slice(),
      )
      .fetch_one(&mut *tx)
      .await?;
      tx.commit().await?;
      let message = self
        .get_message(new.conversation_id, from_db(existing)?)
        .await?
        .ok_or_else(|| DbError::InvalidStoredValue("client ID without message".into()))?;
      return Ok(StoreOutcome {
        message,
        is_new: false,
      });
    }

    sqlx::query!(
      r#"INSERT INTO messages (id, conversation_id, thread_id, sender_device_id, sender_user_id, content_key_id,
                               mentioned_user_ids, ciphertext, client_message_id, is_urgent)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"#,
      to_db(new.id),
      to_db(new.conversation_id),
      new.maybe_thread_id.map(to_db),
      to_db(new.sender_device_id),
      to_db(new.sender_user_id),
      new.content_key_id,
      &to_db_all(new.mentioned_user_ids),
      new.ciphertext,
      new.client_message_id.as_bytes().as_slice(),
      new.is_urgent,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let message = StoredMessage {
      id: new.id,
      client_message_id: new.client_message_id,
      conversation_id: new.conversation_id,
      sender_user_id: new.sender_user_id,
      sender_device_id: new.sender_device_id,
      maybe_thread_id: new.maybe_thread_id,
      mentioned_user_ids: new.mentioned_user_ids.to_vec(),
      is_urgent: new.is_urgent,
      content_key_id: new.content_key_id.to_vec(),
      ciphertext: new.ciphertext.to_vec(),
    };
    Ok(StoreOutcome {
      message,
      is_new: true,
    })
  }

  pub async fn get_message(
    &self,
    conversation: ConversationId,
    message: MessageId,
  ) -> Result<Option<StoredMessage>, DbError> {
    let row = sqlx::query!(
      r#"SELECT id, client_message_id, conversation_id, sender_user_id, sender_device_id, thread_id,
                mentioned_user_ids, is_urgent, content_key_id, ciphertext AS "ciphertext?"
         FROM messages WHERE id = $1 AND conversation_id = $2"#,
      to_db(message),
      to_db(conversation),
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|row| {
        stored_from_parts(
          row.id,
          &row.client_message_id,
          row.conversation_id,
          row.sender_user_id,
          row.sender_device_id,
          row.thread_id,
          &row.mentioned_user_ids,
          row.is_urgent,
          row.content_key_id,
          row.ciphertext,
        )
      })
      .transpose()
  }

  /// One page of history. `after` queries return oldest first, all others newest first.
  /// Fetches `limit + 1` rows so the caller can tell whether more exist.
  pub async fn list_messages(&self, query: &HistoryQuery) -> Result<Vec<StoredMessage>, DbError> {
    let is_forward = query.maybe_after.is_some();
    let rows = if is_forward {
      sqlx::query!(
        r#"SELECT id, client_message_id, conversation_id, sender_user_id, sender_device_id, thread_id,
                  mentioned_user_ids, is_urgent, content_key_id, ciphertext AS "ciphertext?"
           FROM messages
           WHERE conversation_id = $1 AND id > $2 AND ($3::bigint IS NULL OR id >= $3 OR (is_imported AND $5))
           ORDER BY id ASC LIMIT $4"#,
        to_db(query.conversation),
        query.maybe_after.map(to_db),
        query.maybe_not_before.map(to_db),
        query.limit + 1,
        query.sees_imported,
      )
      .fetch_all(self.pool())
      .await?
      .into_iter()
      .map(|row| {
        stored_from_parts(
          row.id,
          &row.client_message_id,
          row.conversation_id,
          row.sender_user_id,
          row.sender_device_id,
          row.thread_id,
          &row.mentioned_user_ids,
          row.is_urgent,
          row.content_key_id,
          row.ciphertext,
        )
      })
      .collect::<Result<Vec<_>, _>>()?
    } else {
      sqlx::query!(
        r#"SELECT id, client_message_id, conversation_id, sender_user_id, sender_device_id, thread_id,
                  mentioned_user_ids, is_urgent, content_key_id, ciphertext AS "ciphertext?"
           FROM messages
           WHERE conversation_id = $1 AND ($2::bigint IS NULL OR id < $2)
             AND ($3::bigint IS NULL OR id >= $3 OR (is_imported AND $5))
           ORDER BY id DESC LIMIT $4"#,
        to_db(query.conversation),
        query.maybe_before.map(to_db),
        query.maybe_not_before.map(to_db),
        query.limit + 1,
        query.sees_imported,
      )
      .fetch_all(self.pool())
      .await?
      .into_iter()
      .map(|row| {
        stored_from_parts(
          row.id,
          &row.client_message_id,
          row.conversation_id,
          row.sender_user_id,
          row.sender_device_id,
          row.thread_id,
          &row.mentioned_user_ids,
          row.is_urgent,
          row.content_key_id,
          row.ciphertext,
        )
      })
      .collect::<Result<Vec<_>, _>>()?
    };
    Ok(rows)
  }
}

#[allow(clippy::too_many_arguments)] // flat row columns, mapped in one place
fn stored_from_parts(
  id: i64,
  client_message_id: &[u8],
  conversation_id: i64,
  sender_user_id: i64,
  sender_device_id: i64,
  thread_id: Option<i64>,
  mentioned_user_ids: &[i64],
  is_urgent: bool,
  content_key_id: Vec<u8>,
  maybe_ciphertext: Option<Vec<u8>>,
) -> Result<StoredMessage, DbError> {
  Ok(StoredMessage {
    id: from_db(id)?,
    client_message_id: ClientMessageId::try_from(client_message_id)
      .map_err(|e| DbError::InvalidStoredValue(e.to_string()))?,
    conversation_id: from_db(conversation_id)?,
    sender_user_id: from_db(sender_user_id)?,
    sender_device_id: from_db(sender_device_id)?,
    maybe_thread_id: from_db_opt(thread_id)?,
    mentioned_user_ids: from_db_all(mentioned_user_ids)?,
    is_urgent,
    content_key_id,
    // Oversized ciphertext lives in S3 (ARCHITECTURE.md §4.2); not implemented yet, so it is empty here.
    ciphertext: maybe_ciphertext.unwrap_or_default(),
  })
}
