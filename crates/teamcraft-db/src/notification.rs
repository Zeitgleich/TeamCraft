//! Notification preferences: per user and per conversation (PLAN.md §5).

use teamcraft_proto::{ConversationId, MessageId, NotificationLevel, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, from_db_opt, to_db};

/// The user-level preferences as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredUserSettings {
  pub auto_follow_mentioned_threads: bool,
  /// `away_only` or `always`.
  pub push_mode: String,
  /// `None`: the instance default applies.
  pub maybe_send_read_receipts: Option<bool>,
  /// The DND schedule as JSON text; the database does not interpret it.
  pub maybe_dnd_schedule: Option<String>,
}

impl Default for StoredUserSettings {
  fn default() -> Self {
    Self {
      auto_follow_mentioned_threads: true,
      push_mode: "away_only".into(),
      maybe_send_read_receipts: None,
      maybe_dnd_schedule: None,
    }
  }
}

/// A member's read position in a conversation where they send read receipts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceiptRow {
  pub user: UserId,
  pub maybe_last_read: Option<MessageId>,
}

impl Database {
  /// The user's preferences, or the defaults if they never changed any.
  pub async fn get_user_notification_settings(&self, user: UserId) -> Result<StoredUserSettings, DbError> {
    let row = sqlx::query!(
      r#"SELECT u.auto_follow_mentioned_threads,
                COALESCE(s.push_mode, 'away_only') AS "push_mode!", s.send_read_receipts, s.dnd_schedule
         FROM users u LEFT JOIN user_notification_settings s ON s.user_id = u.id
         WHERE u.id = $1"#,
      to_db(user),
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(
      row.map_or_else(StoredUserSettings::default, |row| StoredUserSettings {
        auto_follow_mentioned_threads: row.auto_follow_mentioned_threads,
        push_mode: row.push_mode,
        maybe_send_read_receipts: row.send_read_receipts,
        maybe_dnd_schedule: row.dnd_schedule,
      }),
    )
  }

  /// Replaces all of the user's preferences.
  pub async fn set_user_notification_settings(
    &self,
    user: UserId,
    settings: &StoredUserSettings,
  ) -> Result<(), DbError> {
    let mut tx = self.pool().begin().await?;
    sqlx::query!(
      "UPDATE users SET auto_follow_mentioned_threads = $2 WHERE id = $1",
      to_db(user),
      settings.auto_follow_mentioned_threads,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
      r#"INSERT INTO user_notification_settings (user_id, push_mode, send_read_receipts, dnd_schedule)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (user_id) DO UPDATE SET push_mode = EXCLUDED.push_mode,
           send_read_receipts = EXCLUDED.send_read_receipts, dnd_schedule = EXCLUDED.dnd_schedule,
           updated_at = now()"#,
      to_db(user),
      settings.push_mode,
      settings.maybe_send_read_receipts,
      settings.maybe_dnd_schedule,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
  }

  /// Sets the notification level and mute time of one membership.
  pub async fn set_conversation_notifications(
    &self,
    conversation: ConversationId,
    user: UserId,
    level: &NotificationLevel,
    maybe_muted_until: Option<OffsetDateTime>,
  ) -> Result<(), DbError> {
    sqlx::query!(
      r#"UPDATE memberships SET notification_level = $3, muted_until = $4
         WHERE conversation_id = $1 AND user_id = $2"#,
      to_db(conversation),
      to_db(user),
      level.as_str(),
      maybe_muted_until,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Whether the user's read position in the conversation is at or past the message.
  pub async fn has_read_message(
    &self,
    user: UserId,
    conversation: ConversationId,
    message: MessageId,
  ) -> Result<bool, DbError> {
    let row = sqlx::query!(
      r#"SELECT COALESCE(last_read_message_id >= $3, false) AS "has_read!"
         FROM memberships WHERE conversation_id = $1 AND user_id = $2"#,
      to_db(conversation),
      to_db(user),
      to_db(message),
    )
    .fetch_optional(self.pool())
    .await?;
    // Someone who left the conversation has nothing to read.
    Ok(row.is_none_or(|row| row.has_read))
  }

  /// Members of the conversation who send (and therefore may see) read receipts, with their read
  /// positions. `default_is_enabled` applies to members who never chose.
  pub async fn list_receipt_members(
    &self,
    conversation: ConversationId,
    default_is_enabled: bool,
  ) -> Result<Vec<ReceiptRow>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT m.user_id, m.last_read_message_id
         FROM memberships m
         LEFT JOIN user_notification_settings s ON s.user_id = m.user_id
         WHERE m.conversation_id = $1 AND COALESCE(s.send_read_receipts, $2)
         ORDER BY m.user_id"#,
      to_db(conversation),
      default_is_enabled,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(ReceiptRow {
          user: from_db(row.user_id)?,
          maybe_last_read: from_db_opt(row.last_read_message_id)?,
        })
      })
      .collect()
  }
}
