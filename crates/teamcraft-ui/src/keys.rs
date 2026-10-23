//! Keyboard shortcut labels. A [`Shortcut`] is described once and rendered per platform and language:
//! "Ctrl+Shift+F" / "Strg+Umschalt+F" on Windows and Linux, "⌘⇧F" on macOS.

use std::sync::OnceLock;

use leptos::prelude::*;

use crate::i18n::{I18n, Locale, translate, use_i18n};

/// Modifier keys the UI uses in its shortcuts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modifier {
  /// Ctrl on Windows and Linux, Command on macOS.
  Primary,
  Shift,
  Alt,
}

/// The non-modifier key of a shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
  /// A printable key, shown as is (`K`, `,`, `+`).
  Char(&'static str),
  Up,
  Down,
  Enter,
  Escape,
}

/// Operating system family, as far as shortcut labels care.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
  Mac,
  Other,
}

/// Opens the quick switcher.
pub const QUICK_SWITCHER: Shortcut = Shortcut::primary("K");

/// Searches messages for the quick switcher's text.
pub const SEARCH_MESSAGES: Shortcut = Shortcut::new(&[Modifier::Primary], Key::Enter);
pub const ESCAPE: Shortcut = Shortcut::new(&[], Key::Escape);
pub const ENTER: Shortcut = Shortcut::new(&[], Key::Enter);

/// A modifier combination plus one key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shortcut {
  modifiers: &'static [Modifier],
  key: Key,
}

impl Shortcut {
  pub const fn new(modifiers: &'static [Modifier], key: Key) -> Self {
    Self { modifiers, key }
  }

  /// Primary modifier plus a printable key.
  pub const fn primary(key: &'static str) -> Self {
    Self::new(&[Modifier::Primary], Key::Char(key))
  }

  /// One label per key cap, in typing order: `["Strg", "Umschalt", "F"]` or `["⌘", "⇧", "F"]`.
  pub fn parts(&self, platform: Platform, locale: Locale) -> Vec<String> {
    let mut parts: Vec<String> = self
      .modifiers
      .iter()
      .map(|modifier| modifier_label(*modifier, platform, locale))
      .collect();
    parts.push(key_label(self.key, platform, locale));
    parts
  }

  /// The shortcut as one string for running text: "Strg+K", "⌘K".
  pub fn text(&self, platform: Platform, locale: Locale) -> String {
    self.parts(platform, locale).join(Self::separator(platform))
  }

  /// Spoken form for screen readers: "Strg plus K", "Command plus K".
  pub fn spoken(&self, platform: Platform, locale: Locale) -> String {
    let plus = format!(" {} ", translate(locale, "key-plus-word", None));
    let mut names: Vec<String> = self
      .modifiers
      .iter()
      .map(|modifier| spoken_modifier(*modifier, platform, locale))
      .collect();
    names.push(spoken_key(self.key, locale));
    names.join(&plus)
  }

  fn separator(platform: Platform) -> &'static str {
    match platform {
      Platform::Mac => "",
      Platform::Other => "+",
    }
  }
}

impl Platform {
  /// The platform the page runs on, detected once. In the desktop app the webview reports the real OS.
  pub fn current() -> Self {
    static CURRENT: OnceLock<Platform> = OnceLock::new();
    *CURRENT.get_or_init(Self::detect)
  }

  fn detect() -> Self {
    let Some(navigator) = web_sys::window().map(|window| window.navigator()) else {
      return Self::Other;
    };
    // `userAgentData.platform` is the modern source; `navigator.platform` covers Safari and Firefox.
    let user_agent_data_platform = js_sys::Reflect::get(&navigator, &"userAgentData".into())
      .ok()
      .filter(|data| !data.is_undefined() && !data.is_null())
      .and_then(|data| js_sys::Reflect::get(&data, &"platform".into()).ok())
      .and_then(|platform| platform.as_string())
      .filter(|platform| !platform.is_empty());
    let name = user_agent_data_platform.or_else(|| navigator.platform().ok());
    Self::from_name(name.as_deref().unwrap_or_default())
  }

  fn from_name(name: &str) -> Self {
    if name.to_ascii_lowercase().contains("mac") {
      Self::Mac
    } else {
      Self::Other
    }
  }
}

/// A shortcut as `<kbd>` key caps, readable as "Strg plus K" by screen readers. Follows the UI language.
#[component]
pub fn ShortcutKeys(shortcut: Shortcut) -> impl IntoView {
  let i18n = use_i18n();
  let platform = Platform::current();
  view! {
    <span
      class="shortcut"
      role="img"
      aria-label=move || shortcut.spoken(platform, i18n.locale())
    >
      {move || {
        let separator = Shortcut::separator(platform);
        shortcut
          .parts(platform, i18n.locale())
          .into_iter()
          .enumerate()
          .map(|(index, part)| {
            view! {
              {(index > 0 && !separator.is_empty()).then(|| view! { <span class="shortcut-plus" aria-hidden="true">{separator}</span> })}
              <kbd aria-hidden="true">{part}</kbd>
            }
          })
          .collect_view()
      }}
    </span>
  }
}

/// Several alternatives side by side: "Alt+↑ / Alt+↓".
#[component]
pub fn ShortcutList(shortcuts: Vec<Shortcut>) -> impl IntoView {
  shortcuts
    .into_iter()
    .enumerate()
    .map(|(index, shortcut)| {
      view! {
        {(index > 0).then(|| view! { <span class="shortcut-or" aria-hidden="true">" / "</span> })}
        <ShortcutKeys shortcut />
      }
    })
    .collect_view()
}

/// The shortcut for running text on the current platform and language.
pub fn shortcut_text(shortcut: &Shortcut, locale: Locale) -> String {
  shortcut.text(Platform::current(), locale)
}

/// Translates a message whose text names shortcuts: `{ $send }` (Ctrl+Enter), `{ $newline }`
/// (Shift+Enter) and `{ $modifier }` (Ctrl) are filled in for the current platform and language.
pub fn t_with_shortcuts(i18n: I18n, id: &str) -> String {
  let locale = i18n.locale();
  let send = shortcut_text(&SEARCH_MESSAGES, locale);
  let newline = shortcut_text(&Shortcut::new(&[Modifier::Shift], Key::Enter), locale);
  let modifier = primary_label(locale);
  i18n.t_with(
    id,
    &[("send", &send), ("newline", &newline), ("modifier", &modifier)],
  )
}

/// Just the primary modifier: "Strg" or "⌘".
pub fn primary_label(locale: Locale) -> String {
  modifier_label(Modifier::Primary, Platform::current(), locale)
}

fn modifier_label(modifier: Modifier, platform: Platform, locale: Locale) -> String {
  match (modifier, platform) {
    (Modifier::Primary, Platform::Mac) => "⌘".to_owned(),
    (Modifier::Shift, Platform::Mac) => "⇧".to_owned(),
    (Modifier::Alt, Platform::Mac) => "⌥".to_owned(),
    (Modifier::Primary, Platform::Other) => translate(locale, "key-ctrl", None),
    (Modifier::Shift, Platform::Other) => translate(locale, "key-shift", None),
    (Modifier::Alt, Platform::Other) => translate(locale, "key-alt", None),
  }
}

fn key_label(key: Key, platform: Platform, locale: Locale) -> String {
  match key {
    Key::Char(text) => text.to_owned(),
    Key::Up => "↑".to_owned(),
    Key::Down => "↓".to_owned(),
    Key::Enter if platform == Platform::Mac => "↵".to_owned(),
    Key::Enter => translate(locale, "key-enter", None),
    Key::Escape => translate(locale, "key-escape", None),
  }
}

fn spoken_modifier(modifier: Modifier, platform: Platform, locale: Locale) -> String {
  let id = match (modifier, platform) {
    (Modifier::Primary, Platform::Mac) => "key-command",
    (Modifier::Alt, Platform::Mac) => "key-option",
    (Modifier::Primary, Platform::Other) => "key-ctrl",
    (Modifier::Shift, _) => "key-shift",
    (Modifier::Alt, Platform::Other) => "key-alt",
  };
  translate(locale, id, None)
}

fn spoken_key(key: Key, locale: Locale) -> String {
  match key {
    Key::Char(text) => text.to_owned(),
    Key::Up => translate(locale, "key-up", None),
    Key::Down => translate(locale, "key-down", None),
    Key::Enter => translate(locale, "key-enter", None),
    Key::Escape => translate(locale, "key-escape", None),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const SEARCH_ALL: Shortcut = Shortcut::new(&[Modifier::Primary, Modifier::Shift], Key::Char("F"));
  const NEXT_UNREAD: Shortcut = Shortcut::new(&[Modifier::Alt, Modifier::Shift], Key::Up);
  const SEND: Shortcut = Shortcut::new(&[Modifier::Primary], Key::Enter);

  mod windows_and_linux {
    use super::*;

    #[test]
    fn english_uses_ctrl_and_shift() {
      assert_eq!(SEARCH_ALL.text(Platform::Other, Locale::En), "Ctrl+Shift+F");
      assert_eq!(Shortcut::primary("K").text(Platform::Other, Locale::En), "Ctrl+K");
    }

    #[test]
    fn german_uses_strg_and_umschalt() {
      assert_eq!(SEARCH_ALL.text(Platform::Other, Locale::De), "Strg+Umschalt+F");
      assert_eq!(Shortcut::primary("K").text(Platform::Other, Locale::De), "Strg+K");
      assert_eq!(NEXT_UNREAD.text(Platform::Other, Locale::De), "Alt+Umschalt+↑");
    }

    #[test]
    fn named_keys_follow_the_language() {
      assert_eq!(SEND.text(Platform::Other, Locale::En), "Ctrl+Enter");
      assert_eq!(SEND.text(Platform::Other, Locale::De), "Strg+Enter");
      let escape = Shortcut::new(&[], Key::Escape);
      assert_eq!(escape.text(Platform::Other, Locale::De), "Esc");
    }
  }

  mod mac {
    use super::*;

    #[test]
    fn symbols_without_separator_in_every_language() {
      for locale in Locale::ALL {
        assert_eq!(SEARCH_ALL.text(Platform::Mac, locale), "⌘⇧F");
        assert_eq!(NEXT_UNREAD.text(Platform::Mac, locale), "⌥⇧↑");
        assert_eq!(SEND.text(Platform::Mac, locale), "⌘↵");
      }
    }

    #[test]
    fn parts_are_one_key_cap_each() {
      assert_eq!(SEARCH_ALL.parts(Platform::Mac, Locale::De), ["⌘", "⇧", "F"]);
      assert_eq!(
        SEARCH_ALL.parts(Platform::Other, Locale::De),
        ["Strg", "Umschalt", "F"]
      );
    }
  }

  mod spoken {
    use super::*;

    #[test]
    fn reads_as_words() {
      assert_eq!(
        Shortcut::primary("K").spoken(Platform::Other, Locale::De),
        "Strg plus K"
      );
      assert_eq!(
        Shortcut::primary("K").spoken(Platform::Other, Locale::En),
        "Ctrl plus K"
      );
      assert_eq!(
        SEARCH_ALL.spoken(Platform::Mac, Locale::En),
        "Command plus Shift plus F"
      );
      assert_eq!(
        NEXT_UNREAD.spoken(Platform::Mac, Locale::De),
        "Wahl plus Umschalt plus Pfeil hoch"
      );
    }
  }

  mod detection {
    use super::*;

    #[test]
    fn recognises_apple_platform_names() {
      assert_eq!(Platform::from_name("MacIntel"), Platform::Mac);
      assert_eq!(Platform::from_name("macOS"), Platform::Mac);
      assert_eq!(Platform::from_name("Win32"), Platform::Other);
      assert_eq!(Platform::from_name("Linux x86_64"), Platform::Other);
      assert_eq!(Platform::from_name(""), Platform::Other);
    }
  }
}
