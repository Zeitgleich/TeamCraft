//! Writes Opus packets into an Ogg file (RFC 7845), the format of a recorded voice message (#204):
//! what browsers, Android and the desktop play, and what the apps decode where the platform
//! doesn't (`voice_decode`).
//!
//! The writer streams: pages go to the output as they fill, so a long recording costs no memory.
//! A page holds about a second of audio; the last one is marked as the end of the stream.

use std::io::{self, Write};

use crate::audio_frame::SAMPLE_RATE_HZ;

/// A page is written once it holds this many packets (one second of 20 ms packets).
const PACKETS_PER_PAGE: usize = 50;
/// The stream's serial number: one stream per file, so any fixed value does.
const SERIAL: u32 = 0x5443_564d;
/// The vendor string in the comment header.
const VENDOR: &[u8] = b"TeamCraft";
/// Header type flags of a page.
const FIRST_PAGE: u8 = 0x02;
const LAST_PAGE: u8 = 0x04;

/// An Ogg Opus stream being written; [`OggOpusWriter::finish`] writes the last page.
pub struct OggOpusWriter<W: Write> {
  output: W,
  sequence: u32,
  /// Samples at 48 kHz of the packets written, the pre-skip included (RFC 7845 §4).
  granule: u64,
  pre_skip: u16,
  pending: Vec<Vec<u8>>,
}

impl<W: Write> OggOpusWriter<W> {
  /// Starts a mono stream: writes the identification and comment headers. `pre_skip` is the
  /// encoder's lookahead in 48 kHz samples, which players drop from the start.
  pub fn new(mut output: W, pre_skip: u16) -> io::Result<Self> {
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead");
    head.push(1); // version
    head.push(1); // channels
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&SAMPLE_RATE_HZ.to_le_bytes());
    head.extend_from_slice(&0i16.to_le_bytes()); // output gain
    head.push(0); // channel mapping family: mono/stereo
    let mut tags = Vec::with_capacity(16 + VENDOR.len());
    tags.extend_from_slice(b"OpusTags");
    tags.extend_from_slice(&(VENDOR.len() as u32).to_le_bytes());
    tags.extend_from_slice(VENDOR);
    tags.extend_from_slice(&0u32.to_le_bytes()); // no user comments
    write_page(&mut output, FIRST_PAGE, 0, 0, &[head.as_slice()])?;
    write_page(&mut output, 0, 0, 1, &[tags.as_slice()])?;
    Ok(Self {
      output,
      sequence: 2,
      granule: u64::from(pre_skip),
      pre_skip,
      pending: Vec::with_capacity(PACKETS_PER_PAGE),
    })
  }

  /// Adds one packet of `samples` 48 kHz samples (960 for a 20 ms frame).
  pub fn push(&mut self, packet: Vec<u8>, samples: u32) -> io::Result<()> {
    self.granule += u64::from(samples);
    self.pending.push(packet);
    if self.pending.len() >= PACKETS_PER_PAGE {
      self.flush_page(0)?;
    }
    Ok(())
  }

  /// The audio written so far, without the pre-skip, in 48 kHz samples.
  pub fn samples(&self) -> u64 {
    self.granule - u64::from(self.pre_skip)
  }

  /// Writes the last page and hands the output back.
  pub fn finish(mut self) -> io::Result<W> {
    self.flush_page(LAST_PAGE)?;
    self.output.flush()?;
    Ok(self.output)
  }

  fn flush_page(&mut self, flags: u8) -> io::Result<()> {
    let packets: Vec<&[u8]> = self.pending.iter().map(Vec::as_slice).collect();
    write_page(&mut self.output, flags, self.granule, self.sequence, &packets)?;
    self.sequence += 1;
    self.pending.clear();
    Ok(())
  }
}

/// One page with whole packets. Opus packets are at most 1275 bytes (6 segments each), so 50 of
/// them stay below the 255 segments a page may have.
fn write_page(output: &mut impl Write, flags: u8, granule: u64, sequence: u32, packets: &[&[u8]]) -> io::Result<()> {
  let mut segments = Vec::new();
  for packet in packets {
    segments.extend(std::iter::repeat_n(255u8, packet.len() / 255));
    segments.push((packet.len() % 255) as u8);
  }
  if segments.len() > 255 {
    return Err(io::Error::new(io::ErrorKind::InvalidInput, "too many segments for one page"));
  }
  let body_len: usize = packets.iter().map(|packet| packet.len()).sum();
  let mut page = Vec::with_capacity(27 + segments.len() + body_len);
  page.extend_from_slice(b"OggS");
  page.push(0); // version
  page.push(flags);
  page.extend_from_slice(&granule.to_le_bytes());
  page.extend_from_slice(&SERIAL.to_le_bytes());
  page.extend_from_slice(&sequence.to_le_bytes());
  page.extend_from_slice(&0u32.to_le_bytes()); // checksum, filled in below
  page.push(segments.len() as u8);
  page.extend_from_slice(&segments);
  for packet in packets {
    page.extend_from_slice(packet);
  }
  let checksum = crc32(&page);
  page[22..26].copy_from_slice(&checksum.to_le_bytes());
  output.write_all(&page)
}

/// Ogg's CRC-32: polynomial 0x04C11DB7, no reflection, initial value and final XOR zero.
fn crc32(bytes: &[u8]) -> u32 {
  bytes.iter().fold(0u32, |crc, &byte| {
    let mut crc = crc ^ (u32::from(byte) << 24);
    for _ in 0..8 {
      crc = match crc & 0x8000_0000 {
        0 => crc << 1,
        _ => (crc << 1) ^ 0x04C1_1DB7,
      };
    }
    crc
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn writes_headers_pages_and_an_end_of_stream() {
    let mut writer = OggOpusWriter::new(Vec::new(), 312).unwrap();
    for _ in 0..120 {
      writer.push(vec![0xf8, 0xff, 0xfe], 960).unwrap();
    }
    assert_eq!(writer.samples(), 120 * 960);
    let bytes = writer.finish().unwrap();
    let pages = pages_of(&bytes);
    // Head, tags, two full pages of 50 packets and the last one with 20.
    assert_eq!(pages.len(), 5);
    assert_eq!(pages[0].flags, FIRST_PAGE);
    assert!(pages[0].body.starts_with(b"OpusHead"));
    assert!(pages[1].body.starts_with(b"OpusTags"));
    assert_eq!(pages[4].flags, LAST_PAGE);
    assert_eq!(pages[4].granule, 312 + 120 * 960);
    assert_eq!(pages.iter().map(|page| page.sequence).collect::<Vec<_>>(), vec![0, 1, 2, 3, 4]);
  }

  #[test]
  fn long_packets_span_several_segments() {
    let mut writer = OggOpusWriter::new(Vec::new(), 0).unwrap();
    writer.push(vec![7; 600], 960).unwrap();
    let bytes = writer.finish().unwrap();
    let last = pages_of(&bytes).pop().unwrap();
    assert_eq!(last.segments, vec![255, 255, 90]);
    assert_eq!(last.body.len(), 600);
  }

  #[test]
  fn checksum_matches_the_ogg_reference() {
    // CRC-32/CKSUM's check value (0x765e7680) without its final XOR: Ogg's parameters.
    assert_eq!(crc32(b""), 0);
    assert_eq!(crc32(b"123456789"), 0x89a1_897f);
  }

  struct Page {
    flags: u8,
    granule: u64,
    sequence: u32,
    segments: Vec<u8>,
    body: Vec<u8>,
  }

  /// Splits a stream into pages, checking each page's checksum.
  fn pages_of(mut bytes: &[u8]) -> Vec<Page> {
    let mut pages = Vec::new();
    while !bytes.is_empty() {
      assert_eq!(&bytes[..4], b"OggS");
      let count = usize::from(bytes[26]);
      let segments = bytes[27..27 + count].to_vec();
      let body_len: usize = segments.iter().map(|&segment| usize::from(segment)).sum();
      let len = 27 + count + body_len;
      let mut page = bytes[..len].to_vec();
      let stored = u32::from_le_bytes(page[22..26].try_into().unwrap());
      page[22..26].fill(0);
      assert_eq!(crc32(&page), stored, "checksum");
      pages.push(Page {
        flags: bytes[5],
        granule: u64::from_le_bytes(bytes[6..14].try_into().unwrap()),
        sequence: u32::from_le_bytes(bytes[18..22].try_into().unwrap()),
        segments,
        body: bytes[27 + count..len].to_vec(),
      });
      bytes = &bytes[len..];
    }
    pages
  }
}
