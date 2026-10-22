//! Per-room forwarding decisions, kept free of network types so they are tested directly.
//!
//! - **Audio:** accepted only on the sender's assigned stream; its audio level (RFC 6464, unencrypted)
//!   feeds the speaker selection, and only selected speakers reach the others.
//! - **Video:** published tracks with simulcast layers; each receiver gets one layer per subscribed
//!   track. The layer comes from the receiver's downlink estimate ([`LayerAllocator`]) and a switch
//!   happens on a keyframe of the target layer (the SFU asks the publisher for one).
//! - **Bandwidth estimation:** every forwarded datagram gets a per-receiver transport sequence
//!   number; the receiver's feedback drives a [`SendSideEstimator`]. Incoming datagrams with a
//!   transport sequence number are recorded for the sender's own (uplink) estimation.
//!
//! The payload is never read; DAVE frames stay encrypted.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use teamcraft_media::{
  bwe::{BweConfig, FeedbackRecorder, SendSideEstimator},
  datagram::{DatagramError, DatagramHeader, StreamId},
  fragment::{FragmentError, FrameFragment},
  rtp::{AudioLevel, RtpError, RtpHeader},
};
use teamcraft_proto::{
  DeviceId, MAX_SIMULCAST_LAYERS, MediaCapabilities, MediaStreamId, TransportFeedback, VideoCodec,
  VideoLayerInfo, VideoTrack, VideoTrackKind,
};
use thiserror::Error;

use crate::layers::{LayerAllocationConfig, LayerAllocator, TrackDemand, full_demand_bps};
use crate::speakers::{SpeakerSelectionConfig, SpeakerSelector};

/// Keyframe requests per track and layer are sent at most this often.
const KEYFRAME_REQUEST_INTERVAL: Duration = Duration::from_millis(500);

/// Routing state of one room.
#[derive(Debug)]
pub struct Forwarder {
  config: ForwardingConfig,
  participants: HashMap<DeviceId, ParticipantRoute>,
  video_tracks: BTreeMap<StreamId, VideoTrackRoute>,
  speakers: SpeakerSelector,
  last_keyframe_requests: HashMap<(StreamId, u8), Instant>,
}

/// Settings of the forwarding.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ForwardingConfig {
  pub speakers: SpeakerSelectionConfig,
  pub layers: LayerAllocationConfig,
  pub bwe: BweConfig,
}

/// What to do with one incoming datagram.
#[derive(Debug, Default)]
pub struct Route {
  /// Datagrams to send, each restamped for its receiver.
  pub deliveries: Vec<(DeviceId, Vec<u8>)>,
  pub effects: Vec<Effect>,
}

/// Signaling the room has to send because of a forwarding decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
  /// The set of forwarded speakers changed (to everyone).
  ActiveSpeakers(Vec<DeviceId>),
  /// A receiver switches to `layer`; the publisher should send a keyframe there.
  KeyframeRequest {
    publisher: DeviceId,
    stream_id: StreamId,
    layer: u8,
  },
  /// The layers someone receives changed; the publisher may pause the others.
  LayersNeeded {
    publisher: DeviceId,
    stream_id: StreamId,
    layers: Vec<u8>,
  },
}

#[derive(Debug)]
struct ParticipantRoute {
  audio_stream: StreamId,
  capabilities: MediaCapabilities,
  downlink: SendSideEstimator,
  uplink: FeedbackRecorder,
  /// Base for the receiver's arrival times in its feedback (only differences matter).
  feedback_epoch: Instant,
  allocator: LayerAllocator,
}

#[derive(Debug)]
struct VideoTrackRoute {
  track: VideoTrack,
  subscribers: BTreeMap<DeviceId, VideoSubscription>,
  layers_needed: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
struct VideoSubscription {
  max_layer: u8,
  /// Layer the allocation wants, `None` = paused.
  target: Option<u8>,
  /// Layer being forwarded; changes to `target` on a keyframe.
  current: Option<u8>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ForwardError {
  #[error("sender is not in the room")]
  NotJoined,
  #[error("stream {0:?} does not belong to the sender")]
  ForeignStream(StreamId),
  #[error("unknown video track {0:?}")]
  UnknownTrack(StreamId),
  #[error("invalid video publication: {0}")]
  InvalidPublication(&'static str),
  #[error("the receiver can't decode {0:?}")]
  UnsupportedCodec(VideoCodec),
  #[error("can't subscribe to your own track")]
  OwnTrack,
  #[error("invalid datagram: {0}")]
  Datagram(#[from] DatagramError),
  #[error("invalid RTP header: {0}")]
  Rtp(#[from] RtpError),
  #[error("invalid video fragment: {0}")]
  Fragment(#[from] FragmentError),
}

impl Forwarder {
  pub fn new(config: ForwardingConfig) -> Self {
    Self {
      config,
      participants: HashMap::new(),
      video_tracks: BTreeMap::new(),
      speakers: SpeakerSelector::new(config.speakers),
      last_keyframe_requests: HashMap::new(),
    }
  }

  pub fn add_participant(
    &mut self,
    device_id: DeviceId,
    audio_stream: StreamId,
    capabilities: MediaCapabilities,
    now: Instant,
  ) {
    let route = ParticipantRoute {
      audio_stream,
      capabilities,
      downlink: SendSideEstimator::new(self.config.bwe),
      uplink: FeedbackRecorder::new(now),
      feedback_epoch: now,
      allocator: LayerAllocator::new(),
    };
    self.participants.insert(device_id, route);
  }

  /// Removes a participant with its tracks and subscriptions. Returns the effects and the
  /// unpublished tracks.
  pub fn remove_participant(&mut self, device_id: DeviceId, now: Instant) -> (Vec<Effect>, Vec<StreamId>) {
    self.participants.remove(&device_id);
    let mut effects = Vec::new();
    if let Some(speakers) = self.speakers.remove(device_id, now) {
      effects.push(Effect::ActiveSpeakers(speakers));
    }
    let owned: Vec<StreamId> = self
      .video_tracks
      .iter()
      .filter(|(_, route)| route.track.device_id == device_id)
      .map(|(&stream_id, _)| stream_id)
      .collect();
    for stream_id in &owned {
      self.video_tracks.remove(stream_id);
    }
    let subscribed: Vec<StreamId> = self
      .video_tracks
      .iter_mut()
      .filter_map(|(&stream_id, route)| route.subscribers.remove(&device_id).map(|_| stream_id))
      .collect();
    for stream_id in subscribed {
      effects.extend(self.update_layers_needed(stream_id));
    }
    (effects, owned)
  }

  pub fn publish_video(
    &mut self,
    owner: DeviceId,
    stream_id: StreamId,
    kind: VideoTrackKind,
    codec: VideoCodec,
    layers: Vec<VideoLayerInfo>,
  ) -> Result<VideoTrack, ForwardError> {
    let participant = self.participants.get(&owner).ok_or(ForwardError::NotJoined)?;
    if !participant.capabilities.video_encoders.contains(&codec) {
      return Err(ForwardError::InvalidPublication(
        "codec not in the publisher's encoders",
      ));
    }
    if layers.is_empty() || layers.len() > usize::from(MAX_SIMULCAST_LAYERS) {
      return Err(ForwardError::InvalidPublication("needs 1 to 3 layers"));
    }
    if layers
      .iter()
      .zip(0u8..)
      .any(|(info, expected)| info.layer != expected)
    {
      return Err(ForwardError::InvalidPublication(
        "layers must be numbered 0, 1, 2 in order",
      ));
    }
    let track = VideoTrack {
      stream_id: MediaStreamId(stream_id.0),
      device_id: owner,
      kind,
      codec,
      layers,
    };
    let route = VideoTrackRoute {
      track: track.clone(),
      subscribers: BTreeMap::new(),
      layers_needed: Vec::new(),
    };
    self.video_tracks.insert(stream_id, route);
    Ok(track)
  }

  pub fn unpublish_video(&mut self, owner: DeviceId, stream_id: StreamId) -> Result<(), ForwardError> {
    let route = self
      .video_tracks
      .get(&stream_id)
      .ok_or(ForwardError::UnknownTrack(stream_id))?;
    if route.track.device_id != owner {
      return Err(ForwardError::ForeignStream(stream_id));
    }
    self.video_tracks.remove(&stream_id);
    Ok(())
  }

  /// Starts, changes or (with `None`) stops receiving a video track.
  pub fn subscribe(
    &mut self,
    receiver: DeviceId,
    stream_id: StreamId,
    maybe_max_layer: Option<u8>,
    now: Instant,
  ) -> Result<Vec<Effect>, ForwardError> {
    let participant = self.participants.get(&receiver).ok_or(ForwardError::NotJoined)?;
    let route = self
      .video_tracks
      .get_mut(&stream_id)
      .ok_or(ForwardError::UnknownTrack(stream_id))?;
    if route.track.device_id == receiver {
      return Err(ForwardError::OwnTrack);
    }
    if !participant
      .capabilities
      .video_decoders
      .contains(&route.track.codec)
    {
      return Err(ForwardError::UnsupportedCodec(route.track.codec.clone()));
    }
    match maybe_max_layer {
      Some(max_layer) => {
        let subscription = route.subscribers.entry(receiver).or_insert(VideoSubscription {
          max_layer,
          target: None,
          current: None,
        });
        subscription.max_layer = max_layer;
      },
      None => {
        route.subscribers.remove(&receiver);
      },
    }
    let mut effects = self.reallocate(receiver, now);
    effects.extend(self.update_layers_needed(stream_id));
    Ok(effects)
  }

  /// Decides where one datagram from `sender` goes.
  pub fn route(&mut self, sender: DeviceId, datagram: &[u8], now: Instant) -> Result<Route, ForwardError> {
    let header = DatagramHeader::parse(datagram)?;
    let participant = self
      .participants
      .get_mut(&sender)
      .ok_or(ForwardError::NotJoined)?;
    let is_audio = header.stream_id == participant.audio_stream;
    if !is_audio
      && self
        .video_tracks
        .get(&header.stream_id)
        .is_none_or(|route| route.track.device_id != sender)
    {
      return Err(ForwardError::ForeignStream(header.stream_id));
    }
    let (rtp_header, _) = RtpHeader::parse(&datagram[header.encoded_len()..])?;
    if let Some(seq) = header.maybe_transport_seq {
      participant.uplink.on_arrival(seq, now);
    }

    let mut route = Route::default();
    let receivers = if is_audio {
      let level = rtp_header.audio_level().unwrap_or(AudioLevel::SILENCE);
      if let Some(speakers) = self.speakers.record(sender, level, now) {
        route.effects.push(Effect::ActiveSpeakers(speakers));
      }
      if self.speakers.is_selected(sender) {
        self
          .participants
          .keys()
          .copied()
          .filter(|id| *id != sender)
          .collect()
      } else {
        Vec::new()
      }
    } else {
      self.video_receivers(header.stream_id, header.layer.value(), &rtp_header)?
    };
    for receiver in receivers {
      if let Some(target) = self.participants.get_mut(&receiver) {
        let seq = target.downlink.on_send(datagram.len() + 2, now);
        route
          .deliveries
          .push((receiver, DatagramHeader::restamp(datagram, seq)?));
      }
    }
    Ok(route)
  }

  /// Applies a receiver's transport feedback and re-runs its layer allocation.
  pub fn on_transport_feedback(
    &mut self,
    receiver: DeviceId,
    feedback: &TransportFeedback,
    now: Instant,
  ) -> Vec<Effect> {
    let Some(participant) = self.participants.get_mut(&receiver) else {
      return Vec::new();
    };
    participant
      .downlink
      .on_feedback(feedback, now, participant.feedback_epoch);
    self.on_timer(receiver, now)
  }

  /// Re-runs a receiver's layer allocation; called periodically (every ~100 ms) so paused tracks
  /// get their resume probe even when no feedback arrives.
  pub fn on_timer(&mut self, receiver: DeviceId, now: Instant) -> Vec<Effect> {
    let mut effects = self.reallocate(receiver, now);
    let streams: Vec<StreamId> = self
      .video_tracks
      .iter()
      .filter(|(_, route)| route.subscribers.contains_key(&receiver))
      .map(|(&stream_id, _)| stream_id)
      .collect();
    for stream_id in streams {
      effects.extend(self.update_layers_needed(stream_id));
    }
    effects
  }

  /// Feedback for the datagrams `sender` sent since the last call (its uplink estimation).
  pub fn take_uplink_feedback(&mut self, sender: DeviceId) -> Option<TransportFeedback> {
    self.participants.get_mut(&sender)?.uplink.take_feedback()
  }

  /// The current speaker set, loudest first.
  pub fn active_speakers(&self) -> &[DeviceId] {
    self.speakers.selected()
  }

  /// Published video tracks.
  pub fn video_tracks(&self) -> Vec<VideoTrack> {
    self
      .video_tracks
      .values()
      .map(|route| route.track.clone())
      .collect()
  }

  /// Current downlink estimate of a receiver.
  pub fn downlink_estimate_bps(&self, receiver: DeviceId) -> Option<u32> {
    self
      .participants
      .get(&receiver)
      .map(|participant| participant.downlink.estimate_bps())
  }

  /// Receivers of one video packet; switches subscriptions to their target layer on a keyframe.
  fn video_receivers(
    &mut self,
    stream_id: StreamId,
    layer: u8,
    rtp_header: &RtpHeader,
  ) -> Result<Vec<DeviceId>, ForwardError> {
    let fragment = FrameFragment::from_header(rtp_header)?;
    let starts_keyframe = fragment.is_keyframe && fragment.index == 0;
    let Some(route) = self.video_tracks.get_mut(&stream_id) else {
      return Ok(Vec::new());
    };
    let mut receivers = Vec::new();
    for (&receiver, subscription) in &mut route.subscribers {
      if subscription.target == Some(layer) && subscription.current != Some(layer) && starts_keyframe {
        subscription.current = Some(layer);
      }
      if subscription.current == Some(layer) {
        receivers.push(receiver);
      }
    }
    Ok(receivers)
  }

  /// Recomputes a receiver's layers; asks publishers for keyframes where a receiver switches.
  fn reallocate(&mut self, receiver: DeviceId, now: Instant) -> Vec<Effect> {
    let Some(participant) = self.participants.get_mut(&receiver) else {
      return Vec::new();
    };
    // Without any feedback yet (older clients) the estimate is just the start value; it still
    // grants the lowest layers.
    let estimate = participant.downlink.estimate_bps();
    let speakers = self.speakers.selected();
    let demands: Vec<TrackDemand<'_>> = self
      .video_tracks
      .iter()
      .filter_map(|(&stream_id, route)| {
        let subscription = route.subscribers.get(&receiver)?;
        let is_screen = route.track.kind == VideoTrackKind::Screen;
        let priority = match route.track.kind {
          VideoTrackKind::Screen => 0,
          _ if speakers.contains(&route.track.device_id) => 1,
          _ => 2,
        };
        Some(TrackDemand {
          stream_id,
          priority,
          max_layer: subscription.max_layer,
          layers: &route.track.layers,
          can_pause: !is_screen || self.config.layers.can_pause_screen_shares,
        })
      })
      .collect();
    let maybe_demand = (!demands.is_empty()).then(|| full_demand_bps(&self.config.layers, &demands));
    participant.downlink.set_demand_bps(maybe_demand);
    let allocation = participant
      .allocator
      .allocate(&self.config.layers, estimate, &demands, now);

    let mut effects = Vec::new();
    for (stream_id, target) in allocation {
      let Some(route) = self.video_tracks.get_mut(&stream_id) else {
        continue;
      };
      let Some(subscription) = route.subscribers.get_mut(&receiver) else {
        continue;
      };
      subscription.target = target;
      match target {
        None => subscription.current = None,
        Some(layer) if subscription.current != Some(layer) => {
          let key = (stream_id, layer);
          let is_due = self
            .last_keyframe_requests
            .get(&key)
            .is_none_or(|last| now.saturating_duration_since(*last) >= KEYFRAME_REQUEST_INTERVAL);
          if is_due {
            self.last_keyframe_requests.insert(key, now);
            effects.push(Effect::KeyframeRequest {
              publisher: route.track.device_id,
              stream_id,
              layer,
            });
          }
        },
        Some(_) => {},
      }
    }
    effects
  }

  fn update_layers_needed(&mut self, stream_id: StreamId) -> Option<Effect> {
    let route = self.video_tracks.get_mut(&stream_id)?;
    let mut needed: Vec<u8> = route
      .subscribers
      .values()
      .flat_map(|subscription| [subscription.target, subscription.current])
      .flatten()
      .collect();
    needed.sort_unstable();
    needed.dedup();
    if needed == route.layers_needed {
      return None;
    }
    route.layers_needed = needed.clone();
    Some(Effect::LayersNeeded {
      publisher: route.track.device_id,
      stream_id,
      layers: needed,
    })
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_media::{
    datagram::{DATAGRAM_HEADER_BYTES, MediaDatagram, SimulcastLayer, TransportSeq},
    fragment::fragment_frame,
    rtp::{PayloadType, RtpPacket, RtpTimestamp, SequenceNumber, Ssrc},
  };

  use super::*;

  const ENCRYPTED_FRAME: &[u8] = &[0xAB; 40];
  /// Video track of device 1 in the tests.
  const VIDEO: StreamId = StreamId(100);

  mod audio {
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
          if round == 19 && !route.deliveries.is_empty() {
            assert_eq!(route.deliveries.len(), 4, "to everyone else");
            forwarded.push(n);
          }
        }
      }
      assert_eq!(forwarded, vec![1, 2, 3]);
      assert_eq!(forwarder.active_speakers(), devices(&[1, 2, 3]));
    }

    #[test]
    fn missing_audio_level_counts_as_silence() {
      let (mut forwarder, now) = room(2);
      let route = forwarder
        .route(device(1), &rtp_datagram(1, 0, header()), now)
        .unwrap();
      assert!(route.deliveries.is_empty());
      assert!(route.effects.is_empty());
    }

    #[test]
    fn speaker_changes_are_reported_once() {
      let (mut forwarder, now) = room(2);
      let first = forwarder.route(device(1), &audio(1, 20), now).unwrap();
      assert_eq!(first.effects, vec![Effect::ActiveSpeakers(devices(&[1]))]);
      let again = forwarder
        .route(device(1), &audio(1, 20), now + Duration::from_millis(200))
        .unwrap();
      assert!(again.effects.is_empty());
      let (effects, _) = forwarder.remove_participant(device(1), now + Duration::from_millis(300));
      assert_eq!(effects, vec![Effect::ActiveSpeakers(Vec::new())]);
    }

    #[test]
    fn deliveries_are_numbered_per_receiver() {
      let (mut forwarder, now) = room(3);
      for expected in 0..3u16 {
        let route = forwarder.route(device(1), &audio(1, 20), now).unwrap();
        for (_, datagram) in &route.deliveries {
          let header = DatagramHeader::parse(datagram).unwrap();
          assert_eq!(header.maybe_transport_seq, Some(TransportSeq(expected)));
        }
      }
    }
  }

  mod video {
    use super::*;

    #[test]
    fn switch_happens_on_a_keyframe_of_the_target_layer() {
      let (mut forwarder, now) = video_room();
      let effects = forwarder.subscribe(device(2), VIDEO, Some(2), now).unwrap();
      // Start estimate (300k) minus the audio reserve only fits layer 0.
      assert!(effects.contains(&Effect::KeyframeRequest {
        publisher: device(1),
        stream_id: VIDEO,
        layer: 0,
      }));
      assert!(effects.contains(&Effect::LayersNeeded {
        publisher: device(1),
        stream_id: VIDEO,
        layers: vec![0],
      }));
      // Delta frames of layer 0 before the keyframe are not forwarded (undecodable).
      assert!(send_frame(&mut forwarder, 0, false, now).is_empty());
      assert_eq!(send_frame(&mut forwarder, 0, true, now), devices(&[2]));
      assert_eq!(send_frame(&mut forwarder, 0, false, now), devices(&[2]));
      // Other layers never reach this receiver.
      assert!(send_frame(&mut forwarder, 1, true, now).is_empty());
      assert!(send_frame(&mut forwarder, 2, true, now).is_empty());
    }

    #[test]
    fn unsubscribe_stops_forwarding() {
      let (mut forwarder, now) = video_room();
      forwarder.subscribe(device(2), VIDEO, Some(0), now).unwrap();
      send_frame(&mut forwarder, 0, true, now);
      let effects = forwarder.subscribe(device(2), VIDEO, None, now).unwrap();
      assert!(effects.contains(&Effect::LayersNeeded {
        publisher: device(1),
        stream_id: VIDEO,
        layers: Vec::new(),
      }));
      assert!(send_frame(&mut forwarder, 0, false, now).is_empty());
    }

    #[test]
    fn publisher_leaving_removes_the_track() {
      let (mut forwarder, now) = video_room();
      let (_, removed) = forwarder.remove_participant(device(1), now);
      assert_eq!(removed, vec![VIDEO]);
      assert!(forwarder.video_tracks().is_empty());
    }
  }

  mod rejection {
    use super::*;

    #[test]
    fn foreign_and_unknown_streams_are_rejected() {
      let (mut forwarder, now) = room(2);
      assert_eq!(
        forwarder.route(device(1), &audio(2, 10), now).unwrap_err(),
        ForwardError::ForeignStream(StreamId(2))
      );
      assert_eq!(
        forwarder.route(device(9), &audio(1, 10), now).unwrap_err(),
        ForwardError::NotJoined
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

    #[test]
    fn invalid_publications_and_subscriptions() {
      let (mut forwarder, now) = video_room();
      let layers = three_layers();
      assert!(matches!(
        forwarder.publish_video(
          device(2),
          StreamId(7),
          VideoTrackKind::Camera,
          VideoCodec::H264,
          layers.clone()
        ),
        Err(ForwardError::InvalidPublication(_))
      ));
      assert!(matches!(
        forwarder.publish_video(
          device(1),
          StreamId(7),
          VideoTrackKind::Camera,
          VideoCodec::Av1,
          layers[1..].to_vec()
        ),
        Err(ForwardError::InvalidPublication(_))
      ));
      assert_eq!(
        forwarder.subscribe(device(1), VIDEO, Some(1), now),
        Err(ForwardError::OwnTrack)
      );
      assert_eq!(
        forwarder.subscribe(device(3), VIDEO, Some(1), now),
        Err(ForwardError::UnsupportedCodec(VideoCodec::Av1))
      );
      assert_eq!(
        forwarder.subscribe(device(2), StreamId(55), Some(1), now),
        Err(ForwardError::UnknownTrack(StreamId(55)))
      );
      assert_eq!(
        forwarder.unpublish_video(device(2), VIDEO),
        Err(ForwardError::ForeignStream(VIDEO))
      );
    }

    #[test]
    fn video_without_fragment_info_is_rejected() {
      let (mut forwarder, now) = video_room();
      forwarder.subscribe(device(2), VIDEO, Some(0), now).unwrap();
      let datagram = rtp_datagram(VIDEO.0, 0, header());
      assert!(matches!(
        forwarder.route(device(1), &datagram, now),
        Err(ForwardError::Fragment(FragmentError::MissingExtension))
      ));
    }
  }

  mod bandwidth {
    use super::*;

    #[test]
    fn layer_follows_the_receivers_link_capacity() {
      // Layers of 150k / 500k / 1.5M; 120k audio reserve. 900 kbit/s fits layer 1.
      assert_eq!(settled_layer(400_000), Some(0));
      assert_eq!(settled_layer(900_000), Some(1));
      assert_eq!(settled_layer(3_000_000), Some(2));
    }

    #[test]
    fn uplink_arrivals_become_feedback_for_the_sender() {
      let (mut forwarder, now) = room(2);
      for seq in [0u16, 1, 3] {
        let datagram = stamped(audio(1, 20), seq);
        forwarder.route(device(1), &datagram, now).unwrap();
      }
      let feedback = forwarder.take_uplink_feedback(device(1)).unwrap();
      assert_eq!(feedback.base_seq, 0);
      assert_eq!(
        feedback
          .arrivals_us
          .iter()
          .map(Option::is_some)
          .collect::<Vec<_>>(),
        vec![true, true, false, true]
      );
      assert_eq!(forwarder.take_uplink_feedback(device(1)), None);
    }

    /// Runs a publisher sending all three layers at their bitrates and a receiver behind a link of
    /// `capacity_bps` that reports feedback every 100 ms; returns the receiver's dominant layer in
    /// the last 20 of 60 s (the estimate ramps up at 8 %/s from 300 kbit/s).
    fn settled_layer(capacity_bps: u64) -> Option<u8> {
      let (mut forwarder, start) = video_room();
      forwarder.subscribe(device(2), VIDEO, Some(2), start).unwrap();
      let mut link = Link::new(capacity_bps);
      let mut recorder = FeedbackRecorder::new(start);
      let mut in_flight: Vec<(Instant, TransportSeq)> = Vec::new();
      let mut frame_number = 0u16;
      let mut now = start;
      let end = start + Duration::from_secs(60);
      let mut next_feedback = start + Duration::from_millis(100);
      let mut layers_seen: Vec<u8> = Vec::new();
      while now < end {
        // 30 fps; a keyframe every second so layer switches can happen.
        let is_keyframe = frame_number % 30 == 0;
        for info in three_layers() {
          let frame_bytes = (info.max_bitrate_bps / 8 / 30) as usize;
          for (fragment, payload) in
            fragment_frame(&vec![0; frame_bytes], frame_number, is_keyframe, 1000).unwrap()
          {
            let mut rtp = header();
            fragment.write_to(&mut rtp);
            let packet = RtpPacket { header: rtp, payload }.to_bytes().unwrap();
            let datagram = MediaDatagram {
              header: DatagramHeader {
                stream_id: VIDEO,
                layer: SimulcastLayer::new(info.layer).unwrap(),
                maybe_transport_seq: None,
              },
              rtp: &packet,
            }
            .to_bytes(1200)
            .unwrap();
            for (receiver, delivered) in forwarder.route(device(1), &datagram, now).unwrap().deliveries {
              assert_eq!(receiver, device(2));
              let parsed = DatagramHeader::parse(&delivered).unwrap();
              if now >= end - Duration::from_secs(20) {
                layers_seen.push(parsed.layer.value());
              }
              if let (Some(arrival), Some(seq)) =
                (link.send(delivered.len(), now), parsed.maybe_transport_seq)
              {
                in_flight.push((arrival, seq));
              }
            }
          }
        }
        now += Duration::from_millis(33);
        frame_number = frame_number.wrapping_add(1);
        in_flight.retain(|(arrival, seq)| {
          if *arrival <= now {
            recorder.on_arrival(*seq, *arrival);
            false
          } else {
            true
          }
        });
        if now >= next_feedback {
          next_feedback += Duration::from_millis(100);
          match recorder.take_feedback() {
            Some(feedback) => forwarder.on_transport_feedback(device(2), &feedback, now),
            None => forwarder.on_timer(device(2), now),
          };
        }
      }
      // The layer most datagrams arrived on in the last 20 s (probes upward are brief).
      if layers_seen.is_empty() {
        return None;
      }
      (0..3u8)
        .max_by_key(|layer| layers_seen.iter().filter(|seen| *seen == layer).count() / layer_weight(*layer))
    }

    /// Datagrams per frame grow with the layer's bitrate; normalize so time spent counts, not bytes.
    fn layer_weight(layer: u8) -> usize {
      match layer {
        0 => 1,
        1 => 3,
        _ => 10,
      }
    }
  }

  /// A downlink bottleneck: capacity, 30 ms delay, 300 ms drop-tail queue.
  struct Link {
    capacity_bps: u64,
    maybe_free_at: Option<Instant>,
  }

  impl Link {
    fn new(capacity_bps: u64) -> Self {
      Self {
        capacity_bps,
        maybe_free_at: None,
      }
    }

    fn send(&mut self, bytes: usize, now: Instant) -> Option<Instant> {
      let start = self.maybe_free_at.map_or(now, |free| free.max(now));
      if start.saturating_duration_since(now) > Duration::from_millis(300) {
        return None;
      }
      let free = start + Duration::from_secs_f64(bytes as f64 * 8.0 / self.capacity_bps as f64);
      self.maybe_free_at = Some(free);
      Some(free + Duration::from_millis(30))
    }
  }

  /// A room with devices 1..=n on audio streams 1..=n; all decode AV1 and VP9, device 1 encodes them.
  fn room(participants: u64) -> (Forwarder, Instant) {
    let now = Instant::now();
    let mut forwarder = Forwarder::new(ForwardingConfig::default());
    for n in 1..=participants {
      let capabilities = MediaCapabilities {
        video_encoders: vec![VideoCodec::Av1, VideoCodec::Vp9],
        video_decoders: if n == 3 {
          vec![VideoCodec::Vp9]
        } else {
          vec![VideoCodec::Av1, VideoCodec::Vp9]
        },
        max_simulcast_layers: 3,
        ..MediaCapabilities::default()
      };
      forwarder.add_participant(device(n), stream(n), capabilities, now);
    }
    (forwarder, now)
  }

  /// Three participants; device 1 publishes an AV1 camera track with three layers on [`VIDEO`].
  fn video_room() -> (Forwarder, Instant) {
    let (mut forwarder, now) = room(3);
    forwarder
      .publish_video(
        device(1),
        VIDEO,
        VideoTrackKind::Camera,
        VideoCodec::Av1,
        three_layers(),
      )
      .unwrap();
    (forwarder, now)
  }

  /// Sends one single-fragment frame on `layer` of [`VIDEO`]; returns who receives it.
  fn send_frame(forwarder: &mut Forwarder, layer: u8, is_keyframe: bool, now: Instant) -> Vec<DeviceId> {
    let mut rtp = header();
    FrameFragment {
      frame_number: 1,
      index: 0,
      count: 1,
      is_keyframe,
    }
    .write_to(&mut rtp);
    let route = forwarder
      .route(device(1), &rtp_datagram(VIDEO.0, layer, rtp), now)
      .unwrap();
    route
      .deliveries
      .into_iter()
      .map(|(receiver, _)| receiver)
      .collect()
  }

  fn three_layers() -> Vec<VideoLayerInfo> {
    [
      (0, 320, 180, 150_000),
      (1, 640, 360, 500_000),
      (2, 1280, 720, 1_500_000),
    ]
    .into_iter()
    .map(|(layer, width, height, max_bitrate_bps)| VideoLayerInfo {
      layer,
      width,
      height,
      max_framerate: 30,
      max_bitrate_bps,
    })
    .collect()
  }

  fn audio(stream_number: u64, level_dbov: u8) -> Vec<u8> {
    let mut rtp = header();
    rtp.set_audio_level(AudioLevel::new(level_dbov, true).unwrap());
    rtp_datagram(u32::try_from(stream_number).unwrap(), 0, rtp)
  }

  fn rtp_datagram(stream_id: u32, layer: u8, rtp: RtpHeader) -> Vec<u8> {
    let packet = RtpPacket {
      header: rtp,
      payload: ENCRYPTED_FRAME,
    }
    .to_bytes()
    .unwrap();
    MediaDatagram {
      header: DatagramHeader {
        stream_id: StreamId(stream_id),
        layer: SimulcastLayer::new(layer).unwrap(),
        maybe_transport_seq: None,
      },
      rtp: &packet,
    }
    .to_bytes(1200)
    .unwrap()
  }

  fn stamped(datagram: Vec<u8>, seq: u16) -> Vec<u8> {
    DatagramHeader::restamp(&datagram, TransportSeq(seq)).unwrap()
  }

  fn header() -> RtpHeader {
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
