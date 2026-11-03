//! A user's own message sound on the desktop (notifications.md, "Message sounds"; #800): the picked
//! file decoded once into what the app plays (mono, 48 kHz) and cut to the length limit. Symphonia
//! reads MP3, Ogg Vorbis, WAV and FLAC in pure Rust.
//!
//! Decoding stops at the limit, so a long file (a small file can hold minutes of low-rate audio)
//! costs no more than a short one; a file that breaks off keeps what decoded before the break.

use std::io::Cursor;
use std::time::Duration;

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as DecodeError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use thiserror::Error;

use crate::audio_frame::SAMPLE_RATE_HZ;
use crate::message_sounds::cut_to;
use crate::system_audio::MonoResampler;

/// Broken packets in a row after which the rest of the file is given up.
const MAX_BROKEN_PACKETS: usize = 16;
/// Samples at or below this count as silence.
const SILENCE: f32 = 1e-4;

#[derive(Debug, Error)]
pub enum SoundFileError {
  #[error("not a sound this app can play: {0}")]
  NotAudio(String),
  #[error("the file has no sound in it")]
  Silent,
}

/// Decodes a whole sound file (`maybe_extension` helps to recognize it) into mono 48 kHz samples,
/// at most `max` long: a longer sound is cut and faded out.
pub fn decode(
  bytes: Vec<u8>,
  maybe_extension: Option<&str>,
  max: Duration,
) -> Result<Vec<f32>, SoundFileError> {
  let source = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
  let mut hint = Hint::new();
  if let Some(extension) = maybe_extension {
    hint.with_extension(extension);
  }
  let mut format = symphonia::default::get_probe()
    .probe(
      &hint,
      source,
      FormatOptions::default(),
      MetadataOptions::default(),
    )
    .map_err(not_audio)?;
  let track = format
    .default_track(TrackType::Audio)
    .ok_or_else(|| SoundFileError::NotAudio("no audio track".to_owned()))?;
  let track_id = track.id;
  let params = track
    .codec_params
    .as_ref()
    .and_then(|params| params.audio())
    .ok_or_else(|| SoundFileError::NotAudio("no audio parameters".to_owned()))?;
  let mut decoder = symphonia::default::get_codecs()
    .make_audio_decoder(params, &AudioDecoderOptions::default())
    .map_err(not_audio)?;
  let max_samples = (max.as_secs_f32() * SAMPLE_RATE_HZ as f32).ceil() as usize;
  let mut samples: Vec<f32> = Vec::new();
  let mut interleaved: Vec<f32> = Vec::new();
  let mut maybe_resampler: Option<MonoResampler> = None;
  let mut broken_in_row = 0;
  while samples.len() < max_samples {
    let packet = match format.next_packet() {
      Ok(Some(packet)) => packet,
      Ok(None) => break,
      Err(error) if samples.is_empty() => return Err(not_audio(error)),
      // A file that breaks off plays what decoded before the break.
      Err(_) => break,
    };
    if packet.track_id != track_id {
      continue;
    }
    match decoder.decode(&packet) {
      Ok(buffer) => {
        broken_in_row = 0;
        let spec = buffer.spec();
        let resampler =
          maybe_resampler.get_or_insert_with(|| MonoResampler::new(spec.channels().count(), spec.rate()));
        buffer.copy_to_vec_interleaved(&mut interleaved);
        samples.extend(resampler.process(&interleaved));
      },
      // One broken packet is skipped, as players do; many mean the rest is garbage.
      Err(DecodeError::DecodeError(_)) if broken_in_row < MAX_BROKEN_PACKETS => broken_in_row += 1,
      Err(error) if samples.is_empty() => return Err(not_audio(error)),
      Err(_) => break,
    }
  }
  if samples.iter().all(|sample| sample.abs() <= SILENCE) {
    return Err(SoundFileError::Silent);
  }
  cut_to(&mut samples, max);
  Ok(samples)
}

fn not_audio(error: DecodeError) -> SoundFileError {
  SoundFileError::NotAudio(error.to_string())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_wav_file_becomes_mono_48_khz() {
    let samples = decode(wav(44_100, 2, 1.0, 0.5), Some("wav"), Duration::from_secs(5)).unwrap();
    let expected = SAMPLE_RATE_HZ as usize;
    assert!(samples.len().abs_diff(expected) <= 2, "{} samples", samples.len());
    let peak = samples.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    assert!((peak - 0.5).abs() < 0.02, "peaks at {peak}");
  }

  #[test]
  fn a_long_file_is_cut_at_the_limit_with_a_fade() {
    let samples = decode(wav(48_000, 1, 3.0, 0.5), None, Duration::from_secs(1)).unwrap();
    assert_eq!(samples.len(), SAMPLE_RATE_HZ as usize);
    assert!(samples[samples.len() - 1].abs() < 1e-6);
  }

  #[test]
  fn a_file_that_breaks_off_plays_what_is_there() {
    let mut bytes = wav(48_000, 1, 1.0, 0.5);
    bytes.truncate(bytes.len() / 2);
    let samples = decode(bytes, Some("wav"), Duration::from_secs(5)).unwrap();
    assert!(
      samples.len() > SAMPLE_RATE_HZ as usize / 3,
      "{} samples",
      samples.len()
    );
  }

  #[test]
  fn garbage_empty_and_silent_files_are_refused() {
    let garbage: Vec<u8> = (0..4096u32).map(|index| (index * 7919 % 251) as u8).collect();
    assert!(matches!(
      decode(garbage, Some("mp3"), Duration::from_secs(5)),
      Err(SoundFileError::NotAudio(_))
    ));
    assert!(matches!(
      decode(Vec::new(), None, Duration::from_secs(5)),
      Err(SoundFileError::NotAudio(_))
    ));
    assert!(matches!(
      decode(wav(48_000, 1, 0.5, 0.0), Some("wav"), Duration::from_secs(5)),
      Err(SoundFileError::Silent)
    ));
  }

  /// A 16-bit PCM WAV file with a 440 Hz sine of `level` on every channel.
  fn wav(rate_hz: u32, channels: u16, seconds: f32, level: f32) -> Vec<u8> {
    let frames = (rate_hz as f32 * seconds) as u32;
    let data_bytes = frames * u32::from(channels) * 2;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&rate_hz.to_le_bytes());
    bytes.extend_from_slice(&(rate_hz * u32::from(channels) * 2).to_le_bytes());
    bytes.extend_from_slice(&(channels * 2).to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_bytes.to_le_bytes());
    for frame in 0..frames {
      let time = frame as f32 / rate_hz as f32;
      let value = (level * (std::f32::consts::TAU * 440.0 * time).sin() * f32::from(i16::MAX)) as i16;
      for _ in 0..channels {
        bytes.extend_from_slice(&value.to_le_bytes());
      }
    }
    bytes
  }
}
