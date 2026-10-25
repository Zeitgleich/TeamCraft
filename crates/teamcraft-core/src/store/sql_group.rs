//! What the SQLite stores share: groups of writes as one transaction
//! ([`super::LocalStore::begin_writes`]), numbered layout migrations, and error mapping.

use rusqlite::{Connection, ErrorCode};

use super::StoreError;

/// The group of writes open on a connection; kept under the connection's lock.
#[derive(Debug, Default)]
pub(crate) struct WriteGroup {
  /// Nesting depth; the transaction is open while it is above zero.
  depth: u32,
  /// A write of the open group failed: the group is rolled back when it ends.
  has_failed: bool,
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
    // SQLite may have rolled the transaction back by itself (a full disk, an I/O error).
    let is_open = !connection.is_autocommit();
    if self.has_failed {
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

  /// A write failed; inside a group this undoes the group.
  pub(crate) fn note_failure(&mut self) {
    if self.depth > 0 {
      self.has_failed = true;
    }
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
/// edited; a new layout appends a step (and a fixture database, store/fixtures.rs).
pub(crate) struct Migration {
  pub(crate) version: i64,
  pub(crate) apply: fn(&Connection) -> Result<(), StoreError>,
}

/// The version the last of `steps` brings a database to.
pub(crate) const fn latest_version(steps: &[Migration]) -> i64 {
  match steps.last() {
    Some(step) => step.version,
    None => 0,
  }
}

/// Brings the layout (`PRAGMA user_version`) up to the last of `steps`. Each step runs in its own
/// transaction together with setting its version, so a crash or a failed step leaves the database
/// at the last step that completed. A newer layout than this version knows is refused untouched.
pub(crate) fn run_migrations(connection: &Connection, steps: &[Migration]) -> Result<(), StoreError> {
  let supported = latest_version(steps);
  let found: i64 = connection
    .query_row("PRAGMA user_version", [], |row| row.get(0))
    .map_err(sqlite_error)?;
  if found > supported {
    return Err(StoreError::NewerSchema { found, supported });
  }
  for step in steps.iter().filter(|step| step.version > found) {
    let transaction = connection.unchecked_transaction().map_err(sqlite_error)?;
    (step.apply)(&transaction)?;
    transaction
      .execute_batch(&format!("PRAGMA user_version = {}", step.version))
      .map_err(sqlite_error)?;
    transaction.commit().map_err(sqlite_error)?;
  }
  Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
  use super::*;

  const STEPS: &[Migration] = &[
    Migration {
      version: 1,
      apply: |connection| create(connection, "one"),
    },
    Migration {
      version: 2,
      apply: |connection| create(connection, "two"),
    },
    Migration {
      version: 3,
      apply: |connection| {
        create(connection, "three")?;
        Err(StoreError::Database("step 3 fails".into()))
      },
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
      .unwrap()
  }
}
