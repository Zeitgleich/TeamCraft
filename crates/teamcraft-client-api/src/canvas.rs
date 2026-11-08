//! A channel's shared page of notes (issue #1221, e2ee.md §4.11): one Yjs document (`yrs`) per
//! channel whose updates travel encrypted as messages of the channel.
//!
//! The core keeps a page as the list of updates it received ([`CanvasState`]) and never reads
//! them; only the page's editor does, through [`CanvasDoc`]. On the web the editor is a wasm chunk
//! of its own, so the CRDT loads only with the first page that opens.
//!
//! The document is an array of blocks; each block is a map with an ID, a kind, a check mark and
//! its text. Any set of updates gives the same page in any order and any number of times:
//! concurrent typing in one block merges character by character, concurrent changes of a block's
//! kind or check mark end with the same one winning everywhere.
//!
//! The schema, and what it asks of later versions (issue #1456, e2ee.md §4.11):
//!
//! - Root: `blocks`, a Y.Array of Y.Map, one per block.
//! - Block keys: `id` (string), `kind` (string, [`BlockKind::tag`]), `checked` (bool), `text`
//!   (Y.Text). A key this version doesn't know is left alone.
//! - Inline formatting is Y.Text formatting: `bold`, `italic`, `strike` and `code` (`true`), `link`
//!   (the address, a string; only addresses [`is_page_link`] allows are shown as links). An attribute
//!   this version doesn't know is left alone and shown as plain text.
//! - A later version may add kinds and attributes: a kind unknown here reads as a paragraph and its
//!   text stays. It must not put embeds into a block's text: every client counts a block's text
//!   as its characters only, so an inline object has to be text with an attribute (a mention is
//!   `@name` carrying a `mention` attribute).
//!
//! A client from before inline formatting reads the text without it, and its edits leave the
//! formatting of the text around them in place.

mod history;

pub use history::{
  CanvasMark, CanvasOrigin, CanvasVersion, compaction, editors_since, marks, origin_of, page_at, versions,
};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use teamcraft_proto::Base64Bytes;
use yrs::types::Attrs;
use yrs::types::text::YChange;
use yrs::undo::{Options as UndoOptions, UndoManager};
use yrs::updates::decoder::Decode;
use yrs::{
  Any, Array, ArrayRef, Doc, GetString, Map, MapPrelim, MapRef, Out, ReadTxn, StateVector, Text, TextPrelim,
  TextRef, Transact, TransactionMut, Update,
};

use crate::markdown;

/// The root array of blocks.
const BLOCKS: &str = "blocks";
/// The keys of a block's map.
const ID: &str = "id";
const KIND: &str = "kind";
const CHECKED: &str = "checked";
const TEXT: &str = "text";
/// The inline formatting attribute of a link; the marks' are in [`InlineMark::attribute`].
const LINK: &str = "link";
/// The origin of updates applied from outside: what undo leaves alone.
const REMOTE_ORIGIN: &str = "remote";
/// The key the undo log's watch on the document is registered under.
const UNDO_WATCH: &str = "undo";
/// Edits this close together are undone together.
const UNDO_GROUP_MS: u64 = 500;

/// What the core keeps of a channel's page: the updates in the order they arrived, never read by
/// the core itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanvasState {
  /// Grows with every change of the list; a compaction names the revision it read.
  pub revision: u64,
  /// Yjs updates (lib0 v1 encoding), opaque to the core.
  pub updates: Vec<Base64Bytes>,
  /// Updates applied since the last snapshot arrived (or since the start): when to send one.
  pub updates_since_snapshot: u32,
  /// Who sent each of `updates` and when, by position ([`origin_of`]); `None` for a compaction's
  /// merge. Shorter for a page kept before it had a history: the first updates have none.
  #[serde(default)]
  pub origins: Vec<Option<CanvasOrigin>>,
  /// Whether the first of `updates` is a compaction whose changes `history` holds.
  #[serde(default)]
  pub is_compacted: bool,
  /// What the compacted updates changed, oldest first ([`marks`]): the page's version history
  /// before the rest of `updates`.
  #[serde(default)]
  pub history: Vec<CanvasMark>,
}

/// What a block is shown as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlockKind {
  Paragraph,
  Heading,
  Heading2,
  Heading3,
  Bullet,
  Numbered,
  Check,
  Quote,
  Code,
}

impl BlockKind {
  pub const ALL: [Self; 9] = [
    Self::Paragraph,
    Self::Heading,
    Self::Heading2,
    Self::Heading3,
    Self::Bullet,
    Self::Numbered,
    Self::Check,
    Self::Quote,
    Self::Code,
  ];

  /// The block's kind as the document stores it.
  pub fn tag(self) -> &'static str {
    match self {
      Self::Paragraph => "paragraph",
      Self::Heading => "heading",
      Self::Heading2 => "heading2",
      Self::Heading3 => "heading3",
      Self::Bullet => "bullet",
      Self::Numbered => "numbered",
      Self::Check => "check",
      Self::Quote => "quote",
      Self::Code => "code",
    }
  }

  /// A stored kind; one of a newer version reads as a paragraph.
  pub fn from_tag(tag: &str) -> Self {
    Self::ALL
      .into_iter()
      .find(|kind| kind.tag() == tag)
      .unwrap_or(Self::Paragraph)
  }

  /// Enter makes another block of the same kind (a list or a quote); in an empty one it ends it.
  pub fn continues_on_enter(self) -> bool {
    matches!(self, Self::Bullet | Self::Numbered | Self::Check | Self::Quote)
  }

  /// Whether the block's text is shown as it is: code has no inline formatting, and Enter stays
  /// in it.
  pub fn is_code(self) -> bool {
    self == Self::Code
  }

  /// The kind typed at the start of a paragraph turns it into ("# ", "- ", "1. ", "[] ", "> ",
  /// three backticks), with the length of that prefix in bytes.
  pub fn from_prefix(text: &str) -> Option<(Self, usize)> {
    const PREFIXES: [(&str, BlockKind); 11] = [
      ("# ", BlockKind::Heading),
      ("## ", BlockKind::Heading2),
      ("### ", BlockKind::Heading3),
      ("> ", BlockKind::Quote),
      ("```", BlockKind::Code),
      ("- ", BlockKind::Bullet),
      ("* ", BlockKind::Bullet),
      ("1. ", BlockKind::Numbered),
      ("[] ", BlockKind::Check),
      ("[ ] ", BlockKind::Check),
      ("[x] ", BlockKind::Check),
    ];
    PREFIXES
      .into_iter()
      .find(|(prefix, _)| text.starts_with(prefix))
      .map(|(prefix, kind)| (kind, prefix.len()))
  }
}

/// One block of a page as the editor shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
  /// Stable across edits and devices: what the editor keys its rows by.
  pub id: String,
  pub kind: BlockKind,
  pub text: String,
  pub is_checked: bool,
  /// The formatted stretches of `text`, in order and not overlapping; the rest is plain.
  pub spans: Vec<TextSpan>,
}

impl Block {
  /// The text in stretches of one formatting each, plain ones (`None`) included.
  pub fn runs(&self) -> Vec<(&str, Option<&TextStyle>)> {
    let mut runs = Vec::with_capacity(self.spans.len() * 2 + 1);
    let mut at = 0;
    for span in &self.spans {
      if span.start > at {
        runs.push((&self.text[at..span.start], None));
      }
      runs.push((&self.text[span.start..span.end], Some(&span.style)));
      at = span.end;
    }
    if at < self.text.len() || runs.is_empty() {
      runs.push((&self.text[at..], None));
    }
    runs
  }

  /// Whether the text from `start` to `end` (UTF-16 units) has `mark` throughout. A caret asks what
  /// typing there would get: the formatting of the character before it.
  pub fn has_mark(&self, start: u32, end: u32, mark: InlineMark) -> bool {
    let (from, to) = (byte_offset(&self.text, start), byte_offset(&self.text, end));
    if from == to {
      return self.style_before(from).is_some_and(|style| style.has(mark));
    }
    let mut covered = from;
    for span in self.spans.iter().filter(|span| span.style.has(mark)) {
      if span.start > covered {
        break;
      }
      covered = covered.max(span.end);
      if covered >= to {
        return true;
      }
    }
    false
  }

  /// The address of the link at the start of the range, or before a caret.
  pub fn link_at(&self, start: u32) -> Option<&str> {
    self
      .style_before(byte_offset(&self.text, start))
      .or_else(|| self.style_at(byte_offset(&self.text, start)))
      .and_then(|style| style.maybe_link.as_deref())
  }

  fn style_at(&self, at: usize) -> Option<&TextStyle> {
    style_at(&self.spans, at)
  }

  fn style_before(&self, at: usize) -> Option<&TextStyle> {
    self.text[..at]
      .chars()
      .next_back()
      .and_then(|last| self.style_at(at - last.len_utf8()))
  }
}

/// A formatting a stretch of text has or hasn't.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InlineMark {
  Bold,
  Italic,
  Strike,
  Code,
}

impl InlineMark {
  pub const ALL: [Self; 4] = [Self::Bold, Self::Italic, Self::Strike, Self::Code];

  /// The formatting attribute the document stores.
  fn attribute(self) -> &'static str {
    match self {
      Self::Bold => "bold",
      Self::Italic => "italic",
      Self::Strike => "strike",
      Self::Code => "code",
    }
  }
}

/// The formatting of a stretch of text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextStyle {
  pub is_bold: bool,
  pub is_italic: bool,
  pub is_strike: bool,
  pub is_code: bool,
  pub maybe_link: Option<String>,
}

impl TextStyle {
  pub fn has(&self, mark: InlineMark) -> bool {
    match mark {
      InlineMark::Bold => self.is_bold,
      InlineMark::Italic => self.is_italic,
      InlineMark::Strike => self.is_strike,
      InlineMark::Code => self.is_code,
    }
  }

  fn is_plain(&self) -> bool {
    *self == Self::default()
  }

  fn from_attributes(maybe_attributes: Option<&Attrs>) -> Self {
    let Some(attributes) = maybe_attributes else {
      return Self::default();
    };
    let is_on = |mark: InlineMark| matches!(attributes.get(mark.attribute()), Some(Any::Bool(true)));
    Self {
      is_bold: is_on(InlineMark::Bold),
      is_italic: is_on(InlineMark::Italic),
      is_strike: is_on(InlineMark::Strike),
      is_code: is_on(InlineMark::Code),
      maybe_link: match attributes.get(LINK) {
        Some(Any::String(url)) if is_page_link(url) => Some(url.to_string()),
        _ => None,
      },
    }
  }
}

/// A formatted stretch of a block's text, in bytes of its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextSpan {
  pub start: usize,
  pub end: usize,
  pub style: TextStyle,
}

#[derive(Debug, thiserror::Error)]
pub enum CanvasError {
  #[error("a page update could not be read")]
  Decode,
  #[error("a page update could not be applied")]
  Apply,
}

/// A page being read or edited: the Yjs document and the blocks in it. Each edit returns the update
/// it made, which goes to the other members.
pub struct CanvasDoc {
  doc: Doc,
  blocks: ArrayRef,
  /// Block IDs this instance handed out: with the document's random client ID, unique.
  next_block: u64,
  /// This editor's own edits, for undo and redo ([`CanvasDoc::enable_undo`]).
  maybe_undo: Option<UndoLog>,
}

/// The undo and redo of one editor. Only what this editor did: updates from others are applied
/// under [`REMOTE_ORIGIN`], which it doesn't track.
struct UndoLog {
  manager: UndoManager<()>,
  /// The updates undo and redo made, which go to the other members like any edit.
  made: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Default for CanvasDoc {
  fn default() -> Self {
    Self::new()
  }
}

impl CanvasDoc {
  /// An empty page under a new random client ID (one per editor instance, as Yjs requires).
  pub fn new() -> Self {
    let doc = Doc::new();
    let blocks = doc.get_or_insert_array(BLOCKS);
    Self {
      doc,
      blocks,
      next_block: 0,
      maybe_undo: None,
    }
  }

  /// Keeps this editor's edits from now on, so they can be undone. `now_ms` is the time it groups
  /// them by (a parameter: wasm has no system clock).
  pub fn enable_undo(&mut self, now_ms: impl Fn() -> u64 + Send + Sync + 'static) {
    let mut manager = UndoManager::with_options(UndoOptions {
      capture_timeout_millis: UNDO_GROUP_MS,
      tracked_origins: std::collections::HashSet::new(),
      capture_transaction: None,
      timestamp: Arc::new(now_ms),
      init_undo_stack: Vec::new(),
      init_redo_stack: Vec::new(),
    });
    manager.expand_scope(&self.doc, &self.blocks);
    let made = Arc::new(Mutex::new(Vec::new()));
    let origin = manager.as_origin();
    let sink = Arc::clone(&made);
    let watched = self.doc.observe_update_v1(UNDO_WATCH, move |txn, event| {
      if txn.origin() == Some(&origin)
        && let Ok(mut updates) = sink.lock()
      {
        updates.push(event.update.clone());
      }
    });
    if watched.is_ok() {
      self.maybe_undo = Some(UndoLog { manager, made });
    }
  }

  /// Takes back this editor's last edit; the update to send when there was one.
  pub fn undo(&mut self) -> Option<Vec<u8>> {
    let log = self.maybe_undo.as_mut()?;
    log.manager.undo_blocking();
    log.take_update()
  }

  /// Does again what [`Self::undo`] took back.
  pub fn redo(&mut self) -> Option<Vec<u8>> {
    let log = self.maybe_undo.as_mut()?;
    log.manager.redo_blocking();
    log.take_update()
  }

  pub fn can_undo(&self) -> bool {
    self.maybe_undo.as_ref().is_some_and(|log| log.manager.can_undo())
  }

  pub fn can_redo(&self) -> bool {
    self.maybe_undo.as_ref().is_some_and(|log| log.manager.can_redo())
  }

  /// The page made of `updates`, in any order.
  pub fn from_updates<B: AsRef<[u8]>>(updates: &[B]) -> Result<Self, CanvasError> {
    let mut page = Self::new();
    page.sync(updates)?;
    Ok(page)
  }

  /// Brings the page up to `updates`: every update known of it, in any order, own ones and ones
  /// applied before included (they change nothing again). They are merged into one update first
  /// and applied in one go. Applied one by one out of order, yrs 0.28 left some waiting for
  /// predecessors that had arrived and the page differed between devices (a test below found it),
  /// so the page is never fed a part of the set alone. Updates that need others not here yet wait
  /// inside the document until a later set brings them.
  pub fn sync<B: AsRef<[u8]>>(&mut self, updates: &[B]) -> Result<(), CanvasError> {
    if updates.is_empty() {
      return Ok(());
    }
    let merged = merge_updates(updates)?;
    let update = Update::decode_v1(&merged).map_err(|_| CanvasError::Decode)?;
    self
      .doc
      .transact_mut_with(REMOTE_ORIGIN)
      .apply_update(update)
      .map_err(|_| CanvasError::Apply)
  }

  /// The blocks in page order.
  pub fn blocks(&self) -> Vec<Block> {
    let txn = self.doc.transact();
    let mut blocks = Vec::new();
    for index in 0..self.blocks.len(&txn) {
      if let Some(Out::YMap(map)) = self.blocks.get(&txn, index) {
        blocks.push(read_block(&txn, &map));
      }
    }
    blocks
  }

  /// Replaces a block's text with `text`, as the change from the current one: the part both
  /// share at the start and the end stays, so concurrent typing elsewhere in the block survives.
  /// `None` when the block is gone or nothing changed.
  pub fn set_text(&mut self, id: &str, text: &str) -> Option<Vec<u8>> {
    let mut txn = self.doc.transact_mut();
    let (_, map) = find(&txn, &self.blocks, id)?;
    let Some(Out::YText(block_text)) = map.get(&txn, TEXT) else {
      return None;
    };
    let old = block_text.get_string(&txn);
    let edit = TextEdit::between(&old, text)?;
    replace(&mut txn, &block_text, &edit, text);
    Some(txn.encode_update_v1())
  }

  /// Adds a block after `maybe_after` (at the start without one, at the end when that block is
  /// gone) and returns its ID with the update.
  pub fn insert_block(
    &mut self,
    maybe_after: Option<&str>,
    kind: BlockKind,
    text: &str,
  ) -> (String, Vec<u8>) {
    let id = self.next_block_id();
    let mut txn = self.doc.transact_mut();
    let index = match maybe_after {
      Some(after) => find(&txn, &self.blocks, after).map_or(self.blocks.len(&txn), |(index, _)| index + 1),
      None => 0,
    };
    let block = Block {
      id: id.clone(),
      kind,
      text: text.to_owned(),
      is_checked: false,
      spans: Vec::new(),
    };
    insert_block_at(&mut txn, &self.blocks, index, &block);
    let update = txn.encode_update_v1();
    (id, update)
  }

  /// Enter: `start..end` (UTF-16 units) of the block goes, what follows becomes a new block of
  /// `next_kind` right after it, its formatting kept. The new block's ID and the update.
  pub fn split_block(
    &mut self,
    id: &str,
    start: u32,
    end: u32,
    next_kind: BlockKind,
  ) -> Option<(String, Vec<u8>)> {
    let new_id = self.next_block_id();
    let mut txn = self.doc.transact_mut();
    let (index, map) = find(&txn, &self.blocks, id)?;
    let block = read_block(&txn, &map);
    let (from, to) = (byte_offset(&block.text, start), byte_offset(&block.text, end));
    let tail = Block {
      id: new_id.clone(),
      kind: next_kind,
      text: block.text[to..].to_owned(),
      is_checked: false,
      spans: spans_within(&block.spans, to, block.text.len()),
    };
    if let Some(Out::YText(text)) = map.get(&txn, TEXT)
      && block.text.len() > from
    {
      text.remove_range(&mut txn, byte_index(from), byte_index(block.text.len() - from));
    }
    insert_block_at(&mut txn, &self.blocks, index + 1, &tail);
    let update = txn.encode_update_v1();
    Some((new_id, update))
  }

  /// Backspace at a block's start: its text moves to the end of `into`, formatting kept, and the
  /// block goes.
  pub fn join_blocks(&mut self, into: &str, id: &str) -> Option<Vec<u8>> {
    let mut txn = self.doc.transact_mut();
    let (_, target) = find(&txn, &self.blocks, into)?;
    let (index, map) = find(&txn, &self.blocks, id)?;
    let joined = read_block(&txn, &map);
    let kept = read_block(&txn, &target);
    if let Some(Out::YText(text)) = target.get(&txn, TEXT) {
      text.insert(&mut txn, byte_index(kept.text.len()), &joined.text);
      let mut spans = kept.spans;
      let offset = kept.text.len();
      spans.extend(joined.spans.into_iter().map(|span| TextSpan {
        start: span.start + offset,
        end: span.end + offset,
        style: span.style,
      }));
      write_spans(&mut txn, &text, &spans);
    }
    self.blocks.remove(&mut txn, index);
    Some(txn.encode_update_v1())
  }

  /// Turns `mark` on or off for `start..end` (UTF-16 units) of a block; `None` for an empty range.
  pub fn set_mark(
    &mut self,
    id: &str,
    start: u32,
    end: u32,
    mark: InlineMark,
    is_on: bool,
  ) -> Option<Vec<u8>> {
    let value = if is_on { Any::Bool(true) } else { Any::Null };
    self.format(id, start, end, mark.attribute(), value)
  }

  /// `mark` on `start..end` (UTF-16 units) of a block if part of it lacks it, otherwise off.
  pub fn toggle_mark(&mut self, id: &str, start: u32, end: u32, mark: InlineMark) -> Option<Vec<u8>> {
    let is_on = {
      let txn = self.doc.transact();
      let (_, map) = find(&txn, &self.blocks, id)?;
      read_block(&txn, &map).has_mark(start, end, mark)
    };
    self.set_mark(id, start, end, mark, !is_on)
  }

  /// Links `start..end` (UTF-16 units) of a block to an address [`is_page_link`] allows, or takes
  /// the link away.
  pub fn set_link(&mut self, id: &str, start: u32, end: u32, maybe_url: Option<&str>) -> Option<Vec<u8>> {
    let value = match maybe_url {
      Some(url) if is_page_link(url) => Any::from(url),
      Some(_) => return None,
      None => Any::Null,
    };
    self.format(id, start, end, LINK, value)
  }

  /// Removes a block with its text.
  pub fn remove_block(&mut self, id: &str) -> Option<Vec<u8>> {
    let mut txn = self.doc.transact_mut();
    let (index, _) = find(&txn, &self.blocks, id)?;
    self.blocks.remove(&mut txn, index);
    Some(txn.encode_update_v1())
  }

  pub fn set_kind(&mut self, id: &str, kind: BlockKind) -> Option<Vec<u8>> {
    let mut txn = self.doc.transact_mut();
    let (_, map) = find(&txn, &self.blocks, id)?;
    map.insert(&mut txn, KIND, kind.tag());
    Some(txn.encode_update_v1())
  }

  pub fn set_checked(&mut self, id: &str, is_checked: bool) -> Option<Vec<u8>> {
    let mut txn = self.doc.transact_mut();
    let (_, map) = find(&txn, &self.blocks, id)?;
    map.insert(&mut txn, CHECKED, is_checked);
    Some(txn.encode_update_v1())
  }

  /// Makes the page `target` again (a version restored from its history) as one edit on top of
  /// the current page: blocks that stay keep their identity and only their changed text is
  /// replaced, so typing elsewhere meanwhile survives; blocks not in `target` go, the others come
  /// back. `None` when the page already is `target`.
  pub fn restore(&mut self, target: &[Block]) -> Option<Vec<u8>> {
    if self.blocks() == target {
      return None;
    }
    let wanted: std::collections::HashSet<&str> = target.iter().map(|block| block.id.as_str()).collect();
    let mut txn = self.doc.transact_mut();
    // Blocks the version doesn't have go first.
    let mut index = 0;
    while index < self.blocks.len(&txn) {
      let maybe_id = match self.blocks.get(&txn, index) {
        Some(Out::YMap(map)) => string(&txn, &map, ID),
        _ => None,
      };
      if maybe_id.as_deref().is_some_and(|id| wanted.contains(id)) {
        index += 1;
      } else {
        self.blocks.remove(&mut txn, index);
      }
    }
    // Then the version's blocks in its order: one in place is changed where it differs; blocks
    // before it that come later in the version go (and come back there afresh); one gone is
    // inserted with its old ID.
    for (position, block) in target.iter().enumerate() {
      let position = byte_index(position);
      let maybe_at = (position..self.blocks.len(&txn)).find(|&at| match self.blocks.get(&txn, at) {
        Some(Out::YMap(map)) => string(&txn, &map, ID).as_deref() == Some(block.id.as_str()),
        _ => false,
      });
      match maybe_at {
        Some(at) => {
          if at > position {
            self.blocks.remove_range(&mut txn, position, at - position);
          }
          if let Some(Out::YMap(map)) = self.blocks.get(&txn, position) {
            write_block(&mut txn, &map, block);
          }
        },
        None => insert_block_at(&mut txn, &self.blocks, position, block),
      }
    }
    let length = self.blocks.len(&txn);
    let count = byte_index(target.len());
    if length > count {
      self.blocks.remove_range(&mut txn, count, length - count);
    }
    Some(txn.encode_update_v1())
  }

  fn next_block_id(&mut self) -> String {
    let id = format!("{:x}-{:x}", self.doc.client_id().get(), self.next_block);
    self.next_block += 1;
    id
  }

  fn format(&mut self, id: &str, start: u32, end: u32, attribute: &str, value: Any) -> Option<Vec<u8>> {
    let mut txn = self.doc.transact_mut();
    let (_, map) = find(&txn, &self.blocks, id)?;
    let Some(Out::YText(text)) = map.get(&txn, TEXT) else {
      return None;
    };
    let whole = text.get_string(&txn);
    let (from, to) = (byte_offset(&whole, start), byte_offset(&whole, end));
    if from >= to {
      return None;
    }
    let attributes = Attrs::from([(Arc::from(attribute), value)]);
    text.format(&mut txn, byte_index(from), byte_index(to - from), attributes);
    Some(txn.encode_update_v1())
  }

  /// Whether the page has updates waiting for ones not here yet.
  pub fn has_missing(&self) -> bool {
    let txn = self.doc.transact();
    let store = txn.store();
    store.pending_update().is_some() || store.pending_ds().is_some()
  }
}

impl UndoLog {
  /// What the last undo or redo made, as one update.
  fn take_update(&mut self) -> Option<Vec<u8>> {
    let made = std::mem::take(&mut *self.made.lock().ok()?);
    match made.as_slice() {
      [] => None,
      [update] => Some(update.clone()),
      updates => merge_updates(updates).ok(),
    }
  }
}

/// The whole page of `updates` as one update without the text deleted from it: what a snapshot
/// sends. Built in a document of its own, so nothing an editor applied earlier counts. `None`
/// while updates it depends on are missing, which a snapshot would leave out.
pub fn snapshot<B: AsRef<[u8]>>(updates: &[B]) -> Result<Option<Vec<u8>>, CanvasError> {
  let page = CanvasDoc::from_updates(updates)?;
  if page.has_missing() {
    return Ok(None);
  }
  let txn = page.doc.transact();
  Ok(Some(txn.encode_state_as_update_v1(&StateVector::default())))
}

/// `updates` as one update, losing nothing: also the parts still waiting for others. What the
/// editor gives the core when it compacts a page's list.
pub fn merge_updates<B: AsRef<[u8]>>(updates: &[B]) -> Result<Vec<u8>, CanvasError> {
  yrs::merge_updates_v1(updates.iter().map(AsRef::as_ref)).map_err(|_| CanvasError::Decode)
}

/// Where a caret at `offset` (UTF-16 code units, as text fields count) stands once `old` became
/// `new`: after a change before it, it moves with the text; a change after it leaves it.
pub fn shift_caret(old: &str, new: &str, offset: u32) -> u32 {
  let old_units: Vec<u16> = old.encode_utf16().collect();
  let new_units: Vec<u16> = new.encode_utf16().collect();
  let prefix = common_prefix(&old_units, &new_units);
  let suffix = common_suffix(&old_units[prefix..], &new_units[prefix..]);
  let offset = offset as usize;
  let shifted = if offset <= prefix {
    offset
  } else if offset >= old_units.len() - suffix {
    // After the change: keeps its distance from the end.
    new_units.len() - (old_units.len() - offset)
  } else {
    // Inside the replaced part: at its end.
    new_units.len() - suffix
  };
  u32::try_from(shifted).unwrap_or(u32::MAX)
}

/// The UTF-16 range the change from `old` to `new` wrote, in `new`; `None` when it only removed.
pub fn typed_range(old: &str, new: &str) -> Option<(u32, u32)> {
  let (prefix, end) = changed_end(old, new);
  (end > prefix).then(|| (units(prefix), units(end)))
}

/// Where a caret stands after the change from `old` to `new`: at the end of what changed.
pub fn caret_after_change(old: &str, new: &str) -> u32 {
  units(changed_end(old, new).1)
}

/// The byte index of a caret given in UTF-16 units (as text fields count), on a character boundary
/// and within `text`.
pub fn byte_offset(text: &str, units: u32) -> usize {
  let mut counted = 0_u32;
  for (index, character) in text.char_indices() {
    if counted >= units {
      return index;
    }
    counted += u32::try_from(character.len_utf16()).unwrap_or(2);
  }
  text.len()
}

/// Whether a block's text may link to `url`: the web addresses messages may link to (no permalink
/// into the app, no other scheme). What another member's client stored is held to it as well.
pub fn is_page_link(url: &str) -> bool {
  markdown::is_allowed_link(url) && !url.starts_with(markdown::PERMALINK_PREFIX)
}

/// The start of the change from `old` to `new` and its end in `new`, in UTF-16 units.
fn changed_end(old: &str, new: &str) -> (usize, usize) {
  let old_units: Vec<u16> = old.encode_utf16().collect();
  let new_units: Vec<u16> = new.encode_utf16().collect();
  let prefix = common_prefix(&old_units, &new_units);
  let suffix = common_suffix(&old_units[prefix..], &new_units[prefix..]);
  (prefix, new_units.len() - suffix)
}

fn units(count: usize) -> u32 {
  u32::try_from(count).unwrap_or(u32::MAX)
}

/// The smallest change from one text to another: `removed` bytes at `start`, then the new text's
/// bytes from `start` to `inserted_end`.
#[derive(Debug, PartialEq, Eq)]
struct TextEdit {
  start: usize,
  removed: usize,
  inserted_end: usize,
}

impl TextEdit {
  fn between(old: &str, new: &str) -> Option<Self> {
    if old == new {
      return None;
    }
    let prefix = old
      .char_indices()
      .zip(new.chars())
      .find(|((_, a), b)| a != b)
      .map_or_else(|| old.len().min(new.len()), |((index, _), _)| index);
    // Never inside a character of the shorter text: back to a boundary of both.
    let mut start = prefix.min(old.len()).min(new.len());
    while !old.is_char_boundary(start) || !new.is_char_boundary(start) {
      start -= 1;
    }
    let suffix = old[start..]
      .chars()
      .rev()
      .zip(new[start..].chars().rev())
      .take_while(|(a, b)| a == b)
      .map(|(a, _)| a.len_utf8())
      .sum::<usize>();
    Some(Self {
      start,
      removed: old.len() - start - suffix,
      inserted_end: new.len() - suffix,
    })
  }
}

/// Inserts before it removes: the new text then follows the text before it, not what is removed,
/// so typing of another member right after the removed part stays after the new text.
fn replace(txn: &mut TransactionMut, text: &TextRef, edit: &TextEdit, new: &str) {
  let start = byte_index(edit.start);
  let inserted = &new[edit.start..edit.inserted_end];
  if !inserted.is_empty() {
    match replaced_attributes(txn, text, edit) {
      Some(attributes) => text.insert_with_attributes(txn, start, inserted, attributes),
      None => text.insert(txn, start, inserted),
    }
  }
  if edit.removed > 0 {
    text.remove_range(
      txn,
      byte_index(edit.start + inserted.len()),
      byte_index(edit.removed),
    );
  }
}

/// What new text replacing part of a formatted text gets: the formatting of what it replaces. `None`
/// when nothing is replaced (typing takes the formatting around it) or the text has no formatting.
fn replaced_attributes(txn: &TransactionMut, text: &TextRef, edit: &TextEdit) -> Option<Attrs> {
  if edit.removed == 0 {
    return None;
  }
  let mut offset = 0;
  let mut maybe_found = None;
  let mut has_formatting = false;
  for chunk in text.diff(txn, YChange::identity) {
    let Out::Any(Any::String(piece)) = chunk.insert else {
      continue;
    };
    has_formatting |= chunk.attributes.is_some();
    if maybe_found.is_none() && offset + piece.len() > edit.start {
      maybe_found = Some(chunk.attributes.map(|boxed| *boxed).unwrap_or_default());
    }
    offset += piece.len();
  }
  maybe_found.filter(|_| has_formatting)
}

/// Offsets count bytes (the document's default offset kind). A block's text never reaches 4 GiB:
/// a message carrying it would be refused long before.
fn byte_index(bytes: usize) -> u32 {
  u32::try_from(bytes).unwrap_or(u32::MAX)
}

fn find<T: ReadTxn>(txn: &T, blocks: &ArrayRef, id: &str) -> Option<(u32, MapRef)> {
  (0..blocks.len(txn)).find_map(|index| match blocks.get(txn, index) {
    Some(Out::YMap(map)) if string(txn, &map, ID).as_deref() == Some(id) => Some((index, map)),
    _ => None,
  })
}

/// Brings a block's kind, check mark and text to `block`, changing only what differs.
fn write_block(txn: &mut TransactionMut, map: &MapRef, block: &Block) {
  let current = read_block(txn, map);
  if current.kind != block.kind {
    map.insert(txn, KIND, block.kind.tag());
  }
  if current.is_checked != block.is_checked {
    map.insert(txn, CHECKED, block.is_checked);
  }
  match map.get(txn, TEXT) {
    Some(Out::YText(text)) => {
      if let Some(edit) = TextEdit::between(&current.text, &block.text) {
        replace(txn, &text, &edit, &block.text);
      }
      write_spans(txn, &text, &block.spans);
    },
    _ => {
      let text = map.insert(txn, TEXT, TextPrelim::new(block.text.as_str()));
      write_spans(txn, &text, &block.spans);
    },
  }
}

/// A new block at `index` of the array, with `block`'s ID, kind, check mark, text and formatting.
fn insert_block_at(txn: &mut TransactionMut, blocks: &ArrayRef, index: u32, block: &Block) {
  let map = blocks.insert(txn, index, MapPrelim::default());
  map.insert(txn, ID, block.id.as_str());
  map.insert(txn, KIND, block.kind.tag());
  map.insert(txn, CHECKED, block.is_checked);
  let text = map.insert(txn, TEXT, TextPrelim::new(block.text.as_str()));
  write_spans(txn, &text, &block.spans);
}

/// Brings the formatting this version knows of `text` to `target`, stretch by stretch and only
/// where it differs. Attributes it doesn't know stay as they are.
fn write_spans(txn: &mut TransactionMut, text: &TextRef, target: &[TextSpan]) {
  let (whole, current) = read_text(txn, text);
  let mut cuts = vec![0, whole.len()];
  for span in current.iter().chain(target) {
    cuts.extend([span.start.min(whole.len()), span.end.min(whole.len())]);
  }
  cuts.sort_unstable();
  cuts.dedup();
  for stretch in cuts.windows(2) {
    let plain = TextStyle::default();
    let have = style_at(&current, stretch[0]).unwrap_or(&plain);
    let want = style_at(target, stretch[0]).unwrap_or(&plain);
    let attributes = attribute_changes(have, want);
    if !attributes.is_empty() {
      text.format(
        txn,
        byte_index(stretch[0]),
        byte_index(stretch[1] - stretch[0]),
        attributes,
      );
    }
  }
}

/// The attributes that turn the formatting `have` into `want`.
fn attribute_changes(have: &TextStyle, want: &TextStyle) -> Attrs {
  let mut attributes = Attrs::new();
  for mark in InlineMark::ALL {
    if have.has(mark) != want.has(mark) {
      let value = if want.has(mark) {
        Any::Bool(true)
      } else {
        Any::Null
      };
      attributes.insert(Arc::from(mark.attribute()), value);
    }
  }
  if have.maybe_link != want.maybe_link {
    let value = want.maybe_link.as_deref().map_or(Any::Null, Any::from);
    attributes.insert(Arc::from(LINK), value);
  }
  attributes
}

fn style_at(spans: &[TextSpan], at: usize) -> Option<&TextStyle> {
  spans
    .iter()
    .find(|span| span.start <= at && at < span.end)
    .map(|span| &span.style)
}

/// The formatting of `start..end` of a text, relative to `start`.
fn spans_within(spans: &[TextSpan], start: usize, end: usize) -> Vec<TextSpan> {
  spans
    .iter()
    .filter(|span| span.end > start && span.start < end)
    .map(|span| TextSpan {
      start: span.start.max(start) - start,
      end: span.end.min(end) - start,
      style: span.style.clone(),
    })
    .collect()
}

fn read_block<T: ReadTxn>(txn: &T, map: &MapRef) -> Block {
  let (text, spans) = match map.get(txn, TEXT) {
    Some(Out::YText(text)) => read_text(txn, &text),
    _ => (String::new(), Vec::new()),
  };
  Block {
    id: string(txn, map, ID).unwrap_or_default(),
    kind: BlockKind::from_tag(&string(txn, map, KIND).unwrap_or_default()),
    text,
    is_checked: matches!(map.get(txn, CHECKED), Some(Out::Any(Any::Bool(true)))),
    spans,
  }
}

/// A block's text and its formatted stretches. Only text counts: see the module's schema.
fn read_text<T: ReadTxn>(txn: &T, text: &TextRef) -> (String, Vec<TextSpan>) {
  let mut whole = String::new();
  let mut spans: Vec<TextSpan> = Vec::new();
  for chunk in text.diff(txn, YChange::identity) {
    let Out::Any(Any::String(piece)) = chunk.insert else {
      continue;
    };
    let style = TextStyle::from_attributes(chunk.attributes.as_deref());
    let start = whole.len();
    whole.push_str(&piece);
    if style.is_plain() {
      continue;
    }
    match spans.last_mut() {
      Some(last) if last.end == start && last.style == style => last.end = whole.len(),
      _ => spans.push(TextSpan {
        start,
        end: whole.len(),
        style,
      }),
    }
  }
  (whole, spans)
}

fn string<T: ReadTxn>(txn: &T, map: &MapRef, key: &str) -> Option<String> {
  match map.get(txn, key) {
    Some(Out::Any(Any::String(value))) => Some(value.to_string()),
    _ => None,
  }
}

fn common_prefix(a: &[u16], b: &[u16]) -> usize {
  a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

fn common_suffix(a: &[u16], b: &[u16]) -> usize {
  a.iter()
    .rev()
    .zip(b.iter().rev())
    .take_while(|(x, y)| x == y)
    .count()
}

#[cfg(test)]
mod tests {
  use super::*;

  /// An editor as the page's view runs one: its page and every update it knows (own ones too),
  /// which is what it syncs the page with.
  struct Editor {
    page: CanvasDoc,
    known: Vec<Vec<u8>>,
  }

  impl Editor {
    fn new(known: &[Vec<u8>]) -> Self {
      Self {
        page: CanvasDoc::from_updates(known).unwrap(),
        known: known.to_vec(),
      }
    }

    /// Makes an edit and keeps it; returns it to send.
    fn edit(&mut self, edit: impl FnOnce(&mut CanvasDoc) -> Option<Vec<u8>>) -> Option<Vec<u8>> {
      let update = edit(&mut self.page)?;
      self.known.push(update.clone());
      Some(update)
    }

    fn receive<'a>(&mut self, updates: impl IntoIterator<Item = &'a Vec<u8>>) {
      self.known.extend(updates.into_iter().cloned());
      self.page.sync(&self.known).unwrap();
    }

    fn texts(&self) -> Vec<String> {
      texts(&self.page)
    }
  }

  /// Two editors on a page with one heading, exchanging nothing until told.
  fn pair() -> (Editor, Editor) {
    let mut first = CanvasDoc::new();
    let (_, update) = first.insert_block(None, BlockKind::Heading, "Agenda");
    (Editor::new(std::slice::from_ref(&update)), Editor::new(&[update]))
  }

  fn texts(page: &CanvasDoc) -> Vec<String> {
    page.blocks().into_iter().map(|block| block.text).collect()
  }

  #[test]
  fn concurrent_typing_in_one_block_merges_on_both_sides() {
    let (mut alice, mut bob) = pair();
    let id = alice.page.blocks()[0].id.clone();
    let from_alice = alice
      .edit(|page| page.set_text(&id, "Agenda for Monday"))
      .unwrap();
    let from_bob = bob.edit(|page| page.set_text(&id, "Team agenda")).unwrap();
    alice.receive([&from_bob]);
    bob.receive([&from_alice]);
    assert_eq!(alice.texts(), bob.texts());
    assert_eq!(alice.texts(), ["Team agenda for Monday"]);
  }

  #[test]
  fn updates_in_any_order_and_twice_give_the_same_page() {
    let mut alice = CanvasDoc::new();
    let mut updates = Vec::new();
    let (first, update) = alice.insert_block(None, BlockKind::Paragraph, "one");
    updates.push(update);
    let (second, update) = alice.insert_block(Some(&first), BlockKind::Check, "buy milk");
    updates.push(update);
    updates.push(alice.set_checked(&second, true).unwrap());
    updates.push(alice.set_text(&first, "one, two").unwrap());
    let (third, update) = alice.insert_block(Some(&second), BlockKind::Bullet, "gone soon");
    updates.push(update);
    updates.push(alice.remove_block(&third).unwrap());

    let mut reversed = updates.clone();
    reversed.reverse();
    let mut doubled = updates.clone();
    doubled.extend(updates.iter().cloned());
    for order in [updates.clone(), reversed, doubled] {
      let page = CanvasDoc::from_updates(&order).unwrap();
      assert!(!page.has_missing());
      assert_eq!(page.blocks(), alice.blocks());
    }
    let blocks = alice.blocks();
    assert_eq!(texts(&alice), ["one, two", "buy milk"]);
    assert!(blocks[1].is_checked);
    assert_eq!(blocks[1].kind, BlockKind::Check);
  }

  #[test]
  fn concurrent_structure_changes_converge() {
    let (mut alice, mut bob) = pair();
    let id = alice.page.blocks()[0].id.clone();
    // Both add a block after the heading, one changes its kind, the other removes it meanwhile.
    let a1 = alice
      .edit(|page| Some(page.insert_block(Some(&id), BlockKind::Bullet, "from Alice").1))
      .unwrap();
    let a2 = alice
      .edit(|page| page.set_kind(&id, BlockKind::Paragraph))
      .unwrap();
    let b1 = bob
      .edit(|page| Some(page.insert_block(Some(&id), BlockKind::Check, "from Bob").1))
      .unwrap();
    let b2 = bob.edit(|page| page.remove_block(&id)).unwrap();
    alice.receive([&b2, &b1]);
    bob.receive([&a1, &a2]);
    assert_eq!(alice.page.blocks(), bob.page.blocks());
    let mut shown = alice.texts();
    shown.sort();
    assert_eq!(
      shown,
      ["from Alice", "from Bob"],
      "the removed heading stays removed"
    );
  }

  #[test]
  fn a_merged_list_and_a_snapshot_hold_the_same_page() {
    let mut alice = CanvasDoc::new();
    let (id, first) = alice.insert_block(None, BlockKind::Paragraph, "draft text");
    let second = alice.set_text(&id, "final").unwrap();
    let merged = merge_updates(&[&first, &second]).unwrap();
    assert_eq!(
      CanvasDoc::from_updates(&[merged]).unwrap().blocks(),
      alice.blocks()
    );
    let snapshot = snapshot(&[&first, &second]).unwrap().unwrap();
    assert_eq!(
      CanvasDoc::from_updates(&[&snapshot]).unwrap().blocks(),
      alice.blocks()
    );
    // A snapshot carries no deleted text.
    assert!(!contains(&snapshot, b"draft text"));
    assert!(contains(
      &merge_updates(&[&first, &second]).unwrap(),
      b"draft text"
    ));
  }

  #[test]
  fn a_page_missing_an_update_waits_and_makes_no_snapshot() {
    let mut alice = CanvasDoc::new();
    let (id, first) = alice.insert_block(None, BlockKind::Paragraph, "a");
    let second = alice.set_text(&id, "ab").unwrap();
    let mut bob = CanvasDoc::from_updates(&[&second]).unwrap();
    assert!(bob.has_missing());
    assert!(snapshot(&[&second]).unwrap().is_none());
    bob.sync(&[&second, &first]).unwrap();
    assert_eq!(texts(&bob), ["ab"]);
    assert!(snapshot(&[&second, &first]).unwrap().is_some());
  }

  /// Three editors make random edits offline, then receive each other's updates in a random
  /// order, in two parts (the first may leave gaps) and with duplicates: every one ends on the
  /// same page, for many seeds.
  #[test]
  fn random_concurrent_edits_converge() {
    for seed in 1..=60_u64 {
      let mut random = Random(seed);
      let mut first = CanvasDoc::new();
      let (_, base) = first.insert_block(None, BlockKind::Heading, "Agenda");
      let mut editors: Vec<Editor> = (0..3).map(|_| Editor::new(std::slice::from_ref(&base))).collect();
      let mut sent: Vec<Vec<Vec<u8>>> = vec![Vec::new(); editors.len()];
      for _ in 0..30 {
        let who = random.below(editors.len());
        if let Some(update) = editors[who].edit(|page| random_edit(page, &mut random)) {
          sent[who].push(update);
        }
      }
      for (who, editor) in editors.iter_mut().enumerate() {
        let mut inbox: Vec<&Vec<u8>> = sent
          .iter()
          .enumerate()
          .filter(|(from, _)| *from != who)
          .flat_map(|(_, updates)| updates.iter())
          .collect();
        inbox.extend(inbox.clone().into_iter().take(3));
        random.shuffle(&mut inbox);
        let split = random.below(inbox.len() + 1);
        editor.receive(inbox[..split].iter().copied());
        editor.receive(inbox[split..].iter().copied());
        assert!(snapshot(&editor.known).unwrap().is_some(), "seed {seed}");
      }
      let expected = editors[0].page.blocks();
      for editor in &editors[1..] {
        assert_eq!(editor.page.blocks(), expected, "seed {seed}");
      }
      let from_snapshot = snapshot(&editors[0].known).unwrap().unwrap();
      assert_eq!(
        CanvasDoc::from_updates(&[from_snapshot]).unwrap().blocks(),
        expected,
        "seed {seed}"
      );
    }
  }

  fn random_edit(page: &mut CanvasDoc, random: &mut Random) -> Option<Vec<u8>> {
    let blocks = page.blocks();
    let maybe_block = (!blocks.is_empty()).then(|| blocks[random.below(blocks.len())].clone());
    let words = ["alpha", "beta", "Grüße", "✓ done", ""];
    let word = words[random.below(words.len())];
    match (random.below(8), maybe_block) {
      (0, _) | (_, None) => {
        let after = blocks.last().map(|block| block.id.clone());
        Some(page.insert_block(after.as_deref(), BlockKind::Paragraph, word).1)
      },
      (5, Some(block)) => {
        let length = u32::try_from(block.text.encode_utf16().count()).unwrap();
        let start = u32::try_from(random.below(length as usize + 1)).unwrap();
        let mark = InlineMark::ALL[random.below(InlineMark::ALL.len())];
        page.toggle_mark(&block.id, start, length, mark)
      },
      (6, Some(block)) => {
        let length = u32::try_from(block.text.encode_utf16().count()).unwrap();
        let start = u32::try_from(random.below(length as usize + 1)).unwrap();
        page
          .split_block(&block.id, start, start, BlockKind::Bullet)
          .map(|(_, update)| update)
      },
      (7, Some(block)) => {
        let length = u32::try_from(block.text.encode_utf16().count()).unwrap();
        page.set_link(&block.id, 0, length, Some("https://example.com/a"))
      },
      (1, Some(block)) => {
        let cut = block.text.char_indices().map(|(at, _)| at).nth(random.below(3));
        let mut text = block.text.clone();
        text.insert_str(cut.unwrap_or(text.len()), word);
        page.set_text(&block.id, &text)
      },
      (2, Some(block)) => page.remove_block(&block.id),
      (3, Some(block)) => page.set_kind(&block.id, BlockKind::ALL[random.below(BlockKind::ALL.len())]),
      (_, Some(block)) => page.set_checked(&block.id, !block.is_checked),
    }
  }

  /// A small deterministic generator (xorshift), so a failing seed can be replayed.
  struct Random(u64);

  impl Random {
    fn below(&mut self, bound: usize) -> usize {
      self.0 ^= self.0 << 13;
      self.0 ^= self.0 >> 7;
      self.0 ^= self.0 << 17;
      (self.0 % bound as u64) as usize
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
      for index in (1..items.len()).rev() {
        items.swap(index, self.below(index + 1));
      }
    }
  }

  fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
  }

  #[test]
  fn garbage_is_refused() {
    let mut page = CanvasDoc::new();
    assert!(page.sync(&[b"not an update"]).is_err());
  }

  #[test]
  fn text_edits_keep_what_both_sides_share() {
    let edit = |old: &str, new: &str| TextEdit::between(old, new);
    assert_eq!(edit("same", "same"), None);
    assert_eq!(
      edit("hello world", "hello brave world"),
      Some(TextEdit {
        start: 6,
        removed: 0,
        inserted_end: 12
      })
    );
    assert_eq!(
      edit("aaa", "aa"),
      Some(TextEdit {
        start: 2,
        removed: 1,
        inserted_end: 2
      })
    );
    // Multi-byte characters stay whole.
    let mut page = CanvasDoc::new();
    let (id, _) = page.insert_block(None, BlockKind::Paragraph, "Grüße");
    page.set_text(&id, "Grüßen ✓");
    page.set_text(&id, "Gr✓üßen");
    assert_eq!(texts(&page), ["Gr✓üßen"]);
  }

  #[test]
  fn the_caret_moves_with_text_before_it() {
    // "Hello |world" with "Hey " typed before the caret by somebody else.
    assert_eq!(shift_caret("Hello world", "Hey Hello world", 6), 10);
    // A change after the caret leaves it.
    assert_eq!(shift_caret("Hello world", "Hello world!", 6), 6);
    // Inside a replaced part: at its end.
    assert_eq!(shift_caret("abcdef", "aXf", 3), 2);
    // Emoji count two UTF-16 units, as text fields count.
    assert_eq!(shift_caret("ab", "😀ab", 1), 3);
  }

  #[test]
  fn typed_prefixes_choose_a_kind() {
    assert_eq!(BlockKind::from_prefix("# Title"), Some((BlockKind::Heading, 2)));
    assert_eq!(BlockKind::from_prefix("[ ] task"), Some((BlockKind::Check, 4)));
    assert_eq!(BlockKind::from_prefix("plain"), None);
    assert_eq!(
      BlockKind::from_prefix("### Title"),
      Some((BlockKind::Heading3, 4))
    );
    assert_eq!(BlockKind::from_prefix("> said"), Some((BlockKind::Quote, 2)));
    assert_eq!(BlockKind::from_prefix("```"), Some((BlockKind::Code, 3)));
    assert_eq!(BlockKind::from_tag("table"), BlockKind::Paragraph);
    assert_eq!(BlockKind::from_tag("quote"), BlockKind::Quote);
  }

  const BOLD: TextStyle = TextStyle {
    is_bold: true,
    is_italic: false,
    is_strike: false,
    is_code: false,
    maybe_link: None,
  };

  fn span(start: usize, end: usize, style: TextStyle) -> TextSpan {
    TextSpan { start, end, style }
  }

  /// A page with one paragraph, its block's ID and the page, under a fake clock that never reaches
  /// the grouping window, so each edit is its own undo step.
  fn undoable(text: &str) -> (CanvasDoc, String) {
    let mut page = CanvasDoc::new();
    let (id, _) = page.insert_block(None, BlockKind::Paragraph, text);
    let ticks = std::sync::atomic::AtomicU64::new(1);
    page.enable_undo(move || ticks.fetch_add(10 * UNDO_GROUP_MS, std::sync::atomic::Ordering::Relaxed));
    (page, id)
  }

  #[test]
  fn formatting_converges_and_survives_older_clients_edits() {
    let (mut alice, mut bob) = pair();
    let id = alice.page.blocks()[0].id.clone();
    let from_alice = alice
      .edit(|page| page.set_mark(&id, 0, 3, InlineMark::Bold, true))
      .unwrap();
    // An editor without inline formatting replaces the text around the formatted part.
    let from_bob = bob.edit(|page| page.set_text(&id, "Agenda!")).unwrap();
    alice.receive([&from_bob]);
    bob.receive([&from_alice]);
    assert_eq!(alice.page.blocks(), bob.page.blocks());
    let block = &alice.page.blocks()[0];
    assert_eq!(block.text, "Agenda!");
    assert_eq!(block.spans, [span(0, 3, BOLD)]);
    assert_eq!(block.runs(), [("Age", Some(&BOLD)), ("nda!", None)]);
  }

  #[test]
  fn typing_takes_the_formatting_before_it_and_replacing_what_it_replaces() {
    let mut page = CanvasDoc::new();
    let (id, _) = page.insert_block(None, BlockKind::Paragraph, "abcd");
    page.set_mark(&id, 1, 3, InlineMark::Bold, true).unwrap();
    assert_eq!(page.blocks()[0].spans, [span(1, 3, BOLD)]);
    // Typing at the end of the bold part continues it.
    page.set_text(&id, "abcXd").unwrap();
    assert_eq!(page.blocks()[0].spans, [span(1, 4, BOLD)]);
    // Before it, it does not.
    page.set_text(&id, "YabcXd").unwrap();
    assert_eq!(page.blocks()[0].spans, [span(2, 5, BOLD)]);
    // Replacing the bold word keeps it bold; replacing the plain end stays plain.
    page.set_text(&id, "YaNEWd").unwrap();
    assert_eq!(page.blocks()[0].spans, [span(2, 5, BOLD)]);
    page.set_text(&id, "YaNEWz").unwrap();
    assert_eq!(page.blocks()[0].text, "YaNEWz");
    assert_eq!(page.blocks()[0].spans, [span(2, 5, BOLD)]);
  }

  #[test]
  fn toggling_a_mark_and_asking_about_it() {
    let mut page = CanvasDoc::new();
    let (id, _) = page.insert_block(None, BlockKind::Paragraph, "Grüße");
    // In UTF-16 units: "Gr" then "üß".
    page.toggle_mark(&id, 2, 4, InlineMark::Italic).unwrap();
    let block = page.blocks().remove(0);
    assert_eq!(block.spans[0].start, 2);
    assert_eq!(&block.text[block.spans[0].start..block.spans[0].end], "üß");
    assert!(block.has_mark(2, 4, InlineMark::Italic));
    assert!(!block.has_mark(1, 4, InlineMark::Italic), "not throughout");
    assert!(
      block.has_mark(4, 4, InlineMark::Italic),
      "typing after it continues it"
    );
    assert!(
      !block.has_mark(2, 2, InlineMark::Italic),
      "typing before it does not"
    );
    // Partly formatted: on for all first, then off.
    page.toggle_mark(&id, 0, 4, InlineMark::Italic).unwrap();
    assert!(page.blocks()[0].has_mark(0, 4, InlineMark::Italic));
    page.toggle_mark(&id, 0, 4, InlineMark::Italic).unwrap();
    assert!(page.blocks()[0].spans.is_empty());
    assert_eq!(
      page.toggle_mark(&id, 3, 3, InlineMark::Bold),
      None,
      "an empty range"
    );
  }

  #[test]
  fn only_web_links_are_stored_or_shown() {
    let mut page = CanvasDoc::new();
    let (id, _) = page.insert_block(None, BlockKind::Paragraph, "docs");
    assert!(page.set_link(&id, 0, 4, Some("javascript:alert(1)")).is_none());
    page.set_link(&id, 0, 4, Some("https://example.com/a")).unwrap();
    let block = page.blocks().remove(0);
    assert_eq!(block.link_at(2), Some("https://example.com/a"));
    page.set_link(&id, 0, 4, None).unwrap();
    assert!(page.blocks()[0].spans.is_empty());
    // A hostile client writes the attribute directly: it is not read as a link.
    {
      let mut txn = page.doc.transact_mut();
      let (_, map) = find(&txn, &page.blocks, &id).unwrap();
      let Some(Out::YText(text)) = map.get(&txn, TEXT) else {
        panic!("a text");
      };
      let attributes = Attrs::from([(Arc::from(LINK), Any::from("javascript:alert(1)"))]);
      text.format(&mut txn, 0, 4, attributes);
    }
    assert!(page.blocks()[0].spans.is_empty());
  }

  #[test]
  fn newer_attributes_and_kinds_stay_untouched() {
    let mut page = CanvasDoc::new();
    let (id, _) = page.insert_block(None, BlockKind::Paragraph, "hello");
    let text = {
      let mut txn = page.doc.transact_mut();
      let (_, map) = find(&txn, &page.blocks, &id).unwrap();
      map.insert(&mut txn, KIND, "table");
      let Some(Out::YText(text)) = map.get(&txn, TEXT) else {
        panic!("a text");
      };
      let attributes = Attrs::from([(Arc::from("mention"), Any::from("user-1"))]);
      text.format(&mut txn, 0, 5, attributes);
      text
    };
    let block = page.blocks().remove(0);
    assert_eq!(block.kind, BlockKind::Paragraph);
    assert!(block.spans.is_empty());
    page.toggle_mark(&id, 0, 5, InlineMark::Bold).unwrap();
    page.toggle_mark(&id, 0, 5, InlineMark::Bold).unwrap();
    page.restore(std::slice::from_ref(&block));
    let txn = page.doc.transact();
    let chunks = text.diff(&txn, YChange::identity);
    assert_eq!(chunks.len(), 1);
    assert_eq!(
      chunks[0]
        .attributes
        .as_ref()
        .and_then(|attributes| attributes.get("mention")),
      Some(&Any::from("user-1"))
    );
    let (_, map) = find(&txn, &page.blocks, &id).unwrap();
    assert_eq!(
      string(&txn, &map, KIND).as_deref(),
      Some("table"),
      "the kind stays"
    );
  }

  #[test]
  fn splitting_and_joining_keep_formatting() {
    let mut page = CanvasDoc::new();
    let (id, _) = page.insert_block(None, BlockKind::Bullet, "abcdef");
    page.set_mark(&id, 1, 5, InlineMark::Bold, true).unwrap();
    let (new_id, _) = page.split_block(&id, 2, 3, BlockKind::Bullet).unwrap();
    let blocks = page.blocks();
    assert_eq!(texts(&page), ["ab", "def"]);
    assert_eq!(blocks[0].spans, [span(1, 2, BOLD)]);
    assert_eq!(blocks[1].spans, [span(0, 2, BOLD)]);
    assert_eq!(blocks[1].kind, BlockKind::Bullet);
    page.join_blocks(&id, &new_id).unwrap();
    let blocks = page.blocks();
    assert_eq!(texts(&page), ["abdef"]);
    assert_eq!(blocks[0].spans, [span(1, 4, BOLD)]);
  }

  #[test]
  fn restoring_brings_formatting_back() {
    let mut page = CanvasDoc::new();
    let (id, _) = page.insert_block(None, BlockKind::Paragraph, "hello world");
    page.set_mark(&id, 0, 5, InlineMark::Bold, true).unwrap();
    let version = page.blocks();
    page.set_mark(&id, 0, 5, InlineMark::Bold, false).unwrap();
    page.set_mark(&id, 6, 11, InlineMark::Strike, true).unwrap();
    page.set_text(&id, "hello there").unwrap();
    page.restore(&version).unwrap();
    assert_eq!(page.blocks(), version);
    // A block that is gone comes back with its formatting.
    page.remove_block(&id).unwrap();
    page.restore(&version).unwrap();
    assert_eq!(page.blocks(), version);
  }

  #[test]
  fn undo_takes_back_own_edits_only_and_goes_to_the_others() {
    let (mut page, id) = undoable("one");
    let mut other = CanvasDoc::from_updates(&[page
      .doc
      .transact()
      .encode_state_as_update_v1(&StateVector::default())])
    .unwrap();
    assert!(!page.can_undo());
    assert_eq!(page.undo(), None, "nothing to take back");
    let typed = page.set_text(&id, "one two").unwrap();
    other.sync(&[&typed]).unwrap();
    // Somebody else adds to the same block.
    let theirs = other.set_text(&id, "one two three").unwrap();
    page.sync(&[&theirs]).unwrap();
    assert_eq!(texts(&page), ["one two three"]);

    let undone = page.undo().unwrap();
    assert_eq!(texts(&page), ["one three"], "only the own edit goes");
    other.sync(&[&undone]).unwrap();
    assert_eq!(other.blocks(), page.blocks());
    assert!(page.can_redo());

    let redone = page.redo().unwrap();
    assert_eq!(texts(&page), ["one two three"]);
    other.sync(&[&redone]).unwrap();
    assert_eq!(other.blocks(), page.blocks());
    assert!(!page.can_redo());
  }

  #[test]
  fn undo_brings_back_formatting_and_removed_blocks() {
    let (mut page, id) = undoable("one");
    page.set_mark(&id, 0, 3, InlineMark::Bold, true).unwrap();
    let (second, _) = page.insert_block(Some(&id), BlockKind::Check, "two");
    page.set_checked(&second, true).unwrap();
    let before = page.blocks();
    page.remove_block(&second).unwrap();
    assert_eq!(texts(&page), ["one"]);
    page.undo().unwrap();
    assert_eq!(
      page.blocks(),
      before,
      "the block is back with its ID, kind and check mark"
    );
    page.undo().unwrap();
    page.undo().unwrap();
    assert_eq!(texts(&page), ["one"]);
    assert!(page.blocks()[0].spans == [span(0, 3, BOLD)]);
    page.undo().unwrap();
    assert!(page.blocks()[0].spans.is_empty());
  }

  #[test]
  fn edits_in_quick_succession_are_undone_together() {
    let mut page = CanvasDoc::new();
    let (id, _) = page.insert_block(None, BlockKind::Paragraph, "");
    page.enable_undo(|| 1_000);
    page.set_text(&id, "a").unwrap();
    page.set_text(&id, "ab").unwrap();
    page.set_text(&id, "abc").unwrap();
    page.undo().unwrap();
    assert_eq!(texts(&page), [""]);
    assert!(!page.can_undo());
  }

  #[test]
  fn changes_name_their_range_and_end() {
    assert_eq!(typed_range("ab", "aXYb"), Some((1, 3)));
    assert_eq!(typed_range("abc", "ac"), None);
    assert_eq!(typed_range("", "😀"), Some((0, 2)));
    assert_eq!(caret_after_change("one two", "one"), 3);
    assert_eq!(caret_after_change("one", "one two"), 7);
    assert_eq!(caret_after_change("one", "X one"), 2);
  }
}
