//! Native audio pipeline between devices and the network: microphone samples → (audio processing)
//! → 20 ms Opus frames with their level, and received Opus packets → jitter buffer → decoded,
//! mixed 20 ms frames for the speakers.
//!
//! Device-independent so tests and the headless client feed samples directly.

use std::collections::{BTreeMap, VecDeque};

use teamcraft_proto::DeviceId;

use crate::audio_codec::{
  AudioChannels, FRAME_SAMPLES_PER_CHANNEL, OpusDecoder, OpusEncoder, OpusEncoderConfig, OpusError,
};
pub use crate::audio_frame::EncodedAudioFrame;
use crate::audio_level::{DEFAULT_VOICE_THRESHOLD_DBOV, measure_audio_level};
use crate::audio_processing::{AudioProcessingError, AudioProcessor, PROCESSING_FRAME_SAMPLES};
use crate::call_engine::AudioOrigin;
use crate::jitter::{JitterBuffer, JitterBufferConfig, Playout};
use crate::rtp::SequenceNumber;

/// Microphone side: chops samples into 20 ms frames and encodes them.
pub struct CapturePipeline {
  encoder: OpusEncoder,
  pending: Vec<f32>,
  is_muted: bool,
  maybe_processor: Option<std::sync::Arc<AudioProcessor>>,
}

/// At most this much of the own shared screen's sound waits to be added to the echo reference.
const MAX_ECHO_REFERENCE_SAMPLES: usize = 48_000 / 5;

/// Speaker side: one jitter buffer and decoder per remote stream (microphone or shared screen
/// sound of each participant), mixed every 20 ms.
pub struct PlaybackMixer {
  jitter_config: JitterBufferConfig,
  streams: BTreeMap<(DeviceId, AudioOrigin), RemoteAudio>,
  is_deafened: bool,
  maybe_processor: Option<std::sync::Arc<AudioProcessor>>,
  /// The sound of our own shared screen: the speakers play it too, so the echo canceller has to
  /// know it, or the microphone sends it a second time.
  echo_reference: VecDeque<f32>,
}

struct RemoteAudio {
  jitter: JitterBuffer<Vec<u8>>,
  decoder: OpusDecoder,
}

#[derive(Debug, thiserror::Error)]
pub enum AudioPipelineError {
  #[error(transparent)]
  Opus(#[from] OpusError),
  #[error(transparent)]
  Processing(#[from] AudioProcessingError),
}

impl CapturePipeline {
  pub fn new(config: OpusEncoderConfig) -> Result<Self, AudioPipelineError> {
    Ok(Self {
      encoder: OpusEncoder::new(OpusEncoderConfig {
        channels: AudioChannels::Mono,
        ..config
      })?,
      pending: Vec::with_capacity(2 * FRAME_SAMPLES_PER_CHANNEL),
      is_muted: false,
      maybe_processor: None,
    })
  }

  /// Runs capture through echo cancellation, noise suppression and gain control.
  pub fn with_processor(mut self, processor: std::sync::Arc<AudioProcessor>) -> Self {
    self.maybe_processor = Some(processor);
    self
  }

  /// While muted nothing is encoded or sent (the SFU then sees no audio from us).
  pub fn set_muted(&mut self, is_muted: bool) {
    self.is_muted = is_muted;
    self.pending.clear();
    if let Some(processor) = &self.maybe_processor {
      processor.set_capture_muted(is_muted);
    }
  }

  pub fn is_muted(&self) -> bool {
    self.is_muted
  }

  /// Adapts the encoder's forward error correction to the loss measured on the uplink.
  pub fn set_measured_loss_percent(&mut self, measured_percent: u8) -> Result<(), AudioPipelineError> {
    Ok(self.encoder.set_measured_loss_percent(measured_percent)?)
  }

  /// Feeds mono 48 kHz samples (any chunk size); returns the frames that became complete.
  pub fn push(&mut self, samples: &[f32]) -> Result<Vec<EncodedAudioFrame>, AudioPipelineError> {
    if self.is_muted {
      return Ok(Vec::new());
    }
    self.pending.extend_from_slice(samples);
    let mut frames = Vec::new();
    while self.pending.len() >= FRAME_SAMPLES_PER_CHANNEL {
      let mut frame: Vec<f32> = self.pending.drain(..FRAME_SAMPLES_PER_CHANNEL).collect();
      self.process(&mut frame)?;
      let pcm: Vec<i16> = frame.iter().map(|sample| to_i16(*sample)).collect();
      let level = measure_audio_level(&pcm, DEFAULT_VOICE_THRESHOLD_DBOV);
      frames.push(EncodedAudioFrame {
        opus: self.encoder.encode(&pcm)?,
        level,
      });
    }
    Ok(frames)
  }

  fn process(&self, frame: &mut [f32]) -> Result<(), AudioPipelineError> {
    if let Some(processor) = &self.maybe_processor {
      for chunk in frame.chunks_mut(PROCESSING_FRAME_SAMPLES) {
        processor.process_capture(chunk)?;
      }
    }
    Ok(())
  }
}

impl PlaybackMixer {
  pub fn new(jitter_config: JitterBufferConfig) -> Self {
    Self {
      jitter_config,
      streams: BTreeMap::new(),
      is_deafened: false,
      maybe_processor: None,
      echo_reference: VecDeque::new(),
    }
  }

  /// Feeds the mixed output to the echo canceller as the far-end signal.
  pub fn with_processor(mut self, processor: std::sync::Arc<AudioProcessor>) -> Self {
    self.maybe_processor = Some(processor);
    self
  }

  /// While deafened, received audio is dropped and silence is played.
  pub fn set_deafened(&mut self, is_deafened: bool) {
    self.is_deafened = is_deafened;
  }

  pub fn is_deafened(&self) -> bool {
    self.is_deafened
  }

  /// Queues one decrypted Opus packet from `sender`'s microphone or shared screen.
  pub fn push(
    &mut self,
    sender: DeviceId,
    origin: AudioOrigin,
    sequence_number: SequenceNumber,
    opus: Vec<u8>,
  ) -> Result<(), AudioPipelineError> {
    let stream = match self.streams.entry((sender, origin)) {
      std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
      std::collections::btree_map::Entry::Vacant(entry) => entry.insert(RemoteAudio {
        jitter: JitterBuffer::new(self.jitter_config),
        decoder: OpusDecoder::new(AudioChannels::Mono)?,
      }),
    };
    stream.jitter.push(sequence_number, opus);
    Ok(())
  }

  /// A participant left: forget its streams.
  pub fn remove(&mut self, sender: DeviceId) {
    self.streams.retain(|(device_id, _), _| *device_id != sender);
  }

  /// One of a participant's streams ended (a screen share's sound stopped).
  pub fn remove_stream(&mut self, sender: DeviceId, origin: AudioOrigin) {
    self.streams.remove(&(sender, origin));
  }

  /// Our own shared screen's sound as it is captured (mono 48 kHz), for the echo canceller only;
  /// it is not played back here.
  pub fn push_echo_reference(&mut self, samples: &[f32]) {
    if self.maybe_processor.is_none() {
      return;
    }
    self.echo_reference.extend(samples);
    let excess = self
      .echo_reference
      .len()
      .saturating_sub(MAX_ECHO_REFERENCE_SAMPLES);
    self.echo_reference.drain(..excess);
  }

  /// The next 20 ms of mixed audio (call every 20 ms). Lost packets are concealed.
  pub fn next_frame(&mut self) -> Result<Vec<f32>, AudioPipelineError> {
    let mut mix = vec![0.0f32; FRAME_SAMPLES_PER_CHANNEL];
    for stream in self.streams.values_mut() {
      let maybe_pcm = match stream.jitter.pop() {
        Some(Playout::Packet { item, .. }) => Some(stream.decoder.decode(&item)?),
        // The next packet's in-band FEC recovers the lost one, if it is already here.
        Some(Playout::Missing { .. }) => {
          let maybe_next = stream.jitter.peek_next().map(Vec::as_slice);
          Some(stream.decoder.conceal(maybe_next)?)
        },
        None => None,
      };
      if let Some(pcm) = maybe_pcm.filter(|_| !self.is_deafened) {
        for (slot, sample) in mix.iter_mut().zip(pcm) {
          *slot += f32::from(sample) / f32::from(i16::MAX);
        }
      }
    }
    for sample in &mut mix {
      *sample = sample.clamp(-1.0, 1.0);
    }
    self.analyze(&mix)?;
    Ok(mix)
  }

  /// The far-end signal for the echo canceller: what the speakers play, plus our own shared
  /// screen's sound, which the system plays through the same speakers.
  fn analyze(&mut self, mix: &[f32]) -> Result<(), AudioPipelineError> {
    let Some(processor) = &self.maybe_processor else {
      return Ok(());
    };
    let mut render = mix.to_vec();
    for (slot, sample) in render.iter_mut().zip(
      self
        .echo_reference
        .drain(..mix.len().min(self.echo_reference.len())),
    ) {
      *slot = (*slot + sample).clamp(-1.0, 1.0);
    }
    for chunk in render.chunks(PROCESSING_FRAME_SAMPLES) {
      processor.analyze_render(chunk)?;
    }
    Ok(())
  }
}

fn to_i16(sample: f32) -> i16 {
  // Clamped first, so the cast can't overflow.
  (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::audio_io::sine_tone;

  mod capture {
    use super::*;

    #[test]
    fn frames_come_out_every_20_ms_regardless_of_chunking() {
      let mut pipeline = CapturePipeline::new(OpusEncoderConfig::default()).unwrap();
      let tone = sine_tone(440.0, 0.5, 0, 3 * FRAME_SAMPLES_PER_CHANNEL + 100);
      let frames: Vec<EncodedAudioFrame> = tone
        .chunks(441)
        .flat_map(|chunk| pipeline.push(chunk).unwrap())
        .collect();
      assert_eq!(frames.len(), 3);
      // A half-scale sine is about 9 dBov and counts as voice.
      assert!(
        frames
          .iter()
          .all(|frame| frame.level.level_dbov() <= 10 && frame.level.has_voice_activity)
      );
    }

    #[test]
    fn muted_capture_produces_nothing() {
      let mut pipeline = CapturePipeline::new(OpusEncoderConfig::default()).unwrap();
      pipeline.set_muted(true);
      assert!(pipeline.push(&sine_tone(440.0, 0.5, 0, 4000)).unwrap().is_empty());
      pipeline.set_muted(false);
      assert_eq!(pipeline.push(&sine_tone(440.0, 0.5, 0, 960)).unwrap().len(), 1);
    }
  }

  mod playback {
    use super::*;

    #[test]
    fn two_speakers_are_mixed() {
      let (a, b) = (device(1), device(2));
      let mut mixer = PlaybackMixer::new(JitterBufferConfig {
        target_depth_packets: 1,
        max_depth_packets: 10,
        max_excess_packets: 10,
      });
      let mut first = CapturePipeline::new(OpusEncoderConfig::default()).unwrap();
      let mut second = CapturePipeline::new(OpusEncoderConfig::default()).unwrap();
      for index in 0..10u16 {
        let start = u64::from(index) * 960;
        for frame in first.push(&sine_tone(300.0, 0.3, start, 960)).unwrap() {
          mixer
            .push(a, AudioOrigin::Microphone, SequenceNumber(index), frame.opus)
            .unwrap();
        }
        for frame in second.push(&sine_tone(500.0, 0.3, start, 960)).unwrap() {
          mixer
            .push(b, AudioOrigin::Microphone, SequenceNumber(index), frame.opus)
            .unwrap();
        }
      }
      let mut last = Vec::new();
      for _ in 0..10 {
        last = mixer.next_frame().unwrap();
      }
      let peak = last.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
      assert!(peak > 0.35, "both tones are audible in the mix, peak {peak}");
    }

    #[test]
    fn a_speaker_and_their_screen_sound_are_separate_streams() {
      let mut mixer = PlaybackMixer::new(JitterBufferConfig {
        target_depth_packets: 1,
        max_depth_packets: 10,
        max_excess_packets: 10,
      });
      let mut voice = CapturePipeline::new(OpusEncoderConfig::default()).unwrap();
      let mut screen = CapturePipeline::new(OpusEncoderConfig::screen_audio()).unwrap();
      // Both streams start at sequence 0: one shared jitter buffer would drop half of it.
      for index in 0..10u16 {
        let start = u64::from(index) * 960;
        for frame in voice.push(&sine_tone(300.0, 0.3, start, 960)).unwrap() {
          mixer
            .push(
              device(1),
              AudioOrigin::Microphone,
              SequenceNumber(index),
              frame.opus,
            )
            .unwrap();
        }
        for frame in screen.push(&sine_tone(500.0, 0.3, start, 960)).unwrap() {
          mixer
            .push(device(1), AudioOrigin::Screen, SequenceNumber(index), frame.opus)
            .unwrap();
        }
      }
      assert_eq!(mixer.streams.len(), 2);
      mixer.remove_stream(device(1), AudioOrigin::Screen);
      assert_eq!(mixer.streams.len(), 1);
      mixer.remove(device(1));
      assert!(mixer.streams.is_empty());
    }

    #[test]
    fn deafened_mixer_plays_silence_and_gaps_are_concealed() {
      let mut mixer = PlaybackMixer::new(JitterBufferConfig {
        target_depth_packets: 1,
        max_depth_packets: 10,
        max_excess_packets: 10,
      });
      let mut capture = CapturePipeline::new(OpusEncoderConfig::default()).unwrap();
      let frames = capture.push(&sine_tone(300.0, 0.5, 0, 960 * 3)).unwrap();
      mixer
        .push(
          device(1),
          AudioOrigin::Microphone,
          SequenceNumber(0),
          frames[0].opus.clone(),
        )
        .unwrap();
      mixer
        .push(
          device(1),
          AudioOrigin::Microphone,
          SequenceNumber(2),
          frames[2].opus.clone(),
        )
        .unwrap();
      mixer.set_deafened(true);
      assert!(mixer.next_frame().unwrap().iter().all(|sample| *sample == 0.0));
      mixer.set_deafened(false);
      // Sequence 1 is missing: concealment still yields a full frame.
      assert_eq!(mixer.next_frame().unwrap().len(), FRAME_SAMPLES_PER_CHANNEL);
    }

    fn device(raw: i64) -> DeviceId {
      use teamcraft_proto::{Id, Snowflake};
      DeviceId::from_snowflake(Snowflake::new(raw).unwrap())
    }
  }
}
