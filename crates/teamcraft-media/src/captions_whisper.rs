//! whisper.cpp as the recognizer of live captions (#215), on the CPU.
//!
//! Each piece of a sentence is recognized on its own (no context from the one before, so a
//! misheard sentence doesn't pull the next one along), greedy, in one segment. Whisper invents
//! text for noise and silence ("Thank you.", subtitle credits): the segmenter only hands it
//! speech, and segments it rates as most likely no speech, or that are only a sound annotation
//! ("[Music]", "(laughs)"), are dropped.
//!
//! whisper.cpp's log is silenced, not forwarded: its debug builds print tokens, and recognized
//! text must never reach a log.

use std::path::Path;
use std::sync::Once;

use thiserror::Error;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

use crate::captions::{RecognizeError, SpeechRecognizer};

/// A segment Whisper rates as more likely silence than speech is dropped.
const MAX_NO_SPEECH_PROBABILITY: f32 = 0.6;
/// Whisper pads every piece to 30 s; less than a second is padded further for nothing and is
/// mostly a cut-off word.
const MIN_SAMPLES: usize = 16_000;

/// Whisper with a loaded model.
pub struct WhisperRecognizer {
  /// Keeps the model alive for the state.
  _context: WhisperContext,
  state: WhisperState,
  threads: u16,
}

#[derive(Debug, Error)]
#[error("the speech model didn't load: {0}")]
pub struct WhisperLoadError(String);

/// Whether this computer can run the recognizer: on x86-64 the build needs AVX2, FMA and F16C
/// (`.cargo/config.toml`: whisper.cpp isn't built for the build machine's processor). Asked
/// before captions are offered, as running without them would crash.
pub fn is_supported() -> bool {
  #[cfg(target_arch = "x86_64")]
  {
    std::arch::is_x86_feature_detected!("avx2")
      && std::arch::is_x86_feature_detected!("fma")
      && std::arch::is_x86_feature_detected!("f16c")
  }
  #[cfg(not(target_arch = "x86_64"))]
  {
    true
  }
}

impl WhisperRecognizer {
  /// Loads the model at `path` (about a quarter of a second; call it off the UI and the call's
  /// threads). `threads`: how many CPU threads one recognition uses.
  pub fn load(path: &Path, threads: u16) -> Result<Self, WhisperLoadError> {
    silence_log();
    let mut parameters = WhisperContextParameters::default();
    parameters.use_gpu(false);
    let context = WhisperContext::new_with_params(path, parameters)
      .map_err(|error| WhisperLoadError(error.to_string()))?;
    let state = context
      .create_state()
      .map_err(|error| WhisperLoadError(error.to_string()))?;
    Ok(Self {
      _context: context,
      state,
      threads: threads.max(1),
    })
  }
}

impl SpeechRecognizer for WhisperRecognizer {
  fn recognize(
    &mut self,
    samples: &[f32],
    maybe_language: Option<&'static str>,
  ) -> Result<String, RecognizeError> {
    let mut padded;
    let samples = if samples.len() < MIN_SAMPLES {
      padded = samples.to_vec();
      padded.resize(MIN_SAMPLES, 0.0);
      &padded[..]
    } else {
      samples
    };
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(i32::from(self.threads));
    params.set_language(Some(maybe_language.unwrap_or("auto")));
    params.set_translate(false);
    params.set_no_context(true);
    params.set_single_segment(true);
    params.set_no_timestamps(true);
    params.set_suppress_blank(true);
    params.set_suppress_nst(true);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    self
      .state
      .full(params, samples)
      .map_err(|error| RecognizeError(error.to_string()))?;
    let mut text = String::new();
    for segment in self.state.as_iter() {
      if segment.no_speech_probability() > MAX_NO_SPEECH_PROBABILITY {
        continue;
      }
      let Ok(piece) = segment.to_str_lossy() else {
        continue;
      };
      let piece = piece.trim();
      if is_annotation(piece) {
        continue;
      }
      if !text.is_empty() {
        text.push(' ');
      }
      text.push_str(piece);
    }
    Ok(text)
  }
}

/// Routes whisper.cpp's and ggml's log nowhere (the `log`/`tracing` backends are off), once.
fn silence_log() {
  static SILENCED: Once = Once::new();
  SILENCED.call_once(whisper_rs::install_logging_hooks);
}

/// A sound annotation instead of words: "[Music]", "(laughs)", "* applause *", "...".
fn is_annotation(piece: &str) -> bool {
  let is_wrapped = |open: char, close: char| piece.starts_with(open) && piece.ends_with(close);
  piece.is_empty()
    || is_wrapped('[', ']')
    || is_wrapped('(', ')')
    || is_wrapped('*', '*')
    || piece.chars().all(|char| !char.is_alphanumeric())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn sound_annotations_are_not_captions() {
    for piece in ["[Music]", "(laughs)", "* applause *", "...", "", "♪"] {
      assert!(is_annotation(piece), "{piece:?}");
    }
    for piece in ["Hello.", "(Well) maybe", "It's 3 o'clock"] {
      assert!(!is_annotation(piece), "{piece:?}");
    }
  }

  #[test]
  fn a_missing_model_is_a_load_error() {
    let missing = std::env::temp_dir().join("teamcraft-no-such-whisper-model.bin");
    assert!(WhisperRecognizer::load(&missing, 1).is_err());
  }
}
