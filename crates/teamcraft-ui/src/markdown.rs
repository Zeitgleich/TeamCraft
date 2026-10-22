//! A small, safe Markdown subset for message bodies. It produces a tree the UI turns into elements, so
//! no HTML string is ever injected. Supported: paragraphs, fenced code, `code`, **bold**, _italic_,
//! ~~strike~~, http(s) links and @mentions.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
  /// Lines of inline content; a newline in the source becomes a line break.
  Paragraph(Vec<Vec<Inline>>),
  Code {
    maybe_language: Option<String>,
    code: String,
  },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inline {
  Text(String),
  Bold(Vec<Inline>),
  Italic(Vec<Inline>),
  Strike(Vec<Inline>),
  Code(String),
  Link(String),
  Mention(String),
}

pub fn parse(source: &str) -> Vec<Block> {
  let mut blocks = Vec::new();
  let mut paragraph: Vec<Vec<Inline>> = Vec::new();
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
      if !paragraph.is_empty() {
        blocks.push(Block::Paragraph(std::mem::take(&mut paragraph)));
      }
      let info = info.trim();
      maybe_fence = Some(((!info.is_empty()).then(|| info.to_owned()), Vec::new()));
    } else {
      paragraph.push(parse_inline(line));
    }
  }
  // An unterminated fence still renders as code instead of swallowing the rest silently.
  if let Some((language, code_lines)) = maybe_fence {
    blocks.push(Block::Code {
      maybe_language: language,
      code: code_lines.join("\n"),
    });
  }
  if !paragraph.is_empty() {
    blocks.push(Block::Paragraph(paragraph));
  }
  blocks
}

pub fn parse_inline(text: &str) -> Vec<Inline> {
  let mut out = Vec::new();
  let mut plain = String::new();
  let mut rest = text;

  while let Some(ch) = rest.chars().next() {
    let before_is_boundary = plain.chars().last().is_none_or(|prev| !prev.is_alphanumeric());
    let parsed = match ch {
      '`' => delimited(rest, "`").map(|(inner, used)| (Inline::Code(inner.to_owned()), used)),
      '*' if rest.starts_with("**") => {
        delimited(rest, "**").map(|(inner, used)| (Inline::Bold(parse_inline(inner)), used))
      },
      '~' if rest.starts_with("~~") => {
        delimited(rest, "~~").map(|(inner, used)| (Inline::Strike(parse_inline(inner)), used))
      },
      '_' | '*' if before_is_boundary => {
        let marker = &rest[..1];
        delimited(rest, marker).map(|(inner, used)| (Inline::Italic(parse_inline(inner)), used))
      },
      '@' if before_is_boundary => mention(rest),
      'h' if before_is_boundary => link(rest),
      _ => None,
    };
    match parsed {
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

/// Returns the content between `marker` at the start of `rest` and the next `marker`, plus bytes used.
fn delimited<'a>(rest: &'a str, marker: &str) -> Option<(&'a str, usize)> {
  let body = &rest[marker.len()..];
  let end = body.find(marker)?;
  let inner = &body[..end];
  // `** **` or `__` are not emphasis; the content must start and end on a non-space.
  if inner.is_empty() || inner.starts_with(char::is_whitespace) || inner.ends_with(char::is_whitespace) {
    return None;
  }
  Some((inner, marker.len() * 2 + end))
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
  if !(rest.starts_with("http://") || rest.starts_with("https://")) {
    return None;
  }
  let raw = rest.split(char::is_whitespace).next().unwrap_or(rest);
  let url = raw.trim_end_matches(['.', ',', ';', ':', '!', '?', ')']);
  (url.len() > "https://".len()).then(|| (Inline::Link(url.to_owned()), url.len()))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn plain_text_stays_text() {
    assert_eq!(parse_inline("hello world"), [Inline::Text("hello world".into())]);
  }

  #[test]
  fn emphasis_and_code() {
    assert_eq!(
      parse_inline("a **b** _c_ `d`"),
      [
        Inline::Text("a ".into()),
        Inline::Bold(vec![Inline::Text("b".into())]),
        Inline::Text(" ".into()),
        Inline::Italic(vec![Inline::Text("c".into())]),
        Inline::Text(" ".into()),
        Inline::Code("d".into()),
      ]
    );
  }

  #[test]
  fn snake_case_is_not_italic() {
    assert_eq!(parse_inline("my_var_name"), [Inline::Text("my_var_name".into())]);
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
  fn unsafe_schemes_are_plain_text() {
    assert_eq!(
      parse_inline("javascript:alert(1)"),
      [Inline::Text("javascript:alert(1)".into())]
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
}
