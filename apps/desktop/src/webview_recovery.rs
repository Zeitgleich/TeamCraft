//! A web process that dies must not leave a dead window (#824, robustness.md "No dead app"). The
//! client core and the call live in the shell, so a reloaded page finds both again; only what the
//! page shows has to come back.
//!
//! Reloading could loop when the same video kills the page every time, so each crash within
//! [`CRASH_WINDOW`] takes away more of what can have caused it: first the page just reloads, then
//! its video comes as JPEG images (the webview's video decoder is out of the picture), then call
//! video stays off with a message. After that the page keeps coming back, more slowly. The choice
//! holds until the app restarts.
//!
//! WebKitGTK 2.54 kills the page when asked whether WebCodecs decodes a codec while its DMABUF
//! renderer is off: `VideoDecoder.isConfigSupported` reaches a release assert in
//! `setGstElementGLContext`. No page can catch that, so the shell keeps the page from asking where
//! it is known to happen, and remembers a crash during the question for this WebKitGTK and
//! GStreamer version where it isn't.
//!
//! Each webview reports a dead page its own way: WebKitGTK's `web-process-terminated`, WKWebView's
//! content process termination, WebView2's `ProcessFailed` (#1214, [`on_process_failed`]).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use teamcraft_client_api::calls::WebviewVideoMode;

use crate::calls::CallHost;

/// Crashes this close together count as one problem that keeps coming back. A page takes seconds to
/// load and to show a call's video again, so a crash loop is well inside it.
const CRASH_WINDOW: Duration = Duration::from_secs(60);
/// How long a page that crashes with call video off waits before it loads again: the window stays
/// usable between the crashes, should the app be usable at all.
const LAST_RESORT_DELAY: Duration = Duration::from_secs(30);
/// In the app's config directory: the versions whose WebCodecs killed the page when asked.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const CRASHED_PROBE_FILE: &str = "webcodecs-crash.json";
/// WebKitGTK's switch for its DMABUF renderer (`startup_env.rs`); WebKit reads anything but `0` as off.
#[cfg(target_os = "linux")]
const DMABUF_VARIABLE: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";

/// What the shell knows about the state of its webview's page.
pub struct WebviewRecovery {
  inner: Mutex<Inner>,
  maybe_probe_note: Option<ProbeNote>,
}

struct Inner {
  crashes: VecDeque<Instant>,
  mode: WebviewVideoMode,
  /// `isConfigSupported` questions the page has open: a crash now is probably theirs.
  open_probes: u32,
}

/// Where the versions of a crashing probe are kept, and the versions in use.
struct ProbeNote {
  path: PathBuf,
  versions: DecoderVersions,
}

/// The two libraries a WebCodecs decoder of WebKitGTK is made of: an update of either may fix it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct DecoderVersions {
  webkit: String,
  gstreamer: String,
}

/// When to load the page again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reload {
  Now,
  After(Duration),
}

impl WebviewRecovery {
  fn new(mode: WebviewVideoMode, maybe_probe_note: Option<ProbeNote>) -> Self {
    Self {
      inner: Mutex::new(Inner {
        crashes: VecDeque::new(),
        mode,
        open_probes: 0,
      }),
      maybe_probe_note,
    }
  }

  /// What this start knows from its environment and from earlier crashes.
  #[cfg(target_os = "linux")]
  pub fn for_this_system(app: &AppHandle) -> Self {
    let is_dmabuf_off = is_dmabuf_disabled(std::env::var_os(DMABUF_VARIABLE).as_deref());
    let maybe_probe_note = app.path().app_config_dir().ok().map(|directory| ProbeNote {
      path: directory.join(CRASHED_PROBE_FILE),
      versions: DecoderVersions::in_use(),
    });
    let has_crashed_before = maybe_probe_note.as_ref().is_some_and(ProbeNote::applies);
    let mode = start_mode(is_dmabuf_off, has_crashed_before);
    if mode != WebviewVideoMode::Automatic {
      tracing::info!(
        is_dmabuf_off,
        has_crashed_before,
        "call video comes as images: this WebCodecs is not safe to ask"
      );
    }
    Self::new(mode, maybe_probe_note)
  }

  /// The webviews of macOS and Windows have no known crash when asked.
  #[cfg(not(target_os = "linux"))]
  pub fn for_this_system(_app: &AppHandle) -> Self {
    Self::new(WebviewVideoMode::Automatic, None)
  }

  pub fn mode(&self) -> WebviewVideoMode {
    self.lock().mode
  }

  /// The page is about to ask whether WebCodecs decodes a codec. `false`: it must not ask.
  pub fn begin_probe(&self) -> bool {
    let mut inner = self.lock();
    let is_safe = inner.mode == WebviewVideoMode::Automatic;
    if is_safe {
      inner.open_probes += 1;
    }
    is_safe
  }

  /// The page has its answer.
  pub fn end_probe(&self) {
    let mut inner = self.lock();
    inner.open_probes = inner.open_probes.saturating_sub(1);
  }

  /// A new page starts: questions the last one had open won't be answered any more.
  pub fn page_started(&self) {
    self.lock().open_probes = 0;
  }

  /// The web process is gone: what the reloaded page may do from now on, and when it loads.
  pub fn process_terminated(&self, now: Instant) -> Reload {
    let mut inner = self.lock();
    inner
      .crashes
      .retain(|crash| now.saturating_duration_since(*crash) < CRASH_WINDOW);
    inner.crashes.push_back(now);
    let was_probing = std::mem::take(&mut inner.open_probes) > 0;
    let (mode, reload) = recovery(inner.crashes.len(), was_probing, inner.mode);
    inner.mode = mode;
    drop(inner);
    if was_probing && let Some(note) = &self.maybe_probe_note {
      note.write();
    }
    reload
  }

  fn lock(&self) -> MutexGuard<'_, Inner> {
    self.inner.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

/// The page's video from now on, and when it loads again, after `crashes` crashes in a row.
fn recovery(crashes: usize, was_probing: bool, mode: WebviewVideoMode) -> (WebviewVideoMode, Reload) {
  let needed = match crashes {
    0 | 1 if was_probing => WebviewVideoMode::Images,
    0 | 1 => WebviewVideoMode::Automatic,
    2 => WebviewVideoMode::Images,
    _ => WebviewVideoMode::Off,
  };
  let reload = if crashes > 3 {
    Reload::After(LAST_RESORT_DELAY)
  } else {
    Reload::Now
  };
  (mode.max(needed), reload)
}

/// How a start's page may show video before anything has crashed in this run.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn start_mode(is_dmabuf_off: bool, has_crashed_before: bool) -> WebviewVideoMode {
  if is_dmabuf_off || has_crashed_before {
    WebviewVideoMode::Images
  } else {
    WebviewVideoMode::Automatic
  }
}

/// Whether the value of [`DMABUF_VARIABLE`] turns the renderer off, as WebKit reads it.
#[cfg(target_os = "linux")]
fn is_dmabuf_disabled(maybe_value: Option<&std::ffi::OsStr>) -> bool {
  maybe_value.is_some_and(|value| value != "0")
}

impl ProbeNote {
  /// Whether a probe crashed the page with these very versions.
  #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
  fn applies(&self) -> bool {
    std::fs::read(&self.path)
      .ok()
      .and_then(|bytes| serde_json::from_slice::<DecoderVersions>(&bytes).ok())
      .is_some_and(|versions| versions == self.versions)
  }

  /// Without the note the next start asks again and costs one reload: not worth more than a log.
  fn write(&self) {
    let written = serde_json::to_vec(&self.versions)
      .map_err(std::io::Error::other)
      .and_then(|bytes| {
        if let Some(directory) = self.path.parent() {
          std::fs::create_dir_all(directory)?;
        }
        std::fs::write(&self.path, bytes)
      });
    match written {
      Ok(()) => tracing::warn!(
        webkit = %self.versions.webkit,
        gstreamer = %self.versions.gstreamer,
        "asking WebCodecs what it decodes killed the page; call video comes as images on these versions"
      ),
      Err(error) => tracing::warn!(%error, "could not note the crash of the WebCodecs question"),
    }
  }
}

#[cfg(target_os = "linux")]
impl DecoderVersions {
  fn in_use() -> Self {
    Self {
      webkit: teamcraft_desktop_native::webkit_version(),
      gstreamer: teamcraft_media::playback_gstreamer::gstreamer_version(),
    }
  }
}

/// Reloads the page when its web process ends, whatever ended it.
#[cfg(target_os = "linux")]
pub fn watch(webview: &webkit2gtk::WebView, app: AppHandle) {
  use webkit2gtk::WebViewExt;
  webview.connect_web_process_terminated(move |webview, reason| {
    let reload = recover(&app);
    // No content here: only the reason and the next step.
    tracing::warn!(
      ?reason,
      ?reload,
      "the webview's web process ended; loading the page again"
    );
    let webview = webview.clone();
    // Not from inside the signal: WebKit is still cleaning up the process.
    let load = move || webview.reload();
    match reload {
      Reload::Now => {
        gtk::glib::idle_add_local_once(load);
      },
      Reload::After(delay) => {
        gtk::glib::timeout_add_local_once(delay, load);
      },
    }
  });
}

/// macOS kills web content processes it needs the memory of, and they die like any others.
#[cfg(target_os = "macos")]
pub fn on_process_terminated(webview: &tauri::Webview) {
  let reload = recover(webview.app_handle());
  tracing::warn!(?reload, "the webview's web process ended; loading the page again");
  let webview = webview.clone();
  reload_page(reload, move || webview.reload());
}

/// What a failed WebView2 process means for the page.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failure {
  /// The page is gone: it loads again, and the crash counts like one on Linux and macOS.
  PageLost,
  /// The webview itself is gone; a reload would only talk to a dead browser process.
  WebviewLost,
  /// The page still runs: a subframe or a helper process that WebView2 starts again by itself.
  PageRuns,
}

/// A hung main page is as dead as a crashed one for whoever looks at it, and WebView2 reports it
/// only once someone tried to use it; reloading gives it a new render process. A dead frame leaves
/// the page running (its frame shows an error), and a helper process (GPU, utility, ...) comes back
/// on its own.
#[cfg_attr(not(windows), allow(dead_code))]
fn failure(kind: teamcraft_desktop_native::WebviewProcessKind) -> Failure {
  use teamcraft_desktop_native::WebviewProcessKind;
  match kind {
    WebviewProcessKind::Render | WebviewProcessKind::RenderUnresponsive => Failure::PageLost,
    WebviewProcessKind::Browser => Failure::WebviewLost,
    WebviewProcessKind::FrameRender | WebviewProcessKind::Helper(_) => Failure::PageRuns,
  }
}

/// WebView2 reports a failed process (on the UI thread): a lost page loads again like on Linux and
/// macOS.
#[cfg(windows)]
pub fn on_process_failed(
  window: &tauri::WebviewWindow,
  failure_report: teamcraft_desktop_native::WebviewProcessFailure,
) {
  let teamcraft_desktop_native::WebviewProcessFailure {
    kind,
    maybe_reason,
    maybe_exit_code,
  } = failure_report;
  match failure(kind) {
    Failure::PageLost => {
      let reload = recover(window.app_handle());
      tracing::warn!(
        ?kind,
        ?maybe_reason,
        ?maybe_exit_code,
        ?reload,
        "the webview's web process failed; loading the page again"
      );
      let window = window.clone();
      reload_page(reload, move || window.reload());
    },
    // Only a new window would bring it back (follow-up of #1214); the call goes on in the shell.
    Failure::WebviewLost => tracing::error!(
      ?maybe_reason,
      ?maybe_exit_code,
      "the webview's browser process ended; the window stays empty until the app restarts"
    ),
    Failure::PageRuns => tracing::info!(
      ?kind,
      ?maybe_reason,
      ?maybe_exit_code,
      "a webview helper process ended; the page goes on"
    ),
  }
}

/// Loads the page again now or after the delay of a crash loop.
#[cfg(any(target_os = "macos", windows))]
fn reload_page(reload: Reload, load: impl FnOnce() -> tauri::Result<()> + Send + 'static) {
  let load = move || {
    if let Err(error) = load() {
      tracing::warn!(%error, "could not load the page again");
    }
  };
  match reload {
    Reload::Now => load(),
    Reload::After(delay) => {
      tauri::async_runtime::spawn(async move {
        tokio::time::sleep(delay).await;
        load();
      });
    },
  }
}

/// The dead page's watchers go, and the state says what the next page may do.
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn recover(app: &AppHandle) -> Reload {
  app.state::<Arc<CallHost>>().forget_page_watchers();
  app
    .state::<Arc<WebviewRecovery>>()
    .process_terminated(Instant::now())
}

/// A page of the main window starts loading: nobody of the page before it watches anything.
pub fn page_started(app: &AppHandle) {
  app.state::<Arc<CallHost>>().forget_page_watchers();
  app.state::<Arc<WebviewRecovery>>().page_started();
}

/// How the page shows call video.
#[tauri::command(rename_all = "snake_case")]
pub fn webview_video_mode(recovery: State<'_, Arc<WebviewRecovery>>) -> WebviewVideoMode {
  recovery.mode()
}

/// See [`WebviewRecovery::begin_probe`].
#[tauri::command(rename_all = "snake_case")]
pub fn webview_codec_probe_begin(recovery: State<'_, Arc<WebviewRecovery>>) -> bool {
  recovery.begin_probe()
}

/// See [`WebviewRecovery::end_probe`].
#[tauri::command(rename_all = "snake_case")]
pub fn webview_codec_probe_end(recovery: State<'_, Arc<WebviewRecovery>>) {
  recovery.end_probe();
}

#[cfg(test)]
mod tests {
  // Test-only module: a failing step should abort the test with a clear message.
  #![allow(clippy::expect_used, clippy::unwrap_used)]

  use super::*;
  use crate::auth::test_dir::TempDir;

  mod crash_loop {
    use super::*;

    #[test]
    fn each_crash_in_a_row_takes_more_away() {
      let recovery = recovery_without_note(WebviewVideoMode::Automatic);
      let start = Instant::now();
      let steps: Vec<_> = (0..5)
        .map(|crash| {
          let reload = recovery.process_terminated(start + Duration::from_secs(crash * 5));
          (recovery.mode(), reload)
        })
        .collect();
      assert_eq!(
        steps,
        vec![
          (WebviewVideoMode::Automatic, Reload::Now),
          (WebviewVideoMode::Images, Reload::Now),
          (WebviewVideoMode::Off, Reload::Now),
          (WebviewVideoMode::Off, Reload::After(LAST_RESORT_DELAY)),
          (WebviewVideoMode::Off, Reload::After(LAST_RESORT_DELAY)),
        ]
      );
    }

    #[test]
    fn crashes_far_apart_only_reload() {
      let recovery = recovery_without_note(WebviewVideoMode::Automatic);
      let start = Instant::now();
      for crash in 0..5 {
        let at = start + CRASH_WINDOW * crash;
        assert_eq!(recovery.process_terminated(at), Reload::Now);
        assert_eq!(recovery.mode(), WebviewVideoMode::Automatic);
      }
    }

    #[test]
    fn what_was_taken_away_stays_until_the_app_restarts() {
      let recovery = recovery_without_note(WebviewVideoMode::Automatic);
      let start = Instant::now();
      recovery.process_terminated(start);
      recovery.process_terminated(start + Duration::from_secs(1));
      assert_eq!(recovery.mode(), WebviewVideoMode::Images);
      recovery.process_terminated(start + CRASH_WINDOW * 10);
      assert_eq!(recovery.mode(), WebviewVideoMode::Images);
    }

    #[test]
    fn a_long_crash_loop_remembers_only_its_window() {
      let recovery = recovery_without_note(WebviewVideoMode::Off);
      let start = Instant::now();
      for crash in 0..1000 {
        recovery.process_terminated(start + LAST_RESORT_DELAY * crash);
      }
      assert!(recovery.lock().crashes.len() <= 3);
    }
  }

  mod codec_probe {
    use super::*;

    #[test]
    fn a_crash_while_asking_means_images_at_once() {
      let recovery = recovery_without_note(WebviewVideoMode::Automatic);
      assert!(recovery.begin_probe());
      assert_eq!(recovery.process_terminated(Instant::now()), Reload::Now);
      assert_eq!(recovery.mode(), WebviewVideoMode::Images);
      assert!(!recovery.begin_probe(), "the next page must not ask");
    }

    #[test]
    fn a_crash_after_the_answer_is_not_the_questions() {
      let recovery = recovery_without_note(WebviewVideoMode::Automatic);
      assert!(recovery.begin_probe());
      recovery.end_probe();
      recovery.process_terminated(Instant::now());
      assert_eq!(recovery.mode(), WebviewVideoMode::Automatic);
    }

    #[test]
    fn a_new_page_forgets_the_questions_of_the_last_one() {
      let recovery = recovery_without_note(WebviewVideoMode::Automatic);
      assert!(recovery.begin_probe());
      recovery.page_started();
      recovery.process_terminated(Instant::now());
      assert_eq!(recovery.mode(), WebviewVideoMode::Automatic);
    }

    #[test]
    fn a_page_that_answers_twice_counts_no_further_down_than_zero() {
      let recovery = recovery_without_note(WebviewVideoMode::Automatic);
      recovery.end_probe();
      assert!(recovery.begin_probe());
      recovery.process_terminated(Instant::now());
      assert_eq!(recovery.mode(), WebviewVideoMode::Images);
    }

    #[test]
    fn images_and_off_never_ask() {
      for mode in [WebviewVideoMode::Images, WebviewVideoMode::Off] {
        assert!(!recovery_without_note(mode).begin_probe());
      }
    }
  }

  mod probe_note {
    use super::*;

    #[test]
    fn a_crash_while_asking_is_remembered_for_these_versions() {
      let directory = temp_dir("remembered");
      let recovery = WebviewRecovery::new(
        WebviewVideoMode::Automatic,
        Some(note(&directory, "2.54.2", "1.28.8")),
      );
      assert!(recovery.begin_probe());
      recovery.process_terminated(Instant::now());
      assert!(note(&directory, "2.54.2", "1.28.8").applies());
    }

    #[test]
    fn an_update_of_either_library_asks_again() {
      let directory = temp_dir("update");
      note(&directory, "2.54.2", "1.28.8").write();
      assert!(!note(&directory, "2.54.3", "1.28.8").applies());
      assert!(!note(&directory, "2.54.2", "1.28.9").applies());
    }

    #[test]
    fn a_crash_after_the_answer_writes_no_note() {
      let directory = temp_dir("no-note");
      let recovery = WebviewRecovery::new(
        WebviewVideoMode::Automatic,
        Some(note(&directory, "2.54.2", "1.28.8")),
      );
      recovery.process_terminated(Instant::now());
      assert!(!note(&directory, "2.54.2", "1.28.8").applies());
    }

    #[test]
    fn a_missing_or_damaged_note_applies_to_nothing() {
      let directory = temp_dir("damaged");
      let note = note(&directory, "2.54.2", "1.28.8");
      assert!(!note.applies());
      std::fs::write(&note.path, b"{ not json").unwrap();
      assert!(!note.applies());
    }

    #[test]
    fn a_note_that_cannot_be_written_changes_nothing_else() {
      let directory = temp_dir("unwritable");
      // A file where the note's folder should be.
      std::fs::write(directory.0.join("file"), b"").unwrap();
      let recovery = WebviewRecovery::new(
        WebviewVideoMode::Automatic,
        Some(ProbeNote {
          path: directory.0.join("file").join(CRASHED_PROBE_FILE),
          versions: versions("2.54.2", "1.28.8"),
        }),
      );
      assert!(recovery.begin_probe());
      assert_eq!(recovery.process_terminated(Instant::now()), Reload::Now);
      assert_eq!(recovery.mode(), WebviewVideoMode::Images);
    }
  }

  mod webview2 {
    use teamcraft_desktop_native::WebviewProcessKind;

    use super::*;

    #[test]
    fn a_dead_or_hung_page_loads_again() {
      assert_eq!(failure(WebviewProcessKind::Render), Failure::PageLost);
      assert_eq!(failure(WebviewProcessKind::RenderUnresponsive), Failure::PageLost);
    }

    #[test]
    fn a_dead_browser_process_cannot_be_reloaded() {
      assert_eq!(failure(WebviewProcessKind::Browser), Failure::WebviewLost);
    }

    #[test]
    fn frames_and_helpers_leave_the_page_alone() {
      assert_eq!(failure(WebviewProcessKind::FrameRender), Failure::PageRuns);
      for raw in [4, 5, 6, 7, 8, 9, 42] {
        assert_eq!(failure(WebviewProcessKind::from_raw(raw)), Failure::PageRuns);
      }
    }
  }

  mod start {
    use super::*;

    #[test]
    fn the_page_may_ask_only_where_nothing_speaks_against_it() {
      assert_eq!(start_mode(false, false), WebviewVideoMode::Automatic);
      assert_eq!(start_mode(true, false), WebviewVideoMode::Images);
      assert_eq!(start_mode(false, true), WebviewVideoMode::Images);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_switch_is_read_as_webkit_reads_it() {
      use std::ffi::OsStr;
      assert!(!is_dmabuf_disabled(None));
      assert!(!is_dmabuf_disabled(Some(OsStr::new("0"))));
      assert!(is_dmabuf_disabled(Some(OsStr::new("1"))));
      assert!(is_dmabuf_disabled(Some(OsStr::new(""))));
    }
  }

  fn recovery_without_note(mode: WebviewVideoMode) -> WebviewRecovery {
    WebviewRecovery::new(mode, None)
  }

  fn versions(webkit: &str, gstreamer: &str) -> DecoderVersions {
    DecoderVersions {
      webkit: webkit.to_owned(),
      gstreamer: gstreamer.to_owned(),
    }
  }

  fn note(directory: &TempDir, webkit: &str, gstreamer: &str) -> ProbeNote {
    ProbeNote {
      path: directory.0.join(CRASHED_PROBE_FILE),
      versions: versions(webkit, gstreamer),
    }
  }

  fn temp_dir(name: &str) -> TempDir {
    let directory = TempDir::new(&format!("webview-recovery-{name}"));
    std::fs::create_dir_all(&directory.0).unwrap();
    directory
  }
}
