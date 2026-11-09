//! What each commit of the app's writing connection changed, for the app's reading connections
//! (issue #1476): a reader drops only the cache entries a commit touched, instead of everything on
//! every commit of another connection.
//!
//! Every commit through a sealed SQLite store numbers itself in the database
//! ([`CommitStamp`], `sealed_sqlite.rs`), whichever process makes it. The writing connection stages
//! the keys its group of writes touched under that number before it commits, and withdraws them if
//! the group is rolled back. A reader that finds the database at commit `n` replays every staged
//! commit since its last look; a number with no staged set (another process: the iOS notification
//! extension), one that fell out of the bounded journal, or a stamp of another writer makes it drop
//! everything, as before.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use teamcraft_proto::ConversationId;

use super::Table;

/// Commits kept at most: a reader that looks less often drops everything.
const MAX_COMMITS: usize = 1024;
/// Keys kept at most over all commits: a catch-up writes large groups, and a reader that doesn't
/// look for long must not make them pile up.
const MAX_KEYS: usize = 200_000;

/// The commits of one writing connection, shared with the readers of the same database.
#[derive(Debug, Default)]
pub struct CommitJournal {
  commits: Mutex<Commits>,
}

/// A commit's number in the database and the connection that made it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommitStamp {
  /// Raised by one with every commit.
  pub sequence: u64,
  /// Random per connection: tells the writer's commit from another process's of the same number.
  pub writer: u64,
}

/// What one group of writes changed in the caches of a sealed store.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WriteSet {
  /// Entries written or deleted.
  pub(crate) values: BTreeSet<(Table, String)>,
  /// Messages written or deleted, by conversation and client message ID.
  pub(crate) messages: BTreeSet<(ConversationId, [u8; 16])>,
  /// Conversations whose messages all went.
  pub(crate) wiped: BTreeSet<ConversationId>,
  /// Conversations whose messages or feed changed: their revision and feed version move.
  pub(crate) conversations: BTreeSet<ConversationId>,
  /// Index scopes whose nodes were written, per kind of index (messages, directory, feed, relations).
  pub(crate) scopes: [BTreeSet<String>; 4],
  /// The catalog itself changed (built, started over): nothing cached of it holds.
  pub(crate) is_everything: bool,
}

/// What a reader does after looking at the journal.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Replay {
  /// Nothing was committed since the last look.
  Unchanged,
  /// These sets cover every commit since the last look, oldest first.
  Sets(Vec<Arc<WriteSet>>),
  /// A commit isn't accounted for: drop everything.
  Unknown,
}

#[derive(Debug, Default)]
struct Commits {
  /// Staged and committed sets by commit number, with the stamp's writer.
  sets: BTreeMap<u64, (u64, Arc<WriteSet>)>,
  keys: usize,
}

impl CommitJournal {
  pub fn new() -> Arc<Self> {
    Arc::new(Self::default())
  }

  /// Stages what the group that commits as `stamp` wrote, before it commits: a reader that sees the
  /// commit finds it here.
  pub(crate) fn stage(&self, stamp: CommitStamp, written: WriteSet) {
    let mut commits = self.lock();
    let keys = written.len();
    if let Some((_, replaced)) = commits
      .sets
      .insert(stamp.sequence, (stamp.writer, Arc::new(written)))
    {
      commits.keys = commits.keys.saturating_sub(replaced.len());
    }
    commits.keys += keys;
    while commits.sets.len() > MAX_COMMITS || (commits.keys > MAX_KEYS && commits.sets.len() > 1) {
      let Some((_, (_, dropped))) = commits.sets.pop_first() else {
        break;
      };
      commits.keys = commits.keys.saturating_sub(dropped.len());
    }
  }

  /// The group staged as `sequence` was rolled back: another commit may take its number.
  pub(crate) fn withdraw(&self, sequence: u64) {
    let mut commits = self.lock();
    if let Some((_, withdrawn)) = commits.sets.remove(&sequence) {
      commits.keys = commits.keys.saturating_sub(withdrawn.len());
    }
  }

  /// What was committed after `seen` up to the database's `now`. The last commit must be this
  /// journal's own (its writer): a rolled-back number another process took is caught there, and any
  /// commit before it was staged again or withdrawn by the time the writer committed once more.
  pub(crate) fn replay(&self, seen: u64, now: CommitStamp) -> Replay {
    if now.sequence == seen {
      return Replay::Unchanged;
    }
    if now.sequence < seen {
      return Replay::Unknown;
    }
    let commits = self.lock();
    match commits.sets.get(&now.sequence) {
      Some((writer, _)) if *writer == now.writer => {},
      _ => return Replay::Unknown,
    }
    let mut sets = Vec::new();
    for sequence in seen + 1..=now.sequence {
      match commits.sets.get(&sequence) {
        Some((_, written)) => sets.push(Arc::clone(written)),
        None => return Replay::Unknown,
      }
    }
    Replay::Sets(sets)
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, Commits> {
    // Invariant-free data: at worst a reader drops everything.
    self.commits.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

impl WriteSet {
  /// How many keys it holds, for the journal's bound.
  fn len(&self) -> usize {
    self.values.len()
      + self.messages.len()
      + self.wiped.len()
      + self.conversations.len()
      + self.scopes.iter().map(BTreeSet::len).sum::<usize>()
      + 1
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::store::tests::id;

  const WRITER: u64 = 7;

  #[test]
  fn a_reader_replays_the_commits_since_its_last_look() {
    let journal = CommitJournal::new();
    journal.stage(stamp(1), set_of(1));
    journal.stage(stamp(2), set_of(2));
    assert_eq!(journal.replay(2, stamp(2)), Replay::Unchanged);
    let Replay::Sets(sets) = journal.replay(0, stamp(2)) else {
      panic!("not replayed");
    };
    assert_eq!(sets.len(), 2);
    assert!(sets[1].conversations.contains(&id::<ConversationId>(2)));
  }

  #[test]
  fn a_commit_without_a_set_or_of_another_writer_drops_everything() {
    let journal = CommitJournal::new();
    journal.stage(stamp(1), set_of(1));
    // Commit 2 came from another process: nothing staged.
    journal.stage(stamp(3), set_of(3));
    assert_eq!(journal.replay(0, stamp(3)), Replay::Unknown);
    // The writer's group 4 was rolled back and another process committed 4.
    journal.stage(stamp(4), set_of(4));
    journal.withdraw(4);
    assert_eq!(
      journal.replay(
        3,
        CommitStamp {
          sequence: 4,
          writer: 99
        }
      ),
      Replay::Unknown
    );
    // Staged but not withdrawn yet when another process committed the number.
    journal.stage(stamp(5), set_of(5));
    assert_eq!(
      journal.replay(
        4,
        CommitStamp {
          sequence: 5,
          writer: 99
        }
      ),
      Replay::Unknown
    );
  }

  #[test]
  fn a_database_behind_the_reader_drops_everything() {
    let journal = CommitJournal::new();
    assert_eq!(journal.replay(5, stamp(3)), Replay::Unknown);
  }

  #[test]
  fn the_journal_stays_bounded_and_a_slow_reader_drops_everything() {
    let journal = CommitJournal::new();
    for sequence in 1..=(MAX_COMMITS as u64 + 10) {
      journal.stage(stamp(sequence), set_of(1));
    }
    assert!(journal.lock().sets.len() <= MAX_COMMITS);
    assert_eq!(journal.replay(0, stamp(MAX_COMMITS as u64 + 10)), Replay::Unknown);
    assert!(matches!(
      journal.replay(20, stamp(MAX_COMMITS as u64 + 10)),
      Replay::Sets(_)
    ));
  }

  fn stamp(sequence: u64) -> CommitStamp {
    CommitStamp {
      sequence,
      writer: WRITER,
    }
  }

  fn set_of(conversation: i64) -> WriteSet {
    let mut set = WriteSet::default();
    set.conversations.insert(id::<ConversationId>(conversation));
    set
  }
}
