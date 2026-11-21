//! The parts of a call recording (issue #216, e2ee.md §8.3) that the desktop recorder
//! (`call_recording`, a thread with libopus) and the browser's (`call_web::recording`, WebCodecs
//! and the origin private file system, issue #2114) share: the mix of what the device plays with
//! its own voice, the length limit, and the bounded buffers between the encoder and the file.
//!
//! Everything here is plain Rust without I/O, so it is tested on the host and behaves the same on
//! every platform.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::rc::Rc;
use std::time::Duration;

use crate::audio_frame::FRAME_SAMPLES_PER_CHANNEL;

/// The longest recording a recorder makes, whatever it is asked for (the policy's ceiling,
/// `teamcraft_proto::MAX_RECORDING_MINUTES`).
pub const MAX_RECORDING_LENGTH: Duration = Duration::from_secs(8 * 60 * 60);
/// Speech of several people at this rate stays clear in a mix; about 14 MB an hour.
pub const RECORDING_BITRATE_BPS: u32 = 32_000;
/// The own voice waiting for the next played frame, at most (200 ms): more means the two clocks
/// drifted, and the oldest goes.
pub const MAX_OWN_SAMPLES: usize = 10 * FRAME_SAMPLES_PER_CHANNEL;
/// libopus' lookahead at 48 kHz (2.5 ms plus the 4 ms of its delay compensation): the pre-skip of
/// a file whose encoder doesn't say its own.
pub const DEFAULT_PRE_SKIP: u16 = 312;

/// The mix of the played frames and the own voice, frame by frame. The played frames are the
/// clock: one frame out per frame played; the own voice fills in as far as it is there.
#[derive(Debug, Default)]
pub struct RecordingMixer {
  own: VecDeque<f32>,
}

/// Bytes on their way from the encoder to the file: the encoder side writes ([`Write`]), the file
/// side takes them in pieces. Clones share the buffer (one thread: the browser's page).
#[derive(Clone, Debug, Default)]
pub struct RecordingSpool(Rc<RefCell<SpoolState>>);

#[derive(Debug, Default)]
struct SpoolState {
  pending: Vec<u8>,
  written: u64,
}

/// A [`Read`] over bytes fetched ahead from somewhere asynchronous (a browser file read in
/// slices), for a reader that only reads synchronously (`SealedStreamReader`). The caller tops it
/// up with [`Self::push`] before every read so that a whole chunk is there, and marks the end with
/// [`Self::end`]; until then running dry is an error, never an early end of the file.
#[derive(Clone, Debug, Default)]
pub struct PrefetchedBytes(Rc<RefCell<Prefetched>>);

#[derive(Debug, Default)]
struct Prefetched {
  bytes: VecDeque<u8>,
  is_at_end: bool,
}

impl RecordingMixer {
  pub fn new() -> Self {
    Self {
      own: VecDeque::with_capacity(MAX_OWN_SAMPLES),
    }
  }

  /// The own voice as decoded 16-bit samples (the desktop decodes the Opus frames it sent).
  pub fn push_own_i16(&mut self, samples: &[i16]) {
    self.own.extend(
      samples
        .iter()
        .map(|sample| f32::from(*sample) / f32::from(i16::MAX)),
    );
    self.trim();
  }

  /// The own voice as the samples handed to the encoder (the browser, which has them already).
  pub fn push_own(&mut self, samples: &[f32]) {
    self.own.extend(samples.iter().copied());
    self.trim();
  }

  /// The played frame with as much of the own voice as is there; loud parts clip.
  pub fn mix(&mut self, playback: &[f32]) -> Vec<f32> {
    playback
      .iter()
      .map(|sample| {
        let own = self.own.pop_front().unwrap_or(0.0);
        (sample + own).clamp(-1.0, 1.0)
      })
      .collect()
  }

  /// [`Self::mix`] as 16-bit samples.
  pub fn mix_i16(&mut self, playback: &[f32]) -> Vec<i16> {
    self.mix(playback).into_iter().map(to_i16).collect()
  }

  /// Own samples waiting.
  pub fn own_len(&self) -> usize {
    self.own.len()
  }

  fn trim(&mut self) {
    let excess = self.own.len().saturating_sub(MAX_OWN_SAMPLES);
    self.own.drain(..excess);
  }
}

/// The 20 ms frames a recording of at most `max` holds, within [`MAX_RECORDING_LENGTH`].
pub fn max_frames(max: Duration) -> u64 {
  (max.min(MAX_RECORDING_LENGTH).as_millis() / 20) as u64
}

/// The pre-skip an encoder announces in its decoder description: an `OpusHead` (RFC 7845 §5.1),
/// as WebCodecs hands it with the first output. `None` for anything else.
pub fn pre_skip_of(description: &[u8]) -> Option<u16> {
  if description.len() < 19 || &description[..8] != b"OpusHead" {
    return None;
  }
  Some(u16::from_le_bytes([description[10], description[11]]))
}

impl RecordingSpool {
  pub fn new() -> Self {
    Self::default()
  }

  /// Bytes written and not taken yet.
  pub fn pending_len(&self) -> usize {
    self.0.borrow().pending.len()
  }

  /// Everything written so far, the taken bytes included.
  pub fn written(&self) -> u64 {
    self.0.borrow().written
  }

  /// Takes what is pending once it reaches `at_least` bytes (0: whatever is there).
  pub fn take(&self, at_least: usize) -> Option<Vec<u8>> {
    let mut state = self.0.borrow_mut();
    if state.pending.is_empty() || state.pending.len() < at_least {
      return None;
    }
    Some(std::mem::take(&mut state.pending))
  }
}

impl Write for RecordingSpool {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    let mut state = self.0.borrow_mut();
    state.pending.extend_from_slice(buf);
    state.written += buf.len() as u64;
    Ok(buf.len())
  }

  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}

impl PrefetchedBytes {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn push(&self, bytes: &[u8]) {
    self.0.borrow_mut().bytes.extend(bytes.iter().copied());
  }

  /// Nothing more follows: reading what is left and then the end of the file.
  pub fn end(&self) {
    self.0.borrow_mut().is_at_end = true;
  }

  /// Bytes waiting to be read.
  pub fn len(&self) -> usize {
    self.0.borrow().bytes.len()
  }

  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }

  pub fn is_at_end(&self) -> bool {
    self.0.borrow().is_at_end
  }
}

impl Read for PrefetchedBytes {
  fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
    let mut state = self.0.borrow_mut();
    if state.bytes.is_empty() {
      if state.is_at_end || buf.is_empty() {
        return Ok(0);
      }
      // Never an end of file that isn't one: the reader would take a cut-off file for whole.
      return Err(io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "read before enough bytes were fetched",
      ));
    }
    let count = buf.len().min(state.bytes.len());
    for (slot, byte) in buf.iter_mut().zip(state.bytes.drain(..count)) {
      *slot = byte;
    }
    Ok(count)
  }
}

fn to_i16(sample: f32) -> i16 {
  // Invariant: the clamp keeps the product within i16.
  (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16
}

#[cfg(test)]
mod tests {
  use super::*;

  mod mixer {
    use super::*;

    #[test]
    fn the_own_voice_never_queues_more_than_its_bound() {
      let mut mixer = RecordingMixer::new();
      mixer.push_own_i16(&vec![1000; MAX_OWN_SAMPLES * 3]);
      assert_eq!(mixer.own_len(), MAX_OWN_SAMPLES);
      mixer.push_own(&vec![0.1; MAX_OWN_SAMPLES * 2]);
      assert_eq!(mixer.own_len(), MAX_OWN_SAMPLES);
      let mixed = mixer.mix_i16(&[0.0; FRAME_SAMPLES_PER_CHANNEL]);
      assert_eq!(mixed.len(), FRAME_SAMPLES_PER_CHANNEL);
      assert!(mixed.iter().all(|sample| *sample > 0));
    }

    #[test]
    fn loud_parts_clip_instead_of_wrapping_around() {
      let mut loud = RecordingMixer::new();
      loud.push_own_i16(&[i16::MAX; 4]);
      assert_eq!(loud.mix_i16(&[1.0; 4]), vec![i16::MAX; 4]);
      loud.push_own(&[-1.0; 4]);
      assert_eq!(loud.mix(&[-1.0; 4]), vec![-1.0; 4]);
    }

    #[test]
    fn played_frames_are_the_clock() {
      let mut mixer = RecordingMixer::new();
      // No own voice (muted): the played frame as it is.
      assert_eq!(mixer.mix(&[0.25; 3]), vec![0.25; 3]);
      // Less own voice than a frame: the rest of the frame is played audio only.
      mixer.push_own(&[0.5]);
      assert_eq!(mixer.mix(&[0.25; 3]), vec![0.75, 0.25, 0.25]);
      assert_eq!(mixer.own_len(), 0);
    }
  }

  mod limits {
    use super::*;

    #[test]
    fn frames_follow_the_length_within_the_ceiling() {
      assert_eq!(max_frames(Duration::from_secs(60)), 3_000);
      assert_eq!(max_frames(Duration::from_millis(200)), 10);
      assert_eq!(max_frames(Duration::from_secs(24 * 60 * 60)), 8 * 60 * 60 * 50);
    }

    #[test]
    fn the_pre_skip_comes_from_an_opus_head_only() {
      let mut head = b"OpusHead".to_vec();
      head.extend_from_slice(&[1, 1, 0x38, 0x01, 0x80, 0xbb, 0, 0, 0, 0, 0]);
      assert_eq!(pre_skip_of(&head), Some(312));
      assert_eq!(pre_skip_of(&head[..18]), None, "too short");
      assert_eq!(pre_skip_of(b"OpusTags..........."), None);
      assert_eq!(pre_skip_of(&[]), None);
    }
  }

  mod spool {
    use super::*;

    #[test]
    fn bytes_are_taken_once_enough_wait() {
      let mut spool = RecordingSpool::new();
      let reader = spool.clone();
      spool.write_all(b"abc").unwrap();
      assert_eq!(reader.take(4), None);
      assert_eq!(reader.pending_len(), 3);
      spool.write_all(b"de").unwrap();
      assert_eq!(reader.take(4).as_deref(), Some(&b"abcde"[..]));
      assert_eq!(reader.pending_len(), 0);
      assert_eq!(reader.take(0), None, "nothing pending");
      spool.write_all(b"f").unwrap();
      assert_eq!(reader.take(0).as_deref(), Some(&b"f"[..]));
      assert_eq!(reader.written(), 6);
    }
  }

  mod prefetched {
    use super::*;

    #[test]
    fn running_dry_before_the_end_is_an_error_not_the_end_of_the_file() {
      let mut bytes = PrefetchedBytes::new();
      bytes.push(b"ab");
      let mut buf = [0u8; 4];
      assert_eq!(bytes.read(&mut buf).unwrap(), 2);
      assert!(bytes.read(&mut buf).is_err());
      bytes.end();
      assert_eq!(bytes.read(&mut buf).unwrap(), 0);
    }

    /// The browser's way of decrypting a take: slices fetched ahead, a chunk at a time.
    #[cfg(feature = "dave")]
    #[test]
    fn a_sealed_file_fed_in_slices_decrypts_whole() {
      use teamcraft_crypto::sealed_stream::{
        CHUNK_SIZE, SealedStreamKey, SealedStreamReader, SealedStreamWriter,
      };

      let key = SealedStreamKey::generate(&mut rand_core::OsRng);
      let plaintext: Vec<u8> = (0..(3 * CHUNK_SIZE as usize + 1234))
        .map(|i| (i % 251) as u8)
        .collect();
      let mut writer = SealedStreamWriter::new(&key, Vec::new(), &mut rand_core::OsRng).unwrap();
      writer.write_all(&plaintext).unwrap();
      let (sealed, _) = writer.finish().unwrap();
      let source = PrefetchedBytes::new();
      let mut slices = sealed.chunks(10_000);
      let ahead = 2 * (CHUNK_SIZE as usize + 1024);
      let mut top_up = |source: &PrefetchedBytes| {
        while source.len() < ahead && !source.is_at_end() {
          match slices.next() {
            Some(slice) => source.push(slice),
            None => source.end(),
          }
        }
      };
      top_up(&source);
      let mut reader = SealedStreamReader::new(&key, source.clone()).unwrap();
      let mut decrypted = Vec::new();
      let mut buf = vec![0u8; CHUNK_SIZE as usize];
      loop {
        top_up(&source);
        let read = reader.read(&mut buf).unwrap();
        if read == 0 {
          break;
        }
        decrypted.extend_from_slice(&buf[..read]);
      }
      assert_eq!(decrypted, plaintext);
    }
  }
}
