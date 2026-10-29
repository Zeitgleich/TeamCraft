//! From decrypted Opus frames to pieces of speech (e2ee.md §8.2): one decoder and one segmenter
//! per speaking device, so every piece belongs to one speaker without guessing. A piece ends at a
//! pause, or at the longest piece length; pieces with hardly any speech are dropped. A finished
//! piece is downsampled to 16 kHz, the rate speech recognition works at, and leaves this module;
//! nothing here is ever written to disk.

use std::collections::HashMap;
use std::time::Duration;

use teamcraft_media::audio_codec::{AudioChannels, OpusDecoder};
use teamcraft_media::rtp::SequenceNumber;
use teamcraft_proto::DeviceId;
use tracing::debug;

/// Sample rate of the call's Opus audio.
pub const CALL_SAMPLE_RATE_HZ: u32 = 48_000;
/// Sample rate of the pieces sent for transcription.
pub const PIECE_SAMPLE_RATE_HZ: u32 = 16_000;
/// Silence kept in front of a piece, so its first sound isn't cut.
const PRE_ROLL_FRAMES: usize = 10;
/// Within this much of the longest piece, a short pause already ends it (a cleaner cut).
const SOFT_LIMIT_MARGIN: Duration = Duration::from_secs(5);
const SHORT_PAUSE: Duration = Duration::from_millis(200);
/// Lost frames up to this many are concealed; a longer gap is a gap.
const MAX_CONCEALED_FRAMES: u16 = 5;
/// Taps of the low-pass filter in front of the downsampling (odd, symmetric).
const FILTER_TAPS: usize = 47;
/// Cut-off of the low-pass filter: below the 8 kHz Nyquist rate of the pieces.
const FILTER_CUTOFF_HZ: f64 = 7_200.0;

/// How speech is cut into pieces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaptureSettings {
  pub max_piece: Duration,
  pub pause: Duration,
  pub min_speech: Duration,
  pub voice_threshold_dbfs: f32,
}

/// The audio of all speakers of a call, keyed by device.
pub struct Capture {
  settings: CaptureSettings,
  speakers: HashMap<DeviceId, Speaker>,
}

/// One speaker's piece of speech, ready to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioPiece {
  pub device: DeviceId,
  /// From the start of the notetaker's session.
  pub start_ms: u64,
  pub end_ms: u64,
  /// Mono, [`PIECE_SAMPLE_RATE_HZ`].
  pub samples: Vec<i16>,
}

/// Cuts one speaker's 48 kHz PCM into pieces at pauses.
pub struct Segmenter {
  settings: CaptureSettings,
  maybe_open: Option<OpenPiece>,
  /// The last silent frames, for the pre-roll of the next piece.
  pre_roll: Vec<Vec<i16>>,
}

struct Speaker {
  decoder: OpusDecoder,
  maybe_last_sequence: Option<SequenceNumber>,
  segmenter: Segmenter,
}

struct OpenPiece {
  start_ms: u64,
  samples: Vec<i16>,
  speech_ms: u64,
  /// Silence at the end so far.
  silence_ms: u64,
  /// When the last frame arrived.
  last_frame_ms: u64,
}

impl Capture {
  pub fn new(settings: CaptureSettings) -> Self {
    Self {
      settings,
      speakers: HashMap::new(),
    }
  }

  /// One frame of `device`, arrived `at_ms` into the session; a piece it finished, if any.
  pub fn push(
    &mut self,
    device: DeviceId,
    sequence_number: SequenceNumber,
    opus: &[u8],
    at_ms: u64,
  ) -> Option<AudioPiece> {
    let speaker = match self.speakers.entry(device) {
      std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
      std::collections::hash_map::Entry::Vacant(entry) => entry.insert(Speaker::new(self.settings)?),
    };
    let mut maybe_piece = None;
    for frame in speaker.decode(sequence_number, opus) {
      if let Some(piece) = speaker.segmenter.push_frame(at_ms, &frame) {
        maybe_piece = Some(piece);
      }
    }
    maybe_piece.map(|(start_ms, end_ms, samples)| AudioPiece {
      device,
      start_ms,
      end_ms,
      samples,
    })
  }

  /// Ends the pieces of speakers who have sent nothing for a pause (senders send nothing in
  /// silence, and the SFU forwards only the loudest speakers).
  pub fn on_idle(&mut self, now_ms: u64) -> Vec<AudioPiece> {
    let mut pieces = Vec::new();
    for (device, speaker) in &mut self.speakers {
      if let Some((start_ms, end_ms, samples)) = speaker.segmenter.on_idle(now_ms) {
        pieces.push(AudioPiece {
          device: *device,
          start_ms,
          end_ms,
          samples,
        });
      }
    }
    pieces
  }

  /// A device left: its open piece ends.
  pub fn remove(&mut self, device: DeviceId) -> Option<AudioPiece> {
    let mut speaker = self.speakers.remove(&device)?;
    let (start_ms, end_ms, samples) = speaker.segmenter.flush()?;
    Some(AudioPiece {
      device,
      start_ms,
      end_ms,
      samples,
    })
  }

  /// The end of the session: every open piece ends.
  pub fn flush_all(&mut self) -> Vec<AudioPiece> {
    let devices: Vec<DeviceId> = self.speakers.keys().copied().collect();
    devices
      .into_iter()
      .filter_map(|device| self.remove(device))
      .collect()
  }
}

impl Segmenter {
  pub fn new(settings: CaptureSettings) -> Self {
    Self {
      settings,
      maybe_open: None,
      pre_roll: Vec::new(),
    }
  }

  /// Takes one frame (48 kHz mono) that arrived at `at_ms`. Returns a piece (start, end, 16 kHz
  /// samples) when the frame ended one with enough speech in it.
  pub fn push_frame(&mut self, at_ms: u64, frame: &[i16]) -> Option<(u64, u64, Vec<i16>)> {
    let is_voiced = rms_dbfs(frame) > self.settings.voice_threshold_dbfs;
    let frame_ms = frame_ms(frame);
    let Some(open) = self.maybe_open.as_mut() else {
      if !is_voiced {
        self.keep_pre_roll(frame);
        return None;
      }
      self.open(at_ms, frame, frame_ms);
      return None;
    };
    open.samples.extend_from_slice(frame);
    open.last_frame_ms = at_ms;
    if is_voiced {
      open.speech_ms += frame_ms;
      open.silence_ms = 0;
    } else {
      open.silence_ms += frame_ms;
    }
    let length = Duration::from_millis(open.length_ms());
    let silence = Duration::from_millis(open.silence_ms);
    let is_ended = silence >= self.settings.pause
      || length >= self.settings.max_piece
      || (length + SOFT_LIMIT_MARGIN >= self.settings.max_piece && silence >= SHORT_PAUSE);
    if is_ended { self.close() } else { None }
  }

  /// No frame came since the last one: a gap of a pause ends the piece.
  pub fn on_idle(&mut self, now_ms: u64) -> Option<(u64, u64, Vec<i16>)> {
    let open = self.maybe_open.as_ref()?;
    let idle = Duration::from_millis(now_ms.saturating_sub(open.last_frame_ms));
    if idle >= self.settings.pause {
      self.close()
    } else {
      None
    }
  }

  /// Ends the open piece, if any.
  pub fn flush(&mut self) -> Option<(u64, u64, Vec<i16>)> {
    self.close()
  }

  fn open(&mut self, at_ms: u64, frame: &[i16], frame_ms: u64) {
    let mut samples: Vec<i16> = self.pre_roll.drain(..).flatten().collect();
    let pre_roll_ms = samples.len() as u64 * 1000 / u64::from(CALL_SAMPLE_RATE_HZ);
    samples.extend_from_slice(frame);
    self.maybe_open = Some(OpenPiece {
      start_ms: at_ms.saturating_sub(pre_roll_ms),
      samples,
      speech_ms: frame_ms,
      silence_ms: 0,
      last_frame_ms: at_ms,
    });
  }

  fn keep_pre_roll(&mut self, frame: &[i16]) {
    self.pre_roll.push(frame.to_vec());
    if self.pre_roll.len() > PRE_ROLL_FRAMES {
      self.pre_roll.remove(0);
    }
  }

  /// Closes the open piece; one with too little speech is dropped.
  fn close(&mut self) -> Option<(u64, u64, Vec<i16>)> {
    let open = self.maybe_open.take()?;
    self.pre_roll.clear();
    if Duration::from_millis(open.speech_ms) < self.settings.min_speech {
      debug!(
        speech_ms = open.speech_ms,
        "dropped a piece with too little speech"
      );
      return None;
    }
    let end_ms = open.start_ms + open.length_ms();
    Some((open.start_ms, end_ms, downsample_to_16k(&open.samples)))
  }
}

impl OpenPiece {
  fn length_ms(&self) -> u64 {
    self.samples.len() as u64 * 1000 / u64::from(CALL_SAMPLE_RATE_HZ)
  }
}

impl Speaker {
  fn new(settings: CaptureSettings) -> Option<Self> {
    match OpusDecoder::new(AudioChannels::Mono) {
      Ok(decoder) => Some(Self {
        decoder,
        maybe_last_sequence: None,
        segmenter: Segmenter::new(settings),
      }),
      Err(error) => {
        tracing::warn!(%error, "no Opus decoder for a speaker");
        None
      },
    }
  }

  /// The frames one packet yields: concealment for a few lost ones before it, then its own. A
  /// packet older than the last one (reordered on the way) is dropped.
  fn decode(&mut self, sequence_number: SequenceNumber, opus: &[u8]) -> Vec<Vec<i16>> {
    let mut frames = Vec::new();
    if let Some(last) = self.maybe_last_sequence {
      let step = sequence_number.0.wrapping_sub(last.0);
      if step == 0 || step > u16::MAX / 2 {
        return frames;
      }
      let missing = step - 1;
      if (1..=MAX_CONCEALED_FRAMES).contains(&missing) {
        for index in 0..missing {
          let maybe_next = (index + 1 == missing).then_some(opus);
          match self.decoder.conceal(maybe_next) {
            Ok(frame) => frames.push(frame),
            Err(error) => debug!(%error, "concealing a lost frame failed"),
          }
        }
      }
    }
    self.maybe_last_sequence = Some(sequence_number);
    match self.decoder.decode(opus) {
      Ok(frame) => frames.push(frame),
      Err(error) => debug!(%error, "a frame did not decode"),
    }
    frames
  }
}

/// The level of a frame in dB below full scale; digital silence is -100.
pub fn rms_dbfs(frame: &[i16]) -> f32 {
  if frame.is_empty() {
    return -100.0;
  }
  let sum: f64 = frame.iter().map(|sample| f64::from(*sample).powi(2)).sum();
  let rms = (sum / frame.len() as f64).sqrt() / f64::from(i16::MAX);
  if rms <= 0.0 {
    return -100.0;
  }
  (20.0 * rms.log10()).max(-100.0) as f32
}

/// 48 kHz to 16 kHz: a windowed-sinc low-pass below the new Nyquist rate, then every third sample.
pub fn downsample_to_16k(samples: &[i16]) -> Vec<i16> {
  let taps = low_pass_taps();
  let half = FILTER_TAPS / 2;
  let ratio = (CALL_SAMPLE_RATE_HZ / PIECE_SAMPLE_RATE_HZ) as usize;
  (0..samples.len())
    .step_by(ratio)
    .map(|center| {
      let mut sum = 0.0;
      for (index, tap) in taps.iter().enumerate() {
        let position = center as isize + index as isize - half as isize;
        if let Some(sample) = usize::try_from(position).ok().and_then(|at| samples.get(at)) {
          sum += f64::from(*sample) * tap;
        }
      }
      sum.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
    })
    .collect()
}

fn low_pass_taps() -> [f64; FILTER_TAPS] {
  let cutoff = FILTER_CUTOFF_HZ / f64::from(CALL_SAMPLE_RATE_HZ);
  let middle = (FILTER_TAPS / 2) as f64;
  let mut taps = [0.0; FILTER_TAPS];
  for (index, tap) in taps.iter_mut().enumerate() {
    let offset = index as f64 - middle;
    let sinc = if offset == 0.0 {
      2.0 * cutoff
    } else {
      (2.0 * std::f64::consts::PI * cutoff * offset).sin() / (std::f64::consts::PI * offset)
    };
    // Blackman window.
    let phase = 2.0 * std::f64::consts::PI * index as f64 / (FILTER_TAPS - 1) as f64;
    let window = 0.42 - 0.5 * phase.cos() + 0.08 * (2.0 * phase).cos();
    *tap = sinc * window;
  }
  let gain: f64 = taps.iter().sum();
  taps.map(|tap| tap / gain)
}

fn frame_ms(frame: &[i16]) -> u64 {
  (frame.len() as u64 * 1000 / u64::from(CALL_SAMPLE_RATE_HZ)).max(1)
}

#[cfg(test)]
mod tests {
  use teamcraft_media::audio_codec::{OpusEncoder, OpusEncoderConfig};
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  /// One Opus frame of the call.
  const FRAME_MS: u64 = 20;

  const SETTINGS: CaptureSettings = CaptureSettings {
    max_piece: Duration::from_secs(30),
    pause: Duration::from_millis(700),
    min_speech: Duration::from_millis(500),
    voice_threshold_dbfs: -50.0,
  };

  mod segmenter {
    use super::*;

    #[test]
    fn a_pause_ends_a_piece_with_its_pre_roll_and_times() {
      let mut segmenter = Segmenter::new(SETTINGS);
      let mut at = 0;
      for _ in 0..5 {
        assert_eq!(segmenter.push_frame(at, &silence()), None);
        at += FRAME_MS;
      }
      let speech_start = at;
      for _ in 0..50 {
        assert_eq!(segmenter.push_frame(at, &tone()), None);
        at += FRAME_MS;
      }
      let mut maybe_piece = None;
      for _ in 0..40 {
        if let Some(piece) = segmenter.push_frame(at, &silence()) {
          maybe_piece = Some(piece);
          break;
        }
        at += FRAME_MS;
      }
      let (start_ms, end_ms, samples) = maybe_piece.unwrap();
      assert_eq!(start_ms, speech_start - 5 * FRAME_MS);
      // 5 pre-roll frames, 1 s of speech, 0.7 s of pause, at 16 kHz.
      assert_eq!(samples.len(), (5 + 50 + 35) * 320);
      assert_eq!(end_ms, start_ms + (5 + 50 + 35) * FRAME_MS);
    }

    #[test]
    fn a_short_noise_is_dropped() {
      let mut segmenter = Segmenter::new(SETTINGS);
      for index in 0..10 {
        segmenter.push_frame(index * FRAME_MS, &tone());
      }
      assert_eq!(segmenter.flush(), None);
    }

    #[test]
    fn continuous_speech_is_cut_at_the_longest_piece() {
      let mut segmenter = Segmenter::new(SETTINGS);
      let mut pieces = Vec::new();
      for index in 0..(70 * 50) {
        if let Some(piece) = segmenter.push_frame(index * FRAME_MS, &tone()) {
          pieces.push(piece);
        }
      }
      pieces.extend(segmenter.flush());
      assert_eq!(pieces.len(), 3);
      assert!(pieces.iter().all(|(start, end, _)| end - start <= 30_000));
      assert_eq!(pieces[1].0, pieces[0].1);
    }

    #[test]
    fn near_the_limit_a_short_pause_ends_the_piece() {
      let mut segmenter = Segmenter::new(SETTINGS);
      let mut at = 0;
      for _ in 0..(26 * 50) {
        segmenter.push_frame(at, &tone());
        at += FRAME_MS;
      }
      let mut ended_after = None;
      for frame in 0..20 {
        if segmenter.push_frame(at, &silence()).is_some() {
          ended_after = Some(frame + 1);
          break;
        }
        at += FRAME_MS;
      }
      assert_eq!(ended_after, Some(10));
    }

    #[test]
    fn a_speaker_who_stops_sending_is_closed_when_idle() {
      let mut segmenter = Segmenter::new(SETTINGS);
      for index in 0..50 {
        segmenter.push_frame(index * FRAME_MS, &tone());
      }
      assert_eq!(segmenter.on_idle(49 * FRAME_MS + 100), None);
      assert!(segmenter.on_idle(49 * FRAME_MS + 700).is_some());
    }
  }

  mod capture {
    use super::*;

    #[test]
    fn opus_frames_become_one_piece_per_speaker() {
      let mut capture = Capture::new(SETTINGS);
      let mut encoder = OpusEncoder::new(OpusEncoderConfig::default()).unwrap();
      let (alice, bob) = (device(1), device(2));
      for index in 0..60u16 {
        let packet = encoder.encode(&tone()).unwrap();
        let at = u64::from(index) * FRAME_MS;
        assert_eq!(capture.push(alice, SequenceNumber(index), &packet, at), None);
        assert_eq!(capture.push(bob, SequenceNumber(index), &packet, at), None);
      }
      let pieces = capture.flush_all();
      assert_eq!(pieces.len(), 2);
      assert!(pieces.iter().any(|piece| piece.device == alice));
      assert!(pieces.iter().all(|piece| piece.samples.len() > 15_000));
    }

    #[test]
    fn late_and_repeated_frames_are_dropped_and_small_gaps_concealed() {
      let mut capture = Capture::new(SETTINGS);
      let mut encoder = OpusEncoder::new(OpusEncoderConfig::default()).unwrap();
      let alice = device(1);
      let packets: Vec<Vec<u8>> = (0..40).map(|_| encoder.encode(&tone()).unwrap()).collect();
      for (index, packet) in packets.iter().enumerate().take(30) {
        // Every fifth frame is lost on the way.
        if index % 5 != 4 {
          capture.push(
            alice,
            SequenceNumber(index as u16),
            packet,
            index as u64 * FRAME_MS,
          );
        }
      }
      capture.push(alice, SequenceNumber(3), &packets[3], 30 * FRAME_MS);
      let piece = capture.remove(alice).unwrap();
      // Frames 0 to 28 decoded or concealed; the repeated frame 3 added nothing.
      assert_eq!(piece.samples.len(), 29 * 320);
    }
  }

  mod signal {
    use super::*;

    #[test]
    fn levels_of_silence_and_a_tone() {
      assert_eq!(rms_dbfs(&silence()), -100.0);
      assert_eq!(rms_dbfs(&[]), -100.0);
      let level = rms_dbfs(&tone());
      assert!((-14.0..-12.0).contains(&level), "{level}");
    }

    #[test]
    fn downsampling_keeps_speech_and_removes_what_would_alias() {
      let speech = sine(440.0, 4_800);
      let low = downsample_to_16k(&speech);
      assert_eq!(low.len(), 1_600);
      assert!(rms_dbfs(&low[100..1_500]) > rms_dbfs(&speech) - 1.0);
      let high = downsample_to_16k(&sine(12_000.0, 4_800));
      assert!(rms_dbfs(&high[100..1_500]) < rms_dbfs(&speech) - 40.0);
    }
  }

  fn tone() -> Vec<i16> {
    sine(440.0, 960)
  }

  fn silence() -> Vec<i16> {
    vec![0; 960]
  }

  /// A sine at a third of full scale.
  fn sine(frequency_hz: f64, samples: usize) -> Vec<i16> {
    (0..samples)
      .map(|index| {
        let t = index as f64 / f64::from(CALL_SAMPLE_RATE_HZ);
        (f64::from(i16::MAX) / 3.0 * (2.0 * std::f64::consts::PI * frequency_hz * t).sin()) as i16
      })
      .collect()
  }

  fn device(raw: i64) -> DeviceId {
    DeviceId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
