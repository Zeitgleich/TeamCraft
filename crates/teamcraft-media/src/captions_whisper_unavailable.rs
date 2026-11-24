//! Live captions where whisper.cpp isn't built (Windows on ARM: it doesn't build with MSVC
//! there). The same API as `captions_whisper`, which says captions aren't supported, so the app
//! offers no captions button.

use std::path::Path;

use thiserror::Error;

use crate::captions::{RecognizeError, SpeechRecognizer};

/// Never made: [`WhisperRecognizer::load`] always fails here.
pub enum WhisperRecognizer {}

#[derive(Debug, Error)]
#[error("the speech model didn't load: {0}")]
pub struct WhisperLoadError(String);

/// Captions can't run on this platform.
pub fn is_supported() -> bool {
  false
}

impl WhisperRecognizer {
  pub fn load(_path: &Path, _threads: u16) -> Result<Self, WhisperLoadError> {
    Err(WhisperLoadError(
      "captions aren't available on Windows on ARM yet".to_owned(),
    ))
  }
}

impl SpeechRecognizer for WhisperRecognizer {
  fn recognize(
    &mut self,
    _samples: &[f32],
    _language: Option<&'static str>,
  ) -> Result<String, RecognizeError> {
    match *self {}
  }
}
