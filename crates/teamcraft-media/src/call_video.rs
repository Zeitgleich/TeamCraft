//! Video tracks inside the call engine: RTP packets for DAVE-encrypted frames, and reassembly of
//! received ones.
//!
//! Frames are encrypted whole and then split ([`crate::fragment`]); the SFU forwards one layer
//! per receiver and switches layers only at keyframes. Each layer of a track has its own RTP
//! sequence numbers, while frame numbers count across layers so a receiver's reassembly keeps
//! working when the SFU switches it to another layer.
//!
//! A received layer's missing packets are asked for again (#1762, [`crate::nack`]) where the SFU
//! resends them: a complete frame waits for the frames before it, so the decoder gets them in order,
//! until their packets arrive or are given up on. A given-up packet is a loss as before: deltas
//! after it are dropped until the next keyframe, which is asked for.

use std::collections::VecDeque;
use std::time::Duration;

use teamcraft_proto::{DeviceId, MediaStreamId, VideoCodec, VideoLayerInfo, VideoTrack, VideoTrackKind};
// `std::time::Instant` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

use crate::datagram::{SimulcastLayer, StreamId};
use crate::fragment::{CompleteFrame, FragmentError, FrameAssembler, FrameFragment, fragment_frame};
use crate::nack::{LateArrival, MissingPackets, NackConfig};
use crate::rtp::{PayloadType, RtpError, RtpHeader, RtpPacket, RtpTimestamp, SequenceNumber, Ssrc};

/// Encrypted frame bytes per packet: the 1200-byte datagram minus the TeamCraft header (7), the
/// RTP header (12) and the fragment extension with its block header (12), with some margin.
pub const MAX_FRAGMENT_PAYLOAD_BYTES: usize = 1100;
/// Simulcast layers per track the datagram header can address.
const MAX_LAYERS: usize = 16;
/// Extended sequence numbers start here, so a frame's first packet (the number minus its fragment
/// index) never goes below zero.
const SEQUENCE_BASE: u64 = 1 << 32;
/// A packet this far behind the next expected one is no late packet but a new start of the
/// sender's numbering (a paused layer that comes back): reception starts over.
const RESTART_DISTANCE: u64 = 4096;
/// Complete frames held back at most while earlier packets are waited for; beyond it the oldest
/// missing packets are given up on.
const MAX_HELD_FRAMES: usize = 64;
/// A frame whose last packets didn't come while nothing else of the layer arrived for this long
/// has lost them (otherwise only the next frame's packets would show the gap: a second later on a
/// still screen).
const TAIL_WAIT: Duration = Duration::from_millis(50);
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
  /// Layers in another codec than the track's (#1733: a share's AV1 layers above H.264).
  layer_codecs: Vec<(u8, VideoCodec)>,
  pub counters: SentCounters,
  frame_number: u16,
  sequences: [SequenceNumber; MAX_LAYERS],
  /// When the uplink was last looked at, and whether it held the top layer back then.
  maybe_uplink_check: Option<(Instant, bool)>,
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
  /// How long the uplink held the top layer back (#1733).
  pub held_back: Duration,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LayerCounters {
  pub layer: u8,
  pub pictures: u64,
  pub bytes: u64,
  pub keyframes: u64,
}

/// What arrived of another device's track so far (stats for nerds, #938).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReceivedCounters {
  pub packets: u64,
  pub bytes: u64,
  /// Packets that never came: gaps in a layer's sequence numbers that weren't filled.
  pub packets_lost: u64,
  /// Packets that were missing, asked for again and came (#1762).
  pub packets_recovered: u64,
  /// Complete frames handed on to decryption and the decoder.
  pub frames: u64,
  /// Complete frames that couldn't be used: a picture they build on went missing, or they didn't
  /// decrypt.
  pub frames_discarded: u64,
  /// Interarrival jitter (RFC 3550, 6.4.1) in RTP ticks, once two packets arrived.
  maybe_jitter_ticks: Option<f64>,
  /// Arrival minus RTP time of the last packet, in ticks.
  maybe_last_transit: Option<f64>,
  /// Keyframes asked of the SFU (#1733).
  pub keyframe_requests: u64,
  /// The longest time between two frames handed to the decoder while subscribed (#1733).
  pub longest_gap: Duration,
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
  /// Extended sequence number the next packet of the received layer should have (each layer
  /// counts its own), once one arrived.
  maybe_expected_sequence: Option<u64>,
  /// Skipped sequence numbers that may still arrive: reordered, or resent by the SFU when asked.
  missing: MissingPackets,
  /// Complete frames waiting for packets of earlier frames, oldest first.
  held: VecDeque<HeldFrame>,
  /// Frames from this sequence number on can't be used without a keyframe: packets before it
  /// were given up on. Applied when the frames before it went to the decoder.
  maybe_loss_at: Option<u64>,
  /// The end (exclusive) of the newest frame seen, and when the layer's last packet arrived.
  maybe_frame_end: Option<u64>,
  maybe_last_arrival: Option<Instant>,
  /// Packets went missing since the last [`IncomingVideo::take_loss`].
  is_loss: bool,
  /// Deltas are useless until the next keyframe: none arrived yet, or one went missing.
  is_waiting_for_keyframe: bool,
  maybe_last_keyframe_request: Option<Instant>,
  /// When the last frame went to the decoder, since the subscription started.
  maybe_last_used: Option<Instant>,
}

/// A complete frame and the sequence number of its first packet.
#[derive(Debug)]
struct HeldFrame {
  first_sequence: u64,
  timestamp: RtpTimestamp,
  frame: CompleteFrame,
}

/// What one received packet brought.
#[derive(Debug, Default)]
pub struct Reception {
  /// Frames for the decoder, in order.
  pub frames: Vec<ReleasedFrame>,
  /// The packet came after later ones: resent or reordered. Its arrival says nothing about jitter.
  pub is_late: bool,
}

/// A complete (still encrypted) frame whose turn came.
#[derive(Debug)]
pub struct ReleasedFrame {
  pub frame: CompleteFrame,
  /// The frame's RTP time (that of each of its packets).
  pub timestamp: RtpTimestamp,
  /// Whether the decoder can use it: a keyframe, or a delta whose pictures before it all arrived.
  /// Unusable ones are counted as discarded already.
  pub is_usable: bool,
}

/// Packets of the received layer to ask the SFU for again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NackRequest {
  pub layer: SimulcastLayer,
  pub sequence_numbers: Vec<u16>,
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
  /// When the datagram that completed the frame arrived (the `now` the engine got it with): how
  /// late the frame reaches a decoder is measured from here, without the network's delay (#1775).
  pub arrived_at: Instant,
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
      layer_codecs: Vec::new(),
      counters: SentCounters::default(),
      frame_number: 0,
      sequences: [SequenceNumber(0); MAX_LAYERS],
      maybe_uplink_check: None,
    }
  }

  /// The track's layers in another codec than the track's own (#1733).
  pub fn with_layers(mut self, layers: &[VideoLayerInfo]) -> Self {
    self.layer_codecs = layers
      .iter()
      .filter_map(|info| {
        let codec = info.codec(&self.codec);
        (codec != &self.codec).then(|| (info.layer, codec.clone()))
      })
      .collect();
    self
  }

  /// The codec `layer` is encoded in.
  pub fn layer_codec(&self, layer: u8) -> &VideoCodec {
    self
      .layer_codecs
      .iter()
      .find(|(other, _)| *other == layer)
      .map_or(&self.codec, |(_, codec)| codec)
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
    let payload_type = payload_type(self.layer_codec(layer.value()));
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

  /// Counts a picture of `layer` (a keyframe or not) that went out as `packets`.
  pub fn count_sent(&mut self, layer: SimulcastLayer, is_keyframe: bool, packets: &[Vec<u8>]) {
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
      counted.keyframes += u64::from(is_keyframe);
    }
  }

  /// The uplink was looked at `now`: whether it holds the top layer back. The time since the last
  /// look counts as held back when it did then.
  pub fn note_uplink(&mut self, is_top_held: bool, now: Instant) {
    if let Some((last, was_held)) = self.maybe_uplink_check
      && was_held
    {
      self.counters.held_back += now.saturating_duration_since(last);
    }
    self.maybe_uplink_check = Some((now, is_top_held));
  }

  /// Counts a picture that was meant to go out but didn't.
  pub fn count_dropped(&mut self) {
    self.counters.pictures_dropped += 1;
  }
}

impl IncomingVideo {
  /// `maybe_nack`: how missing packets are asked for, `None` where the SFU resends nothing.
  pub fn new(track: &VideoTrack, maybe_nack: Option<NackConfig>) -> Self {
    Self {
      sender: track.device_id,
      kind: track.kind.clone(),
      codec: track.codec.clone(),
      layers: track.layers.clone(),
      counters: ReceivedCounters::default(),
      maybe_layer: None,
      assembler: FrameAssembler::new(),
      maybe_expected_sequence: None,
      missing: MissingPackets::new(maybe_nack),
      held: VecDeque::new(),
      maybe_loss_at: None,
      maybe_frame_end: None,
      maybe_last_arrival: None,
      is_loss: false,
      is_waiting_for_keyframe: true,
      maybe_last_keyframe_request: None,
      maybe_last_used: None,
    }
  }

  /// Counts a packet of `bytes` (the whole RTP packet) with RTP time `timestamp` that arrived
  /// `since_start` after a fixed instant (any, the same for every packet).
  pub fn count_arrival(&mut self, bytes: usize, timestamp: RtpTimestamp, since_start: Duration) {
    self.count_late_arrival(bytes);
    let counters = &mut self.counters;
    let arrival_ticks = since_start.as_secs_f64() * 1000.0 * VIDEO_TICKS_PER_MS;
    counters.on_transit(arrival_ticks - f64::from(timestamp.0));
  }

  /// Counts a packet of `bytes` that came after later ones (resent or reordered): its delay is
  /// no jitter.
  pub fn count_late_arrival(&mut self, bytes: usize) {
    self.counters.packets += 1;
    self.counters.bytes += bytes as u64;
  }

  /// Counts a complete frame: handed on (`is_used`), or discarded.
  pub fn count_frame(&mut self, is_used: bool) {
    if is_used {
      self.counters.frames += 1;
    } else {
      self.counters.frames_discarded += 1;
    }
  }

  /// A frame went to the decoder at `now`: the time since the last one may be the longest the
  /// picture stood still.
  pub fn note_used(&mut self, now: Instant) {
    if let Some(last) = self.maybe_last_used {
      let gap = now.saturating_duration_since(last);
      if gap > self.counters.longest_gap {
        self.counters.longest_gap = gap;
      }
    }
    self.maybe_last_used = Some(now);
  }

  /// A new subscription: the time it wasn't received is no gap.
  pub fn restart_gaps(&mut self) {
    self.maybe_last_used = None;
  }

  /// The codec of `layer` (#1733): a share's large layers may be in another codec than the
  /// track. The SFU switches a receiver to another layer at its keyframe, where a decoder for
  /// that codec opens.
  pub fn layer_codec(&self, layer: SimulcastLayer) -> VideoCodec {
    self
      .layers
      .iter()
      .find(|info| info.layer == layer.value())
      .map_or(&self.codec, |info| info.codec(&self.codec))
      .clone()
  }

  /// The layer arriving now.
  pub fn layer(&self) -> Option<SimulcastLayer> {
    self.maybe_layer
  }

  /// The SFU (on a join or rejoin) resends missing packets as `maybe_nack` says, or not at all:
  /// reception starts over on the next packet.
  pub fn set_nack(&mut self, maybe_nack: Option<NackConfig>) {
    self.missing = MissingPackets::new(maybe_nack);
    self.restart();
  }

  /// Reception starts over with the next packet, as on a layer switch (a new subscription: what
  /// was sent meanwhile went elsewhere and isn't missing).
  pub fn restart(&mut self) {
    self.maybe_layer = None;
  }

  /// Adds one packet, which arrived `now`. Returns the frames that may go to the decoder now (a
  /// complete frame waits while packets before it are still expected).
  pub fn push(
    &mut self,
    layer: SimulcastLayer,
    packet: &RtpPacket<'_>,
    now: Instant,
  ) -> Result<Reception, FragmentError> {
    let fragment = FrameFragment::from_header(&packet.header)?;
    if self.maybe_layer != Some(layer) {
      // Another layer starts at a keyframe; fragments of the old layer can't complete it, and
      // its deltas can't follow the old layer's pictures.
      self.switch_to(layer);
    }
    let mut sequence = self.extend(packet.header.sequence_number);
    if self
      .maybe_expected_sequence
      .is_some_and(|expected| expected.saturating_sub(sequence) > RESTART_DISTANCE)
    {
      self.switch_to(layer);
      sequence = self.extend(packet.header.sequence_number);
    }
    let is_late = self
      .maybe_expected_sequence
      .is_some_and(|expected| sequence < expected);
    let first = sequence - u64::from(fragment.index);
    if is_late {
      match self.missing.arrived(sequence, now) {
        LateArrival::Recovered => self.counters.packets_recovered += 1,
        LateArrival::Reordered => {},
        // A duplicate, or a packet given up on: its frame is gone.
        LateArrival::Unexpected => {
          return Ok(Reception {
            frames: Vec::new(),
            is_late,
          });
        },
      }
    } else {
      let start = self.maybe_expected_sequence.unwrap_or(first);
      self.skipped(start..sequence, now);
      self.maybe_expected_sequence = Some(sequence + 1);
    }
    if fragment.is_keyframe && fragment.index == 0 {
      self.start_keyframe(sequence);
    }
    let end = first + u64::from(fragment.count);
    self.maybe_frame_end = Some(self.maybe_frame_end.map_or(end, |known| known.max(end)));
    self.maybe_last_arrival = Some(now);
    if let Some(complete) = self.assembler.push(fragment, packet.payload)? {
      self.hold(first, packet.header.timestamp, complete);
    }
    Ok(Reception {
      frames: self.release(),
      is_late,
    })
  }

  /// Periodic work (every ~100 ms): a frame whose last packets didn't come has lost them, and
  /// packets asked for too often or for too long are given up on. Returns the frames that may go
  /// to the decoder now.
  pub fn on_timer(&mut self, now: Instant) -> Vec<ReleasedFrame> {
    if self.missing.asks_for_packets()
      && let (Some(expected), Some(end), Some(last)) = (
        self.maybe_expected_sequence,
        self.maybe_frame_end,
        self.maybe_last_arrival,
      )
      && end > expected
      && now.saturating_duration_since(last) >= TAIL_WAIT
    {
      self.skipped(expected..end, now);
      self.maybe_expected_sequence = Some(end);
    }
    if let Some(oldest) = self.missing.oldest()
      && let Some((_, lost)) = self.missing.expire(now)
    {
      self.lose(oldest, lost);
    }
    self.release()
  }

  /// The missing packets to ask the SFU for now, if any (counted as asked for).
  pub fn take_nack(&mut self, now: Instant) -> Option<NackRequest> {
    let layer = self.maybe_layer?;
    let sequence_numbers: Vec<u16> = self
      .missing
      .due_requests(now)
      .into_iter()
      // The low 16 bits are the RTP sequence number.
      .map(|sequence| (sequence & u64::from(u16::MAX)) as u16)
      .collect();
    (!sequence_numbers.is_empty()).then_some(NackRequest {
      layer,
      sequence_numbers,
    })
  }

  /// Whether packets went missing for good, or a frame couldn't be used, since the last call; the
  /// decoder then needs a keyframe.
  pub fn take_loss(&mut self) -> bool {
    std::mem::take(&mut self.is_loss)
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
      self.counters.keyframe_requests += 1;
    }
    is_due
  }

  fn switch_to(&mut self, layer: SimulcastLayer) {
    self.maybe_layer = Some(layer);
    self.assembler = FrameAssembler::new();
    self.maybe_expected_sequence = None;
    self.missing.clear();
    self.counters.frames_discarded += self.held.len() as u64;
    self.held.clear();
    self.maybe_loss_at = None;
    self.maybe_frame_end = None;
    self.is_waiting_for_keyframe = true;
  }

  /// The sequence number closest to the next expected one with these low 16 bits.
  fn extend(&self, sequence: SequenceNumber) -> u64 {
    match self.maybe_expected_sequence {
      None => SEQUENCE_BASE + u64::from(sequence.0),
      Some(expected) => {
        // The low 16 bits of `expected` are its RTP sequence number.
        let offset = sequence.0.wrapping_sub(expected as u16) as i16;
        expected.saturating_add_signed(i64::from(offset))
      },
    }
  }

  /// Packets in `skipped` didn't come in order: they are waited for (and asked for). A gap too
  /// large to track is a loss at once.
  fn skipped(&mut self, skipped: std::ops::Range<u64>, now: Instant) {
    if skipped.is_empty() || self.missing.add(skipped.clone(), now) {
      return;
    }
    let oldest = self.missing.oldest().unwrap_or(skipped.start).min(skipped.start);
    let lost = self.missing.clear() + usize::try_from(skipped.end - skipped.start).unwrap_or(usize::MAX);
    self.lose(oldest, lost);
  }

  /// A keyframe starts at `sequence`: whatever is missing before it isn't needed any more (but
  /// lost all the same), and frames waiting for it are of no use.
  fn start_keyframe(&mut self, sequence: u64) {
    let lost = self.missing.remove_through(sequence.saturating_sub(1));
    self.counters.packets_lost += lost as u64;
    let before = self.held.len();
    self.held.retain(|held| held.first_sequence > sequence);
    self.counters.frames_discarded += (before - self.held.len()) as u64;
    self.maybe_loss_at = self.maybe_loss_at.filter(|loss| *loss > sequence);
  }

  /// `lost` packets, the oldest of them `oldest`, are given up on: frames from there on need a
  /// keyframe.
  fn lose(&mut self, oldest: u64, lost: usize) {
    self.counters.packets_lost += lost as u64;
    self.maybe_loss_at = Some(self.maybe_loss_at.map_or(oldest, |loss| loss.min(oldest)));
  }

  fn hold(&mut self, first_sequence: u64, timestamp: RtpTimestamp, frame: CompleteFrame) {
    let position = self
      .held
      .iter()
      .position(|held| held.first_sequence > first_sequence)
      .unwrap_or(self.held.len());
    self.held.insert(
      position,
      HeldFrame {
        first_sequence,
        timestamp,
        frame,
      },
    );
  }

  /// The held frames whose turn came: nothing before them is still waited for. Without
  /// retransmission (an older SFU), or with too many frames held, what is still missing before a
  /// complete frame is lost.
  fn release(&mut self) -> Vec<ReleasedFrame> {
    let mut released = Vec::new();
    loop {
      let maybe_first = self.held.front().map(|held| held.first_sequence);
      if let Some(loss) = self.maybe_loss_at
        && maybe_first.is_none_or(|first| first >= loss)
      {
        self.maybe_loss_at = None;
        self.is_loss = true;
        self.is_waiting_for_keyframe = true;
      }
      let Some(first) = maybe_first else {
        break;
      };
      if let Some(oldest) = self.missing.oldest()
        && oldest < first
      {
        if self.missing.asks_for_packets() && self.held.len() <= MAX_HELD_FRAMES {
          break;
        }
        let lost = self.missing.remove_through(first - 1);
        self.lose(oldest, lost);
        continue;
      }
      let Some(held) = self.held.pop_front() else {
        break;
      };
      self.assembler.release(held.frame.frame_number);
      if held.frame.is_keyframe {
        self.is_waiting_for_keyframe = false;
        // Whatever went missing before it doesn't matter any more.
        self.is_loss = false;
      }
      let is_usable = !self.is_waiting_for_keyframe;
      if !is_usable {
        // A delta whose reference went missing: the decoder can't use it.
        self.counters.frames_discarded += 1;
        self.is_loss = true;
      }
      released.push(ReleasedFrame {
        frame: held.frame,
        timestamp: held.timestamp,
        is_usable,
      });
    }
    released
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
    let mut incoming = IncomingVideo::new(&track(), None);
    let frame: Vec<u8> = (0..3000u32).map(|i| i as u8).collect();
    let low = SimulcastLayer::BASE;
    let high = SimulcastLayer::new(1).unwrap();
    let mut completed = Vec::new();
    let now = Instant::now();
    for (layer, is_keyframe) in [(low, true), (low, false), (high, true)] {
      let packets = outgoing
        .packetize(StreamId(7), layer, RtpTimestamp(9000), is_keyframe, &frame)
        .unwrap();
      assert_eq!(packets.len(), 3, "3000 bytes in 1100-byte fragments");
      // Out of order: reassembly doesn't depend on arrival order.
      for bytes in packets.iter().rev() {
        let packet = RtpPacket::parse(bytes).unwrap();
        assert_eq!(packet.header.payload_type, payload_type(&VideoCodec::Vp9));
        for released in incoming.push(layer, &packet, now).unwrap().frames {
          completed.push((released.frame.is_keyframe, released.frame.data));
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
    let mut incoming = IncomingVideo::new(&track(), None);
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
    let push =
      |incoming: &mut IncomingVideo, packets: &[Vec<u8>]| usability(incoming, packets, Instant::now());
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
      outgoing.count_sent(layer, bytes == 500, &packets);
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
    assert_eq!(counters.layers[0].keyframes, 1);
    assert_eq!(counters.layers[1].keyframes, 0);
    assert!(counters.layers[1].bytes > 4000, "with the headers");
    assert_eq!(
      counters.bytes,
      counters.layers.iter().map(|layer| layer.bytes).sum::<u64>()
    );
  }

  #[test]
  fn jitter_follows_the_variation_of_transit_times() {
    let mut steady = IncomingVideo::new(&track(), None);
    let mut shaky = IncomingVideo::new(&track(), None);
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
    let mut incoming = IncomingVideo::new(&track(), None);
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
      incoming
        .push(SimulcastLayer::BASE, &packet, Instant::now())
        .unwrap_err(),
      FragmentError::MissingExtension
    );
  }

  #[test]
  fn held_back_time_counts_while_the_top_layer_is_held() {
    let mut outgoing = OutgoingVideo::new(VideoTrackKind::Screen, VideoCodec::Av1);
    let start = Instant::now();
    let at = |ms: u64| start + Duration::from_millis(ms);
    outgoing.note_uplink(false, at(0));
    outgoing.note_uplink(true, at(100));
    outgoing.note_uplink(true, at(400));
    outgoing.note_uplink(false, at(1_000));
    outgoing.note_uplink(false, at(5_000));
    assert_eq!(outgoing.counters.held_back, Duration::from_millis(900));
  }

  #[test]
  fn the_longest_gap_between_used_frames_restarts_with_a_subscription() {
    let mut incoming = IncomingVideo::new(&track(), None);
    let start = Instant::now();
    let at = |ms: u64| start + Duration::from_millis(ms);
    for ms in [0, 33, 66, 1_566, 1_600] {
      incoming.note_used(at(ms));
    }
    assert_eq!(incoming.counters.longest_gap, Duration::from_millis(1_500));
    // Not received for a minute, then subscribed again: no gap.
    incoming.restart_gaps();
    incoming.note_used(at(61_600));
    assert_eq!(incoming.counters.longest_gap, Duration::from_millis(1_500));
    // Keyframe requests that go out are counted, rate-limited ones aren't.
    assert!(incoming.is_keyframe_request_due(at(0), Duration::from_secs(1)));
    assert!(!incoming.is_keyframe_request_due(at(500), Duration::from_secs(1)));
    assert!(incoming.is_keyframe_request_due(at(1_000), Duration::from_secs(1)));
    assert_eq!(incoming.counters.keyframe_requests, 2);
  }

  mod retransmission {
    use super::*;

    const RTT: Duration = Duration::from_millis(150);

    #[test]
    fn a_lost_packet_is_asked_for_and_its_frame_completes_in_order() {
      let (mut sender, mut incoming, now) = nacking();
      let keyframe = sender.frame(true, 3);
      let delta = sender.frame(false, 1);
      // The keyframe's middle packet is lost: the delta completes, but waits for it.
      assert!(usability(&mut incoming, &[keyframe[0].clone(), keyframe[2].clone()], now).is_empty());
      assert!(
        usability(&mut incoming, &delta, now).is_empty(),
        "held behind the keyframe"
      );
      let request = incoming.take_nack(now).unwrap();
      assert_eq!(request.layer, SimulcastLayer::BASE);
      assert_eq!(request.sequence_numbers, vec![1]);
      // The SFU resends it: both frames go on, the keyframe first.
      let later = now + RTT;
      let released = incoming
        .push(SimulcastLayer::BASE, &parse(&keyframe[1]), later)
        .unwrap();
      assert!(released.is_late);
      let frames: Vec<(bool, bool)> = released
        .frames
        .iter()
        .map(|released| (released.frame.is_keyframe, released.is_usable))
        .collect();
      assert_eq!(frames, vec![(true, true), (false, true)]);
      assert!(!incoming.take_loss());
      assert_eq!(incoming.counters.packets_recovered, 1);
      assert_eq!(incoming.counters.packets_lost, 0);
    }

    #[test]
    fn a_packet_that_never_comes_ends_in_a_keyframe_request() {
      let (mut sender, mut incoming, now) = nacking();
      let keyframe = sender.frame(true, 1);
      let lost = sender.frame(false, 1);
      let delta = sender.frame(false, 1);
      assert_eq!(usability(&mut incoming, &keyframe, now), vec![true]);
      drop(lost);
      assert!(usability(&mut incoming, &delta, now).is_empty());
      let mut at = now;
      let mut requests = 0;
      let mut released = Vec::new();
      while released.is_empty() && at < now + Duration::from_secs(2) {
        requests += incoming
          .take_nack(at)
          .map_or(0, |request| request.sequence_numbers.len());
        released = incoming.on_timer(at);
        at += Duration::from_millis(100);
      }
      assert_eq!(requests, 3, "asked for a few times");
      assert!(
        at <= now + Duration::from_secs(1),
        "given up on within the window"
      );
      // The delta behind it can't be used; a keyframe is needed.
      assert_eq!(released.len(), 1);
      assert!(!released[0].is_usable);
      assert!(incoming.take_loss());
      assert_eq!(incoming.counters.packets_lost, 1);
    }

    #[test]
    fn the_first_packets_of_a_layer_are_known_from_the_fragment_index() {
      let (mut sender, mut incoming, now) = nacking();
      let keyframe = sender.frame(true, 4);
      // The first two packets of the keyframe the SFU switched this receiver to are lost.
      assert!(usability(&mut incoming, &keyframe[2..], now).is_empty());
      assert_eq!(incoming.take_nack(now).unwrap().sequence_numbers, vec![0, 1]);
      assert!(
        usability(&mut incoming, &keyframe[..2], now + RTT)
          .first()
          .is_some_and(|usable| *usable)
      );
    }

    #[test]
    fn the_lost_end_of_the_last_frame_is_asked_for_once_the_layer_is_quiet() {
      let (mut sender, mut incoming, now) = nacking();
      let keyframe = sender.frame(true, 3);
      assert!(usability(&mut incoming, &keyframe[..1], now).is_empty());
      assert_eq!(incoming.take_nack(now), None, "the rest may be on its way");
      assert!(incoming.on_timer(now + Duration::from_millis(10)).is_empty());
      assert_eq!(incoming.take_nack(now + Duration::from_millis(10)), None);
      let quiet = now + Duration::from_millis(100);
      incoming.on_timer(quiet);
      assert_eq!(incoming.take_nack(quiet).unwrap().sequence_numbers, vec![1, 2]);
    }

    #[test]
    fn duplicates_are_dropped() {
      let (mut sender, mut incoming, now) = nacking();
      let keyframe = sender.frame(true, 2);
      assert_eq!(usability(&mut incoming, &keyframe, now), vec![true]);
      let again = incoming
        .push(SimulcastLayer::BASE, &parse(&keyframe[1]), now)
        .unwrap();
      assert!(again.frames.is_empty());
      assert_eq!(incoming.counters.packets_recovered, 0);
    }

    #[test]
    fn a_keyframe_ends_the_wait_for_older_packets() {
      let (mut sender, mut incoming, now) = nacking();
      let keyframe = sender.frame(true, 1);
      let lost = sender.frame(false, 2);
      let next_keyframe = sender.frame(true, 1);
      assert_eq!(usability(&mut incoming, &keyframe, now), vec![true]);
      assert_eq!(usability(&mut incoming, &lost[..1], now), Vec::<bool>::new());
      assert_eq!(usability(&mut incoming, &next_keyframe, now), vec![true]);
      assert_eq!(
        incoming.take_nack(now),
        None,
        "nothing before the keyframe is needed"
      );
      assert!(!incoming.take_loss());
      assert_eq!(incoming.counters.packets_lost, 1);
    }

    #[test]
    fn a_numbering_that_starts_over_starts_reception_over() {
      let (mut sender, mut incoming, now) = nacking();
      assert_eq!(usability(&mut incoming, &sender.frame(true, 1), now), vec![true]);
      sender.outgoing.sequences[0] = SequenceNumber(60_000);
      assert_eq!(usability(&mut incoming, &sender.frame(true, 1), now), vec![true]);
      assert_eq!(incoming.take_nack(now), None);
    }

    /// A sender on layer 0 of stream 7, and a receiver that asks for missing packets.
    struct Sender {
      outgoing: OutgoingVideo,
    }

    impl Sender {
      /// The packets of a frame of `packets` fragments.
      fn frame(&mut self, is_keyframe: bool, packets: usize) -> Vec<Vec<u8>> {
        let bytes = vec![7; MAX_FRAGMENT_PAYLOAD_BYTES * packets];
        self
          .outgoing
          .packetize(
            StreamId(7),
            SimulcastLayer::BASE,
            RtpTimestamp(0),
            is_keyframe,
            &bytes,
          )
          .unwrap()
      }
    }

    fn nacking() -> (Sender, IncomingVideo, Instant) {
      let sender = Sender {
        outgoing: OutgoingVideo::new(VideoTrackKind::Screen, VideoCodec::Vp9),
      };
      let config = NackConfig::for_window(Duration::from_secs(1));
      (sender, IncomingVideo::new(&track(), Some(config)), Instant::now())
    }
  }

  /// Pushes `packets` at `now`; returns whether each frame released can be used.
  fn usability(incoming: &mut IncomingVideo, packets: &[Vec<u8>], now: Instant) -> Vec<bool> {
    packets
      .iter()
      .flat_map(|bytes| {
        incoming
          .push(SimulcastLayer::BASE, &parse(bytes), now)
          .unwrap()
          .frames
      })
      .map(|released| released.is_usable)
      .collect()
  }

  fn parse(bytes: &[u8]) -> RtpPacket<'_> {
    RtpPacket::parse(bytes).unwrap()
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
