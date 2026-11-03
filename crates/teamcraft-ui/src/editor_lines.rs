//! Lines of a code block or quote in the composer, and turning some of them back into normal text
//! (#843).
//!
//! The browser's `formatBlock` can't take a single line out of a `<pre>` or `<blockquote>`: it
//! leaves the block as it is, so a block once started could not be left. The composer instead reads
//! the block into a [`DomElement`], splits it into the lines it shows, and replaces it with the lines
//! before the selection (still the block), the selected lines as paragraphs, and the lines after
//! (still the block). Pure Rust, so it is tested natively; `composer_editor` does the DOM.

use crate::editor_doc::{self, CARET_MARKERS, DomElement, DomNode};

/// Elements that are a line of their own inside a quote; a `<p>` or `<div>` is unwrapped to its
/// content, the others stay as they are.
const LINE_TAGS: [&str; 12] = [
  "p",
  "div",
  "h1",
  "h2",
  "h3",
  "h4",
  "h5",
  "h6",
  "ul",
  "ol",
  "pre",
  "blockquote",
];

/// The kind of block whose lines are read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineBlock {
  /// `<pre>`: plain text, lines end at a newline or `<br>`.
  Code,
  /// `<blockquote>`: formatted lines, each a `<p>` (or a `<br>`-separated run).
  Quote,
}

/// One line as it is shown: inline content, or a single block element (a heading or list in a quote).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line(pub Vec<DomNode>);

/// A block with some of its lines turned into paragraphs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unwrapped {
  /// The lines before the selection, still in the block.
  pub maybe_before: Option<DomNode>,
  /// The selected lines, each a paragraph (or the block element it already was).
  pub lines: Vec<DomNode>,
  /// The lines after the selection, still in the block.
  pub maybe_after: Option<DomNode>,
}

impl LineBlock {
  /// The kind of a `pre` or `blockquote` element.
  pub fn of(tag: &str) -> Option<Self> {
    match tag {
      "pre" => Some(Self::Code),
      "blockquote" => Some(Self::Quote),
      _ => None,
    }
  }
}

impl Line {
  /// Whether the line shows nothing (an empty line, or only white space and caret markers).
  pub fn is_blank(&self) -> bool {
    let mut text = String::new();
    editor_doc::code_text(&self.0, &mut text);
    text.trim().is_empty()
  }
}

/// The lines the block shows. A `<br>` or newline at the very end only holds the last line open and
/// starts no line of its own, as in the browser.
pub fn lines(kind: LineBlock, children: &[DomNode]) -> Vec<Line> {
  let mut lines = split(kind, children, Trailing::Placeholder);
  if lines.is_empty() {
    lines.push(Line::default());
  }
  lines
}

/// The index of the line a position is on, from the block's content before it (the block's start
/// up to the position, as a cloned range gives it).
pub fn line_index(kind: LineBlock, before: &[DomNode]) -> usize {
  split(kind, before, Trailing::Line).len().saturating_sub(1)
}

/// How far into its line a position is, in UTF-16 units of the line's text, from the block's content
/// before it; caret markers don't count.
pub fn column(kind: LineBlock, before: &[DomNode]) -> usize {
  split(kind, before, Trailing::Line).last().map_or(0, |line| {
    let mut text = String::new();
    editor_doc::code_text(&line.0, &mut text);
    text.encode_utf16().count()
  })
}

/// Whether a position is at the very start of the block: nothing shown and no line break before it.
pub fn is_at_start(kind: LineBlock, before: &[DomNode]) -> bool {
  let lines = split(kind, before, Trailing::Line);
  lines.len() <= 1 && lines.iter().all(Line::is_blank)
}

/// The block with lines `first..=last` as paragraphs. Indexes past the end are clamped.
pub fn unwrap(block: &DomElement, kind: LineBlock, first: usize, last: usize) -> Unwrapped {
  let lines = lines(kind, &block.children);
  let end = lines.len() - 1;
  let (first, last) = (first.min(end), last.min(end).max(first.min(end)));
  let rebuild = |part: &[Line]| (!part.is_empty()).then(|| rebuilt(block, kind, part));
  Unwrapped {
    maybe_before: rebuild(&lines[..first]),
    lines: lines[first..=last].iter().map(paragraph).collect(),
    maybe_after: rebuild(&lines[last + 1..]),
  }
}

/// What a `<br>` or newline at the end of the content means.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Trailing {
  /// The end of a whole block: it holds the last line open and starts none.
  Placeholder,
  /// The end of the content before a position: the position is on a new line.
  Line,
}

fn split(kind: LineBlock, nodes: &[DomNode], trailing: Trailing) -> Vec<Line> {
  match kind {
    LineBlock::Code => code_lines(nodes, trailing),
    LineBlock::Quote => quote_lines(nodes, trailing),
  }
}

fn code_lines(nodes: &[DomNode], trailing: Trailing) -> Vec<Line> {
  let mut code = String::new();
  editor_doc::code_text(nodes, &mut code);
  if nodes.is_empty() {
    return Vec::new();
  }
  let code = match trailing {
    Trailing::Placeholder => code.strip_suffix('\n').unwrap_or(&code),
    Trailing::Line => &code,
  };
  code
    .split('\n')
    .map(|line| match line {
      "" => Line::default(),
      text => Line(vec![DomNode::Text(text.to_owned())]),
    })
    .collect()
}

fn quote_lines(nodes: &[DomNode], trailing: Trailing) -> Vec<Line> {
  let mut lines = Vec::new();
  let mut current: Vec<DomNode> = Vec::new();
  // Whether `current` is a line even when empty: something ended the line before it.
  let mut is_open = false;
  for node in nodes {
    match node {
      DomNode::Element(element) if element.tag == "br" => {
        lines.push(Line(std::mem::take(&mut current)));
        is_open = true;
      },
      DomNode::Element(element) if LINE_TAGS.contains(&element.tag.as_str()) => {
        if !current.is_empty() {
          lines.push(Line(std::mem::take(&mut current)));
        }
        is_open = false;
        lines.push(match element.tag.as_str() {
          "p" | "div" => Line(element.children.clone()),
          _ => Line(vec![node.clone()]),
        });
      },
      DomNode::Text(text) if text.contains('\n') => {
        for (index, part) in text.split('\n').enumerate() {
          if index > 0 {
            lines.push(Line(std::mem::take(&mut current)));
            is_open = true;
          }
          if !part.is_empty() {
            current.push(DomNode::Text(part.to_owned()));
          }
        }
      },
      other => current.push(other.clone()),
    }
  }
  if !current.is_empty() || (is_open && trailing == Trailing::Line) {
    lines.push(Line(current));
  }
  lines
}

/// Lines put back into a block of the same kind (and code language).
fn rebuilt(block: &DomElement, kind: LineBlock, lines: &[Line]) -> DomNode {
  let children = match kind {
    LineBlock::Code => {
      let texts: Vec<String> = lines
        .iter()
        .map(|line| {
          let mut text = String::new();
          editor_doc::code_text(&line.0, &mut text);
          text
        })
        .collect();
      let code = texts.join("\n");
      // A trailing newline doesn't show: an empty last line is held open by a second `<br>`.
      match code.strip_suffix('\n') {
        Some("") => vec![br(), br()],
        Some(head) => vec![DomNode::Text(head.to_owned()), br(), br()],
        None if code.is_empty() => vec![br()],
        None => vec![DomNode::Text(code)],
      }
    },
    LineBlock::Quote => lines.iter().map(paragraph).collect(),
  };
  DomNode::Element(DomElement {
    tag: block.tag.clone(),
    maybe_language: block.maybe_language.clone(),
    children,
    ..DomElement::default()
  })
}

/// A line as normal text: a `<p>`, or the heading or list it already is. An empty line holds a
/// `<br>` so the caret has somewhere to go.
fn paragraph(line: &Line) -> DomNode {
  match line.0.as_slice() {
    [DomNode::Element(element)] if LINE_TAGS.contains(&element.tag.as_str()) && element.tag != "br" => {
      line.0[0].clone()
    },
    nodes if nodes.iter().all(is_marker) => element("p", vec![br()]),
    nodes => element("p", nodes.to_vec()),
  }
}

fn is_marker(node: &DomNode) -> bool {
  matches!(node, DomNode::Text(text) if text.chars().all(|ch| CARET_MARKERS.contains(&ch)))
}

fn br() -> DomNode {
  element("br", Vec::new())
}

fn element(tag: &str, children: Vec<DomNode>) -> DomNode {
  DomNode::Element(DomElement {
    tag: tag.to_owned(),
    children,
    ..DomElement::default()
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  mod code {
    use super::*;

    #[test]
    fn lines_end_at_breaks_and_newlines() {
      let block = pre(vec![text("a\nb"), el("br", vec![]), text("c")]);
      assert_eq!(texts(LineBlock::Code, &block), ["a", "b", "c"]);
    }

    #[test]
    fn a_trailing_break_holds_the_last_line_and_starts_none() {
      assert_eq!(
        texts(LineBlock::Code, &pre(vec![text("a"), el("br", vec![])])),
        ["a"]
      );
      let open = pre(vec![text("a"), el("br", vec![]), el("br", vec![])]);
      assert_eq!(texts(LineBlock::Code, &open), ["a", ""]);
      assert_eq!(texts(LineBlock::Code, &pre(vec![el("br", vec![])])), [""]);
      assert_eq!(texts(LineBlock::Code, &pre(vec![])), [""]);
    }

    #[test]
    fn the_caret_line_counts_breaks_before_it() {
      assert_eq!(line_index(LineBlock::Code, &[]), 0);
      assert_eq!(line_index(LineBlock::Code, &[text("ab")]), 0);
      assert_eq!(line_index(LineBlock::Code, &[text("a"), el("br", vec![])]), 1);
      assert_eq!(line_index(LineBlock::Code, &[text("a\nb\n")]), 2);
    }

    #[test]
    fn the_last_line_leaves_the_block() {
      let block = pre(vec![text("one\ntwo\nthree")]);
      let out = unwrap(&element_of(&block), LineBlock::Code, 2, 2);
      assert_eq!(out.maybe_before, Some(pre(vec![text("one\ntwo")])));
      assert_eq!(out.lines, [el("p", vec![text("three")])]);
      assert_eq!(out.maybe_after, None);
    }

    #[test]
    fn a_middle_line_splits_the_block() {
      let block = pre(vec![text("one\ntwo\nthree")]);
      let out = unwrap(&element_of(&block), LineBlock::Code, 1, 1);
      assert_eq!(out.maybe_before, Some(pre(vec![text("one")])));
      assert_eq!(out.lines, [el("p", vec![text("two")])]);
      assert_eq!(out.maybe_after, Some(pre(vec![text("three")])));
    }

    #[test]
    fn every_line_selected_leaves_no_block() {
      let block = pre(vec![text("one\ntwo")]);
      let out = unwrap(&element_of(&block), LineBlock::Code, 0, 9);
      assert_eq!(out.maybe_before, None);
      assert_eq!(
        out.lines,
        [el("p", vec![text("one")]), el("p", vec![text("two")])]
      );
      assert_eq!(out.maybe_after, None);
    }

    #[test]
    fn an_empty_last_line_becomes_an_empty_paragraph() {
      let block = pre(vec![text("code"), el("br", vec![]), el("br", vec![])]);
      let out = unwrap(&element_of(&block), LineBlock::Code, 1, 1);
      assert_eq!(out.maybe_before, Some(pre(vec![text("code")])));
      assert_eq!(out.lines, [el("p", vec![el("br", vec![])])]);
    }

    #[test]
    fn kept_lines_keep_an_empty_last_line_and_the_language() {
      let mut block = element_of(&pre(vec![text("a\n\nb")]));
      block.maybe_language = Some("rust".to_owned());
      let out = unwrap(&block, LineBlock::Code, 2, 2);
      let DomNode::Element(before) = out.maybe_before.expect("lines before") else {
        panic!("not an element");
      };
      assert_eq!(before.maybe_language.as_deref(), Some("rust"));
      assert_eq!(before.children, [text("a"), el("br", vec![]), el("br", vec![])]);
    }

    #[test]
    fn spaces_are_kept() {
      let block = pre(vec![text("fn x() {\n    y();\n}")]);
      let out = unwrap(&element_of(&block), LineBlock::Code, 1, 1);
      assert_eq!(out.lines, [el("p", vec![text("    y();")])]);
    }
  }

  mod quote {
    use super::*;

    #[test]
    fn paragraphs_and_breaks_are_lines() {
      let block = quote(vec![
        el("p", vec![text("a")]),
        text("b"),
        el("br", vec![]),
        text("c"),
        el("br", vec![]),
      ]);
      assert_eq!(texts(LineBlock::Quote, &block), ["a", "b", "c"]);
    }

    #[test]
    fn the_caret_line_counts_lines_before_it() {
      let before = [el("p", vec![text("a")]), el("p", vec![])];
      assert_eq!(line_index(LineBlock::Quote, &before), 1);
      assert_eq!(line_index(LineBlock::Quote, &[text("a"), el("br", vec![])]), 1);
      assert_eq!(line_index(LineBlock::Quote, &[el("p", vec![text("a")])]), 0);
    }

    #[test]
    fn formatting_of_a_line_is_kept() {
      let bold = el("strong", vec![text("b")]);
      let block = quote(vec![
        el("p", vec![text("a")]),
        el("p", vec![bold.clone(), text("!")]),
      ]);
      let out = unwrap(&element_of(&block), LineBlock::Quote, 1, 1);
      assert_eq!(out.maybe_before, Some(quote(vec![el("p", vec![text("a")])])));
      assert_eq!(out.lines, [el("p", vec![bold, text("!")])]);
      assert_eq!(out.maybe_after, None);
    }

    #[test]
    fn a_list_inside_stays_a_list() {
      let list = el("ul", vec![el("li", vec![text("x")])]);
      let block = quote(vec![list.clone()]);
      let out = unwrap(&element_of(&block), LineBlock::Quote, 0, 0);
      assert_eq!(out.lines, [list]);
    }

    #[test]
    fn a_single_line_leaves_the_quote() {
      let block = quote(vec![el("p", vec![text("only")])]);
      let out = unwrap(&element_of(&block), LineBlock::Quote, 0, 0);
      assert_eq!(out.maybe_before, None);
      assert_eq!(out.lines, [el("p", vec![text("only")])]);
      assert_eq!(out.maybe_after, None);
    }
  }

  mod position {
    use super::*;

    #[test]
    fn the_start_has_nothing_before_it() {
      assert!(is_at_start(LineBlock::Code, &[]));
      assert!(is_at_start(LineBlock::Code, &[text("\u{200B}")]));
      assert!(is_at_start(LineBlock::Quote, &[el("p", vec![])]));
      assert!(!is_at_start(LineBlock::Code, &[text("a")]));
      assert!(!is_at_start(LineBlock::Code, &[el("br", vec![])]));
      assert!(!is_at_start(
        LineBlock::Quote,
        &[el("p", vec![]), el("p", vec![])]
      ));
    }

    #[test]
    fn the_column_is_the_line_text_before_the_position() {
      assert_eq!(column(LineBlock::Code, &[]), 0);
      assert_eq!(column(LineBlock::Code, &[text("ab\ncd")]), 2);
      assert_eq!(column(LineBlock::Code, &[text("ab"), el("br", vec![])]), 0);
      assert_eq!(column(LineBlock::Code, &[text("\u{200B}é")]), 1);
      assert_eq!(
        column(
          LineBlock::Quote,
          &[el("p", vec![text("a")]), el("p", vec![text("bc")])]
        ),
        2
      );
    }

    #[test]
    fn blank_lines_are_found() {
      assert!(Line::default().is_blank());
      assert!(Line(vec![text("\u{200B} ")]).is_blank());
      assert!(Line(vec![el("br", vec![])]).is_blank());
      assert!(!Line(vec![el("em", vec![text("x")])]).is_blank());
    }

    #[test]
    fn only_code_and_quote_have_lines() {
      assert_eq!(LineBlock::of("pre"), Some(LineBlock::Code));
      assert_eq!(LineBlock::of("blockquote"), Some(LineBlock::Quote));
      assert_eq!(LineBlock::of("p"), None);
    }
  }

  fn texts(kind: LineBlock, block: &DomNode) -> Vec<String> {
    lines(kind, &element_of(block).children)
      .iter()
      .map(|line| {
        let mut out = String::new();
        editor_doc::code_text(&line.0, &mut out);
        out
      })
      .collect()
  }

  fn element_of(node: &DomNode) -> DomElement {
    match node {
      DomNode::Element(element) => element.clone(),
      DomNode::Text(_) => DomElement::default(),
    }
  }

  fn pre(children: Vec<DomNode>) -> DomNode {
    el("pre", children)
  }

  fn quote(children: Vec<DomNode>) -> DomNode {
    el("blockquote", children)
  }

  fn text(value: &str) -> DomNode {
    DomNode::Text(value.to_owned())
  }

  fn el(tag: &str, children: Vec<DomNode>) -> DomNode {
    element(tag, children)
  }
}
