//! Opus encoding and decoding for native clients (libopus). Browsers use WebCodecs instead.
//!
//! Fixed to 48 kHz, the call default (ARCHITECTURE.md §6.3). Frames are 20 ms unless a voice on a
//! thin uplink asks for 40 or 60 ms ones (#1893); bitrate, channels, in-band FEC, DTX and the
//! expected loss are settings. Decoders take packets of any length.

use opus::{Application, Bitrate, Channels, Decoder, Encoder};
use thiserror::Error;

pub use crate::audio_frame::{FRAME_SAMPLES_PER_CHANNEL, SAMPLE_RATE_HZ};
use crate::audio_frame::{MAX_PACKET_SAMPLES_PER_CHANNEL, planned_loss_percent};
use crate::voice_steps::{VoiceEncoding, VoiceFrame};
/// Largest Opus packet we produce (RFC 6716 allows 1275 bytes per frame).
const MAX_PACKET_BYTES: usize = 1275;

/// Encodes PCM frames (20 ms unless set otherwise) to Opus packets.
pub struct OpusEncoder {
  encoder: Encoder,
  channels: AudioChannels,
  frame: VoiceFrame,
  /// The configured expected loss: the least FEC is planned for.
  loss_floor_percent: u8,
}

/// Decodes Opus packets and conceals lost ones.
pub struct OpusDecoder {
  decoder: Decoder,
  channels: AudioChannels,
}

/// Bitrate of a shared screen's sound: music and video sound need more than speech.
const SCREEN_AUDIO_BITRATE_BPS: i32 = 64_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpusEncoderConfig {
  pub content: AudioContent,
  pub channels: AudioChannels,
  pub bitrate_bps: i32,
  /// In-band forward error correction: the next packet carries a low-bitrate copy of this one.
  pub has_inband_fec: bool,
  /// Discontinuous transmission: (almost) nothing is sent during silence.
  pub has_dtx: bool,
  /// Loss the encoder plans FEC for, in percent.
  pub expected_loss_percent: u8,
  /// Packet sizes follow the content. Off, every packet has the same size: the SFU then can't
  /// tell what plays from the sizes (DAVE doesn't pad).
  pub has_variable_bitrate: bool,
}

/// What the encoder is tuned for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioContent {
  /// A microphone: speech intelligibility first.
  Voice,
  /// A shared screen's sound: music and video as they are.
  Music,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioChannels {
  Mono,
  Stereo,
}

#[derive(Debug, Error)]
pub enum OpusError {
  #[error("libopus: {0}")]
  Codec(#[from] opus::Error),
  #[error("frame has {actual} samples, expected {expected}")]
  WrongFrameSize { expected: usize, actual: usize },
}

impl Default for OpusEncoderConfig {
  fn default() -> Self {
    Self {
      content: AudioContent::Voice,
      channels: AudioChannels::Mono,
      bitrate_bps: 32_000,
      has_inband_fec: true,
      has_dtx: true,
      expected_loss_percent: 5,
      has_variable_bitrate: true,
    }
  }
}

impl OpusEncoderConfig {
  /// A shared screen's sound: tuned for music, no silence suppression of quiet passages.
  pub fn screen_audio() -> Self {
    Self {
      content: AudioContent::Music,
      bitrate_bps: SCREEN_AUDIO_BITRATE_BPS,
      has_dtx: false,
      // Shared music or video could be recognised from variable packet sizes.
      has_variable_bitrate: false,
      ..Self::default()
    }
  }
}

impl OpusEncoder {
  pub fn new(config: OpusEncoderConfig) -> Result<Self, OpusError> {
    let application = match config.content {
      AudioContent::Voice => Application::Voip,
      AudioContent::Music => Application::Audio,
    };
    let mut encoder = Encoder::new(SAMPLE_RATE_HZ, config.channels.into(), application)?;
    encoder.set_bitrate(Bitrate::Bits(config.bitrate_bps))?;
    encoder.set_inband_fec(config.has_inband_fec)?;
    encoder.set_dtx(config.has_dtx)?;
    encoder.set_vbr(config.has_variable_bitrate)?;
    encoder.set_packet_loss_perc(i32::from(config.expected_loss_percent.min(100)))?;
    Ok(Self {
      encoder,
      channels: config.channels,
      frame: VoiceFrame::Ms20,
      loss_floor_percent: config.expected_loss_percent.min(100),
    })
  }

  /// Plans in-band FEC for the loss measured on the uplink (never below the configured value).
  pub fn set_measured_loss_percent(&mut self, measured_percent: u8) -> Result<(), OpusError> {
    let planned = planned_loss_percent(self.loss_floor_percent, measured_percent);
    Ok(self.encoder.set_packet_loss_perc(i32::from(planned))?)
  }

  /// Encodes at `encoding`'s bitrate in frames of its length from the next frame on (#1893).
  pub fn set_encoding(&mut self, encoding: VoiceEncoding) -> Result<(), OpusError> {
    self.encoder.set_bitrate(Bitrate::Bits(
      i32::try_from(encoding.bitrate_bps).unwrap_or(i32::MAX),
    ))?;
    self.frame = encoding.frame;
    Ok(())
  }

  /// Samples per channel of the frames [`Self::encode`] takes.
  pub fn frame_samples_per_channel(&self) -> usize {
    self.frame.samples()
  }

  /// The encoder's delay in 48 kHz samples: what a file's player drops from the start.
  pub fn lookahead_samples(&mut self) -> Result<u16, OpusError> {
    Ok(u16::try_from(self.encoder.get_lookahead()?.max(0)).unwrap_or(u16::MAX))
  }

  /// Encodes one frame of [`Self::frame_samples_per_channel`] (interleaved samples for stereo).
  pub fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>, OpusError> {
    let expected = self.frame.samples() * self.channels.count();
    if pcm.len() != expected {
      return Err(OpusError::WrongFrameSize {
        expected,
        actual: pcm.len(),
      });
    }
    Ok(self.encoder.encode_vec(pcm, MAX_PACKET_BYTES)?)
  }
}

impl OpusDecoder {
  pub fn new(channels: AudioChannels) -> Result<Self, OpusError> {
    Ok(Self {
      decoder: Decoder::new(SAMPLE_RATE_HZ, channels.into())?,
      channels,
    })
  }

  /// Decodes one packet, of whatever length it is (20 ms, or 40 or 60 ms from a voice on a thin
  /// uplink).
  pub fn decode(&mut self, packet: &[u8]) -> Result<Vec<i16>, OpusError> {
    self.decode_into(packet, false, MAX_PACKET_SAMPLES_PER_CHANNEL)
  }

  /// `lost_samples` (per channel) for a lost packet of that length: recovered from the in-band FEC
  /// data of `next_packet` when it is available, otherwise packet loss concealment. Feed this for
  /// every [`crate::jitter::Playout::Missing`], with the length of the packet before it.
  pub fn conceal(
    &mut self,
    maybe_next_packet: Option<&[u8]>,
    lost_samples: usize,
  ) -> Result<Vec<i16>, OpusError> {
    match maybe_next_packet {
      Some(next_packet) => self.decode_into(next_packet, true, lost_samples),
      None => self.decode_into(&[], false, lost_samples),
    }
  }

  fn decode_into(
    &mut self,
    packet: &[u8],
    use_fec: bool,
    samples_per_channel: usize,
  ) -> Result<Vec<i16>, OpusError> {
    let mut pcm = vec![0i16; samples_per_channel.min(MAX_PACKET_SAMPLES_PER_CHANNEL) * self.channels.count()];
    let samples_per_channel = self.decoder.decode(packet, &mut pcm, use_fec)?;
    pcm.truncate(samples_per_channel * self.channels.count());
    Ok(pcm)
  }
}

impl AudioChannels {
  pub fn count(self) -> usize {
    match self {
      Self::Mono => 1,
      Self::Stereo => 2,
    }
  }
}

impl From<AudioChannels> for Channels {
  fn from(channels: AudioChannels) -> Self {
    match channels {
      AudioChannels::Mono => Self::Mono,
      AudioChannels::Stereo => Self::Stereo,
    }
  }
}

#[cfg(test)]
mod tests {
  use std::f64::consts::PI;

  use super::*;
  use crate::audio_level::{DEFAULT_VOICE_THRESHOLD_DBOV, measure_audio_level};

  #[test]
  fn round_trip_keeps_the_signal() {
    let mut encoder = OpusEncoder::new(OpusEncoderConfig::default()).unwrap();
    let mut decoder = OpusDecoder::new(AudioChannels::Mono).unwrap();
    let mut decoded = Vec::new();
    for frame in 0..10 {
      let packet = encoder.encode(&tone(frame)).unwrap();
      assert!(
        packet.len() < 200,
        "20 ms at 32 kbit/s is about 80 bytes, got {}",
        packet.len()
      );
      decoded = decoder.decode(&packet).unwrap();
    }
    assert_eq!(decoded.len(), FRAME_SAMPLES_PER_CHANNEL);
    // The codec is lossy, but the level of a steady tone survives within a few dB.
    let original = measure_audio_level(&tone(9), DEFAULT_VOICE_THRESHOLD_DBOV).level_dbov();
    let after = measure_audio_level(&decoded, DEFAULT_VOICE_THRESHOLD_DBOV).level_dbov();
    assert!(original.abs_diff(after) <= 3, "{original} vs {after}");
  }

  #[test]
  fn lost_packets_are_concealed_with_and_without_fec() {
    let mut encoder = OpusEncoder::new(OpusEncoderConfig::default()).unwrap();
    let mut decoder = OpusDecoder::new(AudioChannels::Mono).unwrap();
    let packets: Vec<Vec<u8>> = (0..4)
      .map(|frame| encoder.encode(&tone(frame)).unwrap())
      .collect();
    decoder.decode(&packets[0]).unwrap();
    assert_eq!(
      decoder
        .conceal(Some(&packets[2]), FRAME_SAMPLES_PER_CHANNEL)
        .unwrap()
        .len(),
      FRAME_SAMPLES_PER_CHANNEL
    );
    decoder.decode(&packets[2]).unwrap();
    assert_eq!(
      decoder.conceal(None, FRAME_SAMPLES_PER_CHANNEL).unwrap().len(),
      FRAME_SAMPLES_PER_CHANNEL
    );
  }

  #[test]
  fn stereo_frames_are_interleaved() {
    let config = OpusEncoderConfig {
      channels: AudioChannels::Stereo,
      ..OpusEncoderConfig::default()
    };
    let mut encoder = OpusEncoder::new(config).unwrap();
    let mut decoder = OpusDecoder::new(AudioChannels::Stereo).unwrap();
    let stereo: Vec<i16> = tone(0).iter().flat_map(|&sample| [sample, sample]).collect();
    let packet = encoder.encode(&stereo).unwrap();
    assert_eq!(
      decoder.decode(&packet).unwrap().len(),
      2 * FRAME_SAMPLES_PER_CHANNEL
    );
  }

  #[test]
  fn wrong_frame_size_is_rejected() {
    let mut encoder = OpusEncoder::new(OpusEncoderConfig::default()).unwrap();
    assert!(matches!(
      encoder.encode(&[0; 480]),
      Err(OpusError::WrongFrameSize {
        expected: 960,
        actual: 480
      })
    ));
  }

  /// A voice on a thin uplink (#1893): 12 kbit/s in 60 ms packets, about a third of the packets
  /// at well under half the bytes; the decoder plays and conceals them at their length.
  #[test]
  fn long_frames_round_trip_at_a_low_bitrate() {
    let mut encoder = OpusEncoder::new(OpusEncoderConfig::default()).unwrap();
    encoder
      .set_encoding(VoiceEncoding {
        bitrate_bps: 12_000,
        frame: VoiceFrame::Ms60,
      })
      .unwrap();
    assert_eq!(encoder.frame_samples_per_channel(), 2880);
    assert!(matches!(
      encoder.encode(&tone(0)),
      Err(OpusError::WrongFrameSize { expected: 2880, .. })
    ));
    let mut decoder = OpusDecoder::new(AudioChannels::Mono).unwrap();
    let mut decoded = Vec::new();
    let mut bytes = 0;
    for frame in 0..10 {
      let pcm: Vec<i16> = (0..3).flat_map(|third| tone(3 * frame + third)).collect();
      let packet = encoder.encode(&pcm).unwrap();
      assert_eq!(crate::audio_frame::opus_packet_samples(&packet), Some(2880));
      bytes += packet.len();
      decoded = decoder.decode(&packet).unwrap();
    }
    // Three 20 ms packets at 32 kbit/s are about 240 bytes.
    assert!(
      bytes / 10 < 160,
      "60 ms at 12 kbit/s, {} bytes a packet",
      bytes / 10
    );
    assert_eq!(decoded.len(), 2880);
    let original = measure_audio_level(&tone(29), DEFAULT_VOICE_THRESHOLD_DBOV).level_dbov();
    let after = measure_audio_level(&decoded[1920..], DEFAULT_VOICE_THRESHOLD_DBOV).level_dbov();
    assert!(original.abs_diff(after) <= 6, "{original} vs {after}");
    assert_eq!(decoder.conceal(None, 2880).unwrap().len(), 2880);
  }

  #[test]
  fn garbage_packet_is_an_error_not_a_panic() {
    let mut decoder = OpusDecoder::new(AudioChannels::Mono).unwrap();
    // TOC byte announcing a code-3 packet with an impossible frame count.
    assert!(decoder.decode(&[0xFF, 0xFF, 0xFF]).is_err());
  }

  /// Frame `frame` of a continuous 440 Hz tone at half scale.
  fn tone(frame: usize) -> Vec<i16> {
    (0..FRAME_SAMPLES_PER_CHANNEL)
      .map(|i| {
        let t = (frame * FRAME_SAMPLES_PER_CHANNEL + i) as f64 / f64::from(SAMPLE_RATE_HZ);
        (0.5 * f64::from(i16::MAX) * (2.0 * PI * 440.0 * t).sin()) as i16
      })
      .collect()
  }
}
