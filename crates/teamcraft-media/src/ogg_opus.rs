//! Writes Opus packets into an Ogg file (RFC 7845), the format of a recorded voice message (#204):
//! what browsers, Android and the desktop play, and what the apps decode where the platform
//! doesn't (`voice_decode`).
//!
//! The writer streams: pages go to the output as they fill, so a long recording costs no memory.
//! A page holds about a second of audio; the last one is marked as the end of the stream.
//!
//! A stream a crash cut (a call recording, #2116) is valid up to its last whole page:
//! [`copy_whole_pages`] copies those and marks the last one kept as the end.

use std::io::{self, Read, Write};

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
/// The fixed part of a page header, up to its segment count.
const PAGE_HEADER_LEN: usize = 27;
/// Where the checksum sits in a page.
const CHECKSUM_AT: std::ops::Range<usize> = 22..26;

/// What [`copy_whole_pages`] kept of a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeptOgg {
  /// The bytes written.
  pub bytes: u64,
  /// The audio kept, without the pre-skip, in 48 kHz samples.
  pub samples: u64,
}

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

/// Copies an Ogg Opus stream that may have been cut short (a recording a crash left, #2116) from
/// `input` to `output`: its pages, in order, while each is whole, has a matching checksum and
/// continues the stream (serial and sequence), up to the first that doesn't or the end of the
/// input. The last page kept is marked as the end of the stream, its checksum renewed, so players
/// see a finished file. Holds one page at a time (at most about 64 KiB).
///
/// Fails with `InvalidData` when not even the two header pages and one page of audio are there;
/// `output` may have got the header pages then. Read and write errors are passed on.
pub fn copy_whole_pages(input: &mut impl Read, output: &mut impl Write) -> io::Result<KeptOgg> {
  let mut maybe_held: Option<Vec<u8>> = None;
  let mut kept = KeptOgg { bytes: 0, samples: 0 };
  let mut maybe_serial = None;
  let mut pre_skip = 0u16;
  let mut sequence = 0u32;
  while let Some(page) = read_whole_page(input)? {
    let serial = u32::from_le_bytes([page[14], page[15], page[16], page[17]]);
    let page_sequence = u32::from_le_bytes([page[18], page[19], page[20], page[21]]);
    if page_sequence != sequence || maybe_serial.is_some_and(|known| known != serial) {
      break;
    }
    let body = &page[PAGE_HEADER_LEN + usize::from(page[26])..];
    match sequence {
      0 if body.starts_with(b"OpusHead") && body.len() >= 19 => {
        pre_skip = u16::from_le_bytes([body[10], body[11]]);
        maybe_serial = Some(serial);
      },
      1 if body.starts_with(b"OpusTags") => {},
      0 | 1 => break,
      _ => {},
    }
    if let Some(held) = maybe_held.replace(page) {
      output.write_all(&held)?;
      kept.bytes += held.len() as u64;
    }
    sequence += 1;
  }
  let Some(mut last) = maybe_held.filter(|_| sequence > 2) else {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "no audio to recover in the stream",
    ));
  };
  let granule = u64::from_le_bytes(last[6..14].try_into().unwrap_or_default());
  last[5] |= LAST_PAGE;
  last[CHECKSUM_AT].fill(0);
  let checksum = crc32(&last);
  last[CHECKSUM_AT].copy_from_slice(&checksum.to_le_bytes());
  output.write_all(&last)?;
  output.flush()?;
  kept.bytes += last.len() as u64;
  kept.samples = granule.saturating_sub(u64::from(pre_skip));
  Ok(kept)
}

/// The next page of `input` if it is whole and its checksum matches; `None` at the end of the
/// input, at a page cut short, and at anything that isn't a page.
fn read_whole_page(input: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
  let mut page = vec![0u8; PAGE_HEADER_LEN];
  if !read_all_or_end(input, &mut page)? || &page[..4] != b"OggS" || page[4] != 0 {
    return Ok(None);
  }
  let segment_count = usize::from(page[26]);
  page.resize(PAGE_HEADER_LEN + segment_count, 0);
  if !read_all_or_end(input, &mut page[PAGE_HEADER_LEN..])? {
    return Ok(None);
  }
  let body_len: usize = page[PAGE_HEADER_LEN..]
    .iter()
    .map(|&segment| usize::from(segment))
    .sum();
  let body_at = page.len();
  page.resize(body_at + body_len, 0);
  if !read_all_or_end(input, &mut page[body_at..])? {
    return Ok(None);
  }
  let stored = u32::from_le_bytes([page[22], page[23], page[24], page[25]]);
  let mut unsummed = page.clone();
  unsummed[CHECKSUM_AT].fill(0);
  Ok((crc32(&unsummed) == stored).then_some(page))
}

/// Fills `buffer` from `input`; `false` when the input ends first.
fn read_all_or_end(input: &mut impl Read, buffer: &mut [u8]) -> io::Result<bool> {
  match input.read_exact(buffer) {
    Ok(()) => Ok(true),
    Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
    Err(error) => Err(error),
  }
}

/// One page with whole packets. Opus packets are at most 1275 bytes (6 segments each), so 50 of
/// them stay below the 255 segments a page may have.
fn write_page(
  output: &mut impl Write,
  flags: u8,
  granule: u64,
  sequence: u32,
  packets: &[&[u8]],
) -> io::Result<()> {
  let mut segments = Vec::new();
  for packet in packets {
    segments.extend(std::iter::repeat_n(255u8, packet.len() / 255));
    segments.push((packet.len() % 255) as u8);
  }
  if segments.len() > 255 {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "too many segments for one page",
    ));
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
    assert_eq!(
      pages.iter().map(|page| page.sequence).collect::<Vec<_>>(),
      vec![0, 1, 2, 3, 4]
    );
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

  #[test]
  fn a_stream_cut_anywhere_keeps_its_whole_pages_and_ends_cleanly() {
    let stream = stream_of(120, 312);
    let pages = pages_of(&stream);
    let page_ends: Vec<usize> = pages
      .iter()
      .scan(0, |end, page| {
        *end += page.len;
        Some(*end)
      })
      .collect();
    // Cut inside the fourth page (head, tags, one page of audio stay), right after the fourth
    // page, and not at all.
    for (cut, kept_pages) in [(page_ends[2] + 40, 3), (page_ends[3], 4), (stream.len(), 5)] {
      let mut copy = Vec::new();
      let kept = copy_whole_pages(&mut &stream[..cut], &mut copy).unwrap();
      assert_eq!(kept.bytes, copy.len() as u64);
      let copied = pages_of(&copy);
      assert_eq!(copied.len(), kept_pages, "cut at {cut}");
      let last = copied.last().unwrap();
      assert_eq!(last.flags & LAST_PAGE, LAST_PAGE, "the end is marked");
      assert_eq!(kept.samples, last.granule - 312);
      // Everything before the last page is copied as it was.
      assert_eq!(
        copy[..page_ends[kept_pages - 2]],
        stream[..page_ends[kept_pages - 2]]
      );
    }
  }

  #[test]
  fn a_damaged_page_ends_the_copy_before_it() {
    let mut stream = stream_of(120, 312);
    let pages = pages_of(&stream);
    let fourth = pages[0].len + pages[1].len + pages[2].len;
    stream[fourth + 40] ^= 1;
    let mut copy = Vec::new();
    let kept = copy_whole_pages(&mut stream.as_slice(), &mut copy).unwrap();
    assert_eq!(pages_of(&copy).len(), 3);
    assert_eq!(kept.samples, 50 * 960);
  }

  #[test]
  fn a_stream_without_audio_has_nothing_to_recover() {
    let stream = stream_of(120, 312);
    let pages = pages_of(&stream);
    for cut in [0, 10, pages[0].len, pages[0].len + pages[1].len] {
      let error = copy_whole_pages(&mut &stream[..cut], &mut Vec::new()).unwrap_err();
      assert_eq!(error.kind(), io::ErrorKind::InvalidData, "cut at {cut}");
    }
    assert!(copy_whole_pages(&mut &b"not an ogg file at all"[..], &mut Vec::new()).is_err());
  }

  fn stream_of(packets: usize, pre_skip: u16) -> Vec<u8> {
    let mut writer = OggOpusWriter::new(Vec::new(), pre_skip).unwrap();
    for _ in 0..packets {
      writer.push(vec![0xf8, 0xff, 0xfe], 960).unwrap();
    }
    writer.finish().unwrap()
  }

  struct Page {
    len: usize,
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
        len,
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
