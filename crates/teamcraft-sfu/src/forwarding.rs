//! Per-room forwarding decisions for audio datagrams, kept free of network types so it is tested
//! directly.
//!
//! A datagram is accepted only on the stream id the SFU assigned to its sender. Its audio level
//! (RFC 6464 extension, unencrypted) feeds the speaker selection; only selected speakers are
//! forwarded. The payload is never read.

use std::{collections::HashMap, time::Instant};

use teamcraft_media::{
  datagram::{DATAGRAM_HEADER_BYTES, DatagramError, DatagramHeader, StreamId},
  rtp::{AudioLevel, RtpError, RtpHeader},
};
use teamcraft_proto::DeviceId;
use thiserror::Error;

use crate::speakers::{SpeakerSelectionConfig, SpeakerSelector};

/// Routing state of one room.
#[derive(Debug)]
pub struct Forwarder {
  audio_streams: HashMap<DeviceId, StreamId>,
  speakers: SpeakerSelector,
}

/// What to do with one datagram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
  /// Forward to every other participant.
  pub should_forward: bool,
  /// New active speaker set to announce, loudest first, when it changed.
  pub maybe_active_speakers: Option<Vec<DeviceId>>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ForwardError {
  #[error("sender is not in the room")]
  NotJoined,
  #[error("stream {actual:?} does not belong to the sender (expected {expected:?})")]
  ForeignStream { expected: StreamId, actual: StreamId },
  #[error("invalid datagram: {0}")]
  Datagram(#[from] DatagramError),
  #[error("invalid RTP header: {0}")]
  Rtp(#[from] RtpError),
}

impl Forwarder {
  pub fn new(config: SpeakerSelectionConfig) -> Self {
    Self {
      audio_streams: HashMap::new(),
      speakers: SpeakerSelector::new(config),
    }
  }

  pub fn add(&mut self, device_id: DeviceId, audio_stream_id: StreamId) {
    self.audio_streams.insert(device_id, audio_stream_id);
  }

  /// Removes a participant; returns the new speaker set when it changed.
  pub fn remove(&mut self, device_id: DeviceId, now: Instant) -> Option<Vec<DeviceId>> {
    self.audio_streams.remove(&device_id);
    self.speakers.remove(device_id, now)
  }

  pub fn route(&mut self, sender: DeviceId, datagram: &[u8], now: Instant) -> Result<Route, ForwardError> {
    let expected = *self.audio_streams.get(&sender).ok_or(ForwardError::NotJoined)?;
    let header = DatagramHeader::parse(datagram)?;
    if header.stream_id != expected {
      return Err(ForwardError::ForeignStream {
        expected,
        actual: header.stream_id,
      });
    }
    let (rtp_header, _) = RtpHeader::parse(&datagram[DATAGRAM_HEADER_BYTES..])?;
    let level = rtp_header.audio_level().unwrap_or(AudioLevel::SILENCE);
    let maybe_active_speakers = self.speakers.record(sender, level, now);
    Ok(Route {
      should_forward: self.speakers.is_selected(sender),
      maybe_active_speakers,
    })
  }

  /// The current speaker set, loudest first.
  pub fn active_speakers(&self) -> &[DeviceId] {
    self.speakers.selected()
  }
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use teamcraft_media::{
    datagram::{MediaDatagram, SimulcastLayer},
    rtp::{PayloadType, RtpPacket, RtpTimestamp, SequenceNumber, Ssrc},
  };

  use super::*;

  const ENCRYPTED_FRAME: &[u8] = &[0xAB; 40];

  mod routing {
    use super::*;

    #[test]
    fn top_three_of_five_are_forwarded() {
      let (mut forwarder, now) = room(5);
      // Device n speaks at 10 * n dBov: 1 is the loudest, 5 the quietest.
      let mut forwarded = Vec::new();
      for round in 0..20 {
        for n in 1..=5u64 {
          let at = now + Duration::from_millis(20 * round);
          let route = forwarder.route(device(n), &audio(n, (10 * n) as u8), at).unwrap();
          if round == 19 && route.should_forward {
            forwarded.push(n);
          }
        }
      }
      assert_eq!(forwarded, vec![1, 2, 3]);
      assert_eq!(forwarder.active_speakers(), devices(&[1, 2, 3]));
    }

    #[test]
    fn missing_audio_level_counts_as_silence() {
      let (mut forwarder, now) = room(1);
      let datagram = datagram(1, header_without_level());
      let route = forwarder.route(device(1), &datagram, now).unwrap();
      assert!(!route.should_forward);
      assert_eq!(route.maybe_active_speakers, None);
    }

    #[test]
    fn speaker_changes_are_reported_once() {
      let (mut forwarder, now) = room(2);
      let first = forwarder.route(device(1), &audio(1, 20), now).unwrap();
      assert_eq!(first.maybe_active_speakers, Some(devices(&[1])));
      let again = forwarder
        .route(device(1), &audio(1, 20), now + Duration::from_millis(200))
        .unwrap();
      assert_eq!(again.maybe_active_speakers, None);
      let leave = forwarder.remove(device(1), now + Duration::from_millis(300));
      assert_eq!(leave, Some(Vec::new()));
    }
  }

  mod rejection {
    use super::*;

    #[test]
    fn foreign_stream_is_rejected() {
      let (mut forwarder, now) = room(2);
      // Device 1 sends on device 2's stream.
      assert_eq!(
        forwarder.route(device(1), &audio(2, 10), now),
        Err(ForwardError::ForeignStream {
          expected: StreamId(1),
          actual: StreamId(2),
        })
      );
    }

    #[test]
    fn unknown_sender_is_rejected() {
      let (mut forwarder, now) = room(1);
      assert_eq!(
        forwarder.route(device(9), &audio(1, 10), now),
        Err(ForwardError::NotJoined)
      );
    }

    #[test]
    fn malformed_datagrams_are_rejected() {
      let (mut forwarder, now) = room(1);
      let valid = audio(1, 10);
      for len in [0, 3, DATAGRAM_HEADER_BYTES, DATAGRAM_HEADER_BYTES + 11] {
        assert!(
          forwarder.route(device(1), &valid[..len], now).is_err(),
          "length {len}"
        );
      }
    }
  }

  /// A room with devices 1..=n on streams 1..=n.
  fn room(participants: u64) -> (Forwarder, Instant) {
    let mut forwarder = Forwarder::new(SpeakerSelectionConfig::default());
    for n in 1..=participants {
      forwarder.add(device(n), stream(n));
    }
    (forwarder, Instant::now())
  }

  fn audio(stream_number: u64, level_dbov: u8) -> Vec<u8> {
    let mut header = header_without_level();
    header.set_audio_level(AudioLevel::new(level_dbov, true).unwrap());
    datagram(stream_number, header)
  }

  fn datagram(stream_number: u64, header: RtpHeader) -> Vec<u8> {
    let rtp = RtpPacket {
      header,
      payload: ENCRYPTED_FRAME,
    }
    .to_bytes()
    .unwrap();
    MediaDatagram {
      header: DatagramHeader {
        stream_id: stream(stream_number),
        layer: SimulcastLayer::BASE,
      },
      rtp: &rtp,
    }
    .to_bytes(1200)
    .unwrap()
  }

  fn header_without_level() -> RtpHeader {
    RtpHeader::new(
      PayloadType::new(111).unwrap(),
      SequenceNumber(1),
      RtpTimestamp(0),
      Ssrc(1),
    )
  }

  fn stream(n: u64) -> StreamId {
    StreamId(u32::try_from(n).unwrap())
  }

  fn devices(values: &[u64]) -> Vec<DeviceId> {
    values.iter().copied().map(device).collect()
  }

  fn device(raw: u64) -> DeviceId {
    crate::ids::device_id_from_dave(teamcraft_media::dave::DaveParticipantId(raw)).unwrap()
  }
}
