//! Groups of writes on a SQLite connection as one transaction ([`super::LocalStore::begin_writes`]).

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
