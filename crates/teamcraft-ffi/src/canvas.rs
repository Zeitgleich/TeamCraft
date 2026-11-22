//! A channel's shared notes for the phone apps (issue #1456, e2ee.md §4.11): a page read-only, as the
//! blocks the desktop and web editor shows. The core keeps a page's updates as opaque bytes; this
//! reads them with the client API's document (`teamcraft_client_api::canvas`), so the apps show the
//! same page every other client builds from the same updates and need no CRDT of their own.

use teamcraft_client_api::backend::ChatBackend as _;
use teamcraft_client_api::canvas::{Block, BlockKind, CanvasDoc, TextStyle, numbered_position};
use teamcraft_proto::{CanvasPageId, ConversationId};
use tracing::warn;

use crate::TeamCraft;
use crate::engine::parse_id;
use crate::error::TeamCraftError;
use crate::reads::Reads;

/// A page of a channel's notes as the phone shows it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CanvasPageItem {
  /// The page shown: `None` for the channel's first page. A further page the first page doesn't
  /// list (deleted, or not known here yet) is answered with the first page.
  pub maybe_page_id: Option<String>,
  /// The further pages the first page lists, in its order (at most 50, the same on every device).
  pub pages: Vec<CanvasPageEntryItem>,
  /// The blocks in page order.
  pub blocks: Vec<CanvasBlockItem>,
  /// Some of the page's updates wait for others this device doesn't hold yet (older history not
  /// read, or still on its way): the page may lack text that others see.
  pub is_incomplete: bool,
  /// The page's updates couldn't be read: the app says so instead of showing an empty page.
  pub is_unreadable: bool,
}

/// A further page as the first page lists it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CanvasPageEntryItem {
  pub page_id: String,
  /// Empty for a page nobody named: the app shows "Untitled".
  pub title: String,
}

/// One block of a page.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CanvasBlockItem {
  /// Stable across edits and devices: what the app keys its rows by.
  pub id: String,
  pub kind: CanvasBlockKind,
  /// The text in stretches of one formatting each, in order; together the block's whole text. A
  /// code block has one plain run.
  pub runs: Vec<CanvasRunItem>,
  pub is_checked: bool,
  /// The number a numbered item shows: its place in the run of numbered items it ends; 0 for
  /// other kinds.
  pub number: u32,
  /// The channel's file a file block shows; `None` for other kinds, and for a file block whose
  /// reference isn't one (another member's client stored something else).
  pub maybe_file: Option<CanvasFileItem>,
}

/// What a block is shown as. A kind of a newer version reads as a paragraph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CanvasBlockKind {
  Paragraph,
  Heading,
  Heading2,
  Heading3,
  Bullet,
  Numbered,
  Check,
  Quote,
  Code,
  File,
}

/// A stretch of a block's text with one formatting.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CanvasRunItem {
  pub text: String,
  pub is_bold: bool,
  pub is_italic: bool,
  pub is_strike: bool,
  pub is_code: bool,
  /// A web address (`http://` or `https://`) only: whatever else a client stored is no link.
  pub maybe_link: Option<String>,
}

/// A file of the channel as a page names it. The page holds no key and no copy: the app shows it
/// from the message it was sent with (its conversation's files), and only if it holds that message.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CanvasFileItem {
  pub message_id: String,
  /// The server's attachment ID (`FileInfo::file_id`).
  pub file_id: String,
}

#[uniffi::export]
impl TeamCraft {
  /// A page of a channel's shared notes, read-only: the first page (`maybe_page_id` `None`) or a
  /// further one. Read afresh on every change of the conversation; while it is shown, the app reads
  /// the channel's older history as for the chat (the page's edits are messages in it).
  pub async fn canvas_page(
    &self,
    conversation_id: String,
    maybe_page_id: Option<String>,
  ) -> Result<CanvasPageItem, TeamCraftError> {
    self
      .read(move |reads| read_page(reads, &conversation_id, maybe_page_id.as_deref()))
      .await
  }
}

fn read_page(
  reads: &Reads,
  conversation_id: &str,
  maybe_page_id: Option<&str>,
) -> Result<CanvasPageItem, TeamCraftError> {
  let conversation: ConversationId = parse_id(conversation_id)?;
  let backend = reads.backend()?;
  let first = backend.canvas(conversation, None);
  let Ok(first_doc) = CanvasDoc::from_updates(&first.updates) else {
    warn!(conversation_id = %conversation, "the notes' first page could not be read");
    return Ok(CanvasPageItem::unreadable(None));
  };
  let pages = first_doc.pages();
  // Only a page the list names: an ID the app kept from before a deletion falls back to the first.
  let maybe_page = maybe_page_id
    .and_then(CanvasPageId::parse)
    .filter(|page| pages.iter().any(|entry| entry.id == *page));
  let entries = pages
    .into_iter()
    .map(|entry| CanvasPageEntryItem {
      page_id: entry.id.to_string(),
      title: entry.title,
    })
    .collect();
  let Some(page) = maybe_page else {
    return Ok(page_item(None, entries, &first_doc));
  };
  let stored = backend.canvas(conversation, Some(&page));
  // A core from before further pages answers with the first page; it never is one here.
  if stored.maybe_page.as_ref() != Some(&page) {
    return Ok(page_item(None, entries, &first_doc));
  }
  match CanvasDoc::from_page_updates(Some(&page), &stored.updates) {
    Ok(doc) => Ok(page_item(Some(&page), entries, &doc)),
    Err(_) => {
      warn!(conversation_id = %conversation, "a page of the notes could not be read");
      Ok(CanvasPageItem {
        pages: entries,
        ..CanvasPageItem::unreadable(Some(&page))
      })
    },
  }
}

fn page_item(
  maybe_page: Option<&CanvasPageId>,
  pages: Vec<CanvasPageEntryItem>,
  doc: &CanvasDoc,
) -> CanvasPageItem {
  CanvasPageItem {
    maybe_page_id: maybe_page.map(ToString::to_string),
    pages,
    blocks: block_items(&doc.blocks()),
    is_incomplete: doc.has_missing(),
    is_unreadable: false,
  }
}

/// The blocks as the app shows them, numbered lists counted as on desktop and web.
fn block_items(blocks: &[Block]) -> Vec<CanvasBlockItem> {
  blocks
    .iter()
    .enumerate()
    .map(|(index, block)| CanvasBlockItem {
      id: block.id.clone(),
      kind: block.kind.into(),
      runs: runs_of(block),
      is_checked: block.is_checked,
      number: if block.kind == BlockKind::Numbered {
        u32::try_from(numbered_position(blocks, index)).unwrap_or(u32::MAX)
      } else {
        0
      },
      maybe_file: block.maybe_file.as_ref().map(|file| CanvasFileItem {
        message_id: file.message.to_string(),
        file_id: file.file_id.clone(),
      }),
    })
    .collect()
}

/// Code has no inline formatting (the editor shows its text as it is).
fn runs_of(block: &Block) -> Vec<CanvasRunItem> {
  if block.kind.is_code() {
    return vec![CanvasRunItem::plain(&block.text)];
  }
  block
    .runs()
    .into_iter()
    .map(|(text, maybe_style)| match maybe_style {
      Some(style) => CanvasRunItem::styled(text, style),
      None => CanvasRunItem::plain(text),
    })
    .collect()
}

impl CanvasPageItem {
  fn unreadable(maybe_page: Option<&CanvasPageId>) -> Self {
    Self {
      maybe_page_id: maybe_page.map(ToString::to_string),
      pages: Vec::new(),
      blocks: Vec::new(),
      is_incomplete: false,
      is_unreadable: true,
    }
  }
}

impl CanvasRunItem {
  fn plain(text: &str) -> Self {
    Self::styled(text, &TextStyle::default())
  }

  fn styled(text: &str, style: &TextStyle) -> Self {
    Self {
      text: text.to_owned(),
      is_bold: style.is_bold,
      is_italic: style.is_italic,
      is_strike: style.is_strike,
      is_code: style.is_code,
      maybe_link: style.maybe_link.clone(),
    }
  }
}

impl From<BlockKind> for CanvasBlockKind {
  fn from(kind: BlockKind) -> Self {
    match kind {
      BlockKind::Paragraph => Self::Paragraph,
      BlockKind::Heading => Self::Heading,
      BlockKind::Heading2 => Self::Heading2,
      BlockKind::Heading3 => Self::Heading3,
      BlockKind::Bullet => Self::Bullet,
      BlockKind::Numbered => Self::Numbered,
      BlockKind::Check => Self::Check,
      BlockKind::Quote => Self::Quote,
      BlockKind::Code => Self::Code,
      BlockKind::File => Self::File,
    }
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::canvas::{FileRef, InlineMark};

  use super::*;

  #[test]
  fn numbered_items_count_per_run() {
    let mut doc = CanvasDoc::new();
    let first = insert(&mut doc, None, BlockKind::Numbered, "one");
    let second = insert(&mut doc, Some(&first), BlockKind::Numbered, "two");
    let gap = insert(&mut doc, Some(&second), BlockKind::Paragraph, "between");
    insert(&mut doc, Some(&gap), BlockKind::Numbered, "again");

    let numbers: Vec<u32> = block_items(&doc.blocks())
      .iter()
      .map(|block| block.number)
      .collect();

    assert_eq!(numbers, [1, 2, 0, 1]);
  }

  #[test]
  fn runs_carry_formatting_and_cover_the_text() {
    let mut doc = CanvasDoc::new();
    let id = insert(&mut doc, None, BlockKind::Paragraph, "plain bold end");
    doc.toggle_mark(&id, 6, 10, InlineMark::Bold).unwrap();

    let block = &block_items(&doc.blocks())[0];

    let texts: Vec<&str> = block.runs.iter().map(|run| run.text.as_str()).collect();
    assert_eq!(texts, ["plain ", "bold", " end"]);
    assert!(block.runs[1].is_bold);
    assert!(!block.runs[0].is_bold && !block.runs[2].is_bold);
  }

  #[test]
  fn code_blocks_are_one_plain_run() {
    let mut doc = CanvasDoc::new();
    let id = insert(&mut doc, None, BlockKind::Code, "let x = 1;");
    doc.toggle_mark(&id, 0, 3, InlineMark::Bold).unwrap();

    let block = &block_items(&doc.blocks())[0];

    assert_eq!(block.kind, CanvasBlockKind::Code);
    assert_eq!(block.runs, [CanvasRunItem::plain("let x = 1;")]);
  }

  #[test]
  fn only_web_links_are_links() {
    let mut doc = CanvasDoc::new();
    let id = insert(&mut doc, None, BlockKind::Paragraph, "site other");
    doc.set_link(&id, 0, 4, Some("https://example.com")).unwrap();
    // This client refuses any other scheme; what another client stored is held to the same rule
    // when the page is read (`TextStyle::from_attributes`).
    assert!(doc.set_link(&id, 5, 10, Some("javascript:alert(1)")).is_none());

    let block = &block_items(&doc.blocks())[0];

    assert_eq!(block.runs[0].maybe_link.as_deref(), Some("https://example.com"));
    assert!(block.runs.iter().skip(1).all(|run| run.maybe_link.is_none()));
  }

  #[test]
  fn file_blocks_name_their_message_and_file() {
    let mut doc = CanvasDoc::new();
    let file = FileRef {
      message: "42".parse().unwrap(),
      file_id: "7".to_owned(),
    };
    doc.insert_file(None, file, "plan.pdf");

    let block = &block_items(&doc.blocks())[0];

    assert_eq!(block.kind, CanvasBlockKind::File);
    assert_eq!(
      block.maybe_file,
      Some(CanvasFileItem {
        message_id: "42".to_owned(),
        file_id: "7".to_owned(),
      })
    );
    assert_eq!(block.runs[0].text, "plan.pdf");
  }

  #[test]
  fn checked_items_stay_checked() {
    let mut doc = CanvasDoc::new();
    let id = insert(&mut doc, None, BlockKind::Check, "done");
    doc.set_checked(&id, true).unwrap();

    assert!(block_items(&doc.blocks())[0].is_checked);
  }

  /// Adds a block of `kind` with `text` after `maybe_after`; its ID.
  fn insert(doc: &mut CanvasDoc, maybe_after: Option<&str>, kind: BlockKind, text: &str) -> String {
    doc.insert_block(maybe_after, kind, text).0
  }
}
