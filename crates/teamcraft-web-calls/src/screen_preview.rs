//! The preview of the own screen share in the browser (#1175): the capture itself, played muted in
//! a `<video>`, so nothing is decoded a second time. The video element gets a clone of the
//! capture's track, limited to [`MAX_FRAMES_PER_SECOND`], without touching what the call sends.
//!
//! A clone keeps the browser capturing (and showing that it does) after the original stopped, so
//! this is the clone's only owner and it lives no longer than it is looked at: from [`show`] until
//! [`hide`] or the end of the share. While the tab is hidden or the preview collapsed there is none.
//!
//! [`show`]: ScreenPreview::show
//! [`hide`]: ScreenPreview::hide

use js_sys::{Object, Reflect};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
  HtmlMediaElement, HtmlVideoElement, MediaStream, MediaStreamTrack, MediaStreamTrackState,
  MediaTrackConstraints,
};

/// Enough to see that the share runs and what it shows, and a mirror of the own window turns slowly.
const MAX_FRAMES_PER_SECOND: u32 = 5;

/// What this device shares, and the element that plays it while somebody looks.
#[derive(Default)]
pub(crate) struct ScreenPreview {
  maybe_stream: Option<MediaStream>,
  maybe_video: Option<HtmlVideoElement>,
  /// The clone the element plays.
  maybe_track: Option<MediaStreamTrack>,
  is_shown: bool,
}

impl ScreenPreview {
  /// The share now sends `stream`: an open preview follows, whatever the source was before.
  pub(crate) fn set_stream(&mut self, stream: MediaStream) {
    self.let_go();
    self.maybe_stream = Some(stream);
    if self.is_shown {
      self.play();
    }
  }

  /// The share is over.
  pub(crate) fn end_stream(&mut self) {
    self.let_go();
    self.maybe_stream = None;
  }

  /// The element playing the share; `None` while nothing is shared.
  pub(crate) fn show(&mut self) -> Option<HtmlVideoElement> {
    self.maybe_stream.as_ref()?;
    self.is_shown = true;
    if self.maybe_track.is_none() {
      self.play();
    }
    self.maybe_video.clone()
  }

  /// Out of sight: the clone stops, the capture goes on for the call.
  pub(crate) fn hide(&mut self) {
    self.is_shown = false;
    self.let_go();
  }

  /// Whether the capture delivers pictures to the element. The browser mutes a capture it has no
  /// pictures for; a screen that just doesn't change stays unmuted.
  pub(crate) fn has_picture(&self) -> bool {
    let is_delivering = self
      .maybe_track
      .as_ref()
      .is_some_and(|track| track.ready_state() == MediaStreamTrackState::Live && !track.muted());
    let has_size = self
      .maybe_video
      .as_ref()
      .is_some_and(|video| video.video_width() > 0);
    is_delivering && has_size
  }

  fn play(&mut self) {
    let Some(video) = self.video_element() else {
      return;
    };
    let Some(track) = self.maybe_stream.as_ref().and_then(first_video_track) else {
      return;
    };
    // The inherent method: another track on the same capture, not another handle.
    let clone = MediaStreamTrack::clone(&track);
    limit_frame_rate(&clone);
    let Ok(stream) = MediaStream::new() else {
      clone.stop();
      return;
    };
    stream.add_track(&clone);
    video.set_src_object(Some(&stream));
    self.maybe_track = Some(clone);
  }

  fn let_go(&mut self) {
    if let Some(track) = self.maybe_track.take() {
      track.stop();
    }
    if let Some(video) = &self.maybe_video {
      video.set_src_object(None);
    }
  }

  fn video_element(&mut self) -> Option<HtmlVideoElement> {
    if self.maybe_video.is_none() {
      self.maybe_video = new_video();
    }
    self.maybe_video.clone()
  }
}

fn new_video() -> Option<HtmlVideoElement> {
  let video: HtmlVideoElement = web_sys::window()?
    .document()?
    .create_element("video")
    .ok()?
    .dyn_into()
    .ok()?;
  let media: &HtmlMediaElement = video.as_ref();
  media.set_muted(true);
  media.set_autoplay(true);
  // iOS Safari plays inline only with this attribute.
  let _ = video.set_attribute("playsinline", "");
  Some(video)
}

fn first_video_track(stream: &MediaStream) -> Option<MediaStreamTrack> {
  stream.get_video_tracks().iter().next()?.dyn_into().ok()
}

/// Asks for a lower frame rate on the clone only. A browser that won't do it plays all frames,
/// which costs a little more and shows the same.
fn limit_frame_rate(track: &MediaStreamTrack) {
  let Some(constraints) = frame_rate_limit() else {
    return;
  };
  let Ok(applied) = track.apply_constraints_with_constraints(&constraints) else {
    return;
  };
  spawn_local(async move {
    if let Err(error) = JsFuture::from(applied).await {
      tracing::debug!(?error, "the share preview keeps the capture's frame rate");
    }
  });
}

fn frame_rate_limit() -> Option<MediaTrackConstraints> {
  let limit = Object::new();
  Reflect::set(
    &limit,
    &JsValue::from_str("max"),
    &JsValue::from(MAX_FRAMES_PER_SECOND),
  )
  .ok()?;
  let constraints = Object::new();
  Reflect::set(&constraints, &JsValue::from_str("frameRate"), &limit).ok()?;
  Some(constraints.unchecked_into())
}
