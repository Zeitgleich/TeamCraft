//! Text shortcuts that become emoji while composing: classic emoticons (`:)`) and `:shortcodes:`.
//! Only whole words are converted, and never inside code, so source code and links stay as typed.

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

/// `:name:` shortcodes the composer knows.
const SHORTCODES: [(&str, &str); 24] = [
  ("smile", "😄"),
  ("grin", "😁"),
  ("joy", "😂"),
  ("wink", "😉"),
  ("heart", "❤️"),
  ("thumbsup", "👍"),
  ("+1", "👍"),
  ("thumbsdown", "👎"),
  ("-1", "👎"),
  ("clap", "👏"),
  ("pray", "🙏"),
  ("tada", "🎉"),
  ("fire", "🔥"),
  ("rocket", "🚀"),
  ("eyes", "👀"),
  ("thinking", "🤔"),
  ("100", "💯"),
  ("check", "✅"),
  ("x", "❌"),
  ("warning", "⚠️"),
  ("coffee", "☕"),
  ("bug", "🐛"),
  ("sparkles", "✨"),
  ("wave", "👋"),
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
    && let Some((_, emoji)) = SHORTCODES.iter().find(|(code, _)| *code == name)
  {
    return (*emoji).to_owned();
  }
  word.to_owned()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn whole_word_emoticons_and_shortcodes_become_emoji() {
    assert_eq!(convert("hi :) see you ;) :tada:"), "hi 🙂 see you 😉 🎉");
    assert_eq!(convert("I <3 it"), "I ❤️ it");
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
}
