//! The message Markdown subset (`teamcraft_client_api::markdown`) for the mobile apps, so they show
//! a body exactly as desktop and web do. Nested emphasis is flattened into runs with their styles,
//! which maps directly onto Compose's `AnnotatedString` (and SwiftUI's `AttributedString`).

use teamcraft_client_api::markdown::{self, Block, Inline, ListKind};

/// One block of a message body.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MarkupBlock {
  /// Lines of text; each source line is its own line.
  Paragraph { lines: Vec<MarkupLine> },
  /// `#` to `###` (`level` 1 to 3).
  Heading { level: u8, line: MarkupLine },
  /// Consecutive `>` lines.
  Quote { lines: Vec<MarkupLine> },
  /// A bulleted or numbered list (named `Items`: `List` would shadow the list type in Kotlin).
  Items {
    kind: MarkupListKind,
    items: Vec<MarkupLine>,
  },
  /// A fenced code block, verbatim.
  Code {
    maybe_language: Option<String>,
    code: String,
  },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MarkupListKind {
  Bulleted,
  /// Numbered from `start`.
  Numbered {
    start: u32,
  },
}

/// One line of inline content.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MarkupLine {
  pub runs: Vec<MarkupRun>,
}

/// A piece of a line with one look.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MarkupRun {
  pub kind: MarkupRunKind,
  /// What is shown: the text, the code, the link's label (the URL for a bare link), `@name`.
  pub text: String,
  pub is_bold: bool,
  pub is_italic: bool,
  pub is_underline: bool,
  pub is_strike: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MarkupRunKind {
  Text,
  /// Inline code (no markup inside).
  Code,
  /// A web link or a message permalink (`teamcraft:/c/…`). `is_labeled` for `[label](url)`: the app
  /// shows the URL on request, so the label can't hide where it goes.
  Link {
    url: String,
    is_labeled: bool,
  },
  /// `@name` as typed; whether it names a member is the app's to check.
  Mention {
    name: String,
  },
}

/// A message body in the shared Markdown subset, as blocks of styled runs.
#[uniffi::export]
pub fn message_markup(body: String) -> Vec<MarkupBlock> {
  markdown::parse(&body).into_iter().map(block).collect()
}

/// Whether a message body is long enough to start collapsed (the same limit as desktop and web).
#[uniffi::export]
pub fn is_long_message(body: String) -> bool {
  markdown::is_long(&body)
}

fn block(block: Block) -> MarkupBlock {
  match block {
    Block::Paragraph(lines) => MarkupBlock::Paragraph {
      lines: lines.iter().map(|inlines| line(inlines)).collect(),
    },
    Block::Heading { level, content } => MarkupBlock::Heading {
      level,
      line: line(&content),
    },
    Block::Quote(lines) => MarkupBlock::Quote {
      lines: lines.iter().map(|inlines| line(inlines)).collect(),
    },
    Block::List { kind, items } => MarkupBlock::Items {
      kind: match kind {
        ListKind::Bulleted => MarkupListKind::Bulleted,
        ListKind::Numbered { start } => MarkupListKind::Numbered { start },
      },
      items: items.iter().map(|inlines| line(inlines)).collect(),
    },
    Block::Code { maybe_language, code } => MarkupBlock::Code { maybe_language, code },
  }
}

fn line(inlines: &[Inline]) -> MarkupLine {
  let mut runs = Vec::new();
  flatten(inlines, Styles::default(), &mut runs);
  MarkupLine { runs }
}

/// The emphasis the enclosing inlines put on a run.
#[derive(Clone, Copy, Default)]
struct Styles {
  is_bold: bool,
  is_italic: bool,
  is_underline: bool,
  is_strike: bool,
}

fn flatten(inlines: &[Inline], styles: Styles, runs: &mut Vec<MarkupRun>) {
  for inline in inlines {
    let (kind, text) = match inline {
      Inline::Bold(inner) => {
        flatten(
          inner,
          Styles {
            is_bold: true,
            ..styles
          },
          runs,
        );
        continue;
      },
      Inline::Italic(inner) => {
        flatten(
          inner,
          Styles {
            is_italic: true,
            ..styles
          },
          runs,
        );
        continue;
      },
      Inline::Underline(inner) => {
        flatten(
          inner,
          Styles {
            is_underline: true,
            ..styles
          },
          runs,
        );
        continue;
      },
      Inline::Strike(inner) => {
        flatten(
          inner,
          Styles {
            is_strike: true,
            ..styles
          },
          runs,
        );
        continue;
      },
      Inline::Text(text) => (MarkupRunKind::Text, text.clone()),
      Inline::Code(code) => (MarkupRunKind::Code, code.clone()),
      Inline::Link(url) => (
        MarkupRunKind::Link {
          url: url.clone(),
          is_labeled: false,
        },
        url.clone(),
      ),
      Inline::LabeledLink { url, label } => (
        MarkupRunKind::Link {
          url: url.clone(),
          is_labeled: true,
        },
        label.clone(),
      ),
      Inline::Mention(name) => (MarkupRunKind::Mention { name: name.clone() }, format!("@{name}")),
    };
    runs.push(MarkupRun {
      kind,
      text,
      is_bold: styles.is_bold,
      is_italic: styles.is_italic,
      is_underline: styles.is_underline,
      is_strike: styles.is_strike,
    });
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn inline_code_loses_its_backticks() {
    assert_eq!(
      message_markup("run `curl -v` now".into()),
      vec![MarkupBlock::Paragraph {
        lines: vec![MarkupLine {
          runs: vec![text("run "), run(MarkupRunKind::Code, "curl -v"), text(" now")],
        }]
      }]
    );
  }

  #[test]
  fn nested_emphasis_is_flattened_into_styled_runs() {
    let blocks = message_markup("**bold _both_**".into());
    let MarkupBlock::Paragraph { lines } = &blocks[0] else {
      panic!("not a paragraph: {blocks:?}");
    };
    let runs = &lines[0].runs;
    assert_eq!(runs.len(), 2);
    assert!(runs[0].is_bold && !runs[0].is_italic);
    assert_eq!(runs[1].text, "both");
    assert!(runs[1].is_bold && runs[1].is_italic);
  }

  #[test]
  fn links_keep_their_target_and_label() {
    let blocks = message_markup("see [the docs](https://example.com/a) or https://example.com/b".into());
    let MarkupBlock::Paragraph { lines } = &blocks[0] else {
      panic!("not a paragraph: {blocks:?}");
    };
    let links: Vec<_> = lines[0]
      .runs
      .iter()
      .filter_map(|run| match &run.kind {
        MarkupRunKind::Link { url, is_labeled } => Some((run.text.as_str(), url.as_str(), *is_labeled)),
        _ => None,
      })
      .collect();
    assert_eq!(
      links,
      vec![
        ("the docs", "https://example.com/a", true),
        ("https://example.com/b", "https://example.com/b", false),
      ]
    );
  }

  #[test]
  fn code_blocks_quotes_and_lists_stay_blocks() {
    let blocks = message_markup("> quoted\n```rust\nfn main() {}\n```\n2. two\n3. three".into());
    assert!(matches!(&blocks[0], MarkupBlock::Quote { lines } if lines.len() == 1));
    assert_eq!(
      blocks[1],
      MarkupBlock::Code {
        maybe_language: Some("rust".into()),
        code: "fn main() {}".into()
      }
    );
    assert!(matches!(
      &blocks[2],
      MarkupBlock::Items { kind: MarkupListKind::Numbered { start: 2 }, items } if items.len() == 2
    ));
  }

  #[test]
  fn a_mention_carries_the_name() {
    let blocks = message_markup("hi @alice".into());
    let MarkupBlock::Paragraph { lines } = &blocks[0] else {
      panic!("not a paragraph: {blocks:?}");
    };
    assert_eq!(
      lines[0].runs[1].kind,
      MarkupRunKind::Mention { name: "alice".into() }
    );
    assert_eq!(lines[0].runs[1].text, "@alice");
  }

  fn text(text: &str) -> MarkupRun {
    run(MarkupRunKind::Text, text)
  }

  fn run(kind: MarkupRunKind, text: &str) -> MarkupRun {
    MarkupRun {
      kind,
      text: text.into(),
      is_bold: false,
      is_italic: false,
      is_underline: false,
      is_strike: false,
    }
  }
}
