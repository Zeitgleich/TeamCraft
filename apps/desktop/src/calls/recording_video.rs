//! Where the desktop's call hands a recording with video its pictures (#2250, e2ee.md §8.3): the
//! frames of the tracks the webview watches (the tiles it shows), the own camera and screen share
//! as the self views get them, and what the call screen shows (its layout, the active speaker, the
//! label of a tile left out of the recording, drawn by the page).
//!
//! While no recording with video runs, every hand-over is one lock and an empty check: nothing is
//! copied or decoded (robustness.md, "Performance and resources").

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use teamcraft_client_api::calls::{CallLayout, RecordingLabel};
use teamcraft_media::call_recording_video::{OwnSource, VideoRecordingSink};
use teamcraft_media::call_video::ReceivedVideoFrame;
use teamcraft_media::recording_composite::{LabelMask, SceneLayout};
use teamcraft_media::video::VideoFrame;
use teamcraft_proto::{DeviceId, MediaStreamId, VideoTrackKind};

/// The own captures are copied for the recording at most this often: its frame rate (15 fps).
const OWN_PICTURE_INTERVAL: Duration = Duration::from_millis(66);

/// Shared by the call host, the media task and the self views.
#[derive(Clone, Default)]
pub(crate) struct RecordingVideoTap(Arc<Mutex<TapState>>);

#[derive(Default)]
struct TapState {
  maybe_sink: Option<VideoRecordingSink>,
  /// What the call screen shows; kept for the next recording too.
  layout: SceneLayout,
  maybe_label: Option<LabelMask>,
}

impl RecordingVideoTap {
  /// A recording with video started: it gets the pictures from now on, and what the screen shows.
  pub(crate) fn start(&self, sink: VideoRecordingSink) {
    let mut state = self.lock();
    sink.set_layout(state.layout);
    sink.set_label(state.maybe_label.clone());
    state.maybe_sink = Some(sink);
  }

  /// The recording stopped.
  pub(crate) fn stop(&self) {
    self.lock().maybe_sink = None;
  }

  /// What the call screen shows (the page tells it when it changes).
  pub(crate) fn set_view(&self, layout: CallLayout, maybe_label: Option<&RecordingLabel>) {
    let layout = match layout {
      CallLayout::Gallery => SceneLayout::Gallery,
      CallLayout::ActiveSpeaker => SceneLayout::ActiveSpeaker,
    };
    let maybe_label = maybe_label.and_then(|label| {
      LabelMask::new(
        usize::from(label.width),
        usize::from(label.height),
        label.coverage()?,
      )
    });
    let mut state = self.lock();
    state.layout = layout;
    state.maybe_label = maybe_label.clone();
    if let Some(sink) = &state.maybe_sink {
      sink.set_layout(layout);
      sink.set_label(maybe_label);
    }
  }

  /// A frame of a track the webview watches.
  pub(crate) fn received(&self, frame: &ReceivedVideoFrame, kind: &VideoTrackKind) {
    if let Some(sink) = self.sink() {
      sink.push_received(frame, kind);
    }
  }

  /// The webview stopped watching a track.
  pub(crate) fn forget(&self, stream: MediaStreamId) {
    if let Some(sink) = self.sink() {
      sink.forget(stream);
    }
  }

  /// The active speaker.
  pub(crate) fn set_focus(&self, maybe_device: Option<DeviceId>) {
    if let Some(sink) = self.sink() {
      sink.set_focus(maybe_device);
    }
  }

  /// A picture of an own capture, as its self view sees it.
  pub(crate) fn own(&self, source: OwnSource, frame: &VideoFrame) {
    if let Some(sink) = self.sink() {
      sink.push_own(source, frame, OWN_PICTURE_INTERVAL);
    }
  }

  /// An own capture ended.
  pub(crate) fn forget_own(&self, source: OwnSource) {
    if let Some(sink) = self.sink() {
      sink.forget_own(source);
    }
  }

  /// The running recording's sink; a recording that ended is let go here.
  fn sink(&self) -> Option<VideoRecordingSink> {
    let mut state = self.lock();
    if state
      .maybe_sink
      .as_ref()
      .is_some_and(VideoRecordingSink::is_ended)
    {
      state.maybe_sink = None;
    }
    state.maybe_sink.clone()
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, TapState> {
    self.0.lock().unwrap_or_else(PoisonError::into_inner)
  }
}
