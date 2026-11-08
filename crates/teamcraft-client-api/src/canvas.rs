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

mod history;

pub use history::{
  CanvasMark, CanvasOrigin, CanvasVersion, compaction, editors_since, marks, origin_of, page_at, versions,
};
use serde::{Deserialize, Serialize};
use teamcraft_proto::Base64Bytes;
use yrs::updates::decoder::Decode;
use yrs::{
  Any, Array, ArrayRef, Doc, GetString, Map, MapPrelim, MapRef, Out, ReadTxn, StateVector, Text, TextPrelim,
  TextRef, Transact, TransactionMut, Update,
};

/// The root array of blocks.
const BLOCKS: &str = "blocks";
/// The keys of a block's map.
const ID: &str = "id";
const KIND: &str = "kind";
const CHECKED: &str = "checked";
const TEXT: &str = "text";

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
  Bullet,
  Numbered,
  Check,
}

impl BlockKind {
  pub const ALL: [Self; 5] = [
    Self::Paragraph,
    Self::Heading,
    Self::Bullet,
    Self::Numbered,
    Self::Check,
  ];

  /// The block's kind as the document stores it.
  pub fn tag(self) -> &'static str {
    match self {
      Self::Paragraph => "paragraph",
      Self::Heading => "heading",
      Self::Bullet => "bullet",
      Self::Numbered => "numbered",
      Self::Check => "check",
    }
  }

  /// A stored kind; one of a newer version reads as a paragraph.
  pub fn from_tag(tag: &str) -> Self {
    Self::ALL
      .into_iter()
      .find(|kind| kind.tag() == tag)
      .unwrap_or(Self::Paragraph)
  }

  /// A list item: Enter makes another one of the same kind.
  pub fn is_list(self) -> bool {
    matches!(self, Self::Bullet | Self::Numbered | Self::Check)
  }

  /// The kind typed at the start of a paragraph turns it into ("# ", "- ", "1. ", "[] "), with
  /// the length of that prefix in bytes.
  pub fn from_prefix(text: &str) -> Option<(Self, usize)> {
    const PREFIXES: [(&str, BlockKind); 7] = [
      ("# ", BlockKind::Heading),
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
    }
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
      .transact_mut()
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
    let id = format!("{:x}-{:x}", self.doc.client_id().get(), self.next_block);
    self.next_block += 1;
    let mut txn = self.doc.transact_mut();
    let index = match maybe_after {
      Some(after) => find(&txn, &self.blocks, after).map_or(self.blocks.len(&txn), |(index, _)| index + 1),
      None => 0,
    };
    let map = self.blocks.insert(&mut txn, index, MapPrelim::default());
    map.insert(&mut txn, ID, id.as_str());
    map.insert(&mut txn, KIND, kind.tag());
    map.insert(&mut txn, CHECKED, false);
    map.insert(&mut txn, TEXT, TextPrelim::new(text));
    let update = txn.encode_update_v1();
    (id, update)
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
        None => {
          let map = self.blocks.insert(&mut txn, position, MapPrelim::default());
          map.insert(&mut txn, ID, block.id.as_str());
          map.insert(&mut txn, KIND, block.kind.tag());
          map.insert(&mut txn, CHECKED, block.is_checked);
          map.insert(&mut txn, TEXT, TextPrelim::new(block.text.as_str()));
        },
      }
    }
    let length = self.blocks.len(&txn);
    let count = byte_index(target.len());
    if length > count {
      self.blocks.remove_range(&mut txn, count, length - count);
    }
    Some(txn.encode_update_v1())
  }

  /// Whether the page has updates waiting for ones not here yet.
  pub fn has_missing(&self) -> bool {
    let txn = self.doc.transact();
    let store = txn.store();
    store.pending_update().is_some() || store.pending_ds().is_some()
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
    text.insert(txn, start, inserted);
  }
  if edit.removed > 0 {
    text.remove_range(
      txn,
      byte_index(edit.start + inserted.len()),
      byte_index(edit.removed),
    );
  }
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
    },
    _ => {
      map.insert(txn, TEXT, TextPrelim::new(block.text.as_str()));
    },
  }
}

fn read_block<T: ReadTxn>(txn: &T, map: &MapRef) -> Block {
  let text = match map.get(txn, TEXT) {
    Some(Out::YText(text)) => text.get_string(txn),
    _ => String::new(),
  };
  Block {
    id: string(txn, map, ID).unwrap_or_default(),
    kind: BlockKind::from_tag(&string(txn, map, KIND).unwrap_or_default()),
    text,
    is_checked: matches!(map.get(txn, CHECKED), Some(Out::Any(Any::Bool(true)))),
  }
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
    match (random.below(5), maybe_block) {
      (0, _) | (_, None) => {
        let after = blocks.last().map(|block| block.id.clone());
        Some(page.insert_block(after.as_deref(), BlockKind::Paragraph, word).1)
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
    assert_eq!(BlockKind::from_tag("table"), BlockKind::Paragraph);
  }
}
