//! The texts a bot posts, in German or English (English for everything else), from Fluent files
//! like the app's (`i18n/*.ftl` of each bot).

use fluent_bundle::{FluentArgs, FluentBundle, FluentResource, FluentValue};
use tracing::warn;
use unic_langid::LanguageIdentifier;

/// One language's texts.
pub struct Texts {
  bundle: FluentBundle<FluentResource>,
  fallback: FluentBundle<FluentResource>,
}

/// A bot's translations: English, the source language and the fallback, and German.
pub struct TextSources {
  pub en: &'static str,
  pub de: &'static str,
}

impl Texts {
  /// The texts for `language` (`de`, `en`, `auto` or another code): German for `de`, English for
  /// everything else.
  pub fn for_language(language: &str, sources: &TextSources) -> Self {
    let source = if language == "de" { sources.de } else { sources.en };
    Self {
      bundle: bundle(language, source),
      fallback: bundle("en", sources.en),
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

/// The message ids of a Fluent source, for a test that both languages have the same ones.
pub fn message_ids(source: &str) -> std::collections::BTreeSet<String> {
  source
    .lines()
    .filter_map(|line| line.split_once(" = ").map(|(id, _)| id))
    .filter(|id| id.chars().all(|c| c.is_ascii_lowercase() || c == '-'))
    .map(str::to_owned)
    .collect()
}

/// Whether a Fluent source parses without errors.
pub fn parses(source: &str) -> bool {
  FluentResource::try_new(source.to_owned()).is_ok()
}
