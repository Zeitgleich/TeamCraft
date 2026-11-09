//! The list of the Files view (issue #301): the files the user can see, filtered by kind, sorted,
//! with the starred ones, a page at a time. Shared by the clients so every one lists the same.
//!
//! It runs on the device over what the device already holds; the server is asked nothing and learns
//! nothing about what is looked at. File names are secret: nothing here logs them.
//!
//! A backend reads the page through [`ChatBackend::files_page`]. The client core pages through an
//! index of the messages with files, merging the conversations newest first, so a page reads what it
//! shows (issue #1509); sorting by name or size orders the newest files loaded so far. The default
//! here, for backends without such an index, takes each conversation's newest files
//! ([`crate::space_scope::MAX_FILES_PER_CONVERSATION`] of them), so the work stays bounded by the
//! number of conversations, not by their history. Starred files are looked up in their own
//! conversations, so a starred file older than that window is still listed.

use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::actions::FileInfo;
use crate::backend::ChatBackend;
use crate::model::{ConversationId, MessageId, SpaceId};
use crate::space_scope::{files_of_everything, files_of_space};

/// Files a page of the Files view adds.
pub const FILES_PAGE_SIZE: usize = 60;
/// How many files one account can star. The stars follow the account in its encrypted settings,
/// which have a size limit of their own (`MAX_SYNCED_SETTINGS_BYTES`); 100 stars take about 7 KiB.
pub const MAX_STARRED_FILES: usize = 100;

/// One page of the Files view: the files to show, and whether there are more.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilesPage {
  pub files: Vec<FileInfo>,
  pub has_more: bool,
}

/// What the Files view asks for.
#[derive(Clone, Copy, Debug)]
pub struct FilesQuery<'a> {
  pub scope: FileScope,
  pub kind: FileKind,
  pub sort: FileSort,
  /// The account's starred files.
  pub starred: &'a [StarredFile],
  /// How many files to return at most (a whole number of pages).
  pub max_files: usize,
}

/// Whose files: those of all conversations the user is in, or of one space's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileScope {
  Everything,
  Space(SpaceId),
}

/// Which files the list shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
  All,
  /// Pictures and videos.
  Media,
  /// Everything that isn't a picture or video.
  Documents,
  Starred,
}

/// The order of the list; remembered per account.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileSort {
  #[default]
  Newest,
  Oldest,
  /// By file name, A to Z.
  Name,
  Largest,
  /// An order a newer client knows; sorts newest first here.
  #[serde(other)]
  Unknown,
}

/// A file the user starred: the message it belongs to and the file within it. Encoded as a short
/// array, since the stars share the size limit of the synced settings.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct StarredFile(pub ConversationId, pub MessageId, pub String);

impl FileKind {
  pub const ALL: [Self; 4] = [Self::All, Self::Media, Self::Documents, Self::Starred];

  fn matches(self, file: &FileInfo, starred: &HashSet<StarredFile>) -> bool {
    match self {
      Self::Starred => starred.contains(&StarredFile::of(file)),
      _ => self.includes(file),
    }
  }

  /// Whether `file` is of this kind; every file counts as starred here (the stars decide that).
  pub fn includes(self, file: &FileInfo) -> bool {
    match self {
      Self::All | Self::Starred => true,
      Self::Media => is_media(file),
      Self::Documents => !is_media(file),
    }
  }
}

impl FileSort {
  pub const ALL: [Self; 4] = [Self::Newest, Self::Oldest, Self::Name, Self::Largest];

  /// Whether the order goes by time. The others order the newest files loaded so far (#1509).
  pub fn is_by_time(self) -> bool {
    matches!(self, Self::Newest | Self::Oldest | Self::Unknown)
  }

  pub fn compare(self, left: &FileInfo, right: &FileInfo) -> Ordering {
    let newest_first = || Reverse(left.maybe_sent_at).cmp(&Reverse(right.maybe_sent_at));
    match self {
      Self::Newest | Self::Unknown => newest_first(),
      Self::Oldest => left.maybe_sent_at.cmp(&right.maybe_sent_at),
      Self::Name => name_key(left).cmp(&name_key(right)).then_with(newest_first),
      Self::Largest => right.size_bytes.cmp(&left.size_bytes).then_with(newest_first),
    }
  }
}

impl StarredFile {
  pub fn of(file: &FileInfo) -> Self {
    Self(file.conversation, file.message, file.file_id.clone())
  }
}

/// The conversations whose files `scope` lists: every one the user is in, or one space's.
pub fn conversations_in(backend: &(impl ChatBackend + ?Sized), scope: FileScope) -> Vec<ConversationId> {
  let spaces: Vec<SpaceId> = match scope {
    FileScope::Space(space) => vec![space],
    FileScope::Everything => backend.spaces().into_iter().map(|space| space.id).collect(),
  };
  let mut conversations: Vec<ConversationId> = match scope {
    FileScope::Space(_) => Vec::new(),
    FileScope::Everything => backend
      .direct_conversations()
      .into_iter()
      .map(|conversation| conversation.id)
      .collect(),
  };
  for space in spaces {
    conversations.extend(
      backend
        .space_conversations(space)
        .into_iter()
        .map(|conversation| conversation.id),
    );
  }
  conversations
}

/// A page of the files `query` asks for, from what `backend` holds, by reading each conversation's
/// newest files (the default of [`ChatBackend::files_page`]).
pub fn files_page(backend: &(impl ChatBackend + ?Sized), query: &FilesQuery<'_>) -> FilesPage {
  let candidates = match query.kind {
    FileKind::Starred => starred_candidates(backend, query.scope, query.starred),
    _ => match query.scope {
      FileScope::Everything => files_of_everything(backend),
      FileScope::Space(space) => files_of_space(backend, space),
    },
  };
  select_files(candidates, query)
}

/// The files of `candidates` that match `query`, sorted, at most `query.max_files` of them.
pub fn select_files(candidates: Vec<FileInfo>, query: &FilesQuery<'_>) -> FilesPage {
  let starred: HashSet<StarredFile> = query.starred.iter().cloned().collect();
  let mut seen = HashSet::new();
  let mut files: Vec<FileInfo> = candidates
    .into_iter()
    .filter(|file| query.kind.matches(file, &starred))
    // A file is listed once, also when two sources found it.
    .filter(|file| seen.insert(StarredFile::of(file)))
    .collect();
  files.sort_by(|left, right| query.sort.compare(left, right));
  let has_more = files.len() > query.max_files;
  files.truncate(query.max_files);
  FilesPage { files, has_more }
}

/// `starred` with `file` starred or not, as `is_starred` says. Refuses (returns `None`) to star one
/// more once [`MAX_STARRED_FILES`] are starred; unstarring always works.
pub fn toggle_star(
  starred: &[StarredFile],
  file: &StarredFile,
  is_starred: bool,
) -> Option<Vec<StarredFile>> {
  let mut next: Vec<StarredFile> = starred.iter().filter(|star| *star != file).cloned().collect();
  if is_starred {
    if next.len() >= MAX_STARRED_FILES {
      return None;
    }
    next.push(file.clone());
  }
  Some(next)
}

/// The starred files, read from their own conversations (only those in `scope`).
fn starred_candidates(
  backend: &(impl ChatBackend + ?Sized),
  scope: FileScope,
  starred: &[StarredFile],
) -> Vec<FileInfo> {
  let mut conversations: BTreeSet<ConversationId> = starred.iter().map(|star| star.0).collect();
  if let FileScope::Space(space) = scope {
    let in_space: HashSet<ConversationId> = backend
      .space_conversations(space)
      .into_iter()
      .map(|conversation| conversation.id)
      .collect();
    conversations.retain(|conversation| in_space.contains(conversation));
  }
  conversations
    .into_iter()
    .flat_map(|conversation| backend.conversation_files(conversation))
    .collect()
}

fn is_media(file: &FileInfo) -> bool {
  let media_type = file.media_type.trim().to_ascii_lowercase();
  media_type.starts_with("image/") || media_type.starts_with("video/")
}

fn name_key(file: &FileInfo) -> String {
  file.file_name.to_lowercase()
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::actions::FileTransfer;
  use crate::model::id_from_raw;

  const PICTURE: &str = "image/png";
  const DOCUMENT: &str = "application/pdf";

  mod filters {
    use super::*;

    #[test]
    fn kinds_split_pictures_and_videos_from_documents() {
      let candidates = vec![
        file(1, "a.png", PICTURE, 10, 1),
        file(2, "b.pdf", DOCUMENT, 10, 2),
      ];
      let names = |kind| {
        names(&select_files(
          candidates.clone(),
          &query(kind, FileSort::Newest, &[], 10),
        ))
      };
      assert_eq!(names(FileKind::All), ["b.pdf", "a.png"]);
      assert_eq!(names(FileKind::Media), ["a.png"]);
      assert_eq!(names(FileKind::Documents), ["b.pdf"]);
    }

    #[test]
    fn upper_case_media_types_count_as_media() {
      let candidates = vec![file(1, "a.jpg", " IMAGE/JPEG", 10, 1)];
      let page = select_files(candidates, &query(FileKind::Media, FileSort::Newest, &[], 10));
      assert_eq!(names(&page), ["a.jpg"]);
    }

    #[test]
    fn starred_shows_only_the_starred_file_of_a_message() {
      let mut other = file(1, "b.pdf", DOCUMENT, 10, 1);
      other.file_id = "2".into();
      let starred_one = file(1, "a.pdf", DOCUMENT, 10, 1);
      let stars = [StarredFile::of(&starred_one)];
      let page = select_files(
        vec![starred_one, other],
        &query(FileKind::Starred, FileSort::Newest, &stars, 10),
      );
      assert_eq!(names(&page), ["a.pdf"]);
    }

    #[test]
    fn a_file_found_twice_is_listed_once() {
      let one = file(1, "a.pdf", DOCUMENT, 10, 1);
      let page = select_files(
        vec![one.clone(), one],
        &query(FileKind::All, FileSort::Newest, &[], 10),
      );
      assert_eq!(page.files.len(), 1);
    }
  }

  mod sorting {
    use super::*;

    #[test]
    fn every_order_sorts_as_named() {
      let candidates = vec![
        file(1, "beta.pdf", DOCUMENT, 300, 2),
        file(2, "Alpha.pdf", DOCUMENT, 100, 3),
        file(3, "gamma.pdf", DOCUMENT, 200, 1),
      ];
      let names = |sort| {
        names(&select_files(
          candidates.clone(),
          &query(FileKind::All, sort, &[], 10),
        ))
      };
      assert_eq!(names(FileSort::Newest), ["Alpha.pdf", "beta.pdf", "gamma.pdf"]);
      assert_eq!(names(FileSort::Oldest), ["gamma.pdf", "beta.pdf", "Alpha.pdf"]);
      assert_eq!(names(FileSort::Name), ["Alpha.pdf", "beta.pdf", "gamma.pdf"]);
      assert_eq!(names(FileSort::Largest), ["beta.pdf", "gamma.pdf", "Alpha.pdf"]);
      assert_eq!(names(FileSort::Unknown), names(FileSort::Newest));
    }

    #[test]
    fn sort_names_are_stable_on_the_wire() {
      assert_eq!(serde_json::to_string(&FileSort::Largest).unwrap(), "\"largest\"");
      assert_eq!(
        serde_json::from_str::<FileSort>("\"by_colour\"").unwrap(),
        FileSort::Unknown
      );
    }
  }

  mod paging {
    use super::*;

    #[test]
    fn a_page_stops_at_its_size_and_says_there_is_more() {
      let candidates: Vec<FileInfo> = (1..=5).map(|n| file(n, "f.pdf", DOCUMENT, 1, n as i64)).collect();
      let page = select_files(
        candidates.clone(),
        &query(FileKind::All, FileSort::Newest, &[], 3),
      );
      assert_eq!(page.files.len(), 3);
      assert!(page.has_more);
      let whole = select_files(candidates, &query(FileKind::All, FileSort::Newest, &[], 5));
      assert!(!whole.has_more);
    }
  }

  mod stars {
    use super::*;

    #[test]
    fn starring_adds_once_and_unstarring_removes() {
      let star = StarredFile(id_from_raw(1), id_from_raw(2), "3".into());
      let once = toggle_star(&[], &star, true).unwrap();
      assert_eq!(toggle_star(&once, &star, true).unwrap(), once);
      assert!(toggle_star(&once, &star, false).unwrap().is_empty());
    }

    #[test]
    fn starring_stops_at_the_limit_but_unstarring_does_not() {
      let full: Vec<StarredFile> = (1..=MAX_STARRED_FILES as u64)
        .map(|n| StarredFile(id_from_raw(1), id_from_raw(n), "1".into()))
        .collect();
      let new = StarredFile(id_from_raw(1), id_from_raw(9_999), "1".into());
      assert!(toggle_star(&full, &new, true).is_none());
      assert_eq!(
        toggle_star(&full, &full[0], false).unwrap().len(),
        MAX_STARRED_FILES - 1
      );
    }

    #[test]
    fn stars_encode_as_short_arrays() {
      let star = StarredFile(id_from_raw(1), id_from_raw(2), "3".into());
      assert_eq!(serde_json::to_string(&star).unwrap(), r#"["1","2","3"]"#);
    }

    #[test]
    fn a_full_set_of_stars_fits_well_inside_the_synced_settings() {
      let big = u64::MAX >> 11;
      let full: Vec<StarredFile> = (0..MAX_STARRED_FILES as u64)
        .map(|n| StarredFile(id_from_raw(big - n), id_from_raw(big - n), (big - n).to_string()))
        .collect();
      let bytes = serde_json::to_vec(&full).unwrap().len();
      assert!(
        bytes < teamcraft_proto::synced_settings::MAX_SYNCED_SETTINGS_BYTES / 4,
        "{bytes}"
      );
    }
  }

  fn query(kind: FileKind, sort: FileSort, starred: &[StarredFile], max_files: usize) -> FilesQuery<'_> {
    FilesQuery {
      scope: FileScope::Everything,
      kind,
      sort,
      starred,
      max_files,
    }
  }

  fn names(page: &FilesPage) -> Vec<String> {
    page.files.iter().map(|file| file.file_name.clone()).collect()
  }

  fn file(message: u64, name: &str, media_type: &str, size_bytes: u64, sent_at: i64) -> FileInfo {
    FileInfo {
      conversation: id_from_raw(7),
      message: id_from_raw(message),
      file_id: "1".into(),
      file_name: name.into(),
      media_type: media_type.into(),
      size_bytes,
      maybe_thumbnail_png: None,
      transfer: FileTransfer::Remote,
      maybe_sender: None,
      maybe_sent_at: Some(sent_at),
      maybe_imported_author: None,
      maybe_dimensions: None,
      maybe_duration_ms: None,
      maybe_waveform: None,
      maybe_content_sha256: None,
    }
  }
}
