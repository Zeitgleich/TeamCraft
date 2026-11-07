//! WebView2 reports a web process that died or hangs with `ICoreWebView2::add_ProcessFailed`
//! (#1214). wry offers no hook for it and Tauri has none like macOS's
//! `on_web_content_process_terminate`, so the shell subscribes here, on the webview Tauri made.
//!
//! What a failure means for the page is the shell's choice (`webview_recovery.rs`); this module
//! only says which process failed. The kinds are WebView2's `COREWEBVIEW2_PROCESS_FAILED_KIND`.

/// Which of the webview's processes failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebviewProcessKind {
  /// The browser process is gone and with it the whole webview: a reload cannot bring it back.
  Browser,
  /// The main frame's render process exited: the page is dead, a reload starts a new process.
  Render,
  /// The main frame's render process stopped responding to input.
  RenderUnresponsive,
  /// The render process of a subframe exited: the main page still runs.
  FrameRender,
  /// A helper process (GPU, utility, sandbox helper, plugin, unknown) exited; WebView2 starts a new
  /// one itself. The raw kind is kept for the log.
  Helper(i32),
}

impl WebviewProcessKind {
  /// WebView2's `COREWEBVIEW2_PROCESS_FAILED_KIND` value. Kinds a newer runtime adds count as helpers:
  /// the page is not known to be gone.
  pub fn from_raw(kind: i32) -> Self {
    match kind {
      0 => Self::Browser,
      1 => Self::Render,
      2 => Self::RenderUnresponsive,
      3 => Self::FrameRender,
      other => Self::Helper(other),
    }
  }
}

/// A failure WebView2 reported. Reason and exit code come from runtimes that know
/// `ICoreWebView2ProcessFailedEventArgs2` (`COREWEBVIEW2_PROCESS_FAILED_REASON`; 5 is out of memory).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WebviewProcessFailure {
  pub kind: WebviewProcessKind,
  pub maybe_reason: Option<i32>,
  pub maybe_exit_code: Option<i32>,
}

/// Why the failure handler could not be installed.
#[cfg(windows)]
#[derive(Debug, thiserror::Error)]
pub enum ProcessFailedWatchError {
  #[error("the WebView2 controller has no webview ({0})")]
  NoWebview(windows::core::Error),
  #[error("WebView2 refused the ProcessFailed handler ({0})")]
  Register(windows::core::Error),
}

#[cfg(windows)]
pub use imp::watch_process_failed;

#[cfg(windows)]
mod imp {
  use std::panic::{AssertUnwindSafe, catch_unwind};

  use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_PROCESS_FAILED_KIND, COREWEBVIEW2_PROCESS_FAILED_REASON, ICoreWebView2Controller,
    ICoreWebView2ProcessFailedEventArgs, ICoreWebView2ProcessFailedEventArgs2,
  };
  use webview2_com::ProcessFailedEventHandler;
  use windows::core::Interface;

  use super::{ProcessFailedWatchError, WebviewProcessFailure, WebviewProcessKind};

  /// Calls `on_failure` on the webview's UI thread each time one of its processes fails, for as long
  /// as the webview lives. `controller` is the one Tauri's `PlatformWebview::controller()` hands out.
  pub fn watch_process_failed(
    controller: &ICoreWebView2Controller,
    on_failure: impl Fn(WebviewProcessFailure) + 'static,
  ) -> Result<(), ProcessFailedWatchError> {
    // SAFETY: a property getter on a live controller (the caller's reference keeps it alive); the
    // returned interface is owned (reference counted) by us.
    #[allow(unsafe_code)]
    let webview = unsafe { controller.CoreWebView2() }.map_err(ProcessFailedWatchError::NoWebview)?;
    let handler = ProcessFailedEventHandler::create(Box::new(move |_sender, maybe_args| {
      let Some(args) = maybe_args else {
        tracing::warn!("WebView2 reported a failed process without saying which");
        return Ok(());
      };
      let Some(failure) = read_failure(&args) else {
        tracing::warn!("could not read which WebView2 process failed");
        return Ok(());
      };
      // A panic must not unwind into WebView2's COM call.
      if catch_unwind(AssertUnwindSafe(|| on_failure(failure))).is_err() {
        tracing::error!("the ProcessFailed callback panicked");
      }
      Ok(())
    }));
    let mut token = 0_i64;
    // SAFETY: `webview` is live; `handler` is a valid COM object that WebView2 references for as long
    // as it keeps it (we never remove it: it lives with the webview); `token` is a valid out slot.
    #[allow(unsafe_code)]
    unsafe { webview.add_ProcessFailed(&handler, &mut token) }.map_err(ProcessFailedWatchError::Register)
  }

  fn read_failure(args: &ICoreWebView2ProcessFailedEventArgs) -> Option<WebviewProcessFailure> {
    let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
    // SAFETY: `args` is valid for the duration of the event; `kind` is a valid out slot.
    #[allow(unsafe_code)]
    unsafe { args.ProcessFailedKind(&mut kind) }.ok()?;
    // Older runtimes have only the first version of the arguments.
    let maybe_args2 = args.cast::<ICoreWebView2ProcessFailedEventArgs2>().ok();
    let maybe_reason = maybe_args2.as_ref().and_then(|args2| {
      let mut reason = COREWEBVIEW2_PROCESS_FAILED_REASON::default();
      // SAFETY: as above; `reason` is a valid out slot.
      #[allow(unsafe_code)]
      unsafe { args2.Reason(&mut reason) }.ok().map(|()| reason.0)
    });
    let maybe_exit_code = maybe_args2.as_ref().and_then(|args2| {
      let mut exit_code = 0_i32;
      // SAFETY: as above; `exit_code` is a valid out slot.
      #[allow(unsafe_code)]
      unsafe { args2.ExitCode(&mut exit_code) }.ok().map(|()| exit_code)
    });
    Some(WebviewProcessFailure {
      kind: WebviewProcessKind::from_raw(kind.0),
      maybe_reason,
      maybe_exit_code,
    })
  }
}

#[cfg(test)]
mod tests {
  use super::WebviewProcessKind;

  #[test]
  fn kinds_are_webview2s() {
    let kinds: Vec<_> = (0..=10).map(WebviewProcessKind::from_raw).collect();
    assert_eq!(
      kinds,
      vec![
        WebviewProcessKind::Browser,
        WebviewProcessKind::Render,
        WebviewProcessKind::RenderUnresponsive,
        WebviewProcessKind::FrameRender,
        WebviewProcessKind::Helper(4),
        WebviewProcessKind::Helper(5),
        WebviewProcessKind::Helper(6),
        WebviewProcessKind::Helper(7),
        WebviewProcessKind::Helper(8),
        WebviewProcessKind::Helper(9),
        WebviewProcessKind::Helper(10),
      ]
    );
  }

  /// The numbers above are WebView2's; a newer `webview2-com` must not have moved them.
  #[cfg(windows)]
  #[test]
  fn kinds_match_the_bindings() {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
      COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED,
      COREWEBVIEW2_PROCESS_FAILED_KIND_FRAME_RENDER_PROCESS_EXITED,
      COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED,
      COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE,
    };
    assert_eq!(COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED.0, 0);
    assert_eq!(COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED.0, 1);
    assert_eq!(COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE.0, 2);
    assert_eq!(COREWEBVIEW2_PROCESS_FAILED_KIND_FRAME_RENDER_PROCESS_EXITED.0, 3);
  }
}
