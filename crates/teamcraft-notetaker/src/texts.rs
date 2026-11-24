//! The texts the notetaker posts, in the policy's language (German or English; English for
//! everything else, as the summary model writes the notes themselves in the meeting's language).
//! Translations live in `i18n/*.ftl` like the app's.

pub use teamcraft_bot_runtime::texts::{TextArg, TextSources, Texts};

/// The notetaker's translations.
pub const SOURCES: TextSources = TextSources {
  en: include_str!("../i18n/en.ftl"),
  de: include_str!("../i18n/de.ftl"),
};

/// The notetaker's texts for the policy's `language`.
pub fn for_language(language: &str) -> Texts {
  Texts::for_language(language, &SOURCES)
}

#[cfg(test)]
mod tests {
  use teamcraft_bot_runtime::texts::{message_ids, parses};

  use super::*;

  #[test]
  fn both_languages_have_the_same_messages_and_parse() {
    assert!(parses(SOURCES.en) && parses(SOURCES.de));
    assert_eq!(message_ids(SOURCES.en), message_ids(SOURCES.de));
    assert!(message_ids(SOURCES.en).len() > 15);
  }

  #[test]
  fn german_and_the_english_fallback() {
    let german = for_language("de");
    assert_eq!(
      german.format("participant-fallback", &[("number", TextArg::Number(2))]),
      "Teilnehmer 2"
    );
    let french = for_language("fr");
    assert_eq!(french.get("reason-shutdown"), "it was restarted");
    assert_eq!(
      french.format("result-gaps", &[("count", TextArg::Number(1))]),
      "One piece of speech could not be transcribed."
    );
    assert_eq!(french.get("no-such-text"), "no-such-text");
  }
}
