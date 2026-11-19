//! The browser's voice decoder with Opus in-band FEC (#2140).
//!
//! WebCodecs' `AudioDecoder` can't decode the in-band FEC data an Opus packet carries for the one
//! before it, so a browser receiver conceals a lost voice packet (PLC) where the native receivers
//! recover it (`audio_pipeline`, through libopus). This is the decoder that recovers it in the
//! browser: `ropus`, a pure-Rust port of libopus, in a wasm module of its own
//! (`teamcraft-web-voice`), which runs in a worker the call module starts when the first voice
//! packet goes missing. A call without loss never downloads it, and the module stays out of a
//! guest's download (robustness.md, "Download size").
//!
//! [`FecVoiceDecoder`] is the decoder (feature `voice-fec`); [`protocol`] has the messages between
//! the call module and the worker.

#[cfg(feature = "voice-fec")]
use ropus::{Channels, DecodeMode, Decoder};
#[cfg(feature = "voice-fec")]
use thiserror::Error;

#[cfg(feature = "voice-fec")]
use crate::audio_frame::{
  FRAME_SAMPLES_PER_CHANNEL, MAX_PACKET_SAMPLES_PER_CHANNEL, SAMPLE_RATE_HZ, opus_packet_samples,
};

/// One remote voice's Opus decoder (mono, 48 kHz) that recovers a lost packet from the next one's
/// in-band FEC data where it can, as the native receivers do (`audio_codec::OpusDecoder::conceal`).
#[cfg(feature = "voice-fec")]
pub struct FecVoiceDecoder {
  decoder: Decoder,
  /// The length of the last packet decoded or concealed: a lost one is taken to be as long (#1893).
  last_packet_samples: usize,
}

#[cfg(feature = "voice-fec")]
#[derive(Debug, Error)]
pub enum VoiceFecError {
  #[error("creating the voice decoder: {0}")]
  Create(#[from] ropus::DecoderInitError),
  #[error("decoding a voice packet of {bytes} bytes: {source}")]
  Decode {
    bytes: usize,
    source: ropus::DecodeError,
  },
}

#[cfg(feature = "voice-fec")]
impl FecVoiceDecoder {
  pub fn new() -> Result<Self, VoiceFecError> {
    Ok(Self {
      decoder: Decoder::new(SAMPLE_RATE_HZ, Channels::Mono)?,
      last_packet_samples: FRAME_SAMPLES_PER_CHANNEL,
    })
  }

  /// The samples of `packet`; a packet the decoder refuses is concealed for its length instead, so
  /// every packet handed over comes back as audio of its length (the page counts on it).
  pub fn decode(&mut self, packet: &[u8]) -> Vec<f32> {
    if packet.is_empty() {
      // libopus reads no packet as a loss of the longest length it can play.
      return self.conceal(None, self.last_packet_samples);
    }
    match self.decode_packet(packet) {
      Ok(samples) => samples,
      Err(error) => {
        tracing::debug!(%error, "concealing a voice packet the decoder refused");
        let lost = opus_packet_samples(packet).unwrap_or(self.last_packet_samples);
        self.conceal(None, lost)
      },
    }
  }

  /// `lost_samples` for a lost packet of that length: recovered from the in-band FEC data of
  /// `maybe_next` where that packet is here and as long (a step changed the length in between
  /// otherwise, #1893), concealed otherwise. Never fails: concealment is the floor.
  pub fn conceal(&mut self, maybe_next: Option<&[u8]>, lost_samples: usize) -> Vec<f32> {
    let lost_samples = lost_samples.clamp(1, MAX_PACKET_SAMPLES_PER_CHANNEL);
    let maybe_fec = maybe_next.filter(|next| opus_packet_samples(next) == Some(lost_samples));
    let recovered = match maybe_fec {
      Some(next) => self.decode_into(next, DecodeMode::Fec, lost_samples),
      None => self.decode_into(&[], DecodeMode::Normal, lost_samples),
    };
    recovered.unwrap_or_else(|error| {
      // Concealment itself has nothing to refuse; should it fail anyway, silence keeps the length.
      tracing::debug!(%error, "voice concealment failed");
      vec![0.0; lost_samples]
    })
  }

  /// The length of the last packet decoded or concealed, per channel.
  pub fn last_packet_samples(&self) -> usize {
    self.last_packet_samples
  }

  fn decode_packet(&mut self, packet: &[u8]) -> Result<Vec<f32>, VoiceFecError> {
    let samples = opus_packet_samples(packet).unwrap_or(MAX_PACKET_SAMPLES_PER_CHANNEL);
    self.decode_into(packet, DecodeMode::Normal, samples)
  }

  fn decode_into(
    &mut self,
    packet: &[u8],
    mode: DecodeMode,
    samples: usize,
  ) -> Result<Vec<f32>, VoiceFecError> {
    let mut pcm = vec![0.0f32; samples.min(MAX_PACKET_SAMPLES_PER_CHANNEL)];
    let decoded = self
      .decoder
      .decode_float(packet, &mut pcm, mode)
      .map_err(|source| VoiceFecError::Decode {
        bytes: packet.len(),
        source,
      })?;
    pcm.truncate(decoded);
    if decoded > 0 {
      self.last_packet_samples = decoded;
    }
    Ok(pcm)
  }
}

/// The messages between the browser's call module and its voice worker (#2140).
///
/// Messages are plain objects with a [`KIND`]; packets and samples travel as transferred
/// `ArrayBuffer`s. Each remote voice is a [`STREAM`] number the page picks; the worker keeps one
/// [`FecVoiceDecoder`] per number until the page closes it.
///
/// | From   | `kind`        | Fields                                          | Answer                   |
/// |--------|---------------|-------------------------------------------------|--------------------------|
/// | worker | [`READY`]     | –                                               | –                        |
/// | page   | [`PRIME`]     | [`STREAM`], [`PACKET`]                          | – (warms the decoder)    |
/// | page   | [`DECODE`]    | [`STREAM`], [`PACKET`]                          | [`DECODED`]              |
/// | page   | [`CONCEAL`]   | [`STREAM`], [`SAMPLES`], [`NEXT`] (optional)    | [`DECODED`]              |
/// | page   | [`CLOSE`]     | [`STREAM`]                                      | –                        |
/// | worker | [`DECODED`]   | [`STREAM`], [`PCM`] (f32), [`RECOVERED`]        | –                        |
/// | worker | [`FAILED`]    | [`DETAIL`]                                      | –                        |
///
/// The worker answers in order. Every [`DECODE`] and [`CONCEAL`] gets a [`DECODED`] as long as the
/// packet (a refused packet is concealed), so the page can count the samples on their way.
pub mod protocol {
  /// The message's type.
  pub const KIND: &str = "kind";
  /// The worker's module is initialized and takes messages.
  pub const READY: &str = "ready";
  /// Decode [`PACKET`] and drop the samples: brings a fresh decoder to the state of the one it
  /// takes over from, so the switch is not heard.
  pub const PRIME: &str = "prime";
  /// Decode [`PACKET`].
  pub const DECODE: &str = "decode";
  /// A packet of [`SAMPLES`] went missing: recover it from [`NEXT`]'s in-band FEC, or conceal it.
  pub const CONCEAL: &str = "conceal";
  /// The stream ended: drop its decoder.
  pub const CLOSE: &str = "close";
  /// The samples of one packet.
  pub const DECODED: &str = "decoded";
  /// The worker's module couldn't start or a message made no sense, with a [`DETAIL`].
  pub const FAILED: &str = "failed";

  /// The page's number for one remote voice.
  pub const STREAM: &str = "stream";
  /// One Opus packet's bytes.
  pub const PACKET: &str = "packet";
  /// The packet after a lost one, whose FEC data may carry it.
  pub const NEXT: &str = "next";
  /// How many samples (per channel) a lost packet had.
  pub const SAMPLES: &str = "samples";
  /// Decoded samples, mono `f32` at 48 kHz.
  pub const PCM: &str = "pcm";
  /// The answer to a [`CONCEAL`] came from FEC data, not concealment.
  pub const RECOVERED: &str = "recovered";
  pub const DETAIL: &str = "detail";

  /// Where the page serves the voice worker's script: the `content` of its `<meta>`, only a script
  /// of the page's own origin (its Content-Security-Policy allows no other). `None` for none.
  pub fn worker_url(maybe_meta: Option<&str>) -> Option<String> {
    maybe_meta
      .map(str::trim)
      .filter(|path| path.starts_with('/') && !path.starts_with("//") && path.ends_with(".js"))
      .map(str::to_owned)
  }

  #[cfg(test)]
  mod tests {
    use super::*;

    #[test]
    fn the_worker_is_a_script_of_the_page() {
      assert_eq!(
        worker_url(Some("/teamcraft-voice-worker-0123.js")).as_deref(),
        Some("/teamcraft-voice-worker-0123.js")
      );
      for refused in [
        None,
        Some(""),
        Some("https://elsewhere.example.com/worker.js"),
        Some("//elsewhere.example.com/worker.js"),
        Some("/teamcraft-web-voice-0123_bg.wasm"),
      ] {
        assert_eq!(worker_url(refused), None, "{refused:?}");
      }
    }
  }
}

// Two attributes, not `all(…)`: clippy takes only a plain `cfg(test)` for test code.
#[cfg(feature = "voice-fec")]
#[cfg(test)]
mod tests {
  use ropus::{Application, Bitrate, Encoder, InbandFec, Signal};

  use super::*;

  /// One packet in this many is lost: single losses, which is what FEC recovers.
  const LOSS_EVERY: usize = 7;
  const SECONDS: usize = 4;

  #[test]
  fn fec_recovers_lost_voice_better_than_concealment() {
    let packets = encode(&voice_like(SECONDS * SAMPLE_RATE_HZ as usize));
    let reference = decode_all(&packets, |_| false, false);
    let concealed = decode_all(&packets, is_lost, false);
    let recovered = decode_all(&packets, is_lost, true);
    let concealed_snr = lost_frames_snr_db(&reference, &concealed);
    let recovered_snr = lost_frames_snr_db(&reference, &recovered);
    // The PR's measurement (#2140): printed with `--nocapture`.
    eprintln!(
      "lost frames vs. no loss: concealment {concealed_snr:.1} dB SNR, FEC {recovered_snr:.1} dB SNR"
    );
    assert!(
      recovered_snr > concealed_snr + 6.0,
      "FEC {recovered_snr:.1} dB is no better than concealment {concealed_snr:.1} dB"
    );
  }

  #[test]
  fn every_packet_comes_back_as_long_as_it_was() {
    let packets = encode(&voice_like(SAMPLE_RATE_HZ as usize / 2));
    let mut decoder = FecVoiceDecoder::new().unwrap();
    for packet in &packets {
      assert_eq!(decoder.decode(packet).len(), FRAME_SAMPLES_PER_CHANNEL);
    }
    // Refused bytes are concealed for the last packet's length.
    assert_eq!(decoder.decode(&[0xff; 3]).len(), FRAME_SAMPLES_PER_CHANNEL);
    assert_eq!(decoder.decode(&[]).len(), FRAME_SAMPLES_PER_CHANNEL);
    // A next packet of another length carries no FEC for this one: concealed for the asked length.
    assert_eq!(
      decoder
        .conceal(Some(&packets[0]), 2 * FRAME_SAMPLES_PER_CHANNEL)
        .len(),
      1920
    );
    assert_eq!(decoder.last_packet_samples(), 1920);
    // Nonsense lengths stay within one packet.
    assert_eq!(decoder.conceal(None, 0).len(), 1);
    assert_eq!(
      decoder.conceal(None, usize::MAX).len(),
      MAX_PACKET_SAMPLES_PER_CHANNEL
    );
  }

  fn is_lost(index: usize) -> bool {
    index % LOSS_EVERY == LOSS_EVERY - 1
  }

  /// Decodes `packets` with those `is_lost` says went missing: from the next packet's FEC data
  /// with `uses_fec`, concealed otherwise.
  fn decode_all(packets: &[Vec<u8>], is_lost: impl Fn(usize) -> bool, uses_fec: bool) -> Vec<f32> {
    let mut decoder = FecVoiceDecoder::new().unwrap();
    let mut pcm = Vec::new();
    for (index, packet) in packets.iter().enumerate() {
      if is_lost(index) {
        let maybe_next = packets.get(index + 1).filter(|_| uses_fec).map(Vec::as_slice);
        pcm.extend(decoder.conceal(maybe_next, FRAME_SAMPLES_PER_CHANNEL));
      } else {
        pcm.extend(decoder.decode(packet));
      }
    }
    pcm
  }

  /// Signal-to-noise ratio of `decoded` against `reference` over the lost packets' samples.
  fn lost_frames_snr_db(reference: &[f32], decoded: &[f32]) -> f64 {
    let (mut signal, mut noise) = (0.0f64, 0.0f64);
    for (index, (expected, actual)) in reference.iter().zip(decoded).enumerate() {
      if is_lost(index / FRAME_SAMPLES_PER_CHANNEL) {
        signal += f64::from(*expected).powi(2);
        noise += f64::from(expected - actual).powi(2);
      }
    }
    10.0 * (signal / noise.max(f64::MIN_POSITIVE)).log10()
  }

  /// 20 ms voice packets as a voice call sends them: VoIP, 24 kbit/s, FEC planned for loss.
  fn encode(pcm: &[i16]) -> Vec<Vec<u8>> {
    let mut encoder = Encoder::builder(SAMPLE_RATE_HZ, Channels::Mono, Application::Voip)
      .bitrate(Bitrate::Bits(24_000))
      .signal(Signal::Voice)
      .inband_fec(InbandFec::Enabled)
      .packet_loss_perc(15)
      .build()
      .unwrap();
    let mut packet = vec![0u8; 1500];
    pcm
      .as_chunks::<FRAME_SAMPLES_PER_CHANNEL>()
      .0
      .iter()
      .map(|frame| {
        let length = encoder.encode(frame, &mut packet).unwrap();
        packet[..length].to_vec()
      })
      .collect()
  }

  /// A voiced, speech-like signal: a gliding pitch with harmonics, syllables of about 200 ms and a
  /// little noise. Deterministic.
  fn voice_like(samples: usize) -> Vec<i16> {
    let rate = f64::from(SAMPLE_RATE_HZ);
    let mut phase = 0.0f64;
    let mut noise_state = 0x2140_u32;
    (0..samples)
      .map(|index| {
        let time = index as f64 / rate;
        let pitch_hz = 150.0 + 40.0 * (2.0 * std::f64::consts::PI * 0.7 * time).sin();
        phase += 2.0 * std::f64::consts::PI * pitch_hz / rate;
        let voiced: f64 = (1..=12)
          .map(|harmonic| (f64::from(harmonic) * phase).sin() / f64::from(harmonic))
          .sum();
        let syllable = (2.0 * std::f64::consts::PI * 2.5 * time).sin().abs();
        noise_state = noise_state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let noise = f64::from(noise_state >> 8) / f64::from(1u32 << 24) - 0.5;
        let sample = 0.25 * syllable * voiced + 0.01 * noise;
        (sample.clamp(-1.0, 1.0) * f64::from(i16::MAX)) as i16
      })
      .collect()
  }
}
