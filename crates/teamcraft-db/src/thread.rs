//! Followed threads, thread read positions and unread counts (PLAN.md §3).

use teamcraft_proto::{ConversationId, MessageId, ThreadId, UserId};

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db, to_db_all};

/// A reply that was just stored in a thread.
#[derive(Debug, Clone)]
pub struct ThreadActivity<'a> {
  pub conversation: ConversationId,
  pub thread: ThreadId,
  pub message: MessageId,
  pub sender: UserId,
  /// Author of the thread's root message.
  pub root_sender: UserId,
  pub mentioned: &'a [UserId],
}

/// A thread the user follows, with counts relative to their read position.
#[derive(Debug, Clone)]
pub struct FollowedThread {
  pub conversation: ConversationId,
  pub thread: ThreadId,
  /// `started`, `replied`, `mentioned` or `manual`.
  pub reason: String,
  pub reply_count: u32,
  pub unread_count: u32,
  pub mention_count: u32,
  pub last_reply: MessageId,
}

/// Filter and page of the threads inbox.
#[derive(Debug, Clone)]
pub struct ThreadQuery {
  pub unread_only: bool,
  pub maybe_before: Option<MessageId>,
  pub limit: i64,
}

/// Unread top-level messages of one conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConversationUnreadRow {
  pub conversation: ConversationId,
  pub unread_count: u32,
  pub mention_count: u32,
}

/// Unread replies in followed threads, summed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ThreadUnreadTotals {
  pub unread_thread_count: u32,
  pub mention_count: u32,
}

impl Database {
  /// Applies the automatic follow rules for a stored reply and returns every user whose follow
  /// state changed:
  ///
  /// - the sender follows (also again after an unfollow: replying is explicit),
  /// - the root author follows when somebody else replies,
  /// - mentioned members follow if they enabled `auto_follow_mentioned_threads`.
  ///
  /// Users who unfollowed manually are not brought back by anyone else's reply.
  pub async fn record_thread_activity(&self, activity: &ThreadActivity<'_>) -> Result<Vec<UserId>, DbError> {
    let mut tx = self.pool().begin().await?;
    let sender_reason = if activity.sender == activity.root_sender {
      "started"
    } else {
      "replied"
    };
    sqlx::query!(
      r#"INSERT INTO threads_followed (user_id, conversation_id, thread_id, state, reason, last_read_message_id)
         VALUES ($1, $2, $3, 'following', $4, $5)
         ON CONFLICT (user_id, conversation_id, thread_id) DO UPDATE SET
           reason = CASE WHEN threads_followed.state = 'unfollowed' THEN EXCLUDED.reason
                         ELSE threads_followed.reason END,
           state = 'following',
           last_read_message_id = GREATEST(COALESCE(threads_followed.last_read_message_id, 0),
                                           EXCLUDED.last_read_message_id),
           updated_at = now()"#,
      to_db(activity.sender),
      to_db(activity.conversation),
      to_db(activity.thread),
      sender_reason,
      to_db(activity.message),
    )
    .execute(&mut *tx)
    .await?;
    let mut changed = vec![activity.sender];

    let root_author = sqlx::query_scalar!(
      r#"INSERT INTO threads_followed (user_id, conversation_id, thread_id, state, reason)
         SELECT user_id, conversation_id, $3, 'following', 'started' FROM memberships
         WHERE conversation_id = $1 AND user_id = $2 AND user_id <> $4
         ON CONFLICT DO NOTHING RETURNING user_id"#,
      to_db(activity.conversation),
      to_db(activity.root_sender),
      to_db(activity.thread),
      to_db(activity.sender),
    )
    .fetch_all(&mut *tx)
    .await?;
    let mentioned = sqlx::query_scalar!(
      r#"INSERT INTO threads_followed (user_id, conversation_id, thread_id, state, reason)
         SELECT ms.user_id, ms.conversation_id, $3, 'following', 'mentioned'
         FROM memberships ms JOIN users u ON u.id = ms.user_id
         WHERE ms.conversation_id = $1 AND ms.user_id = ANY($2) AND ms.user_id <> $4
           AND u.auto_follow_mentioned_threads
         ON CONFLICT DO NOTHING RETURNING user_id"#,
      to_db(activity.conversation),
      &to_db_all(activity.mentioned),
      to_db(activity.thread),
      to_db(activity.sender),
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    for raw in root_author.into_iter().chain(mentioned) {
      changed.push(from_db(raw)?);
    }
    Ok(changed)
  }

  /// Follows (starting "from now": existing replies count as read) or unfollows a thread.
  pub async fn set_thread_follow(
    &self,
    user: UserId,
    conversation: ConversationId,
    thread: ThreadId,
    is_following: bool,
  ) -> Result<(), DbError> {
    if is_following {
      sqlx::query!(
        r#"INSERT INTO threads_followed (user_id, conversation_id, thread_id, state, reason, last_read_message_id)
           VALUES ($1, $2, $3, 'following', 'manual',
                   (SELECT MAX(id) FROM messages WHERE conversation_id = $2 AND thread_id = $3))
           ON CONFLICT (user_id, conversation_id, thread_id) DO UPDATE SET
             last_read_message_id = CASE WHEN threads_followed.state = 'unfollowed'
               THEN EXCLUDED.last_read_message_id ELSE threads_followed.last_read_message_id END,
             reason = CASE WHEN threads_followed.state = 'unfollowed' THEN 'manual'
               ELSE threads_followed.reason END,
             state = 'following', updated_at = now()"#,
        to_db(user),
        to_db(conversation),
        to_db(thread),
      )
      .execute(self.pool())
      .await?;
    } else {
      sqlx::query!(
        r#"INSERT INTO threads_followed (user_id, conversation_id, thread_id, state, reason)
           VALUES ($1, $2, $3, 'unfollowed', 'manual')
           ON CONFLICT (user_id, conversation_id, thread_id) DO UPDATE SET
             state = 'unfollowed', updated_at = now()"#,
        to_db(user),
        to_db(conversation),
        to_db(thread),
      )
      .execute(self.pool())
      .await?;
    }
    Ok(())
  }

  /// Moves the thread read position forward (never back). Returns `false` if the user does not
  /// follow the thread.
  pub async fn set_thread_read_position(
    &self,
    user: UserId,
    conversation: ConversationId,
    thread: ThreadId,
    message: MessageId,
  ) -> Result<bool, DbError> {
    let result = sqlx::query!(
      r#"UPDATE threads_followed
         SET last_read_message_id = GREATEST(COALESCE(last_read_message_id, 0), $4), updated_at = now()
         WHERE user_id = $1 AND conversation_id = $2 AND thread_id = $3 AND state = 'following'"#,
      to_db(user),
      to_db(conversation),
      to_db(thread),
      to_db(message),
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() > 0)
  }

  /// The user's followed threads in conversations they are still a member of, newest activity first.
  /// Fetches `limit + 1` rows so the caller can tell whether more exist.
  pub async fn list_followed_threads(
    &self,
    user: UserId,
    query: &ThreadQuery,
  ) -> Result<Vec<FollowedThread>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT tf.conversation_id, tf.thread_id, tf.reason,
                COUNT(m.id) AS "reply_count!",
                MAX(m.id) AS "last_reply!",
                COUNT(m.id) FILTER (WHERE m.id > COALESCE(tf.last_read_message_id, 0)
                                      AND m.sender_user_id <> tf.user_id) AS "unread!",
                COUNT(m.id) FILTER (WHERE m.id > COALESCE(tf.last_read_message_id, 0)
                                      AND m.sender_user_id <> tf.user_id
                                      AND tf.user_id = ANY(m.mentioned_user_ids)) AS "mentions!"
         FROM threads_followed tf
         JOIN memberships ms ON ms.conversation_id = tf.conversation_id AND ms.user_id = tf.user_id
         JOIN messages m ON m.conversation_id = tf.conversation_id AND m.thread_id = tf.thread_id
         WHERE tf.user_id = $1 AND tf.state = 'following'
         GROUP BY tf.conversation_id, tf.thread_id, tf.reason
         HAVING ($2::bigint IS NULL OR MAX(m.id) < $2)
            AND (NOT $3 OR COUNT(m.id) FILTER (WHERE m.id > COALESCE(tf.last_read_message_id, 0)
                                                 AND m.sender_user_id <> tf.user_id) > 0)
         ORDER BY MAX(m.id) DESC
         LIMIT $4"#,
      to_db(user),
      query.maybe_before.map(to_db),
      query.unread_only,
      query.limit + 1,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(FollowedThread {
          conversation: from_db(row.conversation_id)?,
          thread: from_db(row.thread_id)?,
          reason: row.reason,
          reply_count: saturating_u32(row.reply_count),
          unread_count: saturating_u32(row.unread),
          mention_count: saturating_u32(row.mentions),
          last_reply: from_db(row.last_reply)?,
        })
      })
      .collect()
  }

  /// Unread top-level messages per conversation (only those with something unread). Messages from
  /// before the user joined and the user's own messages don't count.
  pub async fn conversation_unread_counts(
    &self,
    user: UserId,
  ) -> Result<Vec<ConversationUnreadRow>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT ms.conversation_id,
                COUNT(m.id) AS "unread!",
                COUNT(m.id) FILTER (WHERE ms.user_id = ANY(m.mentioned_user_ids)) AS "mentions!"
         FROM memberships ms
         JOIN messages m ON m.conversation_id = ms.conversation_id
         WHERE ms.user_id = $1 AND m.thread_id IS NULL AND m.sender_user_id <> ms.user_id
           AND m.id > COALESCE(ms.last_read_message_id, 0)
           AND m.id >= tc_snowflake_floor(ms.joined_at)
         GROUP BY ms.conversation_id
         ORDER BY ms.conversation_id"#,
      to_db(user),
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(ConversationUnreadRow {
          conversation: from_db(row.conversation_id)?,
          unread_count: saturating_u32(row.unread),
          mention_count: saturating_u32(row.mentions),
        })
      })
      .collect()
  }

  /// Number of followed threads with unread replies and the unread replies that mention the user.
  pub async fn thread_unread_totals(&self, user: UserId) -> Result<ThreadUnreadTotals, DbError> {
    let row = sqlx::query!(
      r#"SELECT COUNT(*) AS "threads!", COALESCE(SUM(t.mentions), 0)::bigint AS "mentions!"
         FROM (
           SELECT COUNT(m.id) FILTER (WHERE tf.user_id = ANY(m.mentioned_user_ids)) AS mentions
           FROM threads_followed tf
           JOIN memberships ms ON ms.conversation_id = tf.conversation_id AND ms.user_id = tf.user_id
           JOIN messages m ON m.conversation_id = tf.conversation_id AND m.thread_id = tf.thread_id
           WHERE tf.user_id = $1 AND tf.state = 'following'
             AND m.id > COALESCE(tf.last_read_message_id, 0) AND m.sender_user_id <> tf.user_id
           GROUP BY tf.conversation_id, tf.thread_id
         ) t"#,
      to_db(user),
    )
    .fetch_one(self.pool())
    .await?;
    Ok(ThreadUnreadTotals {
      unread_thread_count: saturating_u32(row.threads),
      mention_count: saturating_u32(row.mentions),
    })
  }
}

fn saturating_u32(count: i64) -> u32 {
  u32::try_from(count).unwrap_or(u32::MAX)
}
