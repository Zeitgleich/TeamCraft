//! The composer's document model between the browser DOM and the Markdown subset ([`crate::markdown`]).
//!
//! The editor is a `contenteditable` element. Its content, and anything pasted into it, is read into a
//! plain [`DomNode`] tree and turned into [`Block`]s by an allow-list walk: only the formatting the
//! subset has survives, every other tag is unwrapped to its text, and scripts, styles, frames, forms
//! and media are dropped with their content. No HTML from outside ever reaches a message or the page:
//! pasted HTML is parsed into an inert document, read through this walk and rebuilt from [`Block`]s.
//!
//! Pure Rust, so the sanitizer and the round trip are tested natively; `composer_editor` does the DOM.

use crate::markdown::{self, Block, Inline, ListKind, MAX_HEADING_LEVEL};

/// Elements dropped together with everything inside them.
const DROPPED_TAGS: [&str; 22] = [
  "script", "style", "head", "title", "meta", "link", "template", "noscript", "iframe", "frame", "frameset",
  "object", "embed", "svg", "math", "canvas", "video", "audio", "img", "button", "select", "textarea",
];

/// Elements that end the line before them and after them.
const BLOCK_TAGS: [&str; 22] = [
  "p",
  "div",
  "section",
  "article",
  "header",
  "footer",
  "main",
  "nav",
  "aside",
  "address",
  "figure",
  "figcaption",
  "dl",
  "dt",
  "dd",
  "table",
  "thead",
  "tbody",
  "tfoot",
  "tr",
  "form",
  "fieldset",
];

/// Monospace font families that mark pasted text as code (word processors have no code element).
const MONOSPACE_FONTS: [&str; 6] = [
  "monospace",
  "courier",
  "consolas",
  "menlo",
  "monaco",
  "source code",
];

/// Zero-width characters the editor uses to place the caret; never part of the message.
const CARET_MARKERS: [char; 2] = ['\u{200B}', '\u{FEFF}'];

/// A DOM node, read from the editor or from pasted HTML.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DomNode {
  Text(String),
  Element(DomElement),
}

/// An element with the few attributes the walk looks at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DomElement {
  /// Lower-case tag name.
  pub tag: String,
  pub maybe_href: Option<String>,
  pub maybe_style: Option<String>,
  /// `start` of an `<ol>`.
  pub maybe_start: Option<String>,
  /// `data-language`, or the `language-…` class of pasted code.
  pub maybe_language: Option<String>,
  pub children: Vec<DomNode>,
}

/// How text nodes treat white space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Whitespace {
  /// The editor (`white-space: pre-wrap`): spaces are kept, a newline breaks the line.
  Preserve,
  /// Pasted HTML: runs of white space are one space, as the browser shows them.
  Collapse,
}

/// Inline formatting the toolbar and the shortcuts apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InlineStyle {
  Bold,
  Italic,
  Underline,
  Strike,
  Code,
}

/// Block formatting the toolbar and the shortcuts apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockFormat {
  Paragraph,
  Heading(u8),
  Quote,
  Bulleted,
  Numbered,
  CodeBlock,
}

/// A Markdown span typed into the editor (`**bold**`), found when its closing marker is typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineShortcut {
  pub style: InlineStyle,
  /// UTF-16 offset of the opening marker in the text before the caret.
  pub start: usize,
  /// The text between the markers.
  pub inner: String,
}

/// Reads editor or pasted content into blocks of the subset.
pub fn to_blocks(nodes: &[DomNode], whitespace: Whitespace) -> Vec<Block> {
  let mut builder = Builder {
    whitespace,
    out: Vec::new(),
    runs: Vec::new(),
    lists: Vec::new(),
  };
  builder.walk(nodes, &Context::default());
  builder.flush(LineKind::Text);
  group(builder.out)
}

/// The editor content for blocks: what the composer shows for a restored draft.
pub fn from_blocks(blocks: &[Block]) -> Vec<DomNode> {
  blocks.iter().map(block_node).collect()
}

/// Escaped HTML of a tree built by [`from_blocks`], for inserting it at the caret.
pub fn to_html(nodes: &[DomNode]) -> String {
  let mut out = String::new();
  for node in nodes {
    write_html(&mut out, node);
  }
  out
}

/// The block format typed at the start of a line, when the space after it is typed: `-` and `*`
/// start a bulleted list, `1.` a numbered one, `>` a quote and `#` to `###` a heading.
pub fn block_shortcut(line_before_space: &str) -> Option<BlockFormat> {
  match line_before_space {
    "-" | "*" => Some(BlockFormat::Bulleted),
    ">" => Some(BlockFormat::Quote),
    marker
      if marker.len() <= usize::from(MAX_HEADING_LEVEL)
        && !marker.is_empty()
        && marker.bytes().all(|b| b == b'#') =>
    {
      // Invariant: at most MAX_HEADING_LEVEL bytes, which fits in u8.
      Some(BlockFormat::Heading(marker.len() as u8))
    },
    marker
      if marker.strip_suffix('.').is_some_and(|digits| {
        !digits.is_empty() && digits.len() <= 3 && digits.bytes().all(|b| b.is_ascii_digit())
      }) =>
    {
      Some(BlockFormat::Numbered)
    },
    _ => None,
  }
}

/// The Markdown span that the text before the caret ends with, if its closing marker was just
/// typed: `**b**`, `__u__`, `~~s~~`, `` `c` ``, `*i*` or `_i_`, with the parser's rules (no space
/// at the edges, `_` only at a word start).
pub fn inline_shortcut(before: &str) -> Option<InlineShortcut> {
  const MARKERS: [(&str, InlineStyle); 6] = [
    ("**", InlineStyle::Bold),
    ("__", InlineStyle::Underline),
    ("~~", InlineStyle::Strike),
    ("`", InlineStyle::Code),
    ("*", InlineStyle::Italic),
    ("_", InlineStyle::Italic),
  ];
  for (marker, style) in MARKERS {
    let Some(head) = before.strip_suffix(marker) else {
      continue;
    };
    // `***` is not an italic `*` closing after a bold one.
    if marker.len() == 1 && head.ends_with(marker) {
      continue;
    }
    let Some(open) = head.rfind(marker) else {
      continue;
    };
    let inner = &head[open + marker.len()..];
    let prefix = &head[..open];
    let edges_ok =
      !inner.is_empty() && !inner.starts_with(char::is_whitespace) && !inner.ends_with(char::is_whitespace);
    let marker_char = marker.chars().next().unwrap_or_default();
    let doubled =
      prefix.ends_with(marker_char) || inner.starts_with(marker_char) || inner.ends_with(marker_char);
    let needs_boundary = marker.starts_with('_');
    let at_boundary = prefix.chars().last().is_none_or(|ch| !ch.is_alphanumeric());
    if edges_ok && !doubled && (!needs_boundary || at_boundary) && !inner.contains(marker) {
      return Some(InlineShortcut {
        style,
        start: prefix.encode_utf16().count(),
        inner: inner.to_owned(),
      });
    }
  }
  None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineKind {
  Text,
  Heading(u8),
  Quote,
  Item { is_numbered: bool, number: u32 },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Style {
  is_bold: bool,
  is_italic: bool,
  is_underline: bool,
  is_strike: bool,
  is_code: bool,
  maybe_link: Option<String>,
}

#[derive(Clone, Debug)]
struct Context {
  kind: LineKind,
  style: Style,
}

impl Default for Context {
  fn default() -> Self {
    Self {
      kind: LineKind::Text,
      style: Style::default(),
    }
  }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Run {
  text: String,
  style: Style,
}

enum Out {
  Line(LineKind, Vec<Run>),
  Code(Option<String>, String),
}

struct Builder {
  whitespace: Whitespace,
  out: Vec<Out>,
  /// The line being read.
  runs: Vec<Run>,
  /// Open lists: whether numbered, and the next item's number.
  lists: Vec<(bool, u32)>,
}

impl Builder {
  fn walk(&mut self, nodes: &[DomNode], context: &Context) {
    for node in nodes {
      match node {
        DomNode::Text(text) => self.add_text(text, context),
        DomNode::Element(element) => self.element(element, context),
      }
    }
  }

  fn element(&mut self, element: &DomElement, context: &Context) {
    let tag = element.tag.as_str();
    if DROPPED_TAGS.contains(&tag) {
      return;
    }
    match tag {
      "br" => self.break_line(context.kind),
      "pre" => {
        self.flush(context.kind);
        let mut code = String::new();
        code_text(&element.children, &mut code);
        let maybe_language = element
          .maybe_language
          .clone()
          .or_else(|| code_language(&element.children));
        self
          .out
          .push(Out::Code(maybe_language, code.trim_end_matches('\n').to_owned()));
      },
      "ul" | "ol" => {
        self.flush(context.kind);
        let start = element
          .maybe_start
          .as_deref()
          .and_then(|start| start.trim().parse().ok())
          .unwrap_or(1);
        self.lists.push((tag == "ol", start));
        self.walk(&element.children, context);
        self.lists.pop();
        self.flush(context.kind);
      },
      "li" => {
        self.flush(context.kind);
        let (is_numbered, number) = match self.lists.last_mut() {
          Some((is_numbered, next)) => {
            let number = *next;
            *next = next.saturating_add(1);
            (*is_numbered, number)
          },
          None => (false, 1),
        };
        let kind = LineKind::Item { is_numbered, number };
        self.walk(
          &element.children,
          &Context {
            kind,
            ..context.clone()
          },
        );
        self.flush(kind);
      },
      "blockquote" => self.block(element, context, LineKind::Quote),
      "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
        let level = tag[1..]
          .parse::<u8>()
          .unwrap_or(MAX_HEADING_LEVEL)
          .min(MAX_HEADING_LEVEL);
        // A heading inside a quote or list item stays part of it.
        let kind = match context.kind {
          LineKind::Text => LineKind::Heading(level),
          other => other,
        };
        self.block(element, context, kind);
      },
      tag if BLOCK_TAGS.contains(&tag) => self.block(element, context, context.kind),
      "td" | "th" => {
        self.walk(&element.children, context);
        self.add_text(" ", context);
      },
      _ => {
        let style = inline_style(element, &context.style);
        self.walk(
          &element.children,
          &Context {
            style,
            ..context.clone()
          },
        );
      },
    }
  }

  fn block(&mut self, element: &DomElement, context: &Context, kind: LineKind) {
    self.flush(context.kind);
    self.walk(
      &element.children,
      &Context {
        kind,
        ..context.clone()
      },
    );
    self.flush(kind);
  }

  fn add_text(&mut self, text: &str, context: &Context) {
    let cleaned: String = text
      .chars()
      .filter(|ch| !CARET_MARKERS.contains(ch))
      .map(|ch| if ch == '\u{A0}' { ' ' } else { ch })
      .collect();
    match self.whitespace {
      Whitespace::Collapse => {
        let mut collapsed = String::with_capacity(cleaned.len());
        let mut last_was_space = self.runs.is_empty() || self.ends_with_space();
        for ch in cleaned.chars() {
          if ch.is_whitespace() {
            if !last_was_space {
              collapsed.push(' ');
            }
            last_was_space = true;
          } else {
            collapsed.push(ch);
            last_was_space = false;
          }
        }
        self.push_run(&collapsed, &context.style);
      },
      Whitespace::Preserve => {
        for (index, part) in cleaned.split('\n').enumerate() {
          if index > 0 {
            self.break_line(context.kind);
          }
          self.push_run(part.trim_end_matches('\r'), &context.style);
        }
      },
    }
  }

  fn ends_with_space(&self) -> bool {
    self.runs.last().is_some_and(|run| run.text.ends_with(' '))
  }

  fn push_run(&mut self, text: &str, style: &Style) {
    if text.is_empty() {
      return;
    }
    match self.runs.last_mut() {
      Some(last) if last.style == *style => last.text.push_str(text),
      _ => self.runs.push(Run {
        text: text.to_owned(),
        style: style.clone(),
      }),
    }
  }

  /// Ends the current line, even when it is empty (a `<br>`).
  fn break_line(&mut self, kind: LineKind) {
    let runs = trimmed(self.whitespace, std::mem::take(&mut self.runs));
    self.out.push(Out::Line(kind, runs));
  }

  /// Ends the current line if it has content (the end of a block).
  fn flush(&mut self, kind: LineKind) {
    let runs = trimmed(self.whitespace, std::mem::take(&mut self.runs));
    if !runs.is_empty() {
      self.out.push(Out::Line(kind, runs));
    }
  }
}

/// Pasted lines lose the spaces around them, as the browser doesn't show them either.
fn trimmed(whitespace: Whitespace, mut runs: Vec<Run>) -> Vec<Run> {
  if whitespace == Whitespace::Collapse {
    if let Some(first) = runs.first_mut() {
      first.text = first.text.trim_start().to_owned();
    }
    if let Some(last) = runs.last_mut() {
      last.text = last.text.trim_end().to_owned();
    }
    runs.retain(|run| !run.text.is_empty());
  }
  runs
}

/// The formatting an inline element adds; links only with a scheme the subset allows.
fn inline_style(element: &DomElement, outer: &Style) -> Style {
  let mut style = outer.clone();
  match element.tag.as_str() {
    "b" | "strong" => style.is_bold = true,
    "i" | "em" | "cite" | "dfn" | "var" => style.is_italic = true,
    "u" | "ins" => style.is_underline = true,
    "s" | "strike" | "del" => style.is_strike = true,
    "code" | "kbd" | "samp" | "tt" => style.is_code = true,
    "a" => {
      style.maybe_link = element
        .maybe_href
        .as_deref()
        .map(str::trim)
        .filter(|href| markdown::is_allowed_link(href))
        .map(str::to_owned);
    },
    _ => {},
  }
  if let Some(css) = &element.maybe_style {
    apply_css(css, &mut style);
  }
  style
}

/// What inline CSS says about weight, slant, decoration and font (word processors paste spans).
fn apply_css(css: &str, style: &mut Style) {
  for declaration in css.split(';') {
    let Some((property, value)) = declaration.split_once(':') else {
      continue;
    };
    let value = value.trim().to_ascii_lowercase();
    match property.trim().to_ascii_lowercase().as_str() {
      "font-weight" => {
        style.is_bold = match value.as_str() {
          "bold" | "bolder" => true,
          "normal" | "lighter" => false,
          number => number
            .parse::<u16>()
            .map_or(style.is_bold, |weight| weight >= 600),
        };
      },
      "font-style" => style.is_italic = value.starts_with("italic") || value.starts_with("oblique"),
      "text-decoration" | "text-decoration-line" => {
        style.is_underline |= value.contains("underline");
        style.is_strike |= value.contains("line-through");
      },
      "font-family" => style.is_code |= MONOSPACE_FONTS.iter().any(|font| value.contains(font)),
      _ => {},
    }
  }
}

/// The text of a `<pre>`, line breaks included.
fn code_text(nodes: &[DomNode], out: &mut String) {
  for node in nodes {
    match node {
      DomNode::Text(text) => out.extend(
        text
          .chars()
          .filter(|ch| !CARET_MARKERS.contains(ch))
          .map(|ch| if ch == '\u{A0}' { ' ' } else { ch }),
      ),
      DomNode::Element(element) if element.tag == "br" => out.push('\n'),
      DomNode::Element(element) if DROPPED_TAGS.contains(&element.tag.as_str()) => {},
      DomNode::Element(element) => {
        let is_line = matches!(element.tag.as_str(), "div" | "p");
        if is_line && !out.is_empty() && !out.ends_with('\n') {
          out.push('\n');
        }
        code_text(&element.children, out);
      },
    }
  }
}

/// `<pre><code class="language-rust">` as pasted from code hosts and documentation.
fn code_language(nodes: &[DomNode]) -> Option<String> {
  nodes.iter().find_map(|node| match node {
    DomNode::Element(element) if element.tag == "code" => element.maybe_language.clone(),
    _ => None,
  })
}

/// Consecutive lines of one kind become one block.
fn group(out: Vec<Out>) -> Vec<Block> {
  let mut blocks: Vec<Block> = Vec::new();
  for item in out {
    let (kind, runs) = match item {
      Out::Code(maybe_language, code) => {
        blocks.push(Block::Code { maybe_language, code });
        continue;
      },
      Out::Line(kind, runs) => (kind, runs),
    };
    let content = inlines(&runs);
    match (blocks.last_mut(), kind) {
      (Some(Block::Paragraph(lines)), LineKind::Text) | (Some(Block::Quote(lines)), LineKind::Quote) => {
        lines.push(content);
      },
      (Some(Block::List { kind: list, items }), LineKind::Item { is_numbered, .. })
        if matches!(list, ListKind::Numbered { .. }) == is_numbered =>
      {
        items.push(content);
      },
      (_, LineKind::Text) => blocks.push(Block::Paragraph(vec![content])),
      (_, LineKind::Quote) => blocks.push(Block::Quote(vec![content])),
      (_, LineKind::Heading(level)) => blocks.push(Block::Heading { level, content }),
      (_, LineKind::Item { is_numbered, number }) => blocks.push(Block::List {
        kind: if is_numbered {
          ListKind::Numbered { start: number }
        } else {
          ListKind::Bulleted
        },
        items: vec![content],
      }),
    }
  }
  blocks
}

/// Styled runs as nested inlines, from the outside in: underline, bold, strike, italic, then code or
/// a link (the order `markdown::to_source` can always write).
fn inlines(runs: &[Run]) -> Vec<Inline> {
  type Flag = fn(&mut Style) -> &mut bool;
  type Make = fn(Vec<Inline>) -> Inline;
  const FLAGS: [(Flag, Make); 4] = [
    (|style| &mut style.is_underline, Inline::Underline),
    (|style| &mut style.is_bold, Inline::Bold),
    (|style| &mut style.is_strike, Inline::Strike),
    (|style| &mut style.is_italic, Inline::Italic),
  ];
  let mut out: Vec<Inline> = Vec::new();
  let mut at = 0;
  while at < runs.len() {
    let mut first = runs[at].style.clone();
    let maybe_flag = FLAGS.iter().find(|(flag, _)| *flag(&mut first));
    if let Some((flag, make)) = maybe_flag {
      let end = at
        + runs[at..]
          .iter()
          .take_while(|run| *flag(&mut run.style.clone()))
          .count();
      let inner: Vec<Run> = runs[at..end]
        .iter()
        .map(|run| {
          let mut run = run.clone();
          *flag(&mut run.style) = false;
          run
        })
        .collect();
      out.push(make(inlines(&inner)));
      at = end;
      continue;
    }
    let run = &runs[at];
    if run.style.is_code {
      out.push(Inline::Code(run.text.clone()));
      at += 1;
    } else if let Some(url) = &run.style.maybe_link {
      let end = at
        + runs[at..]
          .iter()
          .take_while(|next| next.style.maybe_link.as_ref() == Some(url) && !next.style.is_code)
          .count();
      let label: String = runs[at..end].iter().map(|next| next.text.as_str()).collect();
      out.push(if label.trim() == url {
        Inline::Link(url.clone())
      } else {
        Inline::LabeledLink {
          url: url.clone(),
          label,
        }
      });
      at = end;
    } else {
      match out.last_mut() {
        Some(Inline::Text(previous)) => previous.push_str(&run.text),
        _ => out.push(Inline::Text(run.text.clone())),
      }
      at += 1;
    }
  }
  out
}

fn element(tag: &str, children: Vec<DomNode>) -> DomNode {
  DomNode::Element(DomElement {
    tag: tag.to_owned(),
    children,
    ..DomElement::default()
  })
}

/// An editable line: an empty one holds a `<br>` so the caret has somewhere to go.
fn line_children(inlines: &[Inline]) -> Vec<DomNode> {
  let children: Vec<DomNode> = inlines.iter().map(inline_node).collect();
  if children.is_empty() {
    vec![element("br", Vec::new())]
  } else {
    children
  }
}

fn block_node(block: &Block) -> DomNode {
  match block {
    Block::Paragraph(lines) if lines.len() == 1 => element("p", line_children(&lines[0])),
    // Every line is its own `<p>`, as Enter makes them; a wrapper keeps the block one node.
    Block::Paragraph(lines) => element(
      "div",
      lines
        .iter()
        .map(|line| element("p", line_children(line)))
        .collect(),
    ),
    Block::Heading { level, content } => element(&format!("h{level}"), line_children(content)),
    Block::Quote(lines) => element(
      "blockquote",
      lines
        .iter()
        .map(|line| element("p", line_children(line)))
        .collect(),
    ),
    Block::List { kind, items } => {
      let children = items
        .iter()
        .map(|item| element("li", line_children(item)))
        .collect();
      match kind {
        ListKind::Bulleted => element("ul", children),
        ListKind::Numbered { start } => DomNode::Element(DomElement {
          tag: "ol".to_owned(),
          maybe_start: (*start != 1).then(|| start.to_string()),
          children,
          ..DomElement::default()
        }),
      }
    },
    Block::Code { maybe_language, code } => DomNode::Element(DomElement {
      tag: "pre".to_owned(),
      maybe_language: maybe_language.clone(),
      children: if code.is_empty() {
        vec![element("br", Vec::new())]
      } else {
        vec![DomNode::Text(code.clone())]
      },
      ..DomElement::default()
    }),
  }
}

fn inline_node(inline: &Inline) -> DomNode {
  let styled = |tag: &str, inner: &[Inline]| element(tag, inner.iter().map(inline_node).collect());
  match inline {
    Inline::Text(text) | Inline::Link(text) => DomNode::Text(text.clone()),
    Inline::Mention(name) => DomNode::Text(format!("@{name}")),
    Inline::Bold(inner) => styled("strong", inner),
    Inline::Italic(inner) => styled("em", inner),
    Inline::Underline(inner) => styled("u", inner),
    Inline::Strike(inner) => styled("s", inner),
    Inline::Code(code) => element("code", vec![DomNode::Text(code.clone())]),
    Inline::LabeledLink { url, label } => DomNode::Element(DomElement {
      tag: "a".to_owned(),
      maybe_href: Some(url.clone()),
      children: vec![DomNode::Text(label.clone())],
      ..DomElement::default()
    }),
  }
}

fn write_html(out: &mut String, node: &DomNode) {
  match node {
    DomNode::Text(text) => escape_html(out, text),
    DomNode::Element(element) => {
      out.push('<');
      out.push_str(&element.tag);
      for (name, maybe_value) in [
        ("href", &element.maybe_href),
        ("start", &element.maybe_start),
        ("data-language", &element.maybe_language),
      ] {
        if let Some(value) = maybe_value {
          out.push(' ');
          out.push_str(name);
          out.push_str("=\"");
          escape_html(out, value);
          out.push('"');
        }
      }
      out.push('>');
      if element.tag == "br" {
        return;
      }
      for child in &element.children {
        write_html(out, child);
      }
      out.push_str("</");
      out.push_str(&element.tag);
      out.push('>');
    },
  }
}

fn escape_html(out: &mut String, text: &str) {
  for ch in text.chars() {
    match ch {
      '&' => out.push_str("&amp;"),
      '<' => out.push_str("&lt;"),
      '>' => out.push_str("&gt;"),
      '"' => out.push_str("&quot;"),
      '\'' => out.push_str("&#39;"),
      other => out.push(other),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::markdown::{parse, to_source};

  mod paste {
    use super::*;

    #[test]
    fn scripts_styles_and_frames_vanish_with_their_content() {
      let nodes = vec![
        el("script", vec![text("alert(1)")]),
        el("style", vec![text("body{}")]),
        el("p", vec![text("kept"), el("iframe", vec![text("frame")])]),
        el("svg", vec![el("script", vec![text("x")])]),
      ];
      assert_eq!(paste_source(&nodes), "kept");
    }

    #[test]
    fn unknown_tags_are_unwrapped_to_their_text() {
      let nodes = vec![el(
        "p",
        vec![
          el("marquee", vec![text("moving")]),
          text(" "),
          el("font", vec![text("text")]),
        ],
      )];
      assert_eq!(paste_source(&nodes), "moving text");
    }

    #[test]
    fn html_in_text_stays_text() {
      let nodes = vec![el("p", vec![text("<img src=x onerror=alert(1)>")])];
      let source = paste_source(&nodes);
      assert_eq!(source, "<img src=x onerror=alert(1)>");
      assert_eq!(
        parse(&source),
        [Block::Paragraph(vec![vec![Inline::Text(
          "<img src=x onerror=alert(1)>".into()
        )]])]
      );
    }

    #[test]
    fn links_with_other_schemes_lose_the_link() {
      let nodes = vec![el(
        "p",
        vec![
          link("javascript:alert(1)", "click"),
          text(" "),
          link("data:text/html,x", "data"),
          text(" "),
          link("https://example.com/a", "ok"),
        ],
      )];
      assert_eq!(paste_source(&nodes), "click data [ok](https://example.com/a)");
    }

    #[test]
    fn markup_characters_in_pasted_text_stay_literal() {
      let nodes = vec![el("p", vec![text("**not bold** and `x`")])];
      let blocks = to_blocks(&nodes, Whitespace::Collapse);
      assert_eq!(parse(&to_source(&blocks)), blocks);
    }

    #[test]
    fn formatting_tags_map_to_the_subset() {
      let nodes = vec![el(
        "div",
        vec![
          el("b", vec![text("bold")]),
          text(" "),
          el("i", vec![text("italic")]),
          text(" "),
          el("u", vec![text("under")]),
          text(" "),
          el("del", vec![text("gone")]),
          text(" "),
          el("kbd", vec![text("key")]),
        ],
      )];
      assert_eq!(paste_source(&nodes), "**bold** _italic_ __under__ ~~gone~~ `key`");
    }

    #[test]
    fn word_processor_spans_use_inline_css() {
      // Google Docs wraps everything in a `<b style="font-weight:normal">`.
      let nodes = vec![styled(
        "b",
        "font-weight:normal",
        vec![el(
          "p",
          vec![
            styled("span", "font-weight:700", vec![text("heavy")]),
            text(" "),
            styled(
              "span",
              "font-style:italic;text-decoration:line-through",
              vec![text("both")],
            ),
            text(" "),
            styled("span", "font-family:'Courier New',monospace", vec![text("mono")]),
            text(" plain"),
          ],
        )],
      )];
      assert_eq!(paste_source(&nodes), "**heavy** ~~_both_~~ `mono` plain");
    }

    #[test]
    fn white_space_collapses_like_the_browser_shows_it() {
      let nodes = vec![
        text("\n  "),
        el("p", vec![text("  two   spaces\n here ")]),
        text("\n"),
        el("p", vec![text("next")]),
      ];
      assert_eq!(paste_source(&nodes), "two spaces here\nnext");
    }

    #[test]
    fn headings_lists_quotes_and_code() {
      let nodes = vec![
        el("h1", vec![text("Title")]),
        el("h5", vec![text("Deep")]),
        el("blockquote", vec![el("p", vec![text("said")])]),
        el("ul", vec![el("li", vec![text("a")]), el("li", vec![text("b")])]),
        DomNode::Element(DomElement {
          tag: "ol".into(),
          maybe_start: Some("3".into()),
          children: vec![el("li", vec![text("c")])],
          ..DomElement::default()
        }),
        el(
          "pre",
          vec![DomNode::Element(DomElement {
            tag: "code".into(),
            maybe_language: Some("rust".into()),
            children: vec![text("fn main() {\n  run();\n}\n")],
            ..DomElement::default()
          })],
        ),
      ];
      assert_eq!(
        paste_source(&nodes),
        "# Title\n### Deep\n> said\n- a\n- b\n3. c\n```rust\nfn main() {\n  run();\n}\n```"
      );
    }

    #[test]
    fn table_cells_are_separated() {
      let nodes = vec![el(
        "table",
        vec![el(
          "tr",
          vec![el("td", vec![text("a")]), el("td", vec![text("b")])],
        )],
      )];
      assert_eq!(paste_source(&nodes), "a b");
    }

    #[test]
    fn pasted_html_is_rebuilt_escaped() {
      let blocks = vec![Block::Paragraph(vec![vec![
        Inline::Text("<b>\"x\" & 'y'</b>".into()),
        Inline::LabeledLink {
          url: "https://example.com/?a=1&b=\"2\"".into(),
          label: "l".into(),
        },
      ]])];
      assert_eq!(
        to_html(&from_blocks(&blocks)),
        "<p>&lt;b&gt;&quot;x&quot; &amp; &#39;y&#39;&lt;/b&gt;<a href=\"https://example.com/?a=1&amp;b=&quot;2&quot;\">l</a></p>"
      );
    }

    fn paste_source(nodes: &[DomNode]) -> String {
      to_source(&to_blocks(nodes, Whitespace::Collapse))
    }
  }

  mod editor {
    use super::*;

    #[test]
    fn markdown_round_trips_through_the_editor() {
      for source in [
        "hello @carol ",
        "a **bold** _italic_ __under__ ~~gone~~ `code` [label](https://example.com)",
        "line one\n\nline three",
        "# Title\n## Sub\n### Small",
        "> quoted\n>\n> more",
        "- a\n- b\n7. c\n8. d",
        "```rust\nfn main() {\n    run();\n}\n```",
        "```\n```",
        "two  spaces and https://example.com/a_b and snake_case",
        "\\*literal\\* stars",
      ] {
        let shown = from_blocks(&parse(source));
        assert_eq!(
          to_source(&to_blocks(&shown, Whitespace::Preserve)),
          source,
          "{source}"
        );
      }
    }

    #[test]
    fn browser_markup_is_read_like_ours() {
      // Chrome's contenteditable: `<div>` lines, `<b>`, `<br>` placeholders, text at the top level.
      let nodes = vec![
        text("first"),
        el("div", vec![el("b", vec![text("bold")]), text(" line")]),
        el("div", vec![el("br", vec![])]),
        el("div", vec![text("after"), el("br", vec![])]),
      ];
      assert_eq!(
        to_source(&to_blocks(&nodes, Whitespace::Preserve)),
        "first\n**bold** line\n\nafter"
      );
    }

    #[test]
    fn caret_markers_and_no_break_spaces_are_not_content() {
      let nodes = vec![el(
        "p",
        vec![el("code", vec![text("x")]), text("\u{200B}\u{A0}y")],
      )];
      assert_eq!(to_source(&to_blocks(&nodes, Whitespace::Preserve)), "`x` y");
    }

    #[test]
    fn newlines_in_text_break_lines() {
      let nodes = vec![el("p", vec![text("a\nb")])];
      assert_eq!(to_source(&to_blocks(&nodes, Whitespace::Preserve)), "a\nb");
    }

    #[test]
    fn empty_editor_has_no_blocks() {
      assert!(
        to_blocks(&[el("p", vec![el("br", vec![])])], Whitespace::Preserve)
          .iter()
          .all(|block| matches!(block, Block::Paragraph(lines) if lines.iter().all(Vec::is_empty)))
      );
      assert!(to_blocks(&[], Whitespace::Preserve).is_empty());
    }
  }

  mod shortcuts {
    use super::*;

    #[test]
    fn block_markers() {
      assert_eq!(block_shortcut("-"), Some(BlockFormat::Bulleted));
      assert_eq!(block_shortcut("*"), Some(BlockFormat::Bulleted));
      assert_eq!(block_shortcut("1."), Some(BlockFormat::Numbered));
      assert_eq!(block_shortcut("12."), Some(BlockFormat::Numbered));
      assert_eq!(block_shortcut(">"), Some(BlockFormat::Quote));
      assert_eq!(block_shortcut("##"), Some(BlockFormat::Heading(2)));
      for not in ["", "####", "a.", ".", "1", "--", "1.5."] {
        assert_eq!(block_shortcut(not), None, "{not}");
      }
    }

    #[test]
    fn inline_spans() {
      assert_eq!(
        inline_shortcut("say **hi**"),
        Some(InlineShortcut {
          style: InlineStyle::Bold,
          start: 4,
          inner: "hi".into()
        })
      );
      assert_eq!(
        inline_shortcut("`x`").map(|found| found.style),
        Some(InlineStyle::Code)
      );
      assert_eq!(
        inline_shortcut("~~x~~").map(|found| found.style),
        Some(InlineStyle::Strike)
      );
      assert_eq!(
        inline_shortcut("__x__").map(|found| found.style),
        Some(InlineStyle::Underline)
      );
      assert_eq!(
        inline_shortcut("a *x*").map(|found| found.style),
        Some(InlineStyle::Italic)
      );
      assert_eq!(
        inline_shortcut("a _x_").map(|found| found.style),
        Some(InlineStyle::Italic)
      );
    }

    #[test]
    fn offsets_count_utf16() {
      assert_eq!(inline_shortcut("😀 *x*").map(|found| found.start), Some(3));
    }

    #[test]
    fn non_spans() {
      for not in [
        "2 * 3 *",
        "** x**",
        "snake_case_",
        "a**",
        "**",
        "***x***",
        "x * y*",
        "my_var_",
      ] {
        assert_eq!(inline_shortcut(not), None, "{not}");
      }
    }
  }

  fn text(value: &str) -> DomNode {
    DomNode::Text(value.to_owned())
  }

  fn el(tag: &str, children: Vec<DomNode>) -> DomNode {
    element(tag, children)
  }

  fn styled(tag: &str, css: &str, children: Vec<DomNode>) -> DomNode {
    DomNode::Element(DomElement {
      tag: tag.to_owned(),
      maybe_style: Some(css.to_owned()),
      children,
      ..DomElement::default()
    })
  }

  fn link(href: &str, label: &str) -> DomNode {
    DomNode::Element(DomElement {
      tag: "a".to_owned(),
      maybe_href: Some(href.to_owned()),
      children: vec![text(label)],
      ..DomElement::default()
    })
  }
}
