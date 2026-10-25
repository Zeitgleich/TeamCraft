//! A one-time wipe of the free pages a SQLite database collected before `secure_delete` was on
//! (issue #391): old versions of the MLS storage, with deleted key packages and past epochs'
//! secrets, may still sit in them.

use rusqlite::Connection;

/// `PRAGMA user_version` once the free pages were wiped. Nothing else uses the user version.
const WIPED_VERSION: i64 = 1;

/// Rebuilds the database without its free pages, once per database. Takes time proportional to
/// the database's size, so it runs on the first start after the update only. A failure (a full
/// disk: `VACUUM` needs room for a copy) is logged and tried again on the next start; the store
/// works either way.
pub(super) fn wipe_free_pages_once(connection: &Connection) {
  let version = match connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0)) {
    Ok(version) => version,
    Err(error) => {
      tracing::warn!(%error, "could not read the local database version");
      return;
    },
  };
  if version >= WIPED_VERSION {
    return;
  }
  let wiped = connection.execute_batch(&format!(
    "VACUUM; PRAGMA wal_checkpoint(TRUNCATE); PRAGMA user_version = {WIPED_VERSION};"
  ));
  if let Err(error) = wiped {
    tracing::warn!(%error, "could not wipe the free pages of the local database");
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_wipe_runs_once() {
    let connection = Connection::open_in_memory().unwrap();
    wipe_free_pages_once(&connection);
    let version: i64 = connection
      .query_row("PRAGMA user_version", [], |row| row.get(0))
      .unwrap();
    assert_eq!(version, WIPED_VERSION);
    // Marked: a second open doesn't rebuild the database again.
    wipe_free_pages_once(&connection);
  }
}
