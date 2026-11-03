//! Keeps a device's settings and the account's copy in step (issue #945, e2ee.md §6.4): a three-way
//! merge between this device's value, the account's entry and what the two last agreed on. The
//! merge of two devices' entries is `teamcraft_proto::synced_settings`; this is what one device does
//! with the result. Platform shells say which settings exist through [`SyncedValues`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use teamcraft_proto::synced_settings::{
  SEED_VERSION, SettingStamp, SettingWrite, SettingWriteKind, SyncedSettings,
};

/// What a device and the account last agreed on, per setting.
pub type SettingBases = BTreeMap<String, SettingBase>;

/// A setting's value on this device when it last looked, and the account's write it had seen by
/// then. The value differs from the account's when this device kept its own at the first sync.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingBase {
  pub value: Value,
  /// `None` until the account's copy of this device's first value is read back.
  #[serde(rename = "stamp", default, skip_serializing_if = "Option::is_none")]
  pub maybe_stamp: Option<SettingStamp>,
  /// Found on the device when nobody could tell whose it is ([`hold_inherited`]): it stays here and
  /// isn't offered to the account, unless the user changes it.
  #[serde(rename = "held", default, skip_serializing_if = "std::ops::Not::not")]
  pub is_held: bool,
}

/// The settings of this device that follow the account.
pub trait SyncedValues {
  /// Every key this build syncs; everything else stays on the device.
  fn keys(&self) -> Vec<&'static str>;
  /// The current value as it is stored in the account. `None` only for a key that isn't synced.
  fn read(&self, key: &str) -> Option<Value>;
  /// Whether the value is what a new install has, so the user never customized it here.
  fn is_default(&self, key: &str) -> bool;
  /// Takes over the account's value; `false` if this build can't read it (a newer client's), and
  /// then nothing changed. It may read back differently (a value snapped to the nearest step).
  fn adopt(&mut self, key: &str, value: &Value) -> bool;
}

/// Marks every customized value as held: the device stays as it is, but nothing goes to the account.
/// For values whose owner isn't known, since a value in the account's key bag follows that account
/// to all its devices, and keywords are private.
pub fn hold_inherited(local: &impl SyncedValues, bases: &mut SettingBases) {
  for key in local.keys() {
    if local.is_default(key) {
      continue;
    }
    if let Some(value) = local.read(key) {
      bases.insert(
        key.to_owned(),
        SettingBase {
          value,
          maybe_stamp: None,
          is_held: true,
        },
      );
    }
  }
}

/// Brings `local` and `bases` up to date with the account's `account` and returns what the account
/// has to take from this device. Safe to run at any time and again with the same input; a write
/// that was lost is simply sent again.
///
/// - A setting changed here since the last look is a change for the account, also when the change
///   was made offline or before a restart: it takes the version after the account's.
/// - A setting the account changed is taken over, live.
/// - The first time a device meets a setting it takes the account's value only if it never
///   customized it. Customized and different: this device keeps its own and the account stays,
///   until somebody changes the setting on purpose; then that change applies everywhere.
/// - A customized setting the account has none of becomes a seed. Seeds never replace a value a
///   device already has, so two existing installs don't overwrite each other.
/// - A held value ([`hold_inherited`]) acts as customized, but never becomes a seed.
pub fn reconcile(
  local: &mut impl SyncedValues,
  account: &SyncedSettings,
  bases: &mut SettingBases,
) -> Vec<SettingWrite> {
  let mut writes = Vec::new();
  for key in local.keys() {
    let Some(mine) = local.read(key) else {
      continue;
    };
    let Some(entry) = account.get(key) else {
      if let Some(write) = seed_for_empty_account(local, key, mine, bases) {
        writes.push(write);
      }
      continue;
    };
    let stamp = entry.stamp();
    if entry.value == mine {
      settle(bases, key, mine, stamp);
      continue;
    }
    match bases.get(key) {
      None => {
        let is_taken = local.is_default(key) && local.adopt(key, &entry.value);
        let value = if is_taken { local.read(key) } else { None };
        settle(bases, key, value.unwrap_or(mine), stamp);
      },
      Some(base) if base.value != mine => writes.push(SettingWrite {
        key: key.to_owned(),
        value: mine,
        kind: SettingWriteKind::Change,
      }),
      Some(base) if base.maybe_stamp == Some(stamp) => {},
      Some(base) if base.is_held => settle(bases, key, mine, stamp),
      Some(_) => {
        // Seeds compete for the first upload; none of them is a change the user made since.
        let is_taken = entry.version != SEED_VERSION && local.adopt(key, &entry.value);
        let value = if is_taken { local.read(key) } else { None };
        settle(bases, key, value.unwrap_or(mine), stamp);
      },
    }
  }
  writes
}

/// A customized value, or a change, for a setting the account holds nothing for. A value that is
/// still the default stays out of the account.
fn seed_for_empty_account(
  local: &impl SyncedValues,
  key: &str,
  mine: Value,
  bases: &mut SettingBases,
) -> Option<SettingWrite> {
  match bases.get(key) {
    None if local.is_default(key) => return None,
    Some(base) if base.is_held && base.value == mine => return None,
    _ => {},
  }
  bases.insert(
    key.to_owned(),
    SettingBase {
      value: mine.clone(),
      maybe_stamp: None,
      is_held: false,
    },
  );
  Some(SettingWrite {
    key: key.to_owned(),
    value: mine,
    kind: SettingWriteKind::Seed,
  })
}

fn settle(bases: &mut SettingBases, key: &str, value: Value, stamp: SettingStamp) {
  bases.insert(
    key.to_owned(),
    SettingBase {
      value,
      maybe_stamp: Some(stamp),
      is_held: false,
    },
  );
}

#[cfg(test)]
mod tests {
  use serde_json::json;
  use teamcraft_proto::synced_settings::{SettingEntry, merge_settings, write_settings};
  use teamcraft_proto::{DeviceId, Id, Snowflake};

  use super::*;

  const DEFAULT: &str = "system";
  const KEYS: [&str; 2] = ["theme", "density"];
  /// Decoded by no build here.
  const UNREADABLE: &str = "a-theme-from-the-future";
  /// Reads as "light", like a stored value that snaps to the nearest step.
  const SNAPS_TO_LIGHT: &str = "almost-light";

  mod first_sync {
    use super::*;

    #[test]
    fn a_new_install_takes_the_account_settings() {
      let mut laptop = Device::new(2);
      let account = account_with("theme", "dark", 3, 1);
      assert!(laptop.sync(&account).is_empty());
      assert_eq!(laptop.value("theme"), json!("dark"));
    }

    #[test]
    fn a_customized_setting_survives_and_the_account_keeps_its_own() {
      let mut laptop = Device::new(2).with("theme", "light");
      let account = account_with("theme", "dark", 3, 1);
      assert!(laptop.sync(&account).is_empty(), "the account isn't overwritten");
      assert_eq!(laptop.value("theme"), json!("light"));
      assert!(laptop.sync(&account).is_empty(), "a second look changes nothing");
      assert_eq!(laptop.value("theme"), json!("light"));
    }

    #[test]
    fn the_next_change_on_purpose_reaches_a_device_that_kept_its_own() {
      let mut laptop = Device::new(2).with("theme", "light");
      laptop.sync(&account_with("theme", "dark", 3, 1));
      laptop.sync(&account_with("theme", "sepia", 4, 1));
      assert_eq!(laptop.value("theme"), json!("sepia"));
    }

    #[test]
    fn a_customized_setting_seeds_an_empty_account() {
      let mut phone = Device::new(1).with("theme", "dark");
      let writes = phone.sync(&SyncedSettings::new());
      assert_eq!(writes.len(), 1);
      assert_eq!(writes[0].kind, SettingWriteKind::Seed);
      assert_eq!(writes[0].value, json!("dark"));
    }

    #[test]
    fn defaults_stay_out_of_the_account() {
      let mut phone = Device::new(1);
      assert!(phone.sync(&SyncedSettings::new()).is_empty());
    }

    #[test]
    fn two_existing_installs_keep_their_settings_whoever_uploads_first() {
      let mut phone = Device::new(1).with("theme", "dark");
      let mut laptop = Device::new(2).with("theme", "light");
      let (mut phone_account, mut laptop_account) = (SyncedSettings::new(), SyncedSettings::new());
      // Both seed before either has seen the other's.
      phone.send(&mut phone_account);
      laptop.send(&mut laptop_account);
      phone.sync(&phone_account);
      laptop.sync(&laptop_account);
      let merged = merged(&phone_account, &laptop_account);
      assert!(phone.sync(&merged).is_empty());
      assert!(laptop.sync(&merged).is_empty());
      assert_eq!(phone.value("theme"), json!("dark"));
      assert_eq!(laptop.value("theme"), json!("light"));
    }

    #[test]
    fn a_seed_lost_on_the_way_goes_out_again() {
      let mut phone = Device::new(1).with("theme", "dark");
      assert_eq!(phone.sync(&SyncedSettings::new()).len(), 1);
      assert_eq!(phone.sync(&SyncedSettings::new()).len(), 1);
    }

    #[test]
    fn a_change_before_the_seed_was_read_back_is_not_undone() {
      let mut phone = Device::new(1).with("theme", "dark");
      let mut account = SyncedSettings::new();
      phone.send(&mut account);
      phone.set("theme", DEFAULT);
      let writes = phone.sync(&account);
      assert_eq!(writes[0].kind, SettingWriteKind::Change);
      assert_eq!(writes[0].value, json!(DEFAULT));
      assert_eq!(phone.value("theme"), json!(DEFAULT));
    }
  }

  mod held_values {
    use super::*;

    #[test]
    fn nothing_held_goes_to_the_account() {
      let mut device = held_device();
      assert!(device.sync(&SyncedSettings::new()).is_empty());
      assert!(device.sync(&SyncedSettings::new()).is_empty());
      assert_eq!(device.value("theme"), json!("dark"), "the device stays as it is");
    }

    #[test]
    fn a_change_by_the_user_is_theirs_and_goes_out() {
      let mut device = held_device();
      device.sync(&SyncedSettings::new());
      device.set("theme", "light");
      let writes = device.sync(&SyncedSettings::new());
      assert_eq!(writes.len(), 1);
      assert_eq!(writes[0].value, json!("light"));
    }

    #[test]
    fn the_account_doesnt_replace_a_held_value_at_first_but_later_changes_do() {
      let mut device = held_device();
      let writes = device.sync(&account_with("theme", "light", 3, 9));
      assert!(writes.is_empty());
      assert_eq!(device.value("theme"), json!("dark"));
      device.sync(&account_with("theme", "sepia", 4, 9));
      assert_eq!(device.value("theme"), json!("sepia"));
    }

    #[test]
    fn a_default_value_is_not_held_and_takes_the_account() {
      let mut device = held_device();
      device.sync(&account_with("density", "compact", 1, 9));
      assert_eq!(device.value("density"), json!("compact"));
    }

    fn held_device() -> Device {
      let mut device = Device::new(2).with("theme", "dark");
      let mut bases = SettingBases::new();
      hold_inherited(&device, &mut bases);
      device.bases = bases;
      device
    }
  }

  mod changes {
    use super::*;

    #[test]
    fn a_change_elsewhere_applies_here() {
      let mut laptop = Device::new(2);
      laptop.sync(&account_with("theme", "dark", 1, 1));
      laptop.sync(&account_with("theme", "light", 2, 1));
      assert_eq!(laptop.value("theme"), json!("light"));
    }

    #[test]
    fn a_change_here_is_sent_until_it_is_read_back() {
      let mut laptop = Device::new(2);
      let account = account_with("theme", "dark", 1, 1);
      laptop.sync(&account);
      laptop.set("theme", "light");
      let first = laptop.sync(&account);
      assert_eq!(first[0].kind, SettingWriteKind::Change);
      let again = laptop.sync(&account);
      assert_eq!(again, first, "a lost write goes out again");
      assert_eq!(laptop.value("theme"), json!("light"));
    }

    #[test]
    fn nothing_goes_back_and_forth_once_settled() {
      let mut laptop = Device::new(2);
      let mut account = account_with("theme", "dark", 1, 1);
      laptop.sync(&account);
      laptop.set("theme", "light");
      laptop.send(&mut account);
      assert!(laptop.sync(&account).is_empty());
      assert!(laptop.sync(&account).is_empty());
    }

    #[test]
    fn an_offline_change_is_kept_and_wins_over_what_arrived_meanwhile() {
      let mut laptop = Device::new(2);
      laptop.sync(&account_with("theme", "dark", 1, 1));
      laptop.set("theme", "light");
      // Another device moved on while this one couldn't reach the core or the server.
      let mut account = account_with("theme", "sepia", 5, 1);
      laptop.send(&mut account);
      assert_eq!(account["theme"].value, json!("light"));
      assert_eq!(account["theme"].version, 6);
      assert_eq!(laptop.value("theme"), json!("light"));
    }

    #[test]
    fn an_offline_change_leaves_other_settings_to_the_account() {
      let mut laptop = Device::new(2);
      let mut account = account_with("theme", "dark", 1, 1);
      account.extend(account_with("density", "compact", 1, 1));
      laptop.sync(&account);
      laptop.set("theme", "light");
      account.extend(account_with("density", "bubbles", 2, 1));
      let writes = laptop.sync(&account);
      assert_eq!(writes.len(), 1);
      assert_eq!(writes[0].key, "theme");
      assert_eq!(laptop.value("density"), json!("bubbles"));
    }

    #[test]
    fn two_devices_converge_after_changing_the_same_setting() {
      let (mut phone, mut laptop) = (Device::new(1), Device::new(2));
      let base = account_with("theme", "dark", 1, 1);
      phone.sync(&base);
      laptop.sync(&base);
      phone.set("theme", "light");
      laptop.set("theme", "sepia");
      let (mut phone_account, mut laptop_account) = (base.clone(), base);
      phone.send(&mut phone_account);
      laptop.send(&mut laptop_account);
      phone.sync(&phone_account);
      laptop.sync(&laptop_account);
      let merged = merged(&phone_account, &laptop_account);
      assert!(phone.sync(&merged).is_empty());
      assert!(laptop.sync(&merged).is_empty());
      assert_eq!(phone.value("theme"), laptop.value("theme"));
      assert_eq!(
        phone.value("theme"),
        json!("sepia"),
        "same version: the higher device"
      );
    }
  }

  mod newer_clients {
    use super::*;

    #[test]
    fn a_key_this_build_has_no_setting_for_is_left_alone() {
      let mut laptop = Device::new(2);
      let account = account_with("from.a.newer.client", "x", 1, 1);
      assert!(laptop.sync(&account).is_empty());
      assert!(!laptop.values.contains_key("from.a.newer.client"));
    }

    #[test]
    fn a_value_this_build_cant_read_keeps_the_local_one_and_is_not_overwritten() {
      let mut laptop = Device::new(2);
      laptop.sync(&account_with("theme", "dark", 1, 1));
      let account = account_with("theme", UNREADABLE, 2, 1);
      assert!(laptop.sync(&account).is_empty());
      assert_eq!(laptop.value("theme"), json!("dark"));
      assert!(
        laptop.sync(&account).is_empty(),
        "no write back of the older value"
      );
    }

    #[test]
    fn a_value_that_reads_back_differently_is_not_sent_as_a_change() {
      let mut laptop = Device::new(2);
      laptop.sync(&account_with("theme", "dark", 1, 1));
      let account = account_with("theme", SNAPS_TO_LIGHT, 2, 1);
      assert!(laptop.sync(&account).is_empty());
      assert_eq!(laptop.value("theme"), json!("light"));
      assert!(laptop.sync(&account).is_empty());
    }
  }

  mod storage {
    use super::*;

    #[test]
    fn bases_have_a_stable_encoding() {
      let mut bases = SettingBases::new();
      bases.insert(
        "theme".to_owned(),
        SettingBase {
          value: json!("dark"),
          maybe_stamp: None,
          is_held: true,
        },
      );
      bases.insert(
        "density".to_owned(),
        SettingBase {
          value: json!("compact"),
          maybe_stamp: Some(SettingStamp {
            version: 2,
            writer: device(7),
          }),
          is_held: false,
        },
      );
      let encoded = serde_json::to_string(&bases).unwrap();
      assert_eq!(
        encoded,
        r#"{"density":{"value":"compact","stamp":{"version":2,"writer":"7"}},"theme":{"value":"dark","held":true}}"#
      );
      assert_eq!(serde_json::from_str::<SettingBases>(&encoded).unwrap(), bases);
    }
  }

  struct Device {
    id: DeviceId,
    values: BTreeMap<String, Value>,
    bases: SettingBases,
  }

  impl Device {
    fn new(id: i64) -> Self {
      Self {
        id: device(id),
        values: KEYS.map(|key| (key.to_owned(), json!(DEFAULT))).into(),
        bases: SettingBases::new(),
      }
    }

    fn with(mut self, key: &str, value: &str) -> Self {
      self.set(key, value);
      self
    }

    fn set(&mut self, key: &str, value: &str) {
      self.values.insert(key.to_owned(), json!(value));
    }

    fn value(&self, key: &str) -> Value {
      self.values[key].clone()
    }

    fn sync(&mut self, account: &SyncedSettings) -> Vec<SettingWrite> {
      let mut bases = std::mem::take(&mut self.bases);
      let writes = reconcile(self, account, &mut bases);
      self.bases = bases;
      writes
    }

    /// Syncs and hands the writes to the account, as the core does.
    fn send(&mut self, account: &mut SyncedSettings) {
      let writes = self.sync(account);
      write_settings(account, &writes, self.id);
    }
  }

  impl SyncedValues for Device {
    fn keys(&self) -> Vec<&'static str> {
      KEYS.to_vec()
    }

    fn read(&self, key: &str) -> Option<Value> {
      self.values.get(key).cloned()
    }

    fn is_default(&self, key: &str) -> bool {
      self.values.get(key) == Some(&json!(DEFAULT))
    }

    fn adopt(&mut self, key: &str, value: &Value) -> bool {
      let adopted = match value.as_str() {
        Some(UNREADABLE) | None => return false,
        Some(SNAPS_TO_LIGHT) => json!("light"),
        Some(_) => value.clone(),
      };
      self.values.insert(key.to_owned(), adopted);
      true
    }
  }

  fn account_with(key: &str, value: &str, version: u64, writer: i64) -> SyncedSettings {
    [(
      key.to_owned(),
      SettingEntry {
        value: json!(value),
        version,
        writer: device(writer),
      },
    )]
    .into()
  }

  fn device(raw: i64) -> DeviceId {
    DeviceId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn merged(left: &SyncedSettings, right: &SyncedSettings) -> SyncedSettings {
    let mut merged = left.clone();
    merge_settings(&mut merged, right);
    merged
  }
}
