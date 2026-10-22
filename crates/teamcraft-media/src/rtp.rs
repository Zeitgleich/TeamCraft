//! RTP packets (RFC 3550) with RFC 8285 one-byte header extensions.
//!
//! The only extension TeamCraft interprets is the client-to-mixer audio level (RFC 6464). It stays
//! unencrypted so the SFU can pick the loudest speakers without seeing media (ARCHITECTURE.md §6.2).

use thiserror::Error;

/// RTP version 2, the only version in use.
const RTP_VERSION: u8 = 2;
/// Fixed RTP header size without CSRCs and extensions.
const FIXED_HEADER_BYTES: usize = 12;
/// "Defined by profile" value marking RFC 8285 one-byte header extensions.
const ONE_BYTE_EXTENSION_PROFILE: u16 = 0xBEDE;
/// Extension element ID 15 terminates parsing in the one-byte format (RFC 8285 §4.2).
const ONE_BYTE_RESERVED_ID: u8 = 15;
/// Maximum data length of a one-byte extension element.
const ONE_BYTE_MAX_DATA_BYTES: usize = 16;
/// Maximum number of CSRCs representable in the 4-bit CC field.
const MAX_CSRC_COUNT: usize = 15;

/// Extension ID used for the audio level. Fixed by TeamCraft instead of being negotiated via SDP.
pub const AUDIO_LEVEL_EXTENSION_ID: u8 = 1;
/// Extension ID of the video frame fragmentation info ([`crate::fragment`]).
pub const FRAME_FRAGMENT_EXTENSION_ID: u8 = 3;

/// A parsed RTP packet borrowing its payload from the input buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpPacket<'a> {
  pub header: RtpHeader,
  pub payload: &'a [u8],
}

/// The RTP header fields TeamCraft uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpHeader {
  pub is_marker: bool,
  pub payload_type: PayloadType,
  pub sequence_number: SequenceNumber,
  pub timestamp: RtpTimestamp,
  pub ssrc: Ssrc,
  pub csrcs: Vec<Ssrc>,
  pub extensions: Vec<HeaderExtension>,
}

/// 7-bit RTP payload type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PayloadType(u8);

/// 16-bit RTP sequence number (wraps around).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SequenceNumber(pub u16);

/// 32-bit RTP media timestamp in the codec's clock rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RtpTimestamp(pub u32);

/// Synchronization source identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ssrc(pub u32);

/// One RFC 8285 one-byte header extension element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderExtension {
  pub id: u8,
  pub data: Vec<u8>,
}

/// Audio level of a frame (RFC 6464): `level` is -dBov, 0 is the loudest, 127 is silence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioLevel {
  pub has_voice_activity: bool,
  level: u8,
}

/// Errors parsing or writing RTP.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RtpError {
  #[error("packet too short: {actual_bytes} bytes, need at least {needed_bytes}")]
  Truncated {
    needed_bytes: usize,
    actual_bytes: usize,
  },
  #[error("unsupported RTP version {0}")]
  UnsupportedVersion(u8),
  #[error("unsupported header extension profile {0:#06x}")]
  UnsupportedExtensionProfile(u16),
  #[error("malformed header extension")]
  MalformedExtension,
  #[error("invalid padding length {0}")]
  InvalidPadding(u8),
  #[error("payload type {0} does not fit into 7 bits")]
  InvalidPayloadType(u8),
  #[error("audio level {0} is above 127")]
  InvalidAudioLevel(u8),
  #[error("too many CSRCs: {0}")]
  TooManyCsrcs(usize),
  #[error("extension id {0} is not in 1..=14")]
  InvalidExtensionId(u8),
  #[error("extension data must be 1..=16 bytes, got {0}")]
  InvalidExtensionLength(usize),
}

impl<'a> RtpPacket<'a> {
  /// Parses an RTP packet. Padding is stripped from the returned payload.
  pub fn parse(bytes: &'a [u8]) -> Result<Self, RtpError> {
    let (header, header_len) = RtpHeader::parse(bytes)?;
    let has_padding = bytes[0] & 0x20 != 0;
    let mut payload = &bytes[header_len..];
    if has_padding {
      let padding_len = payload.last().copied().unwrap_or(0);
      if padding_len == 0 || usize::from(padding_len) > payload.len() {
        return Err(RtpError::InvalidPadding(padding_len));
      }
      payload = &payload[..payload.len() - usize::from(padding_len)];
    }
    Ok(Self { header, payload })
  }

  /// Serializes the packet (never adds padding).
  pub fn write_to(&self, out: &mut Vec<u8>) -> Result<(), RtpError> {
    self.header.write_to(out)?;
    out.extend_from_slice(self.payload);
    Ok(())
  }

  pub fn to_bytes(&self) -> Result<Vec<u8>, RtpError> {
    let mut out = Vec::with_capacity(FIXED_HEADER_BYTES + 8 + self.payload.len());
    self.write_to(&mut out)?;
    Ok(out)
  }
}

impl RtpHeader {
  /// Creates a header without CSRCs and extensions.
  pub fn new(
    payload_type: PayloadType,
    sequence_number: SequenceNumber,
    timestamp: RtpTimestamp,
    ssrc: Ssrc,
  ) -> Self {
    Self {
      is_marker: false,
      payload_type,
      sequence_number,
      timestamp,
      ssrc,
      csrcs: Vec::new(),
      extensions: Vec::new(),
    }
  }

  /// Parses only the header and returns it with its length in bytes. Cheap enough for the SFU's
  /// per-packet path; the payload is never inspected.
  pub fn parse(bytes: &[u8]) -> Result<(Self, usize), RtpError> {
    ensure_len(bytes, FIXED_HEADER_BYTES)?;
    let version = bytes[0] >> 6;
    if version != RTP_VERSION {
      return Err(RtpError::UnsupportedVersion(version));
    }
    let has_extension = bytes[0] & 0x10 != 0;
    let csrc_count = usize::from(bytes[0] & 0x0F);
    let is_marker = bytes[1] & 0x80 != 0;
    let payload_type = PayloadType(bytes[1] & 0x7F);
    let sequence_number = SequenceNumber(u16::from_be_bytes([bytes[2], bytes[3]]));
    let timestamp = RtpTimestamp(read_u32(bytes, 4));
    let ssrc = Ssrc(read_u32(bytes, 8));

    let mut offset = FIXED_HEADER_BYTES;
    ensure_len(bytes, offset + 4 * csrc_count)?;
    let csrcs = (0..csrc_count)
      .map(|index| Ssrc(read_u32(bytes, offset + 4 * index)))
      .collect();
    offset += 4 * csrc_count;

    let mut extensions = Vec::new();
    if has_extension {
      ensure_len(bytes, offset + 4)?;
      let profile = u16::from_be_bytes([bytes[offset], bytes[offset + 1]]);
      let length_words = usize::from(u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]]));
      offset += 4;
      let extension_end = offset + 4 * length_words;
      ensure_len(bytes, extension_end)?;
      // Other profiles (two-byte header, 0x100x) are skipped, not rejected: a newer client may use
      // them for extensions this SFU doesn't need to understand.
      if profile == ONE_BYTE_EXTENSION_PROFILE {
        extensions = parse_one_byte_extensions(&bytes[offset..extension_end])?;
      } else if profile & 0xFFF0 != 0x1000 {
        return Err(RtpError::UnsupportedExtensionProfile(profile));
      }
      offset = extension_end;
    }

    let header = Self {
      is_marker,
      payload_type,
      sequence_number,
      timestamp,
      ssrc,
      csrcs,
      extensions,
    };
    Ok((header, offset))
  }

  pub fn write_to(&self, out: &mut Vec<u8>) -> Result<(), RtpError> {
    if self.csrcs.len() > MAX_CSRC_COUNT {
      return Err(RtpError::TooManyCsrcs(self.csrcs.len()));
    }
    let has_extension = !self.extensions.is_empty();
    // Invariant: csrcs.len() <= 15 was checked above, so the cast is lossless.
    let csrc_count = self.csrcs.len() as u8;
    out.push((RTP_VERSION << 6) | (u8::from(has_extension) << 4) | csrc_count);
    out.push((u8::from(self.is_marker) << 7) | self.payload_type.0);
    out.extend_from_slice(&self.sequence_number.0.to_be_bytes());
    out.extend_from_slice(&self.timestamp.0.to_be_bytes());
    out.extend_from_slice(&self.ssrc.0.to_be_bytes());
    for csrc in &self.csrcs {
      out.extend_from_slice(&csrc.0.to_be_bytes());
    }
    if has_extension {
      write_one_byte_extensions(&self.extensions, out)?;
    }
    Ok(())
  }

  /// Data of the one-byte extension `id`, if present.
  pub fn extension(&self, id: u8) -> Option<&[u8]> {
    self
      .extensions
      .iter()
      .find(|extension| extension.id == id)
      .map(|extension| extension.data.as_slice())
  }

  /// Sets or replaces the one-byte extension `id` (validated when the header is written).
  pub fn set_extension(&mut self, id: u8, data: Vec<u8>) {
    self.extensions.retain(|extension| extension.id != id);
    self.extensions.push(HeaderExtension { id, data });
  }

  /// The audio level from the [`AUDIO_LEVEL_EXTENSION_ID`] extension, if present and well formed.
  pub fn audio_level(&self) -> Option<AudioLevel> {
    self
      .extension(AUDIO_LEVEL_EXTENSION_ID)
      .and_then(<[u8]>::first)
      .map(|&byte| AudioLevel {
        has_voice_activity: byte & 0x80 != 0,
        level: byte & 0x7F,
      })
  }

  /// Sets or replaces the audio level extension.
  pub fn set_audio_level(&mut self, audio_level: AudioLevel) {
    let byte = (u8::from(audio_level.has_voice_activity) << 7) | audio_level.level;
    self.set_extension(AUDIO_LEVEL_EXTENSION_ID, vec![byte]);
  }
}

impl PayloadType {
  /// Opus (dynamic range; fixed by TeamCraft instead of negotiated).
  pub const OPUS: Self = Self(111);

  pub fn new(value: u8) -> Result<Self, RtpError> {
    if value > 0x7F {
      return Err(RtpError::InvalidPayloadType(value));
    }
    Ok(Self(value))
  }

  pub fn value(self) -> u8 {
    self.0
  }
}

impl SequenceNumber {
  pub fn next(self) -> Self {
    Self(self.0.wrapping_add(1))
  }
}

impl AudioLevel {
  /// The quietest level; also used when no audio level extension is present.
  pub const SILENCE: Self = Self {
    has_voice_activity: false,
    level: 127,
  };

  /// `level_dbov` is the attenuation in -dBov: 0 is the loudest, 127 is silence.
  pub fn new(level_dbov: u8, has_voice_activity: bool) -> Result<Self, RtpError> {
    if level_dbov > 127 {
      return Err(RtpError::InvalidAudioLevel(level_dbov));
    }
    Ok(Self {
      has_voice_activity,
      level: level_dbov,
    })
  }

  /// Attenuation in -dBov (0 = loudest, 127 = silence).
  pub fn level_dbov(self) -> u8 {
    self.level
  }

  /// Loudness on a linear-in-dB scale where higher is louder (0 = silence, 127 = loudest).
  pub fn loudness(self) -> u8 {
    127 - self.level
  }
}

fn parse_one_byte_extensions(mut bytes: &[u8]) -> Result<Vec<HeaderExtension>, RtpError> {
  let mut extensions = Vec::new();
  while let Some((&first, rest)) = bytes.split_first() {
    // Zero bytes are padding between elements.
    if first == 0 {
      bytes = rest;
      continue;
    }
    let id = first >> 4;
    if id == ONE_BYTE_RESERVED_ID {
      break;
    }
    let data_len = usize::from(first & 0x0F) + 1;
    if rest.len() < data_len {
      return Err(RtpError::MalformedExtension);
    }
    extensions.push(HeaderExtension {
      id,
      data: rest[..data_len].to_vec(),
    });
    bytes = &rest[data_len..];
  }
  Ok(extensions)
}

fn write_one_byte_extensions(extensions: &[HeaderExtension], out: &mut Vec<u8>) -> Result<(), RtpError> {
  let mut body = Vec::new();
  for extension in extensions {
    if extension.id == 0 || extension.id >= ONE_BYTE_RESERVED_ID {
      return Err(RtpError::InvalidExtensionId(extension.id));
    }
    let data_len = extension.data.len();
    if data_len == 0 || data_len > ONE_BYTE_MAX_DATA_BYTES {
      return Err(RtpError::InvalidExtensionLength(data_len));
    }
    // Invariant: 1 <= data_len <= 16, so data_len - 1 fits into 4 bits.
    body.push((extension.id << 4) | (data_len - 1) as u8);
    body.extend_from_slice(&extension.data);
  }
  body.resize(body.len().div_ceil(4) * 4, 0);
  // Invariant: at most 14 ids * 17 bytes, far below u16::MAX words.
  let length_words = (body.len() / 4) as u16;
  out.extend_from_slice(&ONE_BYTE_EXTENSION_PROFILE.to_be_bytes());
  out.extend_from_slice(&length_words.to_be_bytes());
  out.extend_from_slice(&body);
  Ok(())
}

fn ensure_len(bytes: &[u8], needed_bytes: usize) -> Result<(), RtpError> {
  if bytes.len() < needed_bytes {
    return Err(RtpError::Truncated {
      needed_bytes,
      actual_bytes: bytes.len(),
    });
  }
  Ok(())
}

/// Callers check the length with [`ensure_len`] first.
fn read_u32(bytes: &[u8], offset: usize) -> u32 {
  u32::from_be_bytes([
    bytes[offset],
    bytes[offset + 1],
    bytes[offset + 2],
    bytes[offset + 3],
  ])
}

#[cfg(test)]
mod tests {
  use super::*;

  const OPUS_PAYLOAD_TYPE: u8 = 111;
  const PAYLOAD: &[u8] = b"encrypted opus frame";

  mod round_trip {
    use super::*;

    #[test]
    fn plain_header() {
      let header = sample_header();
      let packet = RtpPacket {
        header,
        payload: PAYLOAD,
      };
      let bytes = packet.to_bytes().unwrap();
      assert_eq!(bytes.len(), 12 + PAYLOAD.len());
      assert_eq!(RtpPacket::parse(&bytes).unwrap(), packet);
    }

    #[test]
    fn audio_level_extension() {
      let mut header = sample_header();
      header.set_audio_level(AudioLevel::new(30, true).unwrap());
      let packet = RtpPacket {
        header,
        payload: PAYLOAD,
      };
      let bytes = packet.to_bytes().unwrap();
      // 12 fixed + 4 extension header + 4 (2 bytes element padded to a word).
      assert_eq!(bytes.len(), 20 + PAYLOAD.len());
      let parsed = RtpPacket::parse(&bytes).unwrap();
      let level = parsed.header.audio_level().unwrap();
      assert_eq!(level.level_dbov(), 30);
      assert!(level.has_voice_activity);
      assert_eq!(parsed, packet);
    }

    #[test]
    fn csrcs_and_marker() {
      let mut header = sample_header();
      header.is_marker = true;
      header.csrcs = vec![Ssrc(1), Ssrc(2)];
      let packet = RtpPacket {
        header,
        payload: PAYLOAD,
      };
      let bytes = packet.to_bytes().unwrap();
      assert_eq!(RtpPacket::parse(&bytes).unwrap(), packet);
    }
  }

  mod malformed {
    use super::*;

    #[test]
    fn empty_input() {
      assert_eq!(
        RtpPacket::parse(&[]),
        Err(RtpError::Truncated {
          needed_bytes: 12,
          actual_bytes: 0
        })
      );
    }

    #[test]
    fn every_truncation_of_a_valid_header_is_rejected() {
      let mut header = sample_header();
      header.csrcs = vec![Ssrc(7)];
      header.set_audio_level(AudioLevel::new(10, false).unwrap());
      let mut bytes = Vec::new();
      header.write_to(&mut bytes).unwrap();
      for len in 0..bytes.len() {
        assert!(RtpHeader::parse(&bytes[..len]).is_err(), "length {len} must fail");
      }
    }

    #[test]
    fn wrong_version() {
      let mut bytes = sample_bytes();
      bytes[0] = (1 << 6) | (bytes[0] & 0x3F);
      assert_eq!(RtpPacket::parse(&bytes), Err(RtpError::UnsupportedVersion(1)));
    }

    #[test]
    fn extension_element_longer_than_block() {
      let mut bytes = sample_bytes_without_payload();
      bytes[0] |= 0x10;
      // Profile 0xBEDE, one word, element id 1 claiming 4 data bytes in a 3-byte remainder.
      bytes.extend_from_slice(&[0xBE, 0xDE, 0x00, 0x01, 0x13, 0x00, 0x00, 0x00]);
      assert_eq!(RtpPacket::parse(&bytes), Err(RtpError::MalformedExtension));
    }

    #[test]
    fn unknown_extension_profile() {
      let mut bytes = sample_bytes_without_payload();
      bytes[0] |= 0x10;
      bytes.extend_from_slice(&[0x12, 0x34, 0x00, 0x00]);
      assert_eq!(
        RtpPacket::parse(&bytes),
        Err(RtpError::UnsupportedExtensionProfile(0x1234))
      );
    }

    #[test]
    fn padding_longer_than_payload() {
      let mut bytes = sample_bytes_without_payload();
      bytes[0] |= 0x20;
      bytes.extend_from_slice(&[1, 2, 9]);
      assert_eq!(RtpPacket::parse(&bytes), Err(RtpError::InvalidPadding(9)));
    }
  }

  mod values {
    use super::*;

    #[test]
    fn padding_is_stripped() {
      let mut bytes = sample_bytes_without_payload();
      bytes[0] |= 0x20;
      bytes.extend_from_slice(&[0xAA, 0xBB, 0, 0, 3]);
      assert_eq!(RtpPacket::parse(&bytes).unwrap().payload, &[0xAA, 0xBB]);
    }

    #[test]
    fn two_byte_profile_is_skipped() {
      let mut bytes = sample_bytes_without_payload();
      bytes[0] |= 0x10;
      bytes.extend_from_slice(&[0x10, 0x00, 0x00, 0x01, 0x05, 0x01, 0x42, 0x00, 0xFF]);
      let packet = RtpPacket::parse(&bytes).unwrap();
      assert!(packet.header.extensions.is_empty());
      assert_eq!(packet.payload, &[0xFF]);
    }

    #[test]
    fn invalid_constructor_values() {
      assert_eq!(PayloadType::new(128), Err(RtpError::InvalidPayloadType(128)));
      assert_eq!(AudioLevel::new(128, true), Err(RtpError::InvalidAudioLevel(128)));
      assert_eq!(AudioLevel::new(0, true).unwrap().loudness(), 127);
      assert_eq!(AudioLevel::SILENCE.loudness(), 0);
    }

    #[test]
    fn invalid_extension_on_write() {
      let mut header = sample_header();
      header.extensions.push(HeaderExtension {
        id: 15,
        data: vec![0],
      });
      assert_eq!(
        header.write_to(&mut Vec::new()),
        Err(RtpError::InvalidExtensionId(15))
      );
      header.extensions = vec![HeaderExtension {
        id: 2,
        data: vec![0; 17],
      }];
      assert_eq!(
        header.write_to(&mut Vec::new()),
        Err(RtpError::InvalidExtensionLength(17))
      );
    }

    #[test]
    fn sequence_number_wraps() {
      assert_eq!(SequenceNumber(u16::MAX).next(), SequenceNumber(0));
    }
  }

  fn sample_header() -> RtpHeader {
    RtpHeader::new(
      PayloadType::new(OPUS_PAYLOAD_TYPE).unwrap(),
      SequenceNumber(4242),
      RtpTimestamp(960_000),
      Ssrc(0xDEAD_BEEF),
    )
  }

  fn sample_bytes() -> Vec<u8> {
    RtpPacket {
      header: sample_header(),
      payload: PAYLOAD,
    }
    .to_bytes()
    .unwrap()
  }

  fn sample_bytes_without_payload() -> Vec<u8> {
    let mut bytes = Vec::new();
    sample_header().write_to(&mut bytes).unwrap();
    bytes
  }
}
