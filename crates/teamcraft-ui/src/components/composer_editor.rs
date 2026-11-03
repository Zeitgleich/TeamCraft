//! DOM side of the composer's rich-text editor: a `contenteditable` element that shows formatting as
//! it will look in the message and stores it as the Markdown subset.
//!
//! Formatting goes through the browser's editing commands where it has one, so undo and redo keep
//! working; the rest inserts HTML built by [`editor_doc`] from the subset, escaped. Reading the
//! content back always goes through [`editor_doc::to_blocks`], the allow-list walk, so whatever the
//! browser put into the element, only the subset reaches a message. Per keystroke the editor looks at
//! the text node at the caret only; the whole content is read when it is sent or the draft is saved.

use wasm_bindgen::JsCast;
use web_sys::{Document, Element, HtmlDocument, HtmlElement, Node, Range, Selection};

use crate::editor_doc::{self, BlockFormat, DomElement, DomNode, InlineShortcut, InlineStyle, Whitespace};
use crate::editor_lines::{self, LineBlock};
use crate::markdown;

/// Placed after inserted formatting so the caret leaves it; [`editor_doc`] drops it when reading.
const CARET_ESCAPE: &str = "\u{200B}";

/// How much text before the caret the typing helpers look at (a mention or Markdown span is shorter).
const CARET_WINDOW: usize = 256;

/// Deeper elements are read as their text only; the walk recurses, and pasted HTML can nest deeply.
const MAX_READ_DEPTH: usize = 128;

/// `NodeFilter.SHOW_TEXT`.
const SHOW_TEXT: u32 = 0x4;

/// How much of a text node the empty check reads before it reads the rest (only white space so far).
const BLANK_PROBE: u32 = 64;

/// Block elements the editor makes, from the innermost one at the caret outwards.
const BLOCK_SELECTOR: &str = "h1, h2, h3, h4, h5, h6, blockquote, li, pre, p, div";

/// Blocks whose lines the editor takes out itself (`editor_lines`); `formatBlock` can't.
const LINE_BLOCK_SELECTOR: &str = "pre, blockquote";

/// The editor's content as Markdown.
pub fn markdown(editor: &HtmlElement) -> String {
  let nodes = read_children(editor);
  markdown::to_source(&editor_doc::to_blocks(&nodes, Whitespace::Preserve))
}

/// Replaces the editor's content with `source` (a restored draft).
pub fn set_markdown(editor: &HtmlElement, source: &str) {
  clear(editor);
  let Some(document) = editor.owner_document() else {
    return;
  };
  for node in editor_doc::from_blocks(&markdown::parse(source)) {
    if let Some(built) = build(&document, &node) {
      drop(editor.append_child(&built));
    }
  }
}

pub fn clear(editor: &HtmlElement) {
  editor.set_text_content(None);
}

/// Whether the editor shows nothing but white space. Stops at the first text with something in it and
/// looks at its start only, so a keystroke doesn't read the draft.
pub fn is_blank(editor: &HtmlElement) -> bool {
  let Some(document) = editor.owner_document() else {
    return true;
  };
  let Ok(walker) = document.create_tree_walker_with_what_to_show(editor, SHOW_TEXT) else {
    return true;
  };
  while let Ok(Some(node)) = walker.next_node() {
    let Some(text) = node.dyn_ref::<web_sys::Text>() else {
      continue;
    };
    let length = text.length();
    let head = text
      .substring_data(0, length.min(BLANK_PROBE))
      .unwrap_or_default();
    if !is_blank_text(&head) || (length > BLANK_PROBE && !is_blank_text(&text.data())) {
      return false;
    }
  }
  true
}

fn is_blank_text(text: &str) -> bool {
  text.chars().all(|ch| ch.is_whitespace() || ch == '\u{200B}')
}

/// Content of a paste, as the subset. `html` is parsed into an inert document (no scripts run, nothing
/// loads) and read through the allow-list walk; plain text stays literal.
pub fn pasted_blocks(maybe_html: Option<&str>, plain: &str) -> Vec<markdown::Block> {
  if let Some(html) = maybe_html.filter(|html| !html.trim().is_empty())
    && let Ok(parser) = web_sys::DomParser::new()
    && let Ok(document) = parser.parse_from_string(html, web_sys::SupportedType::TextHtml)
    && let Some(body) = document.body()
  {
    return editor_doc::to_blocks(&read_children(&body), Whitespace::Collapse);
  }
  let lines: Vec<Vec<markdown::Inline>> = plain
    .lines()
    .map(|line| vec![markdown::Inline::Text(line.to_owned())])
    .collect();
  if lines.is_empty() {
    Vec::new()
  } else {
    vec![markdown::Block::Paragraph(lines)]
  }
}

/// Inserts blocks at the caret. A single line goes into the current line instead of starting a block.
pub fn insert_blocks(blocks: &[markdown::Block]) {
  let html = match blocks {
    [markdown::Block::Paragraph(lines)] if lines.len() == 1 => {
      let nodes = editor_doc::from_blocks(blocks);
      match nodes.first() {
        Some(DomNode::Element(paragraph)) => editor_doc::to_html(&paragraph.children),
        _ => String::new(),
      }
    },
    blocks => editor_doc::to_html(&editor_doc::from_blocks(blocks)),
  };
  if !html.is_empty() {
    command("insertHTML", &html);
  }
}

/// Inserts text at the caret, replacing the selection.
pub fn insert_text(text: &str) {
  command("insertText", text);
}

/// One-time setup: Enter makes `<p>` lines, as the editor's own content does.
pub fn prepare() {
  command("defaultParagraphSeparator", "p");
  command("styleWithCSS", "false");
}

/// Turns inline formatting on or off for the selection.
pub fn toggle_inline(editor: &HtmlElement, style: InlineStyle) {
  match style {
    InlineStyle::Bold => command("bold", ""),
    InlineStyle::Italic => command("italic", ""),
    InlineStyle::Underline => command("underline", ""),
    InlineStyle::Strike => command("strikeThrough", ""),
    InlineStyle::Code => toggle_code(editor),
  }
}

/// Whether the caret or selection is in that formatting (for the toolbar's pressed state).
pub fn is_inline_active(editor: &HtmlElement, style: InlineStyle) -> bool {
  let name = match style {
    InlineStyle::Bold => "bold",
    InlineStyle::Italic => "italic",
    InlineStyle::Underline => "underline",
    InlineStyle::Strike => "strikeThrough",
    InlineStyle::Code => return caret_closest(editor, "code").is_some(),
  };
  html_document().is_some_and(|document| document.query_command_state(name).unwrap_or(false))
}

/// The block format at the caret.
pub fn block_format(editor: &HtmlElement) -> BlockFormat {
  let Some(block) = caret_closest(editor, BLOCK_SELECTOR) else {
    return BlockFormat::Paragraph;
  };
  match block.tag_name().to_ascii_lowercase().as_str() {
    "h1" => BlockFormat::Heading(1),
    "h2" => BlockFormat::Heading(2),
    "h3" | "h4" | "h5" | "h6" => BlockFormat::Heading(3),
    "pre" => BlockFormat::CodeBlock,
    "li" => match block
      .parent_element()
      .map(|list| list.tag_name().to_ascii_lowercase())
    {
      Some(tag) if tag == "ol" => BlockFormat::Numbered,
      _ => BlockFormat::Bulleted,
    },
    _ if block
      .closest("blockquote")
      .ok()
      .flatten()
      .is_some_and(|quote| editor.contains(Some(&quote))) =>
    {
      BlockFormat::Quote
    },
    _ => BlockFormat::Paragraph,
  }
}

/// Applies a block format to the lines at the caret; applying the current one turns it off.
pub fn set_block_format(editor: &HtmlElement, format: BlockFormat) {
  let current = block_format(editor);
  let target = if current == format {
    BlockFormat::Paragraph
  } else {
    format
  };
  // #843: `formatBlock` leaves a code block or quote as it is; the selected lines leave it here.
  if matches!(current, BlockFormat::CodeBlock | BlockFormat::Quote)
    && target != current
    && leave_line_blocks(editor)
    && target == BlockFormat::Paragraph
  {
    drop(editor.focus());
    return;
  }
  // Lists toggle themselves; leaving a list first keeps a list from nesting inside a quote.
  match current {
    BlockFormat::Bulleted if target != BlockFormat::Bulleted => command("insertUnorderedList", ""),
    BlockFormat::Numbered if target != BlockFormat::Numbered => command("insertOrderedList", ""),
    _ => {},
  }
  match target {
    BlockFormat::Paragraph => command("formatBlock", "p"),
    BlockFormat::Heading(level) => command("formatBlock", &format!("h{level}")),
    BlockFormat::Quote => command("formatBlock", "blockquote"),
    BlockFormat::CodeBlock => command("formatBlock", "pre"),
    BlockFormat::Bulleted => command("insertUnorderedList", ""),
    BlockFormat::Numbered => command("insertOrderedList", ""),
  }
  // `formatBlock` may wrap the editor's top-level text in a `<div>`; it reads the same.
  drop(editor.focus());
}

/// A line break that continues the current block: a new list item or quote line, or a new line in a
/// code block. An empty quote line, the empty last line of a code block or an empty list item ends
/// the quote, code block or list instead (as in Slack and Notion).
pub fn new_line(editor: &HtmlElement) {
  if leave_on_blank_line(editor) {
    return;
  }
  match block_format(editor) {
    BlockFormat::CodeBlock => command("insertLineBreak", ""),
    _ => {
      command("insertParagraph", "");
      leave_empty_inline_code(editor);
    },
  }
}

/// Backspace at the very start of a code block or quote turns its first line into normal text, as
/// in Slack and Notion. Returns whether it did.
pub fn leave_block_at_start(editor: &HtmlElement) -> bool {
  let Some((block, kind, before)) = caret_line_block(editor) else {
    return false;
  };
  if !editor_lines::is_at_start(kind, &before) {
    return false;
  }
  let Some(converted) = replace_lines(&block, kind, 0, 0) else {
    return false;
  };
  if let Some(first) = converted.first() {
    place_caret(first, 0);
  }
  true
}

/// Leaves a code block or quote at the very end of the editor with ↓, so there is a line to type on
/// below it. Returns whether it did.
pub fn leave_last_block(editor: &HtmlElement) -> bool {
  let format = block_format(editor);
  if !matches!(format, BlockFormat::CodeBlock | BlockFormat::Quote) {
    return false;
  }
  let Some(block) = caret_closest(editor, "pre, blockquote") else {
    return false;
  };
  let is_last = editor.last_element_child().is_some_and(|last| last == block);
  if !is_last || !caret_at_end_of(&block) {
    return false;
  }
  let Some(document) = editor.owner_document() else {
    return false;
  };
  let Ok(line) = document.create_element("p") else {
    return false;
  };
  if let Ok(placeholder) = document.create_element("br") {
    drop(line.append_child(&placeholder));
  }
  drop(editor.append_child(&line));
  place_caret(&line, 0);
  true
}

/// Links the selection to `url`, or inserts `label` linked to it where nothing is selected.
pub fn insert_link(url: &str, label: &str) {
  if !markdown::is_allowed_link(url) {
    return;
  }
  let has_selection = selection().is_some_and(|selection| !selection.is_collapsed());
  if has_selection && label.is_empty() {
    command("createLink", url);
    collapse_to_end();
    return;
  }
  let label = if label.trim().is_empty() { url } else { label };
  let node = DomNode::Element(DomElement {
    tag: "a".to_owned(),
    maybe_href: Some(url.to_owned()),
    children: vec![DomNode::Text(label.to_owned())],
    ..DomElement::default()
  });
  command(
    "insertHTML",
    &format!("{}{CARET_ESCAPE}", editor_doc::to_html(&[node])),
  );
}

/// The selected text, for the link dialog's label.
pub fn selected_text() -> String {
  selection()
    .map(|selection| String::from(selection.to_string()))
    .unwrap_or_default()
}

/// The selection, kept while a dialog or picker has the focus.
pub fn save_selection(editor: &HtmlElement) -> Option<Range> {
  let selection = selection()?;
  let range = (selection.range_count() > 0)
    .then(|| selection.get_range_at(0).ok())
    .flatten()?;
  editor
    .contains(Some(&range.common_ancestor_container().ok()?))
    .then(|| range.clone_range())
}

/// Puts a saved selection back (or the caret at the end) and focuses the editor.
pub fn restore_selection(editor: &HtmlElement, maybe_range: Option<&Range>) {
  drop(editor.focus());
  let Some(selection) = selection() else { return };
  match maybe_range {
    Some(range) => {
      drop(selection.remove_all_ranges());
      drop(selection.add_range(range));
    },
    None => place_caret_at_end(editor),
  }
}

/// Puts the caret at the end of the selection, so what comes next is added, not replacing it.
pub fn collapse_to_end() {
  if let Some(selection) = selection() {
    drop(selection.collapse_to_end());
  }
}

pub fn place_caret_at_end(editor: &HtmlElement) {
  let Some(selection) = selection() else { return };
  drop(selection.select_all_children(editor));
  drop(selection.collapse_to_end());
}

/// The text before the caret in the caret's text node, at most [`CARET_WINDOW`] UTF-16 units of it,
/// so a keystroke in a long paragraph doesn't copy the paragraph.
pub struct CaretText {
  pub node: web_sys::Text,
  /// UTF-16 offset of the caret in `node`.
  pub offset: usize,
  /// UTF-16 offset in `node` where `before` starts.
  pub window_start: usize,
  pub before: String,
}

/// The text at the caret, when the selection is collapsed inside the editor.
pub fn caret_text(editor: &HtmlElement) -> Option<CaretText> {
  let selection = selection()?;
  if !selection.is_collapsed() {
    return None;
  }
  let node = selection.anchor_node()?;
  if !editor.contains(Some(&node)) {
    return None;
  }
  let node = node.dyn_into::<web_sys::Text>().ok()?;
  let offset = selection.anchor_offset() as usize;
  let window_start = offset.saturating_sub(CARET_WINDOW);
  let before = node
    .substring_data(window_start as u32, (offset - window_start) as u32)
    .ok()?;
  Some(CaretText {
    node,
    offset,
    window_start,
    before,
  })
}

/// Whether `text` is where the editor's content starts (a `/command` may only be typed there).
pub fn is_first_text(editor: &HtmlElement, text: &web_sys::Text) -> bool {
  let mut maybe_node = editor.first_child();
  while let Some(node) = maybe_node {
    if node.node_type() == Node::TEXT_NODE {
      return node == ***text;
    }
    maybe_node = node.first_child();
  }
  false
}

/// Replaces `start..end` (UTF-16 offsets) of `text` with `insert` as one undoable edit.
pub fn replace_in_text(text: &web_sys::Text, start: usize, end: usize, insert: &str) {
  if select_in_text(text, start, end) {
    insert_text(insert);
  }
}

/// Typing shortcuts, checked after each typed character: a Markdown span whose closing marker was just
/// typed becomes formatting, and `- `, `1. `, `> `, `# ` or ```` ``` ```` at the start of a line
/// turns the line into a list, quote, heading or code block. Looks at the caret's text node only.
pub fn apply_typing_shortcut(editor: &HtmlElement, typed: &str) -> bool {
  let Some(caret) = caret_text(editor) else {
    return false;
  };
  if block_format(editor) == BlockFormat::CodeBlock || caret_closest(editor, "code").is_some() {
    return false;
  }
  let CaretText {
    node,
    offset,
    window_start,
    before,
  } = caret;
  match typed {
    " " | "\u{A0}" if window_start == 0 => block_shortcut(editor, &node, &before),
    "`" if window_start == 0 && before == "```" && is_line_start(editor, &node) => {
      if select_in_text(&node, 0, offset) {
        command("delete", "");
        set_block_format(editor, BlockFormat::CodeBlock);
      }
      true
    },
    "*" | "_" | "~" | "`" => match editor_doc::inline_shortcut(&before) {
      Some(found) => {
        apply_inline_shortcut(&node, window_start + found.start, offset, &found);
        true
      },
      None => false,
    },
    _ => false,
  }
}

fn block_shortcut(editor: &HtmlElement, text: &web_sys::Text, before: &str) -> bool {
  let Some(marker) = before.strip_suffix([' ', '\u{A0}']) else {
    return false;
  };
  let Some(format) = editor_doc::block_shortcut(marker) else {
    return false;
  };
  if !is_line_start(editor, text) || block_format(editor) != BlockFormat::Paragraph {
    return false;
  }
  if select_in_text(text, 0, before.encode_utf16().count()) {
    command("delete", "");
    set_block_format(editor, format);
  }
  true
}

fn apply_inline_shortcut(text: &web_sys::Text, start: usize, offset: usize, found: &InlineShortcut) {
  if !select_in_text(text, start, offset) {
    return;
  }
  let inner = vec![DomNode::Text(found.inner.clone())];
  let tag = match found.style {
    InlineStyle::Bold => "strong",
    InlineStyle::Italic => "em",
    InlineStyle::Underline => "u",
    InlineStyle::Strike => "s",
    InlineStyle::Code => "code",
  };
  let node = DomNode::Element(DomElement {
    tag: tag.to_owned(),
    children: inner,
    ..DomElement::default()
  });
  command(
    "insertHTML",
    &format!("{}{CARET_ESCAPE}", editor_doc::to_html(&[node])),
  );
}

/// Whether `text` starts its line: nothing comes before it in its block.
fn is_line_start(editor: &HtmlElement, text: &web_sys::Text) -> bool {
  let mut node: Node = text.clone().into();
  loop {
    if node.previous_sibling().is_some() {
      return false;
    }
    let Some(parent) = node.parent_node() else {
      return false;
    };
    if parent == ***editor {
      return true;
    }
    if let Some(element) = parent.dyn_ref::<Element>()
      && element.matches(BLOCK_SELECTOR).unwrap_or(false)
    {
      return true;
    }
    node = parent;
  }
}

/// Wraps the selection in `<code>`, or unwraps the code at the caret.
fn toggle_code(editor: &HtmlElement) {
  if let Some(code) = caret_closest(editor, "code") {
    if let Some(range) = editor
      .owner_document()
      .and_then(|document| document.create_range().ok())
      && range.select_node(&code).is_ok()
      && let Some(selection) = selection()
    {
      drop(selection.remove_all_ranges());
      drop(selection.add_range(&range));
      let text = code.text_content().unwrap_or_default().replace(CARET_ESCAPE, "");
      insert_text(&text);
    }
    return;
  }
  let selected = selected_text();
  if selected.is_empty() {
    start_empty_code(editor);
    return;
  }
  let node = DomNode::Element(DomElement {
    tag: "code".to_owned(),
    children: vec![DomNode::Text(selected)],
    ..DomElement::default()
  });
  command(
    "insertHTML",
    &format!("{}{CARET_ESCAPE}", editor_doc::to_html(&[node])),
  );
}

/// An empty `<code>` at the caret with the caret inside it, so what is typed next is code.
fn start_empty_code(editor: &HtmlElement) {
  let (Some(document), Some(selection)) = (editor.owner_document(), selection()) else {
    return;
  };
  let Ok(range) = selection.get_range_at(0) else {
    return;
  };
  let Ok(code) = document.create_element("code") else {
    return;
  };
  let inside = document.create_text_node(CARET_ESCAPE);
  drop(code.append_child(&inside));
  if range.insert_node(&code).is_ok() {
    place_caret(&inside, 1);
  }
}

/// Turns the selected lines of every code block and quote the selection touches into paragraphs;
/// the lines around them stay in their block. Keeps the caret's place in its line, or selects the
/// lines that left. Returns whether there was a block to leave.
fn leave_line_blocks(editor: &HtmlElement) -> bool {
  let Some(range) = selection().and_then(|selection| selection.get_range_at(0).ok()) else {
    return false;
  };
  let is_collapsed = range.collapsed();
  let (Ok(start), Ok(start_offset), Ok(end), Ok(end_offset)) = (
    range.start_container(),
    range.start_offset(),
    range.end_container(),
    range.end_offset(),
  ) else {
    return false;
  };
  let blocks = selected_line_blocks(editor, &range);
  let mut maybe_first: Option<(Node, usize)> = None;
  let mut maybe_last: Option<Node> = None;
  // From the last block up, so a replaced block doesn't move the positions in the ones before it.
  for (block, kind) in blocks.iter().rev() {
    let (first, column) = match block.contains(Some(&start)) {
      true => content_before(block, &start, start_offset)
        .map(|before| {
          (
            editor_lines::line_index(*kind, &before),
            editor_lines::column(*kind, &before),
          )
        })
        .unwrap_or_default(),
      false => (0, 0),
    };
    let last = match (is_collapsed, block.contains(Some(&end))) {
      (true, _) => first,
      (false, true) => content_before(block, &end, end_offset).map_or(first, |before| {
        let index = editor_lines::line_index(*kind, &before);
        // A selection that ends at the start of a line (a triple click) doesn't take that line.
        if index > first && editor_lines::column(*kind, &before) == 0 {
          index - 1
        } else {
          index
        }
      }),
      (false, false) => usize::MAX,
    };
    let Some(converted) = replace_lines(block, *kind, first, last) else {
      continue;
    };
    if maybe_last.is_none() {
      maybe_last = converted.last().cloned();
    }
    maybe_first = converted.first().cloned().map(|node| (node, column));
  }
  match (maybe_first, maybe_last) {
    (Some((first, column)), _) if is_collapsed => place_caret_at_column(&first, column),
    (Some((first, _)), Some(last)) => select_between(&first, &last),
    _ => return false,
  }
  true
}

/// The browser carries inline code at the end of a line into the new line (#843): every following
/// line was code. The empty code element on the new line is unwrapped, so the line starts as text.
fn leave_empty_inline_code(editor: &HtmlElement) {
  let Some(code) = caret_closest(editor, "code") else {
    return;
  };
  let is_empty = code
    .text_content()
    .is_none_or(|text| text.replace(CARET_ESCAPE, "").is_empty());
  let Some(parent) = code.parent_node().filter(|_| is_empty) else {
    return;
  };
  let offset = index_in_parent(&code);
  let children = code.child_nodes();
  let moved: Vec<Node> = (0..children.length())
    .filter_map(|index| children.item(index))
    .collect();
  for child in &moved {
    drop(parent.insert_before(child, Some(&code)));
  }
  code.remove();
  place_caret(&parent, offset);
}

fn index_in_parent(node: &Node) -> u32 {
  let mut index = 0;
  let mut maybe_previous = node.previous_sibling();
  while let Some(previous) = maybe_previous {
    index += 1;
    maybe_previous = previous.previous_sibling();
  }
  index
}

/// The outermost code blocks and quotes the range touches, in document order.
fn selected_line_blocks(editor: &HtmlElement, range: &Range) -> Vec<(Element, LineBlock)> {
  let Ok(list) = editor.query_selector_all(LINE_BLOCK_SELECTOR) else {
    return Vec::new();
  };
  (0..list.length())
    .filter_map(|index| list.item(index))
    .filter(|node| range.intersects_node(node).unwrap_or(false))
    .filter_map(|node| node.dyn_into::<Element>().ok())
    .filter(|block| {
      block
        .parent_element()
        .and_then(|parent| parent.closest(LINE_BLOCK_SELECTOR).ok().flatten())
        .is_none_or(|outer| !editor.contains(Some(&outer)))
    })
    .filter_map(|block| LineBlock::of(&block.tag_name().to_ascii_lowercase()).map(|kind| (block, kind)))
    .collect()
}

/// Enter on an empty quote line, or on the empty last line of a code block, leaves the block there.
fn leave_on_blank_line(editor: &HtmlElement) -> bool {
  let Some((block, kind, before)) = caret_line_block(editor) else {
    return false;
  };
  let Some(DomNode::Element(read_block)) = read(&block, 0) else {
    return false;
  };
  let lines = editor_lines::lines(kind, &read_block.children);
  let index = editor_lines::line_index(kind, &before);
  let is_last = index + 1 >= lines.len();
  let is_blank = lines.get(index).is_none_or(editor_lines::Line::is_blank);
  if !is_blank || (kind == LineBlock::Code && !is_last) {
    return false;
  }
  let Some(converted) = replace_lines(&block, kind, index, index) else {
    return false;
  };
  if let Some(first) = converted.first() {
    place_caret(first, 0);
  }
  true
}

/// The code block or quote the collapsed caret is in, with its content before the caret. Not when
/// the caret is in a list inside a quote: the list handles its own lines.
fn caret_line_block(editor: &HtmlElement) -> Option<(Element, LineBlock, Vec<DomNode>)> {
  if !matches!(block_format(editor), BlockFormat::CodeBlock | BlockFormat::Quote) {
    return None;
  }
  let selection = selection().filter(Selection::is_collapsed)?;
  let block = caret_closest(editor, LINE_BLOCK_SELECTOR)?;
  let kind = LineBlock::of(&block.tag_name().to_ascii_lowercase())?;
  let before = content_before(&block, &selection.anchor_node()?, selection.anchor_offset())?;
  Some((block, kind, before))
}

/// The block's content from its start up to a position in it.
fn content_before(block: &Element, node: &Node, offset: u32) -> Option<Vec<DomNode>> {
  let range = block.owner_document()?.create_range().ok()?;
  range.set_start(block, 0).ok()?;
  range.set_end(node, offset).ok()?;
  let fragment: Node = range.clone_contents().ok()?.into();
  Some(read_children(&fragment))
}

/// Replaces the block with its lines `first..=last` as paragraphs between the rest of it; returns
/// the paragraphs.
fn replace_lines(block: &Element, kind: LineBlock, first: usize, last: usize) -> Option<Vec<Node>> {
  let Some(DomNode::Element(read_block)) = read(block, 0) else {
    return None;
  };
  let unwrapped = editor_lines::unwrap(&read_block, kind, first, last);
  let document = block.owner_document()?;
  let parent = block.parent_node()?;
  let parts = unwrapped
    .maybe_before
    .iter()
    .map(|node| (node, false))
    .chain(unwrapped.lines.iter().map(|node| (node, true)))
    .chain(unwrapped.maybe_after.iter().map(|node| (node, false)));
  let mut converted = Vec::new();
  for (node, is_line) in parts {
    let built = build(&document, node)?;
    parent.insert_before(&built, Some(block)).ok()?;
    if is_line {
      converted.push(built);
    }
  }
  block.remove();
  Some(converted)
}

/// Puts the caret `column` UTF-16 units into the text of `node` (at its end if the text is shorter).
fn place_caret_at_column(node: &Node, column: usize) {
  let walker = node.owner_document().and_then(|document| {
    document
      .create_tree_walker_with_what_to_show(node, SHOW_TEXT)
      .ok()
  });
  let mut left = column as u32;
  let mut maybe_last: Option<Node> = None;
  while let Some(Ok(Some(text))) = walker.as_ref().map(web_sys::TreeWalker::next_node) {
    let length = text.dyn_ref::<web_sys::Text>().map_or(0, |text| text.length());
    if left <= length {
      place_caret(&text, left);
      return;
    }
    left -= length;
    maybe_last = Some(text);
  }
  match maybe_last {
    Some(text) => {
      let length = text.dyn_ref::<web_sys::Text>().map_or(0, |text| text.length());
      place_caret(&text, length);
    },
    None => place_caret(node, 0),
  }
}

fn select_between(first: &Node, last: &Node) {
  let (Some(selection), Some(document)) = (selection(), first.owner_document()) else {
    return;
  };
  let Ok(range) = document.create_range() else {
    return;
  };
  if range.set_start(first, 0).is_ok() && range.set_end(last, last.child_nodes().length()).is_ok() {
    drop(selection.remove_all_ranges());
    drop(selection.add_range(&range));
  }
}

fn caret_at_end_of(block: &Element) -> bool {
  let Some(selection) = selection() else {
    return false;
  };
  let Some(document) = block.owner_document() else {
    return false;
  };
  let (Some(anchor), Ok(range)) = (selection.anchor_node(), document.create_range()) else {
    return false;
  };
  if range.select_node_contents(block).is_err()
    || range.set_start(&anchor, selection.anchor_offset()).is_err()
  {
    return false;
  }
  String::from(range.to_string())
    .replace(CARET_ESCAPE, "")
    .trim()
    .is_empty()
}

/// The innermost element at the caret matching `selector`, inside the editor.
fn caret_closest(editor: &HtmlElement, selector: &str) -> Option<Element> {
  let node = selection()?.anchor_node()?;
  let element = match node.dyn_ref::<Element>() {
    Some(element) => element.clone(),
    None => node.parent_element()?,
  };
  let found = element.closest(selector).ok().flatten()?;
  (found != **editor && editor.contains(Some(&found))).then_some(found)
}

fn select_in_text(text: &web_sys::Text, start: usize, end: usize) -> bool {
  let (Some(selection), Some(document)) = (selection(), text.owner_document()) else {
    return false;
  };
  let Ok(range) = document.create_range() else {
    return false;
  };
  let length = text.length() as usize;
  let (start, end) = (start.min(length), end.min(length));
  if range.set_start(text, start as u32).is_err() || range.set_end(text, end as u32).is_err() {
    return false;
  }
  drop(selection.remove_all_ranges());
  selection.add_range(&range).is_ok()
}

fn place_caret(node: &Node, offset: u32) {
  if let Some(selection) = selection() {
    drop(selection.collapse_with_offset(Some(node), offset));
  }
}

fn selection() -> Option<Selection> {
  web_sys::window()?.get_selection().ok().flatten()
}

fn html_document() -> Option<HtmlDocument> {
  web_sys::window()?.document()?.dyn_into::<HtmlDocument>().ok()
}

/// Runs a browser editing command on the focused editor. Failing commands change nothing, which is
/// the best the editor can do; there is nothing to report to the user.
fn command(name: &str, value: &str) {
  if let Some(document) = html_document() {
    drop(document.exec_command_with_show_ui_and_value(name, false, value));
  }
}

/// Reads DOM children into the plain tree [`editor_doc`] walks.
fn read_children(parent: &Node) -> Vec<DomNode> {
  read_children_at(parent, 0)
}

fn read_children_at(parent: &Node, depth: usize) -> Vec<DomNode> {
  let children = parent.child_nodes();
  (0..children.length())
    .filter_map(|index| children.item(index))
    .filter_map(|child| read(&child, depth))
    .collect()
}

fn read(node: &Node, depth: usize) -> Option<DomNode> {
  if depth >= MAX_READ_DEPTH {
    return Some(DomNode::Text(node.text_content().unwrap_or_default()));
  }
  match node.node_type() {
    Node::TEXT_NODE => Some(DomNode::Text(node.text_content().unwrap_or_default())),
    Node::ELEMENT_NODE => {
      let element = node.dyn_ref::<Element>()?;
      let maybe_language = element.get_attribute("data-language").or_else(|| {
        element
          .get_attribute("class")?
          .split_whitespace()
          .find_map(|class| class.strip_prefix("language-").map(str::to_owned))
      });
      Some(DomNode::Element(DomElement {
        tag: element.tag_name().to_ascii_lowercase(),
        maybe_href: element.get_attribute("href"),
        maybe_style: element.get_attribute("style"),
        maybe_start: element.get_attribute("start"),
        maybe_language,
        children: read_children_at(node, depth + 1),
      }))
    },
    // Comments, processing instructions and the like carry nothing a message shows.
    _ => None,
  }
}

/// Builds DOM nodes for a tree made by [`editor_doc::from_blocks`]; text goes in as text nodes.
fn build(document: &Document, node: &DomNode) -> Option<Node> {
  match node {
    DomNode::Text(text) => Some(document.create_text_node(text).into()),
    DomNode::Element(element) => {
      let built = document.create_element(&element.tag).ok()?;
      for (name, maybe_value) in [
        ("href", &element.maybe_href),
        ("start", &element.maybe_start),
        ("data-language", &element.maybe_language),
      ] {
        if let Some(value) = maybe_value {
          drop(built.set_attribute(name, value));
        }
      }
      for child in &element.children {
        if let Some(child) = build(document, child) {
          drop(built.append_child(&child));
        }
      }
      Some(built.into())
    },
  }
}
