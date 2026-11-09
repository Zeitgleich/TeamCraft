//! The Files view's page from the store's index of messages with files (issue #1509).
//!
//! Each conversation in scope is a stream of its files, newest (or oldest) first, read from
//! [`LocalStore::attached_messages`] a few messages at a time. The streams are merged by time, so a
//! page reads the messages it shows plus about one per conversation, not every message of the
//! workspace. The page is kept until a message with files is written or removed
//! ([`LocalStore::attachments_cursor`]), so the view reads it again on every change of the app for
//! almost nothing. File names are secret: nothing here logs them.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet, VecDeque};

use teamcraft_client_api::actions::FileInfo;
use teamcraft_client_api::files_query::{
  FileKind, FileScope, FileSort, FilesPage, FilesQuery, StarredFile, conversations_in,
};
use teamcraft_client_api::model::{ConversationId, UserId};
use teamcraft_proto::AttachmentId;

use crate::error::CoreError;
use crate::store::{Attached, AttachedRange, LocalStore, StoredMessage};
use crate::ui_backend::CoreChatBackend;

/// Messages a stream reads first; a conversation whose files are listed reads more each time, up to
/// [`MAX_READ_MESSAGES`]. Small, as every conversation in scope reads once to take its place.
const FIRST_READ_MESSAGES: usize = 2;
const MAX_READ_MESSAGES: usize = 64;

/// The page last read, kept while nothing it comes from changes.
pub(crate) struct KeptFilesPage {
  key: PageKey,
  page: FilesPage,
}

/// What a kept page was read for.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PageKey {
  scope: FileScope,
  kind: FileKind,
  sort: FileSort,
  starred: Vec<StarredFile>,
  max_files: usize,
  conversations: Vec<ConversationId>,
  attachments_cursor: u64,
}

/// One conversation's files in the order of the merge, read a few messages at a time.
struct FileStream {
  conversation: ConversationId,
  /// The last message read: the next read goes on from it.
  maybe_after: Option<teamcraft_client_api::model::MessageId>,
  files: VecDeque<FileInfo>,
  next_read: usize,
  is_done: bool,
}

/// A stream's next file in the merge: the heap's greatest goes first.
#[derive(Debug, PartialEq, Eq)]
struct Head {
  /// The time the merge orders by, negated for oldest first.
  rank: (i64, i64),
  stream: usize,
}

impl<S: LocalStore> CoreChatBackend<S> {
  /// A page of the Files view, through the store's index (issue #1509).
  pub(crate) fn ui_files_page(&self, query: &FilesQuery<'_>) -> FilesPage {
    let conversations = conversations_in(self, query.scope);
    let maybe_key = self
      .store_ref()
      .attachments_cursor()
      .map(|attachments_cursor| PageKey {
        scope: query.scope,
        kind: query.kind,
        sort: query.sort,
        starred: query.starred.to_vec(),
        max_files: query.max_files,
        conversations: conversations.clone(),
        attachments_cursor,
      });
    if let Some(key) = &maybe_key
      && let Some(kept) = self.files_page_kept.borrow().as_ref()
      && kept.key == *key
    {
      return self.with_fresh_transfers(kept.page.clone());
    }
    let _step = crate::timing::step("files_page");
    let page = match query.kind {
      FileKind::Starred => self.starred_page(&conversations, query),
      _ => self.merged_page(conversations, query),
    };
    if let Some(key) = maybe_key {
      *self.files_page_kept.borrow_mut() = Some(KeptFilesPage {
        key,
        page: page.clone(),
      });
    }
    page
  }

  /// The starred files in `conversations`, each looked up by its message (at most
  /// [`teamcraft_client_api::files_query::MAX_STARRED_FILES`] reads).
  fn starred_page(&self, conversations: &[ConversationId], query: &FilesQuery<'_>) -> FilesPage {
    let in_scope: HashSet<ConversationId> = conversations.iter().copied().collect();
    let starred: HashSet<&StarredFile> = query.starred.iter().collect();
    let mut messages_read = HashSet::new();
    let maybe_importer = self.importer();
    let mut files: Vec<FileInfo> = query
      .starred
      .iter()
      .filter(|star| in_scope.contains(&star.0) && messages_read.insert((star.0, star.1)))
      .filter_map(|star| self.listed_message(star.0, star.1))
      .flat_map(|stored| self.files_of(&stored, maybe_importer))
      .filter(|file| starred.contains(&StarredFile::of(file)))
      .collect();
    files.sort_by(|left, right| query.sort.compare(left, right));
    let has_more = files.len() > query.max_files;
    files.truncate(query.max_files);
    FilesPage { files, has_more }
  }

  /// The message with this server ID, when the Files view may list it (one indexed read).
  fn listed_message(
    &self,
    conversation: ConversationId,
    message: teamcraft_client_api::model::MessageId,
  ) -> Option<StoredMessage> {
    self
      .read(
        self
          .store_ref()
          .message_by_server_id(conversation, message)
          .map_err(CoreError::from),
      )
      .filter(|stored| stored.has_attached(Attached::Any))
  }

  /// The newest (or oldest) files of `conversations`, merged by time. Name and size order the
  /// newest files of the page's length (the view says so).
  fn merged_page(&self, conversations: Vec<ConversationId>, query: &FilesQuery<'_>) -> FilesPage {
    let is_oldest_first = query.sort == FileSort::Oldest;
    let maybe_importer = self.importer();
    let mut streams: Vec<FileStream> = conversations
      .into_iter()
      .map(|conversation| FileStream {
        conversation,
        maybe_after: None,
        files: VecDeque::new(),
        next_read: FIRST_READ_MESSAGES,
        is_done: false,
      })
      .collect();
    let mut heads = BinaryHeap::new();
    for (index, stream) in streams.iter_mut().enumerate() {
      self.fill(stream, query.kind, is_oldest_first, maybe_importer);
      heads.extend(head(stream, index, is_oldest_first));
    }
    let mut seen = HashSet::new();
    let mut files = Vec::new();
    // One more than the page tells whether there are more.
    while files.len() <= query.max_files
      && let Some(Head { stream: index, .. }) = heads.pop()
    {
      let stream = &mut streams[index];
      if let Some(file) = stream.files.pop_front()
        && seen.insert(StarredFile::of(&file))
      {
        files.push(file);
      }
      if stream.files.is_empty() {
        self.fill(stream, query.kind, is_oldest_first, maybe_importer);
      }
      heads.extend(head(stream, index, is_oldest_first));
    }
    let has_more = files.len() > query.max_files;
    files.truncate(query.max_files);
    files.sort_by(|left, right| query.sort.compare(left, right));
    FilesPage { files, has_more }
  }

  /// Reads the stream's next messages with files until it has a file of `kind` or is done.
  fn fill(
    &self,
    stream: &mut FileStream,
    kind: FileKind,
    is_oldest_first: bool,
    maybe_importer: Option<UserId>,
  ) {
    let attached = match kind {
      FileKind::Media => Attached::Media,
      FileKind::Documents => Attached::Document,
      FileKind::All | FileKind::Starred => Attached::Any,
    };
    while stream.files.is_empty() && !stream.is_done {
      let range = match is_oldest_first {
        true => AttachedRange::Newer(stream.maybe_after),
        false => AttachedRange::Older(stream.maybe_after),
      };
      let limit = stream.next_read;
      let messages = self.read(
        self
          .store_ref()
          .attached_messages(stream.conversation, attached, range, limit)
          .map_err(CoreError::from),
      );
      stream.is_done = messages.len() < limit;
      stream.next_read = (limit * 2).min(MAX_READ_MESSAGES);
      match messages.last().and_then(|last| last.maybe_server_id) {
        Some(last) => stream.maybe_after = Some(last),
        None => stream.is_done = true,
      }
      stream.files.extend(
        messages
          .iter()
          .flat_map(|stored| self.files_of(stored, maybe_importer))
          .filter(|file| kind.includes(file)),
      );
    }
  }

  /// `page` with where each file's bytes are now: a download moves without a write of a message.
  fn with_fresh_transfers(&self, mut page: FilesPage) -> FilesPage {
    for file in &mut page.files {
      if let Ok(attachment) = file.file_id.parse::<AttachmentId>() {
        file.transfer = self.download_view(attachment, file.size_bytes);
      }
    }
    page
  }
}

impl Ord for Head {
  fn cmp(&self, other: &Self) -> Ordering {
    // Ties go to the stream listed first, so the order doesn't depend on the heap.
    self
      .rank
      .cmp(&other.rank)
      .then_with(|| other.stream.cmp(&self.stream))
  }
}

impl PartialOrd for Head {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

/// The heap entry of the stream's next file, if it has one.
fn head(stream: &FileStream, index: usize, is_oldest_first: bool) -> Option<Head> {
  let file = stream.files.front()?;
  let sent_at = file.maybe_sent_at.unwrap_or(i64::MIN);
  let message = teamcraft_proto::Id::snowflake(file.message).get();
  let rank = match is_oldest_first {
    true => (sent_at.saturating_neg(), message.saturating_neg()),
    false => (sent_at, message),
  };
  Some(Head { rank, stream: index })
}
