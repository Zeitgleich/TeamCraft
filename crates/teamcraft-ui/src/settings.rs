//! Per-device user preferences, persisted in `localStorage`. They will move to the synced, encrypted
//! user settings of the client core later; the UI only depends on the signals here.

use leptos::prelude::*;

use crate::i18n::Locale;

const KEY_THEME: &str = "teamcraft.theme";
const KEY_LOCALE: &str = "teamcraft.locale";
const KEY_SEND_MODE: &str = "teamcraft.send_mode";
const KEY_UNREADS_GROUP: &str = "teamcraft.unreads_group";
const KEY_SHOW_JOIN_LEAVE: &str = "teamcraft.show_join_leave";

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

#[derive(Clone, Copy)]
pub struct Settings {
  pub theme: RwSignal<Theme>,
  pub locale: RwSignal<Locale>,
  pub send_mode: RwSignal<SendMode>,
  /// Show conversations with unread messages in a separate "Unreads" group at the top.
  pub show_unreads_group: RwSignal<bool>,
  pub show_join_leave: RwSignal<bool>,
}

impl Settings {
  /// Loads stored preferences and keeps them stored whenever they change.
  pub fn load() -> Self {
    let settings = Self {
      theme: RwSignal::new(load(KEY_THEME, Theme::from_slug).unwrap_or(Theme::System)),
      locale: RwSignal::new(load(KEY_LOCALE, Locale::from_tag).unwrap_or_else(browser_locale)),
      send_mode: RwSignal::new(load(KEY_SEND_MODE, SendMode::from_slug).unwrap_or(SendMode::Enter)),
      show_unreads_group: RwSignal::new(load(KEY_UNREADS_GROUP, parse_bool).unwrap_or(true)),
      show_join_leave: RwSignal::new(load(KEY_SHOW_JOIN_LEAVE, parse_bool).unwrap_or(true)),
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

fn persist<T: Copy + Send + Sync + 'static>(key: &'static str, signal: RwSignal<T>, encode: fn(T) -> String) {
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

fn local_storage() -> Option<web_sys::Storage> {
  web_sys::window()?.local_storage().ok().flatten()
}
