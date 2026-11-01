//! Deleting a user (admin.md §3.2.1, issue #630): the account becomes a tombstone that only names
//! old messages, its key material and sessions go, and its direct messages are deleted.

use std::collections::BTreeMap;

use teamcraft_proto::{ConversationId, UserId};
use time::OffsetDateTime;

use crate::admin::ChatRemovalEffects;
use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db, to_db_all};
use crate::local_account::LOCAL_ISSUER;

/// The display name a tombstone keeps for clients that don't know `is_deleted` yet; newer clients
/// show their own translation.
pub const DELETED_USER_NAME: &str = "Deleted user";

/// Outcome of [`Database::delete_user`].
#[derive(Debug, Clone)]
pub enum UserDeletion {
  Deleted {
    /// The user was deleted before (a retry): nothing was left to change.
    was_already_deleted: bool,
    effects: ChatRemovalEffects,
  },
  UnknownUser,
}

/// A direct message or group DM that goes with a deleted user, with who else was in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectConversationToDelete {
  pub conversation: ConversationId,
  pub remaining: Vec<UserId>,
}

/// Owners and admins with an active account, for the "last admin" checks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ActiveAdminCounts {
  pub owners: i64,
  /// Admins and owners together.
  pub admins: i64,
}

impl Database {
  /// The user's 1:1 direct messages, and group DMs that would keep at most one other person: these
  /// are deleted with the user; group DMs with more people stay, like channels. A DM is the user's
  /// by its participants (issue #643), not by membership: a removal from all chats before the
  /// deletion took the membership but leaves the DM with them (issue #1066), and the import account
  /// is a member of DMs it is no participant of.
  pub async fn direct_conversations_deleted_with(
    &self,
    user: UserId,
  ) -> Result<Vec<DirectConversationToDelete>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT c.id AS "id!", c.kind AS "kind!", o.user_id AS "other?"
         FROM conversations c
         LEFT JOIN memberships o ON o.conversation_id = c.id AND o.user_id <> $1
         WHERE c.kind IN ('dm', 'group')
           AND ($1 = ANY(string_to_array(c.participants_key, '-')::bigint[])
             OR (c.kind = 'group'
                 AND EXISTS (SELECT 1 FROM memberships m WHERE m.conversation_id = c.id AND m.user_id = $1)))
         ORDER BY c.id"#,
      to_db(user),
    )
    .fetch_all(self.pool())
    .await?;
    let mut by_conversation: BTreeMap<i64, (String, Vec<UserId>)> = BTreeMap::new();
    for row in rows {
      let entry = by_conversation
        .entry(row.id)
        .or_insert_with(|| (row.kind.clone(), Vec::new()));
      if let Some(other) = row.other {
        entry.1.push(from_db(other)?);
      }
    }
    by_conversation
      .into_iter()
      .filter(|(_, (kind, remaining))| kind == "dm" || remaining.len() <= 1)
      .map(|(id, (_, remaining))| {
        Ok(DirectConversationToDelete {
          conversation: from_db(id)?,
          remaining,
        })
      })
      .collect()
  }

  /// Deleted users who are still a participant of a DM, at most `limit`: their DMs were left
  /// behind by a deletion that missed them (issue #1066) and go with the next housekeeping.
  pub async fn deleted_users_with_direct_conversations(&self, limit: i64) -> Result<Vec<UserId>, DbError> {
    let ids = sqlx::query_scalar!(
      r#"SELECT u.id FROM users u
         WHERE u.deleted_at IS NOT NULL
           AND EXISTS (SELECT 1 FROM conversations c
                       WHERE c.kind = 'dm'
                         AND u.id = ANY(string_to_array(c.participants_key, '-')::bigint[]))
         ORDER BY u.id LIMIT $1"#,
      limit,
    )
    .fetch_all(self.pool())
    .await?;
    ids.into_iter().map(from_db).collect()
  }

  /// Every object in storage that belongs to the conversation: attachments, message ciphertext
  /// kept in storage and search index segments.
  pub async fn conversation_object_keys(&self, conversation: ConversationId) -> Result<Vec<String>, DbError> {
    let keys = sqlx::query_scalar!(
      r#"SELECT s3_key AS "key!" FROM attachments WHERE conversation_id = $1
         UNION ALL
         SELECT ciphertext_s3_key AS "key!" FROM messages
           WHERE conversation_id = $1 AND ciphertext_s3_key IS NOT NULL
         UNION ALL
         SELECT s3_key AS "key!" FROM index_segments WHERE conversation_id = $1"#,
      to_db(conversation),
    )
    .fetch_all(self.pool())
    .await?;
    Ok(keys)
  }

  /// Deletes a direct conversation with everything in it (messages, MLS state, attachments) and
  /// records it for the remaining participants, whose devices drop it on their next sync. Its
  /// objects in storage are deleted before ([`Self::conversation_object_keys`]). Idempotent.
  pub async fn delete_direct_conversation(
    &self,
    conversation: ConversationId,
    remaining: &[UserId],
  ) -> Result<(), DbError> {
    let mut tx = self.pool().begin().await?;
    sqlx::query!(
      r#"INSERT INTO deleted_conversations (user_id, conversation_id)
         SELECT unnest($1::bigint[]), $2 ON CONFLICT DO NOTHING"#,
      &to_db_all(remaining),
      to_db(conversation),
    )
    .execute(&mut *tx)
    .await?;
    // Attachment rows would only lose their conversation (ON DELETE SET NULL).
    sqlx::query!(
      "DELETE FROM attachments WHERE conversation_id = $1",
      to_db(conversation)
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
      "DELETE FROM conversations WHERE id = $1 AND kind IN ('dm', 'group')",
      to_db(conversation)
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
  }

  /// Direct conversations deleted with another user that this user's devices should drop.
  pub async fn deleted_conversations_of(&self, user: UserId) -> Result<Vec<ConversationId>, DbError> {
    let ids = sqlx::query_scalar!(
      "SELECT conversation_id FROM deleted_conversations WHERE user_id = $1 ORDER BY conversation_id",
      to_db(user),
    )
    .fetch_all(self.pool())
    .await?;
    ids.into_iter().map(from_db).collect()
  }

  /// Forgets deleted conversations recorded before `before`. Returns how many rows went.
  pub async fn prune_deleted_conversations(&self, before: OffsetDateTime) -> Result<u64, DbError> {
    let result = sqlx::query!("DELETE FROM deleted_conversations WHERE deleted_at < $1", before)
      .execute(self.pool())
      .await?;
    Ok(result.rows_affected())
  }

  /// The user's own objects in storage that go with the account: avatars and call backgrounds.
  pub async fn user_object_keys(&self, user: UserId) -> Result<Vec<String>, DbError> {
    let keys = sqlx::query_scalar!(
      r#"SELECT avatar_key AS "key!" FROM user_profiles WHERE user_id = $1 AND avatar_key IS NOT NULL
         UNION ALL
         SELECT pending_avatar_key AS "key!" FROM user_profiles
           WHERE user_id = $1 AND pending_avatar_key IS NOT NULL
         UNION ALL
         SELECT s3_key AS "key!" FROM user_backgrounds WHERE user_id = $1"#,
      to_db(user),
    )
    .fetch_all(self.pool())
    .await?;
    Ok(keys)
  }

  /// When a deletion released this identity, if one did: tokens from a sign-in up to then are
  /// refused.
  pub async fn identity_released_at(
    &self,
    issuer: &str,
    subject: &str,
  ) -> Result<Option<OffsetDateTime>, DbError> {
    let released = sqlx::query_scalar!(
      "SELECT released_at FROM released_identities WHERE issuer = $1 AND subject = $2",
      issuer,
      subject,
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(released)
  }

  /// Owners and admins whose accounts are active (by the role the identity provider gave them).
  pub async fn active_admin_counts(&self) -> Result<ActiveAdminCounts, DbError> {
    let row = sqlx::query!(
      r#"SELECT count(*) FILTER (WHERE oidc_role = 'owner') AS "owners!",
                count(*) FILTER (WHERE oidc_role IN ('owner', 'admin')) AS "admins!"
         FROM users WHERE deactivated_at IS NULL"#,
    )
    .fetch_one(self.pool())
    .await?;
    Ok(ActiveAdminCounts {
      owners: row.owners,
      admins: row.admins,
    })
  }

  /// Turns the user into a tombstone: deactivated and marked deleted, without username, e-mail,
  /// role or profile, named [`DELETED_USER_NAME`]. Devices are revoked (push registrations and key
  /// packages go), the identity key, key bag, local account, sessions and recovery shares are
  /// removed, and the user leaves every conversation, space and user group. The OIDC identity is
  /// released: the issuer gets a `deleted:{id}:` prefix, so the next sign-in with it creates a new
  /// account. The user's objects in storage are deleted before ([`Self::user_object_keys`]).
  /// Idempotent: a retry finds nothing left and reports `was_already_deleted`.
  pub async fn delete_user(&self, user: UserId) -> Result<UserDeletion, DbError> {
    let id = to_db(user);
    let mut tx = self.pool().begin().await?;
    // Locks the row so a concurrent reactivation or deactivation waits.
    let maybe_deleted = sqlx::query_scalar!(
      r#"SELECT deleted_at IS NOT NULL AS "is_deleted!" FROM users WHERE id = $1 FOR UPDATE"#,
      id,
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(was_already_deleted) = maybe_deleted else {
      return Ok(UserDeletion::UnknownUser);
    };

    let devices = sqlx::query_scalar!(
      "UPDATE devices SET revoked_at = now(), push_provider = NULL, push_token = NULL, push_key = NULL
       WHERE user_id = $1 AND revoked_at IS NULL RETURNING id",
      id,
    )
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query!(
      "DELETE FROM key_packages WHERE device_id IN (SELECT id FROM devices WHERE user_id = $1)",
      id,
    )
    .execute(&mut *tx)
    .await?;
    let conversation_rows = sqlx::query!(
      r#"SELECT m.conversation_id AS "conversation_id!", o.user_id AS "other?" FROM memberships m
         LEFT JOIN memberships o ON o.conversation_id = m.conversation_id AND o.user_id <> m.user_id
         WHERE m.user_id = $1"#,
      id,
    )
    .fetch_all(&mut *tx)
    .await?;
    let space_rows = sqlx::query!(
      r#"SELECT m.space_id AS "space_id!", o.user_id AS "other?" FROM space_members m
         LEFT JOIN space_members o ON o.space_id = m.space_id AND o.user_id <> m.user_id
         WHERE m.user_id = $1"#,
      id,
    )
    .fetch_all(&mut *tx)
    .await?;
    // Sessions are keyed by the identity the user signs in with, so they go before it changes.
    sqlx::query!(
      r#"DELETE FROM session_activity s USING users u
         WHERE u.id = $1
           AND ((s.issuer = u.oidc_issuer AND s.subject = u.oidc_subject)
             OR (s.issuer = $2 AND s.subject = $1::bigint::text))"#,
      id,
      LOCAL_ISSUER,
    )
    .execute(&mut *tx)
    .await?;
    // Released before it changes; the first deletion's time counts.
    sqlx::query!(
      r#"INSERT INTO released_identities (issuer, subject)
         SELECT oidc_issuer, oidc_subject FROM users
         WHERE id = $1 AND deleted_at IS NULL AND oidc_issuer IS NOT NULL AND oidc_subject IS NOT NULL
         ON CONFLICT (issuer, subject) DO UPDATE SET released_at = EXCLUDED.released_at"#,
      id,
    )
    .execute(&mut *tx)
    .await?;
    delete_user_rows(&mut tx, id).await?;
    sqlx::query!(
      r#"UPDATE users SET
           deleted_at = COALESCE(deleted_at, now()),
           deactivated_at = COALESCE(deactivated_at, now()),
           deactivation_reason = COALESCE(deactivation_reason, 'admin'),
           username = NULL, email = NULL, display_name = $2, oidc_role = NULL, organization = NULL,
           last_seen_at = NULL, dnd_until = NULL, presence_pref = 'auto',
           oidc_issuer = CASE WHEN deleted_at IS NULL AND oidc_issuer IS NOT NULL
                              THEN 'deleted:' || id::text || ':' || oidc_issuer
                              ELSE oidc_issuer END
         WHERE id = $1"#,
      id,
      DELETED_USER_NAME,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(UserDeletion::Deleted {
      was_already_deleted,
      effects: removal_effects(
        devices,
        conversation_rows
          .into_iter()
          .map(|row| (row.conversation_id, row.other))
          .collect(),
        space_rows
          .into_iter()
          .map(|row| (row.space_id, row.other))
          .collect(),
      )?,
    })
  }
}

/// Everything else that belongs to the user alone: key material, memberships, settings, profile.
async fn delete_user_rows(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, id: i64) -> Result<(), DbError> {
  sqlx::query!("DELETE FROM identity_keys WHERE user_id = $1", id)
    .execute(&mut **tx)
    .await?;
  sqlx::query!("DELETE FROM key_bags WHERE user_id = $1", id)
    .execute(&mut **tx)
    .await?;
  sqlx::query!("DELETE FROM push_retries WHERE user_id = $1", id)
    .execute(&mut **tx)
    .await?;
  sqlx::query!("DELETE FROM local_accounts WHERE user_id = $1", id)
    .execute(&mut **tx)
    .await?;
  sqlx::query!("DELETE FROM memberships WHERE user_id = $1", id)
    .execute(&mut **tx)
    .await?;
  sqlx::query!("DELETE FROM space_members WHERE user_id = $1", id)
    .execute(&mut **tx)
    .await?;
  sqlx::query!("DELETE FROM user_group_members WHERE user_id = $1", id)
    .execute(&mut **tx)
    .await?;
  sqlx::query!("DELETE FROM share_links WHERE created_by = $1", id)
    .execute(&mut **tx)
    .await?;
  sqlx::query!("DELETE FROM org_recovery_requests WHERE user_id = $1", id)
    .execute(&mut **tx)
    .await?;
  sqlx::query!("DELETE FROM org_recovery_shares WHERE admin_id = $1", id)
    .execute(&mut **tx)
    .await?;
  sqlx::query!("DELETE FROM org_recovery_approvals WHERE admin_id = $1", id)
    .execute(&mut **tx)
    .await?;
  sqlx::query!(
    "DELETE FROM org_recovery_request_shares WHERE from_admin_id = $1",
    id
  )
  .execute(&mut **tx)
  .await?;
  sqlx::query!("DELETE FROM presence WHERE user_id = $1", id)
    .execute(&mut **tx)
    .await?;
  sqlx::query!("DELETE FROM custom_status WHERE user_id = $1", id)
    .execute(&mut **tx)
    .await?;
  sqlx::query!("DELETE FROM user_profiles WHERE user_id = $1", id)
    .execute(&mut **tx)
    .await?;
  sqlx::query!("DELETE FROM user_backgrounds WHERE user_id = $1", id)
    .execute(&mut **tx)
    .await?;
  sqlx::query!("DELETE FROM threads_followed WHERE user_id = $1", id)
    .execute(&mut **tx)
    .await?;
  sqlx::query!("DELETE FROM user_notification_settings WHERE user_id = $1", id)
    .execute(&mut **tx)
    .await?;
  sqlx::query!("DELETE FROM user_email_digest WHERE user_id = $1", id)
    .execute(&mut **tx)
    .await?;
  sqlx::query!("DELETE FROM urgent_reminders WHERE user_id = $1", id)
    .execute(&mut **tx)
    .await?;
  sqlx::query!("DELETE FROM deleted_conversations WHERE user_id = $1", id)
    .execute(&mut **tx)
    .await?;
  Ok(())
}

/// Groups the removed memberships by conversation and space, with who remains in each.
fn removal_effects(
  devices: Vec<i64>,
  conversation_rows: Vec<(i64, Option<i64>)>,
  space_rows: Vec<(i64, Option<i64>)>,
) -> Result<ChatRemovalEffects, DbError> {
  let mut effects = ChatRemovalEffects {
    devices: devices.into_iter().map(from_db).collect::<Result<_, _>>()?,
    ..ChatRemovalEffects::default()
  };
  for (id, members) in grouped(conversation_rows)? {
    effects.co_members.extend(members.iter().copied());
    effects.conversations.push((from_db(id)?, members));
  }
  for (id, members) in grouped(space_rows)? {
    effects.spaces.push((from_db(id)?, members));
  }
  effects.co_members.sort_unstable();
  effects.co_members.dedup();
  Ok(effects)
}

fn grouped(rows: Vec<(i64, Option<i64>)>) -> Result<BTreeMap<i64, Vec<UserId>>, DbError> {
  let mut grouped: BTreeMap<i64, Vec<UserId>> = BTreeMap::new();
  for (id, maybe_other) in rows {
    let entry = grouped.entry(id).or_default();
    if let Some(other) = maybe_other {
      entry.push(from_db(other)?);
    }
  }
  Ok(grouped)
}
