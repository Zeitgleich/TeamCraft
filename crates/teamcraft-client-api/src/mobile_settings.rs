//! The settings the mobile apps share with the account (issue #945, e2ee.md §6.4): one typed record
//! the shells fill from their own stores, mapped onto the same keys and values web and desktop use
//! (`teamcraft-ui` `settings_sync`), so a change on any client reaches the others. The three-way
//! merge is [`reconcile`]; this module is what a mobile device feeds it and keeps between runs.
//!
//! What a phone keeps for itself stays out of [`MobileSettings`]: light, dark or as the system
//! (on every client; the colour theme, #202, and the font, #1494, do sync), the send key (Enter makes a new
//! line on a touch keyboard), the language (the system's per-app language), the wallpaper colors,
//! how much a notification shows (capped by the app lock), media downloads and send quality (the data
//! plan), and audio and video devices. Notification levels, mutes and do not disturb live on the
//! server, which enforces them, so they already follow the account.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use teamcraft_proto::UserId;
use teamcraft_proto::synced_settings::{SettingWrite, SyncedSettings};

use crate::calls::CallStatusSounds;
use crate::color_theme::ColorTheme;
use crate::font::UiFont;
use crate::settings_sync::{SettingBases, SyncedValues, keep_on_device, reconcile};

pub const KEY_LIST_LAYOUT: &str = "sidebar.layout";
pub const KEY_LIST_GROUPING: &str = "sidebar.list_grouping";
pub const KEY_ANIMATED_IMAGES: &str = "display.animated_images";
pub const KEY_SHARE_TYPING: &str = "privacy.share_typing";
pub const KEY_FIRST_NAME_IS_MENTION: &str = "notifications.first_name_is_mention";
pub const KEY_CHANNEL_MENTIONS_NOTIFY: &str = "notifications.channel_mentions_notify";
pub const KEY_KEYWORDS: &str = "notifications.keywords";
pub const KEY_CONFIRM_CHANNEL_OVER: &str = "notifications.confirm_channel_over";
pub const KEY_CALL_STATUS_SOUNDS: &str = "calls.status_sounds";
pub const KEY_COLOR_THEME: &str = "display.color_theme";
pub const KEY_FONT: &str = "display.font";

/// Every account key the mobile apps read and write, in the order of [`MobileSettings`].
pub const MOBILE_KEYS: [&str; 11] = [
  KEY_LIST_LAYOUT,
  KEY_LIST_GROUPING,
  KEY_ANIMATED_IMAGES,
  KEY_SHARE_TYPING,
  KEY_FIRST_NAME_IS_MENTION,
  KEY_CHANNEL_MENTIONS_NOTIFY,
  KEY_KEYWORDS,
  KEY_CONFIRM_CHANNEL_OVER,
  KEY_CALL_STATUS_SOUNDS,
  KEY_COLOR_THEME,
  KEY_FONT,
];

/// Lists that say what a person watches for: an upload carries everything in them, so values of
/// unknown origin never go to the account (as on web, `PRIVATE_KEYS`).
const PRIVATE_KEYS: [&str; 1] = [KEY_KEYWORDS];

/// The settings of a phone that follow the account, with the phone's defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MobileSettings {
  pub list_layout: ListLayout,
  pub list_grouping: ListGrouping,
  pub animated_images: AnimatedImages,
  pub shares_typing: bool,
  pub first_name_is_mention: bool,
  pub channel_mentions_notify: bool,
  pub keywords: Vec<String>,
  /// Ask before `@channel` in conversations with more members than this; 0 never asks.
  pub confirm_channel_over: u32,
  pub call_status_sounds: CallStatusSounds,
  /// The colours of the chrome and accent (#202); light or dark stays the device's.
  pub color_theme: ColorTheme,
  /// The interface font (#1494).
  pub font: UiFont,
}

/// How the conversations are listed. Wire values as `teamcraft-ui` `SidebarLayout`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListLayout {
  /// DMs on Home, channels by space.
  #[default]
  Spaces,
  /// DMs and channels in one list.
  List,
}

/// How the one list orders its channels. Wire values as `teamcraft-ui` `ListGrouping`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListGrouping {
  #[default]
  Mixed,
  BySpace,
}

/// When animated images move. Web's "on hover" has no touch equivalent; a phone keeps its own
/// value when the account holds it. Wire values as `teamcraft-ui` `AnimatedImages`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnimatedImages {
  #[default]
  Autoplay,
  OnTap,
}

/// What a device keeps between runs, as an opaque text the shell stores and hands back.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MobileSyncState {
  #[serde(rename = "user")]
  maybe_user: Option<UserId>,
  bases: SettingBases,
}

/// The outcome of [`sync_mobile_settings`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MobileSync {
  /// What the device should hold now; the shell stores the values that differ.
  pub settings: MobileSettings,
  /// Changes for the account (the core's `write_settings`).
  pub writes: Vec<SettingWrite>,
  pub state: MobileSyncState,
}

impl Default for MobileSettings {
  fn default() -> Self {
    Self {
      list_layout: ListLayout::Spaces,
      list_grouping: ListGrouping::Mixed,
      animated_images: AnimatedImages::Autoplay,
      shares_typing: true,
      first_name_is_mention: true,
      channel_mentions_notify: true,
      keywords: Vec::new(),
      confirm_channel_over: 10,
      call_status_sounds: CallStatusSounds::All,
      color_theme: ColorTheme::Brand,
      font: UiFont::System,
    }
  }
}

impl MobileSyncState {
  /// Reads what [`Self::encode`] wrote; anything unreadable starts afresh, which only means the
  /// next sync is a first sync again (customized values are kept, never overwritten).
  pub fn decode(text: &str) -> Self {
    if text.is_empty() {
      return Self::default();
    }
    serde_json::from_str(text).unwrap_or_else(|error| {
      tracing::warn!(%error, "settings sync state unreadable, starting afresh");
      Self::default()
    })
  }

  pub fn encode(&self) -> String {
    // Invariant: a struct of strings, numbers and maps always serializes.
    serde_json::to_string(self).unwrap_or_default()
  }
}

/// Brings a phone's settings and the account's in step: the rules of [`reconcile`], plus whose the
/// values on the device are.
///
/// - Another account than the recorded one: this account starts from the defaults, as on a new
///   install, and takes the account's values.
/// - No account recorded (the first run after the update): the values are this person's, since the
///   mobile apps hold one account at a time, and customized ones are offered to the account. The
///   keywords stay on this device only until a change in the account replaces them, as on web.
pub fn sync_mobile_settings(
  local: MobileSettings,
  account: &SyncedSettings,
  mut state: MobileSyncState,
  user: UserId,
) -> MobileSync {
  let mut local = local;
  match state.maybe_user {
    Some(owner) if owner == user => {},
    Some(_) => {
      local = MobileSettings::default();
      state.bases.clear();
    },
    None => keep_on_device(&local, &PRIVATE_KEYS, &mut state.bases),
  }
  state.maybe_user = Some(user);
  let writes = reconcile(&mut local, account, &mut state.bases);
  MobileSync {
    settings: local,
    writes,
    state,
  }
}

impl SyncedValues for MobileSettings {
  fn keys(&self) -> Vec<&'static str> {
    MOBILE_KEYS.to_vec()
  }

  fn read(&self, key: &str) -> Option<Value> {
    let value = match key {
      KEY_LIST_LAYOUT => serde_json::to_value(self.list_layout).ok()?,
      KEY_LIST_GROUPING => serde_json::to_value(self.list_grouping).ok()?,
      KEY_ANIMATED_IMAGES => serde_json::to_value(self.animated_images).ok()?,
      KEY_SHARE_TYPING => json!(self.shares_typing),
      KEY_FIRST_NAME_IS_MENTION => json!(self.first_name_is_mention),
      KEY_CHANNEL_MENTIONS_NOTIFY => json!(self.channel_mentions_notify),
      KEY_KEYWORDS => json!(self.keywords),
      KEY_CONFIRM_CHANNEL_OVER => json!(self.confirm_channel_over),
      KEY_CALL_STATUS_SOUNDS => serde_json::to_value(self.call_status_sounds).ok()?,
      KEY_COLOR_THEME => serde_json::to_value(&self.color_theme).ok()?,
      KEY_FONT => serde_json::to_value(&self.font).ok()?,
      _ => return None,
    };
    Some(value)
  }

  fn is_default(&self, key: &str) -> bool {
    self.read(key) == Self::default().read(key)
  }

  fn adopt(&mut self, key: &str, value: &Value) -> bool {
    match key {
      KEY_LIST_LAYOUT => assign(&mut self.list_layout, value),
      KEY_LIST_GROUPING => assign(&mut self.list_grouping, value),
      KEY_ANIMATED_IMAGES => assign(&mut self.animated_images, value),
      KEY_SHARE_TYPING => assign(&mut self.shares_typing, value),
      KEY_FIRST_NAME_IS_MENTION => assign(&mut self.first_name_is_mention, value),
      KEY_CHANNEL_MENTIONS_NOTIFY => assign(&mut self.channel_mentions_notify, value),
      KEY_KEYWORDS => assign(&mut self.keywords, value),
      KEY_CONFIRM_CHANNEL_OVER => assign(&mut self.confirm_channel_over, value),
      // "unknown" is the catch-all's own name, never a value a client writes.
      KEY_CALL_STATUS_SOUNDS => value != &json!("unknown") && assign(&mut self.call_status_sounds, value),
      // A newer client's theme keeps the one this device shows.
      KEY_COLOR_THEME => {
        let is_known = value
          .as_str()
          .is_some_and(|text| !matches!(text.parse(), Ok(ColorTheme::Unknown(_))));
        is_known && assign(&mut self.color_theme, value)
      },
      // A newer client's font keeps the one this device shows.
      KEY_FONT => {
        let is_known = value
          .as_str()
          .is_some_and(|text| !matches!(text.parse(), Ok(UiFont::Unknown(_))));
        is_known && assign(&mut self.font, value)
      },
      _ => false,
    }
  }
}

/// Takes `value` only if it reads back exactly: an enum's catch-all (an unknown name read as
/// `Unknown`) or a newer client's value must leave the local one in place.
fn assign<T: Serialize + for<'de> Deserialize<'de>>(place: &mut T, value: &Value) -> bool {
  match serde_json::from_value::<T>(value.clone()) {
    Ok(parsed) if serde_json::to_value(&parsed).ok().as_ref() == Some(value) => {
      *place = parsed;
      true
    },
    _ => false,
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::synced_settings::{SettingWriteKind, write_settings};
  use teamcraft_proto::{DeviceId, Id as _, Snowflake};

  use super::*;

  const ME: i64 = 1;
  const SOMEONE_ELSE: i64 = 2;
  const PHONE: i64 = 10;
  const LAPTOP: i64 = 20;

  mod wire_values {
    use super::*;

    /// The values must be the ones web and desktop write, or a change wouldn't cross over.
    #[test]
    fn match_what_web_and_desktop_write() {
      let settings = MobileSettings {
        list_layout: ListLayout::List,
        list_grouping: ListGrouping::BySpace,
        animated_images: AnimatedImages::OnTap,
        shares_typing: false,
        first_name_is_mention: false,
        channel_mentions_notify: false,
        keywords: vec!["release".into()],
        confirm_channel_over: 25,
        call_status_sounds: CallStatusSounds::OwnOnly,
        color_theme: "preset:pine".parse().unwrap(),
        font: UiFont::Lexend,
      };
      let read = |key| settings.read(key).unwrap();
      assert_eq!(read(KEY_LIST_LAYOUT), json!("list"));
      assert_eq!(read(KEY_LIST_GROUPING), json!("by_space"));
      assert_eq!(read(KEY_ANIMATED_IMAGES), json!("on_tap"));
      assert_eq!(read(KEY_SHARE_TYPING), json!(false));
      assert_eq!(read(KEY_KEYWORDS), json!(["release"]));
      assert_eq!(read(KEY_CONFIRM_CHANNEL_OVER), json!(25));
      assert_eq!(read(KEY_CALL_STATUS_SOUNDS), json!("own_only"));
      assert_eq!(read(KEY_COLOR_THEME), json!("preset:pine"));
      assert_eq!(read(KEY_FONT), json!("lexend"));
    }

    #[test]
    fn every_key_round_trips() {
      let changed = MobileSettings {
        list_layout: ListLayout::List,
        keywords: vec!["a".into(), "b".into()],
        call_status_sounds: CallStatusSounds::Off,
        color_theme: "custom:#1b2233,#2f66e0,#2eb67d,#d1453b".parse().unwrap(),
        font: UiFont::Inter,
        ..MobileSettings::default()
      };
      let mut copy = MobileSettings::default();
      for key in MOBILE_KEYS {
        assert!(copy.adopt(key, &changed.read(key).unwrap()), "{key}");
      }
      assert_eq!(copy, changed);
    }

    /// A newer client's value, or one a phone can't show, leaves the phone's own in place.
    #[test]
    fn values_this_build_cannot_read_are_refused() {
      let mut local = MobileSettings::default();
      assert!(!local.adopt(KEY_ANIMATED_IMAGES, &json!("on_hover")));
      assert!(!local.adopt(KEY_CALL_STATUS_SOUNDS, &json!("only_on_weekdays")));
      assert!(!local.adopt(KEY_CALL_STATUS_SOUNDS, &json!("unknown")));
      assert!(!local.adopt(KEY_LIST_LAYOUT, &json!(3)));
      assert!(!local.adopt(KEY_CONFIRM_CHANNEL_OVER, &json!(-1)));
      assert!(!local.adopt("display.density", &json!("compact")));
      assert!(!local.adopt(KEY_COLOR_THEME, &json!("preset:from-the-future")));
      assert!(!local.adopt(KEY_COLOR_THEME, &json!(1)));
      assert!(!local.adopt(KEY_FONT, &json!("font-from-the-future")));
      assert!(!local.adopt(KEY_FONT, &json!(2)));
      assert_eq!(local, MobileSettings::default());
    }

    #[test]
    fn state_round_trips_and_garbage_starts_afresh() {
      let outcome = sync_mobile_settings(
        customized(),
        &SyncedSettings::new(),
        MobileSyncState::default(),
        user(ME),
      );
      let text = outcome.state.encode();
      assert_eq!(MobileSyncState::decode(&text), outcome.state);
      assert_eq!(MobileSyncState::decode("not json"), MobileSyncState::default());
      assert_eq!(MobileSyncState::decode(""), MobileSyncState::default());
    }
  }

  mod sync {
    use super::*;

    #[test]
    fn a_change_on_the_laptop_reaches_the_phone() {
      let mut account = SyncedSettings::new();
      let (phone, state) = first_sync(MobileSettings::default(), &mut account);
      write_settings(
        &mut account,
        &[change(KEY_LIST_LAYOUT, json!("list"))],
        device(LAPTOP),
      );

      let outcome = sync_mobile_settings(phone, &account, state, user(ME));

      assert_eq!(outcome.settings.list_layout, ListLayout::List);
      assert!(outcome.writes.is_empty());
    }

    /// The account holds nothing for it yet: the change goes in as a seed, which is enough.
    #[test]
    fn a_first_change_on_the_phone_goes_to_the_account() {
      let mut account = SyncedSettings::new();
      let (mut phone, state) = first_sync(MobileSettings::default(), &mut account);
      phone.call_status_sounds = CallStatusSounds::Off;

      let outcome = sync_mobile_settings(phone, &account, state, user(ME));
      write_settings(&mut account, &outcome.writes, device(PHONE));

      assert_eq!(account[KEY_CALL_STATUS_SOUNDS].value, json!("off"));
    }

    /// Made while offline, after the laptop changed the same setting: the change waits in the
    /// phone's store, goes out at the next sync, and wins, since it is the newer decision the phone
    /// made knowing the account's.
    #[test]
    fn an_offline_change_goes_out_later_and_wins_over_an_older_one() {
      let mut account = SyncedSettings::new();
      write_settings(
        &mut account,
        &[change(KEY_CONFIRM_CHANNEL_OVER, json!(20))],
        device(LAPTOP),
      );
      let (mut phone, state) = first_sync(MobileSettings::default(), &mut account);
      assert_eq!(phone.confirm_channel_over, 20);
      write_settings(
        &mut account,
        &[change(KEY_CONFIRM_CHANNEL_OVER, json!(30))],
        device(LAPTOP),
      );
      phone.confirm_channel_over = 40;

      let outcome = sync_mobile_settings(phone, &account, state, user(ME));
      write_settings(&mut account, &outcome.writes, device(PHONE));
      let (another_phone, _) = first_sync(MobileSettings::default(), &mut account);

      assert_eq!(outcome.writes, vec![change(KEY_CONFIRM_CHANNEL_OVER, json!(40))]);
      assert_eq!(another_phone.confirm_channel_over, 40);
    }

    /// Two devices change the same setting before either has seen the other's: every device settles
    /// on the same value, whatever order the uploads land in.
    #[test]
    fn two_devices_changing_one_setting_converge() {
      let mut account = SyncedSettings::new();
      let (mut phone, phone_state) = first_sync(MobileSettings::default(), &mut account);
      let (mut tablet, tablet_state) = first_sync(MobileSettings::default(), &mut account);
      phone.list_layout = ListLayout::List;
      tablet.list_layout = ListLayout::Spaces;
      tablet.list_grouping = ListGrouping::BySpace;

      let from_phone = sync_mobile_settings(phone, &account, phone_state, user(ME));
      let from_tablet = sync_mobile_settings(tablet, &account, tablet_state, user(ME));
      write_settings(&mut account, &from_phone.writes, device(PHONE));
      write_settings(&mut account, &from_tablet.writes, device(LAPTOP));
      let phone = sync_mobile_settings(from_phone.settings, &account, from_phone.state, user(ME));
      let tablet = sync_mobile_settings(from_tablet.settings, &account, from_tablet.state, user(ME));

      assert_eq!(phone.settings, tablet.settings);
      assert_eq!(phone.settings.list_grouping, ListGrouping::BySpace);
    }

    /// The first sync never overwrites what the account already holds: a customized phone keeps its
    /// own, the account keeps its own, until someone changes the setting on purpose.
    #[test]
    fn the_first_sync_keeps_a_customized_value_and_takes_the_rest() {
      let mut account = SyncedSettings::new();
      write_settings(
        &mut account,
        &[
          change(KEY_LIST_LAYOUT, json!("spaces")),
          change(KEY_SHARE_TYPING, json!(false)),
        ],
        device(LAPTOP),
      );

      let outcome = sync_mobile_settings(customized(), &account, MobileSyncState::default(), user(ME));

      assert_eq!(outcome.settings.list_layout, ListLayout::List);
      assert!(!outcome.settings.shares_typing);
      assert!(outcome.writes.is_empty());
    }

    #[test]
    fn the_first_sync_seeds_an_empty_account_but_keeps_keywords_on_the_phone() {
      let mut local = customized();
      local.keywords = vec!["invoice".into()];

      let outcome = sync_mobile_settings(
        local,
        &SyncedSettings::new(),
        MobileSyncState::default(),
        user(ME),
      );

      assert_eq!(outcome.writes.len(), 1);
      assert_eq!(outcome.writes[0].key, KEY_LIST_LAYOUT);
      assert_eq!(outcome.writes[0].kind, SettingWriteKind::Seed);
      assert_eq!(outcome.settings.keywords, vec!["invoice".to_owned()]);
    }

    #[test]
    fn another_account_starts_from_the_defaults() {
      let mut account = SyncedSettings::new();
      let (mut phone, state) = first_sync(customized(), &mut account);
      phone.keywords = vec!["private".into()];

      let outcome = sync_mobile_settings(phone, &SyncedSettings::new(), state, user(SOMEONE_ELSE));

      assert_eq!(outcome.settings, MobileSettings::default());
      assert!(outcome.writes.is_empty());
    }

    /// Keys of newer clients and settings only web uses don't bother a phone.
    #[test]
    fn settings_a_phone_does_not_know_are_left_alone() {
      let mut account = SyncedSettings::new();
      write_settings(
        &mut account,
        &[
          change("display.density", json!("compact")),
          change("from.a.newer.client", json!({"x": 1})),
          change(KEY_ANIMATED_IMAGES, json!("on_hover")),
        ],
        device(LAPTOP),
      );
      let (phone, state) = first_sync(MobileSettings::default(), &mut account);

      let outcome = sync_mobile_settings(phone, &account, state, user(ME));

      assert_eq!(outcome.settings, MobileSettings::default());
      assert!(outcome.writes.is_empty());
    }

    #[test]
    fn syncing_again_with_nothing_new_writes_nothing() {
      let mut account = SyncedSettings::new();
      let (phone, state) = first_sync(customized(), &mut account);

      let outcome = sync_mobile_settings(phone.clone(), &account, state.clone(), user(ME));

      assert!(outcome.writes.is_empty());
      assert_eq!(outcome.settings, phone);
      assert_eq!(outcome.state, state);
    }
  }

  /// A phone's first sync, with its writes handed to the account and read back once.
  fn first_sync(local: MobileSettings, account: &mut SyncedSettings) -> (MobileSettings, MobileSyncState) {
    let outcome = sync_mobile_settings(local, account, MobileSyncState::default(), user(ME));
    write_settings(account, &outcome.writes, device(PHONE));
    let settled = sync_mobile_settings(outcome.settings, account, outcome.state, user(ME));
    (settled.settings, settled.state)
  }

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn device(raw: i64) -> DeviceId {
    DeviceId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn customized() -> MobileSettings {
    MobileSettings {
      list_layout: ListLayout::List,
      ..MobileSettings::default()
    }
  }

  fn change(key: &str, value: Value) -> SettingWrite {
    SettingWrite {
      key: key.to_owned(),
      value,
      kind: SettingWriteKind::Change,
    }
  }
}
