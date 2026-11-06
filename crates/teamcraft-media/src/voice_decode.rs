//! Opus voice messages (#204) as plain PCM, for players that don't read Ogg or WebM: AVFoundation
//! on iOS and the macOS webview. A voice message is Opus in Ogg (the native apps, Firefox) or in
//! WebM (Chrome, Edge); symphonia reads both containers in pure Rust and libopus (as in calls)
//! decodes.
//!
//! The output is a 16-bit mono WAV file at 24 kHz, which is all a voice needs: about 2.9 MB a
//! minute. Input and output are streamed, so a long message costs no memory; decoding stops at the
//! limit given and at the length the input's size can plausibly hold ([`plausible_length`]), so a
//! small hostile file can't fill the disk or memory. The file is the caller's to keep in private
//! storage and delete.

use std::fs::File;
use std::io::{BufWriter, Cursor, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Duration;

use opus::{Channels, Decoder};
use symphonia::core::codecs::audio::well_known::CODEC_ID_OPUS;
use symphonia::core::errors::Error as FormatError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use thiserror::Error;

/// The rate of the decoded file.
pub const DECODED_RATE_HZ: u32 = 24_000;
/// The most samples one Opus packet holds at that rate (120 ms).
const MAX_PACKET_SAMPLES: usize = 2_880;
/// Broken packets in a row after which the rest of the file is given up.
const MAX_BROKEN_PACKETS: usize = 16;
/// The fewest bytes a second of Opus takes in a container: a silent 20 ms packet is a few bytes,
/// so 100 bytes a second is below anything real. Bounds what a small file can decode to.
const MIN_BYTES_PER_SECOND: u64 = 100;

#[derive(Debug, Error)]
pub enum VoiceDecodeError {
  /// Not Ogg or WebM, or no Opus track in it.
  #[error("not an Opus voice message: {0}")]
  NotOpus(String),
  #[error("no sound decoded")]
  Empty,
  #[error("decoder: {0}")]
  Codec(#[from] opus::Error),
  #[error("file: {0}")]
  File(#[from] std::io::Error),
}

/// Decodes the Opus file at `input` into a WAV file at `output`, at most `max` long (and at most
/// what its size can hold). Returns the decoded length.
pub fn decode_to_wav(input: &Path, output: &Path, max: Duration) -> Result<Duration, VoiceDecodeError> {
  let file = File::open(input)?;
  let max = max.min(plausible_length(file.metadata()?.len()));
  into_file(output, |wav| decode_source(Box::new(file), wav, max))
}

/// Like [`decode_to_wav`] from bytes in memory: the decrypted file needn't touch the disk.
pub fn decode_bytes_to_wav(
  bytes: Vec<u8>,
  output: &Path,
  max: Duration,
) -> Result<Duration, VoiceDecodeError> {
  into_file(output, |wav| decode_bytes(bytes, wav, max))
}

/// Decodes Opus `bytes` as WAV into `output` (a file, or memory for tests and fuzzing), with the
/// same limits as [`decode_to_wav`].
pub fn decode_bytes<W: Write + Seek>(
  bytes: Vec<u8>,
  output: W,
  max: Duration,
) -> Result<Duration, VoiceDecodeError> {
  let max = max.min(plausible_length(bytes.len() as u64));
  decode_source(Box::new(Cursor::new(bytes)), output, max)
}

/// The longest a voice message of `size_bytes` can plausibly play ([`MIN_BYTES_PER_SECOND`]).
pub fn plausible_length(size_bytes: u64) -> Duration {
  Duration::from_secs(size_bytes / MIN_BYTES_PER_SECOND)
}

/// Runs `decode` into a new file at `output`; nothing half-written is left behind on failure.
fn into_file(
  output: &Path,
  decode: impl FnOnce(BufWriter<File>) -> Result<Duration, VoiceDecodeError>,
) -> Result<Duration, VoiceDecodeError> {
  let decoded = decode(BufWriter::new(File::create(output)?));
  if decoded.is_err() {
    drop(std::fs::remove_file(output));
  }
  decoded
}

fn decode_source<W: Write + Seek>(
  input: Box<dyn MediaSource>,
  output: W,
  max: Duration,
) -> Result<Duration, VoiceDecodeError> {
  let source = MediaSourceStream::new(input, Default::default());
  let mut format = symphonia::default::get_probe()
    .probe(
      &Hint::new(),
      source,
      FormatOptions::default(),
      MetadataOptions::default(),
    )
    .map_err(not_opus)?;
  let track = format
    .default_track(TrackType::Audio)
    .ok_or_else(|| VoiceDecodeError::NotOpus("no audio track".to_owned()))?;
  let is_opus = track
    .codec_params
    .as_ref()
    .and_then(|params| params.audio())
    .is_some_and(|params| params.codec == CODEC_ID_OPUS);
  if !is_opus {
    return Err(VoiceDecodeError::NotOpus("not Opus".to_owned()));
  }
  let track_id = track.id;
  // libopus mixes a stereo stream down to the one channel asked for.
  let mut decoder = Decoder::new(DECODED_RATE_HZ, Channels::Mono)?;
  let max_samples = (max.as_secs_f64() * f64::from(DECODED_RATE_HZ)) as u64;
  let mut wav = WavWriter::new(output)?;
  decode_packets(&mut *format, track_id, &mut decoder, &mut wav, max_samples)?;
  if wav.samples == 0 {
    return Err(VoiceDecodeError::Empty);
  }
  let samples = wav.finish()?;
  Ok(Duration::from_secs_f64(
    samples as f64 / f64::from(DECODED_RATE_HZ),
  ))
}

/// Decodes the track's packets into `wav` until the end, the limit, or too many broken packets.
/// A message that breaks off keeps what decoded before the break.
fn decode_packets(
  format: &mut dyn FormatReader,
  track_id: u32,
  decoder: &mut Decoder,
  wav: &mut WavWriter<impl Write + Seek>,
  max_samples: u64,
) -> Result<(), VoiceDecodeError> {
  let mut pcm = vec![0i16; MAX_PACKET_SAMPLES];
  let mut broken_in_row = 0;
  while wav.samples < max_samples {
    let packet = match format.next_packet() {
      Ok(Some(packet)) => packet,
      Ok(None) => break,
      Err(error) if wav.samples == 0 => return Err(not_opus(error)),
      Err(_) => break,
    };
    if packet.track_id != track_id {
      continue;
    }
    match decoder.decode(&packet.data, &mut pcm, false) {
      Ok(count) => {
        broken_in_row = 0;
        let room = (max_samples - wav.samples).min(count as u64) as usize;
        wav.write(&pcm[..room])?;
      },
      Err(_) if broken_in_row < MAX_BROKEN_PACKETS => broken_in_row += 1,
      Err(error) if wav.samples == 0 => return Err(error.into()),
      Err(_) => break,
    }
  }
  Ok(())
}

/// A mono 16-bit WAV file written as it goes; the sizes in its header are filled in at the end.
struct WavWriter<W: Write + Seek> {
  file: W,
  samples: u64,
}

impl<W: Write + Seek> WavWriter<W> {
  fn new(mut file: W) -> Result<Self, std::io::Error> {
    file.write_all(&wav_header(0))?;
    Ok(Self { file, samples: 0 })
  }

  fn write(&mut self, samples: &[i16]) -> Result<(), std::io::Error> {
    for sample in samples {
      self.file.write_all(&sample.to_le_bytes())?;
    }
    self.samples += samples.len() as u64;
    Ok(())
  }

  fn finish(mut self) -> Result<u64, std::io::Error> {
    let data_bytes = u32::try_from(self.samples * 2).unwrap_or(u32::MAX);
    self.file.seek(SeekFrom::Start(0))?;
    self.file.write_all(&wav_header(data_bytes))?;
    self.file.flush()?;
    Ok(self.samples)
  }
}

fn wav_header(data_bytes: u32) -> [u8; 44] {
  let mut header = [0u8; 44];
  header[0..4].copy_from_slice(b"RIFF");
  header[4..8].copy_from_slice(&data_bytes.saturating_add(36).to_le_bytes());
  header[8..16].copy_from_slice(b"WAVEfmt ");
  header[16..20].copy_from_slice(&16u32.to_le_bytes());
  header[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
  header[22..24].copy_from_slice(&1u16.to_le_bytes()); // mono
  header[24..28].copy_from_slice(&DECODED_RATE_HZ.to_le_bytes());
  header[28..32].copy_from_slice(&(DECODED_RATE_HZ * 2).to_le_bytes());
  header[32..34].copy_from_slice(&2u16.to_le_bytes());
  header[34..36].copy_from_slice(&16u16.to_le_bytes());
  header[36..40].copy_from_slice(b"data");
  header[40..44].copy_from_slice(&data_bytes.to_le_bytes());
  header
}

fn not_opus(error: FormatError) -> VoiceDecodeError {
  VoiceDecodeError::NotOpus(error.to_string())
}

#[cfg(test)]
mod tests {
  use std::path::PathBuf;

  use super::*;
  use crate::audio_frame::FRAME_SAMPLES_PER_CHANNEL;
  use crate::ogg_opus::OggOpusWriter;

  #[test]
  fn an_ogg_opus_file_becomes_wav_of_the_same_length() {
    let folder = Folder::new("ogg");
    let input = folder.file("in.ogg");
    std::fs::write(&input, ogg_opus(50)).unwrap();
    let output = folder.file("out.wav");
    let length = decode_to_wav(&input, &output, Duration::from_secs(60)).unwrap();
    assert!((length.as_secs_f64() - 1.0).abs() < 0.05, "{length:?}");
    let wav = std::fs::read(&output).unwrap();
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(
      u32::from_le_bytes(wav[40..44].try_into().unwrap()) as usize,
      wav.len() - 44
    );
  }

  #[test]
  fn decoding_stops_at_the_limit() {
    let folder = Folder::new("limit");
    let input = folder.file("in.ogg");
    std::fs::write(&input, ogg_opus(250)).unwrap();
    let output = folder.file("out.wav");
    let length = decode_to_wav(&input, &output, Duration::from_secs(2)).unwrap();
    assert_eq!(length, Duration::from_secs(2));
  }

  #[test]
  fn garbage_empty_and_truncated_files_are_refused_or_cut() {
    let folder = Folder::new("garbage");
    let output = folder.file("out.wav");
    let garbage: Vec<u8> = (0..4096u32).map(|index| (index * 7919 % 251) as u8).collect();
    for bytes in [garbage, Vec::new(), ogg_opus(10)[..60].to_vec()] {
      let input = folder.file("in.ogg");
      std::fs::write(&input, bytes).unwrap();
      assert!(decode_to_wav(&input, &output, Duration::from_secs(5)).is_err());
      assert!(!output.exists(), "no half file left behind");
    }
    let mut truncated = ogg_opus(100);
    truncated.truncate(truncated.len() / 2);
    let input = folder.file("half.ogg");
    std::fs::write(&input, truncated).unwrap();
    // The Ogg reader looks for the last page first, so a cut file may be refused whole; either
    // way nothing half-written is left (attachments are hash-checked before this, so a cut file
    // only comes from a broken sender).
    match decode_to_wav(&input, &output, Duration::from_secs(5)) {
      Ok(length) => assert!(length > Duration::ZERO, "{length:?}"),
      Err(_) => assert!(!output.exists()),
    }
  }

  #[test]
  fn a_small_file_decodes_no_longer_than_its_size_allows() {
    assert_eq!(plausible_length(1_000), Duration::from_secs(10));
    let folder = Folder::new("bytes");
    let output = folder.file("out.wav");
    let bytes = ogg_opus(500);
    let length = decode_bytes_to_wav(bytes.clone(), &output, Duration::from_secs(60)).unwrap();
    assert!(length <= plausible_length(bytes.len() as u64));
    assert!(length > Duration::from_secs(9), "{length:?}");
  }

  #[test]
  fn decodes_into_memory() {
    let mut wav = Cursor::new(Vec::new());
    let length = decode_bytes(ogg_opus(50), &mut wav, Duration::from_secs(60)).unwrap();
    assert!((length.as_secs_f64() - 1.0).abs() < 0.05, "{length:?}");
    assert_eq!(&wav.get_ref()[..4], b"RIFF");
  }

  /// `frames` 20 ms frames of a 440 Hz tone, encoded and written as Ogg Opus.
  fn ogg_opus(frames: usize) -> Vec<u8> {
    let mut encoder = opus::Encoder::new(48_000, Channels::Mono, opus::Application::Voip).unwrap();
    let pre_skip = u16::try_from(encoder.get_lookahead().unwrap()).unwrap();
    let mut writer = OggOpusWriter::new(Vec::new(), pre_skip).unwrap();
    for frame in 0..frames {
      let pcm: Vec<i16> = (0..FRAME_SAMPLES_PER_CHANNEL)
        .map(|index| {
          let time = (frame * FRAME_SAMPLES_PER_CHANNEL + index) as f32 / 48_000.0;
          (0.3 * (std::f32::consts::TAU * 440.0 * time).sin() * f32::from(i16::MAX)) as i16
        })
        .collect();
      writer
        .push(
          encoder.encode_vec(&pcm, 1275).unwrap(),
          FRAME_SAMPLES_PER_CHANNEL as u32,
        )
        .unwrap();
    }
    writer.finish().unwrap()
  }

  /// A folder of its own per test, removed afterwards.
  struct Folder(PathBuf);

  impl Folder {
    fn new(name: &str) -> Self {
      let path = std::env::temp_dir().join(format!("teamcraft-voice-decode-{name}-{}", std::process::id()));
      std::fs::create_dir_all(&path).unwrap();
      Self(path)
    }

    fn file(&self, name: &str) -> PathBuf {
      self.0.join(name)
    }
  }

  impl Drop for Folder {
    fn drop(&mut self) {
      drop(std::fs::remove_dir_all(&self.0));
    }
  }
}
