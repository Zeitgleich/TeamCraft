//! Localisation with Fluent. English is the source language; every user-facing string goes through
//! [`I18n::t`] and friends. Translation files live in `i18n/*.ftl`.

use fluent_bundle::FluentBundle as MessageBundle;
use fluent_bundle::{FluentArgs, FluentResource};
use leptos::prelude::*;
use unic_langid::LanguageIdentifier;

use crate::reactive::get_if_observed;

const EN_SOURCE: &str = include_str!("../i18n/en.ftl");
const DE_SOURCE: &str = include_str!("../i18n/de.ftl");

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
  locale: RwSignal<Locale>,
}

impl I18n {
  pub fn new(initial: Locale) -> Self {
    Self {
      locale: RwSignal::new(initial),
    }
  }

  pub fn locale(&self) -> Locale {
    get_if_observed(self.locale)
  }

  pub fn set_locale(&self, locale: Locale) {
    self.locale.set(locale);
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
  BUNDLES.with(|bundles| {
    // Fall back to English for messages a translation has not caught up with yet.
    for candidate in [locale, Locale::En] {
      let bundle = bundles.get(candidate);
      if let Some(pattern) = bundle.get_message(key).and_then(|message| message.value()) {
        let mut errors = Vec::new();
        return bundle
          .format_pattern(pattern, maybe_args, &mut errors)
          .into_owned();
      }
    }
    tracing::warn!(key, "missing translation key");
    key.to_owned()
  })
}

thread_local! {
  static BUNDLES: Bundles = Bundles::load();
}

struct Bundles {
  en: MessageBundle<FluentResource>,
  de: MessageBundle<FluentResource>,
}

impl Bundles {
  fn load() -> Self {
    Self {
      en: build_bundle(Locale::En, EN_SOURCE),
      de: build_bundle(Locale::De, DE_SOURCE),
    }
  }

  fn get(&self, locale: Locale) -> &MessageBundle<FluentResource> {
    match locale {
      Locale::En => &self.en,
      Locale::De => &self.de,
    }
  }
}

fn build_bundle(locale: Locale, source: &str) -> MessageBundle<FluentResource> {
  let langid: LanguageIdentifier = locale.tag().parse().unwrap_or_default();
  let mut bundle = MessageBundle::new(vec![langid]);
  // Isolation marks confuse text selection and tests; the UI sets `dir` itself.
  bundle.set_use_isolating(false);
  // `FluentResource::try_new` returns the partial resource together with the errors; keep what parsed.
  let resource = match FluentResource::try_new(source.to_owned()) {
    Ok(resource) => resource,
    Err((resource, errors)) => {
      tracing::error!(
        locale = locale.tag(),
        error_count = errors.len(),
        "invalid Fluent resource"
      );
      resource
    },
  };
  if bundle.add_resource(resource).is_err() {
    tracing::error!(locale = locale.tag(), "duplicate message ids in Fluent resource");
  }
  bundle
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn translations_parse_without_errors() {
    for source in [EN_SOURCE, DE_SOURCE] {
      assert!(FluentResource::try_new(source.to_owned()).is_ok());
    }
  }

  #[test]
  fn message_ids_are_unique_per_file() {
    // A duplicate makes `add_resource` fail and logs an error at startup.
    for (name, source) in [("en", EN_SOURCE), ("de", DE_SOURCE)] {
      let ids = message_ids(source);
      let mut sorted = ids.clone();
      sorted.sort();
      sorted.dedup();
      assert_eq!(sorted.len(), ids.len(), "duplicate message ids in {name}.ftl");
    }
  }

  #[test]
  fn german_covers_every_english_message() {
    let german = message_ids(DE_SOURCE);
    let missing: Vec<String> = message_ids(EN_SOURCE)
      .into_iter()
      .filter(|id| !german.contains(id))
      .collect();
    assert!(missing.is_empty(), "missing in de.ftl: {missing:?}");
  }

  #[test]
  fn german_has_no_messages_english_lacks() {
    let english = message_ids(EN_SOURCE);
    let orphans: Vec<String> = message_ids(DE_SOURCE)
      .into_iter()
      .filter(|id| !english.contains(id))
      .collect();
    assert!(
      orphans.is_empty(),
      "only in de.ftl (stale or misspelled): {orphans:?}"
    );
  }

  /// A translation that drops or renames a `$placeholder` shows wrong or empty text.
  #[test]
  fn translations_use_the_same_placeholders() {
    let german = placeholders(DE_SOURCE);
    for (id, english_vars) in placeholders(EN_SOURCE) {
      if let Some(german_vars) = german.get(&id) {
        assert_eq!(
          &english_vars, german_vars,
          "placeholders of {id} differ between en and de"
        );
      }
    }
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
