//! The rules for a channel's name (e2ee.md §4.6.5, "The name hash"): every client checks them at
//! create and rename, and again on receiving, so two names that look alike are rarely two hashes.
//!
//! A name is in Unicode Normalization Form C and holds no bidi controls, no other control
//! characters and no invisible (default-ignorable) code points. The sending client brings it to NFC
//! and refuses the rest at the field; a receiving client counts a listed name for a deletion only
//! if it keeps them. The server stores the name byte for byte and never normalizes.

use unicode_normalization::{UnicodeNormalization, is_nfc};

/// Why a channel name is refused at the field. Each variant has its own message in the apps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ChannelNameError {
  #[error("the name is empty")]
  Empty,
  /// Right-to-left and left-to-right marks, embeddings, overrides and isolates: they reorder what
  /// is shown, so one name can look like another.
  #[error("the name holds a character that changes the writing direction")]
  BidiControl,
  /// C0 and C1 controls, line and paragraph separators.
  #[error("the name holds a control character")]
  ControlCharacter,
  /// Zero-width and other default-ignorable code points: they show as nothing.
  #[error("the name holds an invisible character")]
  InvisibleCharacter,
}

/// The name as a client sends it: without leading and trailing whitespace, brought to NFC, checked
/// against the rules. The server stores it byte for byte.
pub fn checked_channel_name(name: &str) -> Result<String, ChannelNameError> {
  let normalized: String = name.trim().nfc().collect();
  check_characters(&normalized)?;
  Ok(normalized)
}

/// Whether a name another client sent (the server's listing) keeps the rules exactly as it is:
/// trimmed, in NFC, without refused characters. One that doesn't is stale for deletions.
pub fn keeps_channel_name_rules(name: &str) -> bool {
  name == name.trim() && is_nfc(name) && check_characters(name).is_ok()
}

fn check_characters(name: &str) -> Result<(), ChannelNameError> {
  if name.trim().is_empty() {
    return Err(ChannelNameError::Empty);
  }
  // Bidi first: some bidi controls are also default-ignorable, and their message is the clearer one.
  if name.chars().any(is_bidi_control) {
    return Err(ChannelNameError::BidiControl);
  }
  if name.chars().any(is_control) {
    return Err(ChannelNameError::ControlCharacter);
  }
  let chars: Vec<char> = name.chars().collect();
  let is_invisible = chars
    .iter()
    .enumerate()
    .any(|(index, &c)| is_default_ignorable(c) && !is_emoji_joiner(&chars, index));
  if is_invisible {
    return Err(ChannelNameError::InvisibleCharacter);
  }
  Ok(())
}

/// The two default-ignorable code points that are part of a visible emoji: the emoji presentation
/// selector (U+FE0F) right after a symbol, and the zero-width joiner (U+200D) between two symbols
/// (or a symbol and its selector), as an emoji keyboard types "❤️" or a family. Next to letters,
/// digits or spaces they would be invisible, and stay refused.
fn is_emoji_joiner(chars: &[char], index: usize) -> bool {
  let maybe_before = index.checked_sub(1).and_then(|before| chars.get(before)).copied();
  let maybe_after = chars.get(index + 1).copied();
  match chars.get(index) {
    Some('\u{FE0F}') => maybe_before.is_some_and(is_extended_pictographic),
    Some('\u{200D}') => {
      maybe_before.is_some_and(|c| is_extended_pictographic(c) || c == '\u{FE0F}' || is_skin_tone(c))
        && maybe_after.is_some_and(is_extended_pictographic)
    },
    _ => false,
  }
}

/// The emoji skin tone modifiers (`Emoji_Modifier`), which may come before a joiner.
fn is_skin_tone(c: char) -> bool {
  matches!(c, '\u{1F3FB}'..='\u{1F3FF}')
}

/// Unicode's `Extended_Pictographic` property (emoji-data.txt, Unicode 16), the characters emoji
/// sequences are built from. Dashes, bullets, punctuation, private-use and unassigned code points
/// outside these ranges are not, so the selector and the joiner next to them stay refused. The
/// table includes the ranges Unicode reserves for future pictographs, as the property does.
fn is_extended_pictographic(c: char) -> bool {
  matches!(
    c,
    '\u{00A9}'
      | '\u{00AE}'
      | '\u{203C}'
      | '\u{2049}'
      | '\u{2122}'
      | '\u{2139}'
      | '\u{2194}'..='\u{2199}'
      | '\u{21A9}'..='\u{21AA}'
      | '\u{231A}'..='\u{231B}'
      | '\u{2328}'
      | '\u{2388}'
      | '\u{23CF}'
      | '\u{23E9}'..='\u{23F3}'
      | '\u{23F8}'..='\u{23FA}'
      | '\u{24C2}'
      | '\u{25AA}'..='\u{25AB}'
      | '\u{25B6}'
      | '\u{25C0}'
      | '\u{25FB}'..='\u{25FE}'
      | '\u{2600}'..='\u{2605}'
      | '\u{2607}'..='\u{2612}'
      | '\u{2614}'..='\u{2685}'
      | '\u{2690}'..='\u{2705}'
      | '\u{2708}'..='\u{2712}'
      | '\u{2714}'
      | '\u{2716}'
      | '\u{271D}'
      | '\u{2721}'
      | '\u{2728}'
      | '\u{2733}'..='\u{2734}'
      | '\u{2744}'
      | '\u{2747}'
      | '\u{274C}'
      | '\u{274E}'
      | '\u{2753}'..='\u{2755}'
      | '\u{2757}'
      | '\u{2763}'..='\u{2767}'
      | '\u{2795}'..='\u{2797}'
      | '\u{27A1}'
      | '\u{27B0}'
      | '\u{27BF}'
      | '\u{2934}'..='\u{2935}'
      | '\u{2B05}'..='\u{2B07}'
      | '\u{2B1B}'..='\u{2B1C}'
      | '\u{2B50}'
      | '\u{2B55}'
      | '\u{3030}'
      | '\u{303D}'
      | '\u{3297}'
      | '\u{3299}'
      | '\u{1F000}'..='\u{1F0FF}'
      | '\u{1F10D}'..='\u{1F10F}'
      | '\u{1F12F}'
      | '\u{1F16C}'..='\u{1F171}'
      | '\u{1F17E}'..='\u{1F17F}'
      | '\u{1F18E}'
      | '\u{1F191}'..='\u{1F19A}'
      | '\u{1F1AD}'..='\u{1F1E5}'
      | '\u{1F201}'..='\u{1F20F}'
      | '\u{1F21A}'
      | '\u{1F22F}'
      | '\u{1F232}'..='\u{1F23A}'
      | '\u{1F23C}'..='\u{1F23F}'
      | '\u{1F249}'..='\u{1F3FA}'
      | '\u{1F400}'..='\u{1F53D}'
      | '\u{1F546}'..='\u{1F64F}'
      | '\u{1F680}'..='\u{1F6FF}'
      | '\u{1F774}'..='\u{1F77F}'
      | '\u{1F7D5}'..='\u{1F7FF}'
      | '\u{1F80C}'..='\u{1F80F}'
      | '\u{1F848}'..='\u{1F84F}'
      | '\u{1F85A}'..='\u{1F85F}'
      | '\u{1F888}'..='\u{1F88F}'
      | '\u{1F8AE}'..='\u{1F8FF}'
      | '\u{1F90C}'..='\u{1F93A}'
      | '\u{1F93C}'..='\u{1F945}'
      | '\u{1F947}'..='\u{1FAFF}'
      | '\u{1FC00}'..='\u{1FFFD}'
  )
}

/// Unicode's `Bidi_Control` property (PropList.txt).
fn is_bidi_control(c: char) -> bool {
  matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// General category `Cc`, plus the line and paragraph separators (`Zl`, `Zp`).
fn is_control(c: char) -> bool {
  c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')
}

/// Unicode's `Default_Ignorable_Code_Point` property (DerivedCoreProperties.txt, Unicode 16).
fn is_default_ignorable(c: char) -> bool {
  matches!(
    c,
    '\u{00AD}'
      | '\u{034F}'
      | '\u{061C}'
      | '\u{115F}'..='\u{1160}'
      | '\u{17B4}'..='\u{17B5}'
      | '\u{180B}'..='\u{180F}'
      | '\u{200B}'..='\u{200F}'
      | '\u{202A}'..='\u{202E}'
      | '\u{2060}'..='\u{206F}'
      | '\u{3164}'
      | '\u{FE00}'..='\u{FE0F}'
      | '\u{FEFF}'
      | '\u{FFA0}'
      | '\u{FFF0}'..='\u{FFF8}'
      | '\u{1BCA0}'..='\u{1BCA3}'
      | '\u{1D173}'..='\u{1D17A}'
      | '\u{E0000}'..='\u{E0FFF}'
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_plain_name_passes_unchanged() {
    assert_eq!(checked_channel_name("general").unwrap(), "general");
    assert_eq!(
      checked_channel_name("Projekt Ä – 2026").unwrap(),
      "Projekt Ä – 2026"
    );
    assert!(keeps_channel_name_rules("Projekt Ä – 2026"));
  }

  #[test]
  fn a_decomposed_name_is_brought_to_nfc_and_counts_only_composed() {
    let decomposed = "Cafe\u{0301}";
    assert_eq!(checked_channel_name(decomposed).unwrap(), "Caf\u{00E9}");
    assert!(!keeps_channel_name_rules(decomposed));
    assert!(keeps_channel_name_rules("Caf\u{00E9}"));
  }

  #[test]
  fn bidi_controls_are_refused() {
    for name in ["abc\u{202E}gpj.exe", "\u{200F}general", "a\u{2066}b\u{2069}"] {
      assert_eq!(
        checked_channel_name(name),
        Err(ChannelNameError::BidiControl),
        "{name:?}"
      );
      assert!(!keeps_channel_name_rules(name));
    }
  }

  #[test]
  fn control_characters_are_refused() {
    for name in [
      "gen\neral",
      "gen\u{0007}eral",
      "gen\u{0085}eral",
      "gen\u{2028}eral",
    ] {
      assert_eq!(
        checked_channel_name(name),
        Err(ChannelNameError::ControlCharacter),
        "{name:?}"
      );
    }
  }

  #[test]
  fn invisible_characters_are_refused() {
    for name in [
      "gen\u{200B}eral",
      "gen\u{00AD}eral",
      "\u{FEFF}general",
      "gen\u{E0041}eral",
      "a\u{FE0F}",
    ] {
      assert_eq!(
        checked_channel_name(name),
        Err(ChannelNameError::InvisibleCharacter),
        "{name:?}"
      );
    }
  }

  #[test]
  fn surrounding_whitespace_is_trimmed_and_counts_only_trimmed() {
    assert_eq!(checked_channel_name("  general \t").unwrap(), "general");
    assert!(!keeps_channel_name_rules(" general"));
  }

  /// Only real emoji take the selector or the joiner: next to dashes, bullets or private-use
  /// characters they are invisible.
  #[test]
  fn the_selector_and_joiner_next_to_other_symbols_are_refused() {
    for name in [
      "Projekt \u{00C4} \u{2013}\u{FE0F} 2026",
      "a \u{2013}\u{200D}\u{2014} b",
      "\u{2022}\u{200D}\u{2022}",
      "\u{E000}\u{FE0F}",
      "\u{2060}\u{FE0F}",
    ] {
      assert_eq!(
        checked_channel_name(name),
        Err(ChannelNameError::InvisibleCharacter),
        "{name:?}"
      );
    }
  }

  #[test]
  fn an_empty_or_blank_name_is_refused() {
    assert_eq!(checked_channel_name(""), Err(ChannelNameError::Empty));
    assert_eq!(checked_channel_name("   "), Err(ChannelNameError::Empty));
  }

  #[test]
  fn emoji_are_names_like_any_other() {
    assert!(checked_channel_name("team \u{1F680}").is_ok());
    assert!(checked_channel_name("team \u{1F44D}\u{1F3FD}").is_ok());
    // A heart with its presentation selector, and a family joined by zero-width joiners.
    assert!(checked_channel_name("team \u{2764}\u{FE0F}").is_ok());
    assert!(checked_channel_name("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}").is_ok());
    // A rainbow flag, and a woman technologist with a skin tone.
    assert!(checked_channel_name("\u{1F3F3}\u{FE0F}\u{200D}\u{1F308}").is_ok());
    assert!(checked_channel_name("\u{1F469}\u{1F3FD}\u{200D}\u{1F4BB}").is_ok());
    // The same two next to letters are invisible.
    assert_eq!(
      checked_channel_name("ab\u{200D}cd"),
      Err(ChannelNameError::InvisibleCharacter)
    );
    assert_eq!(
      checked_channel_name("ab\u{FE0F}"),
      Err(ChannelNameError::InvisibleCharacter)
    );
  }
}
