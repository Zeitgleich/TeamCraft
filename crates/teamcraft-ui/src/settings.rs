//! Per-device user preferences, persisted in `localStorage`. They will move to the synced, encrypted
//! user settings of the client core later; the UI only depends on the signals here.

use leptos::prelude::*;

use crate::i18n::Locale;
use crate::prefs::Preferences;
use crate::sections::SidebarOptions;

const KEY_THEME: &str = "teamcraft.theme";
const KEY_LOCALE: &str = "teamcraft.locale";
const KEY_SEND_MODE: &str = "teamcraft.send_mode";
const KEY_UNREADS_GROUP: &str = "teamcraft.unreads_group";
const KEY_SHOW_JOIN_LEAVE: &str = "teamcraft.show_join_leave";
const KEY_BACKEND: &str = "teamcraft.backend";
const KEY_PREFS: &str = "teamcraft.prefs";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
  System,
  Light,
  Dark,
}

impl Theme {
  pub const ALL: [Theme; 3] = [Self::System, Self::Light, Self::Dark];

  pub fn slug(self) -> &'static str {
    match self {
      Self::System => "system",
      Self::Light => "light",
      Self::Dark => "dark",
    }
  }

  pub fn from_slug(slug: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|theme| theme.slug() == slug)
  }
}

/// Which key combination sends the message in the composer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendMode {
  /// Enter sends, Shift+Enter inserts a newline.
  Enter,
  /// Ctrl/Cmd+Enter sends, Enter inserts a newline.
  CtrlEnter,
}

impl SendMode {
  pub const ALL: [SendMode; 2] = [Self::Enter, Self::CtrlEnter];

  pub fn slug(self) -> &'static str {
    match self {
      Self::Enter => "enter",
      Self::CtrlEnter => "ctrl-enter",
    }
  }

  pub fn from_slug(slug: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|mode| mode.slug() == slug)
  }
}

/// Where the data comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendMode {
  /// Built-in demo data, no sign-in. The default.
  Mock,
  /// Sign in with OIDC and read spaces, channels and members from `teamcraft-server`.
  Server,
}

impl BackendMode {
  pub fn slug(self) -> &'static str {
    match self {
      Self::Mock => "mock",
      Self::Server => "server",
    }
  }

  pub fn from_slug(slug: &str) -> Option<Self> {
    match slug {
      "mock" => Some(Self::Mock),
      "server" => Some(Self::Server),
      _ => None,
    }
  }

  /// The mode for this page load: `?backend=server|mock` wins (and is remembered), then the stored
  /// choice, then the demo data.
  pub fn load() -> Self {
    let from_query = web_sys::window()
      .and_then(|window| window.location().search().ok())
      .and_then(|search| {
        search
          .trim_start_matches('?')
          .split('&')
          .find_map(|pair| pair.strip_prefix("backend=").and_then(Self::from_slug))
      });
    if let Some(mode) = from_query {
      store_string(KEY_BACKEND, mode.slug());
      return mode;
    }
    load(KEY_BACKEND, Self::from_slug).unwrap_or(Self::Mock)
  }

  /// Remembers the choice; the caller reloads the page.
  pub fn save(self) {
    store_string(KEY_BACKEND, self.slug());
  }
}

#[derive(Clone, Copy)]
pub struct Settings {
  pub theme: RwSignal<Theme>,
  pub locale: RwSignal<Locale>,
  pub send_mode: RwSignal<SendMode>,
  /// Show conversations with unread messages in a separate "Unreads" group at the top.
  pub show_unreads_group: RwSignal<bool>,
  pub show_join_leave: RwSignal<bool>,
  /// Notifications, privacy, calls, display and sidebar options (one JSON document).
  pub prefs: RwSignal<Preferences>,
}

impl Settings {
  /// Sidebar grouping from the current preferences. `usize::MAX` as limit shows every DM (keyboard
  /// navigation must reach conversations that the list currently hides).
  pub fn sidebar_options(&self, dm_limit: usize) -> SidebarOptions {
    let prefs = self.prefs.get();
    SidebarOptions {
      show_unreads_group: self.show_unreads_group.get(),
      sort: prefs.sidebar.sort,
      dm_limit: if dm_limit == usize::MAX { 0 } else { dm_limit },
    }
  }

  /// Loads stored preferences and keeps them stored whenever they change.
  pub fn load() -> Self {
    let settings = Self {
      theme: RwSignal::new(load(KEY_THEME, Theme::from_slug).unwrap_or(Theme::System)),
      locale: RwSignal::new(load(KEY_LOCALE, Locale::from_tag).unwrap_or_else(browser_locale)),
      send_mode: RwSignal::new(load(KEY_SEND_MODE, SendMode::from_slug).unwrap_or(SendMode::Enter)),
      show_unreads_group: RwSignal::new(load(KEY_UNREADS_GROUP, parse_bool).unwrap_or(true)),
      show_join_leave: RwSignal::new(load(KEY_SHOW_JOIN_LEAVE, parse_bool).unwrap_or(true)),
      prefs: RwSignal::new(load(KEY_PREFS, |text| Some(Preferences::from_json(text))).unwrap_or_default()),
    };
    persist(KEY_THEME, settings.theme, |theme| theme.slug().to_owned());
    persist(KEY_LOCALE, settings.locale, |locale| locale.tag().to_owned());
    persist(KEY_SEND_MODE, settings.send_mode, |mode| mode.slug().to_owned());
    persist(KEY_UNREADS_GROUP, settings.show_unreads_group, |value| {
      value.to_string()
    });
    persist(KEY_SHOW_JOIN_LEAVE, settings.show_join_leave, |value| {
      value.to_string()
    });
    persist(KEY_PREFS, settings.prefs, |prefs| prefs.to_json());
    settings
  }
}

fn parse_bool(value: &str) -> Option<bool> {
  value.parse().ok()
}

fn browser_locale() -> Locale {
  web_sys::window()
    .and_then(|window| window.navigator().language())
    .and_then(|tag| Locale::from_tag(&tag))
    .unwrap_or(Locale::En)
}

fn persist<T: Clone + Send + Sync + 'static>(
  key: &'static str,
  signal: RwSignal<T>,
  encode: fn(T) -> String,
) {
  Effect::new(move |_| {
    let value = encode(signal.get());
    if let Some(storage) = local_storage()
      && storage.set_item(key, &value).is_err()
    {
      // Private mode or a full quota: preferences just won't persist.
      tracing::debug!(key, "could not persist setting");
    }
  });
}

fn load<T>(key: &str, decode: fn(&str) -> Option<T>) -> Option<T> {
  let value = local_storage()?.get_item(key).ok().flatten()?;
  decode(&value)
}

/// Reads a raw string setting from `localStorage` (used for connection settings).
pub fn load_string(key: &str) -> Option<String> {
  local_storage()?
    .get_item(key)
    .ok()
    .flatten()
    .filter(|value| !value.is_empty())
}

/// Writes a raw string setting to `localStorage`; failures only mean it will not persist.
pub fn store_string(key: &str, value: &str) {
  if let Some(storage) = local_storage()
    && storage.set_item(key, value).is_err()
  {
    tracing::debug!(key, "could not persist value");
  }
}

fn local_storage() -> Option<web_sys::Storage> {
  web_sys::window()?.local_storage().ok().flatten()
}
