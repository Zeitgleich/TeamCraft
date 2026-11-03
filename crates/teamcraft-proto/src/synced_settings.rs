//! User settings that follow the account (issue #945, e2ee.md §6.4): one entry per setting inside
//! the encrypted account data, so the server sees neither a name nor a value.
//!
//! Each setting is a last-writer-wins register ordered by a logical version, never by a clock: a
//! change takes the version after the one its device saw. A key this build doesn't know is an
//! entry like any other, so it is kept, merged and uploaded again for the newer client that wrote
//! it. An entry that can't be read at all stays as raw JSON ([`StoredSettings`]): settings travel in
//! the key bag with the keys and pins, and must never be what stops those from syncing.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};

use serde::de::Deserializer;
use serde::ser::{Error as _, SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;

use crate::DeviceId;

/// How large the encoded settings may grow through writes of a device. Today's settings take a few
/// KiB; a key bag holds up to `limits.max_key_bag_bytes` (default 1 MiB) with everything else.
pub const MAX_SYNCED_SETTINGS_BYTES: usize = 32 * 1024;
/// The version of a seed: a value a device already had when settings first synced. Only a seed
/// has it, so a device can tell somebody's existing setup from a change made on purpose.
pub const SEED_VERSION: u64 = 0;
/// How deeply a value may nest to count as readable. No setting comes close; deeper ones stay raw
/// JSON, so they can't exceed the decoders' recursion limits on the way to the UI.
const MAX_VALUE_DEPTH: usize = 16;

/// All synced settings, by key. Ordered, so that the same settings encode to the same bytes.
pub type SyncedSettings = BTreeMap<String, SettingEntry>;

/// The settings as the account data stores them: readable entries, and entries this build can't
/// read kept as they came (raw JSON, by key). Reads like its entries; those are what clients use.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoredSettings {
  pub entries: SyncedSettings,
  opaque: BTreeMap<String, String>,
}

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

impl StoredSettings {
  pub fn is_empty(&self) -> bool {
    self.entries.is_empty() && self.opaque.is_empty()
  }

  /// How many entries this build couldn't read. Their content is not for logs.
  pub fn opaque_count(&self) -> usize {
    self.opaque.len()
  }

  /// Takes the winner of every readable setting, and every unreadable one this side lacks; a
  /// readable entry replaces an unreadable one of its key. Returns whether anything changed.
  pub fn merge(&mut self, theirs: &Self) -> bool {
    let mut is_changed = merge_settings(&mut self.entries, &theirs.entries);
    for (key, raw) in &theirs.opaque {
      let is_taken = !self.entries.contains_key(key) && self.opaque.get(key).is_none_or(|mine| mine < raw);
      if is_taken {
        self.opaque.insert(key.clone(), raw.clone());
        is_changed = true;
      }
    }
    let entries = &self.entries;
    self.opaque.retain(|key, _| !entries.contains_key(key));
    is_changed
  }

  fn from_raw(raw: &RawValue) -> Self {
    let Ok(items) = serde_json::from_str::<BTreeMap<String, Box<RawValue>>>(raw.get()) else {
      // Not even a map: kept whole under the empty key, which is no setting, and counted like the
      // other unreadable entries.
      return Self {
        entries: SyncedSettings::new(),
        opaque: [(String::new(), raw.get().to_owned())].into(),
      };
    };
    let mut settings = Self::default();
    for (key, item) in items {
      match readable_entry(&item) {
        Some(entry) => {
          settings.entries.insert(key, entry);
        },
        None => {
          settings.opaque.insert(key, item.get().to_owned());
        },
      }
    }
    settings
  }
}

impl Deref for StoredSettings {
  type Target = SyncedSettings;

  fn deref(&self) -> &SyncedSettings {
    &self.entries
  }
}

impl DerefMut for StoredSettings {
  fn deref_mut(&mut self) -> &mut SyncedSettings {
    &mut self.entries
  }
}

impl From<SyncedSettings> for StoredSettings {
  fn from(entries: SyncedSettings) -> Self {
    Self {
      entries,
      opaque: BTreeMap::new(),
    }
  }
}

impl Serialize for StoredSettings {
  fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    let raw_items = self
      .opaque
      .iter()
      .filter(|(key, _)| !self.entries.contains_key(*key));
    let mut items: BTreeMap<&str, StoredItem<'_>> = self
      .entries
      .iter()
      .map(|(key, entry)| (key.as_str(), StoredItem::Entry(entry)))
      .collect();
    for (key, raw) in raw_items {
      let raw = RawValue::from_string(raw.clone()).map_err(S::Error::custom)?;
      items.insert(key, StoredItem::Raw(raw));
    }
    let mut map = serializer.serialize_map(Some(items.len()))?;
    for (key, item) in &items {
      map.serialize_entry(key, item)?;
    }
    map.end()
  }
}

impl<'de> Deserialize<'de> for StoredSettings {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    // Raw first: skipping over JSON has no recursion limit, parsing into values has.
    let raw = Box::<RawValue>::deserialize(deserializer)?;
    Ok(Self::from_raw(&raw))
  }
}

/// One stored setting on its way out.
#[derive(Serialize)]
#[serde(untagged)]
enum StoredItem<'a> {
  Entry(&'a SettingEntry),
  Raw(Box<RawValue>),
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

fn readable_entry(item: &RawValue) -> Option<SettingEntry> {
  let entry = serde_json::from_str::<SettingEntry>(item.get()).ok()?;
  (depth(&entry.value) <= MAX_VALUE_DEPTH).then_some(entry)
}

/// The nesting of a JSON value, which was decoded within the recursion limit.
fn depth(value: &Value) -> usize {
  match value {
    Value::Array(items) => 1 + items.iter().map(depth).max().unwrap_or(0),
    Value::Object(items) => 1 + items.values().map(depth).max().unwrap_or(0),
    _ => 0,
  }
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

  mod unreadable_entries {
    use super::*;

    const WITH_BAD_ENTRY: &str =
      r#"{"bad":{"version":"not a number"},"theme":{"value":"dark","version":1,"writer":"7"}}"#;

    #[test]
    fn one_bad_entry_leaves_the_others_and_is_kept_as_it_came() {
      let stored: StoredSettings = serde_json::from_str(WITH_BAD_ENTRY).unwrap();
      assert_eq!(stored["theme"].value, json!("dark"));
      assert!(!stored.contains_key("bad"));
      assert_eq!(stored.opaque_count(), 1);
      assert_eq!(serde_json::to_string(&stored).unwrap(), WITH_BAD_ENTRY);
    }

    #[test]
    fn a_value_nested_past_the_decoders_limit_is_kept_too() {
      let deep = format!("{}{}", "[".repeat(5000), "]".repeat(5000));
      let text = format!(r#"{{"deep":{{"value":{deep},"version":1,"writer":"7"}}}}"#);
      let stored: StoredSettings = serde_json::from_str(&text).unwrap();
      assert!(stored.entries.is_empty());
      assert_eq!(stored.opaque_count(), 1);
      assert_eq!(serde_json::to_string(&stored).unwrap(), text);
    }

    #[test]
    fn a_value_too_deep_for_the_way_to_the_ui_stays_out_of_the_entries() {
      let nested = format!("{}{}", "[".repeat(40), "]".repeat(40));
      let text = format!(r#"{{"deep":{{"value":{nested},"version":1,"writer":"7"}}}}"#);
      let stored: StoredSettings = serde_json::from_str(&text).unwrap();
      assert!(stored.entries.is_empty());
      assert_eq!(stored.opaque_count(), 1);
    }

    #[test]
    fn something_that_is_no_map_counts_as_unreadable_and_breaks_nothing() {
      let stored: StoredSettings = serde_json::from_str("7").unwrap();
      assert!(stored.entries.is_empty());
      assert_eq!(stored.opaque_count(), 1);
    }

    #[test]
    fn they_merge_to_the_same_on_both_sides_and_a_readable_entry_replaces_them() {
      let bad_a: StoredSettings = serde_json::from_str(r#"{"bad":{"a":1}}"#).unwrap();
      let bad_b: StoredSettings = serde_json::from_str(r#"{"bad":{"b":1}}"#).unwrap();
      let (mut left, mut right) = (bad_a.clone(), bad_b.clone());
      left.merge(&bad_b);
      right.merge(&bad_a);
      assert_eq!(left, right);

      let readable: StoredSettings = settings(&[("bad", "good", 1, 1)]).into();
      assert!(left.merge(&readable));
      assert_eq!(left.opaque_count(), 0);
      assert_eq!(left["bad"].value, json!("good"));
      assert!(!right.clone().merge(&right));
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
