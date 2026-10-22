//! Video tracks inside the call engine: RTP packets for DAVE-encrypted frames, and reassembly of
//! received ones.
//!
//! Frames are encrypted whole and then split ([`crate::fragment`]); the SFU forwards one layer
//! per receiver and switches layers only at keyframes. Each layer of a track has its own RTP
//! sequence numbers, while frame numbers count across layers so a receiver's reassembly keeps
//! working when the SFU switches it to another layer.

use teamcraft_proto::{DeviceId, MediaStreamId, VideoCodec, VideoTrack, VideoTrackKind};

use crate::datagram::{SimulcastLayer, StreamId};
use crate::fragment::{CompleteFrame, FragmentError, FrameAssembler, FrameFragment, fragment_frame};
use crate::rtp::{PayloadType, RtpError, RtpHeader, RtpPacket, RtpTimestamp, SequenceNumber, Ssrc};

/// Encrypted frame bytes per packet: the 1200-byte datagram minus the TeamCraft header (7), the
/// RTP header (12) and the fragment extension with its block header (12), with some margin.
pub const MAX_FRAGMENT_PAYLOAD_BYTES: usize = 1100;
/// Simulcast layers per track the datagram header can address.
const MAX_LAYERS: usize = 16;

/// A track this device publishes.
#[derive(Debug)]
pub struct OutgoingVideo {
  pub kind: VideoTrackKind,
  pub codec: VideoCodec,
  frame_number: u16,
  sequences: [SequenceNumber; MAX_LAYERS],
}

/// A track another device publishes.
#[derive(Debug)]
pub struct IncomingVideo {
  pub sender: DeviceId,
  pub kind: VideoTrackKind,
  pub codec: VideoCodec,
  maybe_layer: Option<SimulcastLayer>,
  assembler: FrameAssembler,
}

/// A decrypted video frame for the app's decoder.
#[derive(Clone, PartialEq, Eq)]
pub struct ReceivedVideoFrame {
  pub sender: DeviceId,
  pub stream_id: MediaStreamId,
  pub codec: VideoCodec,
  pub layer: SimulcastLayer,
  pub timestamp: RtpTimestamp,
  pub is_keyframe: bool,
  pub data: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum VideoPacketError {
  #[error(transparent)]
  Fragment(#[from] FragmentError),
  #[error(transparent)]
  Rtp(#[from] RtpError),
}

impl OutgoingVideo {
  pub fn new(kind: VideoTrackKind, codec: VideoCodec) -> Self {
    Self {
      kind,
      codec,
      frame_number: 0,
      sequences: [SequenceNumber(0); MAX_LAYERS],
    }
  }

  /// RTP packets (serialized) carrying one encrypted frame of `layer`.
  pub fn packetize(
    &mut self,
    stream_id: StreamId,
    layer: SimulcastLayer,
    timestamp: RtpTimestamp,
    is_keyframe: bool,
    encrypted: &[u8],
  ) -> Result<Vec<Vec<u8>>, VideoPacketError> {
    let frame_number = self.frame_number;
    self.frame_number = self.frame_number.wrapping_add(1);
    let payload_type = payload_type(&self.codec);
    let sequence = &mut self.sequences[usize::from(layer.value()) % MAX_LAYERS];
    let mut packets = Vec::new();
    for (fragment, payload) in
      fragment_frame(encrypted, frame_number, is_keyframe, MAX_FRAGMENT_PAYLOAD_BYTES)?
    {
      let mut header = RtpHeader::new(payload_type, *sequence, timestamp, Ssrc(stream_id.0));
      header.is_marker = fragment.is_last();
      fragment.write_to(&mut header);
      *sequence = sequence.next();
      packets.push(RtpPacket { header, payload }.to_bytes()?);
    }
    Ok(packets)
  }
}

impl IncomingVideo {
  pub fn new(track: &VideoTrack) -> Self {
    Self {
      sender: track.device_id,
      kind: track.kind.clone(),
      codec: track.codec.clone(),
      maybe_layer: None,
      assembler: FrameAssembler::new(),
    }
  }

  /// Adds one packet; returns the still encrypted frame once all its fragments arrived.
  pub fn push(
    &mut self,
    layer: SimulcastLayer,
    packet: &RtpPacket<'_>,
  ) -> Result<Option<CompleteFrame>, FragmentError> {
    if self.maybe_layer != Some(layer) {
      // Another layer starts at a keyframe; fragments of the old layer can't complete it.
      self.maybe_layer = Some(layer);
      self.assembler = FrameAssembler::new();
    }
    let fragment = FrameFragment::from_header(&packet.header)?;
    self.assembler.push(fragment, packet.payload)
  }
}

/// Dynamic RTP payload types per codec. Receivers know the codec from the published track; the
/// payload type only makes captures readable.
pub fn payload_type(codec: &VideoCodec) -> PayloadType {
  let value = match codec {
    VideoCodec::Av1 => 98,
    VideoCodec::Vp9 => 99,
    VideoCodec::H264 => 100,
    VideoCodec::Unknown(_) => 96,
  };
  // Invariant: all values above are below 128, so `new` can't fail; the fallback is unreachable.
  PayloadType::new(value).unwrap_or(PayloadType::OPUS)
}

// Frame contents don't belong in logs (and are large).
impl std::fmt::Debug for ReceivedVideoFrame {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ReceivedVideoFrame")
      .field("sender", &self.sender)
      .field("stream_id", &self.stream_id)
      .field("codec", &self.codec)
      .field("layer", &self.layer)
      .field("is_keyframe", &self.is_keyframe)
      .field("bytes", &self.data.len())
      .finish()
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn frames_survive_packetizing_and_layer_switches() {
    let mut outgoing = OutgoingVideo::new(VideoTrackKind::Screen, VideoCodec::Vp9);
    let mut incoming = IncomingVideo::new(&track());
    let frame: Vec<u8> = (0..3000u32).map(|i| i as u8).collect();
    let low = SimulcastLayer::BASE;
    let high = SimulcastLayer::new(1).unwrap();
    let mut completed = Vec::new();
    for (layer, is_keyframe) in [(low, true), (low, false), (high, true)] {
      let packets = outgoing
        .packetize(StreamId(7), layer, RtpTimestamp(9000), is_keyframe, &frame)
        .unwrap();
      assert_eq!(packets.len(), 3, "3000 bytes in 1100-byte fragments");
      // Out of order: reassembly doesn't depend on arrival order.
      for bytes in packets.iter().rev() {
        let packet = RtpPacket::parse(bytes).unwrap();
        assert_eq!(packet.header.payload_type, payload_type(&VideoCodec::Vp9));
        if let Some(complete) = incoming.push(layer, &packet).unwrap() {
          completed.push((complete.is_keyframe, complete.data));
        }
      }
    }
    assert_eq!(
      completed,
      vec![(true, frame.clone()), (false, frame.clone()), (true, frame)]
    );
  }

  #[test]
  fn packet_without_fragment_extension_is_rejected() {
    let mut incoming = IncomingVideo::new(&track());
    let header = RtpHeader::new(
      payload_type(&VideoCodec::Vp9),
      SequenceNumber(1),
      RtpTimestamp(0),
      Ssrc(7),
    );
    let bytes = RtpPacket {
      header,
      payload: &[1, 2, 3],
    }
    .to_bytes()
    .unwrap();
    let packet = RtpPacket::parse(&bytes).unwrap();
    assert_eq!(
      incoming.push(SimulcastLayer::BASE, &packet),
      Err(FragmentError::MissingExtension)
    );
  }

  fn track() -> VideoTrack {
    VideoTrack {
      stream_id: MediaStreamId(7),
      device_id: DeviceId::from_snowflake(Snowflake::new(2).unwrap()),
      kind: VideoTrackKind::Screen,
      codec: VideoCodec::Vp9,
      layers: Vec::new(),
    }
  }
}
