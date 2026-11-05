//! Messages the server holds until their time (docs/scheduled-messages.md). A release deletes the row
//! and stores the message in one transaction, under a shared lock on the group's epoch: a cancel, an
//! edit or a commit either comes first and stops it, or comes after it.

use std::time::Duration;

use teamcraft_proto::{
  ClientMessageId, ConversationId, DeviceId, ScheduledHoldReason, ScheduledMessageId, ThreadId, UserId,
};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, from_db_all, from_db_opt, to_db, to_db_all};
use crate::message::{NewMessage, StoreOutcome, store_message_in};

/// A held message as stored.
#[derive(Debug, Clone)]
pub struct StoredScheduledMessage {
  pub id: ScheduledMessageId,
  pub user_id: UserId,
  pub conversation_id: ConversationId,
  pub client_message_id: ClientMessageId,
  pub sender_device_id: DeviceId,
  pub maybe_thread_id: Option<ThreadId>,
  pub mentioned_user_ids: Vec<UserId>,
  pub is_urgent: bool,
  pub content_key_id: Vec<u8>,
  pub ciphertext: Vec<u8>,
  pub mls_epoch: u64,
  pub due_at: OffsetDateTime,
  pub version: u64,
  pub maybe_hold: Option<ScheduledHoldReason>,
  /// Whether the conversation is still in `mls_epoch`, when this was read.
  pub is_epoch_current: bool,
}

/// What a device seals: the same for a new message and a change of its text.
#[derive(Debug, Clone, Copy)]
pub struct SealedParts<'a> {
  pub sender_device_id: DeviceId,
  pub mentioned_user_ids: &'a [UserId],
  pub is_urgent: bool,
  pub content_key_id: &'a [u8],
  pub ciphertext: &'a [u8],
  pub mls_epoch: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct NewScheduledMessage<'a> {
  pub id: ScheduledMessageId,
  pub user_id: UserId,
  pub conversation_id: ConversationId,
  pub client_message_id: ClientMessageId,
  pub maybe_thread_id: Option<ThreadId>,
  pub sealed: SealedParts<'a>,
  pub due_at: OffsetDateTime,
}

#[derive(Debug)]
pub enum ScheduleOutcome {
  Created(StoredScheduledMessage),
  /// The same client message ID again (a retry after a lost answer): what is stored.
  Existing(StoredScheduledMessage),
  /// The user holds `max_per_user` already.
  LimitReached,
}

#[derive(Debug)]
pub enum ScheduledUpdateOutcome {
  Updated(StoredScheduledMessage),
  VersionConflict { current_version: u64 },
  NotFound,
}

#[derive(Debug)]
pub enum ReleaseOutcome {
  /// Gone from the held messages and stored as a message.
  Released(StoreOutcome),
  /// The group left the entry's epoch: nothing was stored and the entry stays.
  StaleEpoch { current_epoch: u64 },
  /// Cancelled or changed since it was read: nothing happened.
  Changed,
}

impl Database {
  /// Stores a message to hold. The sender's row is locked for the count, so racing requests can't
  /// pass the limit together.
  pub async fn create_scheduled_message(
    &self,
    new: &NewScheduledMessage<'_>,
    max_per_user: u32,
  ) -> Result<ScheduleOutcome, DbError> {
    let mut tx = self.pool().begin().await?;
    sqlx::query_scalar!(
      "SELECT id FROM users WHERE id = $1 FOR NO KEY UPDATE",
      to_db(new.user_id)
    )
    .fetch_one(&mut *tx)
    .await?;
    let maybe_existing = sqlx::query_scalar!(
      "SELECT id FROM scheduled_messages WHERE user_id = $1 AND conversation_id = $2 AND client_message_id = $3",
      to_db(new.user_id),
      to_db(new.conversation_id),
      new.client_message_id.as_bytes().as_slice(),
    )
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(existing) = maybe_existing {
      tx.commit().await?;
      let found = self
        .get_scheduled_message(from_db(existing)?, new.user_id)
        .await?
        .ok_or_else(|| DbError::InvalidStoredValue("scheduled message went away".into()))?;
      return Ok(ScheduleOutcome::Existing(found));
    }
    let held = sqlx::query_scalar!(
      r#"SELECT count(*) AS "count!" FROM scheduled_messages WHERE user_id = $1"#,
      to_db(new.user_id)
    )
    .fetch_one(&mut *tx)
    .await?;
    if held >= i64::from(max_per_user) {
      return Ok(ScheduleOutcome::LimitReached);
    }
    sqlx::query!(
      r#"INSERT INTO scheduled_messages
           (id, user_id, conversation_id, client_message_id, sender_device_id, thread_id, mentioned_user_ids,
            is_urgent, content_key_id, ciphertext, mls_epoch, due_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)"#,
      to_db(new.id),
      to_db(new.user_id),
      to_db(new.conversation_id),
      new.client_message_id.as_bytes().as_slice(),
      to_db(new.sealed.sender_device_id),
      new.maybe_thread_id.map(to_db),
      &to_db_all(new.sealed.mentioned_user_ids),
      new.sealed.is_urgent,
      new.sealed.content_key_id,
      new.sealed.ciphertext,
      epoch_to_db(new.sealed.mls_epoch)?,
      new.due_at,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let created = self
      .get_scheduled_message(new.id, new.user_id)
      .await?
      .ok_or_else(|| DbError::InvalidStoredValue("scheduled message went away".into()))?;
    Ok(ScheduleOutcome::Created(created))
  }

  /// The user's held messages, soonest first. Only theirs: every read names the owner.
  pub async fn list_scheduled_messages(&self, user: UserId) -> Result<Vec<StoredScheduledMessage>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT s.id, s.user_id, s.conversation_id, s.client_message_id, s.sender_device_id, s.thread_id,
                s.mentioned_user_ids, s.is_urgent, s.content_key_id, s.ciphertext, s.mls_epoch, s.due_at,
                s.version, s.hold_reason, COALESCE(h.epoch, 0) = s.mls_epoch AS "is_epoch_current!"
         FROM scheduled_messages s
         LEFT JOIN mls_epoch_heads h ON h.conversation_id = s.conversation_id
         WHERE s.user_id = $1
         ORDER BY s.due_at, s.id"#,
      to_db(user),
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        from_parts(Parts {
          id: row.id,
          user_id: row.user_id,
          conversation_id: row.conversation_id,
          client_message_id: &row.client_message_id,
          sender_device_id: row.sender_device_id,
          thread_id: row.thread_id,
          mentioned_user_ids: &row.mentioned_user_ids,
          is_urgent: row.is_urgent,
          content_key_id: row.content_key_id,
          ciphertext: row.ciphertext,
          mls_epoch: row.mls_epoch,
          due_at: row.due_at,
          version: row.version,
          hold_reason: row.hold_reason,
          is_epoch_current: row.is_epoch_current,
        })
      })
      .collect()
  }

  /// One held message, if it is `user`'s.
  pub async fn get_scheduled_message(
    &self,
    id: ScheduledMessageId,
    user: UserId,
  ) -> Result<Option<StoredScheduledMessage>, DbError> {
    let maybe_row = sqlx::query!(
      r#"SELECT s.id, s.user_id, s.conversation_id, s.client_message_id, s.sender_device_id, s.thread_id,
                s.mentioned_user_ids, s.is_urgent, s.content_key_id, s.ciphertext, s.mls_epoch, s.due_at,
                s.version, s.hold_reason, COALESCE(h.epoch, 0) = s.mls_epoch AS "is_epoch_current!"
         FROM scheduled_messages s
         LEFT JOIN mls_epoch_heads h ON h.conversation_id = s.conversation_id
         WHERE s.id = $1 AND s.user_id = $2"#,
      to_db(id),
      to_db(user),
    )
    .fetch_optional(self.pool())
    .await?;
    maybe_row
      .map(|row| {
        from_parts(Parts {
          id: row.id,
          user_id: row.user_id,
          conversation_id: row.conversation_id,
          client_message_id: &row.client_message_id,
          sender_device_id: row.sender_device_id,
          thread_id: row.thread_id,
          mentioned_user_ids: &row.mentioned_user_ids,
          is_urgent: row.is_urgent,
          content_key_id: row.content_key_id,
          ciphertext: row.ciphertext,
          mls_epoch: row.mls_epoch,
          due_at: row.due_at,
          version: row.version,
          hold_reason: row.hold_reason,
          is_epoch_current: row.is_epoch_current,
        })
      })
      .transpose()
  }

  /// Changes the time, and with `maybe_sealed` the sealed text, if the row is still `expected_version`.
  /// It waits for its time again, whatever held it.
  pub async fn update_scheduled_message(
    &self,
    id: ScheduledMessageId,
    user: UserId,
    expected_version: u64,
    due_at: OffsetDateTime,
    maybe_sealed: Option<&SealedParts<'_>>,
  ) -> Result<ScheduledUpdateOutcome, DbError> {
    let expected = version_to_db(expected_version)?;
    let updated = match maybe_sealed {
      Some(sealed) => {
        sqlx::query!(
          r#"UPDATE scheduled_messages
             SET due_at = $4, sender_device_id = $5, mentioned_user_ids = $6, is_urgent = $7, content_key_id = $8,
                 ciphertext = $9, mls_epoch = $10, version = version + 1, hold_reason = NULL, claimed_until = NULL, last_wake_at = NULL
             WHERE id = $1 AND user_id = $2 AND version = $3"#,
          to_db(id),
          to_db(user),
          expected,
          due_at,
          to_db(sealed.sender_device_id),
          &to_db_all(sealed.mentioned_user_ids),
          sealed.is_urgent,
          sealed.content_key_id,
          sealed.ciphertext,
          epoch_to_db(sealed.mls_epoch)?,
        )
        .execute(self.pool())
        .await?
      },
      None => {
        sqlx::query!(
          r#"UPDATE scheduled_messages
             SET due_at = $4, version = version + 1, hold_reason = NULL, claimed_until = NULL, last_wake_at = NULL
             WHERE id = $1 AND user_id = $2 AND version = $3"#,
          to_db(id),
          to_db(user),
          expected,
          due_at,
        )
        .execute(self.pool())
        .await?
      },
    };
    if updated.rows_affected() == 1 {
      let stored = self
        .get_scheduled_message(id, user)
        .await?
        .ok_or_else(|| DbError::InvalidStoredValue("scheduled message went away".into()))?;
      return Ok(ScheduledUpdateOutcome::Updated(stored));
    }
    Ok(match self.get_scheduled_message(id, user).await? {
      Some(current) => ScheduledUpdateOutcome::VersionConflict {
        current_version: current.version,
      },
      None => ScheduledUpdateOutcome::NotFound,
    })
  }

  /// Cancels a held message; whether it was still there. Once it was released this finds nothing.
  pub async fn delete_scheduled_message(
    &self,
    id: ScheduledMessageId,
    user: UserId,
  ) -> Result<bool, DbError> {
    let deleted = sqlx::query!(
      "DELETE FROM scheduled_messages WHERE id = $1 AND user_id = $2",
      to_db(id),
      to_db(user),
    )
    .execute(self.pool())
    .await?;
    Ok(deleted.rows_affected() == 1)
  }

  /// Claims up to `limit` messages due at `now` for `lease`. Concurrent servers claim different rows;
  /// one that crashes leaves its rows to whoever claims after the lease.
  pub async fn claim_due_scheduled_messages(
    &self,
    now: OffsetDateTime,
    lease: Duration,
    limit: i64,
  ) -> Result<Vec<StoredScheduledMessage>, DbError> {
    let rows = sqlx::query!(
      r#"WITH claimed AS (
           UPDATE scheduled_messages s
           SET claimed_until = $1 + make_interval(secs => $2)
           WHERE s.id IN (
             SELECT id FROM scheduled_messages
             WHERE hold_reason IS NULL AND due_at <= $1 AND (claimed_until IS NULL OR claimed_until <= $1)
             ORDER BY due_at
             LIMIT $3
             FOR UPDATE SKIP LOCKED)
           RETURNING s.*)
         SELECT c.id, c.user_id, c.conversation_id, c.client_message_id, c.sender_device_id, c.thread_id,
                c.mentioned_user_ids, c.is_urgent, c.content_key_id, c.ciphertext, c.mls_epoch, c.due_at,
                c.version, c.hold_reason, COALESCE(h.epoch, 0) = c.mls_epoch AS "is_epoch_current!"
         FROM claimed c
         LEFT JOIN mls_epoch_heads h ON h.conversation_id = c.conversation_id
         ORDER BY c.due_at, c.id"#,
      now,
      lease.as_secs_f64(),
      limit,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        from_parts(Parts {
          id: row.id,
          user_id: row.user_id,
          conversation_id: row.conversation_id,
          client_message_id: &row.client_message_id,
          sender_device_id: row.sender_device_id,
          thread_id: row.thread_id,
          mentioned_user_ids: &row.mentioned_user_ids,
          is_urgent: row.is_urgent,
          content_key_id: row.content_key_id,
          ciphertext: row.ciphertext,
          mls_epoch: row.mls_epoch,
          due_at: row.due_at,
          version: row.version,
          hold_reason: row.hold_reason,
          is_epoch_current: row.is_epoch_current,
        })
      })
      .collect()
  }

  /// The people who hold a message in `conversation` that sealing again can help: asked after each
  /// commit, so one read on an index, nothing for a conversation without any.
  pub async fn users_waiting_in_conversation(
    &self,
    conversation: ConversationId,
  ) -> Result<Vec<UserId>, DbError> {
    let rows = sqlx::query_scalar!(
      r#"SELECT DISTINCT user_id FROM scheduled_messages
         WHERE conversation_id = $1 AND (hold_reason IS NULL OR hold_reason = 'stale_epoch')"#,
      to_db(conversation),
    )
    .fetch_all(self.pool())
    .await?;
    from_db_all(&rows)
  }

  /// Messages due within `window` of `now` that are still sealed for an old epoch and whose sender's
  /// devices weren't woken for this version yet: the last stretch before their time, for a wake that
  /// was lost or came to nothing. Marks them, so each version is asked about once.
  pub async fn claim_stale_due_soon(
    &self,
    now: OffsetDateTime,
    window: Duration,
    limit: i64,
  ) -> Result<Vec<(UserId, ConversationId)>, DbError> {
    let rows = sqlx::query!(
      r#"UPDATE scheduled_messages s
         SET last_wake_at = $1
         WHERE s.id IN (
           SELECT m.id FROM scheduled_messages m
           JOIN mls_epoch_heads h ON h.conversation_id = m.conversation_id
           WHERE m.hold_reason IS NULL AND m.last_wake_at IS NULL AND h.epoch <> m.mls_epoch
             AND m.due_at > $1 AND m.due_at <= $1 + make_interval(secs => $2)
           ORDER BY m.due_at
           LIMIT $3
           FOR UPDATE OF m SKIP LOCKED)
         RETURNING s.user_id, s.conversation_id"#,
      now,
      window.as_secs_f64(),
      limit,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| Ok((from_db(row.user_id)?, from_db(row.conversation_id)?)))
      .collect()
  }

  /// Notes why a message didn't go out at its time; nothing if it changed since it was read.
  pub async fn hold_scheduled_message(
    &self,
    id: ScheduledMessageId,
    version: u64,
    reason: &ScheduledHoldReason,
  ) -> Result<bool, DbError> {
    let held = sqlx::query!(
      r#"UPDATE scheduled_messages
         SET hold_reason = $3, claimed_until = NULL, version = version + 1
         WHERE id = $1 AND version = $2"#,
      to_db(id),
      version_to_db(version)?,
      reason.as_str(),
    )
    .execute(self.pool())
    .await?;
    Ok(held.rows_affected() == 1)
  }

  /// Turns a held message into a stored one, once: the row goes and the message is stored in one
  /// transaction, and only if the row is still `version` and the group still in `mls_epoch`. The
  /// epoch's row is locked against a commit until this is done. `message` carries the server's ID
  /// and the mentions as they are now.
  pub async fn release_scheduled_message(
    &self,
    id: ScheduledMessageId,
    version: u64,
    mls_epoch: u64,
    message: &NewMessage<'_>,
  ) -> Result<ReleaseOutcome, DbError> {
    let mut tx = self.pool().begin().await?;
    let maybe_epoch = sqlx::query_scalar!(
      "SELECT epoch FROM mls_epoch_heads WHERE conversation_id = $1 FOR SHARE",
      to_db(message.conversation_id),
    )
    .fetch_optional(&mut *tx)
    .await?;
    let current_epoch = maybe_epoch.map_or(0, |epoch| u64::try_from(epoch).unwrap_or(u64::MAX));
    let deleted = sqlx::query!(
      "DELETE FROM scheduled_messages WHERE id = $1 AND version = $2",
      to_db(id),
      version_to_db(version)?,
    )
    .execute(&mut *tx)
    .await?;
    if deleted.rows_affected() == 0 {
      return Ok(ReleaseOutcome::Changed);
    }
    if current_epoch != mls_epoch {
      // Dropping the transaction brings the row back.
      return Ok(ReleaseOutcome::StaleEpoch { current_epoch });
    }
    let outcome = store_message_in(&mut tx, message).await?;
    tx.commit().await?;
    let outcome = match outcome {
      Some(stored) => StoreOutcome {
        message: stored,
        is_new: true,
      },
      None => StoreOutcome {
        message: self
          .get_message_by_client_id(message.conversation_id, message.client_message_id)
          .await?
          .ok_or_else(|| DbError::InvalidStoredValue("client ID without message".into()))?,
        is_new: false,
      },
    };
    Ok(ReleaseOutcome::Released(outcome))
  }
}

struct Parts<'a> {
  id: i64,
  user_id: i64,
  conversation_id: i64,
  client_message_id: &'a [u8],
  sender_device_id: i64,
  thread_id: Option<i64>,
  mentioned_user_ids: &'a [i64],
  is_urgent: bool,
  content_key_id: Vec<u8>,
  ciphertext: Vec<u8>,
  mls_epoch: i64,
  due_at: OffsetDateTime,
  version: i64,
  hold_reason: Option<String>,
  is_epoch_current: bool,
}

fn from_parts(parts: Parts<'_>) -> Result<StoredScheduledMessage, DbError> {
  Ok(StoredScheduledMessage {
    id: from_db(parts.id)?,
    user_id: from_db(parts.user_id)?,
    conversation_id: from_db(parts.conversation_id)?,
    client_message_id: ClientMessageId::try_from(parts.client_message_id)
      .map_err(|error| DbError::InvalidStoredValue(error.to_string()))?,
    sender_device_id: from_db(parts.sender_device_id)?,
    maybe_thread_id: from_db_opt(parts.thread_id)?,
    mentioned_user_ids: from_db_all(parts.mentioned_user_ids)?,
    is_urgent: parts.is_urgent,
    content_key_id: parts.content_key_id,
    ciphertext: parts.ciphertext,
    mls_epoch: u64::try_from(parts.mls_epoch)
      .map_err(|_| DbError::InvalidStoredValue("negative MLS epoch".into()))?,
    due_at: parts.due_at,
    version: u64::try_from(parts.version)
      .map_err(|_| DbError::InvalidStoredValue("negative version".into()))?,
    maybe_hold: parts.hold_reason.map(ScheduledHoldReason::from),
    is_epoch_current: parts.is_epoch_current,
  })
}

fn epoch_to_db(epoch: u64) -> Result<i64, DbError> {
  i64::try_from(epoch).map_err(|_| DbError::InvalidStoredValue("MLS epoch out of range".into()))
}

fn version_to_db(version: u64) -> Result<i64, DbError> {
  i64::try_from(version).map_err(|_| DbError::InvalidStoredValue("version out of range".into()))
}
