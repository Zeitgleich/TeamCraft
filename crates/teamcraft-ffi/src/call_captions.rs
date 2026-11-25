//! Live captions of a call on the phone (#215, e2ee.md §8.6): the call's decrypted playback of the
//! others' voices, cut into sentences by `teamcraft_media::captions` (as on the desktop), turned
//! into text by the system's speech recognizer **in on-device mode only** (Android
//! `SpeechRecognizer` from `createOnDeviceSpeechRecognizer`, iOS `SFSpeechRecognizer` with
//! `requiresOnDeviceRecognition`), behind [`CaptionRecognizer`]. The text goes into the call's
//! state for the call screen and nowhere else: nothing is sent to the server, a cloud service or
//! the others, and nothing here logs it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use teamcraft_client_api::calls::{CaptionLanguage, CaptionLine};
use teamcraft_media::call_view::{CaptionText, apply_caption_line};
use teamcraft_media::captions::{
  CaptionSettings, CaptionTap, CaptionUpdate, CaptionWorker, RECOGNIZER_RATE_HZ, RecognizeError,
  SpeechRecognizer,
};
use teamcraft_proto::{DeviceId, UserId};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// While someone speaks, the sentence so far is recognized again this often. Longer than the
/// desktop's 2 s: the system recognizer starts a session for every piece, which costs battery.
const PARTIAL_INTERVAL: Duration = Duration::from_secs(3);

/// The system's speech recognizer, in on-device mode only, as the app sets it up. Called on the
/// captions' own thread, never the UI thread; one call at a time.
#[uniffi::export(with_foreign)]
pub trait CaptionRecognizer: Send + Sync {
  /// The text spoken in `pcm16` (mono, 16 kHz, signed 16-bit little-endian), in the language the
  /// app picked for the recognizer. Empty when nothing was said. Blocks until recognized; the
  /// implementation bounds how long (a recognizer that hangs fails the call to it).
  fn recognize(&self, pcm16: Vec<u8>) -> Result<String, CaptionRecognizeError>;
}

/// The system recognizer couldn't turn a piece of audio into text: captions stop.
#[derive(Debug, Error, uniffi::Error)]
pub enum CaptionRecognizeError {
  /// The reason as the platform gives it (an error code, never recognized text).
  #[error("speech recognition failed: {reason}")]
  Failed { reason: String },
}

/// One person's sentence in the captions, newest last.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CaptionLineInfo {
  /// Counts up within the call; a sentence so far and its final text share it (one row).
  pub id: u64,
  pub user_id: String,
  pub text: String,
  /// The sentence ended: its text no longer changes (announce it to screen readers now).
  pub is_final: bool,
}

/// Why captions went off by themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum CaptionsStop {
  /// The recognizer fell behind the speech.
  Overloaded,
  /// The recognizer failed.
  Failed,
}

/// Captions running beside the call: the worker and what it says. Dropping it stops the worker.
pub(crate) struct CaptionRun {
  _worker: CaptionWorker,
  updates: mpsc::UnboundedReceiver<CaptionUpdate>,
  lines: Vec<CaptionLine>,
}

/// The foreign recognizer as the caption worker's engine.
struct SystemRecognizer(Arc<dyn CaptionRecognizer>);

impl CaptionRun {
  /// Starts the worker with the app's recognizer; the tap goes to the call's playback.
  pub(crate) fn start(recognizer: Arc<dyn CaptionRecognizer>) -> std::io::Result<(Self, CaptionTap)> {
    let (sender, updates) = mpsc::unbounded_channel();
    let settings = CaptionSettings {
      // The app's recognizer is made for one language; the worker passes none.
      language: CaptionLanguage::Auto,
      partial_interval: PARTIAL_INTERVAL,
      ..CaptionSettings::default()
    };
    // The run may be gone (captions off, the call over) when an update comes.
    let (worker, tap) =
      CaptionWorker::spawn(Box::new(SystemRecognizer(recognizer)), settings, move |update| {
        drop(sender.send(update))
      })?;
    info!("captions on");
    Ok((
      Self {
        _worker: worker,
        updates,
        lines: Vec::new(),
      },
      tap,
    ))
  }

  /// The lines to show, for the call's state.
  pub(crate) fn lines(&self) -> Vec<CaptionLineInfo> {
    self.lines.iter().map(line_info).collect()
  }

  /// Puts a text update into the lines: `true` when they changed. A speaker the roster doesn't
  /// know (left already) is dropped.
  pub(crate) fn apply(&mut self, roster: &BTreeMap<DeviceId, UserId>, update: CaptionUpdate) -> bool {
    let CaptionUpdate::Text {
      speaker,
      sentence,
      text,
      is_final,
    } = update
    else {
      return false;
    };
    let Some(user) = roster.get(&speaker).copied() else {
      return false;
    };
    let caption = CaptionText {
      speaker,
      sentence,
      text,
      is_final,
    };
    apply_caption_line(&mut self.lines, user, caption)
  }
}

/// The next update of the running captions; never, while they are off (a cancel-safe receive).
pub(crate) async fn next_update(maybe_run: &mut Option<CaptionRun>) -> Option<CaptionUpdate> {
  match maybe_run {
    Some(run) => run.updates.recv().await,
    None => std::future::pending().await,
  }
}

/// Why captions stopped by themselves, from the worker's last update.
pub(crate) fn stop_reason(maybe_update: Option<&CaptionUpdate>) -> CaptionsStop {
  match maybe_update {
    Some(CaptionUpdate::Overloaded) => CaptionsStop::Overloaded,
    _ => CaptionsStop::Failed,
  }
}

impl SpeechRecognizer for SystemRecognizer {
  fn recognize(&mut self, samples: &[f32], _: Option<&'static str>) -> Result<String, RecognizeError> {
    self.0.recognize(pcm16(samples)).map_err(|error| {
      warn!(%error, "the system's speech recognizer failed");
      RecognizeError(error.to_string())
    })
  }
}

impl From<uniffi::UnexpectedUniFFICallbackError> for CaptionRecognizeError {
  fn from(_: uniffi::UnexpectedUniFFICallbackError) -> Self {
    // The reason isn't kept: nothing says what is in an error the app didn't mean to throw.
    Self::Failed {
      reason: "unexpected error in the app's recognizer".to_owned(),
    }
  }
}

fn line_info(line: &CaptionLine) -> CaptionLineInfo {
  CaptionLineInfo {
    id: line.id,
    user_id: line.user.to_string(),
    text: line.text.clone(),
    is_final: line.is_final,
  }
}

/// Samples (-1.0..=1.0) as signed 16-bit little-endian, what both platforms' recognizers take.
fn pcm16(samples: &[f32]) -> Vec<u8> {
  const _: () = assert!(
    RECOGNIZER_RATE_HZ == 16_000,
    "the apps' recognizers are set up for 16 kHz"
  );
  samples
    .iter()
    .flat_map(|sample| {
      // In range after the clamp, so the cast only drops the fraction.
      #[allow(clippy::cast_possible_truncation)]
      let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
      value.to_le_bytes()
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use std::sync::Mutex;

  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn samples_become_little_endian_pcm16_clamped() {
    let bytes = pcm16(&[0.0, 1.0, -1.0, 2.0, -2.0, 0.5]);
    let values: Vec<i16> = bytes
      .as_chunks::<2>()
      .0
      .iter()
      .map(|pair| i16::from_le_bytes(*pair))
      .collect();
    assert_eq!(values, [0, i16::MAX, -i16::MAX, i16::MAX, -i16::MAX, 16_383]);
  }

  #[test]
  fn the_app_recognizer_gets_pcm16_and_its_text_comes_back() {
    let heard = Arc::new(Mutex::new(Vec::new()));
    let recognizer = FakeRecognizer {
      heard: Arc::clone(&heard),
      maybe_text: Some("hello".to_owned()),
    };
    let mut system = SystemRecognizer(Arc::new(recognizer));
    assert_eq!(system.recognize(&[0.5, -0.5], None).unwrap(), "hello");
    assert_eq!(heard.lock().unwrap().as_slice(), [4]);
  }

  #[test]
  fn a_failing_app_recognizer_fails_the_recognition() {
    let recognizer = FakeRecognizer {
      heard: Arc::default(),
      maybe_text: None,
    };
    let mut system = SystemRecognizer(Arc::new(recognizer));
    assert!(system.recognize(&[0.0], None).is_err());
  }

  mod lines {
    use super::*;

    #[test]
    fn a_known_speaker_gets_a_line_that_its_final_text_replaces() {
      let (mut run, roster, user) = run_with_speaker();
      assert!(run.apply(&roster, text(1, "so far", false)));
      assert!(run.apply(&roster, text(1, "so far and done", true)));
      let lines = run.lines();
      assert_eq!(lines.len(), 1);
      assert_eq!(lines[0].user_id, user.to_string());
      assert_eq!(lines[0].text, "so far and done");
      assert!(lines[0].is_final);
    }

    #[test]
    fn a_speaker_who_left_is_dropped_and_stops_are_no_lines() {
      let (mut run, _, _) = run_with_speaker();
      assert!(!run.apply(&BTreeMap::new(), text(1, "gone", true)));
      assert!(!run.apply(&BTreeMap::new(), CaptionUpdate::Overloaded));
      assert!(run.lines().is_empty());
    }

    #[test]
    fn stops_say_why() {
      assert_eq!(
        stop_reason(Some(&CaptionUpdate::Overloaded)),
        CaptionsStop::Overloaded
      );
      assert_eq!(stop_reason(Some(&CaptionUpdate::Failed)), CaptionsStop::Failed);
      assert_eq!(stop_reason(None), CaptionsStop::Failed);
    }

    fn run_with_speaker() -> (CaptionRun, BTreeMap<DeviceId, UserId>, UserId) {
      let recognizer = FakeRecognizer {
        heard: Arc::default(),
        maybe_text: Some(String::new()),
      };
      let (run, _tap) = CaptionRun::start(Arc::new(recognizer)).unwrap();
      let user = UserId::from_snowflake(Snowflake::new(7).unwrap());
      (
        run,
        BTreeMap::from([(DeviceId::from_snowflake(Snowflake::new(3).unwrap()), user)]),
        user,
      )
    }

    fn text(sentence: u64, text: &str, is_final: bool) -> CaptionUpdate {
      CaptionUpdate::Text {
        speaker: DeviceId::from_snowflake(Snowflake::new(3).unwrap()),
        sentence,
        text: text.to_owned(),
        is_final,
      }
    }
  }

  /// Records the size of each piece and answers with `maybe_text`, or fails without one.
  struct FakeRecognizer {
    heard: Arc<Mutex<Vec<usize>>>,
    maybe_text: Option<String>,
  }

  impl CaptionRecognizer for FakeRecognizer {
    fn recognize(&self, pcm16: Vec<u8>) -> Result<String, CaptionRecognizeError> {
      self.heard.lock().unwrap().push(pcm16.len());
      self.maybe_text.clone().ok_or(CaptionRecognizeError::Failed {
        reason: "fake".to_owned(),
      })
    }
  }
}
