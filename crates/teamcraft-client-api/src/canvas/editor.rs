//! A page being edited (e2ee.md §4.11): the rules every editor of a page follows, shared by the
//! desktop and web editor (`teamcraft-ui`) and the phones (`teamcraft-ffi`), so a page is edited
//! the same way everywhere.
//!
//! - [`PageEditor`] holds the page and the own edits the core doesn't hold yet, and decides what
//!   goes out: the edits of a burst of typing merged into one update, the whole page as a snapshot
//!   when one is due, and whom a mention notifies.
//! - The typing rules on [`CanvasDoc`]: a prefix typed at the start of a paragraph choosing its
//!   kind, Enter, Backspace at a block's start, and typing that started from an older text of the
//!   block (a phone's text field while another member typed in the same block).

use std::collections::BTreeSet;

use teamcraft_proto::{CanvasPageId, UserId};

use super::{Block, BlockKind, CanvasDoc, CanvasState, TextEdit, merge_updates, snapshot};

/// How long typing pauses before the edits go out, in milliseconds.
pub const FLUSH_IDLE_MS: u32 = 600;
/// The longest an edit waits while typing goes on, in milliseconds.
pub const FLUSH_MAX_WAIT_MS: i64 = 2_000;
/// Updates since the last snapshot after which an edit is sent as the whole page instead.
pub const SNAPSHOT_EVERY: u32 = 50;
/// The largest snapshot sent: with base64 and encryption well below the server's 1 MiB message.
pub const MAX_SNAPSHOT_BYTES: usize = 512 * 1024;
/// Updates the core holds beyond which the editor hands it their merge ([`super::compaction`]).
pub const COMPACT_ABOVE: usize = 32;

/// The page as one editor edits it.
pub struct PageEditor {
  pub page: CanvasDoc,
  /// Own updates the core doesn't hold yet.
  unsent: Vec<Vec<u8>>,
  /// When the first of them was made, in the editor's clock (milliseconds).
  maybe_unsent_since_ms: Option<i64>,
  /// Members mentioned with the picker since the last edit went out (issue #2361): those still on
  /// the page are notified with it. A mention undone, redone or restored is never in here.
  picked_mentions: BTreeSet<UserId>,
  /// The revision of the core's list the page was last brought up to ([`Self::sync_if_changed`]).
  maybe_synced: Option<u64>,
}

/// What goes to the core: an update, whether it is the whole page, and whom it notifies.
#[derive(Debug)]
pub struct Outgoing {
  pub update: Vec<u8>,
  pub is_snapshot: bool,
  pub mentions: Vec<UserId>,
}

/// Where the caret goes after a typing rule: a block and a place in its text (UTF-16 units).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Focus {
  pub block: String,
  pub caret: u32,
}

/// What a typing rule did: the updates it made and where the caret goes, if it moves.
#[derive(Debug, Default)]
pub struct Typed {
  pub updates: Vec<Vec<u8>>,
  pub maybe_focus: Option<Focus>,
}

impl PageEditor {
  pub fn new(maybe_page: Option<&CanvasPageId>) -> Self {
    Self {
      page: CanvasDoc::for_page(maybe_page),
      unsent: Vec::new(),
      maybe_unsent_since_ms: None,
      picked_mentions: BTreeSet::new(),
      maybe_synced: None,
    }
  }

  /// Brings the page up to the core's updates and the own ones not sent yet.
  pub fn sync(&mut self, stored: &CanvasState) -> Vec<Block> {
    let mut all: Vec<&[u8]> = stored.updates.iter().map(AsRef::as_ref).collect();
    all.extend(self.unsent.iter().map(Vec::as_slice));
    if let Err(error) = self.page.sync(&all) {
      tracing::warn!(%error, "a page update was not applied");
    }
    self.maybe_synced = Some(stored.revision);
    self.page.blocks()
  }

  /// [`Self::sync`] only when the core's list changed since: an editor asked on every keystroke
  /// doesn't merge the whole list each time.
  pub fn sync_if_changed(&mut self, stored: &CanvasState) {
    if self.maybe_synced != Some(stored.revision) {
      self.sync(stored);
    }
  }

  /// Makes an edit of the page; whether it changed anything. `now_ms` starts the wait of the
  /// first edit not sent yet.
  pub fn edit(&mut self, now_ms: i64, change: impl FnOnce(&mut CanvasDoc) -> Vec<Vec<u8>>) -> bool {
    let updates = change(&mut self.page);
    if updates.is_empty() {
      return false;
    }
    self.unsent.extend(updates);
    self.maybe_unsent_since_ms.get_or_insert(now_ms);
    true
  }

  /// A member picked in the mention picker: notified with the next edit if still on the page.
  pub fn pick_mention(&mut self, user: UserId) {
    self.picked_mentions.insert(user);
  }

  /// When the first edit not sent yet was made; `None` when all went out.
  pub fn unsent_since_ms(&self) -> Option<i64> {
    self.maybe_unsent_since_ms
  }

  /// Whether the waiting edits waited long enough to go out while typing goes on.
  pub fn has_waited_enough(&self, now_ms: i64) -> bool {
    self
      .maybe_unsent_since_ms
      .is_some_and(|since| now_ms.saturating_sub(since) >= FLUSH_MAX_WAIT_MS)
  }

  /// What goes to the core: the edits since the last time merged, or the whole page when a
  /// snapshot is due and this device holds the history it needs. An edit that mentions somebody is
  /// never a snapshot, which would notify nobody: the snapshot waits for the next edit.
  pub fn outgoing(&mut self, stored: &CanvasState, holds_history: bool) -> Option<Outgoing> {
    if self.unsent.is_empty() {
      return None;
    }
    let unsent = std::mem::take(&mut self.unsent);
    self.maybe_unsent_since_ms = None;
    let picked = std::mem::take(&mut self.picked_mentions);
    let mentions: Vec<UserId> = if picked.is_empty() {
      Vec::new()
    } else {
      picked
        .intersection(&self.page.mentioned_users())
        .copied()
        .collect()
    };
    let is_due = stored.updates_since_snapshot.saturating_add(1) >= SNAPSHOT_EVERY;
    if holds_history && is_due && mentions.is_empty() {
      let mut all: Vec<&[u8]> = stored.updates.iter().map(AsRef::as_ref).collect();
      all.extend(unsent.iter().map(Vec::as_slice));
      if let Ok(Some(whole)) = snapshot(&all)
        && whole.len() <= MAX_SNAPSHOT_BYTES
      {
        return Some(Outgoing {
          update: whole,
          is_snapshot: true,
          mentions,
        });
      }
    }
    match merge_updates(&unsent) {
      Ok(merged) => Some(Outgoing {
        update: merged,
        is_snapshot: false,
        mentions,
      }),
      Err(error) => {
        tracing::warn!(%error, "own page edits could not be merged");
        None
      },
    }
  }
}

impl CanvasDoc {
  /// "# ", "- ", "1. ", "> " or "[] " typed at the start of a paragraph choose its kind; the
  /// updates when `value` (the block's text as typed) starts with one, `None` otherwise.
  pub fn apply_prefix(&mut self, id: &str, value: &str) -> Option<Vec<Vec<u8>>> {
    if self.block(id)?.kind != BlockKind::Paragraph {
      return None;
    }
    let (kind, prefix_len) = BlockKind::from_prefix(value)?;
    let is_checked = value.starts_with("[x] ");
    let updates = [
      self.set_kind(id, kind),
      self.set_text(id, &value[prefix_len..]),
      is_checked.then(|| self.set_checked(id, true)).flatten(),
    ];
    Some(updates.into_iter().flatten().collect())
  }

  /// Typing in a block whose text was `base` when the typing started, which is `text` now: the
  /// change from `base` to `text`, made on the block as it is. What another member typed in the
  /// block meanwhile stays; the change lands where its place moved to.
  pub fn type_onto(&mut self, id: &str, base: &str, text: &str) -> Option<Vec<u8>> {
    let current = self.block(id)?.text;
    if current == base {
      return self.set_text(id, text);
    }
    let own = TextEdit::between(base, text)?;
    let theirs = TextEdit::between(base, &current);
    let start = moved_by(theirs.as_ref(), own.start);
    let end = moved_by(theirs.as_ref(), own.start + own.removed).max(start);
    let merged = format!(
      "{}{}{}",
      &current[..start],
      &text[own.start..own.inserted_end],
      &current[end..]
    );
    self.set_text(id, &merged)
  }

  /// Enter with `start..end` (UTF-16 units) selected: the text after it becomes a new block of the
  /// same list or quote (a paragraph after anything else), the caret at its start; Enter in an
  /// empty list item or quote ends the list instead.
  pub fn enter(&mut self, id: &str, start: u32, end: u32) -> Typed {
    let Some(block) = self.block(id) else {
      return Typed::default();
    };
    if block.kind.continues_on_enter() && block.text.is_empty() {
      return Typed {
        updates: self.set_kind(id, BlockKind::Paragraph).into_iter().collect(),
        maybe_focus: None,
      };
    }
    let next_kind = if block.kind.continues_on_enter() {
      block.kind
    } else {
      BlockKind::Paragraph
    };
    match self.split_block(id, start, end, next_kind) {
      Some((new_id, update)) => Typed {
        updates: vec![update],
        maybe_focus: Some(Focus {
          block: new_id,
          caret: 0,
        }),
      },
      None => Typed::default(),
    }
  }

  /// Backspace at a block's start: a list item, quote or heading becomes a paragraph first, a
  /// paragraph joins the block above (the caret where the two meet); a file above takes the focus
  /// instead. `None` when it does nothing (the first block, a block that's gone).
  pub fn backspace_at_start(&mut self, id: &str) -> Option<Typed> {
    let blocks = self.blocks();
    let index = blocks.iter().position(|block| block.id == id)?;
    let kind = blocks[index].kind;
    let maybe_previous = index.checked_sub(1).map(|previous| &blocks[previous]);
    match maybe_previous {
      // Text doesn't join a file: the file block above takes the focus.
      Some(previous) if previous.kind.is_embed() => Some(Typed {
        updates: Vec::new(),
        maybe_focus: Some(Focus {
          block: previous.id.clone(),
          caret: 0,
        }),
      }),
      _ if kind != BlockKind::Paragraph => Some(Typed {
        updates: self.set_kind(id, BlockKind::Paragraph).into_iter().collect(),
        maybe_focus: None,
      }),
      Some(previous) => {
        let caret = utf16_len(&previous.text);
        let into = previous.id.clone();
        Some(Typed {
          updates: self.join_blocks(&into, id).into_iter().collect(),
          maybe_focus: Some(Focus { block: into, caret }),
        })
      },
      None => None,
    }
  }

  /// The block with `id` as shown, if it is there.
  fn block(&self, id: &str) -> Option<Block> {
    self.blocks().into_iter().find(|block| block.id == id)
  }
}

/// Where a place (bytes) of a block's text stands once `maybe_change` was made to it.
fn moved_by(maybe_change: Option<&TextEdit>, at: usize) -> usize {
  let Some(change) = maybe_change else {
    return at;
  };
  if at <= change.start {
    at
  } else if at >= change.start + change.removed {
    at - change.removed + (change.inserted_end - change.start)
  } else {
    change.inserted_end
  }
}

fn utf16_len(text: &str) -> u32 {
  u32::try_from(text.encode_utf16().count()).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
  use super::*;

  mod outgoing {
    use super::*;

    #[test]
    fn edits_go_out_merged_and_a_snapshot_when_due() {
      let mut editor = PageEditor::new(None);
      let mut id = String::new();
      editor.edit(0, |page| {
        let (new_id, first) = page.insert_block(None, BlockKind::Paragraph, "a");
        id = new_id;
        vec![first]
      });
      editor.edit(10, |page| page.set_text(&id, "ab").into_iter().collect());
      assert_eq!(editor.unsent_since_ms(), Some(0));

      let Outgoing {
        update, is_snapshot, ..
      } = editor.outgoing(&CanvasState::default(), true).unwrap();
      assert!(!is_snapshot);
      assert_eq!(
        CanvasDoc::from_updates(std::slice::from_ref(&update))
          .unwrap()
          .blocks()[0]
          .text,
        "ab"
      );
      assert!(editor.outgoing(&CanvasState::default(), true).is_none());
      assert_eq!(editor.unsent_since_ms(), None);

      editor.edit(20, |page| page.set_text(&id, "abc").into_iter().collect());
      let due = CanvasState {
        updates: vec![update.into()],
        updates_since_snapshot: SNAPSHOT_EVERY,
        ..CanvasState::default()
      };
      let whole = editor.outgoing(&due, true).unwrap();
      assert!(whole.is_snapshot);
      assert_eq!(
        CanvasDoc::from_updates(&[whole.update]).unwrap().blocks()[0].text,
        "abc"
      );

      // Without the history it needs, a device sends no snapshot.
      editor.edit(30, |page| page.set_text(&id, "abcd").into_iter().collect());
      assert!(!editor.outgoing(&due, false).unwrap().is_snapshot);
    }

    #[test]
    fn a_mention_goes_out_once_with_its_edit_never_as_a_snapshot() {
      let mut editor = PageEditor::new(None);
      let mut id = String::new();
      editor.edit(0, |page| {
        let (new_id, first) = page.insert_block(None, BlockKind::Paragraph, "@a @b");
        id = new_id;
        let anna = page.insert_mention(&id, 0, 2, user(7), "Anna").unwrap();
        let text = page.blocks()[0].text.clone();
        let at = u32::try_from(text.encode_utf16().count()).unwrap();
        let ben = page.insert_mention(&id, at - 2, at, user(8), "Ben").unwrap();
        // Ben is taken out again before the edit goes out.
        let at = utf16_len(&page.blocks()[0].text) - 1;
        let (start, end) = page.blocks()[0].mention_before(at).unwrap();
        let gone = page.remove_text(&id, start, end).unwrap();
        vec![first, anna, ben, gone]
      });
      editor.pick_mention(user(7));
      editor.pick_mention(user(8));
      let due = CanvasState {
        updates_since_snapshot: SNAPSHOT_EVERY,
        ..CanvasState::default()
      };

      let out = editor.outgoing(&due, true).unwrap();

      assert!(!out.is_snapshot, "the snapshot waits");
      assert_eq!(out.mentions, vec![user(7)]);
      editor.edit(10, |page| page.set_text(&id, "x").into_iter().collect());
      assert!(editor.outgoing(&due, true).unwrap().mentions.is_empty());
    }

    #[test]
    fn waiting_edits_go_out_after_the_longest_wait() {
      let mut editor = PageEditor::new(None);
      assert!(!editor.has_waited_enough(i64::MAX));
      editor.edit(1_000, |page| {
        vec![page.insert_block(None, BlockKind::Paragraph, "a").1]
      });
      assert!(!editor.has_waited_enough(1_000 + FLUSH_MAX_WAIT_MS - 1));
      assert!(editor.has_waited_enough(1_000 + FLUSH_MAX_WAIT_MS));
    }

    #[test]
    fn an_edit_that_changes_nothing_waits_for_nothing() {
      let mut editor = PageEditor::new(None);
      assert!(!editor.edit(0, |_| Vec::new()));
      assert_eq!(editor.unsent_since_ms(), None);
    }

    #[test]
    fn syncs_only_when_the_list_changed() {
      let mut other = CanvasDoc::new();
      let (_, update) = other.insert_block(None, BlockKind::Paragraph, "theirs");
      let mut editor = PageEditor::new(None);
      let stored = CanvasState {
        revision: 3,
        updates: vec![update.into()],
        ..CanvasState::default()
      };
      editor.sync_if_changed(&stored);
      assert_eq!(editor.page.blocks()[0].text, "theirs");
      // The same revision isn't read again: a list that changed without it (never) wouldn't show.
      let unchanged = CanvasState {
        revision: 3,
        ..CanvasState::default()
      };
      editor.sync_if_changed(&unchanged);
      assert_eq!(editor.page.blocks().len(), 1);
    }
  }

  mod typing {
    use super::*;

    #[test]
    fn a_prefix_at_the_start_of_a_paragraph_chooses_its_kind() {
      let mut page = CanvasDoc::new();
      let (id, _) = page.insert_block(None, BlockKind::Paragraph, "");

      assert!(page.apply_prefix(&id, "[x] done").is_some());

      let block = &page.blocks()[0];
      assert_eq!(block.kind, BlockKind::Check);
      assert!(block.is_checked);
      assert_eq!(block.text, "done");
      // Only a paragraph: a list item keeps its "- ".
      assert!(page.apply_prefix(&id, "- x").is_none());
    }

    #[test]
    fn enter_continues_a_list_and_ends_it_when_empty() {
      let mut page = CanvasDoc::new();
      let (id, _) = page.insert_block(None, BlockKind::Bullet, "onetwo");

      let typed = page.enter(&id, 3, 3);

      let blocks = page.blocks();
      assert_eq!(blocks.len(), 2);
      assert_eq!((blocks[0].text.as_str(), blocks[1].text.as_str()), ("one", "two"));
      assert_eq!(blocks[1].kind, BlockKind::Bullet);
      let focus = typed.maybe_focus.unwrap();
      assert_eq!((focus.block.as_str(), focus.caret), (blocks[1].id.as_str(), 0));

      let (empty, _) = page.insert_block(Some(&blocks[1].id), BlockKind::Bullet, "");
      let ended = page.enter(&empty, 0, 0);
      assert!(ended.maybe_focus.is_none());
      assert_eq!(page.blocks()[2].kind, BlockKind::Paragraph);
    }

    #[test]
    fn enter_after_a_heading_makes_a_paragraph() {
      let mut page = CanvasDoc::new();
      let (id, _) = page.insert_block(None, BlockKind::Heading, "Title");

      page.enter(&id, 5, 5);

      assert_eq!(page.blocks()[1].kind, BlockKind::Paragraph);
    }

    #[test]
    fn backspace_at_the_start_unlists_then_joins() {
      let mut page = CanvasDoc::new();
      let (first, _) = page.insert_block(None, BlockKind::Paragraph, "ab");
      let (second, _) = page.insert_block(Some(&first), BlockKind::Bullet, "cd");

      let unlisted = page.backspace_at_start(&second).unwrap();
      assert!(unlisted.maybe_focus.is_none());
      assert_eq!(page.blocks()[1].kind, BlockKind::Paragraph);

      let joined = page.backspace_at_start(&second).unwrap();
      assert_eq!(
        joined.maybe_focus,
        Some(Focus {
          block: first,
          caret: 2
        })
      );
      assert_eq!(page.blocks().len(), 1);
      assert_eq!(page.blocks()[0].text, "abcd");
    }

    #[test]
    fn backspace_in_the_first_paragraph_does_nothing() {
      let mut page = CanvasDoc::new();
      let (id, _) = page.insert_block(None, BlockKind::Paragraph, "a");
      assert!(page.backspace_at_start(&id).is_none());
    }

    #[test]
    fn typing_from_an_older_text_keeps_what_others_typed() {
      let mut page = CanvasDoc::new();
      let (id, _) = page.insert_block(None, BlockKind::Paragraph, "hello world");
      // Another member typed at the start while this one typed at the end.
      page.set_text(&id, "Oh, hello world").unwrap();

      page.type_onto(&id, "hello world", "hello world!").unwrap();

      assert_eq!(page.blocks()[0].text, "Oh, hello world!");
    }

    #[test]
    fn typing_from_an_older_text_lands_where_its_place_moved() {
      let mut page = CanvasDoc::new();
      let (id, _) = page.insert_block(None, BlockKind::Paragraph, "a b");
      page.set_text(&id, "xx a b").unwrap();

      // "a b" -> "a, b": the comma goes after the "a", wherever it is now.
      page.type_onto(&id, "a b", "a, b").unwrap();

      assert_eq!(page.blocks()[0].text, "xx a, b");
    }

    #[test]
    fn typing_from_the_current_text_is_a_plain_edit() {
      let mut page = CanvasDoc::new();
      let (id, _) = page.insert_block(None, BlockKind::Paragraph, "ab");
      page.type_onto(&id, "ab", "abc").unwrap();
      assert_eq!(page.blocks()[0].text, "abc");
      assert!(page.type_onto(&id, "abc", "abc").is_none());
      assert!(page.type_onto("gone", "", "x").is_none());
    }

    #[test]
    fn typing_after_text_with_wide_characters_stays_on_character_boundaries() {
      let mut page = CanvasDoc::new();
      let (id, _) = page.insert_block(None, BlockKind::Paragraph, "äö");
      page.set_text(&id, "😀äö").unwrap();

      page.type_onto(&id, "äö", "äöü").unwrap();

      assert_eq!(page.blocks()[0].text, "😀äöü");
    }
  }

  fn user(id: i64) -> UserId {
    id.to_string().parse().unwrap()
  }
}
