//! The texts the recorder bot posts, in the configured language (German or English; English for
//! everything else). Translations live in `i18n/*.ftl` like the app's.

pub use teamcraft_bot_runtime::texts::{TextArg, TextSources, Texts};

/// The recorder's translations.
pub const SOURCES: TextSources = TextSources {
  en: include_str!("../i18n/en.ftl"),
  de: include_str!("../i18n/de.ftl"),
};

/// The recorder's texts for `language`.
pub fn for_language(language: &str) -> Texts {
  Texts::for_language(language, &SOURCES)
}

/// The message posted with a take: its length in whole minutes (at least one), its part when the
/// call ran past the longest take, and a note when an earlier part was lost to a restart.
pub fn take_message(
  texts: &Texts,
  duration_ms: u64,
  maybe_part: Option<u32>,
  has_lost_earlier: bool,
) -> String {
  let minutes = duration_ms.div_ceil(60_000).max(1);
  let mut message = match maybe_part {
    Some(part) => texts.format(
      "posted-part",
      &[
        ("part", TextArg::Number(u64::from(part))),
        ("minutes", TextArg::Number(minutes)),
      ],
    ),
    None => texts.format("posted", &[("minutes", TextArg::Number(minutes))]),
  };
  if has_lost_earlier {
    message.push(' ');
    message.push_str(&texts.get("earlier-lost"));
  }
  message
}

#[cfg(test)]
mod tests {
  use teamcraft_bot_runtime::texts::{message_ids, parses};

  use super::*;

  #[test]
  fn both_languages_have_the_same_messages_and_parse() {
    assert!(parses(SOURCES.en) && parses(SOURCES.de));
    assert_eq!(message_ids(SOURCES.en), message_ids(SOURCES.de));
    assert!(message_ids(SOURCES.en).len() >= 9);
  }

  #[test]
  fn a_take_says_its_length_part_and_what_was_lost() {
    let english = for_language("en");
    assert_eq!(
      take_message(&english, 90_000, None, false),
      "Recording of the call (2 min)."
    );
    assert_eq!(
      take_message(&english, 1_000, None, false),
      "Recording of the call (1 min)."
    );
    assert!(
      take_message(&english, 60_000, Some(2), false).starts_with("Recording of the call, part 2 (1 min).")
    );
    assert!(
      take_message(&english, 60_000, None, true)
        .ends_with("the part of the call before that was not recorded.")
    );
    let german = for_language("de");
    assert_eq!(
      take_message(&german, 120_000, None, false),
      "Aufnahme des Anrufs (2 Min.)."
    );
    assert_eq!(for_language("fr").get("removed"), english.get("removed"));
  }
}
