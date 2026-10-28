//! Video tracks inside the call engine: RTP packets for DAVE-encrypted frames, and reassembly of
//! received ones.
//!
//! Frames are encrypted whole and then split ([`crate::fragment`]); the SFU forwards one layer
//! per receiver and switches layers only at keyframes. Each layer of a track has its own RTP
//! sequence numbers, while frame numbers count across layers so a receiver's reassembly keeps
//! working when the SFU switches it to another layer.

use std::time::Duration;

use teamcraft_proto::{DeviceId, MediaStreamId, VideoCodec, VideoLayerInfo, VideoTrack, VideoTrackKind};
// `std::time::Instant` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

use crate::datagram::{SimulcastLayer, StreamId};
use crate::fragment::{CompleteFrame, FragmentError, FrameAssembler, FrameFragment, fragment_frame};
use crate::rtp::{PayloadType, RtpError, RtpHeader, RtpPacket, RtpTimestamp, SequenceNumber, Ssrc};

/// Encrypted frame bytes per packet: the 1200-byte datagram minus the TeamCraft header (7), the
/// RTP header (12) and the fragment extension with its block header (12), with some margin.
pub const MAX_FRAGMENT_PAYLOAD_BYTES: usize = 1100;
/// Simulcast layers per track the datagram header can address.
const MAX_LAYERS: usize = 16;
/// Skipped packets a receiver waits for (they may come late); more missing at once is a loss.
const MAX_TRACKED_MISSING_PACKETS: usize = 64;
/// The RTP clock of video per millisecond, for the jitter in milliseconds.
const VIDEO_TICKS_PER_MS: f64 = crate::video::VIDEO_CLOCK_RATE_HZ as f64 / 1000.0;
/// A change of transit time this large (10 s) is a wrap of the RTP clock or a restart of the
/// sender, not jitter: the estimate goes on from the new value.
const JITTER_RESTART_TICKS: f64 = 10.0 * crate::video::VIDEO_CLOCK_RATE_HZ as f64;

/// A track this device publishes.
#[derive(Debug)]
pub struct OutgoingVideo {
  pub kind: VideoTrackKind,
  pub codec: VideoCodec,
  pub counters: SentCounters,
  frame_number: u16,
  sequences: [SequenceNumber; MAX_LAYERS],
}

/// What an own track sent so far (stats for nerds, #938).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SentCounters {
  pub packets: u64,
  pub bytes: u64,
  /// Pictures that were meant to go out but didn't: the call not secured yet, or encryption or
  /// packetization failed.
  pub pictures_dropped: u64,
  /// Each layer that sent anything, in the order they first did.
  pub layers: Vec<LayerCounters>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LayerCounters {
  pub layer: u8,
  pub pictures: u64,
  pub bytes: u64,
}

/// What arrived of another device's track so far (stats for nerds, #938).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReceivedCounters {
  pub packets: u64,
  pub bytes: u64,
  /// Packets that never came: gaps in a layer's sequence numbers that weren't filled.
  pub packets_lost: u64,
  /// Complete frames handed on to decryption and the decoder.
  pub frames: u64,
  /// Complete frames that couldn't be used: a picture they build on went missing, or they didn't
  /// decrypt.
  pub frames_discarded: u64,
  /// Interarrival jitter (RFC 3550, 6.4.1) in RTP ticks, once two packets arrived.
  maybe_jitter_ticks: Option<f64>,
  /// Arrival minus RTP time of the last packet, in ticks.
  maybe_last_transit: Option<f64>,
}

/// A track another device publishes.
#[derive(Debug)]
pub struct IncomingVideo {
  pub sender: DeviceId,
  pub kind: VideoTrackKind,
  pub codec: VideoCodec,
  /// The layers the sender offers.
  pub layers: Vec<VideoLayerInfo>,
  pub counters: ReceivedCounters,
  maybe_layer: Option<SimulcastLayer>,
  assembler: FrameAssembler,
  /// Sequence number the next packet of the received layer should have (each layer counts its
  /// own), once one arrived.
  maybe_expected_sequence: Option<SequenceNumber>,
  /// Skipped sequence numbers that may still arrive (reordered). Still missing when a later
  /// frame completes means lost.
  missing: Vec<SequenceNumber>,
  /// Packets went missing since the last [`IncomingVideo::take_loss`].
  is_loss: bool,
  /// Deltas are useless until the next keyframe: none arrived yet, or one went missing.
  is_waiting_for_keyframe: bool,
  maybe_last_keyframe_request: Option<Instant>,
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
      counters: SentCounters::default(),
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

  /// Counts a picture of `layer` that went out as `packets`.
  pub fn count_sent(&mut self, layer: SimulcastLayer, packets: &[Vec<u8>]) {
    let bytes: u64 = packets.iter().map(|packet| packet.len() as u64).sum();
    let counters = &mut self.counters;
    counters.packets += packets.len() as u64;
    counters.bytes += bytes;
    let layer = layer.value();
    if !counters.layers.iter().any(|counted| counted.layer == layer) {
      counters.layers.push(LayerCounters {
        layer,
        ..LayerCounters::default()
      });
    }
    if let Some(counted) = counters.layers.iter_mut().find(|counted| counted.layer == layer) {
      counted.pictures += 1;
      counted.bytes += bytes;
    }
  }

  /// Counts a picture that was meant to go out but didn't.
  pub fn count_dropped(&mut self) {
    self.counters.pictures_dropped += 1;
  }
}

impl IncomingVideo {
  pub fn new(track: &VideoTrack) -> Self {
    Self {
      sender: track.device_id,
      kind: track.kind.clone(),
      codec: track.codec.clone(),
      layers: track.layers.clone(),
      counters: ReceivedCounters::default(),
      maybe_layer: None,
      assembler: FrameAssembler::new(),
      maybe_expected_sequence: None,
      missing: Vec::new(),
      is_loss: false,
      is_waiting_for_keyframe: true,
      maybe_last_keyframe_request: None,
    }
  }

  /// Counts a packet of `bytes` (the whole RTP packet) with RTP time `timestamp` that arrived
  /// `since_start` after a fixed instant (any, the same for every packet).
  pub fn count_arrival(&mut self, bytes: usize, timestamp: RtpTimestamp, since_start: Duration) {
    let counters = &mut self.counters;
    counters.packets += 1;
    counters.bytes += bytes as u64;
    let arrival_ticks = since_start.as_secs_f64() * 1000.0 * VIDEO_TICKS_PER_MS;
    counters.on_transit(arrival_ticks - f64::from(timestamp.0));
  }

  /// Counts a complete frame: handed on (`is_used`), or discarded.
  pub fn count_frame(&mut self, is_used: bool) {
    if is_used {
      self.counters.frames += 1;
    } else {
      self.counters.frames_discarded += 1;
    }
  }

  /// The layer arriving now.
  pub fn layer(&self) -> Option<SimulcastLayer> {
    self.maybe_layer
  }

  /// Adds one packet; returns the still encrypted frame once all its fragments arrived.
  pub fn push(
    &mut self,
    layer: SimulcastLayer,
    packet: &RtpPacket<'_>,
  ) -> Result<Option<CompleteFrame>, FragmentError> {
    let fragment = FrameFragment::from_header(&packet.header)?;
    if self.maybe_layer != Some(layer) {
      // Another layer starts at a keyframe; fragments of the old layer can't complete it, and
      // its deltas can't follow the old layer's pictures.
      self.maybe_layer = Some(layer);
      self.assembler = FrameAssembler::new();
      self.maybe_expected_sequence = None;
      self.missing.clear();
      self.is_waiting_for_keyframe = true;
    }
    self.watch_sequence(packet.header.sequence_number, &fragment);
    let maybe_complete = self.assembler.push(fragment, packet.payload)?;
    // All of this frame arrived, so what is still missing belonged to an earlier one.
    if maybe_complete.is_some() && !self.missing.is_empty() {
      self.counters.packets_lost += self.missing.len() as u64;
      self.missing.clear();
      self.note_loss();
    }
    Ok(maybe_complete)
  }

  /// Whether packets went missing since the last call; the decoder then needs a keyframe.
  pub fn take_loss(&mut self) -> bool {
    std::mem::take(&mut self.is_loss)
  }

  /// Whether a complete frame can go to the decoder: a keyframe always, a delta only while no
  /// picture before it went missing.
  pub fn accepts(&mut self, is_keyframe: bool) -> bool {
    if is_keyframe {
      self.is_waiting_for_keyframe = false;
    }
    !self.is_waiting_for_keyframe
  }

  /// Deltas are dropped until the next keyframe (a frame that didn't decrypt, a new or failed
  /// decoder).
  pub fn wait_for_keyframe(&mut self) {
    self.is_waiting_for_keyframe = true;
  }

  /// Whether a keyframe request may go out now (at most one per `interval`); counts it if so.
  pub fn is_keyframe_request_due(&mut self, now: Instant, interval: Duration) -> bool {
    let is_due = self
      .maybe_last_keyframe_request
      .is_none_or(|last| now.saturating_duration_since(last) >= interval);
    if is_due {
      self.maybe_last_keyframe_request = Some(now);
    }
    is_due
  }

  /// Keeps track of skipped sequence numbers of the received layer. A late packet (reordered)
  /// fills its gap. Whatever is missing before a keyframe's first packet isn't needed any more;
  /// a gap too large to track is a loss at once.
  fn watch_sequence(&mut self, sequence: SequenceNumber, fragment: &FrameFragment) {
    if fragment.is_keyframe && fragment.index == 0 {
      // Not needed any more, but lost all the same.
      self.counters.packets_lost += self.missing.len() as u64;
      self.missing.clear();
    }
    let Some(expected) = self.maybe_expected_sequence else {
      self.maybe_expected_sequence = Some(sequence.next());
      return;
    };
    if is_older(sequence, expected) {
      self.missing.retain(|missing| *missing != sequence);
      return;
    }
    let skipped = sequence.0.wrapping_sub(expected.0);
    if self.missing.len() + usize::from(skipped) > MAX_TRACKED_MISSING_PACKETS {
      self.counters.packets_lost += self.missing.len() as u64 + u64::from(skipped);
      self.missing.clear();
      self.note_loss();
    } else {
      self
        .missing
        .extend((0..skipped).map(|offset| SequenceNumber(expected.0.wrapping_add(offset))));
    }
    self.maybe_expected_sequence = Some(sequence.next());
  }

  fn note_loss(&mut self) {
    self.is_loss = true;
    self.is_waiting_for_keyframe = true;
  }
}

impl ReceivedCounters {
  /// The jitter in milliseconds, once two packets arrived.
  pub fn jitter_ms(&self) -> Option<f64> {
    self.maybe_jitter_ticks.map(|ticks| ticks / VIDEO_TICKS_PER_MS)
  }

  /// RFC 3550's estimator: the deviation of the transit time difference of consecutive packets,
  /// smoothed by 1/16.
  fn on_transit(&mut self, transit: f64) {
    if let Some(last) = self.maybe_last_transit {
      let difference = (transit - last).abs();
      if difference < JITTER_RESTART_TICKS {
        let jitter = self.maybe_jitter_ticks.unwrap_or(0.0);
        self.maybe_jitter_ticks = Some(jitter + (difference - jitter) / 16.0);
      }
    }
    self.maybe_last_transit = Some(transit);
  }
}

/// Whether `a` comes before `b` in 16-bit wrapping sequence order.
fn is_older(a: SequenceNumber, b: SequenceNumber) -> bool {
  a != b && b.0.wrapping_sub(a.0) < 0x8000
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
  fn losses_are_told_from_late_packets_and_end_at_a_keyframe() {
    let mut outgoing = OutgoingVideo::new(VideoTrackKind::Screen, VideoCodec::Vp9);
    let mut incoming = IncomingVideo::new(&track());
    let mut send = |is_keyframe: bool| {
      outgoing
        .packetize(
          StreamId(7),
          SimulcastLayer::BASE,
          RtpTimestamp(0),
          is_keyframe,
          &[7; 2000],
        )
        .unwrap()
    };
    let (keyframe, lost_delta, delta, next_keyframe) = (send(true), send(false), send(false), send(true));
    let push = |incoming: &mut IncomingVideo, packets: &[Vec<u8>]| {
      let mut completed = Vec::new();
      for bytes in packets {
        let packet = RtpPacket::parse(bytes).unwrap();
        if let Some(complete) = incoming.push(SimulcastLayer::BASE, &packet).unwrap() {
          completed.push(incoming.accepts(complete.is_keyframe));
        }
      }
      completed
    };
    assert!(!incoming.accepts(false), "nothing before the first keyframe");
    assert_eq!(push(&mut incoming, &keyframe), vec![true]);
    assert!(!incoming.take_loss());
    // A whole delta lost: the next one completes while its packets are still missing.
    drop(lost_delta);
    assert_eq!(push(&mut incoming, &delta), vec![false]);
    assert!(incoming.take_loss());
    // The keyframe ends the wait; what went missing before it no longer matters.
    assert_eq!(push(&mut incoming, &next_keyframe), vec![true]);
    assert!(!incoming.take_loss());
    // The stats (#938) count the lost delta's two packets once.
    assert_eq!(incoming.counters.packets_lost, 2);
  }

  #[test]
  fn sent_pictures_are_counted_per_layer() {
    let mut outgoing = OutgoingVideo::new(VideoTrackKind::Camera, VideoCodec::H264);
    let high = SimulcastLayer::new(2).unwrap();
    for (layer, bytes) in [(SimulcastLayer::BASE, 500), (high, 2000), (high, 2000)] {
      let packets = outgoing
        .packetize(StreamId(7), layer, RtpTimestamp(0), false, &vec![0; bytes])
        .unwrap();
      outgoing.count_sent(layer, &packets);
    }
    outgoing.count_dropped();
    let counters = &outgoing.counters;
    assert_eq!(counters.packets, 5);
    assert_eq!(counters.pictures_dropped, 1);
    let layers: Vec<(u8, u64)> = counters
      .layers
      .iter()
      .map(|layer| (layer.layer, layer.pictures))
      .collect();
    assert_eq!(layers, [(0, 1), (2, 2)]);
    assert!(counters.layers[1].bytes > 4000, "with the headers");
    assert_eq!(
      counters.bytes,
      counters.layers.iter().map(|layer| layer.bytes).sum::<u64>()
    );
  }

  #[test]
  fn jitter_follows_the_variation_of_transit_times() {
    let mut steady = IncomingVideo::new(&track());
    let mut shaky = IncomingVideo::new(&track());
    assert_eq!(steady.counters.jitter_ms(), None);
    // 30 frames per second (3000 ticks apart); the shaky ones arrive up to 20 ms late.
    for frame in 0..60u32 {
      let timestamp = RtpTimestamp(frame * 3000);
      let on_time = Duration::from_micros(u64::from(frame) * 33_333);
      steady.count_arrival(1000, timestamp, on_time);
      let late = Duration::from_millis(if frame % 2 == 0 { 0 } else { 20 });
      shaky.count_arrival(1000, timestamp, on_time + late);
    }
    assert!(steady.counters.jitter_ms().unwrap() < 0.1);
    let jitter = shaky.counters.jitter_ms().unwrap();
    assert!((15.0..=21.0).contains(&jitter), "{jitter}");
    assert_eq!((shaky.counters.packets, shaky.counters.bytes), (60, 60_000));
    // A jump of the RTP clock (a restarted sender) is no jitter.
    shaky.count_arrival(1000, RtpTimestamp(u32::MAX / 2), Duration::from_secs(3));
    assert!(shaky.counters.jitter_ms().unwrap() <= jitter);
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
      maybe_publication_id: None,
    }
  }
}
