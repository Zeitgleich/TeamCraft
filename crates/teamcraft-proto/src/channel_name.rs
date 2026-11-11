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
    Some('\u{FE0F}') => maybe_before.is_some_and(is_symbol),
    Some('\u{200D}') => {
      maybe_before.is_some_and(|c| is_symbol(c) || c == '\u{FE0F}') && maybe_after.is_some_and(is_symbol)
    },
    _ => false,
  }
}

/// A visible character outside letters, digits, spaces and ASCII: what emoji are made of, roughly.
/// Exact emoji properties would need Unicode's emoji data; this errs on refusing.
fn is_symbol(c: char) -> bool {
  u32::from(c) >= 0x2000 && !c.is_alphanumeric() && !c.is_whitespace() && !is_default_ignorable(c)
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
