//! Whether and when a device downloads the shared search index segments ahead of time (e2ee.md
//! §10.1), so searching old history works without waiting. The setting lives in the local store;
//! the platform decides *when* it can run (iOS: a background processing task on power and
//! network, Android: WorkManager) and asks [`SegmentPrefetch::allows`] whether the current network
//! qualifies. The download itself is [`crate::Client::fetch_index_segments`].

use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

const SEGMENT_PREFETCH_RECORD: &str = "segment_prefetch";

/// When index segments are prefetched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentPrefetch {
  Off,
  /// Only on a network that isn't metered (Wi-Fi, ethernet).
  #[default]
  UnmeteredOnly,
  /// On any network.
  Always,
}

impl SegmentPrefetch {
  /// Whether prefetching may run on a network that is (not) metered.
  pub fn allows(self, is_network_metered: bool) -> bool {
    match self {
      Self::Off => false,
      Self::UnmeteredOnly => !is_network_metered,
      Self::Always => true,
    }
  }
}

/// The saved setting, or the default (unmetered networks only).
pub fn load_segment_prefetch(store: &impl LocalStore) -> Result<SegmentPrefetch, CoreError> {
  Ok(records::get_json(store, Table::Account, SEGMENT_PREFETCH_RECORD)?.unwrap_or_default())
}

pub fn save_segment_prefetch(store: &impl LocalStore, mode: SegmentPrefetch) -> Result<(), CoreError> {
  records::put_json(store, Table::Account, SEGMENT_PREFETCH_RECORD, &mode)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::store::MemoryStore;

  #[test]
  fn the_mode_decides_by_network() {
    assert!(!SegmentPrefetch::Off.allows(false));
    assert!(SegmentPrefetch::UnmeteredOnly.allows(false));
    assert!(!SegmentPrefetch::UnmeteredOnly.allows(true));
    assert!(SegmentPrefetch::Always.allows(true));
  }

  #[test]
  fn the_setting_round_trips() {
    let store = MemoryStore::new();
    assert_eq!(
      load_segment_prefetch(&store).unwrap(),
      SegmentPrefetch::UnmeteredOnly
    );
    save_segment_prefetch(&store, SegmentPrefetch::Off).unwrap();
    assert_eq!(load_segment_prefetch(&store).unwrap(), SegmentPrefetch::Off);
  }
}
