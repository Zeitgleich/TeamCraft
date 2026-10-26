//! Files dropped on the window from a file manager, read by the shell (#651, #649).
//!
//! The window takes drops as HTML5 drag and drop (main.rs), so the page decides where they go. On
//! Linux, WebKitGTK hands the page a file drop without its files: `dataTransfer.files` is empty and
//! at most the files' `file://` addresses come along, which the page can't read. The shell watches
//! the same drop on the GTK widget (without taking it from the page), keeps the dropped paths, and
//! the page asks for them when its `drop` arrives and reads each file in chunks. Should the page get
//! no `drop` at all, the shell tells it after [`FALLBACK_DELAY`] so the files still reach the drop
//! zone under the pointer.
//!
//! The page can only read files the user dropped on the window just now, never a path it names.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::State;
use tauri::ipc::Response;

/// Tells the page that files were dropped but it didn't ask for them (no `drop` reached it).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const FILES_DROPPED_EVENT: &str = "files-dropped";
/// How long the page has to ask for a drop's files after the native drop before the shell sends
/// [`FILES_DROPPED_EVENT`]; its own `drop` follows within milliseconds.
#[cfg(target_os = "linux")]
const FALLBACK_DELAY: Duration = Duration::from_millis(500);
/// A drop the page hasn't asked for by then is forgotten.
const PENDING_FOR: Duration = Duration::from_secs(10);
/// Files of one drop the page may read; a bigger drop keeps the first ones.
const MAX_FILES_PER_DROP: usize = 100;
/// Largest dropped file the page may read: it holds the file in memory until it is staged for the
/// upload. Bigger files are attached with the + button (the file picker hands over the file itself).
pub const READ_MAX_BYTES: u64 = 512 * 1024 * 1024;
/// One read of a dropped file; the page asks chunk by chunk.
const CHUNK_BYTES: u64 = 8 * 1024 * 1024;

/// The dropped files the page may read, shared by the GTK signal handlers and the commands.
#[derive(Default)]
pub struct DroppedFilesState(Mutex<DropStore>);

/// A dropped file as the page sees it: an ID to read it by, never its path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DroppedFile {
  pub id: u32,
  pub name: String,
  pub size_bytes: u64,
}

#[derive(Default)]
struct DropStore {
  maybe_pending: Option<PendingDrop>,
  /// The files of the drop the page took last, by ID.
  readable: HashMap<u32, PathBuf>,
  next_id: u32,
  /// Counts drops, so a late fallback timer knows whether its drop is still waiting.
  generation: u64,
}

struct PendingDrop {
  paths: Vec<PathBuf>,
  dropped_at: Instant,
  #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
  generation: u64,
}

impl DroppedFilesState {
  /// Files were dropped on the window; returns the drop's generation for [`Self::is_pending`].
  pub fn dropped(&self, mut paths: Vec<PathBuf>, now: Instant) -> u64 {
    paths.truncate(MAX_FILES_PER_DROP);
    let mut store = self.lock();
    store.generation += 1;
    let generation = store.generation;
    store.maybe_pending = Some(PendingDrop {
      paths,
      dropped_at: now,
      generation,
    });
    generation
  }

  /// The files of the drop the page hasn't asked for yet, readable by ID from now on. Folders and
  /// files that are gone are left out. Empty when nothing was dropped recently.
  pub fn take(&self, now: Instant) -> Vec<DroppedFile> {
    let mut store = self.lock();
    let Some(pending) = store.maybe_pending.take() else {
      return Vec::new();
    };
    store.readable.clear();
    if now.saturating_duration_since(pending.dropped_at) > PENDING_FOR {
      return Vec::new();
    }
    let mut files = Vec::new();
    for path in pending.paths {
      // Follows symbolic links: the user dropped what the link points to.
      let Ok(metadata) = std::fs::metadata(&path) else {
        tracing::debug!("a dropped file is gone");
        continue;
      };
      if !metadata.is_file() {
        tracing::debug!("a dropped folder or special file was left out");
        continue;
      }
      let id = store.next_id;
      store.next_id = store.next_id.wrapping_add(1);
      files.push(DroppedFile {
        id,
        name: display_name(&path),
        size_bytes: metadata.len(),
      });
      store.readable.insert(id, path);
    }
    files
  }

  /// The page attached the drop's files itself (the webview handed them over): nothing to read.
  pub fn forget(&self) {
    let mut store = self.lock();
    store.maybe_pending = None;
    store.readable.clear();
  }

  /// Whether the drop of `generation` still waits for the page.
  #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
  pub fn is_pending(&self, generation: u64) -> bool {
    self
      .lock()
      .maybe_pending
      .as_ref()
      .is_some_and(|pending| pending.generation == generation)
  }

  fn path_of(&self, id: u32) -> Option<PathBuf> {
    self.lock().readable.get(&id).cloned()
  }

  fn lock(&self) -> MutexGuard<'_, DropStore> {
    // A panic while holding the lock leaves plain data behind; it stays usable.
    self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }
}

/// The page's `drop` arrived: the files the shell saw dropped, or none (the webview handed them over
/// itself, `is_handled`, or nothing was dropped from outside).
#[tauri::command(rename_all = "snake_case")]
pub async fn dropped_files_take(
  store: State<'_, Arc<DroppedFilesState>>,
  is_handled: bool,
) -> Result<Vec<DroppedFile>, String> {
  if is_handled {
    store.forget();
    return Ok(Vec::new());
  }
  let store = store.inner().clone();
  // File metadata can be slow (network shares); not on the main thread.
  tauri::async_runtime::spawn_blocking(move || store.take(Instant::now()))
    .await
    .map_err(|error| error.to_string())
}

/// One chunk of a dropped file from `offset`, as raw bytes; empty at the end.
#[tauri::command(rename_all = "snake_case")]
pub async fn dropped_file_read(
  store: State<'_, Arc<DroppedFilesState>>,
  id: u32,
  offset: u64,
) -> Result<Response, String> {
  let path = store
    .path_of(id)
    .ok_or_else(|| "the dropped file is no longer readable".to_owned())?;
  let chunk = tauri::async_runtime::spawn_blocking(move || read_chunk(&path, offset))
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| format!("could not read the dropped file: {error}"))?;
  Ok(Response::new(chunk))
}

/// Up to [`CHUNK_BYTES`] of the file at `path` from `offset`, never past [`READ_MAX_BYTES`].
fn read_chunk(path: &Path, offset: u64) -> std::io::Result<Vec<u8>> {
  if offset >= READ_MAX_BYTES {
    return Err(std::io::Error::other("the file is larger than a drop may be"));
  }
  let length = CHUNK_BYTES.min(READ_MAX_BYTES - offset);
  let mut file = File::open(path)?;
  file.seek(SeekFrom::Start(offset))?;
  let mut chunk = Vec::new();
  file.take(length).read_to_end(&mut chunk)?;
  Ok(chunk)
}

/// The local paths a `text/uri-list` names (RFC 2483: one URI per line, `#` comments, CRLF), in
/// order. Other URIs (web addresses, `trash:`, files on another host) are left out.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn paths_from_uri_list(uri_list: &str) -> Vec<PathBuf> {
  local_paths(
    uri_list
      .lines()
      .map(str::trim)
      .filter(|line| !line.is_empty() && !line.starts_with('#')),
  )
}

/// The local paths of `file:` URIs, percent-decoded; other URIs are left out.
pub fn local_paths<'a>(uris: impl IntoIterator<Item = &'a str>) -> Vec<PathBuf> {
  uris
    .into_iter()
    .filter_map(|uri| tauri::Url::parse(uri.trim()).ok())
    .filter(|url| url.scheme() == "file")
    // Fails for a file on another host (`file://server/…`); `localhost` is this machine.
    .filter_map(|url| url.to_file_path().ok())
    .collect()
}

/// The file's name for the attachment chip and the message.
fn display_name(path: &Path) -> String {
  path
    .file_name()
    .map(|name| name.to_string_lossy().into_owned())
    .unwrap_or_default()
}

/// Watches file drops on the WebKitGTK widget without taking them from the page: the handlers run
/// before WebKit's own and let it continue (they answer `false`).
#[cfg(target_os = "linux")]
pub fn watch(webview: &webkit2gtk::WebView, store: Arc<DroppedFilesState>, app: tauri::AppHandle) {
  use std::cell::RefCell;
  use std::rc::Rc;

  use gtk::prelude::WidgetExt;

  let drag = Rc::new(RefCell::new(NativeDrag::default()));
  let commit = Rc::new(move |paths: Vec<PathBuf>| {
    let generation = store.dropped(paths, Instant::now());
    let store = store.clone();
    let app = app.clone();
    gtk::glib::timeout_add_local_once(FALLBACK_DELAY, move || {
      if store.is_pending(generation) {
        notify_page(&app);
      }
    });
  });
  {
    let drag = drag.clone();
    webview.connect_drag_motion(move |_, context, _, _, _| {
      drag.borrow_mut().follow(context);
      false
    });
  }
  {
    let drag = drag.clone();
    let commit = commit.clone();
    webview.connect_drag_data_received(move |_, context, _, _, data, _, _| {
      let paths = selection_paths(data);
      if paths.is_empty() {
        return;
      }
      let maybe_dropped = drag.borrow_mut().received(context, paths);
      if let Some(paths) = maybe_dropped {
        commit(paths);
      }
    });
  }
  webview.connect_drag_drop(move |_, context, _, _, _| {
    let maybe_dropped = drag.borrow_mut().dropped(context);
    if let Some(paths) = maybe_dropped {
      commit(paths);
    }
    false
  });
}

/// The paths of a `text/uri-list` selection; other targets (text, HTML) name no files.
#[cfg(target_os = "linux")]
fn selection_paths(data: &gtk::SelectionData) -> Vec<PathBuf> {
  let uris = data.uris();
  if !uris.is_empty() {
    return local_paths(uris.iter().map(|uri| uri.as_str()));
  }
  if data.data_type().name() == "text/uri-list" {
    return paths_from_uri_list(&String::from_utf8_lossy(&data.data()));
  }
  Vec::new()
}

#[cfg(target_os = "linux")]
fn notify_page(app: &tauri::AppHandle) {
  use tauri::Emitter;

  if let Err(error) = app.emit_to(crate::tray::MAIN_WINDOW, FILES_DROPPED_EVENT, ()) {
    tracing::warn!(%error, "could not tell the page about dropped files");
  }
}

/// One drag over the window: its paths arrive (`drag-data-received`) before or after the drop
/// (`drag-drop`); the drop is committed once it has both.
#[cfg(target_os = "linux")]
#[derive(Default)]
struct NativeDrag {
  maybe_context: Option<gtk::gdk::DragContext>,
  paths: Vec<PathBuf>,
  is_dropped: bool,
}

#[cfg(target_os = "linux")]
impl NativeDrag {
  /// A new drag starts afresh.
  fn follow(&mut self, context: &gtk::gdk::DragContext) {
    if self.maybe_context.as_ref() != Some(context) {
      *self = Self {
        maybe_context: Some(context.clone()),
        ..Self::default()
      };
    }
  }

  /// The drag's paths; the dropped paths if the drop came first.
  fn received(&mut self, context: &gtk::gdk::DragContext, paths: Vec<PathBuf>) -> Option<Vec<PathBuf>> {
    self.follow(context);
    self.paths = paths;
    self.take_dropped()
  }

  /// The drop; its paths if they are known.
  fn dropped(&mut self, context: &gtk::gdk::DragContext) -> Option<Vec<PathBuf>> {
    self.follow(context);
    self.is_dropped = true;
    self.take_dropped()
  }

  fn take_dropped(&mut self) -> Option<Vec<PathBuf>> {
    if !self.is_dropped || self.paths.is_empty() {
      return None;
    }
    self.is_dropped = false;
    Some(std::mem::take(&mut self.paths))
  }
}

#[cfg(test)]
mod tests {
  // Test-only module: a failing step should abort the test with a clear message.
  #![allow(clippy::expect_used, clippy::unwrap_used)]

  use super::*;

  #[cfg(unix)]
  mod uri_lists {
    use super::*;

    #[test]
    fn file_uris_are_percent_decoded() {
      assert_eq!(
        paths_from_uri_list("file:///home/user/My%20Pictures/caf%C3%A9%20%231.png"),
        vec![PathBuf::from("/home/user/My Pictures/café #1.png")]
      );
    }

    #[test]
    fn every_file_of_a_list_is_kept_in_order() {
      // As KDE Dolphin and GNOME Files send it: CRLF line ends, a trailing line end, comments.
      let list =
        "# dragged from a file manager\r\nfile:///tmp/a.png\r\nfile://localhost/tmp/b%2Bc.pdf\r\n\r\n";
      assert_eq!(
        paths_from_uri_list(list),
        vec![PathBuf::from("/tmp/a.png"), PathBuf::from("/tmp/b+c.pdf")]
      );
    }

    #[test]
    fn other_uris_are_ignored() {
      let list = "https://example.com/picture.jpg\ntrash:/a.png\nfile://server.example.com/share/a.png\n\
                  # file:///commented/out.png\nnot a uri\nFILE:///tmp/upper.txt\n";
      assert_eq!(paths_from_uri_list(list), vec![PathBuf::from("/tmp/upper.txt")]);
      assert!(paths_from_uri_list("").is_empty());
    }

    #[test]
    fn gtk_uris_are_read_one_by_one() {
      assert_eq!(
        local_paths([
          "file:///tmp/a%20b.txt",
          "http://example.com/",
          " file:///tmp/c.txt "
        ]),
        vec![PathBuf::from("/tmp/a b.txt"), PathBuf::from("/tmp/c.txt")]
      );
    }
  }

  mod store {
    use super::*;

    #[test]
    fn the_page_reads_only_the_files_just_dropped() {
      let directory = scratch_dir("take");
      let file = directory.join("notes.txt");
      std::fs::write(&file, b"hello").unwrap();
      let state = DroppedFilesState::default();
      let now = Instant::now();

      assert!(state.take(now).is_empty(), "nothing dropped yet");
      state.dropped(
        vec![file.clone(), directory.clone(), directory.join("gone.txt")],
        now,
      );
      let files = state.take(now);

      assert_eq!(files.len(), 1, "folders and missing files are left out");
      assert_eq!(files[0].name, "notes.txt");
      assert_eq!(files[0].size_bytes, 5);
      assert_eq!(state.path_of(files[0].id), Some(file));
      assert!(state.take(now).is_empty(), "a drop is taken once");
      assert_eq!(state.path_of(files[0].id + 1), None);
    }

    #[test]
    fn a_new_drop_replaces_what_was_readable() {
      let directory = scratch_dir("replace");
      let first = directory.join("a.txt");
      std::fs::write(&first, b"a").unwrap();
      let state = DroppedFilesState::default();
      let now = Instant::now();
      state.dropped(vec![first], now);
      let taken = state.take(now);

      state.dropped(Vec::new(), now);
      assert!(state.take(now).is_empty());
      assert_eq!(state.path_of(taken[0].id), None);
    }

    #[test]
    fn an_old_or_handled_drop_is_not_offered() {
      let directory = scratch_dir("stale");
      let file = directory.join("a.txt");
      std::fs::write(&file, b"a").unwrap();
      let state = DroppedFilesState::default();
      let then = Instant::now();

      state.dropped(vec![file.clone()], then);
      assert!(state.take(then + PENDING_FOR + Duration::from_secs(1)).is_empty());

      let generation = state.dropped(vec![file], then);
      assert!(state.is_pending(generation));
      state.forget();
      assert!(
        !state.is_pending(generation),
        "no fallback after the page handled it"
      );
      assert!(state.take(then).is_empty());
    }

    #[test]
    fn a_drop_is_capped() {
      let state = DroppedFilesState::default();
      let paths = (0..MAX_FILES_PER_DROP + 5)
        .map(|i| PathBuf::from(format!("/x/{i}")))
        .collect();
      state.dropped(paths, Instant::now());
      let count = state
        .lock()
        .maybe_pending
        .as_ref()
        .map(|pending| pending.paths.len());
      assert_eq!(count, Some(MAX_FILES_PER_DROP));
    }

    #[test]
    fn chunks_cover_the_file_and_stop_at_its_end() {
      let directory = scratch_dir("chunks");
      let file = directory.join("data.bin");
      std::fs::write(&file, vec![7_u8; 10]).unwrap();

      assert_eq!(read_chunk(&file, 0).unwrap().len(), 10);
      assert_eq!(read_chunk(&file, 4).unwrap(), vec![7_u8; 6]);
      assert!(read_chunk(&file, 10).unwrap().is_empty());
      assert!(read_chunk(&file, READ_MAX_BYTES).is_err());
    }
  }

  /// A fresh directory under the system temp dir for one test.
  fn scratch_dir(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("teamcraft-drop-{name}-{}", std::process::id()));
    drop(std::fs::remove_dir_all(&directory));
    std::fs::create_dir_all(&directory).unwrap();
    directory
  }
}
