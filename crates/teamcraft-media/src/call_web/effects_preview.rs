//! The pre-join check's camera through the background effects (#1295): no call runs yet, so
//! nothing encodes or sends the pictures. A task on the page's event loop takes a picture from the
//! camera's `<video>` at the camera's rate, runs it through the same pipeline a call sends
//! ([`WebEffects`]) and lets it draw into the self view's canvas, whose `captureStream()` the page
//! shows. The camera stays the caller's: a join hands it to the call, and the check's pipeline
//! keeps the self view going until the call's own has taken over.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use tracing::debug;
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlVideoElement, MediaStream};
use web_time::Instant;

use super::video::effects::WebEffects;
use super::video::play_hidden;
use super::{CameraEffects, WebCallError, sleep};
use crate::effects::BackgroundEffect;
use crate::video::CAMERA_FRAMERATE;

/// Runs the effects on a camera for the self view only, until dropped.
pub struct EffectsPreview {
  shared: Rc<Shared>,
}

/// What the owner and the running task share.
#[derive(Default)]
struct Shared {
  is_stopped: Cell<bool>,
  /// Another effect, taken by the task before its next picture.
  maybe_next_effect: RefCell<Option<BackgroundEffect>>,
}

impl EffectsPreview {
  /// Starts drawing `stream`'s pictures with `effects` into their view canvas
  /// ([`CameraEffects::with_view`]; without one the pictures go nowhere). Fails when the camera
  /// doesn't play or the pipeline can't be set up.
  pub async fn start(stream: &MediaStream, effects: CameraEffects) -> Result<Self, WebCallError> {
    let video = play_hidden(stream).await?;
    let pipeline = match WebEffects::new(effects) {
      Ok(pipeline) => pipeline,
      Err(error) => {
        video.set_src_object(None);
        return Err(error);
      },
    };
    let shared = Rc::new(Shared::default());
    spawn_local(run(video, pipeline, Rc::clone(&shared)));
    Ok(Self { shared })
  }

  /// Applies `effect` from the next picture on.
  pub fn set_effect(&self, effect: BackgroundEffect) {
    *self.shared.maybe_next_effect.borrow_mut() = Some(effect);
  }
}

impl Drop for EffectsPreview {
  fn drop(&mut self) {
    self.shared.is_stopped.set(true);
  }
}

/// Draws a picture every frame period until the preview is dropped; a picture the effects fail
/// on (a required background that can't be applied) is left out, so the view keeps the last one.
async fn run(video: HtmlVideoElement, mut pipeline: WebEffects, shared: Rc<Shared>) {
  let period = Duration::from_secs(1) / u32::from(CAMERA_FRAMERATE);
  let mut timestamp_us = 0.0;
  let mut failures_in_a_row = 0_u32;
  while !shared.is_stopped.get() {
    let started = Instant::now();
    if let Some(effect) = shared.maybe_next_effect.borrow_mut().take() {
      pipeline.set_effect(effect);
    }
    // Nothing plays yet (the first picture is still on its way).
    if video.video_width() > 0 {
      match pipeline.frame(&video, timestamp_us).await {
        Ok(frame) => {
          frame.close();
          failures_in_a_row = 0;
        },
        Err(error) => {
          failures_in_a_row = failures_in_a_row.saturating_add(1);
          if failures_in_a_row == 1 {
            debug!(%error, "the check's self view skipped a picture");
          }
        },
      }
      timestamp_us += period.as_secs_f64() * 1_000_000.0;
    }
    sleep(period.saturating_sub(started.elapsed())).await;
  }
  // The camera itself stays on: it is the owner's (and maybe the call's by now).
  video.set_src_object(None);
}
