//! A channel's shared notes for the phone apps (issue #1456, e2ee.md §4.11): a page read-only, as the
//! blocks the desktop and web editor shows. The core keeps a page's updates as opaque bytes; this
//! reads them with the client API's document (`teamcraft_client_api::canvas`), so the apps show the
//! same page every other client builds from the same updates and need no CRDT of their own.
//!
//! While a page is shown the app also learns who else has it open and says that it has
//! ([`TeamCraft::canvas_presence`]): the editor's presence without a caret, paced here
//! ([`CanvasPresencePacer`]) so the apps only ask on their tick and say when they leave.

use std::collections::{BTreeSet, HashMap};

use teamcraft_client_api::backend::ChatBackend as _;
use teamcraft_client_api::canvas::{
  Block, BlockKind, CanvasDoc, CanvasState, EDITING_WINDOW_MS, TextStyle, editors_since, numbered_position,
};
use teamcraft_client_api::model::{CanvasPeer, CanvasPresenceNote};
use teamcraft_client_api::remote::ChatAction;
use teamcraft_core::canvas_presence::CanvasPresencePacer;
use teamcraft_core::clock::{Clock as _, SystemClock};
use teamcraft_proto::{CanvasPageId, CanvasPageState, ConversationId, UserId};
use tracing::warn;

use crate::TeamCraft;
use crate::engine::parse_id;
use crate::error::TeamCraftError;
use crate::reads::Reads;

/// The most pages whose pacing is kept: the app shows one at a time, and says when it leaves it.
/// Beyond (an app that didn't say so), all are forgotten and a page shown again says it at once.
const MAX_PACED_PAGES: usize = 8;

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
  /// The user this stretch mentions (issue #2361), in decimal: the app shows it as a chip.
  /// Defaulted, so runs the apps build need no change.
  #[uniffi(default = None)]
  pub maybe_mention: Option<String>,
}

/// Who else has a page open, and who edited it lately without having it open: the line under the
/// page's title ("Anna has this page open · Ben is editing"), as desktop and web show it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CanvasPresenceItem {
  /// The other members whose devices have the page open now, each once.
  pub open_by: Vec<CanvasPersonItem>,
  /// The other members who edited the page in the last two minutes and don't have it open, the
  /// latest first (a phone, a client from before presence).
  pub editing: Vec<CanvasPersonItem>,
}

/// A member named in [`CanvasPresenceItem`]; members this device doesn't know are left out.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CanvasPersonItem {
  pub user_id: String,
  pub display_name: String,
}

/// When each page the app shows says it is open, on the main read thread.
#[derive(Debug, Default)]
pub(crate) struct PagePacers {
  pacers: HashMap<PageKey, CanvasPresencePacer>,
}

/// A page of a conversation's notes: `None` is the first page.
type PageKey = (ConversationId, Option<CanvasPageId>);

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

  /// Who else has a page of the notes open and who edited it lately, and, when it is due, tells
  /// the other members connected now that this device has it open (e2ee.md §4.11, "Who has the
  /// page open"; read-only, so without a caret). The app asks while the page is shown: when it
  /// opens, every 10 s while the app is in the foreground, and whenever the core changed.
  /// `is_shared` is the setting "Show when I'm typing": off, this device says nothing.
  pub async fn canvas_presence(
    &self,
    conversation_id: String,
    maybe_page_id: Option<String>,
    is_shared: bool,
  ) -> Result<CanvasPresenceItem, TeamCraftError> {
    let (item, maybe_note) = self
      .read(move |reads| presence_of(reads, &conversation_id, maybe_page_id.as_deref(), is_shared))
      .await?;
    if let Some((conversation, note)) = maybe_note {
      self.say_canvas(conversation, note).await?;
    }
    Ok(item)
  }

  /// The app no longer shows the page (closed it, switched to another page, went to the
  /// background): the others drop this device at once instead of after 50 s.
  pub async fn leave_canvas_page(
    &self,
    conversation_id: String,
    maybe_page_id: Option<String>,
    is_shared: bool,
  ) -> Result<(), TeamCraftError> {
    let key = self
      .read(move |reads| {
        let key = page_key(&conversation_id, maybe_page_id.as_deref())?;
        reads.canvas_pacers.forget(&key);
        Ok(key)
      })
      .await?;
    if !is_shared {
      return Ok(());
    }
    let (conversation, maybe_page) = key;
    let note = CanvasPresenceNote {
      maybe_page,
      state: CanvasPageState::Closed,
      maybe_caret: None,
    };
    self.say_canvas(conversation, note).await
  }
}

impl TeamCraft {
  /// Hands a word about the notes to the engine, which seals and sends it on the open connection
  /// (dropped offline: a late word would be wrong).
  async fn say_canvas(
    &self,
    conversation: ConversationId,
    note: CanvasPresenceNote,
  ) -> Result<(), TeamCraftError> {
    self
      .act_locally(move |reads| reads.apply_action(ChatAction::AnnounceCanvas { conversation, note }))
      .await
  }
}

impl PagePacers {
  /// Whether the app's page says it is open now, `peers` having it open at `now_ms`.
  fn is_due(&mut self, key: PageKey, peers: &[CanvasPeer], now_ms: i64) -> bool {
    if !self.pacers.contains_key(&key) && self.pacers.len() >= MAX_PACED_PAGES {
      self.pacers.clear();
    }
    self.pacers.entry(key).or_default().is_due(peers, now_ms)
  }

  /// The page is left: shown again, it says so at once.
  fn forget(&mut self, key: &PageKey) {
    self.pacers.remove(key);
  }
}

/// [`TeamCraft::canvas_presence`] on the read thread: the line, and the word to say if one is due.
fn presence_of(
  reads: &mut Reads,
  conversation_id: &str,
  maybe_page_id: Option<&str>,
  is_shared: bool,
) -> Result<(CanvasPresenceItem, Option<(ConversationId, CanvasPresenceNote)>), TeamCraftError> {
  let (conversation, maybe_page) = page_key(conversation_id, maybe_page_id)?;
  let now_ms = SystemClock.now_ms();
  let backend = reads.backend()?;
  let peers = backend.canvas_peers(conversation, maybe_page.as_ref());
  let stored = backend.canvas(conversation, maybe_page.as_ref());
  let (open_by, editing) = present_users(&peers, &stored, backend.me().id, now_ms);
  let person = |user: UserId| {
    backend.user(user).map(|found| CanvasPersonItem {
      user_id: user.to_string(),
      display_name: found.display_name,
    })
  };
  let item = CanvasPresenceItem {
    open_by: open_by.into_iter().filter_map(person).collect(),
    editing: editing.into_iter().filter_map(person).collect(),
  };
  let is_due = is_shared
    && reads
      .canvas_pacers
      .is_due((conversation, maybe_page.clone()), &peers, now_ms);
  let maybe_note = is_due.then(|| {
    let note = CanvasPresenceNote {
      maybe_page,
      state: CanvasPageState::Open,
      maybe_caret: None,
    };
    (conversation, note)
  });
  Ok((item, maybe_note))
}

/// The conversation and page the app names; a page ID beyond the rules is refused.
fn page_key(conversation_id: &str, maybe_page_id: Option<&str>) -> Result<PageKey, TeamCraftError> {
  let conversation: ConversationId = parse_id(conversation_id)?;
  let maybe_page = maybe_page_id
    .map(|id| CanvasPageId::parse(id).ok_or_else(|| TeamCraftError::InvalidInput("not a page ID".into())))
    .transpose()?;
  Ok((conversation, maybe_page))
}

/// Who has the page open (`peers`, never `me`), each once, and who else of the others edited
/// `stored` lately, the latest first.
fn present_users(
  peers: &[CanvasPeer],
  stored: &CanvasState,
  me: UserId,
  now_ms: i64,
) -> (Vec<UserId>, Vec<UserId>) {
  let open: BTreeSet<UserId> = peers.iter().map(|peer| peer.user).collect();
  let editing = editors_since(stored, now_ms.saturating_sub(EDITING_WINDOW_MS), me)
    .into_iter()
    .filter(|user| !open.contains(user))
    .collect();
  (open.into_iter().collect(), editing)
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
      maybe_mention: style.maybe_mention.map(|user| user.to_string()),
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

  mod presence {
    use teamcraft_client_api::canvas::CanvasOrigin;
    use teamcraft_proto::{Base64Bytes, DeviceId};

    use super::*;

    #[test]
    fn names_each_member_with_the_page_open_once() {
      let peers = [peer(2, 20), peer(2, 21), peer(3, 30)];

      let (open_by, editing) = present_users(&peers, &CanvasState::default(), user(1), 0);

      assert_eq!(open_by, [user(2), user(3)]);
      assert!(editing.is_empty());
    }

    #[test]
    fn recent_editors_without_the_page_open_are_editing() {
      let now = 10 * 60 * 1000;
      let stored = edited_by(&[
        (2, now - 1_000),
        (4, now - 2_000),
        (5, now - EDITING_WINDOW_MS - 1),
        (1, now),
      ]);

      let (open_by, editing) = present_users(&[peer(2, 20)], &stored, user(1), now);

      assert_eq!(open_by, [user(2)]);
      // 2 has it open, 5 edited too long ago, 1 is me.
      assert_eq!(editing, [user(4)]);
    }

    #[test]
    fn a_page_left_says_it_again_at_once_when_shown_again() {
      let mut pacers = PagePacers::default();
      let key = (conversation(7), None);
      assert!(pacers.is_due(key.clone(), &[], 0));
      assert!(!pacers.is_due(key.clone(), &[], 5_000));
      pacers.forget(&key);
      assert!(pacers.is_due(key, &[], 6_000));
    }

    #[test]
    fn pacing_stays_bounded() {
      let mut pacers = PagePacers::default();
      for id in 0..(MAX_PACED_PAGES as i64 + 3) {
        pacers.is_due((conversation(100 + id), None), &[], 0);
      }
      assert!(pacers.pacers.len() <= MAX_PACED_PAGES);
    }

    #[test]
    fn a_page_id_beyond_the_rules_is_refused() {
      assert!(page_key("7", Some("../x")).is_err());
      assert_eq!(page_key("7", None).unwrap(), (conversation(7), None));
    }

    fn edited_by(edits: &[(i64, i64)]) -> CanvasState {
      CanvasState {
        updates: edits.iter().map(|_| Base64Bytes::new(vec![0])).collect(),
        origins: edits
          .iter()
          .map(|(by, at_ms)| {
            Some(CanvasOrigin {
              by: user(*by),
              at_ms: *at_ms,
              is_snapshot: false,
            })
          })
          .collect(),
        ..CanvasState::default()
      }
    }

    fn peer(user_id: i64, device_id: i64) -> CanvasPeer {
      CanvasPeer {
        user: user(user_id),
        device: device_id.to_string().parse::<DeviceId>().unwrap(),
        maybe_caret: None,
        heard_at_ms: 0,
      }
    }

    fn user(id: i64) -> UserId {
      id.to_string().parse().unwrap()
    }

    fn conversation(id: i64) -> ConversationId {
      id.to_string().parse().unwrap()
    }
  }

  /// Adds a block of `kind` with `text` after `maybe_after`; its ID.
  fn insert(doc: &mut CanvasDoc, maybe_after: Option<&str>, kind: BlockKind, text: &str) -> String {
    doc.insert_block(maybe_after, kind, text).0
  }
}
