//! A wipe of the free pages a SQLite database collected before `secure_delete` was on (issue
//! #391): old versions of the MLS storage, with deleted key packages and past epochs' secrets, may
//! still sit in them.

use rusqlite::Connection;

use super::StoreError;
use super::sql_group::sqlite_error;

/// Rebuilds the database without its free pages. Takes time proportional to the database's size, so
/// it runs as a maintenance step of the layout (`sql_group::MigrationKind::Maintenance`): once, on
/// the first start after the update. A failure (a full disk: `VACUUM` needs room for a copy) is
/// tried again on the next start; the store works either way.
pub(super) fn wipe_free_pages(connection: &Connection) -> Result<(), StoreError> {
  connection
    .execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")
    .map_err(sqlite_error)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::store::sql_group::{Migration, MigrationKind, run_migrations};

  #[test]
  fn the_wipe_is_the_step_after_the_first_layout() {
    const STEPS: &[Migration] = &[
      Migration {
        version: 1,
        kind: MigrationKind::Layout(|_| Ok(())),
      },
      Migration {
        version: 2,
        kind: MigrationKind::Maintenance(wipe_free_pages),
      },
    ];
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("PRAGMA user_version = 1").unwrap();
    run_migrations(&connection, STEPS).unwrap();
    let version: i64 = connection
      .query_row("PRAGMA user_version", [], |row| row.get(0))
      .unwrap();
    assert_eq!(version, 2);
  }
}
