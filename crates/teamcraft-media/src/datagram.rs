//! Framing of one RTP packet inside one QUIC unreliable datagram.
//!
//! ```text
//!  0                   1                   2                   3
//!  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |Ver| reserved  | layer |             stream id (32 bits) ...
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! ... stream id   |                RTP packet ...
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! ```
//!
//! `stream id` is assigned by the SFU per sending participant and track, so routing never depends
//! on client-chosen SSRCs. `layer` is the simulcast layer (0 for audio and the lowest video layer).
//! The header is 5 bytes; reserved bits must be zero and are kept for flags.

use thiserror::Error;

/// Current framing version.
pub const DATAGRAM_VERSION: u8 = 0;
/// Size of the TeamCraft header in front of the RTP packet.
pub const DATAGRAM_HEADER_BYTES: usize = 5;
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
}

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
      rtp: &bytes[DATAGRAM_HEADER_BYTES..],
    })
  }

  /// Serializes the datagram and checks it against the connection's current datagram limit
  /// (`max_datagram_size` of the QUIC connection, which changes with path MTU discovery).
  pub fn to_bytes(&self, max_datagram_bytes: usize) -> Result<Vec<u8>, DatagramError> {
    let actual_bytes = DATAGRAM_HEADER_BYTES + self.rtp.len();
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
    let reserved = header[0] & 0x30;
    if reserved != 0 {
      return Err(DatagramError::ReservedBitsSet(reserved));
    }
    let layer = SimulcastLayer(header[0] & MAX_LAYER);
    let stream_id = StreamId(u32::from_be_bytes([header[1], header[2], header[3], header[4]]));
    Ok(Self { stream_id, layer })
  }

  pub fn write_to(&self, out: &mut Vec<u8>) {
    out.push((DATAGRAM_VERSION << 6) | self.layer.0);
    out.extend_from_slice(&self.stream_id.0.to_be_bytes());
  }

  /// Rewrites the header of an already framed datagram in place. The SFU uses this to replace the
  /// sender's stream id with the one the receiver knows, without copying the RTP packet.
  pub fn overwrite(&self, datagram: &mut [u8]) -> Result<(), DatagramError> {
    let Some(header) = datagram.get_mut(..DATAGRAM_HEADER_BYTES) else {
      return Err(DatagramError::Truncated(datagram.len()));
    };
    header[0] = (DATAGRAM_VERSION << 6) | self.layer.0;
    header[1..].copy_from_slice(&self.stream_id.0.to_be_bytes());
    Ok(())
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
      header: DatagramHeader {
        stream_id: StreamId(0x0102_0304),
        layer: SimulcastLayer::new(2).unwrap(),
      },
      rtp: RTP,
    };
    let bytes = datagram.to_bytes(LIMIT_BYTES).unwrap();
    assert_eq!(&bytes[..5], &[0x02, 0x01, 0x02, 0x03, 0x04]);
    assert_eq!(MediaDatagram::parse(&bytes).unwrap(), datagram);
  }

  #[test]
  fn overwrite_replaces_header_only() {
    let datagram = MediaDatagram {
      header: DatagramHeader {
        stream_id: StreamId(1),
        layer: SimulcastLayer::BASE,
      },
      rtp: RTP,
    };
    let mut bytes = datagram.to_bytes(LIMIT_BYTES).unwrap();
    let new_header = DatagramHeader {
      stream_id: StreamId(99),
      layer: SimulcastLayer::new(1).unwrap(),
    };
    new_header.overwrite(&mut bytes).unwrap();
    let parsed = MediaDatagram::parse(&bytes).unwrap();
    assert_eq!(parsed.header, new_header);
    assert_eq!(parsed.rtp, RTP);
  }

  #[test]
  fn truncated() {
    for len in 0..DATAGRAM_HEADER_BYTES {
      assert_eq!(
        MediaDatagram::parse(&[0; 5][..len]),
        Err(DatagramError::Truncated(len))
      );
    }
    assert_eq!(
      DatagramHeader {
        stream_id: StreamId(1),
        layer: SimulcastLayer::BASE
      }
      .overwrite(&mut [0; 2]),
      Err(DatagramError::Truncated(2))
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
      header: DatagramHeader {
        stream_id: StreamId(1),
        layer: SimulcastLayer::BASE,
      },
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
}
