use teamcraft_proto::{ConversationId, DeviceId, NotificationLevel, PushProvider, ThreadId, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};

/// A registered push device of a conversation member.
#[derive(Debug, Clone)]
pub struct PushDeviceRow {
  pub device: DeviceId,
  pub provider: PushProvider,
  pub token: String,
  pub push_key: Vec<u8>,
}

/// A conversation member who has at least one push device, with what routing needs to know.
#[derive(Debug, Clone)]
pub struct PushRecipientRow {
  pub user: UserId,
  /// The level of the conversation, else of its space, else the user's own default; `None`: the
  /// user never chose any of these, the instance default applies.
  pub maybe_notification_level: Option<NotificationLevel>,
  pub presence_pref: String,
  pub maybe_dnd_until: Option<OffsetDateTime>,
  pub maybe_muted_until: Option<OffsetDateTime>,
  /// `away_only` or `always`.
  pub push_mode: String,
  /// The user's DND schedule as stored (JSON text, parsed by the server).
  pub maybe_dnd_schedule: Option<String>,
  /// The user follows the thread of the message being routed.
  pub is_following_thread: bool,
  /// The user lets desktop attention hold phone pushes back (`None`: never chose, so yes).
  pub maybe_uses_desktop_attention: Option<bool>,
  pub devices: Vec<PushDeviceRow>,
}

impl Database {
  /// Members of the conversation (except `exclude`) that registered push devices. With
  /// `maybe_thread` set, rows say whether the member follows that thread.
  pub async fn list_push_recipients(
    &self,
    conversation: ConversationId,
    exclude: UserId,
    maybe_thread: Option<ThreadId>,
  ) -> Result<Vec<PushRecipientRow>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT m.user_id,
                -- A conversation the member never configured follows their setting for its space,
                -- else their own default level (NULL: the instance default).
                CASE WHEN m.notification_overridden THEN m.notification_level
                     ELSE COALESCE(sm.notification_level, s.notification_level) END AS "notification_level?",
                CASE WHEN m.notification_overridden THEN m.muted_until
                     ELSE GREATEST(m.muted_until, sm.muted_until) END AS muted_until,
                u.presence_pref, u.dnd_until,
                COALESCE(s.push_mode, 'away_only') AS "push_mode!", s.dnd_schedule, s.uses_desktop_attention,
                tf.user_id IS NOT NULL AS "is_following_thread!",
                d.id AS device_id, d.push_provider AS "push_provider!", d.push_token AS "push_token!",
                d.push_key AS "push_key!"
         FROM memberships m
         JOIN conversations c ON c.id = m.conversation_id
         LEFT JOIN space_members sm ON sm.space_id = c.space_id AND sm.user_id = m.user_id
         JOIN users u ON u.id = m.user_id AND u.deactivated_at IS NULL
         JOIN devices d ON d.user_id = u.id AND d.revoked_at IS NULL AND d.push_provider IS NOT NULL
         LEFT JOIN user_notification_settings s ON s.user_id = u.id
         LEFT JOIN threads_followed tf ON tf.user_id = u.id AND tf.conversation_id = m.conversation_id
                                      AND tf.thread_id = $3 AND tf.state = 'following'
         WHERE m.conversation_id = $1 AND m.user_id <> $2
         ORDER BY m.user_id, d.id"#,
      to_db(conversation),
      to_db(exclude),
      maybe_thread.map(to_db),
    )
    .fetch_all(self.pool())
    .await?;

    let mut recipients: Vec<PushRecipientRow> = Vec::new();
    for row in rows {
      let user: UserId = from_db(row.user_id)?;
      let device = PushDeviceRow {
        device: from_db(row.device_id)?,
        provider: PushProvider::from(row.push_provider),
        token: row.push_token,
        push_key: row.push_key,
      };
      match recipients.last_mut() {
        Some(last) if last.user == user => last.devices.push(device),
        _ => recipients.push(PushRecipientRow {
          user,
          maybe_notification_level: row.notification_level.map(NotificationLevel::from),
          presence_pref: row.presence_pref,
          maybe_dnd_until: row.dnd_until,
          maybe_muted_until: row.muted_until,
          push_mode: row.push_mode,
          maybe_dnd_schedule: row.dnd_schedule,
          is_following_thread: row.is_following_thread,
          maybe_uses_desktop_attention: row.uses_desktop_attention,
          devices: vec![device],
        }),
      }
    }
    Ok(recipients)
  }

  /// The user's devices with a push registration (not revoked), by device ID. None while the
  /// account is deactivated: the registrations are kept for a reactivation, but not used.
  pub async fn list_push_devices(&self, user: UserId) -> Result<Vec<PushDeviceRow>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT d.id, d.push_provider AS "push_provider!", d.push_token AS "push_token!",
                d.push_key AS "push_key!"
         FROM devices d JOIN users u ON u.id = d.user_id AND u.deactivated_at IS NULL
         WHERE d.user_id = $1 AND d.revoked_at IS NULL AND d.push_provider IS NOT NULL
           AND d.push_token IS NOT NULL AND d.push_key IS NOT NULL
         ORDER BY d.id"#,
      to_db(user),
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(PushDeviceRow {
          device: from_db(row.id)?,
          provider: PushProvider::from(row.push_provider),
          token: row.push_token,
          push_key: row.push_key,
        })
      })
      .collect()
  }

  /// What the app icon badge of the user shows (notifications.md, "Badge"): conversations with an
  /// unread mention or an unread message in a DM or group DM, plus followed threads with an unread
  /// mention. Each conversation and thread counts once, however much is unread in it.
  pub async fn push_badge_count(&self, user: UserId) -> Result<u32, DbError> {
    let row = sqlx::query!(
      r#"SELECT
           (SELECT COUNT(*) FROM memberships ms
            JOIN conversations c ON c.id = ms.conversation_id
            WHERE ms.user_id = $1
              AND EXISTS (
                SELECT 1 FROM messages m
                WHERE m.conversation_id = ms.conversation_id AND m.thread_id IS NULL
                  AND m.sender_user_id <> ms.user_id
                  AND m.id > COALESCE(ms.last_read_message_id, 0)
                  AND m.id >= tc_snowflake_floor(ms.joined_at)
                  AND (c.kind IN ('dm', 'group') OR ms.user_id = ANY(m.mentioned_user_ids))))
         + (SELECT COUNT(*) FROM threads_followed tf
            WHERE tf.user_id = $1 AND tf.state = 'following'
              AND EXISTS (
                SELECT 1 FROM messages m
                WHERE m.conversation_id = tf.conversation_id AND m.thread_id = tf.thread_id
                  AND m.sender_user_id <> tf.user_id
                  AND m.id > COALESCE(tf.last_read_message_id, 0)
                  AND tf.user_id = ANY(m.mentioned_user_ids)))
           AS "badge!""#,
      to_db(user),
    )
    .fetch_one(self.pool())
    .await?;
    Ok(u32::try_from(row.badge).unwrap_or(u32::MAX))
  }
}
