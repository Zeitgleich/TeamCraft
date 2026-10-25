//! Per-device user preferences, persisted in `localStorage`. They will move to the synced, encrypted
//! user settings of the client core later; the UI only depends on the signals here.

use std::collections::HashMap;

use leptos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::i18n::Locale;
use crate::model::{ConversationId, SpaceId, UserId};
use crate::navigation::push_recent;
use crate::prefs::Preferences;
use crate::sections::SidebarOptions;

const KEY_THEME: &str = "teamcraft.theme";
const KEY_LOCALE: &str = "teamcraft.locale";
const KEY_SEND_MODE: &str = "teamcraft.send_mode";
const KEY_UNREADS_GROUP: &str = "teamcraft.unreads_group";
const KEY_SHOW_JOIN_LEAVE: &str = "teamcraft.show_join_leave";
#[cfg(feature = "demo")]
const KEY_BACKEND: &str = "teamcraft.backend";
const KEY_PREFS: &str = "teamcraft.prefs";
const KEY_RECENT_CONVERSATIONS: &str = "teamcraft.recent_conversations";
const KEY_RECENT_EMOJI: &str = "teamcraft.recent_emoji";
const KEY_FAVORITE_ORDER: &str = "teamcraft.favorite_order";
/// Per account: `teamcraft.last_open.<user id>`, a [`LastOpen`].
const KEY_LAST_OPEN_PREFIX: &str = "teamcraft.last_open.";
/// Conversations the quick switcher remembers as recently visited.
const MAX_RECENT_CONVERSATIONS: usize = 20;
/// Where the last open conversation of an account was: Home, or a space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LastOpenPlace {
  Home(ConversationId),
  Space(SpaceId, ConversationId),
}

/// The conversation each place of one account had open last, so switching back reopens it (#498).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LastOpen {
  pub home: Option<ConversationId>,
  pub spaces: HashMap<SpaceId, ConversationId>,
}

/// What an account had open last on this device.
pub fn last_open(user: UserId) -> LastOpen {
  load_json(&last_open_key(user)).unwrap_or_default()
}

/// Remembers the conversation of a place for an account; the next visit to the place reopens it.
pub fn remember_last_open(user: UserId, place: LastOpenPlace) {
  let mut last = last_open(user);
  match place {
    LastOpenPlace::Home(conversation) => last.home = Some(conversation),
    LastOpenPlace::Space(space, conversation) => {
      last.spaces.insert(space, conversation);
    },
  }
  store_json(&last_open_key(user), &last);
}

fn last_open_key(user: UserId) -> String {
  format!("{KEY_LAST_OPEN_PREFIX}{user}")
}

/// Emoji the pickers offer as recently used.
pub const MAX_RECENT_EMOJI: usize = 16;

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

/// Where the data comes from. Only development builds with the `demo` feature have a choice; every
/// other build always signs in to a server.
#[cfg(feature = "demo")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendMode {
  /// Built-in demo data, no sign-in. The default of demo builds.
  Mock,
  /// Sign in with OIDC and read spaces, channels and members from `teamcraft-server`.
  Server,
}

#[cfg(feature = "demo")]
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
  /// Recently opened conversations, newest first (the quick switcher lists them first).
  pub recent_conversations: RwSignal<Vec<ConversationId>>,
  /// Recently used emoji, newest first (reactions and the composer's picker).
  pub recent_emoji: RwSignal<Vec<String>>,
  /// The user's own order of favorites in the sidebar (this device).
  pub favorite_order: RwSignal<Vec<ConversationId>>,
}

impl Settings {
  /// Sidebar grouping from the current preferences. `usize::MAX` as limit shows every DM (keyboard
  /// navigation must reach conversations that the list currently hides).
  pub fn sidebar_options(&self) -> SidebarOptions {
    SidebarOptions {
      show_unreads_group: self.show_unreads_group.get(),
      sort: self.prefs.with(|prefs| prefs.sidebar.sort),
    }
  }

  pub fn remember_conversation(&self, conversation: ConversationId) {
    self
      .recent_conversations
      .update(|list| push_recent(list, conversation, MAX_RECENT_CONVERSATIONS));
  }

  pub fn remember_emoji(&self, emoji: &str) {
    self
      .recent_emoji
      .update(|list| push_recent(list, emoji.to_owned(), MAX_RECENT_EMOJI));
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
      recent_conversations: RwSignal::new(load(KEY_RECENT_CONVERSATIONS, from_json).unwrap_or_default()),
      recent_emoji: RwSignal::new(load(KEY_RECENT_EMOJI, from_json).unwrap_or_default()),
      favorite_order: RwSignal::new(load(KEY_FAVORITE_ORDER, from_json).unwrap_or_default()),
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
    persist(KEY_RECENT_CONVERSATIONS, settings.recent_conversations, |list| {
      to_json(&list)
    });
    persist(KEY_RECENT_EMOJI, settings.recent_emoji, |list| to_json(&list));
    persist(KEY_FAVORITE_ORDER, settings.favorite_order, |list| to_json(&list));
    settings
  }
}

#[cfg(test)]
impl Settings {
  /// Defaults without `localStorage`, for tests outside a browser.
  pub fn for_tests() -> Self {
    Self {
      theme: RwSignal::new(Theme::System),
      locale: RwSignal::new(Locale::En),
      send_mode: RwSignal::new(SendMode::Enter),
      show_unreads_group: RwSignal::new(true),
      show_join_leave: RwSignal::new(true),
      prefs: RwSignal::new(Preferences::default()),
      recent_conversations: RwSignal::new(Vec::new()),
      recent_emoji: RwSignal::new(Vec::new()),
      favorite_order: RwSignal::new(Vec::new()),
    }
  }
}

fn from_json<T: serde::de::DeserializeOwned>(text: &str) -> Option<T> {
  serde_json::from_str(text).ok()
}

fn to_json<T: serde::Serialize>(value: &T) -> String {
  // Lists of IDs and strings always serialize; an empty list is the safe fallback.
  serde_json::to_string(value).unwrap_or_else(|_| "[]".to_owned())
}

fn parse_bool(value: &str) -> Option<bool> {
  value.parse().ok()
}

/// The language the app starts in: the saved choice, else the browser's. For code that runs before
/// (or without) the settings, like the notification button labels the service worker keeps.
pub fn stored_locale() -> Locale {
  load(KEY_LOCALE, Locale::from_tag).unwrap_or_else(browser_locale)
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

/// Reads a JSON value from `localStorage`; missing or unreadable values are `None`.
pub fn load_json<T: serde::de::DeserializeOwned>(key: &str) -> Option<T> {
  load_string(key).and_then(|text| from_json(&text))
}

/// Writes a JSON value to `localStorage`; failures only mean it will not persist.
pub fn store_json<T: serde::Serialize>(key: &str, value: &T) {
  match serde_json::to_string(value) {
    Ok(text) => store_string(key, &text),
    Err(error) => tracing::debug!(key, %error, "could not encode value"),
  }
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
  // Native unit tests have no browser: web-sys would panic, so they run without storage.
  if cfg!(not(target_arch = "wasm32")) {
    return None;
  }
  web_sys::window()?.local_storage().ok().flatten()
}
