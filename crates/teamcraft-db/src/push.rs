use teamcraft_proto::{ConversationId, DeviceId, NotificationLevel, PushProvider, UserId};
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
  pub devices: Vec<PushDeviceRow>,
}

impl Database {
  /// Members of the conversation (except `exclude`) that registered push devices.
  pub async fn list_push_recipients(
    &self,
    conversation: ConversationId,
    exclude: UserId,
  ) -> Result<Vec<PushRecipientRow>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT m.user_id, m.notification_level, u.presence_pref, u.dnd_until,
                d.id AS device_id, d.push_provider AS "push_provider!", d.push_token AS "push_token!",
                d.push_key AS "push_key!"
         FROM memberships m
         JOIN users u ON u.id = m.user_id AND u.deactivated_at IS NULL
         JOIN devices d ON d.user_id = u.id AND d.revoked_at IS NULL AND d.push_provider IS NOT NULL
         WHERE m.conversation_id = $1 AND m.user_id <> $2
         ORDER BY m.user_id, d.id"#,
      to_db(conversation),
      to_db(exclude),
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
          devices: vec![device],
        }),
      }
    }
    Ok(recipients)
  }
}
