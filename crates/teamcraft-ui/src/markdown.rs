//! A small, safe Markdown subset for message bodies. It produces a tree the UI turns into elements, so
//! no HTML string is ever injected. Supported: paragraphs, `#` headings, `>` quotes, `-` and `1.`
//! lists, fenced code, `code`, **bold**, _italic_, __underline__, ~~strike~~, http(s) links (bare or
//! `[label](url)`) and @mentions. Everything else is text.
//!
//! [`to_source`] writes a tree back into this format; the composer's editor stores what the user
//! formatted through it, so `parse(to_source(blocks))` gives the same blocks back.

/// How message permalinks start (the client core makes them).
pub const PERMALINK_PREFIX: &str = "teamcraft:/c/";

/// Schemes a link may use; anything else stays text.
const LINK_PREFIXES: [&str; 3] = ["https://", "http://", PERMALINK_PREFIX];

/// The deepest heading the subset has (`###`).
pub const MAX_HEADING_LEVEL: u8 = 3;

/// The longest list number accepted (`123456789.`), so a long number in text isn't a list.
const MAX_LIST_NUMBER_DIGITS: usize = 9;

/// Messages longer than this many lines (or characters) start collapsed with a "Show more" button.
const LONG_MESSAGE_LINES: usize = 14;
const LONG_MESSAGE_CHARS: usize = 1_400;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
  /// Lines of inline content; a newline in the source becomes a line break.
  Paragraph(Vec<Vec<Inline>>),
  /// `#` to `###`.
  Heading { level: u8, content: Vec<Inline> },
  /// Consecutive `>` lines.
  Quote(Vec<Vec<Inline>>),
  /// Consecutive `-` (or `*`) items, or `1.` items numbered from `start`.
  List { kind: ListKind, items: Vec<Vec<Inline>> },
  Code {
    maybe_language: Option<String>,
    code: String,
  },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListKind {
  Bulleted,
  Numbered { start: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inline {
  Text(String),
  Bold(Vec<Inline>),
  Italic(Vec<Inline>),
  Underline(Vec<Inline>),
  Strike(Vec<Inline>),
  Code(String),
  /// A bare link: the URL is its own label.
  Link(String),
  /// `[label](url)`; the UI shows the URL as the tooltip, so the label can't hide where it goes.
  LabeledLink {
    url: String,
    label: String,
  },
  Mention(String),
}

/// What one source line is, before consecutive lines are grouped into blocks.
enum Line<'a> {
  Text(&'a str),
  Heading(u8, &'a str),
  Quote(&'a str),
  Item(ListKind, &'a str),
}

pub fn parse(source: &str) -> Vec<Block> {
  let mut blocks: Vec<Block> = Vec::new();
  let mut maybe_fence: Option<(Option<String>, Vec<&str>)> = None;

  for line in source.lines() {
    if let Some((language, mut code_lines)) = maybe_fence.take() {
      if line.trim_end() == "```" {
        blocks.push(Block::Code {
          maybe_language: language,
          code: code_lines.join("\n"),
        });
      } else {
        code_lines.push(line);
        maybe_fence = Some((language, code_lines));
      }
    } else if let Some(info) = line.trim_start().strip_prefix("```") {
      let info = info.trim();
      maybe_fence = Some(((!info.is_empty()).then(|| info.to_owned()), Vec::new()));
    } else {
      push_line(&mut blocks, classify(line));
    }
  }
  // An unterminated fence still renders as code instead of swallowing the rest silently.
  if let Some((language, code_lines)) = maybe_fence {
    blocks.push(Block::Code {
      maybe_language: language,
      code: code_lines.join("\n"),
    });
  }
  blocks
}

/// Adds a line to the block it continues, or starts a new block.
fn push_line(blocks: &mut Vec<Block>, line: Line<'_>) {
  match (blocks.last_mut(), line) {
    (Some(Block::Paragraph(lines)), Line::Text(text)) => lines.push(parse_inline(text)),
    (Some(Block::Quote(lines)), Line::Quote(text)) => lines.push(parse_inline(text)),
    (Some(Block::List { kind, items }), Line::Item(item_kind, text)) if same_list(*kind, item_kind) => {
      items.push(parse_inline(text));
    },
    (_, Line::Text(text)) => blocks.push(Block::Paragraph(vec![parse_inline(text)])),
    (_, Line::Quote(text)) => blocks.push(Block::Quote(vec![parse_inline(text)])),
    (_, Line::Item(kind, text)) => blocks.push(Block::List {
      kind,
      items: vec![parse_inline(text)],
    }),
    (_, Line::Heading(level, text)) => blocks.push(Block::Heading {
      level,
      content: parse_inline(text),
    }),
  }
}

fn same_list(list: ListKind, item: ListKind) -> bool {
  matches!(
    (list, item),
    (ListKind::Bulleted, ListKind::Bulleted) | (ListKind::Numbered { .. }, ListKind::Numbered { .. })
  )
}

fn classify(line: &str) -> Line<'_> {
  let hashes = line.bytes().take_while(|byte| *byte == b'#').count();
  if (1..=usize::from(MAX_HEADING_LEVEL)).contains(&hashes)
    && let Some(text) = line[hashes..].strip_prefix(' ')
  {
    // Invariant: `hashes` is at most MAX_HEADING_LEVEL, which fits in u8.
    return Line::Heading(hashes as u8, text);
  }
  if line == ">" {
    return Line::Quote("");
  }
  if let Some(text) = line.strip_prefix("> ") {
    return Line::Quote(text);
  }
  if let Some(text) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
    return Line::Item(ListKind::Bulleted, text);
  }
  let digits = line.bytes().take_while(u8::is_ascii_digit).count();
  if (1..=MAX_LIST_NUMBER_DIGITS).contains(&digits)
    && let Some(text) = line[digits..].strip_prefix(". ")
    && let Ok(start) = line[..digits].parse::<u32>()
  {
    return Line::Item(ListKind::Numbered { start }, text);
  }
  Line::Text(line)
}

pub fn parse_inline(text: &str) -> Vec<Inline> {
  let mut out = Vec::new();
  let mut plain = String::new();
  let mut rest = text;

  while let Some(ch) = rest.chars().next() {
    let maybe_prev = plain.chars().last();
    let before_is_boundary = maybe_prev.is_none_or(|prev| !prev.is_alphanumeric());
    let before_is_word_start = before_is_boundary && maybe_prev != Some('_');
    let parsed = match ch {
      '`' => delimited(rest, "`").map(|(inner, used)| (Inline::Code(inner.to_owned()), used)),
      '*' if rest.starts_with("**") => {
        delimited(rest, "**").map(|(inner, used)| (Inline::Bold(parse_inline(inner)), used))
      },
      '~' if rest.starts_with("~~") => {
        delimited(rest, "~~").map(|(inner, used)| (Inline::Strike(parse_inline(inner)), used))
      },
      // `snake_case` is not emphasis, so underscores need a word boundary in front.
      '_' if before_is_word_start && rest.starts_with("__") => {
        delimited(rest, "__").map(|(inner, used)| (Inline::Underline(parse_inline(inner)), used))
      },
      '_' if before_is_word_start => {
        delimited(rest, "_").map(|(inner, used)| (Inline::Italic(parse_inline(inner)), used))
      },
      // A single `*` marks italics inside a word as well (`un*believ*able`), as in CommonMark.
      '*' => delimited(rest, "*").map(|(inner, used)| (Inline::Italic(parse_inline(inner)), used)),
      '[' => labeled_link(rest),
      // A backslash before punctuation makes it literal (`\\_` is an underscore).
      '\\' => rest[1..]
        .chars()
        .next()
        .filter(char::is_ascii_punctuation)
        .map(|escaped| (Inline::Text(escaped.to_string()), 1 + escaped.len_utf8())),
      '@' if before_is_boundary => mention(rest),
      'h' | 't' if before_is_boundary => link(rest),
      _ => None,
    };
    match parsed {
      Some((Inline::Text(escaped), used)) => {
        plain.push_str(&escaped);
        rest = &rest[used..];
      },
      Some((inline, used)) => {
        if !plain.is_empty() {
          out.push(Inline::Text(std::mem::take(&mut plain)));
        }
        out.push(inline);
        rest = &rest[used..];
      },
      None => {
        plain.push(ch);
        rest = &rest[ch.len_utf8()..];
      },
    }
  }
  if !plain.is_empty() {
    out.push(Inline::Text(plain));
  }
  out
}

/// Returns the content between `marker` at the start of `rest` and the next unescaped `marker`, plus
/// the bytes used.
fn delimited<'a>(rest: &'a str, marker: &str) -> Option<(&'a str, usize)> {
  let body = &rest[marker.len()..];
  // Code is verbatim; elsewhere `\*` inside the span doesn't close it.
  let end = if marker == "`" {
    body.find(marker)?
  } else {
    find_unescaped(body, marker)?
  };
  let inner = &body[..end];
  // `** **` or `__` are not emphasis; the content must start and end on a non-space.
  if inner.is_empty() || inner.starts_with(char::is_whitespace) || inner.ends_with(char::is_whitespace) {
    return None;
  }
  Some((inner, marker.len() * 2 + end))
}

/// Byte offset of the first `needle` in `haystack` that isn't escaped by a backslash.
fn find_unescaped(haystack: &str, needle: &str) -> Option<usize> {
  let mut is_escaped = false;
  for (at, ch) in haystack.char_indices() {
    if is_escaped {
      is_escaped = false;
    } else if ch == '\\' {
      is_escaped = true;
    } else if haystack[at..].starts_with(needle) {
      return Some(at);
    }
  }
  None
}

fn mention(rest: &str) -> Option<(Inline, usize)> {
  let name: String = rest[1..]
    .chars()
    .take_while(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '.' | '-'))
    .collect();
  let name = name.trim_end_matches(['.', '-']).to_owned();
  if name.is_empty() {
    return None;
  }
  let used = 1 + name.len();
  Some((Inline::Mention(name), used))
}

fn link(rest: &str) -> Option<(Inline, usize)> {
  // `teamcraft:/c/...` is a permalink to a message (shown as a chip, never opened by the browser).
  let url = link_at(rest)?;
  Some((Inline::Link(url.to_owned()), url.len()))
}

/// The bare link `rest` starts with, without trailing punctuation.
fn link_at(rest: &str) -> Option<&str> {
  let prefix = LINK_PREFIXES
    .into_iter()
    .find(|prefix| rest.starts_with(prefix))?;
  let mut url = rest.split(char::is_whitespace).next().unwrap_or(rest);
  // Trailing punctuation ends the sentence, not the link; a `)` only when it closes nothing in the
  // URL, so `(see https://example.com/a)` loses it and `…/Rust_(language)` keeps it.
  loop {
    url = url.trim_end_matches(['.', ',', ';', ':', '!', '?']);
    match url.strip_suffix(')') {
      Some(shorter) if url.matches(')').count() > url.matches('(').count() => url = shorter,
      _ => break,
    }
  }
  (url.len() > prefix.len()).then_some(url)
}

/// `[label](https://…)`: the label is plain text, the URL must be a link the subset allows.
fn labeled_link(rest: &str) -> Option<(Inline, usize)> {
  let label_end = 1 + find_unescaped(&rest[1..], "]")?;
  let raw_label = &rest[1..label_end];
  let target = rest[label_end + 1..].strip_prefix('(')?;
  let url_end = target.find(')')?;
  let url = &target[..url_end];
  let is_allowed = LINK_PREFIXES
    .into_iter()
    .any(|prefix| url.len() > prefix.len() && url.starts_with(prefix));
  if !is_allowed || url.contains(char::is_whitespace) || raw_label.trim().is_empty() {
    return None;
  }
  let label = unescape(raw_label);
  // `(` + url + `)` after the `]`.
  let used = label_end + 1 + 1 + url_end + 1;
  Some((
    Inline::LabeledLink {
      url: decode_parens(url),
      label,
    },
    used,
  ))
}

/// Parentheses in a `[label](url)` target are written as `%28` and `%29` (the first `)` ends the
/// target), and read back as they were; percent-encoded and plain parentheses address the same thing.
fn encode_parens(url: &str) -> String {
  url.replace('(', "%28").replace(')', "%29")
}

fn decode_parens(url: &str) -> String {
  url.replace("%28", "(").replace("%29", ")")
}

fn unescape(text: &str) -> String {
  let mut out = String::with_capacity(text.len());
  let mut chars = text.chars().peekable();
  while let Some(ch) = chars.next() {
    match chars.peek() {
      Some(next) if ch == '\\' && next.is_ascii_punctuation() => {
        out.push(*next);
        chars.next();
      },
      _ => out.push(ch),
    }
  }
  out
}

/// Whether a message body is long enough to start collapsed in the feed.
pub fn is_long(body: &str) -> bool {
  body.lines().count() > LONG_MESSAGE_LINES || body.chars().count() > LONG_MESSAGE_CHARS
}

/// Whether `url` may be the target of a link (what the parser accepts).
pub fn is_allowed_link(url: &str) -> bool {
  link_at(url) == Some(url)
}

/// Writes blocks in the subset so that [`parse`] reads the same blocks back. Text that would read as
/// markup is escaped; formatting the subset can't express at that spot is dropped rather than shown
/// as stray markers.
pub fn to_source(blocks: &[Block]) -> String {
  let mut lines: Vec<String> = Vec::new();
  for block in blocks {
    match block {
      Block::Paragraph(paragraph) => {
        lines.extend(paragraph.iter().map(|line| escape_line_start(&line_source(line))));
      },
      Block::Heading { level, content } => {
        let level = (*level).clamp(1, MAX_HEADING_LEVEL);
        lines.push(format!(
          "{} {}",
          "#".repeat(usize::from(level)),
          line_source(content)
        ));
      },
      Block::Quote(quote) => lines.extend(quote.iter().map(|line| match line_source(line) {
        text if text.is_empty() => ">".to_owned(),
        text => format!("> {text}"),
      })),
      Block::List { kind, items } => {
        for (index, item) in items.iter().enumerate() {
          let marker = match kind {
            ListKind::Bulleted => "-".to_owned(),
            ListKind::Numbered { start } => format!("{}.", u64::from(*start) + index as u64),
          };
          lines.push(format!("{marker} {}", line_source(item)));
        }
      },
      Block::Code { maybe_language, code } => {
        lines.push(format!("```{}", maybe_language.as_deref().unwrap_or_default()));
        lines.extend(code.lines().map(str::to_owned));
        lines.push("```".to_owned());
      },
    }
  }
  lines.join("\n")
}

/// One line of inline content in the subset. Tries the lightest escaping first and checks each
/// candidate by parsing it again, so the result always reads back as `inlines` (or, if formatting
/// can't be expressed, at least as the same text).
fn line_source(inlines: &[Inline]) -> String {
  let expected = normalize(inlines);
  for escaping in [Escaping::Light, Escaping::Strict] {
    let candidate = write_inlines(inlines, escaping);
    if normalize(&parse_inline(&candidate)) == expected {
      return candidate;
    }
  }
  write_inlines(&[Inline::Text(plain_text(inlines))], Escaping::Strict)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Escaping {
  /// Only what the parser can't miss: backslashes before punctuation.
  Light,
  /// Every character that can open markup, except inside links and mentions.
  Strict,
}

fn write_inlines(inlines: &[Inline], escaping: Escaping) -> String {
  let mut out = String::new();
  for inline in inlines {
    write_inline(&mut out, inline, escaping);
  }
  out
}

fn write_inline(out: &mut String, inline: &Inline, escaping: Escaping) {
  match inline {
    Inline::Text(text) => out.push_str(&escape_text(text, escaping)),
    Inline::Bold(inner) => wrap(out, "**", inner, escaping),
    Inline::Strike(inner) => wrap(out, "~~", inner, escaping),
    Inline::Underline(inner) => wrap(out, "__", inner, escaping),
    // `_` doesn't clash with a `**` around it, but needs a word boundary in front and can't hold
    // another `_`; `*` covers those cases.
    Inline::Italic(inner) => {
      let after_word = out.chars().last().is_some_and(char::is_alphanumeric);
      let holds_underscore = write_inlines(inner, escaping).contains('_');
      let marker = if after_word || holds_underscore { "*" } else { "_" };
      wrap(out, marker, inner, escaping);
    },
    Inline::Code(code) if code.contains('`') || code.trim() != code || code.is_empty() => {
      out.push_str(&escape_text(code, Escaping::Strict));
    },
    Inline::Code(code) => {
      out.push('`');
      out.push_str(code);
      out.push('`');
    },
    Inline::Link(url) => out.push_str(url),
    Inline::LabeledLink { url, label } => {
      out.push('[');
      for ch in label.chars() {
        if ch.is_ascii_punctuation() && matches!(ch, '[' | ']' | '\\') {
          out.push('\\');
        }
        out.push(ch);
      }
      out.push_str("](");
      out.push_str(&encode_parens(url));
      out.push(')');
    },
    Inline::Mention(name) => {
      out.push('@');
      out.push_str(name);
    },
  }
}

/// Emphasis around `inner`; spaces at its edges move outside, since `** x**` isn't emphasis.
fn wrap(out: &mut String, marker: &str, inner: &[Inline], escaping: Escaping) {
  let body = write_inlines(inner, escaping);
  let trimmed = body.trim();
  if trimmed.is_empty() {
    out.push_str(&body);
    return;
  }
  let leading = &body[..body.len() - body.trim_start().len()];
  let trailing = &body[body.trim_end().len()..];
  out.push_str(leading);
  out.push_str(marker);
  out.push_str(trimmed);
  out.push_str(marker);
  out.push_str(trailing);
}

fn escape_text(text: &str, escaping: Escaping) -> String {
  let mut out = String::with_capacity(text.len());
  let mut rest = text;
  while let Some(ch) = rest.chars().next() {
    let before_is_boundary = out.chars().last().is_none_or(|prev| !prev.is_alphanumeric());
    // Links and mentions stay verbatim: a backslash inside them would change what they point to.
    let verbatim = match ch {
      'h' | 't' if before_is_boundary => link_at(rest).map(str::len),
      '@' if before_is_boundary => mention(rest).map(|(_, used)| used),
      _ => None,
    };
    if let Some(used) = verbatim {
      out.push_str(&rest[..used]);
      rest = &rest[used..];
      continue;
    }
    let next_is_punctuation = rest[ch.len_utf8()..]
      .chars()
      .next()
      .is_some_and(|next| next.is_ascii_punctuation());
    let needs_escape = match (escaping, ch) {
      (_, '\\') => next_is_punctuation,
      (Escaping::Light, _) => false,
      (Escaping::Strict, '*' | '`' | '~' | '[') => true,
      (Escaping::Strict, '_') => before_is_boundary,
      (Escaping::Strict, _) => false,
    };
    if needs_escape {
      out.push('\\');
    }
    out.push(ch);
    rest = &rest[ch.len_utf8()..];
  }
  out
}

/// A paragraph line that starts like a heading, quote, list item or fence keeps that as text.
fn escape_line_start(line: &str) -> String {
  let starts_block = !matches!(classify(line), Line::Text(_)) || line.trim_start().starts_with("```");
  if !starts_block {
    return line.to_owned();
  }
  let digits = line.bytes().take_while(u8::is_ascii_digit).count();
  if digits > 0 {
    // `1. x` → `1\. x`
    format!("{}\\{}", &line[..digits], &line[digits..])
  } else {
    let indent = line.len() - line.trim_start().len();
    format!("{}\\{}", &line[..indent], &line[indent..])
  }
}

/// The text of inline content without its formatting (mentions and links as written).
pub fn plain_text(inlines: &[Inline]) -> String {
  let mut out = String::new();
  for inline in inlines {
    match inline {
      Inline::Text(text) | Inline::Code(text) | Inline::Link(text) => out.push_str(text),
      Inline::LabeledLink { label, .. } => out.push_str(label),
      Inline::Mention(name) => {
        out.push('@');
        out.push_str(name);
      },
      Inline::Bold(inner) | Inline::Italic(inner) | Inline::Underline(inner) | Inline::Strike(inner) => {
        out.push_str(&plain_text(inner));
      },
    }
  }
  out
}

/// The same content in a canonical shape: adjacent text merged, empty pieces dropped, and plain text
/// split into the links and mentions the parser would find in it.
fn normalize(inlines: &[Inline]) -> Vec<Inline> {
  let mut merged: Vec<Inline> = Vec::new();
  for inline in inlines {
    match (merged.last_mut(), inline) {
      (Some(Inline::Text(previous)), Inline::Text(text)) => previous.push_str(text),
      (_, inline) => merged.push(inline.clone()),
    }
  }
  let mut out: Vec<Inline> = Vec::new();
  for inline in &merged {
    let pieces = match inline {
      Inline::Text(text) => parse_inline(&escape_text(text, Escaping::Strict)),
      Inline::Bold(inner) => styled(Inline::Bold, inner),
      Inline::Italic(inner) => styled(Inline::Italic, inner),
      Inline::Underline(inner) => styled(Inline::Underline, inner),
      Inline::Strike(inner) => styled(Inline::Strike, inner),
      other => vec![other.clone()],
    };
    for piece in pieces {
      match (out.last_mut(), piece) {
        (Some(Inline::Text(previous)), Inline::Text(text)) => previous.push_str(&text),
        (_, piece) => out.push(piece),
      }
    }
  }
  out
}

/// Emphasis around normalized content; spaces at the edges belong outside it (see [`wrap`]).
fn styled(make: fn(Vec<Inline>) -> Inline, inner: &[Inline]) -> Vec<Inline> {
  let mut content = normalize(inner);
  let mut leading = String::new();
  let mut trailing = String::new();
  if let Some(Inline::Text(first)) = content.first_mut() {
    let kept = first.trim_start().len();
    leading = first[..first.len() - kept].to_owned();
    *first = first.trim_start().to_owned();
  }
  if let Some(Inline::Text(last)) = content.last_mut() {
    let kept = last.trim_end().len();
    trailing = last[kept..].to_owned();
    last.truncate(kept);
  }
  content.retain(|inline| !matches!(inline, Inline::Text(text) if text.is_empty()));
  if content.is_empty() {
    return vec![Inline::Text(leading + &trailing)];
  }
  let mut out = Vec::new();
  if !leading.is_empty() {
    out.push(Inline::Text(leading));
  }
  out.push(make(content));
  if !trailing.is_empty() {
    out.push(Inline::Text(trailing));
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn long_messages_by_lines_or_length() {
    assert!(!is_long("short"));
    assert!(!is_long(&"line\n".repeat(14)));
    assert!(is_long(&"line\n".repeat(15)));
    assert!(is_long(&"x".repeat(1_401)));
    assert!(!is_long(&"ü".repeat(1_400)));
  }

  #[test]
  fn escaped_punctuation_is_literal() {
    assert_eq!(
      parse_inline("¯\\\\\\_(ツ)\\_/¯"),
      [Inline::Text("¯\\_(ツ)_/¯".into())]
    );
  }

  #[test]
  fn plain_text_stays_text() {
    assert_eq!(parse_inline("hello world"), [Inline::Text("hello world".into())]);
  }

  #[test]
  fn permalinks_are_links() {
    assert_eq!(
      parse_inline("see teamcraft:/c/1/m/2 now"),
      [
        Inline::Text("see ".into()),
        Inline::Link("teamcraft:/c/1/m/2".into()),
        Inline::Text(" now".into()),
      ]
    );
  }

  #[test]
  fn emphasis_and_code() {
    assert_eq!(
      parse_inline("a **b** _c_ `d` __e__ *f*"),
      [
        Inline::Text("a ".into()),
        Inline::Bold(vec![Inline::Text("b".into())]),
        Inline::Text(" ".into()),
        Inline::Italic(vec![Inline::Text("c".into())]),
        Inline::Text(" ".into()),
        Inline::Code("d".into()),
        Inline::Text(" ".into()),
        Inline::Underline(vec![Inline::Text("e".into())]),
        Inline::Text(" ".into()),
        Inline::Italic(vec![Inline::Text("f".into())]),
      ]
    );
  }

  #[test]
  fn snake_case_is_not_italic_or_underline() {
    assert_eq!(parse_inline("my_var_name"), [Inline::Text("my_var_name".into())]);
    assert_eq!(
      parse_inline("my__var__name"),
      [Inline::Text("my__var__name".into())]
    );
  }

  #[test]
  fn mentions_and_links() {
    assert_eq!(
      parse_inline("hi @sam, see https://example.com/a."),
      [
        Inline::Text("hi ".into()),
        Inline::Mention("sam".into()),
        Inline::Text(", see ".into()),
        Inline::Link("https://example.com/a".into()),
        Inline::Text(".".into()),
      ]
    );
  }

  #[test]
  fn labeled_links() {
    assert_eq!(
      parse_inline("read [the docs](https://example.com/d) now"),
      [
        Inline::Text("read ".into()),
        Inline::LabeledLink {
          url: "https://example.com/d".into(),
          label: "the docs".into()
        },
        Inline::Text(" now".into()),
      ]
    );
  }

  #[test]
  fn parentheses_in_links() {
    assert_eq!(
      parse_inline("see https://en.wikipedia.org/wiki/Rust_(programming_language)."),
      [
        Inline::Text("see ".into()),
        Inline::Link("https://en.wikipedia.org/wiki/Rust_(programming_language)".into()),
        Inline::Text(".".into()),
      ]
    );
    assert_eq!(
      parse_inline("(see https://example.com/a)"),
      [
        Inline::Text("(see ".into()),
        Inline::Link("https://example.com/a".into()),
        Inline::Text(")".into()),
      ]
    );
    assert_eq!(
      parse_inline("[Rust](https://en.wikipedia.org/wiki/Rust_%28programming_language%29)"),
      [Inline::LabeledLink {
        url: "https://en.wikipedia.org/wiki/Rust_(programming_language)".into(),
        label: "Rust".into()
      }]
    );
  }

  #[test]
  fn unsafe_schemes_are_plain_text() {
    assert_eq!(
      parse_inline("javascript:alert(1)"),
      [Inline::Text("javascript:alert(1)".into())]
    );
    assert_eq!(
      parse_inline("[x](javascript:alert(1))"),
      [Inline::Text("[x](javascript:alert(1))".into())]
    );
    assert_eq!(
      parse_inline("[x](data:text/html,hi)"),
      [Inline::Text("[x](data:text/html,hi)".into())]
    );
  }

  #[test]
  fn html_is_text() {
    assert_eq!(
      parse("<script>alert(1)</script>"),
      [Block::Paragraph(vec![vec![Inline::Text(
        "<script>alert(1)</script>".into()
      )]])]
    );
  }

  #[test]
  fn fenced_code_and_unterminated_fence() {
    let blocks = parse("before\n```rust\nlet x = 1;\n```\nafter");
    assert_eq!(blocks.len(), 3);
    assert_eq!(
      blocks[1],
      Block::Code {
        maybe_language: Some("rust".into()),
        code: "let x = 1;".into()
      }
    );
    assert!(matches!(parse("```\nopen").as_slice(), [Block::Code { .. }]));
  }

  #[test]
  fn unclosed_delimiters_stay_literal() {
    assert_eq!(parse_inline("2 * 3 ** 4"), [Inline::Text("2 * 3 ** 4".into())]);
  }

  #[test]
  fn block_kinds() {
    let blocks = parse("# Title\n> said\n>\n- a\n* b\n3. c\n4. d\nend\n####no");
    assert_eq!(
      blocks,
      [
        Block::Heading {
          level: 1,
          content: vec![Inline::Text("Title".into())]
        },
        Block::Quote(vec![vec![Inline::Text("said".into())], vec![]]),
        Block::List {
          kind: ListKind::Bulleted,
          items: vec![vec![Inline::Text("a".into())], vec![Inline::Text("b".into())]]
        },
        Block::List {
          kind: ListKind::Numbered { start: 3 },
          items: vec![vec![Inline::Text("c".into())], vec![Inline::Text("d".into())]]
        },
        Block::Paragraph(vec![
          vec![Inline::Text("end".into())],
          vec![Inline::Text("####no".into())]
        ]),
      ]
    );
  }

  #[test]
  fn near_misses_are_paragraphs() {
    for line in [
      "-1 degrees",
      "#hashtag",
      "1.5 litres",
      ">quote",
      "1234567890. big",
    ] {
      assert!(
        matches!(parse(line).as_slice(), [Block::Paragraph(_)]),
        "{line} should be text"
      );
    }
  }

  mod round_trip {
    use super::*;

    #[test]
    fn sources_in_canonical_form_round_trip() {
      for source in [
        "hello world",
        "a **bold** and _italic_ and __under__ and ~~gone~~",
        "**bold _and italic_**",
        "un*believ*able and *__both__*",
        "`code` and [label](https://example.com/x) and https://example.com",
        "@sam see teamcraft:/c/1/m/2",
        "# One\n## Two\n### Three",
        "> quoted\n> lines",
        "- one\n- two\n3. three\n4. four",
        "```rust\nfn main() {}\n```",
        "line one\n\nline three",
        "snake_case_name and C:\\path",
      ] {
        assert_eq!(to_source(&parse(source)), source, "{source}");
      }
    }

    #[test]
    fn literal_markup_characters_are_escaped() {
      let blocks = vec![Block::Paragraph(vec![vec![Inline::Text(
        "**not bold** _nor this_ `nor` ~~that~~ [x] *y*".into(),
      )]])];
      assert_round_trip(&blocks);
    }

    #[test]
    fn paragraph_lines_that_look_like_blocks_stay_paragraphs() {
      let blocks = vec![Block::Paragraph(vec![
        vec![Inline::Text("- not a list".into())],
        vec![Inline::Text("1. nor this".into())],
        vec![Inline::Text("# nor a heading".into())],
        vec![Inline::Text("> nor a quote".into())],
        vec![Inline::Text("```".into())],
      ])];
      assert_round_trip(&blocks);
    }

    #[test]
    fn spaces_at_emphasis_edges_move_outside() {
      let blocks = vec![Block::Paragraph(vec![vec![
        Inline::Text("a".into()),
        Inline::Bold(vec![Inline::Text(" b ".into())]),
        Inline::Text("c".into()),
      ]])];
      let source = to_source(&blocks);
      assert_eq!(source, "a **b** c");
    }

    #[test]
    fn italic_inside_a_word() {
      let blocks = vec![Block::Paragraph(vec![vec![
        Inline::Text("un".into()),
        Inline::Italic(vec![Inline::Text("believ".into())]),
        Inline::Text("able".into()),
      ]])];
      assert_round_trip(&blocks);
    }

    #[test]
    fn nested_emphasis_in_the_editor_order() {
      // The editor nests underline, bold, strike, italic from the outside in.
      let blocks = vec![Block::Paragraph(vec![vec![Inline::Underline(vec![
        Inline::Bold(vec![Inline::Strike(vec![Inline::Italic(vec![Inline::Text(
          "all".into(),
        )])])]),
      ])]])];
      assert_round_trip(&blocks);
    }

    #[test]
    fn code_with_a_backtick_falls_back_to_text() {
      let blocks = vec![Block::Paragraph(vec![vec![Inline::Code("a`b".into())]])];
      assert_eq!(
        parse(&to_source(&blocks)),
        [Block::Paragraph(vec![vec![Inline::Text("a`b".into())]])]
      );
    }

    #[test]
    fn labeled_links_with_parentheses() {
      let blocks = vec![Block::Paragraph(vec![vec![
        Inline::LabeledLink {
          url: "https://en.wikipedia.org/wiki/Rust_(programming_language)".into(),
          label: "Rust".into(),
        },
        Inline::Text(" and ".into()),
        Inline::Link("https://en.wikipedia.org/wiki/C_(language)".into()),
      ]])];
      assert_round_trip(&blocks);
    }

    #[test]
    fn labels_with_brackets() {
      let blocks = vec![Block::Paragraph(vec![vec![Inline::LabeledLink {
        url: "https://example.com".into(),
        label: "see [1]".into(),
      }]])];
      assert_round_trip(&blocks);
    }

    fn assert_round_trip(blocks: &[Block]) {
      let source = to_source(blocks);
      assert_eq!(parse(&source), blocks, "via {source:?}");
    }
  }
}
