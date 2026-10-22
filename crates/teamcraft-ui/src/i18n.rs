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

fn translate(locale: Locale, key: &str, maybe_args: Option<&FluentArgs>) -> String {
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
  fn german_covers_every_english_message() {
    let german = message_ids(DE_SOURCE);
    let missing: Vec<String> = message_ids(EN_SOURCE)
      .into_iter()
      .filter(|id| !german.contains(id))
      .collect();
    assert!(missing.is_empty(), "missing in de.ftl: {missing:?}");
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

  fn message_ids(source: &str) -> Vec<String> {
    source
      .lines()
      .filter(|line| !line.starts_with([' ', '#', '-', '.']))
      .filter_map(|line| line.split_once(" =").map(|(id, _)| id.trim().to_owned()))
      .filter(|id| !id.is_empty())
      .collect()
  }
}
