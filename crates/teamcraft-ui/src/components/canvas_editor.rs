//! DOM side of the shared notes' blocks: each is a `contenteditable` element that shows its text
//! with the formatting of the page (issue #1456).
//!
//! The Yjs document decides what a block says and how it looks; the element only shows it. The
//! browser edits text itself (so typing and input methods work as anywhere), the view reads the
//! element's text after each input and writes it as an edit, and [`show_block`] brings the element
//! back whenever it differs from the document: what the browser wrapped in its own tags, a change of
//! another member, a change of formatting. Elements are built here from stretches of text, never
//! from HTML.

use teamcraft_client_api::canvas::{Block, TextStyle, shift_caret};
use wasm_bindgen::JsCast;
use web_sys::{Document, Element, HtmlElement, Node};

/// `NodeFilter.SHOW_TEXT`.
const SHOW_TEXT: u32 = 0x4;
/// Deeper elements are not read: a browser's own tags don't nest this far.
const MAX_READ_DEPTH: usize = 128;
/// A link in an editable block is not an anchor: a click must place the caret, and the shell opens
/// every click on an anchor (`external_links`).
const EDITABLE_LINK: &str = "canvas-link";
const EDITABLE_LINK_ADDRESS: &str = "data-href";

/// A selection in a block, in UTF-16 units of its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caret {
  pub start: u32,
  pub end: u32,
}

/// A stretch of text with its formatting.
type Run = (String, TextStyle);

/// Brings the element to the block; the selection stays with its text. While an input method is
/// composing only a change of the text does, so a composition isn't interrupted over formatting.
pub fn show_block(element: &HtmlElement, block: &Block, is_editable: bool, is_composing: bool) {
  let shown = read_runs(element, is_editable);
  let wanted = wanted_runs(block);
  if shown == wanted {
    return;
  }
  let shown_text = text_of_runs(&shown);
  if is_composing && shown_text == block.text {
    return;
  }
  let maybe_caret = selection_in(element);
  render(element, &wanted, is_editable);
  if let Some(caret) = maybe_caret {
    set_selection(
      element,
      Caret {
        start: shift_caret(&shown_text, &block.text, caret.start),
        end: shift_caret(&shown_text, &block.text, caret.end),
      },
    );
  }
}

/// The element's text as the user sees it.
pub fn text_of(element: &HtmlElement) -> String {
  text_of_runs(&read_runs(element, true))
}

/// The selection inside the element, if it is there.
pub fn selection_in(element: &HtmlElement) -> Option<Caret> {
  let selection = web_sys::window()?.get_selection().ok()??;
  if selection.range_count() == 0 {
    return None;
  }
  let range = selection.get_range_at(0).ok()?;
  let start = range.start_container().ok()?;
  let end = range.end_container().ok()?;
  if !element.contains(Some(&start)) || !element.contains(Some(&end)) {
    return None;
  }
  Some(Caret {
    start: units_before(element, &start, range.start_offset().ok()?)?,
    end: units_before(element, &end, range.end_offset().ok()?)?,
  })
}

/// Puts the selection into the element.
pub fn set_selection(element: &HtmlElement, caret: Caret) {
  let Some(selection) = web_sys::window().and_then(|window| window.get_selection().ok().flatten()) else {
    return;
  };
  let (anchor, anchor_offset) = point_at(element, caret.start);
  let (focus, focus_offset) = point_at(element, caret.end);
  drop(selection.set_base_and_extent(&anchor, anchor_offset, &focus, focus_offset));
}

/// The address of the link in an editable block under `target`.
pub fn link_under(target: &Element) -> Option<String> {
  target
    .closest(&format!(".{EDITABLE_LINK}"))
    .ok()
    .flatten()?
    .get_attribute(EDITABLE_LINK_ADDRESS)
}

/// What the block looks like: code shows its text as it is.
fn wanted_runs(block: &Block) -> Vec<Run> {
  if block.kind.is_code() {
    return if block.text.is_empty() {
      Vec::new()
    } else {
      vec![(block.text.clone(), TextStyle::default())]
    };
  }
  block
    .runs()
    .into_iter()
    .filter(|(text, _)| !text.is_empty())
    .map(|(text, maybe_style)| (text.to_owned(), maybe_style.cloned().unwrap_or_default()))
    .collect()
}

fn text_of_runs(runs: &[Run]) -> String {
  runs.iter().map(|(text, _)| text.as_str()).collect()
}

/// The element's content as stretches of text; tags the editor doesn't make count as nothing, so
/// [`show_block`] replaces them.
fn read_runs(element: &HtmlElement, is_editable: bool) -> Vec<Run> {
  let mut runs = Vec::new();
  read_children(element, &TextStyle::default(), is_editable, 0, element, &mut runs);
  runs
}

fn read_children(
  parent: &Node,
  style: &TextStyle,
  is_editable: bool,
  depth: usize,
  root: &HtmlElement,
  runs: &mut Vec<Run>,
) {
  if depth > MAX_READ_DEPTH {
    return;
  }
  let mut maybe_child = parent.first_child();
  while let Some(child) = maybe_child {
    match child.node_type() {
      Node::TEXT_NODE => push_text(runs, &child.text_content().unwrap_or_default(), style),
      Node::ELEMENT_NODE => {
        if let Some(element) = child.dyn_ref::<Element>() {
          let tag = element.tag_name().to_ascii_lowercase();
          if tag == "br" {
            if !is_last_in(&child, root) {
              push_text(runs, "\n", style);
            }
          } else {
            let inner = style_inside(element, &tag, style, is_editable);
            read_children(&child, &inner, is_editable, depth + 1, root, runs);
          }
        }
      },
      _ => {},
    }
    maybe_child = child.next_sibling();
  }
}

/// A `<br>` closing the element is the browser's placeholder for its last line, not a line break.
fn is_last_in(node: &Node, root: &HtmlElement) -> bool {
  let root: &Node = root;
  let mut current = node.clone();
  loop {
    if current.next_sibling().is_some() {
      return false;
    }
    match current.parent_node() {
      Some(parent) if parent == *root => return true,
      Some(parent) => current = parent,
      None => return true,
    }
  }
}

fn style_inside(element: &Element, tag: &str, style: &TextStyle, is_editable: bool) -> TextStyle {
  let mut inner = style.clone();
  match tag {
    "strong" | "b" => inner.is_bold = true,
    "em" | "i" => inner.is_italic = true,
    "s" | "strike" | "del" => inner.is_strike = true,
    "code" => inner.is_code = true,
    "a" if !is_editable => inner.maybe_link = element.get_attribute("href"),
    "span" if is_editable && element.class_list().contains(EDITABLE_LINK) => {
      inner.maybe_link = element.get_attribute(EDITABLE_LINK_ADDRESS);
    },
    _ => {},
  }
  inner
}

fn push_text(runs: &mut Vec<Run>, text: &str, style: &TextStyle) {
  if text.is_empty() {
    return;
  }
  // A browser may keep a space at a line's end as a no-break one.
  let text = text.replace('\u{a0}', " ");
  match runs.last_mut() {
    Some((last, last_style)) if last_style == style => last.push_str(&text),
    _ => runs.push((text, style.clone())),
  }
}

fn render(element: &HtmlElement, runs: &[Run], is_editable: bool) {
  element.set_text_content(None);
  let Some(document) = element.owner_document() else {
    return;
  };
  for (text, style) in runs {
    if let Some(node) = styled_node(&document, text, style, is_editable) {
      drop(element.append_child(&node));
    }
  }
  // Text ending in a line break needs an element after it for its last line to show.
  if runs.last().is_some_and(|(text, _)| text.ends_with('\n'))
    && let Ok(placeholder) = document.create_element("br")
  {
    drop(element.append_child(&placeholder));
  }
}

fn styled_node(document: &Document, text: &str, style: &TextStyle, is_editable: bool) -> Option<Node> {
  let mut node: Node = document.create_text_node(text).into();
  if style.is_code {
    let code = wrap(document, "code", &node)?;
    drop(code.class_list().add_1("md-inline-code"));
    node = code.into();
  }
  for (is_on, tag) in [
    (style.is_strike, "s"),
    (style.is_italic, "em"),
    (style.is_bold, "strong"),
  ] {
    if is_on {
      node = wrap(document, tag, &node)?.into();
    }
  }
  if let Some(url) = &style.maybe_link {
    let link = if is_editable {
      let link = wrap(document, "span", &node)?;
      link.set_attribute(EDITABLE_LINK_ADDRESS, url).ok()?;
      drop(link.class_list().add_2("md-link", EDITABLE_LINK));
      link
    } else {
      let link = wrap(document, "a", &node)?;
      link.set_attribute("href", url).ok()?;
      link.set_attribute("target", "_blank").ok()?;
      link.set_attribute("rel", "noopener noreferrer").ok()?;
      drop(link.class_list().add_1("md-link"));
      link
    };
    // The tooltip shows where it goes, so the text can't disguise the target.
    link.set_attribute("title", url).ok()?;
    node = link.into();
  }
  Some(node)
}

fn wrap(document: &Document, tag: &str, node: &Node) -> Option<Element> {
  let element = document.create_element(tag).ok()?;
  element.append_child(node).ok()?;
  Some(element)
}

/// The UTF-16 units of the element's text before a point in it.
fn units_before(element: &HtmlElement, node: &Node, offset: u32) -> Option<u32> {
  let range = element.owner_document()?.create_range().ok()?;
  range.select_node_contents(element).ok()?;
  range.set_end(node, offset).ok()?;
  u32::try_from(String::from(range.to_string()).encode_utf16().count()).ok()
}

/// The text position `units` into the element: the end of a stretch rather than the start of the
/// next, as typing there continues the stretch before it.
fn point_at(element: &HtmlElement, units: u32) -> (Node, u32) {
  let end = (element.clone().into(), element.child_nodes().length());
  let Some(document) = element.owner_document() else {
    return end;
  };
  let Ok(walker) = document.create_tree_walker_with_what_to_show(element, SHOW_TEXT) else {
    return end;
  };
  let mut counted = 0_u32;
  while let Ok(Some(node)) = walker.next_node() {
    let length = node
      .dyn_ref::<web_sys::CharacterData>()
      .map_or(0, web_sys::CharacterData::length);
    if units <= counted + length {
      return (node, units - counted);
    }
    counted += length;
  }
  end
}
