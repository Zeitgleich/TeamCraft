//! A fragmented MP4 file of one H.264 video track and one Opus audio track: the container of a
//! call recording with video (issue #2250, e2ee.md §8.3).
//!
//! - **Fragmented** (ISO/IEC 14496-12 §8.8): the header (`ftyp`, `moov`) names the tracks and
//!   holds no samples; the media follows in fragments (`moof` + `mdat`) of about two seconds, each
//!   starting at a keyframe. A file cut short (a crash, #2116) is playable up to its last whole
//!   fragment, which [`copy_whole_fragments`] keeps, as `ogg_opus::copy_whole_pages` keeps the
//!   whole pages of an Ogg file.
//! - **H.264** as `avc1` with the parameter sets of the first keyframe in its `avcC`; the samples
//!   carry their parameter sets in-band too, so an encoder that changes them mid-recording (a
//!   hardware encoder that fell back to software) still plays.
//! - **Opus** as `Opus` with `dOps` ("Encapsulation of Opus in ISO Base Media File Format"),
//!   mono at 48 kHz.
//!
//! Plays in VLC, mpv, ffmpeg and the browsers (Chromium, Firefox, Safari 17+), and in the Windows
//! "Media Player". QuickTime Player plays the video but not Opus in MP4. Plain Rust without I/O
//! beyond [`Write`] and [`Read`], so it builds for the browser too.

use std::io::{self, Read, Write};

/// The video track's clock: the RTP video clock.
pub const VIDEO_TIMESCALE: u32 = 90_000;
/// The audio track's clock: Opus' 48 kHz.
pub const AUDIO_TIMESCALE: u32 = 48_000;
/// A fragment ends at the first keyframe after this much video, ...
const MIN_FRAGMENT_TICKS: u64 = VIDEO_TIMESCALE as u64;
/// ... or after this much audio without one (the video stalled, or it is audio only): what a crash
/// can lose, besides the sealed chunk still in memory.
const MAX_FRAGMENT_AUDIO_SAMPLES: u64 = 4 * AUDIO_TIMESCALE as u64;
/// The largest fragment [`copy_whole_fragments`] reads back: far above what a recording writes
/// (about 100 KB per two seconds), so a damaged size can't make it hold the whole file.
const MAX_FRAGMENT_BYTES: u64 = 32 * 1024 * 1024;
/// The largest header [`copy_whole_fragments`] reads back.
const MAX_HEADER_BYTES: u64 = 64 * 1024;
const VIDEO_TRACK: u32 = 1;
const AUDIO_TRACK: u32 = 2;
/// `sample_depends_on = 2` (an I picture): a sync sample.
const KEYFRAME_FLAGS: u32 = 0x0200_0000;
/// `sample_depends_on = 1` and `sample_is_non_sync_sample`.
const DELTA_FLAGS: u32 = 0x0101_0000;
/// `trun` with a data offset, and each sample's duration and size ...
const TRUN_AUDIO_FLAGS: u32 = 0x0000_0001 | 0x0000_0100 | 0x0000_0200;
/// ... and its flags.
const TRUN_VIDEO_FLAGS: u32 = TRUN_AUDIO_FLAGS | 0x0000_0400;
/// `tfhd`: offsets count from the `moof`.
const TFHD_DEFAULT_BASE_IS_MOOF: u32 = 0x0002_0000;
/// NAL unit types: SPS, PPS, access unit delimiter.
const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;
const NAL_AUD: u8 = 9;

/// Writes a fragmented MP4 file to `output`, sample by sample.
pub struct FragmentedMp4Writer<W: Write> {
  output: W,
  bytes: u64,
  sequence: u32,
  fragment: Fragment,
  /// The last video sample: its duration is known once the next one comes.
  maybe_held: Option<VideoSample>,
  /// The decode time of the next audio sample, in [`AUDIO_TIMESCALE`] ticks.
  audio_time: u64,
  /// The presentation time of the last video sample written.
  maybe_last_video: Option<u64>,
  frame_ticks: u32,
  has_video: bool,
}

/// What the header says about the tracks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mp4Tracks {
  /// `None`: an audio-only file (no keyframe came to take the parameter sets from).
  pub maybe_video: Option<Mp4Video>,
  /// Opus' pre-skip, in samples.
  pub pre_skip: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mp4Video {
  pub width: u16,
  pub height: u16,
  /// The nominal frame rate: the duration of the last sample of the file.
  pub framerate: u8,
  pub sps: Vec<u8>,
  pub pps: Vec<u8>,
}

/// What [`copy_whole_fragments`] kept.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeptMp4 {
  pub bytes: u64,
  /// Audio samples at 48 kHz: the recording's length.
  pub audio_samples: u64,
}

#[derive(Default)]
struct Fragment {
  video: Vec<SampleEntry>,
  video_data: Vec<u8>,
  maybe_video_start: Option<u64>,
  video_ticks: u64,
  audio: Vec<SampleEntry>,
  audio_data: Vec<u8>,
  audio_start: u64,
  audio_samples: u64,
}

struct SampleEntry {
  duration: u32,
  size: u32,
  flags: u32,
}

struct VideoSample {
  time: u64,
  is_keyframe: bool,
  data: Vec<u8>,
}

impl<W: Write> FragmentedMp4Writer<W> {
  /// Writes the header for `tracks`.
  pub fn new(mut output: W, tracks: &Mp4Tracks) -> io::Result<Self> {
    let header = header(tracks);
    output.write_all(&header)?;
    let frame_ticks = tracks.maybe_video.as_ref().map_or(VIDEO_TIMESCALE / 15, |video| {
      VIDEO_TIMESCALE / u32::from(video.framerate.max(1))
    });
    Ok(Self {
      output,
      bytes: header.len() as u64,
      sequence: 1,
      fragment: Fragment::default(),
      maybe_held: None,
      audio_time: 0,
      maybe_last_video: None,
      frame_ticks,
      has_video: tracks.maybe_video.is_some(),
    })
  }

  /// One Opus packet of `samples` samples (48 kHz).
  pub fn push_audio(&mut self, packet: &[u8], samples: u32) -> io::Result<()> {
    if self.fragment.audio_samples >= MAX_FRAGMENT_AUDIO_SAMPLES {
      self.flush()?;
    }
    if self.fragment.audio.is_empty() {
      self.fragment.audio_start = self.audio_time;
    }
    self.fragment.audio.push(SampleEntry {
      duration: samples,
      size: entry_size(packet.len())?,
      flags: 0,
    });
    self.fragment.audio_data.extend_from_slice(packet);
    self.fragment.audio_samples += u64::from(samples);
    self.audio_time += u64::from(samples);
    Ok(())
  }

  /// One H.264 picture in Annex B at `time` ([`VIDEO_TIMESCALE`] ticks from the file's start).
  /// A picture not after the last one is dropped (`false`); so is anything in an audio-only file.
  pub fn push_video(&mut self, annex_b: &[u8], time: u64, is_keyframe: bool) -> io::Result<bool> {
    if !self.has_video || self.maybe_last_video.is_some_and(|last| time <= last) {
      return Ok(false);
    }
    self.maybe_last_video = Some(time);
    if let Some(held) = self.maybe_held.take() {
      let duration = u32::try_from(time - held.time).unwrap_or(u32::MAX);
      self.add_video(held, duration)?;
    }
    if is_keyframe && self.fragment.video_ticks >= MIN_FRAGMENT_TICKS {
      self.flush()?;
    }
    self.maybe_held = Some(VideoSample {
      time,
      is_keyframe,
      data: length_prefixed(annex_b),
    });
    Ok(true)
  }

  /// Audio samples written so far (48 kHz).
  pub fn audio_samples(&self) -> u64 {
    self.audio_time
  }

  /// Writes what is left and returns the output and the file's length.
  pub fn finish(mut self) -> io::Result<(W, u64)> {
    if let Some(held) = self.maybe_held.take() {
      self.add_video(held, self.frame_ticks)?;
    }
    self.flush()?;
    self.output.flush()?;
    Ok((self.output, self.bytes))
  }

  fn add_video(&mut self, sample: VideoSample, duration: u32) -> io::Result<()> {
    let fragment = &mut self.fragment;
    if fragment.maybe_video_start.is_none() {
      fragment.maybe_video_start = Some(sample.time);
    }
    fragment.video.push(SampleEntry {
      duration,
      size: entry_size(sample.data.len())?,
      flags: if sample.is_keyframe {
        KEYFRAME_FLAGS
      } else {
        DELTA_FLAGS
      },
    });
    fragment.video_data.extend_from_slice(&sample.data);
    fragment.video_ticks += u64::from(duration);
    Ok(())
  }

  /// Writes the fragment gathered so far, if it holds anything.
  fn flush(&mut self) -> io::Result<()> {
    let fragment = std::mem::take(&mut self.fragment);
    if fragment.video.is_empty() && fragment.audio.is_empty() {
      return Ok(());
    }
    let moof = moof(&fragment, self.sequence);
    let mdat_len = 8 + fragment.video_data.len() + fragment.audio_data.len();
    self.output.write_all(&moof)?;
    self.output.write_all(&box_header(mdat_len, *b"mdat")?)?;
    self.output.write_all(&fragment.video_data)?;
    self.output.write_all(&fragment.audio_data)?;
    self.bytes += (moof.len() + mdat_len) as u64;
    self.sequence += 1;
    Ok(())
  }
}

/// The parameter sets (SPS, PPS) of a keyframe in Annex B, if it carries both.
pub fn parameter_sets(annex_b: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
  let mut maybe_sps = None;
  let mut maybe_pps = None;
  for nal in nal_units(annex_b) {
    match nal.first().map(|header| header & 0x1f) {
      Some(NAL_SPS) if maybe_sps.is_none() && nal.len() >= 4 => maybe_sps = Some(nal.to_vec()),
      Some(NAL_PPS) if maybe_pps.is_none() => maybe_pps = Some(nal.to_vec()),
      _ => {},
    }
  }
  maybe_sps.zip(maybe_pps)
}

/// Copies a fragmented MP4 file that may have been cut short (a recording a crash left) from
/// `input` to `output`: the header, then each fragment (`moof` and its `mdat`) while both are
/// whole, up to the first that isn't or the end of the input. Holds one fragment at a time.
///
/// Fails with `InvalidData` when not even the header and one fragment are there. Read and write
/// errors are passed on.
pub fn copy_whole_fragments(input: &mut impl Read, output: &mut impl Write) -> io::Result<KeptMp4> {
  let mut kept = KeptMp4::default();
  for expected in [*b"ftyp", *b"moov"] {
    let Some((kind, whole)) = read_box(input, MAX_HEADER_BYTES)? else {
      return Err(nothing_to_recover());
    };
    if kind != expected {
      return Err(nothing_to_recover());
    }
    output.write_all(&whole)?;
    kept.bytes += whole.len() as u64;
  }
  let mut fragments = 0u64;
  while let Some((kind, moof)) = read_box(input, MAX_FRAGMENT_BYTES)? {
    if kind != *b"moof" {
      break;
    }
    let Some((kind, mdat)) = read_box(input, MAX_FRAGMENT_BYTES)? else {
      break;
    };
    if kind != *b"mdat" {
      break;
    }
    output.write_all(&moof)?;
    output.write_all(&mdat)?;
    kept.bytes += (moof.len() + mdat.len()) as u64;
    kept.audio_samples += audio_samples_in(&moof);
    fragments += 1;
  }
  if fragments == 0 {
    return Err(nothing_to_recover());
  }
  output.flush()?;
  Ok(kept)
}

/// The NAL units of an Annex B stream, without their start codes.
fn nal_units(annex_b: &[u8]) -> impl Iterator<Item = &[u8]> {
  let mut starts = Vec::new();
  let mut index = 0;
  while index + 3 <= annex_b.len() {
    if annex_b[index] == 0 && annex_b[index + 1] == 0 && annex_b[index + 2] == 1 {
      starts.push(index + 3);
      index += 3;
    } else {
      index += 1;
    }
  }
  let ends: Vec<usize> = starts
    .iter()
    .skip(1)
    .map(|next| {
      // The start code before `next`, with the zero byte of a four-byte one.
      let code = next - 3;
      if code > 0 && annex_b[code - 1] == 0 {
        code - 1
      } else {
        code
      }
    })
    .chain(std::iter::once(annex_b.len()))
    .collect();
  starts
    .into_iter()
    .zip(ends)
    .filter(|(start, end)| start < end)
    .map(move |(start, end)| &annex_b[start..end])
}

/// Annex B as MP4 samples carry it: each NAL unit after its length (four bytes), without access
/// unit delimiters.
fn length_prefixed(annex_b: &[u8]) -> Vec<u8> {
  let mut sample = Vec::with_capacity(annex_b.len() + 16);
  for nal in nal_units(annex_b) {
    if nal.first().is_some_and(|header| header & 0x1f == NAL_AUD) {
      continue;
    }
    sample.extend_from_slice(&(nal.len() as u32).to_be_bytes());
    sample.extend_from_slice(nal);
  }
  sample
}

fn entry_size(len: usize) -> io::Result<u32> {
  u32::try_from(len).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "sample too large"))
}

fn nothing_to_recover() -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, "no fragment to recover in the file")
}

/// The next box of `input`, whole, with its type; `None` at the end of the input, at a box cut
/// short, and at a size that doesn't fit (smaller than its header, larger than `max_bytes`).
fn read_box(input: &mut impl Read, max_bytes: u64) -> io::Result<Option<([u8; 4], Vec<u8>)>> {
  let mut header = [0u8; 8];
  if !read_all_or_end(input, &mut header)? {
    return Ok(None);
  }
  let size = u64::from(u32::from_be_bytes([header[0], header[1], header[2], header[3]]));
  let kind = [header[4], header[5], header[6], header[7]];
  if size < 8 || size > max_bytes {
    return Ok(None);
  }
  let mut whole = header.to_vec();
  whole.resize(size as usize, 0);
  if !read_all_or_end(input, &mut whole[8..])? {
    return Ok(None);
  }
  Ok(Some((kind, whole)))
}

/// Fills `buffer`; `false` when the input ends first.
fn read_all_or_end(input: &mut impl Read, buffer: &mut [u8]) -> io::Result<bool> {
  let mut filled = 0;
  while filled < buffer.len() {
    match input.read(&mut buffer[filled..]) {
      Ok(0) => return Ok(false),
      Ok(read) => filled += read,
      Err(error) if error.kind() == io::ErrorKind::Interrupted => {},
      Err(error) => return Err(error),
    }
  }
  Ok(true)
}

/// The audio samples a `moof` describes: the durations in the audio track's `trun`.
fn audio_samples_in(moof: &[u8]) -> u64 {
  children(&moof[8..])
    .filter(|(kind, _)| kind == b"traf")
    .filter_map(|(_, traf)| {
      let mut track = 0;
      let mut samples = 0u64;
      for (kind, body) in children(traf) {
        match &kind {
          b"tfhd" if body.len() >= 8 => track = read_u32(body, 4),
          b"trun" => samples = trun_duration(body),
          _ => {},
        }
      }
      (track == AUDIO_TRACK).then_some(samples)
    })
    .sum()
}

/// The sum of a `trun`'s sample durations, where it gives them per sample.
fn trun_duration(trun: &[u8]) -> u64 {
  if trun.len() < 8 {
    return 0;
  }
  let flags = read_u32(trun, 0) & 0x00ff_ffff;
  let count = read_u32(trun, 4) as usize;
  let mut at = 8;
  if flags & 0x1 != 0 {
    at += 4;
  }
  if flags & 0x4 != 0 {
    at += 4;
  }
  if flags & 0x100 == 0 {
    return 0;
  }
  let per_sample = [0x100, 0x200, 0x400, 0x800]
    .iter()
    .filter(|bit| flags & **bit != 0)
    .count()
    * 4;
  (0..count)
    .map_while(|index| {
      let offset = at + index * per_sample;
      (offset + 4 <= trun.len()).then(|| u64::from(read_u32(trun, offset)))
    })
    .sum()
}

/// The boxes in `body`, as type and contents (after their header), while they are whole.
fn children(body: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
  let mut at = 0;
  std::iter::from_fn(move || {
    if at + 8 > body.len() {
      return None;
    }
    let size = read_u32(body, at) as usize;
    if size < 8 || at + size > body.len() {
      return None;
    }
    let kind = [body[at + 4], body[at + 5], body[at + 6], body[at + 7]];
    let contents = &body[at + 8..at + size];
    at += size;
    Some((kind, contents))
  })
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
  u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn box_header(len: usize, kind: [u8; 4]) -> io::Result<[u8; 8]> {
  let size = u32::try_from(len).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "box too large"))?;
  let mut header = [0u8; 8];
  header[..4].copy_from_slice(&size.to_be_bytes());
  header[4..].copy_from_slice(&kind);
  Ok(header)
}

/// Builds boxes in memory: each written by a closure that fills its contents.
struct Boxes(Vec<u8>);

impl Boxes {
  fn new() -> Self {
    Self(Vec::new())
  }

  fn plain(&mut self, kind: &[u8; 4], contents: impl FnOnce(&mut Self)) {
    let start = self.0.len();
    self.0.extend_from_slice(&[0, 0, 0, 0]);
    self.0.extend_from_slice(kind);
    contents(self);
    let size = (self.0.len() - start) as u32;
    self.0[start..start + 4].copy_from_slice(&size.to_be_bytes());
  }

  fn full(&mut self, kind: &[u8; 4], version: u8, flags: u32, contents: impl FnOnce(&mut Self)) {
    self.plain(kind, |boxes| {
      boxes.u32((u32::from(version) << 24) | (flags & 0x00ff_ffff));
      contents(boxes);
    });
  }

  fn u8(&mut self, value: u8) {
    self.0.push(value);
  }

  fn u16(&mut self, value: u16) {
    self.0.extend_from_slice(&value.to_be_bytes());
  }

  fn u32(&mut self, value: u32) {
    self.0.extend_from_slice(&value.to_be_bytes());
  }

  fn u64(&mut self, value: u64) {
    self.0.extend_from_slice(&value.to_be_bytes());
  }

  fn bytes(&mut self, bytes: &[u8]) {
    self.0.extend_from_slice(bytes);
  }

  fn zeros(&mut self, count: usize) {
    self.0.resize(self.0.len() + count, 0);
  }

  /// The identity matrix of `mvhd` and `tkhd`.
  fn matrix(&mut self) {
    for value in [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
      self.u32(value);
    }
  }
}

/// `ftyp` and `moov`.
fn header(tracks: &Mp4Tracks) -> Vec<u8> {
  let mut boxes = Boxes::new();
  boxes.plain(b"ftyp", |boxes| {
    boxes.bytes(b"isom");
    boxes.u32(0x200);
    for brand in [b"isom", b"iso6", b"iso2", b"avc1", b"mp41"] {
      boxes.bytes(brand);
    }
  });
  boxes.plain(b"moov", |boxes| {
    boxes.full(b"mvhd", 0, 0, |boxes| {
      // Creation and modification time, timescale (ms), duration (unknown: fragments).
      boxes.u32(0);
      boxes.u32(0);
      boxes.u32(1000);
      boxes.u32(0);
      boxes.u32(0x0001_0000);
      boxes.u16(0x0100);
      boxes.zeros(10);
      boxes.matrix();
      boxes.zeros(24);
      boxes.u32(AUDIO_TRACK + 1);
    });
    if let Some(video) = &tracks.maybe_video {
      video_trak(boxes, video);
    }
    audio_trak(boxes, tracks.pre_skip);
    boxes.plain(b"mvex", |boxes| {
      let tracks_present = tracks
        .maybe_video
        .as_ref()
        .map(|_| VIDEO_TRACK)
        .into_iter()
        .chain([AUDIO_TRACK]);
      for track in tracks_present {
        boxes.full(b"trex", 0, 0, |boxes| {
          boxes.u32(track);
          boxes.u32(1);
          boxes.zeros(12);
        });
      }
    });
  });
  boxes.0
}

fn tkhd(boxes: &mut Boxes, track: u32, volume: u16, width: u16, height: u16) {
  // Enabled and in the movie.
  boxes.full(b"tkhd", 0, 0x3, |boxes| {
    boxes.u32(0);
    boxes.u32(0);
    boxes.u32(track);
    boxes.u32(0);
    boxes.u32(0);
    boxes.zeros(8);
    boxes.u16(0);
    boxes.u16(0);
    boxes.u16(volume);
    boxes.u16(0);
    boxes.matrix();
    boxes.u32(u32::from(width) << 16);
    boxes.u32(u32::from(height) << 16);
  });
}

fn mdhd_hdlr(boxes: &mut Boxes, timescale: u32, handler: &[u8; 4], name: &[u8]) {
  boxes.full(b"mdhd", 0, 0, |boxes| {
    boxes.u32(0);
    boxes.u32(0);
    boxes.u32(timescale);
    boxes.u32(0);
    // "und"
    boxes.u16(0x55c4);
    boxes.u16(0);
  });
  boxes.full(b"hdlr", 0, 0, |boxes| {
    boxes.u32(0);
    boxes.bytes(handler);
    boxes.zeros(12);
    boxes.bytes(name);
    boxes.u8(0);
  });
}

/// `dinf` and the empty sample tables of a fragmented track around `stsd`.
fn dinf_stbl(boxes: &mut Boxes, sample_entry: impl FnOnce(&mut Boxes)) {
  boxes.plain(b"dinf", |boxes| {
    boxes.full(b"dref", 0, 0, |boxes| {
      boxes.u32(1);
      // The media is in this file.
      boxes.full(b"url ", 0, 1, |_| {});
    });
  });
  boxes.plain(b"stbl", |boxes| {
    boxes.full(b"stsd", 0, 0, |boxes| {
      boxes.u32(1);
      sample_entry(boxes);
    });
    boxes.full(b"stts", 0, 0, |boxes| boxes.u32(0));
    boxes.full(b"stsc", 0, 0, |boxes| boxes.u32(0));
    boxes.full(b"stsz", 0, 0, |boxes| {
      boxes.u32(0);
      boxes.u32(0);
    });
    boxes.full(b"stco", 0, 0, |boxes| boxes.u32(0));
  });
}

fn video_trak(boxes: &mut Boxes, video: &Mp4Video) {
  boxes.plain(b"trak", |boxes| {
    tkhd(boxes, VIDEO_TRACK, 0, video.width, video.height);
    boxes.plain(b"mdia", |boxes| {
      mdhd_hdlr(boxes, VIDEO_TIMESCALE, b"vide", b"VideoHandler");
      boxes.plain(b"minf", |boxes| {
        boxes.full(b"vmhd", 0, 1, |boxes| boxes.zeros(8));
        dinf_stbl(boxes, |boxes| {
          boxes.plain(b"avc1", |boxes| {
            boxes.zeros(6);
            boxes.u16(1);
            boxes.zeros(16);
            boxes.u16(video.width);
            boxes.u16(video.height);
            // 72 dpi, no data size, one frame per sample, no compressor name, 24-bit colour.
            boxes.u32(0x0048_0000);
            boxes.u32(0x0048_0000);
            boxes.u32(0);
            boxes.u16(1);
            boxes.zeros(32);
            boxes.u16(0x18);
            boxes.u16(0xffff);
            boxes.plain(b"avcC", |boxes| {
              boxes.u8(1);
              boxes.bytes(&video.sps[1..4]);
              // Four-byte lengths; one SPS, one PPS.
              boxes.u8(0xff);
              boxes.u8(0xe1);
              boxes.u16(video.sps.len() as u16);
              boxes.bytes(&video.sps);
              boxes.u8(1);
              boxes.u16(video.pps.len() as u16);
              boxes.bytes(&video.pps);
            });
          });
        });
      });
    });
  });
}

fn audio_trak(boxes: &mut Boxes, pre_skip: u16) {
  boxes.plain(b"trak", |boxes| {
    tkhd(boxes, AUDIO_TRACK, 0x0100, 0, 0);
    boxes.plain(b"mdia", |boxes| {
      mdhd_hdlr(boxes, AUDIO_TIMESCALE, b"soun", b"SoundHandler");
      boxes.plain(b"minf", |boxes| {
        boxes.full(b"smhd", 0, 0, |boxes| boxes.zeros(4));
        dinf_stbl(boxes, |boxes| {
          boxes.plain(b"Opus", |boxes| {
            boxes.zeros(6);
            boxes.u16(1);
            boxes.zeros(8);
            // Mono, 16 bits, 48 kHz.
            boxes.u16(1);
            boxes.u16(16);
            boxes.zeros(4);
            boxes.u32(AUDIO_TIMESCALE << 16);
            boxes.plain(b"dOps", |boxes| {
              boxes.u8(0);
              boxes.u8(1);
              boxes.u16(pre_skip);
              boxes.u32(AUDIO_TIMESCALE);
              boxes.u16(0);
              boxes.u8(0);
            });
          });
        });
      });
    });
  });
}

/// The `moof` of `fragment`; its `mdat` follows with the video data first, then the audio.
fn moof(fragment: &Fragment, sequence: u32) -> Vec<u8> {
  // Built twice: the data offsets depend on the size of the `moof` itself.
  let build = |moof_len: u32| {
    let mut boxes = Boxes::new();
    boxes.plain(b"moof", |boxes| {
      boxes.full(b"mfhd", 0, 0, |boxes| boxes.u32(sequence));
      let video_offset = moof_len + 8;
      if let Some(start) = fragment.maybe_video_start.filter(|_| !fragment.video.is_empty()) {
        traf(
          boxes,
          VIDEO_TRACK,
          start,
          TRUN_VIDEO_FLAGS,
          video_offset,
          &fragment.video,
        );
      }
      if !fragment.audio.is_empty() {
        let audio_offset = video_offset + fragment.video_data.len() as u32;
        traf(
          boxes,
          AUDIO_TRACK,
          fragment.audio_start,
          TRUN_AUDIO_FLAGS,
          audio_offset,
          &fragment.audio,
        );
      }
    });
    boxes.0
  };
  let first = build(0);
  build(first.len() as u32)
}

fn traf(
  boxes: &mut Boxes,
  track: u32,
  start: u64,
  trun_flags: u32,
  data_offset: u32,
  samples: &[SampleEntry],
) {
  boxes.plain(b"traf", |boxes| {
    boxes.full(b"tfhd", 0, TFHD_DEFAULT_BASE_IS_MOOF, |boxes| boxes.u32(track));
    boxes.full(b"tfdt", 1, 0, |boxes| boxes.u64(start));
    boxes.full(b"trun", 0, trun_flags, |boxes| {
      boxes.u32(samples.len() as u32);
      boxes.u32(data_offset);
      for sample in samples {
        boxes.u32(sample.duration);
        boxes.u32(sample.size);
        if trun_flags & 0x400 != 0 {
          boxes.u32(sample.flags);
        }
      }
    });
  });
}

#[cfg(test)]
mod tests {
  use super::*;

  const SPS: [u8; 8] = [0x67, 0x42, 0xc0, 0x1f, 0xda, 0x01, 0x40, 0x16];
  const PPS: [u8; 4] = [0x68, 0xce, 0x3c, 0x80];

  #[test]
  fn parameter_sets_and_samples_come_from_annex_b() {
    let keyframe = keyframe(0);
    assert_eq!(parameter_sets(&keyframe), Some((SPS.to_vec(), PPS.to_vec())));
    assert_eq!(parameter_sets(&delta(1)), None);
    let sample = length_prefixed(&[0, 0, 0, 1, 0x09, 0xf0, 0, 0, 1, 0x65, 1, 2, 3]);
    assert_eq!(sample, [0, 0, 0, 4, 0x65, 1, 2, 3], "the delimiter is left out");
  }

  #[test]
  fn fragments_start_at_keyframes_and_hold_both_tracks() {
    let file = recording(6, 30);
    let boxes: Vec<[u8; 4]> = children(&file).map(|(kind, _)| kind).collect();
    assert_eq!(&boxes[..2], &[*b"ftyp", *b"moov"]);
    // Six seconds at a keyframe every two seconds: three fragments.
    assert_eq!(
      boxes[2..],
      [*b"moof", *b"mdat", *b"moof", *b"mdat", *b"moof", *b"mdat"]
    );
    let mut copy = Vec::new();
    let kept = copy_whole_fragments(&mut file.as_slice(), &mut copy).unwrap();
    assert_eq!(copy, file, "a finished file copies over unchanged");
    assert_eq!(kept.audio_samples, 6 * 48_000);
    assert_eq!(kept.bytes, file.len() as u64);
  }

  #[test]
  fn the_header_names_both_codecs() {
    let file = recording(1, 15);
    let moov = children(&file).find(|(kind, _)| kind == b"moov").unwrap().1;
    let traks: Vec<&[u8]> = children(moov)
      .filter(|(kind, _)| kind == b"trak")
      .map(|(_, body)| body)
      .collect();
    assert_eq!(traks.len(), 2);
    assert!(contains(traks[0], b"avcC") && contains(traks[0], &SPS));
    assert!(contains(traks[1], b"dOps"));
    assert!(contains(moov, b"trex"));
  }

  #[test]
  fn data_offsets_point_at_the_samples() {
    let file = recording(2, 15);
    let mut at = 0;
    let mut checked = 0;
    for (kind, body) in children(&file) {
      if kind == *b"moof" {
        for (_, traf) in children(body).filter(|(kind, _)| kind == b"traf") {
          let (_, trun) = children(traf).find(|(kind, _)| kind == b"trun").unwrap();
          let track = read_u32(children(traf).next().unwrap().1, 4);
          let offset = read_u32(trun, 8) as usize;
          let first_sample = &file[at + offset..];
          if track == VIDEO_TRACK {
            // A length, then the SPS of the keyframe each fragment starts with.
            assert_eq!(&first_sample[4..6], &SPS[..2]);
          } else {
            assert_eq!(first_sample[0], 0xfc, "an Opus packet of the test");
          }
          checked += 1;
        }
      }
      at += 8 + body.len();
    }
    assert!(checked >= 2);
  }

  #[test]
  fn a_file_cut_short_keeps_its_whole_fragments() {
    let file = recording(6, 30);
    // Cut in the middle of the last fragment's mdat.
    let cut = &file[..file.len() - 100];
    let mut copy = Vec::new();
    let kept = copy_whole_fragments(&mut &cut[..], &mut copy).unwrap();
    assert_eq!(kept.audio_samples, 4 * 48_000);
    assert_eq!(copy.len() as u64, kept.bytes);
    assert!(file.starts_with(&copy));
    // A whole fragment again copies unchanged: the copy is a valid file of its own.
    let mut again = Vec::new();
    assert_eq!(
      copy_whole_fragments(&mut copy.as_slice(), &mut again).unwrap(),
      kept
    );
    assert_eq!(again, copy);
  }

  #[test]
  fn a_file_without_a_whole_fragment_or_header_has_nothing_to_recover() {
    let file = recording(2, 15);
    let header_len = children(&file)
      .take(2)
      .map(|(_, body)| body.len() + 8)
      .sum::<usize>();
    for cut in [0, 10, header_len - 1, header_len, header_len + 20] {
      let error = copy_whole_fragments(&mut &file[..cut], &mut Vec::new()).unwrap_err();
      assert_eq!(error.kind(), io::ErrorKind::InvalidData, "cut at {cut}");
    }
    // Garbage, and a box claiming to be enormous.
    let mut garbage = file[..header_len].to_vec();
    garbage.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, b'm', b'o', b'o', b'f']);
    garbage.extend_from_slice(&[0; 64]);
    assert!(copy_whole_fragments(&mut garbage.as_slice(), &mut Vec::new()).is_err());
    assert!(copy_whole_fragments(&mut &b"not a file at all"[..], &mut Vec::new()).is_err());
  }

  #[test]
  fn audio_alone_still_makes_fragments() {
    let tracks = Mp4Tracks {
      maybe_video: None,
      pre_skip: 312,
    };
    let mut writer = FragmentedMp4Writer::new(Vec::new(), &tracks).unwrap();
    for _ in 0..(10 * 50) {
      writer.push_audio(&[0xfc, 1, 2], 960).unwrap();
    }
    assert!(!writer.push_video(&keyframe(0), 0, true).unwrap());
    let (file, len) = writer.finish().unwrap();
    assert_eq!(len, file.len() as u64);
    let fragments = children(&file).filter(|(kind, _)| kind == b"moof").count();
    assert_eq!(fragments, 3, "every four seconds without video");
    let moov = children(&file).find(|(kind, _)| kind == b"moov").unwrap().1;
    assert_eq!(children(moov).filter(|(kind, _)| kind == b"trak").count(), 1);
  }

  #[test]
  fn pictures_out_of_order_are_dropped() {
    let mut writer = FragmentedMp4Writer::new(Vec::new(), &tracks(15)).unwrap();
    assert!(writer.push_video(&keyframe(0), 6000, true).unwrap());
    assert!(!writer.push_video(&delta(1), 6000, false).unwrap());
    assert!(!writer.push_video(&delta(2), 10, false).unwrap());
    assert!(writer.push_video(&delta(3), 12_000, false).unwrap());
  }

  /// `seconds` of audio (20 ms packets) and video at `framerate`, with a keyframe every two
  /// seconds, interleaved as a recording writes them.
  fn recording(seconds: u32, framerate: u32) -> Vec<u8> {
    let mut writer = FragmentedMp4Writer::new(Vec::new(), &tracks(framerate as u8)).unwrap();
    let frame_ticks = u64::from(VIDEO_TIMESCALE / framerate);
    let mut next_frame = 0u64;
    for packet in 0..(seconds * 50) {
      let now = u64::from(packet) * u64::from(VIDEO_TIMESCALE) / 50;
      while next_frame * frame_ticks <= now {
        let is_keyframe = next_frame.is_multiple_of(2 * u64::from(framerate));
        let picture = if is_keyframe {
          keyframe(next_frame)
        } else {
          delta(next_frame)
        };
        writer
          .push_video(&picture, next_frame * frame_ticks, is_keyframe)
          .unwrap();
        next_frame += 1;
      }
      writer.push_audio(&[0xfc, packet as u8, 0, 0, 0], 960).unwrap();
    }
    writer.finish().unwrap().0
  }

  fn tracks(framerate: u8) -> Mp4Tracks {
    Mp4Tracks {
      maybe_video: Some(Mp4Video {
        width: 1280,
        height: 720,
        framerate,
        sps: SPS.to_vec(),
        pps: PPS.to_vec(),
      }),
      pre_skip: 312,
    }
  }

  fn keyframe(index: u64) -> Vec<u8> {
    let mut data = vec![0, 0, 0, 1];
    data.extend_from_slice(&SPS);
    data.extend_from_slice(&[0, 0, 0, 1]);
    data.extend_from_slice(&PPS);
    data.extend_from_slice(&[0, 0, 1, 0x65, index as u8, 0x88, 0x84]);
    data
  }

  fn delta(index: u64) -> Vec<u8> {
    vec![0, 0, 0, 1, 0x41, index as u8, 0x9a, 0x02]
  }

  fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
  }
}
