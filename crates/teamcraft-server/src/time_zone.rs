//! IANA time zones by name, from the database compiled into the server (`jiff-tzdb`), so the answer
//! doesn't depend on the host's `/usr/share/zoneinfo` (a slim container has none).
//!
//! Names are stored as the user gave them (profile time zone, quiet hours), so only the database's
//! own spelling is a name: the lookup itself ignores case, the check here doesn't.

use std::sync::LazyLock;

use jiff::tz::{TimeZone, TimeZoneDatabase};

/// The bundled database, opened once.
static DATABASE: LazyLock<TimeZoneDatabase> = LazyLock::new(TimeZoneDatabase::bundled);

/// The database's placeholder for "no zone set yet" (offset 0, abbreviation `-00`): not a place.
const PLACEHOLDER_ZONE: &str = "Factory";

/// The zone named exactly `name` (`Europe/Berlin`, `UTC`, links such as `US/Eastern`).
pub fn named(name: &str) -> Option<TimeZone> {
  if name == PLACEHOLDER_ZONE {
    return None;
  }
  DATABASE
    .get(name)
    .ok()
    .filter(|zone| zone.iana_name() == Some(name))
}

/// Whether `name` is a zone of the database, spelled as the database spells it.
pub fn is_known(name: &str) -> bool {
  named(name).is_some()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn names_resolve_in_their_own_spelling_only() {
    for name in [
      "Europe/Berlin",
      "UTC",
      "Etc/UTC",
      "US/Eastern",
      "Australia/Lord_Howe",
    ] {
      assert_eq!(
        named(name)
          .and_then(|zone| zone.iana_name().map(str::to_owned))
          .as_deref(),
        Some(name)
      );
    }
    for name in [
      "europe/berlin",
      "utc",
      "Etc/Unknown",
      "",
      "Mars/Base",
      "posix/Europe/Berlin",
      "Factory",
    ] {
      assert!(!is_known(name), "{name}");
    }
  }
}
