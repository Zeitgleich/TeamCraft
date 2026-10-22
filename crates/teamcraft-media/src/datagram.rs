//! Framing of one RTP packet inside one QUIC unreliable datagram.
//!
//! ```text
//!  0                   1                   2                   3
//!  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |Ver|T|R| layer |             stream id (32 bits) ...
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! ... stream id   | [transport sequence number (16 bits), if T]   |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                RTP packet ...
//! ```
//!
//! `stream id` is assigned by the SFU per sending participant and track, so routing never depends
//! on client-chosen SSRCs. `layer` is the simulcast layer (0 for audio and the lowest video layer).
//! `T` marks a transport-wide sequence number for bandwidth estimation: it numbers all datagrams on
//! one connection and one direction (the sender for the uplink, the SFU per receiver for the
//! downlink), and the receiver reports their arrival times (`crate::bwe`). The header is 5 bytes,
//! or 7 with `T`; the reserved bit `R` must be zero.

use thiserror::Error;

/// Current framing version.
pub const DATAGRAM_VERSION: u8 = 0;
/// Size of the TeamCraft header in front of the RTP packet, without transport sequence number.
pub const DATAGRAM_HEADER_BYTES: usize = 5;
/// Flag: a transport sequence number follows the stream id.
const TRANSPORT_SEQ_FLAG: u8 = 0x20;
/// Reserved bit; must be zero.
const RESERVED_MASK: u8 = 0x10;
/// Highest simulcast layer representable in 4 bits.
pub const MAX_LAYER: u8 = 0x0F;

/// One media datagram borrowing its RTP packet from the input buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaDatagram<'a> {
  pub header: DatagramHeader,
  pub rtp: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatagramHeader {
  pub stream_id: StreamId,
  pub layer: SimulcastLayer,
  pub maybe_transport_seq: Option<TransportSeq>,
}

/// Transport-wide sequence number of one datagram on one connection and direction (wraps).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TransportSeq(pub u16);

/// SFU-assigned identifier of one media stream (participant + track).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StreamId(pub u32);

/// Simulcast layer, 0..=15.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SimulcastLayer(u8);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DatagramError {
  #[error("datagram too short: {0} bytes")]
  Truncated(usize),
  #[error("unsupported datagram version {0}")]
  UnsupportedVersion(u8),
  #[error("reserved bits set: {0:#04x}")]
  ReservedBitsSet(u8),
  #[error("simulcast layer {0} is above {MAX_LAYER}")]
  InvalidLayer(u8),
  #[error("datagram of {actual_bytes} bytes exceeds the path limit of {max_bytes} bytes")]
  TooLarge { actual_bytes: usize, max_bytes: usize },
}

impl<'a> MediaDatagram<'a> {
  pub fn parse(bytes: &'a [u8]) -> Result<Self, DatagramError> {
    let header = DatagramHeader::parse(bytes)?;
    Ok(Self {
      header,
      rtp: &bytes[header.encoded_len()..],
    })
  }

  /// Serializes the datagram and checks it against the connection's current datagram limit
  /// (`max_datagram_size` of the QUIC connection, which changes with path MTU discovery).
  pub fn to_bytes(&self, max_datagram_bytes: usize) -> Result<Vec<u8>, DatagramError> {
    let actual_bytes = self.header.encoded_len() + self.rtp.len();
    if actual_bytes > max_datagram_bytes {
      return Err(DatagramError::TooLarge {
        actual_bytes,
        max_bytes: max_datagram_bytes,
      });
    }
    let mut out = Vec::with_capacity(actual_bytes);
    self.header.write_to(&mut out);
    out.extend_from_slice(self.rtp);
    Ok(out)
  }
}

impl DatagramHeader {
  pub fn parse(bytes: &[u8]) -> Result<Self, DatagramError> {
    let Some(header) = bytes.get(..DATAGRAM_HEADER_BYTES) else {
      return Err(DatagramError::Truncated(bytes.len()));
    };
    let version = header[0] >> 6;
    if version != DATAGRAM_VERSION {
      return Err(DatagramError::UnsupportedVersion(version));
    }
    let reserved = header[0] & RESERVED_MASK;
    if reserved != 0 {
      return Err(DatagramError::ReservedBitsSet(reserved));
    }
    let layer = SimulcastLayer(header[0] & MAX_LAYER);
    let stream_id = StreamId(u32::from_be_bytes([header[1], header[2], header[3], header[4]]));
    let maybe_transport_seq = if header[0] & TRANSPORT_SEQ_FLAG != 0 {
      let Some(seq) = bytes.get(DATAGRAM_HEADER_BYTES..DATAGRAM_HEADER_BYTES + 2) else {
        return Err(DatagramError::Truncated(bytes.len()));
      };
      Some(TransportSeq(u16::from_be_bytes([seq[0], seq[1]])))
    } else {
      None
    };
    Ok(Self {
      stream_id,
      layer,
      maybe_transport_seq,
    })
  }

  /// Encoded size in bytes.
  pub fn encoded_len(&self) -> usize {
    match self.maybe_transport_seq {
      Some(_) => DATAGRAM_HEADER_BYTES + 2,
      None => DATAGRAM_HEADER_BYTES,
    }
  }

  pub fn write_to(&self, out: &mut Vec<u8>) {
    let flags = if self.maybe_transport_seq.is_some() {
      TRANSPORT_SEQ_FLAG
    } else {
      0
    };
    out.push((DATAGRAM_VERSION << 6) | flags | self.layer.0);
    out.extend_from_slice(&self.stream_id.0.to_be_bytes());
    if let Some(seq) = self.maybe_transport_seq {
      out.extend_from_slice(&seq.0.to_be_bytes());
    }
  }

  /// Copies a framed datagram with `seq` as its transport sequence number (added or replaced).
  /// The SFU numbers datagrams per receiver this way; the RTP packet is copied unchanged.
  pub fn restamp(datagram: &[u8], seq: TransportSeq) -> Result<Vec<u8>, DatagramError> {
    let header = Self::parse(datagram)?;
    let rtp = &datagram[header.encoded_len()..];
    let stamped = Self {
      maybe_transport_seq: Some(seq),
      ..header
    };
    let mut out = Vec::with_capacity(stamped.encoded_len() + rtp.len());
    stamped.write_to(&mut out);
    out.extend_from_slice(rtp);
    Ok(out)
  }
}

impl TransportSeq {
  pub fn next(self) -> Self {
    Self(self.0.wrapping_add(1))
  }
}

impl SimulcastLayer {
  pub const BASE: Self = Self(0);

  pub fn new(value: u8) -> Result<Self, DatagramError> {
    if value > MAX_LAYER {
      return Err(DatagramError::InvalidLayer(value));
    }
    Ok(Self(value))
  }

  pub fn value(self) -> u8 {
    self.0
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const RTP: &[u8] = &[0x80, 0x6F, 0x00, 0x01];
  const LIMIT_BYTES: usize = 1200;

  #[test]
  fn round_trip() {
    let datagram = MediaDatagram {
      header: header(0x0102_0304, 2, None),
      rtp: RTP,
    };
    let bytes = datagram.to_bytes(LIMIT_BYTES).unwrap();
    assert_eq!(&bytes[..5], &[0x02, 0x01, 0x02, 0x03, 0x04]);
    assert_eq!(MediaDatagram::parse(&bytes).unwrap(), datagram);
  }

  #[test]
  fn transport_seq_round_trip() {
    let datagram = MediaDatagram {
      header: header(7, 1, Some(TransportSeq(0xABCD))),
      rtp: RTP,
    };
    let bytes = datagram.to_bytes(LIMIT_BYTES).unwrap();
    assert_eq!(&bytes[..7], &[0x21, 0, 0, 0, 7, 0xAB, 0xCD]);
    assert_eq!(MediaDatagram::parse(&bytes).unwrap(), datagram);
  }

  #[test]
  fn restamp_adds_or_replaces_the_transport_seq() {
    let plain = MediaDatagram {
      header: header(1, 0, None),
      rtp: RTP,
    }
    .to_bytes(LIMIT_BYTES)
    .unwrap();
    let stamped = DatagramHeader::restamp(&plain, TransportSeq(5)).unwrap();
    let parsed = MediaDatagram::parse(&stamped).unwrap();
    assert_eq!(parsed.header, header(1, 0, Some(TransportSeq(5))));
    assert_eq!(parsed.rtp, RTP);
    let restamped = DatagramHeader::restamp(&stamped, TransportSeq(6)).unwrap();
    assert_eq!(restamped.len(), stamped.len());
    assert_eq!(
      MediaDatagram::parse(&restamped)
        .unwrap()
        .header
        .maybe_transport_seq,
      Some(TransportSeq(6))
    );
  }

  #[test]
  fn truncated() {
    for len in 0..DATAGRAM_HEADER_BYTES {
      assert_eq!(
        MediaDatagram::parse(&[0; 5][..len]),
        Err(DatagramError::Truncated(len))
      );
    }
    // The flag promises two more bytes.
    assert_eq!(
      MediaDatagram::parse(&[0x20, 0, 0, 0, 1, 9]),
      Err(DatagramError::Truncated(6))
    );
  }

  #[test]
  fn wrong_version_and_reserved_bits() {
    assert_eq!(
      MediaDatagram::parse(&[0x40, 0, 0, 0, 1]),
      Err(DatagramError::UnsupportedVersion(1))
    );
    assert_eq!(
      MediaDatagram::parse(&[0x10, 0, 0, 0, 1]),
      Err(DatagramError::ReservedBitsSet(0x10))
    );
  }

  #[test]
  fn too_large() {
    let datagram = MediaDatagram {
      header: header(1, 0, None),
      rtp: RTP,
    };
    assert_eq!(
      datagram.to_bytes(8),
      Err(DatagramError::TooLarge {
        actual_bytes: 9,
        max_bytes: 8
      })
    );
  }

  #[test]
  fn invalid_layer() {
    assert_eq!(SimulcastLayer::new(16), Err(DatagramError::InvalidLayer(16)));
  }

  fn header(stream_id: u32, layer: u8, maybe_transport_seq: Option<TransportSeq>) -> DatagramHeader {
    DatagramHeader {
      stream_id: StreamId(stream_id),
      layer: SimulcastLayer::new(layer).unwrap(),
      maybe_transport_seq,
    }
  }
}
