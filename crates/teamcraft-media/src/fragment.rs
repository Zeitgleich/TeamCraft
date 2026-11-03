//! Splitting encoded (and DAVE-encrypted) video frames into RTP packets and reassembling them.
//!
//! One generic format for every codec instead of per-codec RTP payload formats: there is no WebRTC
//! interoperability to keep, and DAVE encrypts whole frames before they are split (SPIKE.md, M6
//! design notes). Each packet carries an unencrypted [`FRAME_FRAGMENT_EXTENSION_ID`] extension:
//!
//! ```text
//! frame number (16 bits) | fragment index (8) | fragment count (8) | flags (8, bit 0 = keyframe)
//! ```
//!
//! A frame of more than 255 fragments (a keyframe of a text-heavy screen share at 1440p is about
//! 400 KB, issue #941) uses the long form, with 16-bit index and count:
//!
//! ```text
//! frame number (16 bits) | fragment index (16) | fragment count (16) | flags (8)
//! ```
//!
//! Every other frame keeps the short form, so receivers and SFUs that only know it get those as
//! before; they drop the packets of a long one as malformed (such frames couldn't be sent at
//! all before).
//!
//! The SFU reads it to switch simulcast layers on keyframes and to drop whole frames; it never
//! reads the payload. A frame is complete when all `count` fragments arrived; incomplete frames are
//! dropped once a newer frame completes (the decoder then needs a keyframe or tolerates the gap).

use std::collections::BTreeMap;

use teamcraft_proto::MAX_VIDEO_FRAME_FRAGMENTS;
use thiserror::Error;

use crate::rtp::{FRAME_FRAGMENT_EXTENSION_ID, RtpHeader};

/// Bytes of the fragment extension in its short form (up to 255 fragments)...
const SHORT_EXTENSION_BYTES: usize = 5;
/// ...and in its long form.
const LONG_EXTENSION_BYTES: usize = 7;
/// Keyframe bit in the flags byte.
const KEYFRAME_FLAG: u8 = 0x01;
/// At most this many frames are reassembled at the same time.
const MAX_FRAMES_IN_FLIGHT: usize = 8;

/// Fragmentation info of one packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameFragment {
  pub frame_number: u16,
  pub index: u16,
  pub count: u16,
  pub is_keyframe: bool,
}

/// A reassembled frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteFrame {
  pub frame_number: u16,
  pub is_keyframe: bool,
  pub data: Vec<u8>,
}

/// Reassembles frames from fragments that may arrive out of order.
#[derive(Debug, Default)]
pub struct FrameAssembler {
  partial: BTreeMap<u16, PartialFrame>,
  maybe_last_completed: Option<u16>,
  dropped_frames: u64,
}

#[derive(Debug)]
struct PartialFrame {
  is_keyframe: bool,
  fragments: Vec<Option<Vec<u8>>>,
  received: usize,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FragmentError {
  #[error(
    "frame of {frame_bytes} bytes needs more than {MAX_VIDEO_FRAME_FRAGMENTS} fragments of {max_payload_bytes} bytes"
  )]
  FrameTooLarge {
    frame_bytes: usize,
    max_payload_bytes: usize,
  },
  #[error("fragment payload size must be at least 1 byte")]
  ZeroPayloadSize,
  #[error("fragment extension is missing")]
  MissingExtension,
  #[error("fragment extension is malformed")]
  MalformedExtension,
  #[error("fragment index {index} is not below count {count} (at most {MAX_VIDEO_FRAME_FRAGMENTS})")]
  IndexOutOfRange { index: u16, count: u16 },
  #[error("fragment count changed within frame {0}")]
  InconsistentFrame(u16),
}

impl FrameFragment {
  /// Reads the fragment extension of a packet header (short or long form).
  pub fn from_header(header: &RtpHeader) -> Result<Self, FragmentError> {
    let data = header
      .extension(FRAME_FRAGMENT_EXTENSION_ID)
      .ok_or(FragmentError::MissingExtension)?;
    let (frame_number, index, count, flags) = match data {
      [frame_high, frame_low, index, count, flags] => (
        u16::from_be_bytes([*frame_high, *frame_low]),
        u16::from(*index),
        u16::from(*count),
        *flags,
      ),
      [
        frame_high,
        frame_low,
        index_high,
        index_low,
        count_high,
        count_low,
        flags,
      ] => (
        u16::from_be_bytes([*frame_high, *frame_low]),
        u16::from_be_bytes([*index_high, *index_low]),
        u16::from_be_bytes([*count_high, *count_low]),
        *flags,
      ),
      _ => return Err(FragmentError::MalformedExtension),
    };
    if count == 0 || index >= count || count > MAX_VIDEO_FRAME_FRAGMENTS {
      return Err(FragmentError::IndexOutOfRange { index, count });
    }
    Ok(Self {
      frame_number,
      index,
      count,
      is_keyframe: flags & KEYFRAME_FLAG != 0,
    })
  }

  /// Writes the fragment extension into a packet header: the short form while the frame has at
  /// most 255 fragments, the long form otherwise.
  pub fn write_to(&self, header: &mut RtpHeader) {
    let [frame_high, frame_low] = self.frame_number.to_be_bytes();
    let flags = if self.is_keyframe { KEYFRAME_FLAG } else { 0 };
    let data = match (u8::try_from(self.index), u8::try_from(self.count)) {
      (Ok(index), Ok(count)) => vec![frame_high, frame_low, index, count, flags],
      _ => {
        let ([index_high, index_low], [count_high, count_low]) =
          (self.index.to_be_bytes(), self.count.to_be_bytes());
        vec![
          frame_high, frame_low, index_high, index_low, count_high, count_low, flags,
        ]
      },
    };
    debug_assert!([SHORT_EXTENSION_BYTES, LONG_EXTENSION_BYTES].contains(&data.len()));
    header.set_extension(FRAME_FRAGMENT_EXTENSION_ID, data);
  }

  pub fn is_last(&self) -> bool {
    self.index + 1 == self.count
  }
}

/// Splits a frame into fragments of at most `max_payload_bytes` each.
pub fn fragment_frame(
  frame: &[u8],
  frame_number: u16,
  is_keyframe: bool,
  max_payload_bytes: usize,
) -> Result<Vec<(FrameFragment, &[u8])>, FragmentError> {
  if max_payload_bytes == 0 {
    return Err(FragmentError::ZeroPayloadSize);
  }
  let chunk_count = frame.len().div_ceil(max_payload_bytes).max(1);
  let count = u16::try_from(chunk_count)
    .ok()
    .filter(|count| *count <= MAX_VIDEO_FRAME_FRAGMENTS)
    .ok_or(FragmentError::FrameTooLarge {
      frame_bytes: frame.len(),
      max_payload_bytes,
    })?;
  if frame.is_empty() {
    let fragment = FrameFragment {
      frame_number,
      index: 0,
      count: 1,
      is_keyframe,
    };
    return Ok(vec![(fragment, frame)]);
  }
  Ok(
    frame
      .chunks(max_payload_bytes)
      .zip(0u16..)
      .map(|(chunk, index)| {
        let fragment = FrameFragment {
          frame_number,
          index,
          count,
          is_keyframe,
        };
        (fragment, chunk)
      })
      .collect(),
  )
}

impl FrameAssembler {
  pub fn new() -> Self {
    Self::default()
  }

  /// Adds one fragment; returns the frame when it is complete.
  pub fn push(
    &mut self,
    fragment: FrameFragment,
    payload: &[u8],
  ) -> Result<Option<CompleteFrame>, FragmentError> {
    if self
      .maybe_last_completed
      .is_some_and(|last| !is_newer(fragment.frame_number, last))
    {
      // A late fragment of a frame we already completed or gave up on.
      return Ok(None);
    }
    let partial = self
      .partial
      .entry(fragment.frame_number)
      .or_insert_with(|| PartialFrame {
        is_keyframe: fragment.is_keyframe,
        fragments: vec![None; usize::from(fragment.count)],
        received: 0,
      });
    if partial.fragments.len() != usize::from(fragment.count) {
      return Err(FragmentError::InconsistentFrame(fragment.frame_number));
    }
    let slot = &mut partial.fragments[usize::from(fragment.index)];
    if slot.is_none() {
      *slot = Some(payload.to_vec());
      partial.received += 1;
    }
    if partial.received < partial.fragments.len() {
      self.evict_oldest_if_full();
      return Ok(None);
    }
    let Some(partial) = self.partial.remove(&fragment.frame_number) else {
      return Ok(None);
    };
    self.drop_older_than(fragment.frame_number);
    self.maybe_last_completed = Some(fragment.frame_number);
    Ok(Some(CompleteFrame {
      frame_number: fragment.frame_number,
      is_keyframe: partial.is_keyframe,
      data: partial.fragments.into_iter().flatten().flatten().collect(),
    }))
  }

  /// Frames given up on because fragments were missing.
  pub fn dropped_frames(&self) -> u64 {
    self.dropped_frames
  }

  fn drop_older_than(&mut self, frame_number: u16) {
    let before = self.partial.len();
    self.partial.retain(|&other, _| is_newer(other, frame_number));
    self.dropped_frames += (before - self.partial.len()) as u64;
  }

  fn evict_oldest_if_full(&mut self) {
    while self.partial.len() > MAX_FRAMES_IN_FLIGHT {
      let Some(&oldest) = self.partial.keys().min_by(|a, b| {
        if is_newer(**a, **b) {
          std::cmp::Ordering::Greater
        } else {
          std::cmp::Ordering::Less
        }
      }) else {
        return;
      };
      self.partial.remove(&oldest);
      self.dropped_frames += 1;
    }
  }
}

/// Whether `a` comes after `b` in the wrapping 16-bit frame number space.
fn is_newer(a: u16, b: u16) -> bool {
  a != b && a.wrapping_sub(b) < 0x8000
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::rtp::{PayloadType, RtpTimestamp, SequenceNumber, Ssrc};

  mod splitting {
    use super::*;

    #[test]
    fn frame_is_split_and_reassembled() {
      let frame: Vec<u8> = (0..2500u32).map(|i| (i % 251) as u8).collect();
      let fragments = fragment_frame(&frame, 7, true, 1000).unwrap();
      assert_eq!(fragments.len(), 3);
      assert!(fragments[2].0.is_last());
      let mut assembler = FrameAssembler::new();
      let mut result = None;
      for (fragment, payload) in fragments.iter().rev() {
        result = assembler.push(*fragment, payload).unwrap();
      }
      let complete = result.unwrap();
      assert_eq!(complete.data, frame);
      assert!(complete.is_keyframe);
    }

    #[test]
    fn empty_frame_is_one_fragment() {
      let fragments = fragment_frame(&[], 1, false, 1000).unwrap();
      assert_eq!(fragments.len(), 1);
      assert_eq!(fragments[0].0.count, 1);
    }

    #[test]
    fn oversized_frame_and_zero_payload_are_rejected() {
      let frame = vec![0u8; (usize::from(MAX_VIDEO_FRAME_FRAGMENTS) + 1) * 10];
      assert_eq!(
        fragment_frame(&frame, 1, false, 10),
        Err(FragmentError::FrameTooLarge {
          frame_bytes: frame.len(),
          max_payload_bytes: 10
        })
      );
      assert_eq!(
        fragment_frame(&frame, 1, false, 0),
        Err(FragmentError::ZeroPayloadSize)
      );
    }

    #[test]
    fn extension_round_trips_through_rtp_header() {
      let fragment = FrameFragment {
        frame_number: 0xBEEF,
        index: 2,
        count: 5,
        is_keyframe: true,
      };
      let parsed = through_header(fragment);
      assert_eq!(
        parsed.extension(FRAME_FRAGMENT_EXTENSION_ID).map(<[u8]>::len),
        Some(5)
      );
      assert_eq!(FrameFragment::from_header(&parsed).unwrap(), fragment);
    }

    /// Issue #941: a text-heavy keyframe at 1440p (about 400 KB) needs more than 255 fragments;
    /// such a frame couldn't be sent at all, so the top layer of a share never arrived.
    #[test]
    fn a_frame_of_more_than_255_fragments_uses_the_long_form() {
      let frame: Vec<u8> = (0..400_000u32).map(|i| (i % 251) as u8).collect();
      let fragments = fragment_frame(&frame, 9, true, 1100).unwrap();
      assert_eq!(fragments.len(), 364);
      let mut assembler = FrameAssembler::new();
      let mut maybe_complete = None;
      for (fragment, payload) in &fragments {
        let parsed = through_header(*fragment);
        assert_eq!(
          parsed.extension(FRAME_FRAGMENT_EXTENSION_ID).map(<[u8]>::len),
          Some(7)
        );
        let fragment = FrameFragment::from_header(&parsed).unwrap();
        maybe_complete = assembler.push(fragment, payload).unwrap().or(maybe_complete);
      }
      let complete = maybe_complete.unwrap();
      assert_eq!(complete.data, frame);
      assert!(complete.is_keyframe);
    }

    /// `fragment` written into an RTP header and parsed back.
    fn through_header(fragment: FrameFragment) -> RtpHeader {
      let mut header = RtpHeader::new(
        PayloadType::new(100).unwrap(),
        SequenceNumber(1),
        RtpTimestamp(0),
        Ssrc(1),
      );
      fragment.write_to(&mut header);
      let mut bytes = Vec::new();
      header.write_to(&mut bytes).unwrap();
      RtpHeader::parse(&bytes).unwrap().0
    }
  }

  mod reassembly {
    use super::*;

    #[test]
    fn incomplete_frame_is_dropped_when_a_newer_one_completes() {
      let mut assembler = FrameAssembler::new();
      let lost = fragment_frame(&[1; 20], 1, false, 10).unwrap();
      assert_eq!(assembler.push(lost[0].0, lost[0].1).unwrap(), None);
      let next = fragment_frame(&[2; 5], 2, false, 10).unwrap();
      let complete = assembler.push(next[0].0, next[0].1).unwrap().unwrap();
      assert_eq!(complete.frame_number, 2);
      assert_eq!(assembler.dropped_frames(), 1);
      // The rest of frame 1 arrives too late and is ignored.
      assert_eq!(assembler.push(lost[1].0, lost[1].1).unwrap(), None);
    }

    #[test]
    fn duplicates_are_ignored_and_frame_numbers_wrap() {
      let mut assembler = FrameAssembler::new();
      let fragments = fragment_frame(&[9; 20], u16::MAX, false, 10).unwrap();
      assert_eq!(assembler.push(fragments[0].0, fragments[0].1).unwrap(), None);
      assert_eq!(assembler.push(fragments[0].0, fragments[0].1).unwrap(), None);
      assert!(assembler.push(fragments[1].0, fragments[1].1).unwrap().is_some());
      let wrapped = fragment_frame(&[3; 1], 0, false, 10).unwrap();
      assert!(assembler.push(wrapped[0].0, wrapped[0].1).unwrap().is_some());
    }

    #[test]
    fn inconsistent_counts_are_rejected() {
      let mut assembler = FrameAssembler::new();
      let first = FrameFragment {
        frame_number: 5,
        index: 0,
        count: 3,
        is_keyframe: false,
      };
      assembler.push(first, &[1]).unwrap();
      let liar = FrameFragment { count: 4, ..first };
      assert_eq!(
        assembler.push(liar, &[1]),
        Err(FragmentError::InconsistentFrame(5))
      );
    }

    #[test]
    fn too_many_partial_frames_evict_the_oldest() {
      let mut assembler = FrameAssembler::new();
      for frame_number in 0..=(MAX_FRAMES_IN_FLIGHT as u16) {
        let fragment = FrameFragment {
          frame_number,
          index: 0,
          count: 2,
          is_keyframe: false,
        };
        assembler.push(fragment, &[0]).unwrap();
      }
      assert_eq!(assembler.dropped_frames(), 1);
    }
  }

  mod malformed {
    use super::*;

    #[test]
    fn bad_extensions_are_rejected() {
      let mut header = RtpHeader::new(
        PayloadType::new(100).unwrap(),
        SequenceNumber(1),
        RtpTimestamp(0),
        Ssrc(1),
      );
      assert_eq!(
        FrameFragment::from_header(&header),
        Err(FragmentError::MissingExtension)
      );
      header.set_extension(FRAME_FRAGMENT_EXTENSION_ID, vec![0, 1, 2]);
      assert_eq!(
        FrameFragment::from_header(&header),
        Err(FragmentError::MalformedExtension)
      );
      header.set_extension(FRAME_FRAGMENT_EXTENSION_ID, vec![0, 1, 3, 3, 0]);
      assert_eq!(
        FrameFragment::from_header(&header),
        Err(FragmentError::IndexOutOfRange { index: 3, count: 3 })
      );
      header.set_extension(FRAME_FRAGMENT_EXTENSION_ID, vec![0, 1, 0, 0, 0]);
      assert!(FrameFragment::from_header(&header).is_err());
      // Long form: index beyond count, more fragments than any frame has, a length in between.
      header.set_extension(FRAME_FRAGMENT_EXTENSION_ID, vec![0, 1, 1, 0, 1, 0, 0]);
      assert_eq!(
        FrameFragment::from_header(&header),
        Err(FragmentError::IndexOutOfRange {
          index: 256,
          count: 256
        })
      );
      header.set_extension(FRAME_FRAGMENT_EXTENSION_ID, vec![0, 1, 0, 0, 0xFF, 0xFF, 0]);
      assert!(FrameFragment::from_header(&header).is_err());
      header.set_extension(FRAME_FRAGMENT_EXTENSION_ID, vec![0, 1, 0, 0, 1, 0]);
      assert_eq!(
        FrameFragment::from_header(&header),
        Err(FragmentError::MalformedExtension)
      );
    }
  }
}
