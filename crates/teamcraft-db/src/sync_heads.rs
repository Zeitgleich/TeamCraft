//! Where a user's conversations stand, read in one go for a client coming back (issue #714,
//! `GET /sync/heads`): the newest message and MLS entry the user can read per conversation, and
//! every member of those conversations for the member list digests.

use teamcraft_proto::{ChannelRole, ConversationId, MessageId, MlsMessageId, UserId};

use crate::conversation::ConversationMember;
use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, from_db_opt, to_db};
use crate::user::User;

/// The newest entries of one conversation the user is a member of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationHeadRow {
  pub conversation: ConversationId,
  pub maybe_last_message_id: Option<MessageId>,
  pub maybe_last_mls_message_id: Option<MlsMessageId>,
}

impl Database {
  /// One row per conversation `user` is a member of, ordered by conversation. The bounds are those
  /// of the history and MLS backlog reads: messages from the member's join on unless the history
  /// is `full` and the user no guest, MLS entries from the join on, welcomes only to the user's
  /// devices. Each maximum is one backward index probe (`messages_conversation_idx`,
  /// `mls_messages_conversation_id_idx`), so the cost grows with the conversations, not with
  /// their history.
  pub async fn list_conversation_heads(
    &self,
    user: UserId,
    is_guest: bool,
  ) -> Result<Vec<ConversationHeadRow>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT m.conversation_id,
                (SELECT max(x.id) FROM messages x
                  WHERE x.conversation_id = m.conversation_id
                    AND x.id >= CASE WHEN c.history_policy = 'full' AND NOT $2 THEN 0
                                     ELSE tc_snowflake_floor(m.joined_at) END) AS last_message_id,
                (SELECT max(y.id) FROM mls_messages y
                  WHERE y.conversation_id = m.conversation_id
                    AND y.id >= tc_snowflake_floor(m.joined_at)
                    AND (y.kind <> 'welcome' OR EXISTS (
                          SELECT 1 FROM devices d WHERE d.id = y.recipient_device_id AND d.user_id = $1))
                ) AS last_mls_message_id
         FROM memberships m JOIN conversations c ON c.id = m.conversation_id
         WHERE m.user_id = $1
         ORDER BY m.conversation_id"#,
      to_db(user),
      is_guest,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(ConversationHeadRow {
          conversation: from_db(row.conversation_id)?,
          maybe_last_message_id: from_db_opt(row.last_message_id)?,
          maybe_last_mls_message_id: from_db_opt(row.last_mls_message_id)?,
        })
      })
      .collect()
  }

  /// The members of every conversation `user` is a member of, grouped by conversation and in the
  /// order [`Database::list_conversation_members`] lists them: one query instead of one per
  /// conversation.
  pub async fn list_members_of_conversations_of(
    &self,
    user: UserId,
  ) -> Result<Vec<(ConversationId, ConversationMember)>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT m.conversation_id, u.id, u.username, u.display_name, u.kind, u.created_at, u.organization,
                u.deactivated_at IS NOT NULL AS "is_deactivated!", m.role
         FROM memberships mine
         JOIN memberships m ON m.conversation_id = mine.conversation_id
         JOIN users u ON u.id = m.user_id
         WHERE mine.user_id = $1
         ORDER BY m.conversation_id, u.display_name, u.id"#,
      to_db(user),
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok((
          from_db(row.conversation_id)?,
          ConversationMember {
            user: User {
              id: from_db(row.id)?,
              maybe_username: row.username,
              display_name: row.display_name,
              kind: row.kind.into(),
              created_at: row.created_at,
              is_deactivated: row.is_deactivated,
              maybe_organization: row.organization,
            },
            role: ChannelRole::from(row.role),
          },
        ))
      })
      .collect()
  }
}
