//! Native audio pipeline between devices and the network: microphone samples → (audio processing,
//! every 20 ms) → Opus packets (20 ms, or 40 or 60 ms on a thin uplink, #1893) with their level,
//! and received Opus packets of any length → jitter buffer → decoded, mixed 20 ms frames for the
//! speakers.
//!
//! Device-independent so tests and the headless client feed samples directly.

use std::collections::{BTreeMap, VecDeque};

use teamcraft_proto::DeviceId;

use crate::audio_codec::{
  AudioChannels, FRAME_SAMPLES_PER_CHANNEL, OpusDecoder, OpusEncoder, OpusEncoderConfig, OpusError,
};
use crate::audio_diagnostics::{LevelSummary, LevelWindow};
pub use crate::audio_frame::EncodedAudioFrame;
use crate::audio_frame::opus_packet_samples;
use crate::audio_level::{DEFAULT_VOICE_THRESHOLD_DBOV, measure_audio_level};
use crate::audio_processing::{AudioProcessingError, AudioProcessor, PROCESSING_FRAME_SAMPLES};
use crate::call_engine::AudioOrigin;
use crate::jitter::{JitterBuffer, JitterBufferConfig, Playout};
use crate::rtp::SequenceNumber;
use crate::voice_steps::{VoiceEncoding, VoiceStepConfig};

/// Microphone side: processes samples in 20 ms frames and encodes them in packets of the
/// encoder's length.
pub struct CapturePipeline {
  encoder: OpusEncoder,
  pending: Vec<f32>,
  /// Processed samples waiting for a packet's worth.
  processed: Vec<f32>,
  is_muted: bool,
  maybe_processor: Option<std::sync::Arc<AudioProcessor>>,
  /// How the voice's encoder was set up: the thin copy's starts the same.
  encoder_config: OpusEncoderConfig,
  /// The voice's thin copy while it is sent (#1893 step 3).
  maybe_thin: Option<ThinCopy>,
  /// The microphone's level as it came and as it leaves after processing, for the debug log
  /// (#2418).
  microphone_level: LevelWindow,
  sent_level: LevelWindow,
}

/// The voice a second time at the thinnest step, for receivers on thin downlinks (#1893 step 3):
/// its own encoder, fed the same processed samples.
struct ThinCopy {
  encoder: OpusEncoder,
  processed: Vec<f32>,
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
  /// The level of what the speakers are given, for the debug log (#2418).
  played_level: LevelWindow,
  /// Where the others' voices go for live captions on this device (#215), while they are on.
  #[cfg(feature = "captions")]
  maybe_caption_tap: Option<crate::captions::CaptionTap>,
}

struct RemoteAudio {
  jitter: JitterBuffer<Vec<u8>>,
  decoder: OpusDecoder,
  /// Decoded samples not played yet: a packet longer than 20 ms plays over several frames.
  decoded: VecDeque<f32>,
  /// Samples per channel of the last packet played: what a lost one is concealed for.
  last_packet_samples: usize,
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
    let encoder_config = OpusEncoderConfig {
      channels: AudioChannels::Mono,
      ..config
    };
    Ok(Self {
      encoder: OpusEncoder::new(encoder_config)?,
      pending: Vec::with_capacity(2 * FRAME_SAMPLES_PER_CHANNEL),
      processed: Vec::new(),
      is_muted: false,
      maybe_processor: None,
      encoder_config,
      maybe_thin: None,
      microphone_level: LevelWindow::default(),
      sent_level: LevelWindow::default(),
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
    self.processed.clear();
    if let Some(thin) = &mut self.maybe_thin {
      thin.processed.clear();
    }
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

  /// Encodes at another bitrate and packet length from the next packet on (#1893).
  pub fn set_voice_encoding(&mut self, encoding: VoiceEncoding) -> Result<(), AudioPipelineError> {
    Ok(self.encoder.set_encoding(encoding)?)
  }

  /// Starts or stops encoding the voice's thin copy (#1893 step 3): the thinnest step, from the
  /// next samples on.
  pub fn set_thin_voice(&mut self, is_sent: bool) -> Result<(), AudioPipelineError> {
    if !is_sent {
      self.maybe_thin = None;
      return Ok(());
    }
    if self.maybe_thin.is_none() {
      let mut encoder = OpusEncoder::new(self.encoder_config)?;
      encoder.set_encoding(VoiceStepConfig::default().thinnest)?;
      self.maybe_thin = Some(ThinCopy {
        encoder,
        processed: Vec::new(),
      });
    }
    Ok(())
  }

  /// The thin copy's packets that became complete with what [`Self::push`] took.
  pub fn take_thin_frames(&mut self) -> Result<Vec<EncodedAudioFrame>, AudioPipelineError> {
    let Some(thin) = &mut self.maybe_thin else {
      return Ok(Vec::new());
    };
    let packet_samples = thin.encoder.frame_samples_per_channel();
    let mut frames = Vec::new();
    while thin.processed.len() >= packet_samples {
      let pcm: Vec<i16> = thin.processed.drain(..packet_samples).map(to_i16).collect();
      let level = measure_audio_level(&pcm, DEFAULT_VOICE_THRESHOLD_DBOV);
      frames.push(EncodedAudioFrame {
        opus: thin.encoder.encode(&pcm)?,
        level,
      });
    }
    Ok(frames)
  }

  /// Feeds mono 48 kHz samples (any chunk size); returns the packets that became complete.
  pub fn push(&mut self, samples: &[f32]) -> Result<Vec<EncodedAudioFrame>, AudioPipelineError> {
    if self.is_muted {
      return Ok(Vec::new());
    }
    self.pending.extend_from_slice(samples);
    while self.pending.len() >= FRAME_SAMPLES_PER_CHANNEL {
      let mut frame: Vec<f32> = self.pending.drain(..FRAME_SAMPLES_PER_CHANNEL).collect();
      self.microphone_level.push(&frame);
      self.process(&mut frame)?;
      self.sent_level.push(&frame);
      if let Some(thin) = &mut self.maybe_thin {
        thin.processed.extend_from_slice(&frame);
      }
      self.processed.extend(frame);
    }
    let packet_samples = self.encoder.frame_samples_per_channel();
    let mut frames = Vec::new();
    while self.processed.len() >= packet_samples {
      let pcm: Vec<i16> = self.processed.drain(..packet_samples).map(to_i16).collect();
      let level = measure_audio_level(&pcm, DEFAULT_VOICE_THRESHOLD_DBOV);
      frames.push(EncodedAudioFrame {
        opus: self.encoder.encode(&pcm)?,
        level,
      });
    }
    Ok(frames)
  }

  /// The microphone's level as the device delivered it and as it is sent, since the last call
  /// (#2418). Muted, nothing is measured.
  pub fn take_levels(&mut self) -> (LevelSummary, LevelSummary) {
    (self.microphone_level.take(), self.sent_level.take())
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
      played_level: LevelWindow::default(),
      #[cfg(feature = "captions")]
      maybe_caption_tap: None,
    }
  }

  /// Feeds the mixed output to the echo canceller as the far-end signal.
  pub fn with_processor(mut self, processor: std::sync::Arc<AudioProcessor>) -> Self {
    self.maybe_processor = Some(processor);
    self
  }

  /// The call moved to a path with other timing (ARCHITECTURE.md §6.6): every stream's buffer
  /// follows, also the ones of people who start talking later.
  pub fn set_jitter_config(&mut self, jitter_config: JitterBufferConfig) {
    self.jitter_config = jitter_config;
    for stream in self.streams.values_mut() {
      stream.jitter.set_config(jitter_config);
    }
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
        decoded: VecDeque::new(),
        last_packet_samples: FRAME_SAMPLES_PER_CHANNEL,
      }),
    };
    // The buffer's depths count packets of the length that arrives, from the first one on.
    if let Some(samples) = opus_packet_samples(&opus) {
      stream
        .jitter
        .set_packet_frames(samples.div_ceil(FRAME_SAMPLES_PER_CHANNEL));
    }
    stream.jitter.push(sequence_number, opus);
    Ok(())
  }

  /// A participant left: forget its streams.
  pub fn remove(&mut self, sender: DeviceId) {
    self.streams.retain(|(device_id, _), _| *device_id != sender);
    #[cfg(feature = "captions")]
    if let Some(tap) = &self.maybe_caption_tap {
      tap.end(sender);
    }
  }

  /// One of a participant's streams ended (a screen share's sound stopped).
  pub fn remove_stream(&mut self, sender: DeviceId, origin: AudioOrigin) {
    self.streams.remove(&(sender, origin));
    #[cfg(feature = "captions")]
    if origin == AudioOrigin::Microphone
      && let Some(tap) = &self.maybe_caption_tap
    {
      tap.end(sender);
    }
  }

  /// Hands the others' voices (their microphones, not shared screens' sound) to live captions,
  /// frame by frame as they are played, or stops with `None` (#215). Deafened, nothing is
  /// captioned: nothing is heard either.
  #[cfg(feature = "captions")]
  pub fn set_caption_tap(&mut self, maybe_tap: Option<crate::captions::CaptionTap>) {
    self.maybe_caption_tap = maybe_tap;
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
    Ok(self.next_frames(None)?.0)
  }

  /// The next 20 ms of mixed audio, and with `Some(admitted)` also a second mix of only those
  /// devices' streams: what a recording that asked for consent may take in (#2115). Everybody
  /// else's voice stays out of it, played or not.
  pub fn next_frames(
    &mut self,
    maybe_admitted: Option<&[DeviceId]>,
  ) -> Result<(Vec<f32>, Option<Vec<f32>>), AudioPipelineError> {
    let mut mix = vec![0.0f32; FRAME_SAMPLES_PER_CHANNEL];
    let mut maybe_admitted_mix = maybe_admitted.map(|_| vec![0.0f32; FRAME_SAMPLES_PER_CHANNEL]);
    for ((sender, origin), stream) in self.streams.iter_mut() {
      stream.decode_frame()?;
      let available = stream.decoded.len().min(FRAME_SAMPLES_PER_CHANNEL);
      let frame: Vec<f32> = stream.decoded.drain(..available).collect();
      if self.is_deafened {
        continue;
      }
      #[cfg(feature = "captions")]
      if let (Some(tap), AudioOrigin::Microphone) = (&self.maybe_caption_tap, origin) {
        tap.push(*sender, &frame);
      }
      // The stream's origin matters only to captions.
      let _ = origin;
      if let (Some(admitted_mix), Some(admitted)) = (maybe_admitted_mix.as_mut(), maybe_admitted)
        && admitted.contains(sender)
      {
        for (slot, sample) in admitted_mix.iter_mut().zip(&frame) {
          *slot += sample;
        }
      }
      for (slot, sample) in mix.iter_mut().zip(frame) {
        *slot += sample;
      }
    }
    for sample in mix.iter_mut().chain(maybe_admitted_mix.iter_mut().flatten()) {
      *sample = sample.clamp(-1.0, 1.0);
    }
    self.played_level.push(&mix);
    self.analyze(&mix)?;
    Ok((mix, maybe_admitted_mix))
  }

  /// The level of the mixes played since the last call (#2418), status sounds not included.
  pub fn take_played_level(&mut self) -> LevelSummary {
    self.played_level.take()
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

impl RemoteAudio {
  /// Decodes packets until a 20 ms frame is there to play, or the buffer has nothing due.
  fn decode_frame(&mut self) -> Result<(), AudioPipelineError> {
    while self.decoded.len() < FRAME_SAMPLES_PER_CHANNEL {
      let pcm = match self.jitter.pop() {
        Some(Playout::Packet { item, .. }) => {
          let pcm = self.decoder.decode(&item)?;
          if !pcm.is_empty() {
            self.last_packet_samples = pcm.len();
          }
          pcm
        },
        // The next packet's in-band FEC recovers the lost one, if it is already here.
        Some(Playout::Missing { .. }) => {
          let maybe_next = self.jitter.peek_next().map(Vec::as_slice);
          // A lost packet is as long as the one before it; FEC data in the next one only where
          // that one is as long too (a step changed the length in between otherwise).
          let maybe_fec =
            maybe_next.filter(|next| opus_packet_samples(next) == Some(self.last_packet_samples));
          self.decoder.conceal(maybe_fec, self.last_packet_samples)?
        },
        None => break,
      };
      self.decoded.extend(
        pcm
          .into_iter()
          .map(|sample| f32::from(sample) / f32::from(i16::MAX)),
      );
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

    /// #1893 step 3: the thin copy encodes the same samples in 60 ms packets next to the voice.
    #[test]
    fn the_thin_copy_encodes_the_same_samples_in_60_ms_packets() {
      let mut pipeline = CapturePipeline::new(OpusEncoderConfig::default()).unwrap();
      assert!(pipeline.take_thin_frames().unwrap().is_empty(), "not sent yet");
      pipeline.set_thin_voice(true).unwrap();
      let voice = pipeline
        .push(&sine_tone(440.0, 0.5, 0, 6 * FRAME_SAMPLES_PER_CHANNEL))
        .unwrap();
      let thin = pipeline.take_thin_frames().unwrap();
      assert_eq!(voice.len(), 6);
      assert_eq!(thin.len(), 2);
      assert!(
        thin
          .iter()
          .all(|frame| opus_packet_samples(&frame.opus) == Some(2880))
      );
      assert!(thin.iter().all(|frame| frame.level.has_voice_activity));
      pipeline.set_thin_voice(false).unwrap();
      pipeline
        .push(&sine_tone(440.0, 0.5, 0, 3 * FRAME_SAMPLES_PER_CHANNEL))
        .unwrap();
      assert!(pipeline.take_thin_frames().unwrap().is_empty(), "stopped");
    }

    /// #1893 step 3: muted, nothing of the microphone reaches the thin copy's encoder either: it
    /// stays as an object for the next turn, and only what comes after the unmute is encoded.
    #[test]
    fn muted_the_thin_copy_encodes_nothing() {
      let mut pipeline = CapturePipeline::new(OpusEncoderConfig::default()).unwrap();
      pipeline.set_thin_voice(true).unwrap();
      // Half a packet before the mute: dropped with it, not sent after it.
      pipeline
        .push(&sine_tone(440.0, 0.5, 0, 2 * FRAME_SAMPLES_PER_CHANNEL))
        .unwrap();
      pipeline.set_muted(true);
      assert!(
        pipeline
          .push(&sine_tone(440.0, 0.5, 0, 9 * FRAME_SAMPLES_PER_CHANNEL))
          .unwrap()
          .is_empty()
      );
      assert!(pipeline.take_thin_frames().unwrap().is_empty());
      pipeline.set_muted(false);
      pipeline
        .push(&sine_tone(440.0, 0.5, 0, 3 * FRAME_SAMPLES_PER_CHANNEL))
        .unwrap();
      assert_eq!(
        pipeline.take_thin_frames().unwrap().len(),
        1,
        "only what came after the unmute"
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
        max_adaptive_depth_packets: 1,
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

    /// #2115: a recording that asked for consent mixes only who allowed it; the speakers still
    /// play everybody.
    #[test]
    fn the_admitted_mix_leaves_out_who_didnt_allow_it() {
      let (a, b) = (device(1), device(2));
      let mut mixer = PlaybackMixer::new(JitterBufferConfig {
        target_depth_packets: 1,
        max_depth_packets: 10,
        max_adaptive_depth_packets: 1,
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
      let peak = |frame: &[f32]| frame.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
      let (mut played, mut admitted) = (Vec::new(), Vec::new());
      for _ in 0..10 {
        let (mix, maybe_admitted) = mixer.next_frames(Some(&[a])).unwrap();
        played = mix;
        admitted = maybe_admitted.unwrap();
      }
      assert!(peak(&played) > 0.35, "both play");
      // One tone of 0.3 is in it, not the sum of both.
      assert!(peak(&admitted) > 0.1, "the allowed voice is in it");
      assert!(peak(&admitted) < 0.35, "the other isn't");
      assert!(
        mixer
          .next_frames(Some(&[]))
          .unwrap()
          .1
          .unwrap()
          .iter()
          .all(|sample| *sample == 0.0)
      );
      assert!(mixer.next_frames(None).unwrap().1.is_none());
    }

    /// A voice on a thin uplink (#1893) sends 60 ms packets: each plays over three 20 ms frames,
    /// a lost one is concealed for 60 ms, and the stream keeps time with the sender's capture.
    #[test]
    fn long_packets_play_at_their_length() {
      let mut mixer = PlaybackMixer::new(JitterBufferConfig {
        target_depth_packets: 3,
        max_depth_packets: 50,
        max_excess_packets: 10,
        max_adaptive_depth_packets: 3,
      });
      let mut capture = CapturePipeline::new(OpusEncoderConfig::default()).unwrap();
      capture
        .set_voice_encoding(VoiceEncoding {
          bitrate_bps: 12_000,
          frame: crate::voice_steps::VoiceFrame::Ms60,
        })
        .unwrap();
      let packets: Vec<Vec<u8>> = (0..30u64)
        .flat_map(|index| capture.push(&sine_tone(300.0, 0.3, index * 960, 960)).unwrap())
        .map(|frame| frame.opus)
        .collect();
      assert_eq!(packets.len(), 10, "600 ms in 60 ms packets");
      let mut played = Vec::new();
      // Each packet arrives while the one before it plays; packet 5 is lost.
      for (index, packet) in packets.into_iter().enumerate() {
        if index != 5 {
          mixer
            .push(
              device(1),
              AudioOrigin::Microphone,
              SequenceNumber(index as u16),
              packet,
            )
            .unwrap();
        }
        if index > 0 {
          played.extend((0..3).map(|_| mixer.next_frame().unwrap()));
        }
      }
      played.extend((0..3).map(|_| mixer.next_frame().unwrap()));
      assert_eq!(played.len(), 30);
      // Low but not silent: the decoder fades in after concealing a pure tone.
      let audible = |frame: &Vec<f32>| frame.iter().any(|sample| sample.abs() > 0.02);
      // Every frame plays sound, the lost packet's concealed 60 ms included: one packet was the
      // 60 ms of the target, the packets never ran dry, and the lost one didn't skip ahead.
      assert!(
        played.iter().all(audible),
        "{:?}",
        played
          .iter()
          .map(|frame| frame.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs())))
          .collect::<Vec<_>>()
      );
      assert!(
        mixer.next_frame().unwrap().iter().all(|sample| *sample == 0.0),
        "all played"
      );
    }

    #[test]
    fn a_speaker_and_their_screen_sound_are_separate_streams() {
      let mut mixer = PlaybackMixer::new(JitterBufferConfig {
        target_depth_packets: 1,
        max_depth_packets: 10,
        max_adaptive_depth_packets: 1,
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
        max_adaptive_depth_packets: 1,
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
