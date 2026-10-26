//! The pre-join check's microphone in the browser (#442): the microphone, as the call would open
//! it, into an `AnalyserNode` that the level meter reads. Nothing is encoded or sent.

use wasm_bindgen::JsCast;
use web_sys::{AnalyserNode, AudioContext, MediaStream, MediaStreamAudioSourceNode, MediaStreamTrack};

use super::{WebCallError, browser, capture_microphone};
use crate::audio_io::AudioDeviceChoice;
use crate::audio_level::meter_level;

/// Samples one reading looks at: about 43 ms at 48 kHz, enough for a steady level.
const ANALYSER_SAMPLES: u32 = 2048;

/// A microphone measured for the meter; dropping it releases the microphone.
pub struct MicrophoneMeter {
  context: AudioContext,
  stream: MediaStream,
  analyser: AnalyserNode,
  _source: MediaStreamAudioSourceNode,
}

impl MicrophoneMeter {
  /// Opens the microphone (the browser may ask) and starts measuring it.
  pub async fn open(choice: &AudioDeviceChoice) -> Result<Self, WebCallError> {
    let stream = capture_microphone(choice).await?;
    match Self::measure(stream.clone()) {
      Ok(meter) => Ok(meter),
      Err(error) => {
        stop_tracks(&stream);
        Err(error)
      },
    }
  }

  /// The level right now, from 0.0 (silence) to 1.0 (full scale).
  pub fn level(&self) -> f32 {
    let mut samples = vec![0.0; self.analyser.fft_size() as usize];
    self.analyser.get_float_time_domain_data(&mut samples);
    meter_level(&samples)
  }

  fn measure(stream: MediaStream) -> Result<Self, WebCallError> {
    let context = AudioContext::new().map_err(browser("AudioContext"))?;
    let source = context
      .create_media_stream_source(&stream)
      .map_err(browser("microphone source"))?;
    let analyser = context.create_analyser().map_err(browser("analyser"))?;
    analyser.set_fft_size(ANALYSER_SAMPLES);
    // Not connected to the speakers: the analyser runs without, and the user doesn't hear
    // themselves.
    source
      .connect_with_audio_node(&analyser)
      .map_err(browser("meter graph"))?;
    // A context made outside a click may start suspended; the open microphone lets it run.
    let _ = context.resume();
    Ok(Self {
      context,
      stream,
      analyser,
      _source: source,
    })
  }
}

impl Drop for MicrophoneMeter {
  fn drop(&mut self) {
    stop_tracks(&self.stream);
    // Closing releases the audio hardware; the promise only says when that finished.
    let _ = self.context.close();
  }
}

fn stop_tracks(stream: &MediaStream) {
  for track in stream.get_tracks().iter() {
    track.unchecked_into::<MediaStreamTrack>().stop();
  }
}
