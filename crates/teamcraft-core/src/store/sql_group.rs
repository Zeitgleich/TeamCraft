//! What the SQLite stores share: groups of writes as one transaction
//! ([`super::LocalStore::begin_writes`]), numbered layout migrations, and error mapping.

use rusqlite::{Connection, ErrorCode};

use super::{StoreError, WriteTally};

/// The group of writes open on a connection; kept under the connection's lock.
#[derive(Debug, Default)]
pub(crate) struct WriteGroup {
  /// Nesting depth; the transaction is open while it is above zero.
  depth: u32,
  /// A write of the open group failed: the group is rolled back when it ends.
  has_failed: bool,
  /// Groups committed and lost so far ([`super::LocalStore::write_tally`]).
  tally: WriteTally,
}

impl WriteGroup {
  pub(crate) fn begin(&mut self, connection: &Connection) -> Result<(), StoreError> {
    if self.depth == 0 {
      // IMMEDIATE takes the write lock now, so the commit can't fail on a reader upgrading late.
      connection
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(sqlite_error)?;
      self.has_failed = false;
    }
    self.depth += 1;
    Ok(())
  }

  /// Commits the outermost group, or rolls it back if one of its writes failed.
  pub(crate) fn end(&mut self, connection: &Connection) -> Result<(), StoreError> {
    match self.depth {
      0 => return Ok(()),
      1 => {},
      _ => {
        self.depth -= 1;
        return Ok(());
      },
    }
    self.depth = 0;
    let ended = Self::commit_or_roll_back(self.has_failed, connection);
    if ended.is_ok() {
      self.tally.committed = self.tally.committed.saturating_add(1);
    } else {
      self.tally.lost = self.tally.lost.saturating_add(1);
    }
    ended
  }

  /// What became of the groups so far.
  pub(crate) fn tally(&self) -> WriteTally {
    self.tally
  }

  /// Before a write: inside a group, the transaction must still be open. SQLite rolls it back by
  /// itself on some errors (a full disk, an I/O error); a later write would then be committed on
  /// its own, outside the group, so it is refused and the group fails.
  pub(crate) fn check_open(&mut self, connection: &Connection) -> Result<(), StoreError> {
    if self.depth > 0 && connection.is_autocommit() {
      self.has_failed = true;
      return Err(StoreError::GroupFailed("the transaction ended early".into()));
    }
    Ok(())
  }

  /// Whether a group of writes is open on the connection.
  #[cfg_attr(not(feature = "sealed-sqlite"), allow(dead_code))]
  pub(crate) fn is_open(&self) -> bool {
    self.depth > 0
  }

  /// A write failed; inside a group this undoes the group.
  pub(crate) fn note_failure(&mut self) {
    if self.depth > 0 {
      self.has_failed = true;
    }
  }

  /// Ends the outermost group's transaction: commits it, or rolls it back if one of its writes
  /// failed.
  fn commit_or_roll_back(has_failed: bool, connection: &Connection) -> Result<(), StoreError> {
    // SQLite may have rolled the transaction back by itself (a full disk, an I/O error).
    let is_open = !connection.is_autocommit();
    if has_failed {
      if is_open {
        connection.execute_batch("ROLLBACK").map_err(sqlite_error)?;
      }
      return Err(StoreError::GroupFailed("a write of the group failed".into()));
    }
    if !is_open {
      return Err(StoreError::GroupFailed("the transaction ended early".into()));
    }
    if let Err(error) = connection.execute_batch("COMMIT") {
      // Deliberately ignored: rolling back after a failed commit only makes sure the connection
      // leaves the transaction; the commit's error is the one reported.
      let _ = connection.execute_batch("ROLLBACK");
      return Err(StoreError::GroupFailed(error.to_string()));
    }
    Ok(())
  }
}

/// SQLite's own corruption reports become [`StoreError::Corrupt`], so the app offers recovery
/// instead of a generic failure.
pub(crate) fn sqlite_error(error: rusqlite::Error) -> StoreError {
  match error.sqlite_error_code() {
    Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => StoreError::Corrupt(error.to_string()),
    _ => StoreError::Database(error.to_string()),
  }
}

/// One step of a store's layout: brings `version - 1` to `version`. Released steps are never
/// edited; a new one is appended (with a fixture database, store/fixtures.rs, if it changes the
/// layout).
#[derive(Clone, Copy)]
pub(crate) struct Migration {
  pub(crate) version: i64,
  pub(crate) kind: MigrationKind,
}

/// How a step runs.
#[derive(Clone, Copy)]
pub(crate) enum MigrationKind {
  /// Changes tables: runs in a transaction together with setting its version, so it is applied
  /// whole or not at all.
  Layout(fn(&Connection) -> Result<(), StoreError>),
  /// Upkeep that can't run in a transaction (`VACUUM`), after the earlier steps committed; the
  /// version is set only once it succeeded. It doesn't change what the store reads, so a failure
  /// (a full disk) is retried on the next start, unless a later layout step depends on it.
  Maintenance(fn(&Connection) -> Result<(), StoreError>),
  /// Changes no table but what records hold, which the client moves when it opens the store (the
  /// MLS state per entry, `mls_storage`): versions before refuse the database instead of misreading
  /// it. It depends on no upkeep, so a failed one before it is retried as usual and the marker
  /// waits for it.
  Marker,
}

/// The version the last of `steps` brings a database to.
pub(crate) const fn latest_version(steps: &[Migration]) -> i64 {
  match steps.last() {
    Some(step) => step.version,
    None => 0,
  }
}

/// Brings the layout (`PRAGMA user_version`) up to the last of `steps`, in order. A layout step
/// runs in its own transaction together with setting its version, so a crash or a failed step
/// leaves the database at the last step that completed; a maintenance step runs outside one
/// ([`MigrationKind::Maintenance`]). A newer layout than this version knows is refused untouched.
pub(crate) fn run_migrations(connection: &Connection, steps: &[Migration]) -> Result<(), StoreError> {
  let supported = latest_version(steps);
  let found = user_version(connection)?;
  if found > supported {
    return Err(StoreError::NewerSchema { found, supported });
  }
  for (index, step) in steps.iter().enumerate().filter(|(_, step)| step.version > found) {
    match step.kind {
      MigrationKind::Layout(apply) => {
        let transaction = connection.unchecked_transaction().map_err(sqlite_error)?;
        apply(&transaction)?;
        set_user_version(&transaction, step.version)?;
        transaction.commit().map_err(sqlite_error)?;
      },
      MigrationKind::Marker => set_user_version(connection, step.version)?,
      MigrationKind::Maintenance(apply) => {
        if let Err(error) = apply(connection).and_then(|()| set_user_version(connection, step.version)) {
          let is_needed_later = steps[index + 1..]
            .iter()
            .any(|later| matches!(later.kind, MigrationKind::Layout(_)));
          if error.is_unreadable() || is_needed_later {
            return Err(error);
          }
          // Steps run in order: the later maintenance steps wait for this one too.
          tracing::warn!(%error, version = step.version, "local database upkeep failed; trying again on the next start");
          return Ok(());
        }
      },
    }
  }
  Ok(())
}

fn user_version(connection: &Connection) -> Result<i64, StoreError> {
  connection
    .query_row("PRAGMA user_version", [], |row| row.get(0))
    .map_err(sqlite_error)
}

fn set_user_version(connection: &Connection, version: i64) -> Result<(), StoreError> {
  connection
    .execute_batch(&format!("PRAGMA user_version = {version}"))
    .map_err(sqlite_error)
}

#[cfg(test)]
pub(crate) mod tests {
  use super::*;

  const STEPS: &[Migration] = &[
    Migration {
      version: 1,
      kind: MigrationKind::Layout(|connection| create(connection, "one")),
    },
    Migration {
      version: 2,
      kind: MigrationKind::Layout(|connection| create(connection, "two")),
    },
    Migration {
      version: 3,
      kind: MigrationKind::Layout(|connection| {
        create(connection, "three")?;
        Err(StoreError::Database("step 3 fails".into()))
      }),
    },
  ];

  /// Upkeep that works once the table `allowed` exists (a full disk until then).
  const WITH_UPKEEP: &[Migration] = &[
    Migration {
      version: 1,
      kind: MigrationKind::Layout(|connection| create(connection, "one")),
    },
    Migration {
      version: 2,
      kind: MigrationKind::Maintenance(|connection| {
        if has_table(connection, "allowed") {
          connection.execute_batch("VACUUM").map_err(sqlite_error)
        } else {
          Err(StoreError::Database("disk full".into()))
        }
      }),
    },
  ];

  /// A store's steps go 1, 2, 3, … without gaps or repeats.
  pub(crate) fn assert_numbered(steps: &[Migration]) {
    for (index, step) in steps.iter().enumerate() {
      assert_eq!(step.version, index as i64 + 1);
    }
  }

  #[test]
  fn each_step_commits_on_its_own_and_a_failed_one_leaves_the_last_good_version() {
    assert_numbered(STEPS);
    let connection = Connection::open_in_memory().unwrap();
    let failed = run_migrations(&connection, STEPS);
    assert!(failed.is_err());
    assert_eq!(version(&connection), 2);
    assert!(has_table(&connection, "two"));
    assert!(
      !has_table(&connection, "three"),
      "the failed step was rolled back"
    );
    // Run again: only the missing step, which fails again.
    assert!(run_migrations(&connection, STEPS).is_err());
    run_migrations(&connection, &STEPS[..2]).unwrap();
    assert_eq!(version(&connection), 2);
    let newer = run_migrations(&connection, &STEPS[..1]);
    assert!(matches!(
      newer,
      Err(StoreError::NewerSchema {
        found: 2,
        supported: 1
      })
    ));
  }

  #[test]
  fn failed_upkeep_is_tried_again_on_the_next_start_and_the_store_opens() {
    assert_numbered(WITH_UPKEEP);
    let connection = Connection::open_in_memory().unwrap();
    run_migrations(&connection, WITH_UPKEEP).unwrap();
    assert_eq!(version(&connection), 1, "not marked done");
    create(&connection, "allowed").unwrap();
    run_migrations(&connection, WITH_UPKEEP).unwrap();
    assert_eq!(version(&connection), 2);
    // A later layout step that depends on it makes a failure an error.
    let connection = Connection::open_in_memory().unwrap();
    let with_layout_after: Vec<Migration> = WITH_UPKEEP
      .iter()
      .copied()
      .chain([Migration {
        version: 3,
        kind: MigrationKind::Layout(|connection| create(connection, "three")),
      }])
      .collect();
    assert!(run_migrations(&connection, &with_layout_after).is_err());
    assert_eq!(version(&connection), 1);
  }

  /// A marker (issue #424) waits for a failed upkeep before it instead of failing the open.
  #[test]
  fn a_marker_after_failed_upkeep_waits_for_it() {
    let with_marker: Vec<Migration> = WITH_UPKEEP
      .iter()
      .copied()
      .chain([Migration {
        version: 3,
        kind: MigrationKind::Marker,
      }])
      .collect();
    assert_numbered(&with_marker);
    let connection = Connection::open_in_memory().unwrap();
    run_migrations(&connection, &with_marker).unwrap();
    assert_eq!(version(&connection), 1);
    create(&connection, "allowed").unwrap();
    run_migrations(&connection, &with_marker).unwrap();
    assert_eq!(version(&connection), 3);
  }

  fn create(connection: &Connection, name: &str) -> Result<(), StoreError> {
    connection
      .execute_batch(&format!("CREATE TABLE {name} (x INTEGER)"))
      .map_err(sqlite_error)
  }

  fn version(connection: &Connection) -> i64 {
    connection
      .query_row("PRAGMA user_version", [], |row| row.get(0))
      .unwrap()
  }

  fn has_table(connection: &Connection, name: &str) -> bool {
    connection
      .prepare("SELECT 1 FROM sqlite_master WHERE name = ?1")
      .and_then(|mut statement| statement.exists([name]))
      .unwrap_or(false)
  }
}
