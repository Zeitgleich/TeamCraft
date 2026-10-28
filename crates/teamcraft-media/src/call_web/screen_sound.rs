//! A screen share's sound in the browser (#441): the tab's or the system's audio that
//! `getDisplayMedia` captured with the picture, sent as the screen audio stream like the desktop's
//! (`CallEngine::publish_screen_audio`). Receivers play it like any shared screen's sound.
//!
//! Chromium captures a tab's sound everywhere and the whole system's on Windows and ChromeOS (on
//! macOS only a tab's); Safari and Firefox capture none. Where the browser gives no audio track
//! (or the user unticked "share audio"), the picture goes alone and the app shows the sound as
//! unavailable.
//!
//! Echo: the call's own tab is not offered for sharing (`selfBrowserSurface: "exclude"`), and the
//! page's own playback (the call) is kept out of the captured system sound where the browser can
//! (`restrictOwnAudio`); the shared tab keeps playing for the sharer
//! (`suppressLocalAudioPlayback: false`), as the desktop leaves the computer's playback alone.
//!
//! The samples take the microphone's way (an AudioWorklet in the call's 48 kHz context, which
//! resamples and mixes stereo down to mono) into a WebCodecs Opus encoder tuned like the desktop's
//! for shared sound: 64 kbit/s at a constant bitrate, no silence suppression.

use futures::channel::mpsc;
use js_sys::Array;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{AudioContext, MediaStream, MediaStreamTrack};

use super::{Capture, CaptureMix, Input, OpusEncoder, OpusProfile, WebCallError, browser};

/// The audio track's constraints when sound is asked for: the sound as it plays, not processed
/// like a microphone, still audible for the sharer, and without the page's own playback.
const SOUND_TRACK_CONSTRAINTS: [(&str, bool); 5] = [
  ("echoCancellation", false),
  ("noiseSuppression", false),
  ("autoGainControl", false),
  ("suppressLocalAudioPlayback", false),
  ("restrictOwnAudio", true),
];

/// `getDisplayMedia` options beside the picture when sound is asked for: the call's own tab is not
/// offered (its sound would loop back into the call), the whole system's sound is.
const SOUND_REQUEST_OPTIONS: [(&str, &str); 2] =
  [("selfBrowserSurface", "exclude"), ("systemAudio", "include")];

/// What a screen request asks for besides the picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SoundRequest {
  /// Members of the audio track's `MediaTrackConstraints`.
  pub track: &'static [(&'static str, bool)],
  /// Members of `DisplayMediaStreamOptions`.
  pub options: &'static [(&'static str, &'static str)],
}

/// The sound part of a screen request: nothing when sound wasn't asked for, so the picker shows no
/// "share audio" choice and the request stays the video-only one.
pub(super) fn sound_request(has_sound: bool) -> Option<SoundRequest> {
  has_sound.then_some(SoundRequest {
    track: &SOUND_TRACK_CONSTRAINTS,
    options: &SOUND_REQUEST_OPTIONS,
  })
}

/// The first audio track of a picked screen, if the browser captured its sound.
pub(super) fn sound_track(stream: &MediaStream) -> Option<MediaStreamTrack> {
  stream.get_audio_tracks().get(0).dyn_into().ok()
}

/// The shared screen's sound on its way to the engine.
pub(super) struct ScreenSound {
  capture: Capture,
  pub encoder: OpusEncoder,
  track: MediaStreamTrack,
  _ended: Closure<dyn FnMut()>,
}

impl ScreenSound {
  /// Captures and encodes `track`; its samples arrive as [`Input::Captured`] and its packets as
  /// [`Input::Encoded`] with [`crate::call_engine::AudioOrigin::Screen`]. When the browser ends
  /// the track by itself, [`Input::ScreenSoundEnded`] says so.
  pub fn start(
    context: &AudioContext,
    track: MediaStreamTrack,
    inputs: &mpsc::UnboundedSender<Input>,
  ) -> Result<Self, WebCallError> {
    let stream = MediaStream::new_with_tracks(&Array::of1(&track)).map_err(browser("screen sound"))?;
    let encoder = OpusEncoder::new(inputs, OpusProfile::ScreenSound)?;
    let capture = Capture::from_stream(context, stream, CaptureMix::DownToMono, inputs)?;
    let sender = inputs.clone();
    let ended = Closure::<dyn FnMut()>::new(move || {
      // The call task may be gone; the sound stops with it.
      let _ = sender.unbounded_send(Input::ScreenSoundEnded);
    });
    track.set_onended(Some(ended.as_ref().unchecked_ref()));
    Ok(Self {
      capture,
      encoder,
      track,
      _ended: ended,
    })
  }

  /// Stops capturing (the browser's capture of the sound ends with it).
  pub fn stop(self) {
    self.track.set_onended(None);
    self.capture.close();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_share_without_sound_asks_for_the_picture_alone() {
    assert_eq!(sound_request(false), None);
  }

  #[test]
  fn a_share_with_sound_asks_for_the_sound_as_it_plays() {
    let request = sound_request(true).expect("sound is asked for");
    let track = |name: &str| {
      request
        .track
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| *value)
    };
    // Music is not speech: no processing meant for microphones.
    assert_eq!(track("echoCancellation"), Some(false));
    assert_eq!(track("noiseSuppression"), Some(false));
    assert_eq!(track("autoGainControl"), Some(false));
    // The sharer keeps hearing the shared tab.
    assert_eq!(track("suppressLocalAudioPlayback"), Some(false));
    // The call's own playback doesn't go back into the call.
    assert_eq!(track("restrictOwnAudio"), Some(true));
  }

  #[test]
  fn the_call_tab_itself_is_not_offered_with_sound() {
    let request = sound_request(true).expect("sound is asked for");
    assert!(request.options.contains(&("selfBrowserSurface", "exclude")));
    assert!(request.options.contains(&("systemAudio", "include")));
  }
}
