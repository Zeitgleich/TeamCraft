//! What the image viewer pages through and where it stands (#245, #274, #240): one message's album
//! or every picture of the conversation, read a page at a time around the opened one, oldest first,
//! the ends not wrapping around. Shared by the desktop and web lightbox (`teamcraft-ui`) and the
//! Android and iOS viewers (through `teamcraft-ffi`), so all of them page the same way, show albums
//! the same way and zoom within the same limits.
//!
//! No I/O: [`MediaViewer`] says which page of the conversation it needs next
//! ([`MediaViewer::next_read`]); the caller reads it from the core
//! ([`crate::backend::ChatBackend::conversation_media`]) and hands it back.

use teamcraft_proto::AttachmentId;

use crate::actions::{FileInfo, MediaAnchor, MediaPage};
use crate::model::{ConversationId, MessageId};

/// Messages (with pictures or videos) read at a time: around the opened one when the viewer opens,
/// then a page further out whenever the user comes within [`PAGE_MARGIN`] pictures of an end
/// (#274). The core reads only the page from its store, so a conversation of any size opens at once.
pub const PAGE_MESSAGES: usize = 40;
/// How close to an end of the loaded pictures the next page is read.
pub const PAGE_MARGIN: usize = 8;
/// Most tiles an album shows in its message; the last one counts the pictures it doesn't show ("+3").
pub const MAX_ALBUM_TILES: usize = 4;
/// Largest zoom of a picture, relative to its fitted size.
pub const MAX_ZOOM: f64 = 8.0;
/// Zoom of a double-click or double-tap on a fitted picture.
pub const DOUBLE_TAP_ZOOM: f64 = 2.5;
/// Pictures kept decoded on each side of the one on screen: a swipe to a neighbour shows it at
/// once, and memory stays bounded however long the album or the conversation is.
pub const DECODED_NEIGHBORS: usize = 1;

/// The state of one open viewer: the pictures it pages through, the one on screen, and the page of
/// the conversation it needs next.
#[derive(Clone, Debug)]
pub struct MediaViewer {
  conversation: ConversationId,
  media: ViewerMedia,
  /// The file the viewer was opened on.
  start: FileRef,
  current: FileRef,
  /// The opened message's files as the conversation showed them: its album is there at once,
  /// before the first page is read, and stays if that page doesn't have the message.
  seed: Vec<FileInfo>,
  maybe_pages: Option<MediaPages>,
  /// `None` until the user switches: then the size of the album decides ([`MediaScope::for_album_of`]).
  maybe_chosen_scope: Option<MediaScope>,
  /// Pages handed out by [`Self::next_read`] and not answered: not asked again, so a read that
  /// failed doesn't loop.
  asked: Vec<MediaAnchor>,
}

/// One file of one message.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FileRef {
  pub message: MessageId,
  /// The [`FileInfo::file_id`].
  pub file_id: String,
}

/// Which pictures previous/next page through.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MediaScope {
  /// Only the pictures of the message the viewer was opened from.
  Album,
  /// Every picture of the conversation the client knows.
  #[default]
  Conversation,
}

/// Which files a viewer shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewerMedia {
  /// Pictures and videos that are on the server: the desktop and web lightbox.
  PicturesAndVideos,
  /// Pictures with a preview, the ones a message shows as tiles: the mobile viewers, which open on a
  /// tile and swap in the original once it is downloaded.
  PicturesWithPreview,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
  Previous,
  Next,
}

/// Where the current picture is in the list, for "3 of 12" and the arrows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
  pub index: usize,
  pub len: usize,
}

/// What the viewer has read of a conversation's pictures and videos, newest first: the page around
/// the opened file, then pages further out as the user comes near an end (#274).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaPages {
  files: Vec<FileInfo>,
  has_older: bool,
  has_newer: bool,
}

/// How a message's pictures sit in its album grid, the same on every platform: two to
/// [`MAX_ALBUM_TILES`] tiles in two columns, and "+N" on the last tile for the pictures it doesn't
/// show (they open in the viewer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AlbumGrid {
  /// Tiles shown.
  pub tiles: usize,
  /// Pictures beyond the tiles, counted on the last one.
  pub hidden: usize,
}

/// When the mobile viewers fetch a picture's original by themselves (#199, #240); the thumbnail
/// shows at once either way, and the original can always be loaded by hand. A setting of the
/// device: data plans belong to the phone, not to the account.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OriginalDownload {
  /// As soon as the picture is opened, as desktop and web do.
  #[default]
  Always,
  /// Only on networks that aren't metered (Wi-Fi, not mobile data or a hotspot).
  UnmeteredOnly,
  /// Only when the user asks ("Load original").
  Never,
}

impl MediaViewer {
  /// A viewer opened on `file_id` of `message`, whose files (`message_files`, as the conversation
  /// showed them) form the album until the first page is read.
  pub fn new(
    conversation: ConversationId,
    message: MessageId,
    file_id: String,
    message_files: Vec<FileInfo>,
    media: ViewerMedia,
  ) -> Self {
    let start = FileRef { message, file_id };
    Self {
      conversation,
      media,
      current: start.clone(),
      start,
      seed: message_files
        .into_iter()
        .filter(|file| file.message == message)
        .collect(),
      maybe_pages: None,
      maybe_chosen_scope: None,
      asked: Vec::new(),
    }
  }

  pub fn conversation(&self) -> ConversationId {
    self.conversation
  }

  /// What previous/next page through: the album when its message has several pictures and the user
  /// didn't switch.
  pub fn scope(&self) -> MediaScope {
    self
      .maybe_chosen_scope
      .unwrap_or_else(|| MediaScope::for_album_of(self.album_len()))
  }

  /// How many pictures the opened message has.
  pub fn album_len(&self) -> usize {
    self.album().len()
  }

  /// Whether the user can switch between the album and the whole conversation.
  pub fn can_switch_scope(&self) -> bool {
    self.album_len() > 1
  }

  /// The pictures to page through, oldest first and each message's in the order they were attached.
  pub fn images(&self) -> Vec<FileInfo> {
    self.listed().into_iter().cloned().collect()
  }

  /// [`Self::images`] by reference: a conversation's pictures carry their thumbnails, so the list
  /// is not copied to page through it.
  pub fn listed(&self) -> Vec<&FileInfo> {
    match self.scope() {
      MediaScope::Album => self.album(),
      MediaScope::Conversation => {
        let Some(pages) = &self.maybe_pages else {
          return self.album();
        };
        let all = scoped(
          pages.files(),
          MediaScope::Conversation,
          self.start.message,
          self.media,
        );
        // A picture the pages don't have (a message still going up, one read after the page): the
        // album keeps the viewer where it is rather than losing the picture on screen.
        match all.iter().any(|file| self.current.matches(file)) {
          true => all,
          false => self.album(),
        }
      },
    }
  }

  /// The picture on screen.
  pub fn current(&self) -> &FileRef {
    &self.current
  }

  /// Where the picture on screen is in [`Self::images`].
  pub fn position(&self) -> Option<Position> {
    position_in(&self.listed(), &self.current)
  }

  /// The user paged to `file`; `false` (and nothing changes) when it isn't in the list.
  pub fn show(&mut self, file: FileRef) -> bool {
    if !self.is_listed(&file) {
      return false;
    }
    self.current = file;
    true
  }

  /// Switches between the album and the conversation. Back in the album from another message's
  /// picture, the viewer shows the picture it was opened on.
  pub fn set_scope(&mut self, scope: MediaScope) {
    self.maybe_chosen_scope = Some(scope);
    if !self.is_listed(&self.current) {
      self.current = self.start.clone();
    }
  }

  /// The page to read next, if any: the one around the opened message first, then, while paging
  /// through the conversation, the next one near an end that has more. Each is handed out once.
  pub fn next_read(&mut self) -> Option<MediaAnchor> {
    let anchor = self.wanted()?;
    self.asked.push(anchor);
    Some(anchor)
  }

  /// Adds a page read for `anchor` (from [`Self::next_read`]); `true` if the list changed.
  pub fn add_page(&mut self, anchor: MediaAnchor, page: MediaPage) -> bool {
    self.asked.retain(|asked| *asked != anchor);
    let before = self.keys();
    let is_first = matches!(anchor, MediaAnchor::Around(_) | MediaAnchor::Newest);
    match &mut self.maybe_pages {
      None if is_first => self.maybe_pages = Some(MediaPages::first(page)),
      Some(pages) if !is_first => pages.add(anchor, page),
      // A page that doesn't continue what is there (a second first page, a further one before the
      // first): nothing to join it to.
      _ => return false,
    }
    self.keys() != before
  }

  /// The messages of the pictures on screen and next to it ([`DECODED_NEIGHBORS`]), whose files the
  /// caller reads again after the core reported a change: a download's progress, its end, a
  /// deleted message.
  pub fn refresh_messages(&self) -> Vec<MessageId> {
    let listed = self.listed();
    let Some(at) = position_in(&listed, &self.current) else {
      return vec![self.current.message];
    };
    let from = at.index.saturating_sub(DECODED_NEIGHBORS);
    let to = (at.index + DECODED_NEIGHBORS + 1).min(listed.len());
    let mut messages: Vec<MessageId> = Vec::new();
    for file in &listed[from..to] {
      if !messages.contains(&file.message) {
        messages.push(file.message);
      }
    }
    messages
  }

  /// The files of `message` as they are now (from its extras); `true` if anything changed. A
  /// picture that is gone (its message deleted) leaves the list, and the viewer shows the one that
  /// took its place.
  pub fn update_files(&mut self, message: MessageId, files: Vec<FileInfo>) -> bool {
    let files: Vec<FileInfo> = files.into_iter().filter(|file| file.message == message).collect();
    let is_seed = message == self.start.message;
    let is_in_pages = self
      .maybe_pages
      .as_ref()
      .is_some_and(|pages| pages.files.iter().any(|file| file.message == message));
    let is_changed = (is_seed && self.seed != files)
      || self
        .maybe_pages
        .as_ref()
        .is_some_and(|pages| is_in_pages && pages.files_of(message) != files.iter().collect::<Vec<_>>());
    if !is_changed {
      return false;
    }
    let maybe_index = self.position().map(|at| at.index);
    if is_seed {
      self.seed = files.clone();
    }
    if let Some(pages) = &mut self.maybe_pages {
      pages.replace_message(message, files);
    }
    if !self.is_listed(&self.current) {
      let listed = self.listed();
      if let Some(index) = maybe_index
        && let Some(file) = listed.get(index.min(listed.len().saturating_sub(1)))
      {
        self.current = FileRef::of(file);
      }
    }
    true
  }

  /// The file `file` refers to, as the viewer knows it now.
  pub fn file(&self, file: &FileRef) -> Option<FileInfo> {
    self
      .listed()
      .into_iter()
      .find(|listed| file.matches(listed))
      .cloned()
  }

  fn wanted(&self) -> Option<MediaAnchor> {
    let anchor = match &self.maybe_pages {
      None => MediaAnchor::Around(self.start.message),
      Some(pages) => {
        if self.scope() != MediaScope::Conversation {
          return None;
        }
        pages.wanted(self.position()?, PAGE_MARGIN)?
      },
    };
    (!self.asked.contains(&anchor)).then_some(anchor)
  }

  /// The opened message's pictures, from the pages when they have it (fresher), else as it opened.
  fn album(&self) -> Vec<&FileInfo> {
    let from_pages = self
      .maybe_pages
      .as_ref()
      .map(|pages| scoped(pages.files(), MediaScope::Album, self.start.message, self.media))
      .unwrap_or_default();
    match from_pages.is_empty() {
      true => scoped(&self.seed, MediaScope::Album, self.start.message, self.media),
      false => from_pages,
    }
  }

  fn keys(&self) -> Vec<FileRef> {
    self.listed().into_iter().map(FileRef::of).collect()
  }

  fn is_listed(&self, file: &FileRef) -> bool {
    self.listed().into_iter().any(|listed| file.matches(listed))
  }
}

impl FileRef {
  pub fn of(file: &FileInfo) -> Self {
    Self {
      message: file.message,
      file_id: file.file_id.clone(),
    }
  }

  pub fn matches(&self, file: &FileInfo) -> bool {
    file.message == self.message && file.file_id == self.file_id
  }
}

impl MediaScope {
  /// A message with several pictures opens as an album; a single picture pages through the
  /// conversation.
  pub fn for_album_of(images_in_message: usize) -> Self {
    match images_in_message {
      0 | 1 => Self::Conversation,
      _ => Self::Album,
    }
  }
}

impl ViewerMedia {
  pub fn shows(self, file: &FileInfo) -> bool {
    let media_type = file.media_type.trim().to_ascii_lowercase();
    match self {
      Self::PicturesAndVideos => {
        (media_type.starts_with("image/") || media_type.starts_with("video/"))
          // Files still going up have no attachment ID yet.
          && file.file_id.parse::<AttachmentId>().is_ok()
      },
      Self::PicturesWithPreview => media_type.starts_with("image/") && file.maybe_thumbnail_png.is_some(),
    }
  }
}

impl Position {
  pub fn has_previous(self) -> bool {
    self.index > 0
  }

  pub fn has_next(self) -> bool {
    self.index + 1 < self.len
  }
}

impl MediaPages {
  /// The first page, read around the opened file.
  pub fn first(page: MediaPage) -> Self {
    Self {
      files: page.files,
      has_older: page.has_older,
      has_newer: page.has_newer,
    }
  }

  /// Newest first.
  pub fn files(&self) -> &[FileInfo] {
    &self.files
  }

  /// The page to read next at `position` of the viewer's list, if the user is within `margin`
  /// pictures of an end that has more: the older end first.
  pub fn wanted(&self, position: Position, margin: usize) -> Option<MediaAnchor> {
    if wants_older(position, margin, self.has_older) {
      return self.files.last().map(|oldest| MediaAnchor::Older(oldest.message));
    }
    if wants_newer(position, margin, self.has_newer) {
      return self
        .files
        .first()
        .map(|newest| MediaAnchor::Newer(newest.message));
    }
    None
  }

  /// Adds the page read at `anchor`. A page that brings nothing new ends the paging that way, so an
  /// unchanged edge isn't asked again.
  pub fn add(&mut self, anchor: MediaAnchor, page: MediaPage) {
    let before = self.files.len();
    match anchor {
      MediaAnchor::Older(_) => {
        append_older(&mut self.files, page.files);
        self.has_older = page.has_older && self.files.len() > before;
      },
      MediaAnchor::Newer(_) => {
        prepend_newer(&mut self.files, page.files);
        self.has_newer = page.has_newer && self.files.len() > before;
      },
      // The first page comes through `first`; nothing else reads these.
      MediaAnchor::Around(_) | MediaAnchor::Newest => {},
    }
  }

  /// The files of `message`, in the order they were attached.
  fn files_of(&self, message: MessageId) -> Vec<&FileInfo> {
    self.files.iter().filter(|file| file.message == message).collect()
  }

  /// Puts `files` (the message's files as they are now) where the message's files are; a message
  /// that isn't listed stays out.
  fn replace_message(&mut self, message: MessageId, files: Vec<FileInfo>) {
    let Some(start) = self.files.iter().position(|file| file.message == message) else {
      return;
    };
    let len = self.files[start..]
      .iter()
      .take_while(|file| file.message == message)
      .count();
    self.files.splice(start..start + len, files);
  }
}

impl AlbumGrid {
  /// The grid of a message with `pictures` pictures; `None` below two (one picture shows alone).
  pub fn of(pictures: usize) -> Option<Self> {
    (pictures >= 2).then(|| Self {
      tiles: pictures.min(MAX_ALBUM_TILES),
      hidden: pictures.saturating_sub(MAX_ALBUM_TILES),
    })
  }

  /// With three tiles the first spans both rows on the left and the other two stack on the right;
  /// two sit side by side, four in two rows.
  pub fn is_first_tall(self) -> bool {
    self.tiles == 3
  }

  /// The "+N" of the tile at `index`: on the last tile, when pictures are hidden.
  pub fn hidden_at(self, index: usize) -> Option<usize> {
    (index + 1 == self.tiles && self.hidden > 0).then_some(self.hidden)
  }
}

impl OriginalDownload {
  /// Whether opening a picture fetches its original by itself on the current network.
  pub fn fetches_automatically(self, is_metered: bool) -> bool {
    match self {
      Self::Always => true,
      Self::UnmeteredOnly => !is_metered,
      Self::Never => false,
    }
  }
}

/// The pictures to page through, oldest first (left to right): the conversation's, or only those
/// of `start`'s message. `files` comes newest first, as
/// [`crate::backend::ChatBackend::conversation_media`] returns it.
pub fn images_in_scope(
  files: &[FileInfo],
  scope: MediaScope,
  start: MessageId,
  media: ViewerMedia,
) -> Vec<FileInfo> {
  scoped(files, scope, start, media).into_iter().cloned().collect()
}

/// How many pictures `message` has among `files`, to choose the scope the viewer opens with.
pub fn album_len(files: &[FileInfo], message: MessageId, media: ViewerMedia) -> usize {
  files
    .iter()
    .filter(|file| file.message == message && media.shows(file))
    .count()
}

/// Where `current` is in `images`.
pub fn position(images: &[FileInfo], current: &FileRef) -> Option<Position> {
  images
    .iter()
    .position(|file| current.matches(file))
    .map(|index| Position {
      index,
      len: images.len(),
    })
}

/// [`position`] in a list of references.
fn position_in(images: &[&FileInfo], current: &FileRef) -> Option<Position> {
  images
    .iter()
    .position(|file| current.matches(file))
    .map(|index| Position {
      index,
      len: images.len(),
    })
}

/// The picture before or after `current`; `None` at either end (the list doesn't wrap around).
pub fn neighbor<'a>(images: &'a [FileInfo], current: &FileRef, direction: Direction) -> Option<&'a FileInfo> {
  let at = position(images, current)?;
  let index = match direction {
    Direction::Previous => at.index.checked_sub(1)?,
    Direction::Next => at.index + 1,
  };
  images.get(index)
}

/// Whether the viewer, at `position` of its list, should read the next older page: the user is
/// within `margin` pictures of the old end and the conversation has more (#274).
pub fn wants_older(position: Position, margin: usize, has_older: bool) -> bool {
  has_older && position.index < margin
}

/// Like [`wants_older`] for the newer end.
pub fn wants_newer(position: Position, margin: usize, has_newer: bool) -> bool {
  has_newer && position.len.saturating_sub(position.index + 1) < margin
}

/// Adds an older page (newest first) to the end of `files`, leaving out files that are in the list
/// already (a page read while messages arrived can overlap).
pub fn append_older(files: &mut Vec<FileInfo>, page: Vec<FileInfo>) {
  let fresh: Vec<FileInfo> = page.into_iter().filter(|file| !is_listed(files, file)).collect();
  files.extend(fresh);
}

/// Adds a newer page (newest first) to the start of `files`.
pub fn prepend_newer(files: &mut Vec<FileInfo>, page: Vec<FileInfo>) {
  let mut fresh: Vec<FileInfo> = page.into_iter().filter(|file| !is_listed(files, file)).collect();
  fresh.append(files);
  *files = fresh;
}

fn is_listed(files: &[FileInfo], file: &FileInfo) -> bool {
  files
    .iter()
    .any(|listed| listed.message == file.message && listed.file_id == file.file_id)
}

/// [`images_in_scope`] by reference.
fn scoped(files: &[FileInfo], scope: MediaScope, start: MessageId, media: ViewerMedia) -> Vec<&FileInfo> {
  let mut images: Vec<&FileInfo> = files
    .iter()
    .filter(|file| media.shows(file))
    .filter(|file| scope == MediaScope::Conversation || file.message == start)
    .collect();
  oldest_first(&mut images);
  images
}

/// Turns the newest-first list into oldest-first while keeping the files of one message in the order
/// they were attached.
fn oldest_first(files: &mut [&FileInfo]) {
  files.reverse();
  let mut start = 0;
  while start < files.len() {
    let message = files[start].message;
    let len = files[start..]
      .iter()
      .take_while(|file| file.message == message)
      .count();
    files[start..start + len].reverse();
    start += len;
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::actions::FileTransfer;

  const PICTURES: ViewerMedia = ViewerMedia::PicturesAndVideos;
  const TILES: ViewerMedia = ViewerMedia::PicturesWithPreview;

  mod navigation {
    use super::*;

    #[test]
    fn the_conversation_pages_oldest_to_newest_keeping_album_order() {
      let images = images_in_scope(&conversation(), MediaScope::Conversation, id(1), PICTURES);
      assert_eq!(names(&images), ["a.png", "b1.png", "b2.png", "b3.png", "c.png"]);
    }

    #[test]
    fn an_album_holds_only_the_pictures_of_its_message() {
      let images = images_in_scope(&conversation(), MediaScope::Album, id(2), PICTURES);
      assert_eq!(
        names(&images),
        ["b1.png", "b2.png", "b3.png"],
        "the text file is not a picture"
      );
    }

    #[test]
    fn previous_and_next_stop_at_the_ends() {
      let album = images_in_scope(&conversation(), MediaScope::Album, id(2), PICTURES);
      assert_eq!(neighbor(&album, &file_ref(2, 21), Direction::Previous), None);
      assert_eq!(
        neighbor(&album, &file_ref(2, 21), Direction::Next).map(FileRef::of),
        Some(file_ref(2, 22))
      );
      assert_eq!(neighbor(&album, &file_ref(2, 23), Direction::Next), None);
      let first = position(&album, &file_ref(2, 21)).expect("in the album");
      assert_eq!(first, Position { index: 0, len: 3 });
      assert!(!first.has_previous() && first.has_next());
      let last = position(&album, &file_ref(2, 23)).expect("in the album");
      assert!(last.has_previous() && !last.has_next());
    }

    #[test]
    fn the_scope_follows_the_number_of_pictures_in_the_message() {
      let files = conversation();
      assert_eq!(album_len(&files, id(2), PICTURES), 3);
      assert_eq!(MediaScope::for_album_of(3), MediaScope::Album);
      assert_eq!(
        MediaScope::for_album_of(album_len(&files, id(1), PICTURES)),
        MediaScope::Conversation
      );
      assert_eq!(MediaScope::for_album_of(0), MediaScope::Conversation);
    }

    #[test]
    fn mobile_viewers_show_pictures_with_a_preview_and_desktop_pictures_and_videos_on_the_server() {
      let mut uploading = image(4, 40, "d.png");
      uploading.file_id = "pending-0".to_owned();
      uploading.maybe_thumbnail_png = Some(vec![1]);
      let video = file(4, 41, "clip.mp4", "video/mp4");
      let bare = image(4, 42, "e.png");
      let mut tile = image(4, 43, "f.png");
      tile.maybe_thumbnail_png = Some(vec![1]);
      tile.media_type = " Image/PNG ".to_owned();
      assert!(!PICTURES.shows(&uploading) && TILES.shows(&uploading));
      assert!(PICTURES.shows(&video) && !TILES.shows(&video));
      assert!(
        PICTURES.shows(&bare) && !TILES.shows(&bare),
        "no preview to show first"
      );
      assert!(PICTURES.shows(&tile) && TILES.shows(&tile));
      assert!(!PICTURES.shows(&file(4, 44, "notes.txt", "text/plain")));
    }
  }

  mod pages {
    use super::*;

    #[test]
    fn the_next_page_is_wanted_near_an_end_that_has_more() {
      let at = |index, len| Position { index, len };
      assert!(wants_older(at(2, 50), 8, true));
      assert!(!wants_older(at(2, 50), 8, false), "nothing older left");
      assert!(!wants_older(at(8, 50), 8, true), "far enough from the old end");
      assert!(wants_newer(at(45, 50), 8, true));
      assert!(!wants_newer(at(41, 50), 8, true));
      assert!(!wants_newer(at(49, 50), 8, false));
      assert!(
        wants_older(at(0, 3), 8, true) && wants_newer(at(0, 3), 8, true),
        "a short list wants both"
      );
    }

    #[test]
    fn pages_join_the_list_without_repeats() {
      // Newest first: message 5 is the newest, 3 the oldest loaded.
      let mut files = vec![image(5, 50, "e"), image(4, 40, "d"), image(3, 30, "c")];
      append_older(
        &mut files,
        vec![image(3, 30, "c"), image(2, 20, "b"), image(1, 10, "a")],
      );
      assert_eq!(ids(&files), ["50", "40", "30", "20", "10"]);
      prepend_newer(
        &mut files,
        vec![image(7, 70, "g"), image(6, 60, "f"), image(5, 50, "e")],
      );
      assert_eq!(ids(&files), ["70", "60", "50", "40", "30", "20", "10"]);
    }

    #[test]
    fn pages_are_read_towards_the_end_the_user_comes_near() {
      let mut pages = MediaPages::first(page(
        vec![image(5, 50, "e"), image(4, 40, "d"), image(3, 30, "c")],
        true,
        true,
      ));
      let at = |index, len| Position { index, len };
      assert_eq!(pages.wanted(at(0, 3), 8), Some(MediaAnchor::Older(id(3))));
      pages.add(
        MediaAnchor::Older(id(3)),
        page(vec![image(2, 20, "b")], false, false),
      );
      assert_eq!(
        pages.wanted(at(1, 4), 8),
        Some(MediaAnchor::Newer(id(5))),
        "the old end is reached: the newer one is next"
      );
      // A page that only repeats what is listed ends that way too, instead of asking again forever.
      pages.add(
        MediaAnchor::Newer(id(5)),
        page(vec![image(5, 50, "e")], false, true),
      );
      assert_eq!(pages.wanted(at(1, 4), 8), None);
      assert_eq!(ids(pages.files()), ["50", "40", "30", "20"]);
    }
  }

  mod viewer {
    use super::*;

    #[test]
    fn an_album_opens_at_once_and_pages_within_its_message() {
      let viewer = open(2, 22, album());
      assert_eq!(viewer.scope(), MediaScope::Album);
      assert!(viewer.can_switch_scope());
      assert_eq!(names(&viewer.images()), ["b1.png", "b2.png", "b3.png"]);
      assert_eq!(viewer.position(), Some(Position { index: 1, len: 3 }));
    }

    #[test]
    fn a_single_picture_pages_through_the_conversation_once_its_page_is_read() {
      let mut viewer = open(1, 10, vec![tile(1, 10, "a.png")]);
      assert_eq!(viewer.scope(), MediaScope::Conversation);
      assert!(!viewer.can_switch_scope());
      assert_eq!(
        names(&viewer.images()),
        ["a.png"],
        "the picture shows before the page is read"
      );
      let anchor = viewer.next_read().expect("the first page");
      assert_eq!(anchor, MediaAnchor::Around(id(1)));
      assert_eq!(viewer.next_read(), None, "asked once");
      assert!(viewer.add_page(anchor, page(tiles_of_conversation(), false, false)));
      assert_eq!(
        names(&viewer.images()),
        ["a.png", "b1.png", "b2.png", "b3.png", "c.png"]
      );
      assert_eq!(viewer.position(), Some(Position { index: 0, len: 5 }));
    }

    #[test]
    fn switching_to_the_conversation_and_back_returns_to_the_opened_picture() {
      let mut viewer = open(2, 21, album());
      let anchor = viewer.next_read().expect("the first page");
      viewer.add_page(anchor, page(tiles_of_conversation(), false, false));
      assert_eq!(viewer.scope(), MediaScope::Album);
      assert_eq!(viewer.next_read(), None, "an album reads no further pages");
      viewer.set_scope(MediaScope::Conversation);
      assert_eq!(viewer.position(), Some(Position { index: 1, len: 5 }));
      assert!(viewer.show(file_ref(3, 30)));
      viewer.set_scope(MediaScope::Album);
      assert_eq!(viewer.current(), &file_ref(2, 21), "c.png is not in the album");
      assert!(
        !viewer.show(file_ref(3, 30)),
        "only the album's pictures can be shown"
      );
    }

    #[test]
    fn the_conversation_reads_further_pages_near_its_ends() {
      let mut viewer = open(5, 50, vec![tile(5, 50, "e")]);
      let first = viewer.next_read().expect("the first page");
      viewer.add_page(first, page(vec![tile(5, 50, "e"), tile(4, 40, "d")], true, false));
      let older = viewer.next_read().expect("near the old end");
      assert_eq!(older, MediaAnchor::Older(id(4)));
      assert!(viewer.add_page(older, page(vec![tile(3, 30, "c")], false, false)));
      assert_eq!(ids(&viewer.images()), ["30", "40", "50"]);
      assert_eq!(
        viewer.position(),
        Some(Position { index: 2, len: 3 }),
        "still on the same picture"
      );
      assert_eq!(viewer.next_read(), None, "both ends reached");
    }

    #[test]
    fn a_failed_read_is_not_asked_again() {
      let mut viewer = open(1, 10, vec![tile(1, 10, "a.png")]);
      assert!(viewer.next_read().is_some());
      // The caller couldn't read it and never answers.
      assert_eq!(viewer.next_read(), None);
      assert_eq!(names(&viewer.images()), ["a.png"]);
    }

    #[test]
    fn a_page_that_does_not_continue_the_list_is_ignored() {
      let mut viewer = open(1, 10, vec![tile(1, 10, "a.png")]);
      assert!(!viewer.add_page(
        MediaAnchor::Older(id(1)),
        page(vec![tile(9, 90, "z")], false, false)
      ));
      assert_eq!(names(&viewer.images()), ["a.png"]);
    }

    #[test]
    fn the_pictures_around_the_current_one_are_refreshed() {
      let mut viewer = open(2, 22, album());
      assert_eq!(viewer.refresh_messages(), [id(2)]);
      let anchor = viewer.next_read().expect("the first page");
      viewer.add_page(anchor, page(tiles_of_conversation(), false, false));
      viewer.set_scope(MediaScope::Conversation);
      assert!(viewer.show(file_ref(2, 23)));
      assert_eq!(viewer.refresh_messages(), [id(2), id(3)], "b2, b3 and c");
    }

    #[test]
    fn a_finished_download_reaches_the_viewer() {
      let mut viewer = open(2, 22, album());
      let mut fresh = album();
      fresh[2].transfer = FileTransfer::Local {
        path: "/tmp/b2.png".to_owned(),
      };
      assert!(viewer.update_files(id(2), fresh.clone()));
      let shown = viewer.file(&file_ref(2, 22)).expect("listed");
      assert_eq!(shown.transfer, fresh[2].transfer);
      assert!(!viewer.update_files(id(2), fresh), "nothing new");
    }

    #[test]
    fn a_deleted_picture_leaves_and_its_neighbour_takes_its_place() {
      let mut viewer = open(2, 22, album());
      let without_b2: Vec<FileInfo> = album().into_iter().filter(|file| file.file_id != "22").collect();
      assert!(viewer.update_files(id(2), without_b2));
      assert_eq!(viewer.current(), &file_ref(2, 23));
      assert!(viewer.update_files(id(2), Vec::new()));
      assert!(
        viewer.images().is_empty(),
        "the message is gone: nothing left to show"
      );
    }
  }

  mod album_grid {
    use super::*;

    #[test]
    fn a_single_picture_has_no_grid() {
      assert_eq!(AlbumGrid::of(0), None);
      assert_eq!(AlbumGrid::of(1), None);
    }

    #[test]
    fn two_to_four_pictures_fill_their_tiles() {
      let two = AlbumGrid::of(2).expect("a grid");
      assert_eq!((two.tiles, two.hidden, two.is_first_tall()), (2, 0, false));
      let three = AlbumGrid::of(3).expect("a grid");
      assert_eq!((three.tiles, three.hidden, three.is_first_tall()), (3, 0, true));
      let four = AlbumGrid::of(4).expect("a grid");
      assert_eq!((four.tiles, four.hidden, four.hidden_at(3)), (4, 0, None));
    }

    #[test]
    fn more_pictures_are_counted_on_the_last_tile() {
      let grid = AlbumGrid::of(9).expect("a grid");
      assert_eq!((grid.tiles, grid.hidden), (MAX_ALBUM_TILES, 5));
      assert_eq!(grid.hidden_at(3), Some(5));
      assert_eq!(grid.hidden_at(2), None);
    }
  }

  mod original_download {
    use super::*;

    #[test]
    fn the_setting_decides_by_the_network() {
      assert!(OriginalDownload::Always.fetches_automatically(true));
      assert!(OriginalDownload::UnmeteredOnly.fetches_automatically(false));
      assert!(!OriginalDownload::UnmeteredOnly.fetches_automatically(true));
      assert!(!OriginalDownload::Never.fetches_automatically(false));
      assert_eq!(
        OriginalDownload::default(),
        OriginalDownload::Always,
        "as desktop and web"
      );
    }
  }

  /// Newest first, like the backend: message 3 (c), message 2 (b1, notes, b2, b3), message 1 (a).
  fn conversation() -> Vec<FileInfo> {
    vec![
      image(3, 30, "c.png"),
      image(2, 21, "b1.png"),
      file(2, 24, "notes.txt", "text/plain"),
      image(2, 22, "b2.png"),
      image(2, 23, "b3.png"),
      image(1, 10, "a.png"),
    ]
  }

  /// [`conversation`] with previews, as the mobile viewers show it.
  fn tiles_of_conversation() -> Vec<FileInfo> {
    conversation()
      .into_iter()
      .map(|mut file| {
        file.maybe_thumbnail_png = Some(vec![1]);
        file
      })
      .collect()
  }

  /// Message 2's files as its extras list them.
  fn album() -> Vec<FileInfo> {
    vec![
      tile(2, 21, "b1.png"),
      file(2, 24, "notes.txt", "text/plain"),
      tile(2, 22, "b2.png"),
      tile(2, 23, "b3.png"),
    ]
  }

  fn open(message: i64, attachment: i64, files: Vec<FileInfo>) -> MediaViewer {
    MediaViewer::new(
      "1".parse().expect("valid id"),
      id(message),
      attachment.to_string(),
      files,
      TILES,
    )
  }

  fn page(files: Vec<FileInfo>, has_older: bool, has_newer: bool) -> MediaPage {
    MediaPage {
      files,
      has_older,
      has_newer,
    }
  }

  fn names(files: &[FileInfo]) -> Vec<&str> {
    files.iter().map(|file| file.file_name.as_str()).collect()
  }

  fn ids(files: &[FileInfo]) -> Vec<&str> {
    files.iter().map(|file| file.file_id.as_str()).collect()
  }

  fn file_ref(message: i64, attachment: i64) -> FileRef {
    FileRef {
      message: id(message),
      file_id: attachment.to_string(),
    }
  }

  fn tile(message: i64, attachment: i64, name: &str) -> FileInfo {
    let mut file = image(message, attachment, name);
    file.maybe_thumbnail_png = Some(vec![1]);
    file
  }

  fn image(message: i64, attachment: i64, name: &str) -> FileInfo {
    file(message, attachment, name, "image/png")
  }

  fn file(message: i64, attachment: i64, name: &str, media_type: &str) -> FileInfo {
    FileInfo {
      conversation: "1".parse().expect("valid id"),
      message: id(message),
      file_id: attachment.to_string(),
      file_name: name.to_owned(),
      media_type: media_type.to_owned(),
      size_bytes: 1,
      maybe_thumbnail_png: None,
      transfer: FileTransfer::Remote,
      maybe_sender: None,
      maybe_sent_at: None,
      maybe_dimensions: None,
      maybe_duration_ms: None,
      maybe_content_sha256: None,
    }
  }

  fn id(message: i64) -> MessageId {
    message.to_string().parse().expect("valid id")
  }
}
