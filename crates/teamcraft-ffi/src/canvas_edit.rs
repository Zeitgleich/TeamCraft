//! Editing a page of the notes on the phones, and its version history (issue #2361, e2ee.md §4.11):
//! the same editor desktop and web run (`teamcraft_client_api::canvas::PageEditor`), so a phone
//! edits a page by the same rules, sends its edits the same way and makes the same versions.
//!
//! The app holds a [`CanvasPageEditor`] while it shows a page and hands it every edit
//! ([`TeamCraft::canvas_edit`]): typing in a block, Enter, Backspace at a block's start, a block's
//! kind or check mark, adding, moving and removing blocks, undo and redo, restoring a version. The
//! edit applies at once and the answer is the page with it. The edits of a burst of typing go out
//! together: the app calls [`TeamCraft::flush_canvas_edits`] once typing paused
//! ([`CanvasEditOutcome::flush_after_ms`]), when it leaves the page and when it goes to the
//! background; an edit that waited longer than the editor allows goes out with the next one.
//!
//! The editors live on the actions' read thread ([`CanvasEditors`]): the document's undo log isn't
//! `Send`, and every edit is an action there anyway. The app's object only names its editor; it
//! says when it is done with it ([`TeamCraft::close_canvas_editor`]), and the thread keeps at most
//! [`MAX_EDITORS`], sending the waiting edits of one it lets go.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use teamcraft_client_api::backend::ChatBackend as _;
use teamcraft_client_api::canvas::{
  BlockKind, COMPACT_ABOVE, CanvasVersion, FLUSH_IDLE_MS, Focus, Outgoing, PageEditor, Typed, compaction,
  page_at, shift_caret, versions,
};
use teamcraft_client_api::model::OlderHistory;
use teamcraft_client_api::remote::ChatAction;
use teamcraft_core::clock::{Clock as _, SystemClock};
use teamcraft_proto::{Base64Bytes, CanvasPageId, ConversationId, UserId};
use tracing::warn;

use crate::TeamCraft;
use crate::canvas::{
  CanvasBlockItem, CanvasBlockKind, CanvasPageItem, block_items, may_edit, page_key, read_page_of,
};
use crate::engine::parse_id;
use crate::error::TeamCraftError;
use crate::reads::Reads;

/// Editors the actions' thread keeps at most: the app edits one page at a time and closes it when
/// it leaves; beyond (an app that didn't), the least recently used one goes, its edits sent.
const MAX_EDITORS: usize = 8;

/// The next editor's number.
static NEXT_EDITOR: AtomicU64 = AtomicU64::new(1);

/// A page of the notes the phone shows and edits. One per page shown; the app closes it when it
/// leaves the page ([`TeamCraft::close_canvas_editor`]). The editor itself (the page as it has it,
/// the own edits not sent yet, the undo of its edits) is on the actions' thread.
#[derive(uniffi::Object)]
pub struct CanvasPageEditor {
  id: u64,
  conversation: ConversationId,
  maybe_page: Option<CanvasPageId>,
}

/// The phone's editors on the actions' thread, the most recently used last.
#[derive(Default)]
pub(crate) struct CanvasEditors {
  entries: Vec<(Arc<CanvasPageEditor>, PageEditor)>,
}

/// One edit the app makes on the page.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CanvasEdit {
  /// Typing in a block: its text was `base` when the app last took it from the page, it is `text`
  /// now. What another member typed in the block meanwhile stays. "# ", "- ", "1. ", "> " or "[] "
  /// at the start of a paragraph choose its kind.
  Type {
    block_id: String,
    base: String,
    text: String,
  },
  /// Enter at `caret` (UTF-16 units) of `text`, the block's text without the line break: the text
  /// after it becomes a new block of the same list (a paragraph after anything else); in an empty
  /// list item it ends the list. A code block keeps the line break: the app sends it as typing.
  Enter {
    block_id: String,
    base: String,
    text: String,
    caret: u32,
  },
  /// Backspace at a block's start: a list item or heading becomes a paragraph, a paragraph joins
  /// the block above.
  BackspaceAtStart {
    block_id: String,
  },
  /// A new empty block after `maybe_after_id` (at the top without one), the caret in it.
  AddBlock {
    maybe_after_id: Option<String>,
    kind: CanvasBlockKind,
  },
  SetKind {
    block_id: String,
    kind: CanvasBlockKind,
  },
  SetChecked {
    block_id: String,
    is_checked: bool,
  },
  Remove {
    block_id: String,
  },
  MoveUp {
    block_id: String,
  },
  MoveDown {
    block_id: String,
  },
  /// Takes back this editor's last edit (only its own: others' typing stays).
  Undo,
  Redo,
  /// Makes the page `version` again, as one edit on top: typing elsewhere meanwhile survives.
  Restore {
    version: CanvasVersionKey,
  },
}

/// The page after an edit, and what the app does next.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CanvasEditOutcome {
  pub page: CanvasPageItem,
  /// Where the caret goes: the block and the place in its text (UTF-16 units); `None` keeps it.
  pub maybe_focus: Option<CanvasFocusItem>,
  pub can_undo: bool,
  pub can_redo: bool,
  /// Edits wait to go out: the app calls [`TeamCraft::flush_canvas_edits`] this long after the
  /// last edit unless another one comes first; `None` when nothing waits.
  pub flush_after_ms: Option<u32>,
}

/// Where the caret goes after an edit.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CanvasFocusItem {
  pub block_id: String,
  pub caret: u32,
}

/// A version of a page in its history: who made it and the stretch of time of its edits.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CanvasVersionItem {
  pub key: CanvasVersionKey,
  /// Empty for a member this device doesn't know: the app shows "Someone".
  pub display_name: String,
}

/// Names a version for [`TeamCraft::canvas_version_page`] and [`CanvasEdit::Restore`]: who made it
/// and when, which stays the same while newer versions are added on top.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CanvasVersionKey {
  pub user_id: String,
  /// The first and last edit of the version, in milliseconds since the epoch (the server's time).
  pub from_ms: i64,
  pub until_ms: i64,
}

#[uniffi::export]
impl CanvasPageEditor {
  /// An editor of a page of the notes of a conversation: the first page (`maybe_page_id` `None`)
  /// or a further one.
  #[uniffi::constructor]
  pub fn new(conversation_id: String, maybe_page_id: Option<String>) -> Result<Arc<Self>, TeamCraftError> {
    let (conversation, maybe_page) = page_key(&conversation_id, maybe_page_id.as_deref())?;
    Ok(Arc::new(Self {
      id: NEXT_EDITOR.fetch_add(1, Ordering::Relaxed),
      conversation,
      maybe_page,
    }))
  }
}

#[uniffi::export]
impl TeamCraft {
  /// The page the editor shows, with its own edits not sent yet: what the app reads on every
  /// change of the conversation while it edits.
  pub async fn canvas_editor_page(
    &self,
    editor: Arc<CanvasPageEditor>,
  ) -> Result<CanvasPageItem, TeamCraftError> {
    self
      .act_locally(move |reads| {
        CanvasPageEditor::with_editor(&editor, reads, |reads, page| editor.page(reads, page))
      })
      .await
  }

  /// Makes `edit` on the editor's page, and sends the waiting edits when the first of them waited
  /// long enough. Where the user may not edit, nothing changes and the page says so (`can_edit`).
  pub async fn canvas_edit(
    &self,
    editor: Arc<CanvasPageEditor>,
    edit: CanvasEdit,
  ) -> Result<CanvasEditOutcome, TeamCraftError> {
    self
      .act_locally(move |reads| {
        CanvasPageEditor::with_editor(&editor, reads, |reads, page| editor.edit(reads, page, edit))
      })
      .await
  }

  /// Sends the edits waiting in the editor: typing paused, or the app leaves the page or goes to
  /// the background. Hands the core the merge of a long list as well.
  pub async fn flush_canvas_edits(&self, editor: Arc<CanvasPageEditor>) -> Result<(), TeamCraftError> {
    self
      .act_locally(move |reads| {
        CanvasPageEditor::with_editor(&editor, reads, |reads, page| editor.flush(reads, page))
      })
      .await
  }

  /// The app is done with the editor (left the page): its waiting edits go out and it is let go.
  pub async fn close_canvas_editor(&self, editor: Arc<CanvasPageEditor>) -> Result<(), TeamCraftError> {
    self
      .act_locally(move |reads| match reads.canvas_editors.take(&editor) {
        Some(mut page) => editor.flush(reads, &mut page),
        None => Ok(()),
      })
      .await
  }

  /// The versions of a page as this device holds them, newest first (e2ee.md §4.11, "Version
  /// history"): one per author and stretch of editing.
  pub async fn canvas_versions(
    &self,
    conversation_id: String,
    maybe_page_id: Option<String>,
  ) -> Result<Vec<CanvasVersionItem>, TeamCraftError> {
    self
      .read(move |reads| {
        let (conversation, maybe_page) = page_key(&conversation_id, maybe_page_id.as_deref())?;
        let backend = reads.backend()?;
        let stored = backend.canvas(conversation, maybe_page.as_ref());
        let found = versions(&stored).map_err(|error| TeamCraftError::Core(error.to_string()))?;
        Ok(
          found
            .iter()
            .map(|version| CanvasVersionItem {
              key: CanvasVersionKey::of(version),
              display_name: backend
                .user(version.by)
                .map(|user| user.display_name)
                .unwrap_or_default(),
            })
            .collect(),
        )
      })
      .await
  }

  /// The page as it was at `version`, read-only; `None` when this device no longer has that
  /// version (folded into a later one).
  pub async fn canvas_version_page(
    &self,
    conversation_id: String,
    maybe_page_id: Option<String>,
    version: CanvasVersionKey,
  ) -> Result<Option<Vec<CanvasBlockItem>>, TeamCraftError> {
    self
      .read(move |reads| {
        let (conversation, maybe_page) = page_key(&conversation_id, maybe_page_id.as_deref())?;
        let stored = reads.backend()?.canvas(conversation, maybe_page.as_ref());
        let Some(found) = version.find(&stored)? else {
          return Ok(None);
        };
        let blocks = page_at(&stored, &found).map_err(|error| TeamCraftError::Core(error.to_string()))?;
        Ok(Some(block_items(&blocks)))
      })
      .await
  }
}

/// Where a caret at `caret` (UTF-16 units) stands once a block's text changed from `old` to `new`
/// under it (another member typed): it moves with the text before it. A cheap, pure calculation.
#[uniffi::export]
pub fn canvas_caret_after(old: String, new: String, caret: u32) -> u32 {
  shift_caret(&old, &new, caret)
}

impl CanvasPageEditor {
  /// Runs `job` with this editor, made when the thread has none (first use, or let go).
  fn with_editor<R>(
    self: &Arc<Self>,
    reads: &mut Reads,
    job: impl FnOnce(&mut Reads, &mut PageEditor) -> Result<R, TeamCraftError>,
  ) -> Result<R, TeamCraftError> {
    let mut page = reads
      .canvas_editors
      .take(self)
      .unwrap_or_else(|| new_editor(self.maybe_page.as_ref()));
    let result = job(reads, &mut page);
    if let Some((evicted, mut its_page)) = reads.canvas_editors.put(Arc::clone(self), page) {
      warn!(conversation_id = %evicted.conversation, "a page editor the app didn't close was let go");
      evicted.flush(reads, &mut its_page)?;
    }
    result
  }

  fn page(&self, reads: &Reads, editor: &mut PageEditor) -> Result<CanvasPageItem, TeamCraftError> {
    let maybe_page_id = self.maybe_page.as_ref().map(ToString::to_string);
    let mut item = read_page_of(reads, self.conversation, maybe_page_id.as_deref(), Some(editor))?;
    // A page that fell back to the first one isn't this editor's: the app opens an editor for it.
    item.can_edit =
      !item.is_unreadable && item.maybe_page_id == maybe_page_id && may_edit(reads, self.conversation)?;
    Ok(item)
  }

  fn edit(
    &self,
    reads: &mut Reads,
    editor: &mut PageEditor,
    edit: CanvasEdit,
  ) -> Result<CanvasEditOutcome, TeamCraftError> {
    let page = self.page(reads, editor)?;
    if !page.can_edit {
      return Ok(outcome(editor, page, None));
    }
    let maybe_target = match &edit {
      CanvasEdit::Restore { version } => {
        let stored = reads
          .backend()?
          .canvas(self.conversation, self.maybe_page.as_ref());
        match version.find(&stored)? {
          Some(found) => {
            Some(page_at(&stored, &found).map_err(|error| TeamCraftError::Core(error.to_string()))?)
          },
          None => return Ok(outcome(editor, page, None)),
        }
      },
      _ => None,
    };
    let now_ms = SystemClock.now_ms();
    let mut maybe_focus = None;
    editor.edit(now_ms, |doc| {
      let typed = apply(doc, edit, maybe_target.as_deref());
      maybe_focus = typed.maybe_focus;
      typed.updates
    });
    if editor.has_waited_enough(now_ms) {
      self.flush(reads, editor)?;
    }
    let page = self.page(reads, editor)?;
    Ok(outcome(editor, page, maybe_focus))
  }

  /// Hands the waiting edits to the core, and the merge of a long list with the history of what
  /// it merged.
  fn flush(&self, reads: &mut Reads, editor: &mut PageEditor) -> Result<(), TeamCraftError> {
    let backend = reads.backend()?;
    let stored = backend.canvas(self.conversation, self.maybe_page.as_ref());
    let holds_history = backend.older_history(self.conversation) == OlderHistory::Complete;
    editor.sync_if_changed(&stored);
    if let Some(Outgoing {
      update,
      is_snapshot,
      mentions,
    }) = editor.outgoing(&stored, holds_history)
    {
      reads.apply_action(ChatAction::EditCanvas {
        conversation: self.conversation,
        maybe_page: self.maybe_page.clone(),
        update: Base64Bytes::new(update),
        is_snapshot,
        mentions,
      })?;
    }
    self.compact_if_long(reads)
  }

  fn compact_if_long(&self, reads: &mut Reads) -> Result<(), TeamCraftError> {
    let stored = reads
      .backend()?
      .canvas(self.conversation, self.maybe_page.as_ref());
    if stored.updates.len() <= COMPACT_ABOVE {
      return Ok(());
    }
    let (merged, history) = match compaction(&stored) {
      Ok(compacted) => compacted,
      Err(error) => {
        warn!(%error, "the page's updates could not be merged");
        return Ok(());
      },
    };
    reads.apply_action(ChatAction::CompactCanvas {
      conversation: self.conversation,
      maybe_page: stored.maybe_page,
      revision: stored.revision,
      merged: Base64Bytes::new(merged),
      history,
    })
  }
}

impl CanvasEditors {
  /// Takes the app's editor out while it is used.
  fn take(&mut self, editor: &CanvasPageEditor) -> Option<PageEditor> {
    let index = self.entries.iter().position(|(entry, _)| entry.id == editor.id)?;
    Some(self.entries.remove(index).1)
  }

  /// Keeps the app's editor as the most recently used; the one let go beyond [`MAX_EDITORS`].
  fn put(
    &mut self,
    editor: Arc<CanvasPageEditor>,
    page: PageEditor,
  ) -> Option<(Arc<CanvasPageEditor>, PageEditor)> {
    self.entries.push((editor, page));
    (self.entries.len() > MAX_EDITORS).then(|| self.entries.remove(0))
  }
}

fn new_editor(maybe_page: Option<&CanvasPageId>) -> PageEditor {
  let mut editor = PageEditor::new(maybe_page);
  editor
    .page
    .enable_undo(|| u64::try_from(SystemClock.now_ms()).unwrap_or_default());
  editor
}

fn outcome(editor: &PageEditor, page: CanvasPageItem, maybe_focus: Option<Focus>) -> CanvasEditOutcome {
  CanvasEditOutcome {
    page,
    maybe_focus: maybe_focus.map(|focus| CanvasFocusItem {
      block_id: focus.block,
      caret: focus.caret,
    }),
    can_undo: editor.page.can_undo(),
    can_redo: editor.page.can_redo(),
    flush_after_ms: editor.unsent_since_ms().map(|_| FLUSH_IDLE_MS),
  }
}

/// Makes `edit` on the page; `maybe_target`, the blocks of the version a restore names.
fn apply(
  doc: &mut teamcraft_client_api::canvas::CanvasDoc,
  edit: CanvasEdit,
  maybe_target: Option<&[teamcraft_client_api::canvas::Block]>,
) -> Typed {
  let only = |maybe_update: Option<Vec<u8>>| Typed {
    updates: maybe_update.into_iter().collect(),
    maybe_focus: None,
  };
  match edit {
    CanvasEdit::Type { block_id, base, text } => match doc.apply_prefix(&block_id, &text) {
      Some(updates) => Typed {
        updates,
        maybe_focus: Some(Focus {
          block: block_id,
          caret: 0,
        }),
      },
      None => only(doc.type_onto(&block_id, &base, &text)),
    },
    CanvasEdit::Enter {
      block_id,
      base,
      text,
      caret,
    } => {
      let mut updates: Vec<Vec<u8>> = doc.type_onto(&block_id, &base, &text).into_iter().collect();
      let entered = doc.enter(&block_id, caret, caret);
      updates.extend(entered.updates);
      Typed {
        updates,
        maybe_focus: entered.maybe_focus,
      }
    },
    CanvasEdit::BackspaceAtStart { block_id } => doc.backspace_at_start(&block_id).unwrap_or_default(),
    CanvasEdit::AddBlock { maybe_after_id, kind } => {
      let kind = typed_kind(kind);
      let (id, update) = doc.insert_block(maybe_after_id.as_deref(), kind, "");
      Typed {
        updates: vec![update],
        maybe_focus: Some(Focus { block: id, caret: 0 }),
      }
    },
    CanvasEdit::SetKind { block_id, kind } => only(doc.set_kind(&block_id, typed_kind(kind))),
    CanvasEdit::SetChecked { block_id, is_checked } => only(doc.set_checked(&block_id, is_checked)),
    CanvasEdit::Remove { block_id } => only(doc.remove_block(&block_id)),
    CanvasEdit::MoveUp { block_id } => only(move_by(doc, &block_id, -1)),
    CanvasEdit::MoveDown { block_id } => only(move_by(doc, &block_id, 1)),
    CanvasEdit::Undo => only(doc.undo()),
    CanvasEdit::Redo => only(doc.redo()),
    CanvasEdit::Restore { .. } => only(maybe_target.and_then(|target| doc.restore(target))),
  }
}

/// A kind the app may give a block it types into: a file block is made from a file, never chosen.
fn typed_kind(kind: CanvasBlockKind) -> BlockKind {
  match kind {
    CanvasBlockKind::File => BlockKind::Paragraph,
    other => BlockKind::from(other),
  }
}

/// Moves a block one place up (`-1`) or down (`1`), as the desktop editor's Ctrl+Shift+arrows do.
fn move_by(doc: &mut teamcraft_client_api::canvas::CanvasDoc, id: &str, step: isize) -> Option<Vec<u8>> {
  let blocks = doc.blocks();
  let index = blocks.iter().position(|block| block.id == id)?;
  let maybe_after = if step < 0 {
    // Up: after the block two above, or to the top.
    match index.checked_sub(2) {
      Some(above) => Some(blocks[above].id.clone()),
      None if index == 1 => None,
      None => return None,
    }
  } else {
    Some(blocks.get(index + 1)?.id.clone())
  };
  doc.move_block(id, maybe_after.as_deref())
}

impl CanvasVersionKey {
  fn of(version: &CanvasVersion) -> Self {
    Self {
      user_id: version.by.to_string(),
      from_ms: version.from_ms,
      until_ms: version.until_ms,
    }
  }

  /// The version of `stored` this names, if this device still has it.
  fn find(
    &self,
    stored: &teamcraft_client_api::canvas::CanvasState,
  ) -> Result<Option<CanvasVersion>, TeamCraftError> {
    let by: UserId = parse_id(&self.user_id)?;
    let found = versions(stored).map_err(|error| TeamCraftError::Core(error.to_string()))?;
    Ok(found.into_iter().find(|version| {
      version.by == by && version.from_ms == self.from_ms && version.until_ms == self.until_ms
    }))
  }
}

impl From<CanvasBlockKind> for BlockKind {
  fn from(kind: CanvasBlockKind) -> Self {
    match kind {
      CanvasBlockKind::Paragraph => Self::Paragraph,
      CanvasBlockKind::Heading => Self::Heading,
      CanvasBlockKind::Heading2 => Self::Heading2,
      CanvasBlockKind::Heading3 => Self::Heading3,
      CanvasBlockKind::Bullet => Self::Bullet,
      CanvasBlockKind::Numbered => Self::Numbered,
      CanvasBlockKind::Check => Self::Check,
      CanvasBlockKind::Quote => Self::Quote,
      CanvasBlockKind::Code => Self::Code,
      CanvasBlockKind::File => Self::File,
    }
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::canvas::CanvasDoc;

  use super::*;

  #[test]
  fn typing_a_prefix_makes_a_list_item() {
    let mut doc = CanvasDoc::new();
    let (id, _) = doc.insert_block(None, BlockKind::Paragraph, "");

    let typed = apply(
      &mut doc,
      CanvasEdit::Type {
        block_id: id.clone(),
        base: String::new(),
        text: "- milk".to_owned(),
      },
      None,
    );

    assert!(!typed.updates.is_empty());
    assert_eq!(doc.blocks()[0].kind, BlockKind::Bullet);
    assert_eq!(doc.blocks()[0].text, "milk");
    assert_eq!(typed.maybe_focus, Some(Focus { block: id, caret: 0 }));
  }

  #[test]
  fn enter_takes_the_typing_with_it() {
    let mut doc = CanvasDoc::new();
    let (id, _) = doc.insert_block(None, BlockKind::Bullet, "one");

    let typed = apply(
      &mut doc,
      CanvasEdit::Enter {
        block_id: id,
        base: "one".to_owned(),
        text: "onetwo".to_owned(),
        caret: 3,
      },
      None,
    );

    let blocks = doc.blocks();
    assert_eq!(
      blocks.iter().map(|block| block.text.as_str()).collect::<Vec<_>>(),
      ["one", "two"]
    );
    assert_eq!(typed.maybe_focus.unwrap().block, blocks[1].id);
  }

  #[test]
  fn blocks_move_one_place_and_stop_at_the_ends() {
    let mut doc = CanvasDoc::new();
    let (a, _) = doc.insert_block(None, BlockKind::Paragraph, "a");
    let (b, _) = doc.insert_block(Some(&a), BlockKind::Paragraph, "b");
    let (c, _) = doc.insert_block(Some(&b), BlockKind::Paragraph, "c");

    assert!(move_by(&mut doc, &a, -1).is_none());
    assert!(move_by(&mut doc, &c, 1).is_none());
    move_by(&mut doc, &c, -1).unwrap();
    assert_eq!(texts(&doc), ["a", "c", "b"]);
    move_by(&mut doc, &c, -1).unwrap();
    assert_eq!(texts(&doc), ["c", "a", "b"]);
    move_by(&mut doc, &c, 1).unwrap();
    assert_eq!(texts(&doc), ["a", "c", "b"]);
  }

  #[test]
  fn a_file_block_is_never_chosen() {
    let mut doc = CanvasDoc::new();
    let (id, _) = doc.insert_block(None, BlockKind::Paragraph, "x");

    apply(
      &mut doc,
      CanvasEdit::SetKind {
        block_id: id,
        kind: CanvasBlockKind::File,
      },
      None,
    );

    assert_eq!(doc.blocks()[0].kind, BlockKind::Paragraph);
  }

  #[test]
  fn undo_takes_back_the_own_edit() {
    let mut doc = CanvasDoc::new();
    doc.enable_undo(|| 0);
    let (id, _) = doc.insert_block(None, BlockKind::Paragraph, "");
    apply(
      &mut doc,
      CanvasEdit::SetChecked {
        block_id: id,
        is_checked: true,
      },
      None,
    );

    let undone = apply(&mut doc, CanvasEdit::Undo, None);

    assert!(!undone.updates.is_empty());
  }

  #[test]
  fn an_editor_refuses_a_page_id_beyond_the_rules() {
    assert!(CanvasPageEditor::new("7".to_owned(), Some("../x".to_owned())).is_err());
    assert!(CanvasPageEditor::new("7".to_owned(), None).is_ok());
  }

  fn texts(doc: &CanvasDoc) -> Vec<String> {
    doc.blocks().into_iter().map(|block| block.text).collect()
  }
}
