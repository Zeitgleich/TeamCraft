//! Where a user's conversations stand, read in one go for a client coming back (issue #714,
//! `GET /sync/heads`): the newest message and MLS entry the user can read per conversation, and
//! the tokens of the member lists (migration 0075), which change whenever a list's answer would.

use teamcraft_proto::{ConversationId, MessageId, MlsMessageId, SpaceId, UserId};

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, from_db_opt, to_db};

/// The newest entries of one conversation the user is a member of, and its member list token
/// (`None`: unchanged since migration 0075).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationHeadRow {
  pub conversation: ConversationId,
  pub maybe_last_message_id: Option<MessageId>,
  pub maybe_last_mls_message_id: Option<MlsMessageId>,
  pub maybe_members_token: Option<String>,
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
                    AND x.id >= CASE WHEN c.history_policy = 'full' AND (NOT $2 OR m.sees_imported_history) THEN 0
                                     ELSE tc_snowflake_floor(m.joined_at) END) AS last_message_id,
                (SELECT max(y.id) FROM mls_messages y
                  WHERE y.conversation_id = m.conversation_id
                    AND y.id >= tc_snowflake_floor(m.joined_at)
                    AND (y.kind <> 'welcome' OR EXISTS (
                          SELECT 1 FROM devices d WHERE d.id = y.recipient_device_id AND d.user_id = $1))
                ) AS last_mls_message_id,
                t.token::text AS "members_token?"
         FROM memberships m JOIN conversations c ON c.id = m.conversation_id
         LEFT JOIN conversation_member_tokens t ON t.conversation_id = m.conversation_id
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
          maybe_members_token: row.members_token,
        })
      })
      .collect()
  }

  /// The member list tokens of `spaces` that have one (migration 0075).
  pub async fn list_space_member_tokens(
    &self,
    spaces: &[SpaceId],
  ) -> Result<Vec<(SpaceId, String)>, DbError> {
    let ids: Vec<i64> = spaces.iter().map(|space| to_db(*space)).collect();
    let rows = sqlx::query!(
      r#"SELECT space_id, token::text AS "token!" FROM space_member_tokens WHERE space_id = ANY($1)"#,
      &ids,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| Ok((from_db(row.space_id)?, row.token)))
      .collect()
  }
}
