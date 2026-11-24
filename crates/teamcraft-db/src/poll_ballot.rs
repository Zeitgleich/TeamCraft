//! Ballot boxes of anonymous polls (issue #2121, e2ee.md §4.12.1). The server keeps one sealed ballot
//! per account while a box is open and forgets whose ballot is whose when it closes, in the same
//! transaction that closes it.

use teamcraft_proto::{ConversationId, MessageId, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};

/// A ballot box as a member reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBallotBox {
  pub creator_user_id: UserId,
  pub maybe_closes_at: Option<OffsetDateTime>,
  pub is_closed: bool,
  pub ballot_count: u32,
  /// The reader's own ballot while the box is open.
  pub maybe_mine: Option<Vec<u8>>,
  /// Every ballot, sorted by its bytes, once the box is closed; empty while open.
  pub ballots: Vec<Vec<u8>>,
}

/// What a change of a ballot did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BallotOutcome {
  /// Taken; the box now holds this many ballots.
  Taken { ballot_count: u32 },
  /// There is no box for this poll.
  NoBox,
  /// The box is closed (now or before).
  Closed,
}

/// A box closed by a sweep, for the event its members get.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClosedBallotBox {
  pub conversation_id: ConversationId,
  pub message_id: MessageId,
  pub ballot_count: u32,
}

impl Database {
  /// The account whose device sent `message` of `conversation`, if the server still holds it.
  pub async fn message_sender_user(
    &self,
    conversation: ConversationId,
    message: MessageId,
  ) -> Result<Option<UserId>, DbError> {
    let raw = sqlx::query_scalar!(
      r#"SELECT d.user_id FROM messages m JOIN devices d ON d.id = m.sender_device_id
         WHERE m.id = $1 AND m.conversation_id = $2"#,
      to_db(message),
      to_db(conversation),
    )
    .fetch_optional(self.pool())
    .await?;
    raw.map(from_db).transpose()
  }

  /// Opens the box of the poll `message`. A box already open stays as it is; `false` then.
  pub async fn open_ballot_box(
    &self,
    conversation: ConversationId,
    message: MessageId,
    creator: UserId,
    maybe_closes_at: Option<OffsetDateTime>,
  ) -> Result<bool, DbError> {
    let result = sqlx::query!(
      r#"INSERT INTO poll_ballot_boxes (conversation_id, message_id, creator_user_id, closes_at)
         VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING"#,
      to_db(conversation),
      to_db(message),
      to_db(creator),
      maybe_closes_at,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() > 0)
  }

  /// The box of the poll `message` as `reader` sees it at `now`, closing it first if its time
  /// passed. `None` when there is none.
  pub async fn ballot_box(
    &self,
    conversation: ConversationId,
    message: MessageId,
    reader: UserId,
    now: OffsetDateTime,
  ) -> Result<Option<StoredBallotBox>, DbError> {
    self.close_ballot_box_if_due(conversation, message, now).await?;
    let Some(row) = sqlx::query!(
      r#"SELECT creator_user_id, closes_at, closed_at IS NOT NULL AS "is_closed!",
           (SELECT count(*) FROM poll_ballots b
             WHERE b.conversation_id = x.conversation_id AND b.message_id = x.message_id) AS "ballot_count!"
         FROM poll_ballot_boxes x WHERE conversation_id = $1 AND message_id = $2"#,
      to_db(conversation),
      to_db(message),
    )
    .fetch_optional(self.pool())
    .await?
    else {
      return Ok(None);
    };
    let (maybe_mine, ballots) = if row.is_closed {
      let ballots = sqlx::query_scalar!(
        "SELECT ballot FROM poll_ballots WHERE conversation_id = $1 AND message_id = $2 ORDER BY ballot",
        to_db(conversation),
        to_db(message),
      )
      .fetch_all(self.pool())
      .await?;
      (None, ballots)
    } else {
      let mine = sqlx::query_scalar!(
        r#"SELECT ballot FROM poll_ballots
           WHERE conversation_id = $1 AND message_id = $2 AND voter_user_id = $3"#,
        to_db(conversation),
        to_db(message),
        to_db(reader),
      )
      .fetch_optional(self.pool())
      .await?;
      (mine, Vec::new())
    };
    Ok(Some(StoredBallotBox {
      creator_user_id: from_db(row.creator_user_id)?,
      maybe_closes_at: row.closes_at,
      is_closed: row.is_closed,
      ballot_count: count(row.ballot_count),
      maybe_mine,
      ballots,
    }))
  }

  /// Keeps `ballot` as `voter`'s one ballot in the box of `message` (replacing an earlier one), or
  /// withdraws it when `maybe_ballot` is `None`, while the box is open at `now`.
  pub async fn set_ballot(
    &self,
    conversation: ConversationId,
    message: MessageId,
    voter: UserId,
    maybe_ballot: Option<&[u8]>,
    now: OffsetDateTime,
  ) -> Result<BallotOutcome, DbError> {
    self.close_ballot_box_if_due(conversation, message, now).await?;
    let mut tx = self.pool().begin().await?;
    // Locks the box, so a close either comes first (and the ballot is refused) or waits for this.
    // A closing time that passed since the check above counts as closed, by the server's clock.
    let maybe_is_closed = sqlx::query_scalar!(
      r#"SELECT (closed_at IS NOT NULL OR COALESCE(closes_at <= $3, false)) AS "is_closed!"
         FROM poll_ballot_boxes WHERE conversation_id = $1 AND message_id = $2 FOR UPDATE"#,
      to_db(conversation),
      to_db(message),
      now,
    )
    .fetch_optional(&mut *tx)
    .await?;
    match maybe_is_closed {
      None => return Ok(BallotOutcome::NoBox),
      Some(true) => return Ok(BallotOutcome::Closed),
      Some(false) => {},
    }
    match maybe_ballot {
      Some(ballot) => {
        sqlx::query!(
          r#"INSERT INTO poll_ballots (conversation_id, message_id, voter_user_id, ballot)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (conversation_id, message_id, voter_user_id) DO UPDATE SET ballot = EXCLUDED.ballot"#,
          to_db(conversation),
          to_db(message),
          to_db(voter),
          ballot,
        )
        .execute(&mut *tx)
        .await?;
      },
      None => {
        sqlx::query!(
          "DELETE FROM poll_ballots WHERE conversation_id = $1 AND message_id = $2 AND voter_user_id = $3",
          to_db(conversation),
          to_db(message),
          to_db(voter),
        )
        .execute(&mut *tx)
        .await?;
      },
    }
    let ballot_count = ballot_count_in(&mut tx, conversation, message).await?;
    tx.commit().await?;
    Ok(BallotOutcome::Taken { ballot_count })
  }

  /// Closes the box of `message` at `now` and forgets its voters. `None` when there is no box;
  /// otherwise whether this call closed it, and the number of ballots.
  pub async fn close_ballot_box(
    &self,
    conversation: ConversationId,
    message: MessageId,
    now: OffsetDateTime,
  ) -> Result<Option<(bool, u32)>, DbError> {
    let mut tx = self.pool().begin().await?;
    let maybe_closed_at = sqlx::query_scalar!(
      "SELECT closed_at FROM poll_ballot_boxes WHERE conversation_id = $1 AND message_id = $2 FOR UPDATE",
      to_db(conversation),
      to_db(message),
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(closed_at) = maybe_closed_at else {
      return Ok(None);
    };
    let did_close = closed_at.is_none();
    if did_close {
      close_in(&mut tx, conversation, message, now).await?;
    }
    let ballot_count = ballot_count_in(&mut tx, conversation, message).await?;
    tx.commit().await?;
    Ok(Some((did_close, ballot_count)))
  }

  /// Closes every open box whose closing time passed at `now`, at most `limit` per call.
  pub async fn close_due_ballot_boxes(
    &self,
    now: OffsetDateTime,
    limit: i64,
  ) -> Result<Vec<ClosedBallotBox>, DbError> {
    let due = sqlx::query!(
      r#"SELECT conversation_id, message_id FROM poll_ballot_boxes
         WHERE closed_at IS NULL AND closes_at <= $1 ORDER BY closes_at LIMIT $2"#,
      now,
      limit,
    )
    .fetch_all(self.pool())
    .await?;
    let mut closed = Vec::with_capacity(due.len());
    for row in due {
      let conversation_id = from_db(row.conversation_id)?;
      let message_id = from_db(row.message_id)?;
      if let Some((true, ballot_count)) = self.close_ballot_box(conversation_id, message_id, now).await? {
        closed.push(ClosedBallotBox {
          conversation_id,
          message_id,
          ballot_count,
        });
      }
    }
    Ok(closed)
  }

  /// Closes the box of `message` when its closing time passed at `now`.
  async fn close_ballot_box_if_due(
    &self,
    conversation: ConversationId,
    message: MessageId,
    now: OffsetDateTime,
  ) -> Result<(), DbError> {
    let is_due = sqlx::query_scalar!(
      r#"SELECT EXISTS (SELECT 1 FROM poll_ballot_boxes WHERE conversation_id = $1 AND message_id = $2
           AND closed_at IS NULL AND closes_at <= $3) AS "is_due!""#,
      to_db(conversation),
      to_db(message),
      now,
    )
    .fetch_one(self.pool())
    .await?;
    if is_due {
      self.close_ballot_box(conversation, message, now).await?;
    }
    Ok(())
  }
}

/// Marks the box closed at `now`, at its closing time if that came first, and erases its voters.
async fn close_in(
  tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
  conversation: ConversationId,
  message: MessageId,
  now: OffsetDateTime,
) -> Result<(), DbError> {
  sqlx::query!(
    r#"UPDATE poll_ballot_boxes SET closed_at = LEAST($3, COALESCE(closes_at, $3))
       WHERE conversation_id = $1 AND message_id = $2"#,
    to_db(conversation),
    to_db(message),
    now,
  )
  .execute(&mut **tx)
  .await?;
  sqlx::query!(
    "UPDATE poll_ballots SET voter_user_id = NULL WHERE conversation_id = $1 AND message_id = $2",
    to_db(conversation),
    to_db(message),
  )
  .execute(&mut **tx)
  .await?;
  Ok(())
}

async fn ballot_count_in(
  tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
  conversation: ConversationId,
  message: MessageId,
) -> Result<u32, DbError> {
  let raw = sqlx::query_scalar!(
    r#"SELECT count(*) AS "count!" FROM poll_ballots WHERE conversation_id = $1 AND message_id = $2"#,
    to_db(conversation),
    to_db(message),
  )
  .fetch_one(&mut **tx)
  .await?;
  Ok(count(raw))
}

fn count(raw: i64) -> u32 {
  u32::try_from(raw).unwrap_or(u32::MAX)
}
