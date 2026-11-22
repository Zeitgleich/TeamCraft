//! What the server derives from the legal holds of the signed policy (data-export.md §4.2, issue
//! #2135): the held people, the conversations their holds cover, and the messages their senders
//! withdrew while a hold kept them.
//!
//! The policy entry decides; [`HoldEvidence`] is what a deletion reads in its transaction after it
//! locked what it deletes: the newest entry's payload and which of the locked conversations a person
//! hold covers. The held people are written in the transaction that stores the entry
//! ([`Database::append_workspace_log`]), so both come from the same entry.

use std::collections::BTreeSet;

use sqlx::{Postgres, Transaction};
use teamcraft_proto::{ClientMessageId, ConversationId, UserId};

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db, to_db_all};

/// What a deletion decides a legal hold by, read in its transaction after it locked the rows it
/// deletes (issue #2143).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HoldEvidence {
  /// The newest policy entry's payload as stored; its `legal_holds` name conversations.
  pub maybe_policy: Option<Vec<u8>>,
  /// Of the conversations the deletion asked about, those a held person's hold covers.
  pub covered_by_people: Vec<ConversationId>,
}

/// A held person as the server keeps them, from the newest policy entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeldPerson {
  pub user: UserId,
  /// Only their direct messages and groups.
  pub is_direct_only: bool,
}

/// What became of a member's withdrawal of their own message ([`Database::withdraw_message_locked`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Withdrawal {
  /// No such message of this sender in the conversation (gone already, or someone else's).
  Nothing,
  /// The row is gone; the object holding its ciphertext, if any, is for the caller to delete.
  Deleted { maybe_object_key: Option<String> },
  /// A legal hold keeps the conversation: the message is marked deleted and kept.
  KeptForHold,
}

/// Withdrawn messages a released hold no longer keeps, removed by
/// [`Database::apply_pending_deletions_locked`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PendingDeletionsApplied {
  pub messages: u64,
  /// Objects holding their ciphertext, for the caller to delete after the commit.
  pub object_keys: Vec<String>,
}

impl Database {
  /// The newest policy payload and which of `conversations` a person hold covers, in a deletion's
  /// transaction after it locked them.
  pub(crate) async fn hold_evidence_in(
    tx: &mut Transaction<'_, Postgres>,
    conversations: &[ConversationId],
  ) -> Result<HoldEvidence, DbError> {
    let maybe_policy = Self::latest_policy_payload_in(tx).await?;
    let covered_by_people = if conversations.is_empty() {
      Vec::new()
    } else {
      let ids = sqlx::query_scalar!(
        r#"SELECT DISTINCT conversation_id AS "conversation_id!" FROM legal_hold_covered
           WHERE conversation_id = ANY($1) ORDER BY 1"#,
        &to_db_all(conversations),
      )
      .fetch_all(&mut **tx)
      .await?;
      ids.into_iter().map(from_db).collect::<Result<_, _>>()?
    };
    Ok(HoldEvidence {
      maybe_policy,
      covered_by_people,
    })
  }

  /// Every conversation a person hold covers now: to leave them out of a list before any lock is
  /// taken. Deciding a deletion uses the [`HoldEvidence`] read under its locks instead.
  pub async fn conversations_covered_by_people(&self) -> Result<Vec<ConversationId>, DbError> {
    let ids = sqlx::query_scalar!(
      r#"SELECT DISTINCT conversation_id AS "conversation_id!" FROM legal_hold_covered ORDER BY 1"#,
    )
    .fetch_all(self.pool())
    .await?;
    ids.into_iter().map(from_db).collect()
  }

  /// The held people as the newest policy entry placed them.
  pub async fn held_people(&self) -> Result<Vec<HeldPerson>, DbError> {
    let rows = sqlx::query!("SELECT user_id, direct_only FROM legal_hold_people ORDER BY user_id")
      .fetch_all(self.pool())
      .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(HeldPerson {
          user: from_db(row.user_id)?,
          is_direct_only: row.direct_only,
        })
      })
      .collect()
  }

  /// Makes the held people those of a new policy entry, in its transaction. A released person's
  /// departures go with them. The conversations newly held people cover are share-locked first, in
  /// id order with `channels` (the conversations the entry newly holds itself), so a deletion of one
  /// of them waits or is waited for (issue #2143). `None` when a newly held person or a held
  /// conversation doesn't exist (anymore): nothing may be stored then. Otherwise the conversations
  /// that were locked.
  pub(crate) async fn place_holds_in(
    tx: &mut Transaction<'_, Postgres>,
    channels: &[ConversationId],
    maybe_people: Option<&[HeldPerson]>,
  ) -> Result<HoldsPlaced, DbError> {
    let mut held_ids: Vec<i64> = to_db_all(channels);
    held_ids.sort_unstable();
    held_ids.dedup();
    let Some(people) = maybe_people else {
      return Ok(if lock_existing(tx, &held_ids, &held_ids).await? {
        HoldsPlaced::Placed
      } else {
        HoldsPlaced::NotExistingConversations
      });
    };
    let user_ids: Vec<i64> = people.iter().map(|person| to_db(person.user)).collect();
    let direct_only: Vec<bool> = people.iter().map(|person| person.is_direct_only).collect();
    let known = sqlx::query_scalar!(
      r#"SELECT count(*) AS "count!" FROM users WHERE id = ANY($1)"#,
      &user_ids,
    )
    .fetch_one(&mut **tx)
    .await?;
    if usize::try_from(known).unwrap_or(0) != user_ids.iter().collect::<BTreeSet<_>>().len() {
      return Ok(HoldsPlaced::UnknownPeople);
    }
    let newly_held: Vec<i64> = sqlx::query_scalar!(
      r#"SELECT p.id AS "id!" FROM unnest($1::bigint[], $2::boolean[]) AS p (id, direct_only)
         WHERE NOT EXISTS (SELECT 1 FROM legal_hold_people h
                           WHERE h.user_id = p.id AND (h.direct_only = p.direct_only OR NOT h.direct_only))"#,
      &user_ids,
      &direct_only,
    )
    .fetch_all(&mut **tx)
    .await?;
    let covered = if newly_held.is_empty() {
      Vec::new()
    } else {
      sqlx::query_scalar!(
        r#"SELECT DISTINCT c.id AS "id!" FROM conversations c
           WHERE c.deleted_at IS NULL AND c.kind IN ('dm', 'group', 'channel')
             AND (EXISTS (SELECT 1 FROM memberships m WHERE m.conversation_id = c.id AND m.user_id = ANY($1))
               OR (c.participants_key IS NOT NULL
                   AND string_to_array(c.participants_key, '-')::bigint[] && $1))"#,
        &newly_held,
      )
      .fetch_all(&mut **tx)
      .await?
    };
    let mut lock_ids: Vec<i64> = held_ids.iter().copied().chain(covered).collect();
    lock_ids.sort_unstable();
    lock_ids.dedup();
    if !lock_existing(tx, &lock_ids, &held_ids).await? {
      return Ok(HoldsPlaced::NotExistingConversations);
    }
    sqlx::query!(
      "DELETE FROM legal_hold_departures WHERE NOT (user_id = ANY($1))",
      &user_ids,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
      "DELETE FROM legal_hold_people WHERE NOT (user_id = ANY($1))",
      &user_ids,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
      r#"INSERT INTO legal_hold_people (user_id, direct_only)
         SELECT * FROM unnest($1::bigint[], $2::boolean[])
         ON CONFLICT (user_id) DO UPDATE SET direct_only = EXCLUDED.direct_only"#,
      &user_ids,
      &direct_only,
    )
    .execute(&mut **tx)
    .await?;
    Ok(HoldsPlaced::Placed)
  }

  /// A member withdraws their own message (`deletes` of a sent message, issue #2135), in one
  /// transaction that first locks the conversation's row and then reads the holds: `is_held` gets the
  /// [`HoldEvidence`] and says whether a legal hold keeps the conversation. Kept, the message is
  /// marked deleted, hidden from history and removed once the hold is released
  /// ([`Self::apply_pending_deletions_locked`]); otherwise its row goes now. Only a message `sender`
  /// sent counts. Idempotent: a repeated withdrawal finds the mark, or nothing.
  pub async fn withdraw_message_locked<E: From<DbError>>(
    &self,
    conversation: ConversationId,
    sender: UserId,
    target: ClientMessageId,
    is_held: impl FnOnce(&HoldEvidence) -> Result<bool, E>,
  ) -> Result<Withdrawal, E> {
    let mut tx = self.pool().begin().await.map_err(DbError::from)?;
    if !lock_conversation(&mut tx, conversation).await? {
      return Ok(Withdrawal::Nothing);
    }
    let maybe_id = sqlx::query_scalar!(
      r#"SELECT m.id FROM message_client_ids c
         JOIN messages m ON m.id = c.message_id AND m.conversation_id = c.conversation_id
         WHERE c.conversation_id = $1 AND c.client_message_id = $2 AND m.sender_user_id = $3"#,
      to_db(conversation),
      target.as_bytes().as_slice(),
      to_db(sender),
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(DbError::from)?;
    let Some(id) = maybe_id else {
      return Ok(Withdrawal::Nothing);
    };
    let evidence = Self::hold_evidence_in(&mut tx, &[conversation]).await?;
    let withdrawal = if is_held(&evidence)? {
      sqlx::query!(
        r#"UPDATE messages SET deleted_at = now()
           WHERE id = $1 AND conversation_id = $2 AND deleted_at IS NULL"#,
        id,
        to_db(conversation),
      )
      .execute(&mut *tx)
      .await
      .map_err(DbError::from)?;
      Withdrawal::KeptForHold
    } else {
      let maybe_object_key = delete_messages_in(&mut tx, conversation, &[id])
        .await?
        .into_iter()
        .next();
      Withdrawal::Deleted { maybe_object_key }
    };
    tx.commit().await.map_err(DbError::from)?;
    Ok(withdrawal)
  }

  /// Conversations with messages withdrawn while a hold kept them, at most `limit`.
  pub async fn conversations_with_pending_deletions(
    &self,
    limit: i64,
  ) -> Result<Vec<ConversationId>, DbError> {
    let ids = sqlx::query_scalar!(
      r#"SELECT DISTINCT conversation_id FROM messages WHERE deleted_at IS NOT NULL
         ORDER BY conversation_id LIMIT $1"#,
      limit,
    )
    .fetch_all(self.pool())
    .await?;
    ids.into_iter().map(from_db).collect()
  }

  /// Removes the messages withdrawn in `conversation` while a hold kept it, if `is_held` says no hold
  /// keeps it any more, by the [`HoldEvidence`] read after the conversation's row is locked: a hold
  /// placed again at the same moment either waits for this or is seen here. `None` when it is still
  /// held or gone (a deleted conversation's messages go with it).
  pub async fn apply_pending_deletions_locked<E: From<DbError>>(
    &self,
    conversation: ConversationId,
    is_held: impl FnOnce(&HoldEvidence) -> Result<bool, E>,
  ) -> Result<Option<PendingDeletionsApplied>, E> {
    let mut tx = self.pool().begin().await.map_err(DbError::from)?;
    if !lock_conversation(&mut tx, conversation).await? {
      return Ok(None);
    }
    let evidence = Self::hold_evidence_in(&mut tx, &[conversation]).await?;
    if is_held(&evidence)? {
      return Ok(None);
    }
    let ids = sqlx::query_scalar!(
      "SELECT id FROM messages WHERE conversation_id = $1 AND deleted_at IS NOT NULL",
      to_db(conversation),
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(DbError::from)?;
    let messages = ids.len() as u64;
    let object_keys = delete_messages_in(&mut tx, conversation, &ids).await?;
    tx.commit().await.map_err(DbError::from)?;
    Ok(Some(PendingDeletionsApplied {
      messages,
      object_keys,
    }))
  }
}

/// How [`Database::place_holds_in`] went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HoldsPlaced {
  Placed,
  NotExistingConversations,
  UnknownPeople,
}

/// Locks a conversation's row as retention does: a policy append share-locking it waits, and sending
/// (a key share) doesn't. `false` when it is gone.
async fn lock_conversation(
  tx: &mut Transaction<'_, Postgres>,
  conversation: ConversationId,
) -> Result<bool, DbError> {
  let is_there = sqlx::query_scalar!(
    "SELECT id FROM conversations WHERE id = $1 AND deleted_at IS NULL FOR NO KEY UPDATE",
    to_db(conversation),
  )
  .fetch_optional(&mut **tx)
  .await?
  .is_some();
  Ok(is_there)
}

/// Share-locks the conversations `ids` (sorted) in id order, like the deletion of a space locks them.
/// `false` when one of `required` (a subset: what the entry holds by name) is missing, deleted or a
/// meeting chat. Others that went meanwhile are just not locked.
async fn lock_existing(
  tx: &mut Transaction<'_, Postgres>,
  ids: &[i64],
  required: &[i64],
) -> Result<bool, DbError> {
  if ids.is_empty() {
    return Ok(true);
  }
  let locked = sqlx::query_scalar!(
    r#"SELECT id FROM conversations
       WHERE id = ANY($1) AND kind IN ('dm', 'group', 'channel') AND deleted_at IS NULL
       ORDER BY id FOR SHARE"#,
    ids,
  )
  .fetch_all(&mut **tx)
  .await?;
  Ok(required.iter().all(|id| locked.contains(id)))
}

/// Deletes the message rows `ids` of `conversation` and their client IDs; returns the objects that
/// held their ciphertext.
async fn delete_messages_in(
  tx: &mut Transaction<'_, Postgres>,
  conversation: ConversationId,
  ids: &[i64],
) -> Result<Vec<String>, DbError> {
  if ids.is_empty() {
    return Ok(Vec::new());
  }
  let keys = sqlx::query_scalar!(
    r#"DELETE FROM messages WHERE conversation_id = $1 AND id = ANY($2)
       RETURNING ciphertext_s3_key AS "key?""#,
    to_db(conversation),
    ids,
  )
  .fetch_all(&mut **tx)
  .await?;
  sqlx::query!(
    "DELETE FROM message_client_ids WHERE conversation_id = $1 AND message_id = ANY($2)",
    to_db(conversation),
    ids,
  )
  .execute(&mut **tx)
  .await?;
  Ok(keys.into_iter().flatten().collect())
}
