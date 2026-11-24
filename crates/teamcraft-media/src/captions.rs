//! Live captions of a call, recognized on this device (#215, e2ee.md §8.6).
//!
//! The playback mixer hands each remote microphone's decoded 20 ms frames to a [`CaptionTap`]
//! (the call's task: a downsample and a non-blocking send, nothing more). A worker thread cuts
//! each speaker's audio into sentences at pauses ([`SpeakerSegmenter`]) and runs a
//! [`SpeechRecognizer`] on them: the whole sentence once it ended (final), and while it goes on
//! the part so far, every [`CaptionSettings::partial_interval`], whenever the recognizer has
//! nothing final to do (partial). The text goes to the app as [`CaptionUpdate`]s, with the
//! speaker's device; the app shows it and nothing else gets it.
//!
//! Neither audio nor text leaves the device: the recognizer runs here, and nothing is sent to the
//! others or the server. Nothing here logs the text.
//!
//! The call never waits for captions: the tap's queue is bounded and drops when full, and the
//! worker turns captions off ([`CaptionUpdate::Overloaded`]) when it falls behind, rather than
//! show text from long ago or take ever more memory.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::time::Duration;

use teamcraft_client_api::calls::CaptionLanguage;
use teamcraft_proto::DeviceId;
use thiserror::Error;
use tracing::{debug, info, warn};

use crate::audio_frame::SAMPLE_RATE_HZ;
use crate::worker_thread::WorkerThread;

/// The rate recognizers take (Whisper's).
pub const RECOGNIZER_RATE_HZ: u32 = 16_000;
/// Call audio (48 kHz) to the recognizer's rate.
const DECIMATION: usize = (SAMPLE_RATE_HZ / RECOGNIZER_RATE_HZ) as usize;
const FRAME_DURATION: Duration = Duration::from_millis(20);
/// Frames waiting for the worker, all speakers together: 20 s of one speaker, about 1.3 MB.
/// While a sentence is recognized (up to about a second) frames wait here; a full queue means
/// the worker is far behind.
const TAP_CAPACITY_FRAMES: usize = 1_000;
/// Finished sentences waiting for the recognizer before captions give up: more means it takes
/// longer to recognize speech than to speak it, and the captions would only lag further.
const MAX_PENDING_FINALS: usize = 4;
/// How often the worker looks at its stop flag while there is nothing to do.
const IDLE_WAIT: Duration = Duration::from_millis(100);
/// Audio kept from before a sentence starts, so its first syllable isn't cut off.
const PRE_ROLL: Duration = Duration::from_millis(200);

/// Recognizes speech: the engine behind captions. Whisper on the desktop
/// (`captions_whisper`); a fake in tests.
pub trait SpeechRecognizer: Send {
  /// The text spoken in `samples` (mono, [`RECOGNIZER_RATE_HZ`], -1.0..=1.0), in `language`, or
  /// recognized if `None`. Empty when nothing was said. Blocks for as long as it takes.
  fn recognize(
    &mut self,
    samples: &[f32],
    maybe_language: Option<&'static str>,
  ) -> Result<String, RecognizeError>;
}

/// The recognizer failed on a piece of audio.
#[derive(Debug, Error)]
#[error("speech recognition failed: {0}")]
pub struct RecognizeError(pub String);

/// How sentences are cut and how often the text of one still going on is shown.
#[derive(Clone, Copy, Debug)]
pub struct CaptionSettings {
  pub language: CaptionLanguage,
  /// While someone speaks, the sentence so far is recognized again this often (when the
  /// recognizer has time).
  pub partial_interval: Duration,
  /// A pause this long ends a sentence.
  pub end_silence: Duration,
  /// A sentence without a pause ends here anyway: it is shown final, and a new one starts.
  pub max_sentence: Duration,
  /// A frame louder than this (RMS, dBFS) is speech. The others' audio arrives noise-suppressed,
  /// so a plain level works.
  pub speech_threshold_dbfs: f32,
  /// A sentence shorter than this isn't recognized (a cough, a click).
  pub min_speech: Duration,
}

/// What the worker tells the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptionUpdate {
  /// A speaker's sentence: the part so far, or all of it with `is_final`. Later updates of the
  /// same `sentence` replace earlier ones.
  Text {
    speaker: DeviceId,
    sentence: u64,
    text: String,
    is_final: bool,
  },
  /// The recognizer fell behind the speech; the worker stopped. The app turns captions off and
  /// says why.
  Overloaded,
  /// The recognizer failed; the worker stopped.
  Failed,
}

/// Where the call's playback hands the others' voices to captions. Cheap to clone; dropping
/// every clone ends the worker's input.
#[derive(Clone)]
pub struct CaptionTap {
  sender: SyncSender<TapMessage>,
  /// Frames dropped because the queue was full: the worker reads it as falling behind.
  dropped: Arc<AtomicU64>,
}

/// Runs the recognizer on its own thread until dropped or stopped by itself.
pub struct CaptionWorker {
  stop: Arc<AtomicBool>,
  /// Kept so the thread has an owner; never joined: a recognition in progress (up to about a
  /// second) finishes on its own after the stop, and nothing waits for it.
  _thread: WorkerThread,
}

enum TapMessage {
  /// 20 ms of one speaker's voice at the recognizer's rate.
  Frame(DeviceId, Vec<f32>),
  /// The speaker left or their stream ended: their sentence ends here.
  Ended(DeviceId),
}

/// A piece of one speaker's audio for the recognizer.
#[derive(Debug)]
struct Job {
  speaker: DeviceId,
  sentence: u64,
  samples: Vec<f32>,
  is_final: bool,
}

/// Cuts one speaker's audio into sentences at pauses.
#[derive(Debug, Default)]
struct SpeakerSegmenter {
  maybe_sentence: Option<Sentence>,
  /// The last [`PRE_ROLL`] of audio before a sentence.
  pre_roll: VecDeque<f32>,
}

#[derive(Debug)]
struct Sentence {
  id: u64,
  samples: Vec<f32>,
  /// Speech in it so far (frames above the threshold).
  speech: Duration,
  /// Silence since the last speech.
  silence: Duration,
  /// Audio since the part so far was last handed out.
  since_partial: Duration,
}

/// The worker's state between recognitions.
struct Scheduler {
  settings: CaptionSettings,
  speakers: BTreeMap<DeviceId, SpeakerSegmenter>,
  next_sentence: u64,
  finals: VecDeque<Job>,
  /// The latest part-so-far of each speaker's sentence; a newer one replaces it.
  partials: BTreeMap<DeviceId, Job>,
}

impl CaptionTap {
  /// One 20 ms frame (48 kHz mono) of `speaker`'s voice, as the speakers play it. Never blocks:
  /// a full queue drops the frame, and the worker then stops as overloaded.
  pub fn push(&self, speaker: DeviceId, frame: &[f32]) {
    let samples = downsample(frame);
    match self.sender.try_send(TapMessage::Frame(speaker, samples)) {
      Ok(()) | Err(TrySendError::Disconnected(_)) => {},
      Err(TrySendError::Full(_)) => {
        self.dropped.fetch_add(1, Ordering::Relaxed);
      },
    }
  }

  /// `speaker` left the call, or their voice stream ended: their sentence ends.
  pub fn end(&self, speaker: DeviceId) {
    // Full: the worker is behind and stops for that anyway. Disconnected: it stopped already.
    let _ = self.sender.try_send(TapMessage::Ended(speaker));
  }
}

impl CaptionWorker {
  /// Starts the worker with `recognizer`; `on_update` gets the text and the worker's end, on the
  /// worker's thread. The tap goes to the call's playback.
  pub fn spawn(
    recognizer: Box<dyn SpeechRecognizer>,
    settings: CaptionSettings,
    on_update: impl FnMut(CaptionUpdate) + Send + 'static,
  ) -> std::io::Result<(Self, CaptionTap)> {
    let (sender, receiver) = mpsc::sync_channel(TAP_CAPACITY_FRAMES);
    let dropped = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let worker = Worker {
      recognizer,
      scheduler: Scheduler::new(settings),
      receiver,
      dropped: Arc::clone(&dropped),
      stop: Arc::clone(&stop),
      on_update: Box::new(on_update),
    };
    let thread = WorkerThread::spawn("captions", move || worker.run())?;
    Ok((
      Self {
        stop,
        _thread: thread,
      },
      CaptionTap { sender, dropped },
    ))
  }
}

impl Drop for CaptionWorker {
  fn drop(&mut self) {
    self.stop.store(true, Ordering::Relaxed);
  }
}

struct Worker {
  recognizer: Box<dyn SpeechRecognizer>,
  scheduler: Scheduler,
  receiver: Receiver<TapMessage>,
  dropped: Arc<AtomicU64>,
  stop: Arc<AtomicBool>,
  on_update: Box<dyn FnMut(CaptionUpdate) + Send>,
}

/// Why the worker's loop ended.
enum WorkerEnd {
  Stopped,
  Overloaded,
  Failed,
}

impl Worker {
  fn run(mut self) {
    info!("captions started");
    let end = self.run_until_end();
    match end {
      WorkerEnd::Stopped => info!("captions stopped"),
      WorkerEnd::Overloaded => {
        warn!(
          dropped_frames = self.dropped.load(Ordering::Relaxed),
          "captions fell behind; stopped"
        );
        (self.on_update)(CaptionUpdate::Overloaded);
      },
      WorkerEnd::Failed => (self.on_update)(CaptionUpdate::Failed),
    }
  }

  /// Recognizes until stopped, overloaded or failed; once the call dropped its taps, until what
  /// came before is recognized.
  fn run_until_end(&mut self) -> WorkerEnd {
    let mut is_input_ended = false;
    loop {
      if self.stop.load(Ordering::Relaxed) {
        return WorkerEnd::Stopped;
      }
      is_input_ended |= !self.take_waiting();
      if self.scheduler.is_overloaded() || self.dropped.load(Ordering::Relaxed) > 0 {
        return WorkerEnd::Overloaded;
      }
      let Some(job) = self.scheduler.next_job() else {
        if is_input_ended {
          return WorkerEnd::Stopped;
        }
        match self.receiver.recv_timeout(IDLE_WAIT) {
          Ok(message) => self.scheduler.take(message),
          Err(RecvTimeoutError::Timeout) => {},
          Err(RecvTimeoutError::Disconnected) => is_input_ended = true,
        }
        continue;
      };
      if let Err(error) = self.recognize(job) {
        warn!(%error, "captions stopped");
        return WorkerEnd::Failed;
      }
    }
  }

  /// Takes every frame waiting; `false` once the call dropped its taps (the input ended).
  fn take_waiting(&mut self) -> bool {
    loop {
      match self.receiver.try_recv() {
        Ok(message) => self.scheduler.take(message),
        Err(mpsc::TryRecvError::Empty) => return true,
        Err(mpsc::TryRecvError::Disconnected) => return false,
      }
    }
  }

  fn recognize(&mut self, job: Job) -> Result<(), RecognizeError> {
    let started = web_time::Instant::now();
    let text = self
      .recognizer
      .recognize(&job.samples, self.scheduler.settings.language.code())?;
    let audio = Duration::from_secs_f64(job.samples.len() as f64 / f64::from(RECOGNIZER_RATE_HZ));
    // How much of the computer captions take: the real-time factor (recognition time over audio
    // length), never the text.
    debug!(
      audio_ms = audio.as_millis() as u64,
      took_ms = started.elapsed().as_millis() as u64,
      is_final = job.is_final,
      "caption recognized"
    );
    let text = text.trim();
    // An empty partial changes nothing; an empty final still ends the sentence's line.
    if text.is_empty() && !job.is_final {
      return Ok(());
    }
    (self.on_update)(CaptionUpdate::Text {
      speaker: job.speaker,
      sentence: job.sentence,
      text: text.to_owned(),
      is_final: job.is_final,
    });
    Ok(())
  }
}

impl Scheduler {
  fn new(settings: CaptionSettings) -> Self {
    Self {
      settings,
      speakers: BTreeMap::new(),
      next_sentence: 0,
      finals: VecDeque::new(),
      partials: BTreeMap::new(),
    }
  }

  fn take(&mut self, message: TapMessage) {
    let (speaker, maybe_job) = match message {
      TapMessage::Frame(speaker, samples) => {
        let segmenter = self.speakers.entry(speaker).or_default();
        (
          speaker,
          segmenter.push(&samples, &self.settings, &mut self.next_sentence),
        )
      },
      TapMessage::Ended(speaker) => {
        let maybe_job = self
          .speakers
          .remove(&speaker)
          .and_then(|mut segmenter| segmenter.finish(&self.settings));
        (
          speaker,
          maybe_job.map(|(sentence, samples)| (sentence, samples, true)),
        )
      },
    };
    let Some((sentence, samples, is_final)) = maybe_job else {
      return;
    };
    let job = Job {
      speaker,
      sentence,
      samples,
      is_final,
    };
    if is_final {
      // The whole sentence makes its part so far moot.
      self.partials.remove(&speaker);
      self.finals.push_back(job);
    } else {
      self.partials.insert(speaker, job);
    }
  }

  /// The next piece to recognize: sentences that ended first, oldest first; then a speaker's
  /// sentence so far.
  fn next_job(&mut self) -> Option<Job> {
    if let Some(job) = self.finals.pop_front() {
      return Some(job);
    }
    let speaker = *self.partials.keys().next()?;
    self.partials.remove(&speaker)
  }

  fn is_overloaded(&self) -> bool {
    self.finals.len() > MAX_PENDING_FINALS
  }
}

impl SpeakerSegmenter {
  /// Takes one frame; hands out a piece to recognize when one is due, as (sentence, samples,
  /// is_final).
  fn push(
    &mut self,
    frame: &[f32],
    settings: &CaptionSettings,
    next_sentence: &mut u64,
  ) -> Option<(u64, Vec<f32>, bool)> {
    let is_speech = level_dbfs(frame) > settings.speech_threshold_dbfs;
    let Some(sentence) = self.maybe_sentence.as_mut() else {
      if is_speech {
        *next_sentence += 1;
        let mut samples: Vec<f32> = self.pre_roll.drain(..).collect();
        samples.extend_from_slice(frame);
        self.maybe_sentence = Some(Sentence {
          id: *next_sentence,
          samples,
          speech: FRAME_DURATION,
          silence: Duration::ZERO,
          since_partial: FRAME_DURATION,
        });
      } else {
        self.keep_pre_roll(frame);
      }
      return None;
    };
    sentence.samples.extend_from_slice(frame);
    sentence.since_partial += FRAME_DURATION;
    if is_speech {
      sentence.speech += FRAME_DURATION;
      sentence.silence = Duration::ZERO;
    } else {
      sentence.silence += FRAME_DURATION;
    }
    let length = samples_duration(sentence.samples.len());
    if sentence.silence >= settings.end_silence || length >= settings.max_sentence {
      return self
        .finish(settings)
        .map(|(sentence, samples)| (sentence, samples, true));
    }
    if sentence.since_partial >= settings.partial_interval && sentence.speech >= settings.min_speech {
      sentence.since_partial = Duration::ZERO;
      return Some((sentence.id, sentence.samples.clone(), false));
    }
    None
  }

  /// Ends the sentence going on, if any: it is handed out unless it was too short to be speech.
  fn finish(&mut self, settings: &CaptionSettings) -> Option<(u64, Vec<f32>)> {
    let sentence = self.maybe_sentence.take()?;
    (sentence.speech >= settings.min_speech).then_some((sentence.id, sentence.samples))
  }

  fn keep_pre_roll(&mut self, frame: &[f32]) {
    self.pre_roll.extend(frame);
    let keep = duration_samples(PRE_ROLL);
    let excess = self.pre_roll.len().saturating_sub(keep);
    self.pre_roll.drain(..excess);
  }
}

impl Default for CaptionSettings {
  fn default() -> Self {
    Self {
      language: CaptionLanguage::Auto,
      partial_interval: Duration::from_secs(2),
      end_silence: Duration::from_millis(700),
      max_sentence: Duration::from_secs(10),
      speech_threshold_dbfs: -45.0,
      min_speech: Duration::from_millis(300),
    }
  }
}

/// 48 kHz to the recognizer's 16 kHz: the mean of each three samples (a simple low-pass that
/// keeps speech's band) taken as one.
fn downsample(frame: &[f32]) -> Vec<f32> {
  frame
    .chunks(DECIMATION)
    .map(|chunk| chunk.iter().sum::<f32>() / chunk.len() as f32)
    .collect()
}

/// RMS level of `samples` in dBFS; very quiet (-100) when empty or silent.
fn level_dbfs(samples: &[f32]) -> f32 {
  if samples.is_empty() {
    return -100.0;
  }
  let mean_square = samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32;
  if mean_square <= 1e-10 {
    return -100.0;
  }
  10.0 * mean_square.log10()
}

fn samples_duration(samples: usize) -> Duration {
  Duration::from_secs_f64(samples as f64 / f64::from(RECOGNIZER_RATE_HZ))
}

fn duration_samples(duration: Duration) -> usize {
  (duration.as_secs_f64() * f64::from(RECOGNIZER_RATE_HZ)) as usize
}

#[cfg(test)]
mod tests {
  use std::sync::Mutex;

  use teamcraft_proto::{Id, Snowflake};

  use crate::audio_frame::FRAME_SAMPLES_PER_CHANNEL;

  use super::*;

  /// Samples of one 20 ms frame at the recognizer's rate.
  const FRAME_SAMPLES: usize = FRAME_SAMPLES_PER_CHANNEL / DECIMATION;

  mod segmenter {
    use super::*;

    #[test]
    fn silence_hands_out_nothing() {
      let mut segmenter = SpeakerSegmenter::default();
      let mut next = 0;
      for _ in 0..500 {
        assert!(segmenter.push(&silence(), &settings(), &mut next).is_none());
      }
      assert_eq!(next, 0);
    }

    #[test]
    fn a_pause_ends_the_sentence_with_its_pre_roll() {
      let settings = settings();
      let mut segmenter = SpeakerSegmenter::default();
      let mut next = 0;
      let mut jobs = Vec::new();
      for frame in frames(&[(false, 20), (true, 50), (false, 40)]) {
        jobs.extend(segmenter.push(&frame, &settings, &mut next));
      }
      let finals: Vec<_> = jobs.iter().filter(|(_, _, is_final)| *is_final).collect();
      assert_eq!(finals.len(), 1);
      let (sentence, samples, _) = finals[0];
      assert_eq!(*sentence, 1);
      // Pre-roll (200 ms), one second of speech and the pause up to its end.
      let expected_frames = 10 + 50 + 35;
      assert_eq!(samples.len(), expected_frames * FRAME_SAMPLES);
    }

    #[test]
    fn a_long_sentence_shows_its_part_so_far_at_the_interval() {
      let settings = settings();
      let mut segmenter = SpeakerSegmenter::default();
      let mut next = 0;
      let jobs: Vec<_> = frames(&[(true, 200)])
        .iter()
        .filter_map(|frame| segmenter.push(frame, &settings, &mut next))
        .collect();
      // Four seconds of speech: a part so far every 2 s, all of the same sentence.
      assert_eq!(jobs.len(), 2);
      assert!(
        jobs
          .iter()
          .all(|(sentence, _, is_final)| *sentence == 1 && !is_final)
      );
      assert!(jobs[1].1.len() > jobs[0].1.len());
    }

    #[test]
    fn a_sentence_without_a_pause_ends_at_its_longest() {
      let settings = settings();
      let mut segmenter = SpeakerSegmenter::default();
      let mut next = 0;
      let finals: Vec<_> = frames(&[(true, 1_100)])
        .iter()
        .filter_map(|frame| segmenter.push(frame, &settings, &mut next))
        .filter(|(_, _, is_final)| *is_final)
        .collect();
      // 22 s of speech: two sentences of 10 s end, the third goes on.
      assert_eq!(finals.len(), 2);
      assert_eq!(finals[0].0, 1);
      assert_eq!(finals[1].0, 2);
      assert_eq!(finals[0].1.len(), duration_samples(settings.max_sentence));
    }

    #[test]
    fn a_click_is_not_a_sentence() {
      let settings = settings();
      let mut segmenter = SpeakerSegmenter::default();
      let mut next = 0;
      let jobs: Vec<_> = frames(&[(true, 5), (false, 100)])
        .iter()
        .filter_map(|frame| segmenter.push(frame, &settings, &mut next))
        .collect();
      assert!(jobs.is_empty());
    }
  }

  mod scheduler {
    use super::*;

    #[test]
    fn finished_sentences_go_first_and_replace_their_part_so_far() {
      let mut scheduler = Scheduler::new(settings());
      for frame in frames(&[(true, 110)]) {
        scheduler.take(TapMessage::Frame(speaker(1), frame));
      }
      for frame in frames(&[(true, 80), (false, 40)]) {
        scheduler.take(TapMessage::Frame(speaker(2), frame));
      }
      let first = scheduler.next_job().unwrap();
      assert_eq!((first.speaker, first.is_final), (speaker(2), true));
      let second = scheduler.next_job().unwrap();
      assert_eq!((second.speaker, second.is_final), (speaker(1), false));
      assert!(scheduler.next_job().is_none());
    }

    #[test]
    fn a_speaker_who_leaves_ends_their_sentence() {
      let mut scheduler = Scheduler::new(settings());
      for frame in frames(&[(true, 30)]) {
        scheduler.take(TapMessage::Frame(speaker(1), frame));
      }
      scheduler.take(TapMessage::Ended(speaker(1)));
      let job = scheduler.next_job().unwrap();
      assert!(job.is_final);
      assert!(scheduler.speakers.is_empty());
    }

    #[test]
    fn too_many_waiting_sentences_is_overload() {
      let mut scheduler = Scheduler::new(settings());
      for _ in 0..=MAX_PENDING_FINALS {
        for frame in frames(&[(true, 30), (false, 40)]) {
          scheduler.take(TapMessage::Frame(speaker(1), frame));
        }
      }
      assert!(scheduler.is_overloaded());
    }
  }

  mod worker {
    use super::*;

    #[test]
    fn text_reaches_the_app_with_its_speaker() {
      let (updates, on_update) = collector();
      let (worker, tap) = CaptionWorker::spawn(
        Box::new(FakeRecognizer::new("hello there")),
        settings(),
        on_update,
      )
      .unwrap();
      for frame in frames(&[(true, 40), (false, 40)]) {
        tap.push(speaker(1), &upsample(&frame));
      }
      drop(tap);
      let seen = wait_for(&updates, |update| {
        matches!(update, CaptionUpdate::Text { is_final: true, .. })
      });
      let last = seen.last().cloned().unwrap();
      assert_eq!(
        last,
        CaptionUpdate::Text {
          speaker: speaker(1),
          sentence: 1,
          text: "hello there".to_owned(),
          is_final: true,
        }
      );
      drop(worker);
    }

    #[test]
    fn a_failing_recognizer_stops_captions() {
      let (updates, on_update) = collector();
      let (_worker, tap) = CaptionWorker::spawn(Box::new(FailingRecognizer), settings(), on_update).unwrap();
      for frame in frames(&[(true, 40), (false, 40)]) {
        tap.push(speaker(1), &upsample(&frame));
      }
      wait_for(&updates, |update| *update == CaptionUpdate::Failed);
    }

    #[test]
    fn a_full_queue_drops_without_blocking_and_stops_as_overloaded() {
      let (updates, on_update) = collector();
      let (gate, recognizer) = GatedRecognizer::new();
      let (_worker, tap) = CaptionWorker::spawn(Box::new(recognizer), settings(), on_update).unwrap();
      // A sentence gets the worker stuck in the recognizer; then far more than the queue holds.
      for frame in frames(&[(true, 40), (false, 40)]) {
        tap.push(speaker(1), &upsample(&frame));
      }
      gate.wait_entered();
      for frame in frames(&[(true, TAP_CAPACITY_FRAMES + 10)]) {
        tap.push(speaker(2), &upsample(&frame));
      }
      assert!(tap.dropped.load(Ordering::Relaxed) > 0);
      gate.release();
      wait_for(&updates, |update| *update == CaptionUpdate::Overloaded);
    }
  }

  #[test]
  fn downsampling_keeps_a_constant_level_and_a_third_of_the_samples() {
    let frame = vec![0.5; FRAME_SAMPLES_PER_CHANNEL];
    let out = downsample(&frame);
    assert_eq!(out.len(), FRAME_SAMPLES);
    assert!(out.iter().all(|sample| (sample - 0.5).abs() < 1e-6));
  }

  #[test]
  fn level_of_silence_and_full_scale() {
    assert_eq!(level_dbfs(&[]), -100.0);
    assert_eq!(level_dbfs(&[0.0; 320]), -100.0);
    assert!(level_dbfs(&[1.0; 320]).abs() < 1e-3);
  }

  struct FakeRecognizer {
    text: String,
  }

  struct FailingRecognizer;

  /// Blocks in `recognize` until released, and says when it got there.
  struct GatedRecognizer {
    gate: Arc<Gate>,
  }

  #[derive(Default)]
  struct Gate {
    state: Mutex<(bool, bool)>,
    changed: std::sync::Condvar,
  }

  impl FakeRecognizer {
    fn new(text: &str) -> Self {
      Self {
        text: text.to_owned(),
      }
    }
  }

  impl SpeechRecognizer for FakeRecognizer {
    fn recognize(
      &mut self,
      _samples: &[f32],
      _language: Option<&'static str>,
    ) -> Result<String, RecognizeError> {
      Ok(self.text.clone())
    }
  }

  impl SpeechRecognizer for FailingRecognizer {
    fn recognize(
      &mut self,
      _samples: &[f32],
      _language: Option<&'static str>,
    ) -> Result<String, RecognizeError> {
      Err(RecognizeError("broken".to_owned()))
    }
  }

  impl GatedRecognizer {
    fn new() -> (Arc<Gate>, Self) {
      let gate = Arc::new(Gate::default());
      (Arc::clone(&gate), Self { gate })
    }
  }

  impl SpeechRecognizer for GatedRecognizer {
    fn recognize(
      &mut self,
      _samples: &[f32],
      _language: Option<&'static str>,
    ) -> Result<String, RecognizeError> {
      let mut state = self.gate.state.lock().unwrap();
      state.0 = true;
      self.gate.changed.notify_all();
      while !state.1 {
        state = self.gate.changed.wait(state).unwrap();
      }
      Ok(String::new())
    }
  }

  impl Gate {
    fn wait_entered(&self) {
      let mut state = self.state.lock().unwrap();
      while !state.0 {
        state = self.changed.wait(state).unwrap();
      }
    }

    fn release(&self) {
      self.state.lock().unwrap().1 = true;
      self.changed.notify_all();
    }
  }

  fn speaker(raw: i64) -> DeviceId {
    DeviceId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn settings() -> CaptionSettings {
    CaptionSettings::default()
  }

  fn silence() -> Vec<f32> {
    vec![0.0; FRAME_SAMPLES]
  }

  /// Frames at the recognizer's rate: (is speech, how many) in turn. Speech is a tone at about
  /// -12 dBFS, silence is zero.
  fn frames(parts: &[(bool, usize)]) -> Vec<Vec<f32>> {
    parts
      .iter()
      .flat_map(|&(is_speech, count)| {
        (0..count).map(move |_| {
          if is_speech {
            (0..FRAME_SAMPLES)
              .map(|index| 0.35 * (index as f32 * 0.2).sin())
              .collect()
          } else {
            silence()
          }
        })
      })
      .collect()
  }

  /// A frame at the recognizer's rate back to the call's 48 kHz (each sample three times).
  fn upsample(frame: &[f32]) -> Vec<f32> {
    frame
      .iter()
      .flat_map(|&sample| std::iter::repeat_n(sample, DECIMATION))
      .collect()
  }

  type Updates = Receiver<CaptionUpdate>;

  fn collector() -> (Updates, impl FnMut(CaptionUpdate) + Send + 'static) {
    let (sender, receiver) = mpsc::channel();
    // The test may have finished and dropped the receiver.
    (receiver, move |update| drop(sender.send(update)))
  }

  /// The worker's updates up to the first that meets `is_awaited` (bounded: fails after 10 s).
  fn wait_for(updates: &Updates, is_awaited: impl Fn(&CaptionUpdate) -> bool) -> Vec<CaptionUpdate> {
    let mut seen = Vec::new();
    loop {
      match updates.recv_timeout(Duration::from_secs(10)) {
        Ok(update) => {
          let is_done = is_awaited(&update);
          seen.push(update);
          if is_done {
            return seen;
          }
        },
        Err(error) => panic!("no awaited update ({error}); seen: {seen:?}"),
      }
    }
  }
}
