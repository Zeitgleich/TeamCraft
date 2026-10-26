//! The image viewer of the mobile apps (#240): which pictures it pages through (one message's album
//! or the whole conversation), where it stands, which page of the conversation to read next, how
//! albums sit in a message and when an original is fetched by itself. The logic is
//! `teamcraft_client_api::media_viewer`, shared with the desktop and web lightbox; the apps only
//! draw it.
//!
//! The viewer holds no handle on the core: it says which `ChatQuery` it needs
//! ([`MediaViewer::next_query`], [`MediaViewer::refresh_query`]), the app runs it with
//! `TeamCraft::query` and hands the answer back ([`MediaViewer::add_answer`]). The first read is
//! the opened message's own files (its album), then the page of the conversation around it. Pages
//! stay here, in Rust; the app decodes only the files it shows ([`MediaViewer::file_json`]).

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use teamcraft_client_api::actions::FileInfo;
use teamcraft_client_api::media_viewer::{
  self, AlbumGrid, DECODED_NEIGHBORS, DOUBLE_TAP_ZOOM, FileRef, MAX_ZOOM, PAGE_MESSAGES, ViewerMedia,
};
use teamcraft_client_api::remote::{ChatAnswer, ChatQuery};

use crate::engine::parse_id;
use crate::error::TeamCraftError;

/// One open viewer. Created when a picture is opened, dropped when the viewer closes.
#[derive(uniffi::Object)]
pub struct MediaViewer {
  state: Mutex<ViewerState>,
}

/// What the viewer shows now.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MediaViewerState {
  /// The pictures to page through, oldest first. Empty until the opened message is read (the app
  /// shows the tapped picture's thumbnail meanwhile), and when its pictures are gone (close).
  pub items: Vec<MediaViewerItem>,
  /// The opened message has been read: an empty list now means its pictures are gone.
  pub is_loaded: bool,
  /// Where the picture on screen is in `items`.
  pub index: u32,
  pub scope: MediaScope,
  /// The opened message has several pictures: the user can switch between them and every picture
  /// of the conversation.
  pub can_switch_scope: bool,
}

/// A picture of the list; [`MediaViewer::file_json`] gives its file.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MediaViewerItem {
  /// Unique in the list and stable while pages are added: the pager's key.
  pub key: String,
  pub message_id: String,
  pub file_id: String,
}

/// Which pictures the viewer pages through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MediaScope {
  /// The pictures of the message it was opened from.
  Album,
  /// Every picture of the conversation.
  Conversation,
}

/// When the viewer fetches a picture's original by itself (Settings → Chats); the app keeps the
/// choice per device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OriginalDownload {
  Always,
  /// Only on networks that aren't metered.
  UnmeteredOnly,
  /// Only on "Load original".
  Never,
}

/// How a message's pictures sit in its album grid: `tiles` tiles in two columns (with three, the
/// first spans both rows), and "+`hidden`" on the last tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AlbumLayout {
  pub tiles: u32,
  pub hidden: u32,
  pub is_first_tall: bool,
}

/// Zoom and memory limits of the viewer, the same on every platform.
#[derive(Clone, Copy, Debug, PartialEq, uniffi::Record)]
pub struct MediaViewerLimits {
  /// Largest zoom, relative to the fitted picture.
  pub max_zoom: f64,
  /// Zoom of a double tap on a fitted picture.
  pub double_tap_zoom: f64,
  /// Pictures kept decoded on each side of the one on screen; the rest are released.
  pub decoded_neighbors: u32,
}

/// The viewer and how far the opened message has been read.
struct ViewerState {
  viewer: media_viewer::MediaViewer,
  seed: Seed,
}

/// The opened message's own files: the album, there before any page of the conversation is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seed {
  Wanted,
  Asked,
  Read,
}

#[uniffi::export]
impl MediaViewer {
  /// A viewer opened on `file_id` of `message_id`; its first query reads that message's files.
  #[uniffi::constructor]
  pub fn new(
    conversation_id: String,
    message_id: String,
    file_id: String,
  ) -> Result<Arc<Self>, TeamCraftError> {
    let viewer = media_viewer::MediaViewer::new(
      parse_id(&conversation_id)?,
      parse_id(&message_id)?,
      file_id,
      Vec::new(),
      ViewerMedia::PicturesWithPreview,
    );
    Ok(Arc::new(Self {
      state: Mutex::new(ViewerState {
        viewer,
        seed: Seed::Wanted,
      }),
    }))
  }

  pub fn state(&self) -> MediaViewerState {
    let state = self.lock();
    let viewer = &state.viewer;
    let index = viewer.position().map_or(0, |at| at.index);
    MediaViewerState {
      items: viewer.listed().into_iter().map(MediaViewerItem::of).collect(),
      is_loaded: state.seed == Seed::Read,
      index: to_u32(index),
      scope: viewer.scope().into(),
      can_switch_scope: viewer.can_switch_scope(),
    }
  }

  /// The file of a listed picture as JSON (`FileInfo`: thumbnail, transfer, sender and time).
  pub fn file_json(&self, message_id: String, file_id: String) -> Option<String> {
    let message = parse_id(&message_id).ok()?;
    let file = self.lock().viewer.file(&FileRef { message, file_id })?;
    serde_json::to_string(&file).ok()
  }

  /// The user paged to this picture; `false` if it isn't listed.
  pub fn show(&self, message_id: String, file_id: String) -> bool {
    let Ok(message) = parse_id(&message_id) else {
      return false;
    };
    self.lock().viewer.show(FileRef { message, file_id })
  }

  /// Switches between the album and the conversation.
  pub fn set_scope(&self, scope: MediaScope) {
    self.lock().viewer.set_scope(scope.into());
  }

  /// The next read the viewer needs, as a `ChatQuery` (JSON) for `TeamCraft::query`: the opened
  /// message, then the conversation's pages around the picture on screen; `None` when nothing is
  /// needed. Each is handed out once: one that fails isn't asked again.
  pub fn next_query(&self) -> Option<String> {
    let mut state = self.lock();
    if state.seed == Seed::Wanted {
      state.seed = Seed::Asked;
      return query_json(&ChatQuery::MessageExtras {
        conversation: state.viewer.conversation(),
        messages: vec![state.viewer.current().message],
      });
    }
    let anchor = state.viewer.next_read()?;
    query_json(&ChatQuery::ConversationMedia {
      conversation: state.viewer.conversation(),
      anchor,
      max_messages: PAGE_MESSAGES,
    })
  }

  /// After the core reported a change: the files of the picture on screen and its neighbours, as a
  /// `ChatQuery` (JSON), for their download progress, a finished download or a deleted message.
  pub fn refresh_query(&self) -> Option<String> {
    let state = self.lock();
    query_json(&ChatQuery::MessageExtras {
      conversation: state.viewer.conversation(),
      messages: state.viewer.refresh_messages(),
    })
  }

  /// The answer to a query from [`Self::next_query`] or [`Self::refresh_query`]; `true` if what
  /// the viewer shows changed (read [`Self::state`] and the shown files again).
  pub fn add_answer(&self, query_json: String, answer_json: String) -> Result<bool, TeamCraftError> {
    let query: ChatQuery = from_json(&query_json)?;
    let answer: ChatAnswer = from_json(&answer_json)?;
    let mut state = self.lock();
    match (query, answer) {
      (ChatQuery::ConversationMedia { anchor, .. }, ChatAnswer::MediaPage { page }) => {
        Ok(state.viewer.add_page(anchor, page))
      },
      (ChatQuery::MessageExtras { messages, .. }, ChatAnswer::MessageExtras { extras }) => {
        let mut is_changed = false;
        if state.seed == Seed::Asked && messages.contains(&state.viewer.current().message) {
          state.seed = Seed::Read;
          is_changed = true;
        }
        // A message the store doesn't answer for (an own one that got its server ID meanwhile)
        // keeps what the viewer has; one deleted for everyone takes its pictures out of the list.
        for extra in extras
          .into_iter()
          .filter(|extra| messages.contains(&extra.message))
        {
          let files = match extra.is_deleted {
            true => Vec::new(),
            false => extra.files,
          };
          is_changed |= state.viewer.update_files(extra.message, files);
        }
        Ok(is_changed)
      },
      _ => Err(TeamCraftError::InvalidInput(
        "not an answer to a query of the image viewer".into(),
      )),
    }
  }
}

impl MediaViewer {
  fn lock(&self) -> MutexGuard<'_, ViewerState> {
    // A panic while holding the lock left plain data behind, still consistent enough to show.
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

impl MediaViewerItem {
  fn of(file: &FileInfo) -> Self {
    Self {
      key: format!("{}:{}", file.message, file.file_id),
      message_id: file.message.to_string(),
      file_id: file.file_id.clone(),
    }
  }
}

impl From<media_viewer::MediaScope> for MediaScope {
  fn from(scope: media_viewer::MediaScope) -> Self {
    match scope {
      media_viewer::MediaScope::Album => Self::Album,
      media_viewer::MediaScope::Conversation => Self::Conversation,
    }
  }
}

impl From<MediaScope> for media_viewer::MediaScope {
  fn from(scope: MediaScope) -> Self {
    match scope {
      MediaScope::Album => Self::Album,
      MediaScope::Conversation => Self::Conversation,
    }
  }
}

impl From<OriginalDownload> for media_viewer::OriginalDownload {
  fn from(setting: OriginalDownload) -> Self {
    match setting {
      OriginalDownload::Always => Self::Always,
      OriginalDownload::UnmeteredOnly => Self::UnmeteredOnly,
      OriginalDownload::Never => Self::Never,
    }
  }
}

/// The album grid of a message with `pictures` pictures; `None` below two (one shows alone).
#[uniffi::export]
pub fn album_layout(pictures: u32) -> Option<AlbumLayout> {
  let grid = AlbumGrid::of(usize::try_from(pictures).unwrap_or(usize::MAX))?;
  Some(AlbumLayout {
    tiles: to_u32(grid.tiles),
    hidden: to_u32(grid.hidden),
    is_first_tall: grid.is_first_tall(),
  })
}

/// Whether opening a picture fetches its original by itself, with `setting` on the current network.
#[uniffi::export]
pub fn fetches_original_automatically(setting: OriginalDownload, is_metered: bool) -> bool {
  media_viewer::OriginalDownload::from(setting).fetches_automatically(is_metered)
}

/// The viewer's limits.
#[uniffi::export]
pub fn media_viewer_limits() -> MediaViewerLimits {
  MediaViewerLimits {
    max_zoom: MAX_ZOOM,
    double_tap_zoom: DOUBLE_TAP_ZOOM,
    decoded_neighbors: to_u32(DECODED_NEIGHBORS),
  }
}

fn to_u32(count: usize) -> u32 {
  u32::try_from(count).unwrap_or(u32::MAX)
}

fn query_json(query: &ChatQuery) -> Option<String> {
  serde_json::to_string(query).ok()
}

fn from_json<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, TeamCraftError> {
  serde_json::from_str(text).map_err(|error| TeamCraftError::InvalidInput(error.to_string()))
}

#[cfg(test)]
mod tests {
  use super::*;
  use teamcraft_client_api::actions::{FileTransfer, MediaAnchor, MediaPage, MessageExtras};

  #[test]
  fn the_opened_message_is_read_first_and_its_album_shows() {
    let viewer = MediaViewer::new("1".into(), "2".into(), "22".into()).expect("a viewer");
    assert!(!viewer.state().is_loaded && viewer.state().items.is_empty());
    let query = viewer.next_query().expect("the message");
    let ChatQuery::MessageExtras { messages, .. } = serde_json::from_str(&query).expect("a query") else {
      panic!("not the message's extras: {query}");
    };
    assert_eq!(messages, [id(2)]);
    assert!(
      viewer
        .add_answer(query, extras_answer(album()))
        .expect("an answer to it")
    );
    let state = viewer.state();
    assert!(state.is_loaded);
    assert_eq!(keys(&state), ["2:21", "2:22", "2:23"]);
    assert_eq!(
      (state.index, state.scope, state.can_switch_scope),
      (1, MediaScope::Album, true)
    );
    let json = viewer.file_json("2".into(), "22".into()).expect("listed");
    let file: FileInfo = serde_json::from_str(&json).expect("a FileInfo");
    assert_eq!(file.file_name, "b2.png");
    assert_eq!(viewer.file_json("2".into(), "99".into()), None);
  }

  #[test]
  fn the_conversation_is_read_through_queries_the_app_runs() {
    let viewer = open(1, 10, vec![tile(1, 10, "a.png")]);
    let query = viewer.next_query().expect("the first page");
    let ChatQuery::ConversationMedia { anchor, .. } = serde_json::from_str(&query).expect("a query") else {
      panic!("not a media query: {query}");
    };
    assert_eq!(anchor, MediaAnchor::Around(id(1)));
    let answer = ChatAnswer::MediaPage {
      page: MediaPage {
        files: vec![tile(3, 30, "c.png"), tile(2, 21, "b1.png"), tile(1, 10, "a.png")],
        has_older: false,
        has_newer: false,
      },
    };
    let is_changed = viewer
      .add_answer(query, serde_json::to_string(&answer).expect("json"))
      .expect("an answer to it");
    assert!(is_changed);
    assert_eq!(keys(&viewer.state()), ["1:10", "2:21", "3:30"]);
    assert!(viewer.show("3".into(), "30".into()));
    assert_eq!(viewer.state().index, 2);
    assert_eq!(viewer.next_query(), None, "both ends are reached");
  }

  #[test]
  fn a_refresh_brings_the_finished_download() {
    let viewer = open(2, 22, album());
    let query = viewer.refresh_query().expect("a query");
    let mut files = album();
    files[1].transfer = FileTransfer::Local {
      path: "/data/b2.png".into(),
    };
    assert!(
      viewer
        .add_answer(query, extras_answer(files))
        .expect("an answer to it")
    );
    let json = viewer.file_json("2".into(), "22".into()).expect("listed");
    assert!(json.contains("/data/b2.png"));
  }

  #[test]
  fn a_deleted_message_empties_the_viewer() {
    let viewer = open(2, 22, album());
    let query = viewer.refresh_query().expect("a query");
    let unknown = serde_json::to_string(&ChatAnswer::MessageExtras { extras: Vec::new() }).expect("json");
    assert!(
      !viewer
        .add_answer(query.clone(), unknown)
        .expect("an answer to it"),
      "a message the store doesn't answer for stays"
    );
    let mut deleted = MessageExtras::new(id(2));
    deleted.files = album();
    deleted.is_deleted = true;
    let answer = serde_json::to_string(&ChatAnswer::MessageExtras {
      extras: vec![deleted],
    })
    .expect("json");
    assert!(viewer.add_answer(query, answer).expect("an answer to it"));
    let state = viewer.state();
    assert!(state.is_loaded && state.items.is_empty());
  }

  #[test]
  fn switching_scope_and_bad_input_are_handled() {
    let viewer = open(2, 21, album());
    viewer.set_scope(MediaScope::Conversation);
    assert_eq!(viewer.state().scope, MediaScope::Conversation);
    assert!(!viewer.show("x".into(), "21".into()));
    assert!(viewer.add_answer("{}".into(), "{}".into()).is_err());
    assert!(MediaViewer::new("1".into(), "two".into(), "21".into()).is_err());
  }

  #[test]
  fn albums_downloads_and_limits_are_shared() {
    assert_eq!(album_layout(1), None);
    assert_eq!(
      album_layout(3),
      Some(AlbumLayout {
        tiles: 3,
        hidden: 0,
        is_first_tall: true
      })
    );
    assert_eq!(
      album_layout(7).map(|layout| (layout.tiles, layout.hidden)),
      Some((4, 3))
    );
    assert!(fetches_original_automatically(
      OriginalDownload::UnmeteredOnly,
      false
    ));
    assert!(!fetches_original_automatically(
      OriginalDownload::UnmeteredOnly,
      true
    ));
    assert!(!fetches_original_automatically(OriginalDownload::Never, false));
    assert_eq!(media_viewer_limits().decoded_neighbors, 1);
  }

  /// A viewer whose opened message (with `files`) has been read.
  fn open(message: i64, attachment: i64, files: Vec<FileInfo>) -> Arc<MediaViewer> {
    let viewer = MediaViewer::new("1".into(), message.to_string(), attachment.to_string()).expect("a viewer");
    let query = viewer.next_query().expect("the message");
    viewer
      .add_answer(query, extras_answer(files))
      .expect("an answer to it");
    viewer
  }

  fn extras_answer(files: Vec<FileInfo>) -> String {
    let mut extras = MessageExtras::new(files.first().map_or(id(1), |file| file.message));
    extras.files = files;
    serde_json::to_string(&ChatAnswer::MessageExtras { extras: vec![extras] }).expect("json")
  }

  fn keys(state: &MediaViewerState) -> Vec<&str> {
    state.items.iter().map(|item| item.key.as_str()).collect()
  }

  fn album() -> Vec<FileInfo> {
    vec![
      tile(2, 21, "b1.png"),
      tile(2, 22, "b2.png"),
      tile(2, 23, "b3.png"),
    ]
  }

  fn tile(message: i64, attachment: i64, name: &str) -> FileInfo {
    FileInfo {
      conversation: "1".parse().expect("valid id"),
      message: id(message),
      file_id: attachment.to_string(),
      file_name: name.to_owned(),
      media_type: "image/png".to_owned(),
      size_bytes: 1,
      maybe_thumbnail_png: Some(vec![1]),
      transfer: FileTransfer::Remote,
      maybe_sender: None,
      maybe_sent_at: None,
      maybe_dimensions: None,
      maybe_duration_ms: None,
      maybe_content_sha256: None,
    }
  }

  fn id(message: i64) -> teamcraft_client_api::model::MessageId {
    message.to_string().parse().expect("valid id")
  }
}
