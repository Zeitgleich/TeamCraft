//! Text shortcuts that become emoji while composing: classic emoticons (`:)`) and `:shortcodes:`.
//! Only whole words are converted, and never inside code, so source code and links stay as typed.

use crate::emoji_catalog;

/// Emoticons and the emoji they become.
const EMOTICONS: [(&str, &str); 14] = [
  (":)", "🙂"),
  (":-)", "🙂"),
  (":D", "😀"),
  (":-D", "😀"),
  (";)", "😉"),
  (";-)", "😉"),
  (":(", "🙁"),
  (":-(", "🙁"),
  (":P", "😛"),
  (":'(", "😢"),
  (":o", "😮"),
  (":|", "😐"),
  ("<3", "❤️"),
  ("xD", "😆"),
];

/// Replaces emoticons and shortcodes in `text`, leaving fenced code, `inline code` and anything
/// attached to other characters (like `http://x.y:)`) alone.
pub fn convert(text: &str) -> String {
  let mut out = String::with_capacity(text.len());
  let mut is_fenced = false;
  for (index, line) in text.split('\n').enumerate() {
    if index > 0 {
      out.push('\n');
    }
    if line.trim_start().starts_with("```") {
      is_fenced = !is_fenced;
      out.push_str(line);
    } else if is_fenced {
      out.push_str(line);
    } else {
      convert_line(line, &mut out);
    }
  }
  out
}

fn convert_line(line: &str, out: &mut String) {
  let mut is_code = false;
  let mut word = String::new();
  let flush = |word: &mut String, is_code: bool, out: &mut String| {
    if word.is_empty() {
      return;
    }
    out.push_str(&if is_code { word.clone() } else { convert_word(word) });
    word.clear();
  };
  for character in line.chars() {
    if character.is_whitespace() {
      flush(&mut word, is_code, out);
      out.push(character);
    } else {
      if character == '`' {
        is_code = !is_code;
      }
      word.push(character);
    }
  }
  flush(&mut word, is_code, out);
}

fn convert_word(word: &str) -> String {
  if let Some((_, emoji)) = EMOTICONS.iter().find(|(emoticon, _)| *emoticon == word) {
    return (*emoji).to_owned();
  }
  if let Some(name) = word.strip_prefix(':').and_then(|rest| rest.strip_suffix(':'))
    && let Some(emoji) = emoji_catalog::by_shortcode(name)
  {
    return emoji.to_owned();
  }
  word.to_owned()
}

/// Most emoji a message may hold and still count as "emoji only" (jumbomoji, #1603). One place for
/// every client, so they agree; more emoji read as text at the normal size.
pub const JUMBO_MAX_EMOJI: usize = 3;

/// How many emoji `text` consists of, when it is nothing but 1 to [`JUMBO_MAX_EMOJI`] emoji with
/// whitespace between them; `None` for anything else (text, links, mentions, Markdown, more emoji,
/// no emoji at all). Counting is by emoji, not by code point: a ZWJ family, a flag, a skin tone,
/// a keycap or a tag flag is one. Characters that default to text (`©`, `™`, digits) count only
/// with the emoji variation selector (`©️`).
pub fn jumbo_emoji_count(text: &str) -> Option<usize> {
  let mut rest = text.trim_start();
  let mut count = 0;
  while !rest.is_empty() {
    let length = emoji_length(rest)?;
    count += 1;
    if count > JUMBO_MAX_EMOJI {
      return None;
    }
    rest = rest[length..].trim_start();
  }
  (count > 0).then_some(count)
}

/// How much larger than body text a message of `count` emoji is drawn: big for one, smaller steps
/// for two and three. `1.0` for any other count.
pub fn jumbo_emoji_scale(count: usize) -> f32 {
  match count {
    1 => 2.5,
    2 => 2.0,
    3 => 1.6,
    _ => 1.0,
  }
}

const VARIATION_EMOJI: char = '\u{FE0F}';
const ZERO_WIDTH_JOINER: char = '\u{200D}';
const KEYCAP: char = '\u{20E3}';

/// The byte length of the emoji at the start of `text`, if one starts there.
fn emoji_length(text: &str) -> Option<usize> {
  let mut chars = text.char_indices().peekable();
  let (_, first) = chars.next()?;
  let mut end = first.len_utf8();
  if is_regional_indicator(first) {
    // A flag is exactly two indicators.
    let (at, second) = chars.next()?;
    return is_regional_indicator(second).then_some(at + second.len_utf8());
  }
  if is_keycap_base(first) {
    let mut ahead = text[end..].chars();
    let mut next = ahead.next()?;
    if next == VARIATION_EMOJI {
      end += next.len_utf8();
      next = ahead.next()?;
    }
    return (next == KEYCAP).then_some(end + next.len_utf8());
  }
  let is_emoji_part = |character: char, followed_by_selector: bool| {
    is_emoji_presentation(character) || (followed_by_selector && is_pictographic(character))
  };
  let mut current = first;
  loop {
    // One ZWJ element: the base (already in `end`), then selector / skin tone / tag letters.
    let selector = text[end..].starts_with(VARIATION_EMOJI);
    if !is_emoji_part(current, selector) {
      return None;
    }
    if selector {
      end += VARIATION_EMOJI.len_utf8();
    }
    for character in text[end..].chars() {
      if is_skin_tone(character) || is_tag(character) {
        end += character.len_utf8();
      } else {
        break;
      }
    }
    // A joiner needs another element after it; a dangling one is not an emoji.
    let mut after = text[end..].chars();
    if after.next() != Some(ZERO_WIDTH_JOINER) {
      return Some(end);
    }
    let next = after.next()?;
    end += ZERO_WIDTH_JOINER.len_utf8() + next.len_utf8();
    current = next;
  }
}

fn is_regional_indicator(character: char) -> bool {
  ('\u{1F1E6}'..='\u{1F1FF}').contains(&character)
}

fn is_keycap_base(character: char) -> bool {
  character.is_ascii_digit() || character == '#' || character == '*'
}

fn is_skin_tone(character: char) -> bool {
  ('\u{1F3FB}'..='\u{1F3FF}').contains(&character)
}

/// Tag characters of subdivision flags (England, Scotland, Wales) and their cancel tag.
fn is_tag(character: char) -> bool {
  ('\u{E0020}'..='\u{E007F}').contains(&character)
}

/// Emoji that show as a picture with no selector (Unicode `Emoji_Presentation`, without the
/// regional indicators). A test checks the table against the full emoji set.
fn is_emoji_presentation(character: char) -> bool {
  // The BMP list is closed and exact. Everything astral is accepted as a block, so emoji added by
  // a newer Unicode version count before this table learns them; the few text-default symbols in
  // it (a thermometer without selector) are rare enough to be drawn large too.
  const BMP: &[(u32, u32)] = &[
    (0x231A, 0x231B),
    (0x23E9, 0x23EC),
    (0x23F0, 0x23F0),
    (0x23F3, 0x23F3),
    (0x25FD, 0x25FE),
    (0x2614, 0x2615),
    (0x2648, 0x2653),
    (0x267F, 0x267F),
    (0x2693, 0x2693),
    (0x26A1, 0x26A1),
    (0x26AA, 0x26AB),
    (0x26BD, 0x26BE),
    (0x26C4, 0x26C5),
    (0x26CE, 0x26CE),
    (0x26D4, 0x26D4),
    (0x26EA, 0x26EA),
    (0x26F2, 0x26F3),
    (0x26F5, 0x26F5),
    (0x26FA, 0x26FA),
    (0x26FD, 0x26FD),
    (0x2705, 0x2705),
    (0x270A, 0x270B),
    (0x2728, 0x2728),
    (0x274C, 0x274C),
    (0x274E, 0x274E),
    (0x2753, 0x2755),
    (0x2757, 0x2757),
    (0x2795, 0x2797),
    (0x27B0, 0x27B0),
    (0x27BF, 0x27BF),
    (0x2B1B, 0x2B1C),
    (0x2B50, 0x2B50),
    (0x2B55, 0x2B55),
  ];
  const ASTRAL: (u32, u32) = (0x1F004, 0x1FAFF);
  let code = u32::from(character);
  in_ranges(character, BMP) || (ASTRAL.0..=ASTRAL.1).contains(&code)
}

/// Characters that become emoji when the variation selector follows (`©️`, `❤️`, `☺️`): the
/// pictographic blocks, a superset of what the selector is defined for.
fn is_pictographic(character: char) -> bool {
  const RANGES: &[(u32, u32)] = &[
    (0x00A9, 0x00A9),
    (0x00AE, 0x00AE),
    (0x203C, 0x203C),
    (0x2049, 0x2049),
    (0x2122, 0x2122),
    (0x2139, 0x2139),
    (0x2194, 0x21AA),
    (0x231A, 0x23FF),
    (0x24C2, 0x24C2),
    (0x25AA, 0x25FE),
    (0x2600, 0x27BF),
    (0x2934, 0x2935),
    (0x2B05, 0x2B55),
    (0x3030, 0x3030),
    (0x303D, 0x303D),
    (0x3297, 0x3297),
    (0x3299, 0x3299),
    (0x1F000, 0x1FAFF),
  ];
  in_ranges(character, RANGES)
}

fn in_ranges(character: char, ranges: &[(u32, u32)]) -> bool {
  let code = u32::from(character);
  ranges.iter().any(|(low, high)| (*low..=*high).contains(&code))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn whole_word_emoticons_and_shortcodes_become_emoji() {
    assert_eq!(convert("hi :) see you ;) :tada:"), "hi 🙂 see you 😉 🎉");
    assert_eq!(convert("I <3 it"), "I ❤️ it");
    assert_eq!(convert(":melting_face: :check: :+1:"), "🫠 ✅ 👍");
  }

  #[test]
  fn code_links_and_attached_text_stay() {
    assert_eq!(convert("see http://a.b:) now"), "see http://a.b:) now");
    assert_eq!(convert("use `:)` literally"), "use `:)` literally");
    assert_eq!(convert("```\n:)\n```\n:)"), "```\n:)\n```\n🙂");
    assert_eq!(convert("x:)"), "x:)");
    assert_eq!(convert(":unknown:"), ":unknown:");
  }

  #[test]
  fn line_breaks_and_spacing_are_kept() {
    assert_eq!(convert("a  :)\n\nb"), "a  🙂\n\nb");
  }

  mod jumbo {
    use super::*;

    #[test]
    fn counts_one_emoji_in_every_form() {
      for single in [
        "😀",
        "👍🏽",
        "👨\u{200D}👩\u{200D}👧\u{200D}👦",
        "🇩🇪",
        "1\u{FE0F}\u{20E3}",
        "#\u{20E3}",
        "©\u{FE0F}",
        "❤\u{FE0F}",
        "🏳\u{FE0F}\u{200D}🌈",
        "🏴\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}",
        "✅",
        "  😀\n",
      ] {
        assert_eq!(jumbo_emoji_count(single), Some(1), "{single:?}");
      }
    }

    #[test]
    fn counts_two_and_three_with_whitespace() {
      assert_eq!(jumbo_emoji_count("😀 😀"), Some(2));
      assert_eq!(jumbo_emoji_count("😀😀😀"), Some(3));
      assert_eq!(jumbo_emoji_count("🇩🇪🇫🇷 👍🏽"), Some(3));
    }

    #[test]
    fn rejects_everything_else() {
      for other in [
        "",
        "   ",
        "😀😀😀😀",
        "😀 hi",
        "hi 😀",
        "©",
        "™",
        "7",
        "1",
        "#",
        "©™",
        "😀.",
        "**😀**",
        "@alex 😀",
        "https://example.com",
        "\u{FE0F}",
        "😀\u{200D}",
        "\u{200D}😀",
        "🇩",
        "🇩🇪🇫",
        "😀\u{20E3}x",
      ] {
        assert_eq!(jumbo_emoji_count(other), None, "{other:?}");
      }
    }

    #[test]
    fn the_scale_shrinks_with_the_count() {
      assert!(jumbo_emoji_scale(1) > jumbo_emoji_scale(2));
      assert!(jumbo_emoji_scale(2) > jumbo_emoji_scale(3));
      assert_eq!(jumbo_emoji_scale(JUMBO_MAX_EMOJI + 1), 1.0);
    }

    /// Guards the hand-written tables against the full emoji set.
    #[test]
    fn every_known_emoji_is_one_emoji() {
      let misses: Vec<String> = emojis::iter()
        .filter(|emoji| jumbo_emoji_count(emoji.as_str()) != Some(1))
        .map(|emoji| {
          format!(
            "{} {:X?}",
            emoji.name(),
            emoji.as_str().chars().map(u32::from).collect::<Vec<_>>()
          )
        })
        .collect();
      assert!(misses.is_empty(), "{misses:#?}");
    }
  }
}
