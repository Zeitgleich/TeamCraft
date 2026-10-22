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
  pub notification_level: NotificationLevel,
  pub presence_pref: String,
  pub maybe_dnd_until: Option<OffsetDateTime>,
  pub maybe_muted_until: Option<OffsetDateTime>,
  /// `away_only` or `always`.
  pub push_mode: String,
  /// The user's DND schedule as stored (JSON text, parsed by the server).
  pub maybe_dnd_schedule: Option<String>,
  /// The user follows the thread of the message being routed.
  pub is_following_thread: bool,
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
                -- A conversation the member never configured follows their setting for its space.
                CASE WHEN m.notification_overridden OR sm.notification_level IS NULL
                     THEN m.notification_level ELSE sm.notification_level END AS "notification_level!",
                CASE WHEN m.notification_overridden THEN m.muted_until
                     ELSE GREATEST(m.muted_until, sm.muted_until) END AS muted_until,
                u.presence_pref, u.dnd_until,
                COALESCE(s.push_mode, 'away_only') AS "push_mode!", s.dnd_schedule,
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
          notification_level: NotificationLevel::from(row.notification_level),
          presence_pref: row.presence_pref,
          maybe_dnd_until: row.dnd_until,
          maybe_muted_until: row.muted_until,
          push_mode: row.push_mode,
          maybe_dnd_schedule: row.dnd_schedule,
          is_following_thread: row.is_following_thread,
          devices: vec![device],
        }),
      }
    }
    Ok(recipients)
  }
}
