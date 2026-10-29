//! The texts the notetaker posts, in the policy's language (German or English; English for
//! everything else, as the summary model writes the notes themselves in the meeting's language).
//! Translations live in `i18n/*.ftl` like the app's.

use fluent_bundle::{FluentArgs, FluentBundle, FluentResource, FluentValue};
use tracing::warn;
use unic_langid::LanguageIdentifier;

const EN: &str = include_str!("../i18n/en.ftl");
const DE: &str = include_str!("../i18n/de.ftl");

/// One language's texts.
pub struct Texts {
  bundle: FluentBundle<FluentResource>,
  fallback: FluentBundle<FluentResource>,
}

impl Texts {
  /// The texts for the policy's `language` (`de`, `en`, `auto` or another code).
  pub fn for_language(language: &str) -> Self {
    let source = if language == "de" { DE } else { EN };
    Self {
      bundle: bundle(language, source),
      fallback: bundle("en", EN),
    }
  }

  /// A text without arguments.
  pub fn get(&self, key: &str) -> String {
    self.format(key, &[])
  }

  /// A text with string or number arguments.
  pub fn format(&self, key: &str, args: &[(&str, TextArg<'_>)]) -> String {
    let mut fluent_args = FluentArgs::new();
    for (name, value) in args {
      match value {
        TextArg::Text(text) => fluent_args.set(*name, FluentValue::from(*text)),
        TextArg::Number(number) => fluent_args.set(*name, FluentValue::from(*number)),
      }
    }
    for bundle in [&self.bundle, &self.fallback] {
      if let Some(pattern) = bundle.get_message(key).and_then(|message| message.value()) {
        let mut errors = Vec::new();
        return bundle
          .format_pattern(pattern, Some(&fluent_args), &mut errors)
          .into_owned();
      }
    }
    warn!(key, "missing text");
    key.to_owned()
  }
}

/// An argument of a text.
#[derive(Debug, Clone, Copy)]
pub enum TextArg<'a> {
  Text(&'a str),
  Number(u64),
}

fn bundle(language: &str, source: &str) -> FluentBundle<FluentResource> {
  let langid: LanguageIdentifier = language.parse().unwrap_or_default();
  let mut bundle = FluentBundle::new(vec![langid]);
  // Isolation marks would end up in the posted messages.
  bundle.set_use_isolating(false);
  let resource = FluentResource::try_new(source.to_owned()).unwrap_or_else(|(resource, errors)| {
    warn!(error_count = errors.len(), "invalid Fluent resource");
    resource
  });
  if bundle.add_resource(resource).is_err() {
    warn!("duplicate message ids in a Fluent resource");
  }
  bundle
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeSet;

  use super::*;

  #[test]
  fn both_languages_have_the_same_messages_and_parse() {
    let ids = |source: &str| -> BTreeSet<String> {
      assert!(FluentResource::try_new(source.to_owned()).is_ok());
      source
        .lines()
        .filter_map(|line| line.split_once(" = ").map(|(id, _)| id))
        .filter(|id| id.chars().all(|c| c.is_ascii_lowercase() || c == '-'))
        .map(str::to_owned)
        .collect()
    };
    assert_eq!(ids(EN), ids(DE));
    assert!(ids(EN).len() > 15);
  }

  #[test]
  fn german_and_the_english_fallback() {
    let german = Texts::for_language("de");
    assert_eq!(
      german.format("participant-fallback", &[("number", TextArg::Number(2))]),
      "Teilnehmer 2"
    );
    let french = Texts::for_language("fr");
    assert_eq!(french.get("reason-shutdown"), "it was restarted");
    assert_eq!(
      french.format("result-gaps", &[("count", TextArg::Number(1))]),
      "One piece of speech could not be transcribed."
    );
    assert_eq!(french.get("no-such-text"), "no-such-text");
  }
}
