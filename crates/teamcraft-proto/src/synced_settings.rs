//! User settings that follow the account (issue #945, e2ee.md §6.4): one entry per setting inside
//! the encrypted account data, so the server sees neither a name nor a value.
//!
//! Each setting is a last-writer-wins register ordered by a logical version, never by a clock: a
//! change takes the version after the one its device saw. A key this build doesn't know is an
//! entry like any other, so it is kept, merged and uploaded again for the newer client that wrote
//! it.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::DeviceId;

/// How large the encoded settings may grow through writes of a device. Today's settings take a few
/// KiB; a key bag holds up to `limits.max_key_bag_bytes` (default 1 MiB) with everything else.
pub const MAX_SYNCED_SETTINGS_BYTES: usize = 32 * 1024;
/// The version of a seed: a value a device already had when settings first synced. Only a seed
/// has it, so a device can tell somebody's existing setup from a change made on purpose.
pub const SEED_VERSION: u64 = 0;

/// All synced settings, by key. Ordered, so that the same settings encode to the same bytes.
pub type SyncedSettings = BTreeMap<String, SettingEntry>;

/// A setting's value and who wrote which version of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingEntry {
  /// Opaque JSON here: only the client that reads a key knows its type.
  pub value: Value,
  pub version: u64,
  pub writer: DeviceId,
}

/// Which write of a setting a device last saw.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingStamp {
  pub version: u64,
  pub writer: DeviceId,
}

/// A change a device wants to make to a setting.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingWrite {
  pub key: String,
  pub value: Value,
  pub kind: SettingWriteKind,
}

/// Whether a write replaces what the account holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingWriteKind {
  /// This device's value for a setting the account has none of. Does nothing if it has one by now.
  Seed,
  /// The user changed the setting: wins over every version this device saw.
  Change,
}

/// What [`write_settings`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsWritten {
  pub is_changed: bool,
  /// Keys whose new value would take the settings over [`MAX_SYNCED_SETTINGS_BYTES`]; their entries
  /// stay as they were.
  pub refused: Vec<String>,
}

impl SettingEntry {
  pub fn stamp(&self) -> SettingStamp {
    SettingStamp {
      version: self.version,
      writer: self.writer,
    }
  }

  /// The merge order: the higher version, then the writer, then the value (two writes can only
  /// share version and writer after a device's store was restored). A total order, so devices
  /// agree whatever order they see the writes in.
  pub fn order_against(&self, other: &Self) -> Ordering {
    self
      .version
      .cmp(&other.version)
      .then_with(|| self.writer.cmp(&other.writer))
      .then_with(|| self.value.to_string().cmp(&other.value.to_string()))
  }
}

/// Takes the winner of every setting from `theirs`; returns whether that changed anything. Entries
/// are never dropped: a merge of two bounded sides may exceed the bound, and then only writes that
/// add to it are refused.
pub fn merge_settings(mine: &mut SyncedSettings, theirs: &SyncedSettings) -> bool {
  let mut is_changed = false;
  for (key, their_entry) in theirs {
    let is_taken = mine
      .get(key)
      .is_none_or(|my_entry| their_entry.order_against(my_entry) == Ordering::Greater);
    if is_taken {
      mine.insert(key.clone(), their_entry.clone());
      is_changed = true;
    }
  }
  is_changed
}

/// Applies a device's writes, within [`MAX_SYNCED_SETTINGS_BYTES`].
pub fn write_settings(
  settings: &mut SyncedSettings,
  writes: &[SettingWrite],
  writer: DeviceId,
) -> SettingsWritten {
  let mut written = SettingsWritten::default();
  for write in writes {
    let Some(entry) = written_entry(settings.get(&write.key), write, writer) else {
      continue;
    };
    let size_before = encoded_size(settings);
    let maybe_previous = settings.insert(write.key.clone(), entry);
    let size_after = encoded_size(settings);
    if size_after > MAX_SYNCED_SETTINGS_BYTES && size_after > size_before {
      match maybe_previous {
        Some(previous) => settings.insert(write.key.clone(), previous),
        None => settings.remove(&write.key),
      };
      written.refused.push(write.key.clone());
    } else {
      written.is_changed = true;
    }
  }
  written
}

/// The entry a write leaves, or `None` when it changes nothing. A value the account already holds
/// keeps its version: sending a change again after a lost answer is harmless.
fn written_entry(
  maybe_current: Option<&SettingEntry>,
  write: &SettingWrite,
  writer: DeviceId,
) -> Option<SettingEntry> {
  let version = match (maybe_current, write.kind) {
    (Some(_), SettingWriteKind::Seed) => return None,
    (Some(current), SettingWriteKind::Change) if current.value == write.value => return None,
    (Some(current), SettingWriteKind::Change) => current.version.saturating_add(1),
    (None, SettingWriteKind::Seed) => SEED_VERSION,
    (None, SettingWriteKind::Change) => SEED_VERSION + 1,
  };
  Some(SettingEntry {
    value: write.value.clone(),
    version,
    writer,
  })
}

fn encoded_size(settings: &SyncedSettings) -> usize {
  // Strings, numbers and JSON values always encode; nothing to count otherwise.
  serde_json::to_vec(settings).map_or(0, |bytes| bytes.len())
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;
  use crate::Id;

  mod merging {
    use super::*;

    #[test]
    fn the_higher_version_wins_in_both_directions() {
      let newer = settings(&[("theme", "dark", 3, 1)]);
      let older = settings(&[("theme", "light", 2, 9)]);
      let mut left = newer.clone();
      assert!(!merge_settings(&mut left, &older));
      assert_eq!(left, newer);
      let mut right = older;
      assert!(merge_settings(&mut right, &newer));
      assert_eq!(right, newer);
    }

    #[test]
    fn equal_versions_settle_on_the_same_writer_either_way() {
      let first = settings(&[("theme", "dark", 2, 1)]);
      let second = settings(&[("theme", "light", 2, 2)]);
      let (mut left, mut right) = (first.clone(), second.clone());
      merge_settings(&mut left, &second);
      merge_settings(&mut right, &first);
      assert_eq!(left, right);
      assert_eq!(left["theme"].value, json!("light"));
    }

    #[test]
    fn each_setting_merges_on_its_own() {
      let mut left = settings(&[("theme", "dark", 5, 1), ("density", "compact", 1, 1)]);
      let right = settings(&[("theme", "light", 1, 2), ("density", "bubbles", 2, 2)]);
      merge_settings(&mut left, &right);
      assert_eq!(left["theme"].value, json!("dark"));
      assert_eq!(left["density"].value, json!("bubbles"));
    }

    #[test]
    fn a_key_nobody_here_knows_is_kept() {
      let mut left = settings(&[("theme", "dark", 1, 1)]);
      let right = settings(&[("from.a.newer.client", "x", 7, 2)]);
      assert!(merge_settings(&mut left, &right));
      assert_eq!(left["from.a.newer.client"].value, json!("x"));
    }

    #[test]
    fn merging_again_changes_nothing() {
      let theirs = settings(&[("theme", "dark", 1, 1)]);
      let mut mine = theirs.clone();
      assert!(!merge_settings(&mut mine, &theirs));
    }
  }

  mod writing {
    use super::*;

    #[test]
    fn a_change_goes_past_the_version_the_account_holds() {
      let mut held = settings(&[("theme", "dark", 4, 1)]);
      let written = write_settings(&mut held, &[change("theme", "light")], device(2));
      assert!(written.is_changed);
      assert_eq!(held["theme"].stamp().version, 5);
      assert_eq!(held["theme"].writer, device(2));
    }

    #[test]
    fn sending_the_same_value_again_keeps_the_version() {
      let mut held = settings(&[("theme", "dark", 4, 1)]);
      let written = write_settings(&mut held, &[change("theme", "dark")], device(2));
      assert!(!written.is_changed);
      assert_eq!(held["theme"].stamp().version, 4);
    }

    #[test]
    fn a_seed_only_fills_an_empty_place() {
      let mut held = SyncedSettings::new();
      write_settings(&mut held, &[seed("theme", "dark")], device(1));
      assert_eq!(held["theme"].version, SEED_VERSION);
      let written = write_settings(&mut held, &[seed("theme", "light")], device(2));
      assert!(!written.is_changed);
      assert_eq!(held["theme"].value, json!("dark"));
    }

    #[test]
    fn a_change_of_a_new_setting_outranks_seeds() {
      let mut held = SyncedSettings::new();
      write_settings(&mut held, &[change("theme", "dark")], device(1));
      assert!(held["theme"].version > SEED_VERSION);
    }
  }

  mod size_bound {
    use super::*;

    #[test]
    fn a_value_over_the_bound_is_refused_and_the_old_one_stays() {
      let mut held = settings(&[("keywords", "short", 1, 1)]);
      let huge = "x".repeat(MAX_SYNCED_SETTINGS_BYTES);
      let written = write_settings(
        &mut held,
        &[change("keywords", &huge), change("theme", "dark")],
        device(1),
      );
      assert_eq!(written.refused, ["keywords"]);
      assert!(written.is_changed, "what fits is still written");
      assert_eq!(held["keywords"].value, json!("short"));
      assert_eq!(held["theme"].value, json!("dark"));
    }

    #[test]
    fn a_new_key_over_the_bound_leaves_no_entry() {
      let mut held = SyncedSettings::new();
      let huge = "x".repeat(MAX_SYNCED_SETTINGS_BYTES);
      let written = write_settings(&mut held, &[seed("keywords", &huge)], device(1));
      assert_eq!(written.refused, ["keywords"]);
      assert!(!written.is_changed);
      assert!(held.is_empty());
    }

    #[test]
    fn settings_over_the_bound_after_a_merge_can_still_shrink() {
      let huge = "x".repeat(MAX_SYNCED_SETTINGS_BYTES);
      let mut held = settings(&[("keywords", &huge, 1, 1)]);
      let written = write_settings(&mut held, &[change("keywords", "short")], device(2));
      assert!(written.refused.is_empty());
      assert_eq!(held["keywords"].value, json!("short"));
    }
  }

  mod wire {
    use super::*;

    /// What older key bags hold must keep reading, and devices must keep writing the same shape.
    #[test]
    fn an_entry_has_a_stable_encoding() {
      let held = settings(&[("display.density", "compact", 3, 42)]);
      let encoded = serde_json::to_string(&held).unwrap();
      assert_eq!(
        encoded,
        r#"{"display.density":{"value":"compact","version":3,"writer":"42"}}"#
      );
      let decoded: SyncedSettings = serde_json::from_str(&encoded).unwrap();
      assert_eq!(decoded, held);
    }

    #[test]
    fn write_kinds_have_stable_names() {
      assert_eq!(
        serde_json::to_string(&SettingWriteKind::Seed).unwrap(),
        r#""seed""#
      );
      assert_eq!(
        serde_json::to_string(&SettingWriteKind::Change).unwrap(),
        r#""change""#
      );
    }
  }

  fn settings(entries: &[(&str, &str, u64, i64)]) -> SyncedSettings {
    entries
      .iter()
      .map(|(key, value, version, writer)| {
        (
          (*key).to_owned(),
          SettingEntry {
            value: json!(value),
            version: *version,
            writer: device(*writer),
          },
        )
      })
      .collect()
  }

  fn change(key: &str, value: &str) -> SettingWrite {
    SettingWrite {
      key: key.to_owned(),
      value: json!(value),
      kind: SettingWriteKind::Change,
    }
  }

  fn seed(key: &str, value: &str) -> SettingWrite {
    SettingWrite {
      key: key.to_owned(),
      value: json!(value),
      kind: SettingWriteKind::Seed,
    }
  }

  fn device(raw: i64) -> DeviceId {
    DeviceId::from_snowflake(crate::Snowflake::new(raw).unwrap())
  }
}
