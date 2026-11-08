//! Localisation with Fluent. English is the source language; every user-facing string goes through
//! [`I18n::t`] and friends. Translations live in `i18n/<language>/<area>.ftl`, one Fluent resource
//! per area of the app (#614).
//!
//! Native builds (the tests, the checks) embed every language's files (build.rs). The web app
//! carries none of them (#1437): `scripts/finish-loader.sh` joins each language's files into one of
//! the build (`i18n/<language>-<hash>.ftl`), `init.js` fetches the one the app starts in next to the
//! app, and [`load_start_language`] waits for it before anything mounts, so no view ever shows a
//! message's key in place of its text. Another language is fetched when the user switches to it;
//! until it is there, the UI stays in the one it shows ([`I18n::set_locale`]).

use std::cell::RefCell;

use fluent_bundle::FluentBundle as MessageBundle;
use fluent_bundle::{FluentArgs, FluentResource};
use leptos::prelude::*;
use unic_langid::LanguageIdentifier;

use crate::reactive::get_if_observed;

// `fn sources(tag)`: the embedded files of one language (natively, and in every build of the tests).
#[cfg(any(not(target_arch = "wasm32"), test))]
include!(concat!(env!("OUT_DIR"), "/i18n_sources.rs"));

/// UI languages that ship with translations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Locale {
  En,
  De,
}

impl Locale {
  pub const ALL: [Locale; 2] = [Self::En, Self::De];

  /// BCP 47 tag, also used for `Intl` formatting.
  pub fn tag(self) -> &'static str {
    match self {
      Self::En => "en",
      Self::De => "de",
    }
  }

  /// Name of the language in that language.
  pub fn native_name(self) -> &'static str {
    match self {
      Self::En => "English",
      Self::De => "Deutsch",
    }
  }

  pub fn from_tag(tag: &str) -> Option<Self> {
    let primary = tag.split(['-', '_']).next().unwrap_or(tag).to_ascii_lowercase();
    Self::ALL.into_iter().find(|locale| locale.tag() == primary)
  }
}

/// Reactive translator. Cheap to copy; reading it inside a closure re-renders when the language changes.
#[derive(Clone, Copy)]
pub struct I18n {
  /// The language the UI shows: always one whose texts are there.
  locale: RwSignal<Locale>,
  /// The language last asked for; it replaces `locale` once its texts are there (natively at once).
  #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
  wanted: RwSignal<Locale>,
}

impl I18n {
  pub fn new(initial: Locale) -> Self {
    // The web app starts with its stored language's texts (`load_start_language`); should they
    // be missing, a language that is there shows until they arrive.
    let shown = if is_loaded(initial) {
      initial
    } else {
      loaded_locale().unwrap_or(initial)
    };
    let i18n = Self {
      locale: RwSignal::new(shown),
      wanted: RwSignal::new(initial),
    };
    if shown != initial {
      i18n.set_locale(initial);
    }
    i18n
  }

  pub fn locale(&self) -> Locale {
    get_if_observed(self.locale)
  }

  /// Switches to `locale` once its texts are there: at once if they are, else (web) after fetching
  /// them. Until then the UI stays in the language it shows, never with keys in place of texts; a
  /// fetch that fails leaves it there too.
  pub fn set_locale(&self, locale: Locale) {
    self.wanted.set(locale);
    if is_loaded(locale) {
      self.locale.set(locale);
    } else {
      self.fetch_then_show(locale);
    }
  }

  #[cfg(target_arch = "wasm32")]
  fn fetch_then_show(self, locale: Locale) {
    wasm_bindgen_futures::spawn_local(async move {
      match web::load(locale).await {
        Ok(()) => self.arrived(locale),
        Err(error) => tracing::warn!(locale = locale.tag(), %error, "the language's texts did not load"),
      }
    });
  }

  /// Natively every language is there.
  #[cfg(not(target_arch = "wasm32"))]
  fn fetch_then_show(self, _locale: Locale) {}

  /// `locale`'s texts are there: the UI shows them if it is still the language asked for.
  #[cfg(any(target_arch = "wasm32", test))]
  fn arrived(self, locale: Locale) {
    if self.wanted.try_get_untracked() == Some(locale) && is_loaded(locale) {
      self.locale.try_set(locale);
    }
  }

  /// Translates a message without arguments.
  pub fn t(&self, key: &str) -> String {
    translate(get_if_observed(self.locale), key, None)
  }

  /// Translates a message with string arguments, e.g. `t_with("greeting", &[("name", "Sam")])`.
  pub fn t_with(&self, key: &str, args: &[(&str, &str)]) -> String {
    let mut fluent_args = FluentArgs::new();
    for (name, value) in args {
      fluent_args.set(name.to_string(), value.to_string());
    }
    translate(get_if_observed(self.locale), key, Some(&fluent_args))
  }

  /// Translates a message that selects a plural form from `$count`.
  pub fn t_count(&self, key: &str, count: usize) -> String {
    let mut fluent_args = FluentArgs::new();
    fluent_args.set("count", count);
    translate(get_if_observed(self.locale), key, Some(&fluent_args))
  }

  /// Translates a message that selects a plural form from `$count` and has further arguments.
  pub fn t_count_with(&self, key: &str, count: usize, args: &[(&str, &str)]) -> String {
    let mut fluent_args = FluentArgs::new();
    fluent_args.set("count", count);
    for (name, value) in args {
      fluent_args.set(name.to_string(), value.to_string());
    }
    translate(get_if_observed(self.locale), key, Some(&fluent_args))
  }
}

pub fn provide_i18n(initial: Locale) -> I18n {
  let i18n = I18n::new(initial);
  provide_context(i18n);
  i18n
}

pub fn use_i18n() -> I18n {
  expect_context::<I18n>()
}

/// Translates a message without arguments in the UI's language, for code outside components that
/// may run without the translator in reach (then English).
pub fn translate_current(key: &str) -> String {
  let locale = use_context::<I18n>().map_or(Locale::En, |i18n| i18n.locale());
  translate(locale, key, None)
}

pub(crate) fn translate(locale: Locale, key: &str, maybe_args: Option<&FluentArgs>) -> String {
  BUNDLES
    .with_borrow(|bundles| bundles.translate(locale, key, maybe_args))
    .unwrap_or_else(|| {
      tracing::warn!(key, "missing translation key");
      key.to_owned()
    })
}

/// Whether the texts of `locale` are there (natively always).
fn is_loaded(locale: Locale) -> bool {
  BUNDLES.with_borrow(|bundles| bundles.get(locale).is_some())
}

/// A language whose texts are there, English first.
fn loaded_locale() -> Option<Locale> {
  Locale::ALL.into_iter().find(|locale| is_loaded(*locale))
}

/// Builds the bundle of `locale` from `source` (all of the language's files as one text, as
/// `scripts/finish-loader.sh` joins them) and keeps it.
#[cfg(target_arch = "wasm32")]
fn install(locale: Locale, source: &str) {
  let bundle = build_bundle(locale, [("all files", source)]);
  BUNDLES.with_borrow_mut(|bundles| *bundles.slot(locale) = Some(bundle));
}

/// Before the app mounts (web): the texts of the language it starts in, which `init.js` fetched
/// next to the app (`globalThis.teamcraftTranslations`). Should the app's stored choice be another
/// language than `init.js` picked (they read the same setting), that one is fetched too. An error
/// means no texts at all: `init.js` then shows the loading screen's failure with its retry.
#[cfg(target_arch = "wasm32")]
pub async fn load_start_language() -> Result<(), String> {
  web::load_start().await?;
  let wanted = crate::settings::stored_locale();
  if !is_loaded(wanted)
    && let Err(error) = web::load(wanted).await
  {
    tracing::warn!(locale = wanted.tag(), %error, "the language's texts did not load");
  }
  Ok(())
}

/// Natively every language is embedded.
#[cfg(not(target_arch = "wasm32"))]
pub async fn load_start_language() -> Result<(), String> {
  Ok(())
}

thread_local! {
  static BUNDLES: RefCell<Bundles> = RefCell::new(Bundles::embedded());
}

/// The bundle of each language whose texts are there.
#[derive(Default)]
struct Bundles {
  en: Option<MessageBundle<FluentResource>>,
  de: Option<MessageBundle<FluentResource>>,
}

impl Bundles {
  #[cfg(any(not(target_arch = "wasm32"), test))]
  fn embedded() -> Self {
    let mut bundles = Self::default();
    for locale in Locale::ALL {
      *bundles.slot(locale) = Some(build_bundle(locale, sources(locale.tag()).iter().copied()));
    }
    bundles
  }

  /// The web app embeds none: they are fetched ([`load_start_language`]).
  #[cfg(all(target_arch = "wasm32", not(test)))]
  fn embedded() -> Self {
    Self::default()
  }

  fn get(&self, locale: Locale) -> Option<&MessageBundle<FluentResource>> {
    match locale {
      Locale::En => self.en.as_ref(),
      Locale::De => self.de.as_ref(),
    }
  }

  fn slot(&mut self, locale: Locale) -> &mut Option<MessageBundle<FluentResource>> {
    match locale {
      Locale::En => &mut self.en,
      Locale::De => &mut self.de,
    }
  }

  /// The message in `locale`, else in English (a translation that has not caught up yet), else in
  /// any language that is there (`locale` not fetched yet); `None` when no language has it.
  fn translate(&self, locale: Locale, key: &str, maybe_args: Option<&FluentArgs>) -> Option<String> {
    let candidates = [locale].into_iter().chain(Locale::ALL);
    for bundle in candidates.filter_map(|candidate| self.get(candidate)) {
      if let Some(pattern) = bundle.get_message(key).and_then(|message| message.value()) {
        let mut errors = Vec::new();
        return Some(
          bundle
            .format_pattern(pattern, maybe_args, &mut errors)
            .into_owned(),
        );
      }
    }
    None
  }
}

/// The bundle of `locale` from its files (`(file name, source)`).
fn build_bundle<'a>(
  locale: Locale,
  files: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> MessageBundle<FluentResource> {
  let langid: LanguageIdentifier = locale.tag().parse().unwrap_or_default();
  let mut bundle = MessageBundle::new(vec![langid]);
  // Isolation marks confuse text selection and tests; the UI sets `dir` itself.
  bundle.set_use_isolating(false);
  for (file, source) in files {
    // `FluentResource::try_new` returns the partial resource together with the errors; keep what parsed.
    let resource = match FluentResource::try_new(source.to_owned()) {
      Ok(resource) => resource,
      Err((resource, errors)) => {
        tracing::error!(
          locale = locale.tag(),
          file,
          error_count = errors.len(),
          "invalid Fluent resource"
        );
        resource
      },
    };
    // An id defined in two files keeps its first definition (the tests rule that out).
    if bundle.add_resource(resource).is_err() {
      tracing::error!(
        locale = locale.tag(),
        file,
        "duplicate message ids in Fluent resource"
      );
    }
  }
  bundle
}

/// The web app's texts: the start language from `init.js`, others fetched when asked for.
#[cfg(target_arch = "wasm32")]
mod web {
  use js_sys::{Promise, Reflect};
  use wasm_bindgen::{JsCast, JsValue};
  use wasm_bindgen_futures::JsFuture;

  use super::{Locale, install};

  /// `init.js`'s fetch of the start language: a promise of `{ tag, text }`.
  const START: &str = "teamcraftTranslations";
  /// Each language's file of this build (`{ tag: url }`, scripts/finish-loader.sh).
  const FILES: &str = "teamcraftTranslationFiles";

  pub(super) async fn load_start() -> Result<(), String> {
    let promise = Reflect::get(&js_sys::global(), &JsValue::from_str(START))
      .ok()
      .and_then(|value| value.dyn_into::<Promise>().ok())
      .ok_or("init.js fetched no texts")?;
    let start = JsFuture::from(promise).await.map_err(describe)?;
    let field = |name: &str| {
      Reflect::get(&start, &JsValue::from_str(name))
        .ok()
        .and_then(|value| value.as_string())
    };
    let locale = field("tag")
      .and_then(|tag| Locale::from_tag(&tag))
      .ok_or("init.js fetched texts of no known language")?;
    let text = field("text").ok_or("init.js fetched no text")?;
    install(locale, &text);
    Ok(())
  }

  pub(super) async fn load(locale: Locale) -> Result<(), String> {
    let files = Reflect::get(&js_sys::global(), &JsValue::from_str(FILES)).map_err(describe)?;
    let url = Reflect::get(&files, &JsValue::from_str(locale.tag()))
      .ok()
      .and_then(|value| value.as_string())
      .ok_or("this build names no file for the language")?;
    let window = web_sys::window().ok_or("no window")?;
    let response: web_sys::Response = JsFuture::from(window.fetch_with_str(&url))
      .await
      .map_err(describe)?
      .dyn_into()
      .map_err(describe)?;
    // A file the server no longer has comes back as the app's page (its fallback), with 200.
    let is_page = response
      .headers()
      .get("content-type")
      .ok()
      .flatten()
      .is_some_and(|kind| kind.starts_with("text/html"));
    if !response.ok() || is_page {
      return Err(format!("{url}: HTTP {}", response.status()));
    }
    let text = JsFuture::from(response.text().map_err(describe)?)
      .await
      .map_err(describe)?
      .as_string()
      .ok_or("the file is no text")?;
    install(locale, &text);
    Ok(())
  }

  fn describe(error: JsValue) -> String {
    error
      .as_string()
      .or_else(|| {
        error
          .dyn_ref::<js_sys::Error>()
          .map(|error| String::from(error.message()))
      })
      .unwrap_or_else(|| format!("{error:?}"))
  }
}

/// All Fluent files of `locale` as one text, for tests that look for messages in the sources.
#[cfg(test)]
pub(crate) fn joined_source(locale: Locale) -> String {
  sources(locale.tag()).iter().map(|(_, source)| *source).collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn translations_parse_without_errors() {
    for locale in Locale::ALL {
      assert!(
        !sources(locale.tag()).is_empty(),
        "no Fluent files for {locale:?}"
      );
      for (file, source) in sources(locale.tag()) {
        assert!(
          FluentResource::try_new((*source).to_owned()).is_ok(),
          "i18n/{}/{file} doesn't parse",
          locale.tag()
        );
      }
    }
  }

  /// A duplicate makes `add_resource` fail and logs an error at startup, also when the two
  /// definitions are in different area files.
  #[test]
  fn message_ids_are_unique_across_files() {
    for locale in Locale::ALL {
      let mut seen: std::collections::BTreeMap<String, &str> = Default::default();
      for (file, source) in sources(locale.tag()) {
        for id in message_ids(source) {
          if let Some(first) = seen.insert(id.clone(), file) {
            panic!(
              "{id} is defined in i18n/{0}/{first} and i18n/{0}/{file}",
              locale.tag()
            );
          }
        }
      }
    }
  }

  #[test]
  fn german_covers_every_english_message() {
    let german = message_ids(&joined_source(Locale::De));
    let missing: Vec<String> = message_ids(&joined_source(Locale::En))
      .into_iter()
      .filter(|id| !german.contains(id))
      .collect();
    assert!(missing.is_empty(), "missing in i18n/de: {missing:?}");
  }

  #[test]
  fn german_has_no_messages_english_lacks() {
    let english = message_ids(&joined_source(Locale::En));
    let orphans: Vec<String> = message_ids(&joined_source(Locale::De))
      .into_iter()
      .filter(|id| !english.contains(id))
      .collect();
    assert!(
      orphans.is_empty(),
      "only in i18n/de (stale or misspelled): {orphans:?}"
    );
  }

  /// Every language has the same area files as English, each with the same messages, so a
  /// translation is found (and missed) where its English text is.
  #[test]
  fn every_language_has_the_english_area_files() {
    let files_of = |locale: Locale| -> std::collections::BTreeMap<&str, Vec<String>> {
      sources(locale.tag())
        .iter()
        .map(|(file, source)| {
          let mut ids = message_ids(source);
          ids.sort();
          (*file, ids)
        })
        .collect()
    };
    let english = files_of(Locale::En);
    for locale in Locale::ALL {
      let translated = files_of(locale);
      assert_eq!(
        translated.keys().collect::<Vec<_>>(),
        english.keys().collect::<Vec<_>>(),
        "i18n/{} has other files than i18n/en",
        locale.tag()
      );
      for (file, ids) in &english {
        assert_eq!(
          &translated[file],
          ids,
          "i18n/{}/{file} has other messages than i18n/en/{file}",
          locale.tag()
        );
      }
    }
  }

  /// A translation that drops or renames a `$placeholder` shows wrong or empty text.
  #[test]
  fn translations_use_the_same_placeholders() {
    let german = placeholders(&joined_source(Locale::De));
    for (id, english_vars) in placeholders(&joined_source(Locale::En)) {
      if let Some(german_vars) = german.get(&id) {
        assert_eq!(
          &english_vars, german_vars,
          "placeholders of {id} differ between en and de"
        );
      }
    }
  }

  /// The web app has only the language it shows (#1437), as one text of all its files: every
  /// message of English resolves from it, whichever language a caller asks for, so nothing that
  /// renders before another language arrives shows a key.
  #[test]
  fn one_language_alone_has_every_message() {
    let english = message_ids(&joined_source(Locale::En));
    for locale in Locale::ALL {
      let mut bundles = Bundles::default();
      *bundles.slot(locale) = Some(build_bundle(
        locale,
        [("all files", joined_source(locale).as_str())],
      ));
      for asked in Locale::ALL {
        for id in &english {
          // Messages with only attributes have no value; none of the UI's do.
          assert!(
            bundles.translate(asked, id, None).is_some(),
            "{id} is missing when only {locale:?} is there ({asked:?} asked)"
          );
        }
      }
    }
  }

  /// A language switch shows the new language only once its texts are there, and a switch back
  /// before they arrive wins: the UI never shows keys and never jumps to a language left behind.
  #[test]
  fn a_switch_waits_for_the_languages_texts() {
    let german = BUNDLES.with_borrow_mut(|bundles| bundles.de.take());
    let i18n = I18n::new(Locale::De);
    assert_eq!(i18n.locale(), Locale::En, "starts in a language that is there");
    assert_eq!(i18n.t("settings-language"), "Language");
    BUNDLES.with_borrow_mut(|bundles| bundles.de = german);
    i18n.arrived(Locale::De);
    assert_eq!(i18n.locale(), Locale::De);
    assert_eq!(i18n.t("settings-language"), "Sprache");

    let german = BUNDLES.with_borrow_mut(|bundles| bundles.de.take());
    i18n.set_locale(Locale::En);
    i18n.set_locale(Locale::De);
    assert_eq!(i18n.locale(), Locale::En, "stays until German is there");
    i18n.set_locale(Locale::En);
    BUNDLES.with_borrow_mut(|bundles| bundles.de = german);
    i18n.arrived(Locale::De);
    assert_eq!(i18n.locale(), Locale::En, "German arrived after the switch back");
  }

  #[test]
  fn plural_and_arguments_resolve() {
    let en = translate(Locale::En, "thread-replies", Some(&count_args(1)));
    let en_many = translate(Locale::En, "thread-replies", Some(&count_args(3)));
    assert_eq!(en, "1 reply");
    assert_eq!(en_many, "3 replies");
    assert_eq!(
      translate(Locale::De, "thread-replies", Some(&count_args(2))),
      "2 Antworten"
    );
  }

  #[test]
  fn sidebar_rows_say_how_many_messages_are_unread() {
    let say = |locale, key, count: i64, unread: &str| {
      let mut args = FluentArgs::new();
      args.set("name", "Team");
      args.set("unread", unread);
      args.set("count", count);
      translate(locale, key, Some(&args))
    };
    assert_eq!(
      say(Locale::En, "sidebar-item-unread", 1, ""),
      "Team, 1 unread message"
    );
    assert_eq!(
      say(Locale::En, "sidebar-item-unread", 120, ""),
      "Team, 120 unread messages"
    );
    assert_eq!(
      say(Locale::En, "sidebar-item-unread-mentions", 2, "5"),
      "Team, 5 unread messages, 2 mentions"
    );
    assert_eq!(
      say(Locale::De, "sidebar-item-unread", 1, ""),
      "Team, 1 ungelesene Nachricht"
    );
    assert_eq!(
      say(Locale::De, "sidebar-item-unread-mentions", 1, "3"),
      "Team, 3 ungelesene Nachrichten, 1 Erwähnung"
    );
  }

  #[test]
  fn unknown_key_falls_back_to_the_key() {
    assert_eq!(translate(Locale::De, "no-such-key", None), "no-such-key");
  }

  /// Keys the admin console builds from permission, role and provider names must all exist.
  #[test]
  fn console_keys_built_from_names_exist() {
    let mut keys = Vec::new();
    for permission in teamcraft_proto::ALL_PERMISSIONS {
      keys.push(format!("permission-{}", permission.as_str()));
      keys.push(format!("permission-{}-hint", permission.as_str()));
    }
    for role in [
      "guest",
      "member",
      "space_admin",
      "channel_admin",
      "admin",
      "owner",
    ] {
      keys.push(format!("permissions-role-{role}"));
    }
    for group in ["content", "channel", "space", "instance"] {
      keys.push(format!("permissions-group-{group}"));
    }
    for provider in ["webpush", "fcm", "unifiedpush", "apns"] {
      keys.push(format!("instance-provider-{provider}"));
    }
    for state in ["active", "not-configured", "failing", "unknown"] {
      keys.push(format!("instance-push-{state}"));
    }
    for health in ["off", "unknown", "ok", "silent"] {
      keys.push(format!("instance-sfu-{health}"));
    }
    for level in ["info", "warning", "critical"] {
      keys.push(format!("announcement-level-{level}"));
    }
    for mode in ["generic", "sender", "full"] {
      keys.push(format!("policy-push-{mode}"));
      keys.push(format!("policy-push-{mode}-hint"));
    }
    for name in ["as_provided", "first_last", "last_first", "username"] {
      keys.push(format!("policy-name-{name}"));
    }
    for action in teamcraft_client_api::admin::AuditAction::ALL {
      keys.push(format!("audit-{}", action.slug()));
    }
    for section in teamcraft_client_api::admin::AdminSection::ALL {
      keys.push(format!("admin-nav-{}", section.slug()));
    }
    for locale in [Locale::En, Locale::De] {
      for key in &keys {
        assert_ne!(&translate(locale, key, None), key, "{locale:?} lacks {key}");
      }
    }
  }

  /// Every key the lightbox and the file views ask for exists in each language.
  #[test]
  fn lightbox_and_file_keys_exist() {
    let keys: Vec<String> = [
      include_str!("components/lightbox.rs"),
      include_str!("components/lightbox_controls.rs"),
      include_str!("components/file_views.rs"),
    ]
    .into_iter()
    .flat_map(keys_in_source)
    .collect();
    for expected in [
      "lightbox-save",
      "lightbox-saving",
      "lightbox-shortcut",
      "lightbox-details-dimensions-value",
      "media-open",
      "file-upload-stalled",
      "file-too-large",
    ] {
      assert!(
        keys.iter().any(|key| key == expected),
        "the scan missed {expected}"
      );
    }
    for locale in [Locale::En, Locale::De] {
      for key in &keys {
        assert_ne!(&translate(locale, key, None), key, "{locale:?} lacks {key}");
      }
    }
  }

  /// Every key the settings sections ask for exists in each language: the `.t(..)` calls, the
  /// `label`, `hint` and `heading` of their rows and cards, and what the settings search reads.
  #[test]
  fn settings_keys_exist() {
    let sources = [
      include_str!("components/settings_account.rs"),
      include_str!("components/settings_keyboard.rs"),
      include_str!("components/settings_modal.rs"),
      include_str!("components/settings_notifications.rs"),
      include_str!("components/settings_security.rs"),
      include_str!("components/settings_tabs.rs"),
    ];
    let mut keys: Vec<String> = sources.into_iter().flat_map(keys_in_source).collect();
    for source in sources {
      for marker in [
        "label=\"",
        "hint=\"",
        "heading=\"",
        "heading_key: \"",
        ".t_count(\"",
      ] {
        for (at, _) in source.match_indices(marker) {
          // `enterkeyhint="…"` is no key: only attributes that start a word count.
          let starts_word = source[..at].ends_with(char::is_whitespace) || marker.starts_with('.');
          let rest = &source[at + marker.len()..];
          if let (true, Some(end)) = (starts_word, rest.find('"')) {
            keys.push(rest[..end].to_owned());
          }
        }
      }
    }
    keys.extend(crate::components::settings_search::every_key());
    assert!(
      keys.iter().any(|key| key == "notif-master"),
      "the scan missed rows"
    );
    for locale in [Locale::En, Locale::De] {
      for key in &keys {
        assert_ne!(&translate(locale, key, None), key, "{locale:?} lacks {key}");
      }
    }
  }

  /// Every key any component asks for exists in each language: the `.t`, `.t_with` and `.t_count`
  /// calls, and the key props components hand on (`label="…"` of a toolbar button, `title_key`,
  /// `body_key`, …). The per-area scans above miss keys that only travel through a prop, as
  /// `file-copy-share` did (#742).
  #[test]
  fn every_key_the_code_names_exists() {
    let mut sources = Vec::new();
    collect_sources(
      &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
      &mut sources,
    );
    assert!(sources.len() > 50, "the scan found the sources");
    let mut missing = Vec::new();
    for (path, source) in &sources {
      // Test modules name keys that must not exist (`no-such-key`).
      let code = source.split("#[cfg(test)]").next().unwrap_or_default();
      for key in named_keys(code) {
        for locale in [Locale::En, Locale::De] {
          if translate(locale, &key, None) == key {
            missing.push(format!("{} lacks {key} ({locale:?})", path.display()));
          }
        }
      }
    }
    assert!(missing.is_empty(), "keys without a translation: {missing:#?}");
  }

  #[test]
  fn the_key_scan_finds_calls_and_key_props() {
    let source = r#"
      i18n.t("a-key"); i18n.t_with(
        "b-key", &[]); i18n.t_count("c-key", 2);
      <ToolbarButton label="d-key" />
      <EmptyState title_key="e-key" body_key="f-key" />
      EmptyAction { label_key: "g-key" }
      <input enterkeyhint="send" aria-label="plain words" />
      i18n.t(&format!("level-{}", slug));
    "#;
    assert_eq!(
      named_keys(source),
      ["a-key", "b-key", "c-key", "d-key", "e-key", "f-key", "g-key"]
    );
  }

  /// The literal i18n keys `source` names (built keys like `format!(…)` aren't literals).
  fn named_keys(source: &str) -> Vec<String> {
    let mut found: Vec<(usize, String)> = Vec::new();
    for marker in [".t(", ".t_with(", ".t_count("] {
      for (at, _) in source.match_indices(marker) {
        let rest = source[at + marker.len()..].trim_start();
        if let Some(quoted) = rest.strip_prefix('"')
          && let Some(end) = quoted.find('"')
        {
          found.push((at, quoted[..end].to_owned()));
        }
      }
    }
    for marker in [
      "label=\"",
      "title_key=\"",
      "body_key=\"",
      "placeholder_key=\"",
      "label_key: \"",
      "title_key: \"",
      "body_key: \"",
      "heading_key: \"",
    ] {
      for (at, _) in source.match_indices(marker) {
        // `aria-label="…"` or `enterkeyhint="…"` are no keys: only props that start a word count.
        let starts_word = source[..at].ends_with(|c: char| c.is_whitespace() || c == '{');
        let rest = &source[at + marker.len()..];
        if let (true, Some(end)) = (starts_word, rest.find('"')) {
          found.push((at, rest[..end].to_owned()));
        }
      }
    }
    found.sort();
    found
      .into_iter()
      .map(|(_, key)| key)
      .filter(|key| is_key_like(key))
      .collect()
  }

  /// Keys are lowercase words joined by dashes; anything else is plain text or a value.
  fn is_key_like(text: &str) -> bool {
    text.contains('-')
      && text
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
  }

  fn collect_sources(directory: &std::path::Path, sources: &mut Vec<(std::path::PathBuf, String)>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
      return;
    };
    for entry in entries.flatten() {
      let path = entry.path();
      if path.is_dir() {
        collect_sources(&path, sources);
      } else if path.extension().is_some_and(|extension| extension == "rs")
        && let Ok(source) = std::fs::read_to_string(&path)
      {
        sources.push((path, source));
      }
    }
  }

  #[test]
  fn locale_tags_parse() {
    assert_eq!(Locale::from_tag("de-DE"), Some(Locale::De));
    assert_eq!(Locale::from_tag("en_US"), Some(Locale::En));
    assert_eq!(Locale::from_tag("fr"), None);
  }

  fn count_args(count: usize) -> FluentArgs<'static> {
    let mut args = FluentArgs::new();
    args.set("count", count);
    args
  }

  /// The `$variables` each message uses, including the lines of its plural branches.
  fn placeholders(source: &str) -> std::collections::BTreeMap<String, std::collections::BTreeSet<String>> {
    let mut found: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
      Default::default();
    let mut current: Option<String> = None;
    for line in source.lines() {
      if !line.starts_with([' ', '#', '-', '.'])
        && let Some((id, _)) = line.split_once(" =")
      {
        current = Some(id.trim().to_owned());
        found.entry(id.trim().to_owned()).or_default();
      }
      let Some(id) = &current else { continue };
      let mut rest = line;
      while let Some(position) = rest.find('$') {
        let name: String = rest[position + 1..]
          .chars()
          .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
          .collect();
        if !name.is_empty() {
          found.entry(id.clone()).or_default().insert(name);
        }
        rest = &rest[position + 1..];
      }
    }
    found
  }

  /// The literal keys of `.t("…")`, `.t_with("…", …)` and of match arms that pick a key (`=> "…"`).
  fn keys_in_source(source: &str) -> Vec<String> {
    let mut keys = Vec::new();
    // rustfmt may break a long call after its parenthesis: the key starts after the whitespace.
    for marker in [".t(", ".t_with(", "=> \""] {
      for (at, _) in source.match_indices(marker) {
        let rest = &source[at + marker.len()..];
        let rest = match marker.ends_with('"') {
          true => rest,
          false => match rest.trim_start().strip_prefix('"') {
            Some(quoted) => quoted,
            None => continue,
          },
        };
        if let Some(end) = rest.find('"') {
          keys.push(rest[..end].to_owned());
        }
      }
    }
    keys
  }

  fn message_ids(source: &str) -> Vec<String> {
    source
      .lines()
      .filter(|line| !line.starts_with([' ', '#', '-', '.']))
      .filter_map(|line| line.split_once(" =").map(|(id, _)| id.trim().to_owned()))
      .filter(|id| !id.is_empty())
      .collect()
  }
}
