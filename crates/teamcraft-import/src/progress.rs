//! What the import did so far, so a run that stops (Ctrl-C, a crash, a lost network) goes on where
//! it was and a second run changes nothing (docs/import-synology.md §8). Holds IDs and file names
//! only, never message content or keys; the device's keys are in the core's encrypted store.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use teamcraft_proto::{ConversationId, Id, MessageId, Snowflake};

use crate::export::SourceId;

/// A step of one conversation's import that is done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
  /// Every post is stored.
  History,
  /// The members are in (or waiting for their first device).
  Members,
  /// Read positions, favourites and mute.
  MemberState,
  /// A member who has set up the app is the new channel's admin.
  Admin,
  /// The server announced the finished import.
  Finished,
  /// Archived, as in the old system.
  Archived,
  /// The import account left (everybody was in).
  Left,
}

impl Step {
  fn as_str(self) -> &'static str {
    match self {
      Self::History => "history",
      Self::Members => "members",
      Self::MemberState => "member_state",
      Self::Admin => "admin",
      Self::Finished => "finished",
      Self::Archived => "archived",
      Self::Left => "left",
    }
  }
}

pub struct Progress {
  db: Connection,
}

#[derive(Debug, thiserror::Error)]
pub enum ProgressError {
  #[error("the progress database: {0}")]
  Sql(#[from] rusqlite::Error),
  #[error("the progress database holds an invalid ID: {0}")]
  BadId(i64),
}

impl Progress {
  pub fn open(path: &Path) -> Result<Self, ProgressError> {
    let db = Connection::open(path)?;
    Self::set_up(db)
  }

  pub fn in_memory() -> Result<Self, ProgressError> {
    Self::set_up(Connection::open_in_memory()?)
  }

  fn set_up(db: Connection) -> Result<Self, ProgressError> {
    db.execute_batch(
      "PRAGMA journal_mode = WAL;
       PRAGMA synchronous = NORMAL;
       CREATE TABLE IF NOT EXISTS conversations (key TEXT PRIMARY KEY, conversation_id INTEGER NOT NULL);
       CREATE TABLE IF NOT EXISTS steps (key TEXT NOT NULL, step TEXT NOT NULL, PRIMARY KEY (key, step));
       CREATE TABLE IF NOT EXISTS posts (
         post INTEGER PRIMARY KEY, channel INTEGER NOT NULL, message_id INTEGER NOT NULL,
         sent_at INTEGER NOT NULL);
       CREATE INDEX IF NOT EXISTS posts_by_time ON posts (channel, sent_at);
       CREATE TABLE IF NOT EXISTS missing_files (post INTEGER NOT NULL, name TEXT NOT NULL);
       CREATE TABLE IF NOT EXISTS notes (
         at INTEGER NOT NULL, key TEXT NOT NULL, text TEXT NOT NULL, PRIMARY KEY (key, text));",
    )?;
    Ok(Self { db })
  }

  pub fn conversation(&self, key: &str) -> Result<Option<ConversationId>, ProgressError> {
    self
      .db
      .query_row(
        "SELECT conversation_id FROM conversations WHERE key = ?1",
        params![key],
        |row| row.get::<_, i64>(0),
      )
      .optional()?
      .map(id)
      .transpose()
  }

  pub fn set_conversation(&self, key: &str, conversation: ConversationId) -> Result<(), ProgressError> {
    self.db.execute(
      "INSERT INTO conversations (key, conversation_id) VALUES (?1, ?2)
       ON CONFLICT (key) DO UPDATE SET conversation_id = excluded.conversation_id",
      params![key, conversation.snowflake().get()],
    )?;
    Ok(())
  }

  pub fn is_done(&self, key: &str, step: Step) -> Result<bool, ProgressError> {
    Ok(
      self
        .db
        .query_row(
          "SELECT 1 FROM steps WHERE key = ?1 AND step = ?2",
          params![key, step.as_str()],
          |_| Ok(()),
        )
        .optional()?
        .is_some(),
    )
  }

  pub fn mark_done(&self, key: &str, step: Step) -> Result<(), ProgressError> {
    self.db.execute(
      "INSERT OR IGNORE INTO steps (key, step) VALUES (?1, ?2)",
      params![key, step.as_str()],
    )?;
    Ok(())
  }

  /// Notes the stored IDs of a batch of posts, in one transaction.
  pub fn record_posts(
    &mut self,
    channel: SourceId,
    posts: &[(SourceId, MessageId, i64)],
    missing: &[(SourceId, String)],
  ) -> Result<(), ProgressError> {
    let tx = self.db.transaction()?;
    for (post, message, sent_at) in posts {
      tx.execute(
        "INSERT OR REPLACE INTO posts (post, channel, message_id, sent_at) VALUES (?1, ?2, ?3, ?4)",
        params![post, channel, message.snowflake().get(), sent_at],
      )?;
    }
    for (post, name) in missing {
      tx.execute(
        "INSERT INTO missing_files (post, name) VALUES (?1, ?2)",
        params![post, name],
      )?;
    }
    tx.commit()?;
    Ok(())
  }

  /// The server ID a post was stored under.
  pub fn message_of(&self, post: SourceId) -> Result<Option<MessageId>, ProgressError> {
    self
      .db
      .query_row(
        "SELECT message_id FROM posts WHERE post = ?1",
        params![post],
        |row| row.get::<_, i64>(0),
      )
      .optional()?
      .map(id)
      .transpose()
  }

  /// How many of the channel's posts are stored.
  pub fn stored_posts(&self, channel: SourceId) -> Result<u64, ProgressError> {
    let count: i64 = self.db.query_row(
      "SELECT count(*) FROM posts WHERE channel = ?1",
      params![channel],
      |row| row.get(0),
    )?;
    Ok(u64::try_from(count).unwrap_or(0))
  }

  /// The newest stored post of the channel written at or before `at_ms`: what was read up to then.
  pub fn read_through(&self, channel: SourceId, at_ms: i64) -> Result<Option<MessageId>, ProgressError> {
    self
      .db
      .query_row(
        "SELECT message_id FROM posts WHERE channel = ?1 AND sent_at <= ?2 ORDER BY sent_at DESC, post DESC LIMIT 1",
        params![channel, at_ms],
        |row| row.get::<_, i64>(0),
      )
      .optional()?
      .map(id)
      .transpose()
  }

  /// Files the export didn't have, per channel.
  pub fn missing_files(&self, channel: SourceId) -> Result<u64, ProgressError> {
    let count: i64 = self.db.query_row(
      "SELECT count(*) FROM missing_files m JOIN posts p ON p.post = m.post WHERE p.channel = ?1",
      params![channel],
      |row| row.get(0),
    )?;
    Ok(u64::try_from(count).unwrap_or(0))
  }

  /// Something the report should tell about a conversation (a problem, a wait); kept once.
  pub fn note(&self, key: &str, at_ms: i64, text: &str) -> Result<(), ProgressError> {
    self.db.execute(
      "INSERT OR IGNORE INTO notes (at, key, text) VALUES (?1, ?2, ?3)",
      params![at_ms, key, text],
    )?;
    Ok(())
  }

  pub fn notes(&self, key: &str) -> Result<Vec<String>, ProgressError> {
    let mut statement = self
      .db
      .prepare("SELECT text FROM notes WHERE key = ?1 ORDER BY at")?;
    let rows = statement.query_map(params![key], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
  }

  pub fn clear_notes(&self, key: &str) -> Result<(), ProgressError> {
    self
      .db
      .execute("DELETE FROM notes WHERE key = ?1", params![key])?;
    Ok(())
  }
}

fn id<T: Id>(raw: i64) -> Result<T, ProgressError> {
  Snowflake::new(raw)
    .map(T::from_snowflake)
    .map_err(|_| ProgressError::BadId(raw))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn steps_posts_and_read_positions_are_kept() {
    let mut progress = Progress::in_memory().unwrap();
    assert!(!progress.is_done("synology:1", Step::History).unwrap());
    progress.mark_done("synology:1", Step::History).unwrap();
    progress.mark_done("synology:1", Step::History).unwrap();
    assert!(progress.is_done("synology:1", Step::History).unwrap());

    let conversation: ConversationId = "77".parse().unwrap();
    progress.set_conversation("synology:1", conversation).unwrap();
    assert_eq!(progress.conversation("synology:1").unwrap(), Some(conversation));

    let (first, second): (MessageId, MessageId) = ("100".parse().unwrap(), "101".parse().unwrap());
    progress
      .record_posts(
        1,
        &[(10, first, 1_000), (11, second, 2_000)],
        &[(11, "a.pdf".into())],
      )
      .unwrap();
    assert_eq!(progress.stored_posts(1).unwrap(), 2);
    assert_eq!(progress.message_of(11).unwrap(), Some(second));
    assert_eq!(progress.read_through(1, 1_500).unwrap(), Some(first));
    assert_eq!(progress.read_through(1, 999).unwrap(), None);
    assert_eq!(progress.missing_files(1).unwrap(), 1);

    progress.note("synology:1", 5, "waits for Anna").unwrap();
    progress.note("synology:1", 6, "waits for Anna").unwrap();
    assert_eq!(progress.notes("synology:1").unwrap(), ["waits for Anna"]);
  }
}
