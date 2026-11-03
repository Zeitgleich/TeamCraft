//! Which settings follow the account, and keeping them in step with it (issue #945, e2ee.md §6.4).
//!
//! The rules are `teamcraft_client_api::settings_sync::reconcile`. Here is what they apply to:
//! [`SYNCED_PREFS`] and the top-level settings in [`SYNCED_TOP_LEVEL`] live in the account's
//! encrypted data. The server-enforced notification settings stay with the server
//! ([`SERVER_PREFS`], `notify_sync`), and what only makes sense on one machine stays there
//! ([`DEVICE_PREFS`], and everything else in `settings.rs`: recent lists, last open places, cache and
//! preview limits). A test makes every new field of `Preferences` pick one of the lists.
//!
//! A change here reaches the core after [`WRITE_DEBOUNCE`], so going through a row of options costs
//! one key bag upload. A change elsewhere arrives through the gateway's `key_bag_update`: the core
//! merges it and moves `settings_mark`, and the UI applies it at once.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use leptos::leptos_dom::helpers::set_timeout;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use teamcraft_client_api::emoji_catalog::SkinTone;
use teamcraft_client_api::settings_sync::{SettingBases, SyncedValues, reconcile};
use teamcraft_proto::synced_settings::SyncedSettings;

use crate::i18n::Locale;
use crate::model::{ConversationId, UserId};
use crate::prefs::Preferences;
use crate::remote_backend;
use crate::settings::{SendMode, Settings, Theme, browser_locale, load_json, store_json};
use crate::state::AppState;

/// Where this device keeps what it last agreed on with the account. One account at a time, like the
/// preferences themselves.
const KEY_SYNC_STATE: &str = "teamcraft.settings_sync";
/// How long the settings have to rest before they go to the core. A write re-encodes the account
/// data and the next sync round encrypts and uploads the whole key bag.
const WRITE_DEBOUNCE: Duration = Duration::from_millis(1500);

const KEY_THEME: &str = "theme";
const KEY_LOCALE: &str = "locale";
const KEY_SEND_MODE: &str = "send_mode";
const KEY_SHOW_JOIN_LEAVE: &str = "show_join_leave";
const KEY_SKIN_TONE: &str = "emoji_skin_tone";
const KEY_FAVORITE_ORDER: &str = "favorite_order";
/// Synced settings outside [`Preferences`], which have signals of their own.
const SYNCED_TOP_LEVEL: [&str; 6] = [
  KEY_THEME,
  KEY_LOCALE,
  KEY_SEND_MODE,
  KEY_SHOW_JOIN_LEAVE,
  KEY_SKIN_TONE,
  KEY_FAVORITE_ORDER,
];

/// The key of a preference is its place in [`Preferences`], so a key means the same on every client
/// and a field's type decides what it reads.
macro_rules! synced_prefs {
  ($($key:literal => $($field:ident).+,)*) => {
    /// The preferences that follow the account.
    pub const SYNCED_PREFS: &[&str] = &[$($key),*];

    fn read_pref(prefs: &Preferences, key: &str) -> Option<Value> {
      match key {
        $($key => serde_json::to_value(&prefs.$($field).+).ok(),)*
        _ => None,
      }
    }

    /// Sets a preference from the account; `false` if the value isn't one this build can read.
    fn write_pref(prefs: &mut Preferences, key: &str, value: &Value) -> bool {
      match key {
        $($key => match serde_json::from_value(value.clone()) {
          Ok(parsed) => {
            prefs.$($field).+ = parsed;
            true
          },
          Err(_) => false,
        },)*
        _ => false,
      }
    }
  };
}

synced_prefs! {
  "display.density" => display.density,
  "display.image_size" => display.image_size,
  "display.show_status_in_messages" => display.show_status_in_messages,
  "display.maybe_divider_dwell_secs" => display.maybe_divider_dwell_secs,
  "sidebar.sort" => sidebar.sort,
  "sidebar.dm_sort" => sidebar.dm_sort,
  "sidebar.dm_limit" => sidebar.dm_limit,
  "sidebar.is_grouped_by_space" => sidebar.is_grouped_by_space,
  "sidebar.reopen_last_conversation" => sidebar.reopen_last_conversation,
  "sidebar.shows_unreads_section" => sidebar.shows_unreads_section,
  "composer.convert_emoticons" => composer.convert_emoticons,
  "composer.show_formatting" => composer.show_formatting,
  "privacy.share_typing" => privacy.share_typing,
  "privacy.away_after_minutes" => privacy.away_after_minutes,
  "notifications.desktop" => notifications.desktop,
  "notifications.push_delay_minutes" => notifications.push_delay_minutes,
  "notifications.first_name_is_mention" => notifications.first_name_is_mention,
  "notifications.channel_mentions_notify" => notifications.channel_mentions_notify,
  "notifications.show_preview" => notifications.show_preview,
  "notifications.confirm_channel_over" => notifications.confirm_channel_over,
  "notifications.keywords" => notifications.keywords,
  "notifications.highlight_keywords" => notifications.highlight_keywords,
  "notifications.thread_replies" => notifications.thread_replies,
  "notifications.plays_ringtone" => notifications.plays_ringtone,
  "notifications.shows_call_window" => notifications.shows_call_window,
  "notifications.maybe_meeting_reminder_minutes" => notifications.maybe_meeting_reminder_minutes,
  "notifications.notifies_meeting_start" => notifications.notifies_meeting_start,
  "calls.start_muted" => calls.start_muted,
  "calls.start_camera_off" => calls.start_camera_off,
  "calls.background" => calls.background,
  "calls.mirror_self_view" => calls.mirror_self_view,
  "calls.push_to_talk" => calls.push_to_talk,
  "calls.status_sounds" => calls.status_sounds,
  "onboarding.done" => onboarding.done,
  "onboarding.is_dismissed" => onboarding.is_dismissed,
}

/// Preferences the server keeps, because it enforces them (`notify_sync`). They already follow the
/// account there; a second copy would only add a conflict.
pub const SERVER_PREFS: &[&str] = &[
  "notifications.push",
  "notifications.push_when",
  "notifications.uses_desktop_attention",
  "notifications.auto_follow_on_channel_mention",
  "notifications.dnd",
  "notifications.maybe_dnd_until",
  "notifications.conversations",
  "notifications.spaces",
  "privacy.share_read_receipts",
];

/// Preferences that belong to one machine: its hardware, screen, disk, or a file picked on it.
pub const DEVICE_PREFS: &[&str] = &[
  "display.zoom",
  "display.text_size",
  "calls.camera_id",
  "calls.microphone_id",
  "calls.speaker_id",
  "calls.noise_suppression",
  "calls.echo_cancellation",
  "calls.share_sound",
  "calls.hide_share_preview",
  "calls.screen_mode",
  "calls.screen_resolution",
  "calls.camera_quality",
  "notifications.attention_input_secs",
  "notifications.message_sound",
  "notifications.own_sound_limits",
  "notifications.level_defaults_revision",
];

/// What this device keeps between runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct SyncState {
  #[serde(rename = "user")]
  maybe_user: Option<UserId>,
  bases: SettingBases,
}

/// The settings [`Settings`] holds for syncing, as plain values.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LocalValues {
  theme: Theme,
  locale: Locale,
  default_locale: Locale,
  send_mode: SendMode,
  show_join_leave: bool,
  skin_tone: SkinTone,
  favorite_order: Vec<ConversationId>,
  prefs: Preferences,
}

/// Keeps the account's copy and this device's settings in step until the shell goes away.
struct SettingsSync {
  state: AppState,
  settings: Settings,
  maybe_account: RefCell<Option<SyncedSettings>>,
  maybe_sync_state: RefCell<Option<SyncState>>,
  /// Only the newest read counts: an older answer overtaking it would look like a change back.
  newest_read: Cell<u64>,
  /// Only the last change of a burst goes out.
  newest_change: Cell<u64>,
}

/// Starts following the account: reads its settings whenever the core says they changed, and offers
/// local changes once they rest. Does nothing on a backend without a core.
pub fn start(state: AppState, settings: Settings) {
  let sync = Rc::new(SettingsSync {
    state,
    settings,
    maybe_account: RefCell::new(None),
    maybe_sync_state: RefCell::new(None),
    newest_read: Cell::new(0),
    newest_change: Cell::new(0),
  });
  let mark = Memo::new(move |_| state.read(|backend| backend.settings_mark()));
  let reader = sync.clone();
  Effect::new(move |_| {
    // 0: this device hasn't merged the account yet, so it can't tell what the account holds.
    if mark.get() > 0 {
      reader.read_account();
    }
  });
  Effect::new(move |_| {
    settings.theme.track();
    settings.locale.track();
    settings.send_mode.track();
    settings.show_join_leave.track();
    settings.emoji_skin_tone.track();
    settings.favorite_order.track();
    settings.prefs.track();
    sync.plan_write();
  });
}

impl SettingsSync {
  fn read_account(self: &Rc<Self>) {
    let ticket = self.newest_read.get().wrapping_add(1);
    self.newest_read.set(ticket);
    let sync = self.clone();
    spawn_local(async move {
      match remote_backend::account_settings().await {
        // async-ok: a Cell the task owns, not a signal
        Ok(account) if sync.newest_read.get() == ticket => {
          *sync.maybe_account.borrow_mut() = Some(account);
          sync.reconcile_now();
        },
        Ok(_) => {},
        // The next change of the mark, or of a setting, reads again. Nothing is lost: changes wait
        // in the settings until the three-way merge sees them.
        Err(error) => tracing::debug!(%error, "account settings not read"),
      }
    });
  }

  fn plan_write(self: &Rc<Self>) {
    let ticket = self.newest_change.get().wrapping_add(1);
    self.newest_change.set(ticket);
    let sync = self.clone();
    set_timeout(
      move || {
        // async-ok: a Cell the timer owns, not a signal
        if sync.newest_change.get() == ticket {
          sync.reconcile_now();
        }
      },
      WRITE_DEBOUNCE,
    );
  }

  /// Runs the three-way merge against the last answer; before the first answer there is nothing to
  /// merge with, and the changes wait.
  fn reconcile_now(&self) {
    let account_guard = self.maybe_account.borrow();
    let Some(account) = account_guard.as_ref() else {
      return;
    };
    let Some(user) = self.state.try_read(|backend| backend.me().id) else {
      return;
    };
    let Some(mut local) = LocalValues::capture(self.settings) else {
      return;
    };
    let before = local.clone();
    let mut sync_state_guard = self.maybe_sync_state.borrow_mut();
    let sync_state =
      sync_state_guard.get_or_insert_with(|| load_json::<SyncState>(KEY_SYNC_STATE).unwrap_or_default());
    let stored_state = sync_state.clone();
    prepare_for_user(&mut local, sync_state, user);
    let writes = reconcile(&mut local, account, &mut sync_state.bases);
    if local != before {
      local.apply(self.settings);
    }
    if *sync_state != stored_state {
      store_json(KEY_SYNC_STATE, &*sync_state);
    }
    if !writes.is_empty() {
      self.state.try_read(|backend| backend.write_settings(writes));
    }
  }
}

/// Another account's settings don't carry over: its synced values go back to the defaults, and this
/// account's are taken from the account as on a new install. A first run after the update has no
/// owner on record and keeps what the device has.
fn prepare_for_user(local: &mut LocalValues, sync_state: &mut SyncState, user: UserId) {
  if sync_state.maybe_user.is_some_and(|known| known != user) {
    local.reset();
    sync_state.bases.clear();
  }
  sync_state.maybe_user = Some(user);
}

impl LocalValues {
  /// The current values; `None` once the shell is gone.
  fn capture(settings: Settings) -> Option<Self> {
    Some(Self {
      theme: settings.theme.try_get_untracked()?,
      locale: settings.locale.try_get_untracked()?,
      default_locale: browser_locale(),
      send_mode: settings.send_mode.try_get_untracked()?,
      show_join_leave: settings.show_join_leave.try_get_untracked()?,
      skin_tone: settings.emoji_skin_tone.try_get_untracked()?,
      favorite_order: settings.favorite_order.try_get_untracked()?,
      prefs: settings.prefs.try_get_untracked()?,
    })
  }

  /// Sets the signals that changed, so each only tells its readers when it has news.
  fn apply(self, settings: Settings) {
    if settings.theme.try_get_untracked() != Some(self.theme) {
      settings.theme.try_set(self.theme);
    }
    if settings.locale.try_get_untracked() != Some(self.locale) {
      settings.locale.try_set(self.locale);
    }
    if settings.send_mode.try_get_untracked() != Some(self.send_mode) {
      settings.send_mode.try_set(self.send_mode);
    }
    if settings.show_join_leave.try_get_untracked() != Some(self.show_join_leave) {
      settings.show_join_leave.try_set(self.show_join_leave);
    }
    if settings.emoji_skin_tone.try_get_untracked() != Some(self.skin_tone) {
      settings.emoji_skin_tone.try_set(self.skin_tone);
    }
    if settings.favorite_order.try_get_untracked().as_ref() != Some(&self.favorite_order) {
      settings.favorite_order.try_set(self.favorite_order);
    }
    if settings.prefs.try_get_untracked().as_ref() != Some(&self.prefs) {
      settings.prefs.try_set(self.prefs);
    }
  }

  /// Back to what a new install has, for every synced setting. The rest stays.
  fn reset(&mut self) {
    let defaults = Self::defaults(self.default_locale);
    for key in self.keys() {
      if let Some(value) = defaults.read(key) {
        self.adopt(key, &value);
      }
    }
  }

  fn defaults(default_locale: Locale) -> Self {
    Self {
      theme: Theme::System,
      locale: default_locale,
      default_locale,
      send_mode: SendMode::Enter,
      show_join_leave: true,
      skin_tone: SkinTone::default(),
      favorite_order: Vec::new(),
      prefs: Preferences::default(),
    }
  }
}

impl SyncedValues for LocalValues {
  fn keys(&self) -> Vec<&'static str> {
    SYNCED_TOP_LEVEL.iter().chain(SYNCED_PREFS).copied().collect()
  }

  fn read(&self, key: &str) -> Option<Value> {
    match key {
      KEY_THEME => Some(json!(self.theme.slug())),
      KEY_LOCALE => Some(json!(self.locale.tag())),
      KEY_SEND_MODE => Some(json!(self.send_mode.slug())),
      KEY_SHOW_JOIN_LEAVE => Some(json!(self.show_join_leave)),
      KEY_SKIN_TONE => Some(json!(self.skin_tone.slug())),
      KEY_FAVORITE_ORDER => serde_json::to_value(&self.favorite_order).ok(),
      _ => read_pref(&self.prefs, key),
    }
  }

  fn is_default(&self, key: &str) -> bool {
    self.read(key) == Self::defaults(self.default_locale).read(key)
  }

  fn adopt(&mut self, key: &str, value: &Value) -> bool {
    match key {
      KEY_THEME => assign(&mut self.theme, value.as_str().and_then(Theme::from_slug)),
      KEY_LOCALE => assign(&mut self.locale, value.as_str().and_then(Locale::from_tag)),
      KEY_SEND_MODE => assign(&mut self.send_mode, value.as_str().and_then(SendMode::from_slug)),
      KEY_SHOW_JOIN_LEAVE => assign(&mut self.show_join_leave, value.as_bool()),
      KEY_SKIN_TONE => assign(&mut self.skin_tone, value.as_str().and_then(SkinTone::from_slug)),
      KEY_FAVORITE_ORDER => assign(
        &mut self.favorite_order,
        serde_json::from_value(value.clone()).ok(),
      ),
      _ => write_pref(&mut self.prefs, key, value),
    }
  }
}

fn assign<T>(place: &mut T, maybe_value: Option<T>) -> bool {
  match maybe_value {
    Some(value) => {
      *place = value;
      true
    },
    None => false,
  }
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeSet;

  use teamcraft_client_api::settings_sync::SettingBase;
  use teamcraft_proto::synced_settings::SettingEntry;
  use teamcraft_proto::{DeviceId, Id, Snowflake};

  use super::*;
  use crate::prefs::{Density, NotifyLevel, Zoom};

  mod lists {
    use super::*;

    /// A new field of `Preferences` has to say where it belongs, or this fails.
    #[test]
    fn every_preference_is_synced_or_stays_on_purpose() {
      let tree = serde_json::to_value(Preferences::default()).unwrap();
      let mut fields = BTreeSet::new();
      for (section, values) in tree.as_object().unwrap() {
        for field in values.as_object().unwrap().keys() {
          fields.insert(format!("{section}.{field}"));
        }
      }
      let listed: BTreeSet<String> = SYNCED_PREFS
        .iter()
        .chain(SERVER_PREFS)
        .chain(DEVICE_PREFS)
        .map(|key| (*key).to_owned())
        .collect();
      assert_eq!(fields, listed);
    }

    #[test]
    fn no_preference_is_in_two_lists() {
      let total = SYNCED_PREFS.len() + SERVER_PREFS.len() + DEVICE_PREFS.len();
      let distinct: BTreeSet<&&str> = SYNCED_PREFS
        .iter()
        .chain(SERVER_PREFS)
        .chain(DEVICE_PREFS)
        .collect();
      assert_eq!(distinct.len(), total);
    }

    #[test]
    fn hardware_and_zoom_stay_on_the_device() {
      for key in [
        "display.zoom",
        "calls.camera_id",
        "calls.microphone_id",
        "calls.speaker_id",
      ] {
        assert!(!SYNCED_PREFS.contains(&key), "{key}");
      }
    }
  }

  mod values {
    use super::*;

    #[test]
    fn every_key_reads_and_takes_its_own_value_back() {
      let mut local = values();
      for key in local.keys() {
        let value = local.read(key).unwrap_or_else(|| panic!("{key} has no value"));
        assert!(local.adopt(key, &value), "{key}");
        assert_eq!(local.read(key).as_ref(), Some(&value), "{key}");
      }
      assert_eq!(local, values());
    }

    #[test]
    fn a_value_of_a_newer_client_is_refused_and_changes_nothing() {
      let mut local = values();
      for key in [
        "display.density",
        KEY_THEME,
        KEY_LOCALE,
        KEY_SEND_MODE,
        KEY_SKIN_TONE,
      ] {
        assert!(!local.adopt(key, &json!("from-the-future")), "{key}");
      }
      assert_eq!(local, values());
    }

    #[test]
    fn stored_names_stay_stable() {
      let mut local = values();
      local.theme = Theme::Dark;
      local.prefs.display.density = Density::Compact;
      local.prefs.notifications.desktop = NotifyLevel::Mentions;
      assert_eq!(local.read("theme"), Some(json!("dark")));
      assert_eq!(local.read("display.density"), Some(json!("compact")));
      assert_eq!(local.read("notifications.desktop"), Some(json!("mentions")));
    }

    #[test]
    fn the_default_is_the_browser_language_not_a_fixed_one() {
      let mut local = LocalValues::defaults(Locale::De);
      assert!(local.is_default(KEY_LOCALE));
      local.locale = Locale::En;
      assert!(!local.is_default(KEY_LOCALE));
    }
  }

  mod first_sync {
    use super::*;

    /// The issue's case: everything customized in the browser, then a fresh desktop app.
    #[test]
    fn a_new_install_gets_the_customized_settings_of_the_account() {
      let mut browser = values();
      browser.theme = Theme::Dark;
      browser.prefs.display.density = Density::Compact;
      browser.prefs.notifications.keywords = vec!["release".to_owned()];
      browser.prefs.display.zoom = Zoom::from_percent(150);
      let mut browser_bases = SettingBases::new();
      let writes = reconcile(&mut browser, &SyncedSettings::new(), &mut browser_bases);
      assert_eq!(
        writes.len(),
        3,
        "only what was customized, zoom stays: {writes:?}"
      );

      let mut account = SyncedSettings::new();
      teamcraft_proto::synced_settings::write_settings(&mut account, &writes, device(1));

      let mut desktop = values();
      let mut desktop_bases = SettingBases::new();
      assert!(reconcile(&mut desktop, &account, &mut desktop_bases).is_empty());
      assert_eq!(desktop.theme, Theme::Dark);
      assert_eq!(desktop.prefs.display.density, Density::Compact);
      assert_eq!(desktop.prefs.notifications.keywords, ["release"]);
      assert_eq!(desktop.prefs.display.zoom, Zoom::DEFAULT, "zoom stays per device");
    }

    #[test]
    fn a_setting_customized_on_this_device_is_kept() {
      let mut desktop = values();
      desktop.theme = Theme::Light;
      let mut bases = SettingBases::new();
      assert!(reconcile(&mut desktop, &entry(KEY_THEME, json!("dark")), &mut bases).is_empty());
      assert_eq!(desktop.theme, Theme::Light);
    }
  }

  mod another_account {
    use super::*;

    #[test]
    fn its_synced_settings_dont_carry_over() {
      let mut local = values();
      local.theme = Theme::Dark;
      local.prefs.notifications.keywords = vec!["private".to_owned()];
      local.prefs.display.zoom = Zoom::from_percent(150);
      let mut state = state_of(1);
      prepare_for_user(&mut local, &mut state, user(2));
      assert_eq!(local.theme, Theme::System);
      assert!(local.prefs.notifications.keywords.is_empty());
      assert_eq!(local.prefs.display.zoom.percent(), 150, "the device's own stay");
      assert!(state.bases.is_empty());
      assert_eq!(state.maybe_user, Some(user(2)));
    }

    #[test]
    fn the_same_account_keeps_everything() {
      let mut local = values();
      local.theme = Theme::Dark;
      let mut state = state_of(1);
      prepare_for_user(&mut local, &mut state, user(1));
      assert_eq!(local.theme, Theme::Dark);
      assert_eq!(state.bases.len(), 1);
    }

    #[test]
    fn the_first_run_after_the_update_keeps_what_the_device_has() {
      let mut local = values();
      local.theme = Theme::Dark;
      let mut state = SyncState::default();
      prepare_for_user(&mut local, &mut state, user(1));
      assert_eq!(local.theme, Theme::Dark);
    }

    fn state_of(raw: i64) -> SyncState {
      SyncState {
        maybe_user: Some(user(raw)),
        bases: [(
          KEY_THEME.to_owned(),
          SettingBase {
            value: json!("dark"),
            maybe_stamp: None,
          },
        )]
        .into(),
      }
    }

    fn user(raw: i64) -> UserId {
      UserId::from_snowflake(Snowflake::new(raw).unwrap())
    }
  }

  fn values() -> LocalValues {
    LocalValues::defaults(Locale::En)
  }

  fn entry(key: &str, value: Value) -> SyncedSettings {
    [(
      key.to_owned(),
      SettingEntry {
        value,
        version: 1,
        writer: device(9),
      },
    )]
    .into()
  }

  fn device(raw: i64) -> DeviceId {
    DeviceId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
