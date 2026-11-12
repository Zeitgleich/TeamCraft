//! Per-room forwarding decisions, kept free of network types so they are tested directly.
//!
//! - **Audio:** accepted only on the sender's assigned stream; its audio level (RFC 6464, unencrypted)
//!   feeds the speaker selection, and only selected speakers reach the others.
//! - **Screen audio:** a participant's shared screen sound on its own stream, forwarded to everyone
//!   else regardless of the speaker selection (it is not speech and must not crowd out speakers).
//! - **Video:** published tracks with simulcast layers; each receiver gets one layer per subscribed
//!   track. The layer comes from the receiver's downlink estimate ([`LayerAllocator`]) and a switch
//!   happens on a keyframe of the target layer (the SFU asks the publisher for one).
//! - **Layers that are actually sent (#921):** a publisher on a weak uplink pauses its top layers.
//!   It says which layers its uplink carries ([`Forwarder::set_uplink_layers`]), and the
//!   allocation stays below them. A layer that stops arriving while a lower one keeps coming
//!   (older publishers, an encoder that fails) is replaced by the best lower one that flows, until
//!   it comes back: receivers never wait on a layer nobody sends.
//! - **Large frames (#941):** a layer whose frames have more fragments than a receiver reassembles
//!   ([`MediaCapabilities::max_frame_fragments`], 255 for clients from before the long form) is
//!   left out of that receiver's allocation, and such frames don't go to it.
//! - **Bandwidth estimation:** every forwarded datagram gets a per-receiver transport sequence
//!   number; the receiver's feedback drives a [`SendSideEstimator`]. A new subscription measures
//!   the receiver's downlink afresh (#941). Incoming datagrams with a transport sequence number
//!   are recorded for the sender's own (uplink) estimation.
//! - **Probes before a switch up (#994, #1792):** a receiver that sends feedback gets a higher
//!   layer only once a cluster of padding showed its link carries it
//!   ([`teamcraft_media::rate_probe`]); one that falls short holds the estimate instead of a
//!   picture that stalls, and the next comes within seconds, so a link that got better is used
//!   while it lasts.
//!
//! - **Resending lost video (#1762):** forwarded video packets are kept for a short while per
//!   track and layer; a receiver's NACK gets the ones it names again, restamped for it, within a
//!   budget of its downlink ([`crate::retransmit`]). Audio is never resent.
//! - **Probing at the start (#1762):** a participant that joins or comes back gets a few short
//!   trains of padding ([`teamcraft_media::join_probe`]) once it sent transport feedback; their
//!   spread in its feedback shows what
//!   its downlink carries, and its estimate (and so its first layers) starts from that instead of
//!   a guess. Its own trains towards the SFU (the uplink probe) are taken and reported like any
//!   datagram, and forwarded nowhere.
//!
//! - **Reconnecting participants** keep their streams and subscriptions but receive nothing until
//!   they are back ([`Forwarder::suspend`], [`Forwarder::resume`]); a new path starts with fresh
//!   bandwidth estimates.
//!
//! The payload is never read; DAVE frames stay encrypted.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use teamcraft_media::{
  bwe::{BweConfig, FeedbackRecorder, SendSideEstimator},
  datagram::{DatagramError, DatagramHeader, PADDING_STREAM, StreamId, padding_datagram},
  fragment::{FragmentError, FrameFragment},
  join_probe::{JoinProbe, JoinProbeConfig, LinkMeasurement},
  rate_probe::{ProbeOutcome, RateProbe, RateProbeConfig},
  rtp::{AudioLevel, RtpError, RtpHeader},
};
use teamcraft_proto::{
  DeviceId, KNOWN_VIDEO_CODECS, MAX_NACK_SEQUENCE_NUMBERS, MAX_SIMULCAST_LAYERS, MediaCapabilities,
  MediaStreamId, ScreenAudioTrack, TransportFeedback, VideoCodec, VideoLayerInfo, VideoPublicationId,
  VideoTrack, VideoTrackKind,
};
use thiserror::Error;

use crate::layers::{
  LayerAllocationConfig, LayerAllocator, ProbeRequest, TrackDemand, UpgradeProof, full_demand_bps,
};
use crate::retransmit::{PacketHistory, RetransmitBudget, RetransmitConfig};
use crate::speakers::{SpeakerSelectionConfig, SpeakerSelector};

/// Video tracks (camera, screen share) one participant may publish at the same time.
const MAX_VIDEO_TRACKS_PER_PARTICIPANT: usize = 4;
/// The upswitch hold while a receiver's downlink estimate is in its fast start.
const FAST_START_UPSWITCH_HOLD: Duration = Duration::from_millis(500);

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
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ForwardingConfig {
  pub speakers: SpeakerSelectionConfig,
  pub layers: LayerAllocationConfig,
  pub bwe: BweConfig,
  pub keyframes: KeyframeRequestConfig,
  pub probe: RateProbeConfig,
  /// A layer counts as stopped when a lower layer of its track kept arriving this long without
  /// it; receivers get the best lower layer that still flows. Above the gaps of a
  /// layer that runs (a still screen sends its picture again every second, `SCREEN_IDLE_REFRESH`),
  /// short enough that a frozen picture doesn't last.
  pub layer_stalled_after: Duration,
  /// Resending lost video packets to the receiver that asks.
  pub retransmit: RetransmitConfig,
  /// Bytes of padding the probe of a participant's link at the start may send, each direction:
  /// the SFU's of the downlink, and what it tells clients they may send for the uplink. Zero
  /// turns both off.
  pub probe_bytes: u32,
}

/// How often publishers are asked for keyframes. Keyframes are large: a receiver that keeps
/// losing pictures must not turn a track into keyframes only.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyframeRequestConfig {
  /// One request per track and layer at most this often, whoever asks (layer switches,
  /// subscriptions, receivers' requests).
  pub per_layer_interval: Duration,
  /// One request per receiver and track at most this often ([`Forwarder::request_keyframe`]).
  pub per_receiver_interval: Duration,
  /// A layer switch that waits for its keyframe asks again only this often (a keyframe that
  /// starts arriving ends the wait). A large keyframe takes longer than `per_layer_interval` to
  /// be encoded and to arrive on a slow machine, and each request meanwhile cost one more: on a
  /// CI runner a screen share's 1440p layer came with three keyframes of 500 KB in three seconds,
  /// and the viewer's sharp picture only after the third (#939).
  pub switch_retry_interval: Duration,
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
  /// What the probe of a receiver's downlink at the start showed, for its stats.
  DownlinkMeasured {
    receiver: DeviceId,
    measurement: LinkMeasurement,
  },
  /// Padding that probes a receiver's downlink, numbered for it: to be sent as they are, back to
  /// back.
  Padding {
    receiver: DeviceId,
    datagrams: Vec<Vec<u8>>,
  },
}

/// What a leaving participant took with it.
#[derive(Debug, Default)]
pub struct Removal {
  pub effects: Vec<Effect>,
  /// Its video tracks, now unpublished.
  pub video_tracks: Vec<StreamId>,
  pub maybe_screen_audio: Option<StreamId>,
}

/// The video codecs a publisher's viewers decode ([`Forwarder::decodable_video_codecs`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodableCodecs {
  /// Decoded by every other participant that receives video.
  pub by_all: Vec<VideoCodec>,
  /// Decoded by at least one of them that takes layers in other codecs than their track's.
  pub by_some: Vec<VideoCodec>,
}

#[derive(Debug)]
struct ParticipantRoute {
  audio_stream: StreamId,
  maybe_screen_audio: Option<StreamId>,
  capabilities: MediaCapabilities,
  downlink: SendSideEstimator,
  uplink: FeedbackRecorder,
  /// Base for the receiver's arrival times in its feedback (only differences matter).
  feedback_epoch: Instant,
  allocator: LayerAllocator,
  /// Probes of the downlink before a switch up.
  probe: RateProbe,
  /// The probe the allocation waits for.
  maybe_probe_request: Option<ProbeRequest>,
  /// The receiver sent transport feedback: its downlink can be probed.
  has_feedback: bool,
  /// The connection was lost: nothing is sent to it until it is back.
  is_suspended: bool,
  /// What may still be resent to it.
  retransmit_budget: RetransmitBudget,
  /// The probe of its downlink at the start, while it runs.
  maybe_join_probe: Option<JoinProbe>,
}

#[derive(Debug)]
struct VideoTrackRoute {
  track: VideoTrack,
  subscribers: BTreeMap<DeviceId, VideoSubscription>,
  layers_needed: Vec<u8>,
  /// How many of the published layers (the lowest ones) the publisher's uplink carries
  /// ([`Forwarder::set_uplink_layers`]); all of them until it says otherwise.
  uplink_layers: usize,
  /// The latest run of packets per layer.
  arrivals: LayerArrivals,
  /// The most fragments a frame of each layer had: a receiver that reassembles fewer (clients
  /// from before #941 take 255) doesn't get the layer, it would wait for its keyframes forever.
  frame_fragments: [u16; MAX_SIMULCAST_LAYERS as usize],
  /// The packets forwarded recently per layer, to resend to a receiver that lost them.
  history: [PacketHistory; MAX_SIMULCAST_LAYERS as usize],
}

/// The latest run of packets per layer; `None` before the first.
type LayerArrivals = [Option<LayerRun>; MAX_SIMULCAST_LAYERS as usize];

/// Packets of one layer without a gap longer than [`ForwardingConfig::layer_stalled_after`].
#[derive(Debug, Clone, Copy)]
struct LayerRun {
  since: Instant,
  last: Instant,
}

#[derive(Debug, Clone, Copy)]
struct VideoSubscription {
  max_layer: u8,
  /// Layer the allocation wants, `None` = paused.
  target: Option<u8>,
  /// Layer being forwarded; changes on a keyframe to `target`, or to the best lower layer that
  /// flows while `target` doesn't ([`live_layer`]).
  current: Option<u8>,
  /// When this receiver last asked for a keyframe of the track.
  maybe_last_request: Option<Instant>,
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

impl Default for ForwardingConfig {
  fn default() -> Self {
    Self {
      speakers: SpeakerSelectionConfig::default(),
      layers: LayerAllocationConfig::default(),
      bwe: BweConfig::default(),
      keyframes: KeyframeRequestConfig::default(),
      probe: RateProbeConfig::default(),
      layer_stalled_after: Duration::from_millis(1500),
      retransmit: RetransmitConfig::default(),
      probe_bytes: 15_000,
    }
  }
}

impl Default for KeyframeRequestConfig {
  fn default() -> Self {
    Self {
      per_layer_interval: Duration::from_millis(500),
      per_receiver_interval: Duration::from_secs(1),
      switch_retry_interval: Duration::from_secs(2),
    }
  }
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
      maybe_screen_audio: None,
      capabilities,
      downlink: SendSideEstimator::new(self.config.bwe),
      uplink: FeedbackRecorder::new(now),
      feedback_epoch: now,
      allocator: LayerAllocator::new(),
      probe: RateProbe::new(),
      maybe_probe_request: None,
      has_feedback: false,
      is_suspended: false,
      retransmit_budget: RetransmitBudget::default(),
      maybe_join_probe: self.new_join_probe(now),
    };
    self.participants.insert(device_id, route);
  }

  /// A participant's connection was lost: it stops receiving and drops out of the speaker
  /// selection; its tracks stay published and its subscriptions stay (paused) for its return.
  pub fn suspend(&mut self, device_id: DeviceId, now: Instant) -> Vec<Effect> {
    let Some(participant) = self.participants.get_mut(&device_id) else {
      return Vec::new();
    };
    participant.is_suspended = true;
    participant.maybe_probe_request = None;
    let mut effects = Vec::new();
    if let Some(speakers) = self.speakers.remove(device_id, now) {
      effects.push(Effect::ActiveSpeakers(speakers));
    }
    // Its tracks start afresh when it is back: their silence meanwhile is the lost connection, not
    // a stopped encoder to stand in for (#1719).
    for route in self.video_tracks.values_mut() {
      if route.track.device_id == device_id {
        route.arrivals = LayerArrivals::default();
      }
    }
    let subscribed: Vec<StreamId> = self
      .video_tracks
      .iter_mut()
      .filter_map(|(&stream_id, route)| {
        let subscription = route.subscribers.get_mut(&device_id)?;
        subscription.target = None;
        subscription.current = None;
        Some(stream_id)
      })
      .collect();
    for stream_id in subscribed {
      effects.extend(self.update_layers_needed(stream_id, now));
    }
    effects
  }

  /// A suspended participant is back on a new connection, possibly over another network: its
  /// estimates start over; the next allocation resumes its subscriptions (with keyframes). As a
  /// publisher it hears again which layers of its tracks are needed: changes while it was away
  /// never reached it (#921).
  pub fn resume(
    &mut self,
    device_id: DeviceId,
    capabilities: MediaCapabilities,
    now: Instant,
  ) -> Vec<Effect> {
    let bwe = self.config.bwe;
    let maybe_join_probe = self.new_join_probe(now);
    let Some(participant) = self.participants.get_mut(&device_id) else {
      return Vec::new();
    };
    // Maybe another network: what it carries is measured afresh.
    participant.maybe_join_probe = maybe_join_probe;
    participant.capabilities = capabilities;
    participant.downlink = SendSideEstimator::new(bwe);
    participant.uplink = FeedbackRecorder::new(now);
    participant.feedback_epoch = now;
    participant.allocator = LayerAllocator::new();
    participant.probe = RateProbe::new();
    participant.maybe_probe_request = None;
    participant.has_feedback = false;
    participant.is_suspended = false;
    participant.retransmit_budget = RetransmitBudget::default();
    self
      .video_tracks
      .iter()
      .filter(|(_, route)| route.track.device_id == device_id)
      .map(|(&stream_id, route)| Effect::LayersNeeded {
        publisher: device_id,
        stream_id,
        layers: route.layers_needed.clone(),
      })
      .collect()
  }

  /// The video codecs every other participant that decodes video decodes, best first (#565):
  /// what `device_id` can publish so that everyone may watch it. Participants without video
  /// decoders (audio only) don't count; with nobody else, every codec there is. `None` for a
  /// participant that encodes no video (it publishes nothing), or one that isn't there.
  ///
  /// Besides, the codecs at least one of them decodes and takes as a layer in another codec than
  /// its track ([`teamcraft_proto::VideoFeature::LayerCodecs`], #1733): a screen share's large layers may go out
  /// in them above a base layer in a codec of the first list. With nobody else, every codec too.
  pub fn decodable_video_codecs(&self, device_id: DeviceId) -> Option<DecodableCodecs> {
    let publisher = self.participants.get(&device_id)?;
    if publisher.capabilities.video_encoders.is_empty() {
      return None;
    }
    let mut by_all = KNOWN_VIDEO_CODECS.to_vec();
    let mut by_some = Vec::new();
    let mut has_receivers = false;
    for (other, participant) in &self.participants {
      let decoders = &participant.capabilities.video_decoders;
      if *other == device_id || decoders.is_empty() {
        continue;
      }
      has_receivers = true;
      by_all.retain(|codec| decoders.contains(codec));
      if participant.capabilities.has_layer_codecs() {
        by_some.extend(
          decoders
            .iter()
            .filter(|codec| KNOWN_VIDEO_CODECS.contains(codec))
            .cloned(),
        );
      }
    }
    let by_some = if has_receivers {
      KNOWN_VIDEO_CODECS
        .iter()
        .filter(|codec| by_some.contains(codec))
        .cloned()
        .collect()
    } else {
      KNOWN_VIDEO_CODECS.to_vec()
    };
    Some(DecodableCodecs { by_all, by_some })
  }

  /// Removes a participant with its tracks and subscriptions.
  pub fn remove_participant(&mut self, device_id: DeviceId, now: Instant) -> Removal {
    let maybe_screen_audio = self
      .participants
      .remove(&device_id)
      .and_then(|route| route.maybe_screen_audio);
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
    self.forget_keyframe_requests();
    let subscribed: Vec<StreamId> = self
      .video_tracks
      .iter_mut()
      .filter_map(|(&stream_id, route)| route.subscribers.remove(&device_id).map(|_| stream_id))
      .collect();
    for stream_id in subscribed {
      effects.extend(self.update_layers_needed(stream_id, now));
    }
    Removal {
      effects,
      video_tracks: owned,
      maybe_screen_audio,
    }
  }

  /// Starts forwarding `owner`'s screen sound on `stream_id`; one per participant.
  /// Everyone's published screen sound: owner and stream.
  pub fn screen_audio_streams(&self) -> Vec<(DeviceId, StreamId)> {
    self
      .participants
      .iter()
      .filter_map(|(device, route)| route.maybe_screen_audio.map(|stream| (*device, stream)))
      .collect()
  }

  /// Whether a participant's audio, screen sound or a video track uses the id.
  pub fn is_stream_in_use(&self, stream_id: StreamId) -> bool {
    self.video_tracks.contains_key(&stream_id)
      || self
        .participants
        .values()
        .any(|route| route.audio_stream == stream_id || route.maybe_screen_audio == Some(stream_id))
  }

  pub fn publish_screen_audio(
    &mut self,
    owner: DeviceId,
    stream_id: StreamId,
  ) -> Result<ScreenAudioTrack, ForwardError> {
    let participant = self.participants.get_mut(&owner).ok_or(ForwardError::NotJoined)?;
    if participant.maybe_screen_audio.is_some() {
      return Err(ForwardError::InvalidPublication("screen audio already published"));
    }
    participant.maybe_screen_audio = Some(stream_id);
    Ok(ScreenAudioTrack {
      device_id: owner,
      stream_id: MediaStreamId(stream_id.0),
    })
  }

  pub fn unpublish_screen_audio(&mut self, owner: DeviceId, stream_id: StreamId) -> Result<(), ForwardError> {
    let participant = self.participants.get_mut(&owner).ok_or(ForwardError::NotJoined)?;
    if participant.maybe_screen_audio != Some(stream_id) {
      return Err(ForwardError::ForeignStream(stream_id));
    }
    participant.maybe_screen_audio = None;
    Ok(())
  }

  pub fn publish_video(
    &mut self,
    owner: DeviceId,
    stream_id: StreamId,
    kind: VideoTrackKind,
    codec: VideoCodec,
    layers: Vec<VideoLayerInfo>,
    maybe_publication_id: Option<VideoPublicationId>,
  ) -> Result<VideoTrack, ForwardError> {
    let participant = self.participants.get(&owner).ok_or(ForwardError::NotJoined)?;
    let encoders = &participant.capabilities.video_encoders;
    if !encoders.contains(&codec) || layers.iter().any(|info| !encoders.contains(info.codec(&codec))) {
      return Err(ForwardError::InvalidPublication(
        "codec not in the publisher's encoders",
      ));
    }
    // #1733: the lowest layer is in the track's codec, so a receiver that knows only the track's
    // codec (a client from before layer codecs) has a layer to get.
    if layers.first().is_some_and(|info| info.codec(&codec) != &codec) {
      return Err(ForwardError::InvalidPublication(
        "the lowest layer must be in the track's codec",
      ));
    }
    // Four since #978; clients publish three to SFUs from before, which say no limit in `joined`.
    if layers.is_empty() || layers.len() > usize::from(MAX_SIMULCAST_LAYERS) {
      return Err(ForwardError::InvalidPublication("needs 1 to 4 layers"));
    }
    if layers
      .iter()
      .zip(0u8..)
      .any(|(info, expected)| info.layer != expected)
    {
      return Err(ForwardError::InvalidPublication(
        "layers must be numbered from 0 in order",
      ));
    }
    let is_new_stream = !self.video_tracks.contains_key(&stream_id);
    let published = self
      .video_tracks
      .values()
      .filter(|route| route.track.device_id == owner)
      .count();
    if is_new_stream && published >= MAX_VIDEO_TRACKS_PER_PARTICIPANT {
      return Err(ForwardError::InvalidPublication("too many video tracks"));
    }
    let track = VideoTrack {
      stream_id: MediaStreamId(stream_id.0),
      device_id: owner,
      kind,
      codec,
      layers,
      maybe_publication_id,
    };
    let route = VideoTrackRoute {
      uplink_layers: track.layers.len(),
      track: track.clone(),
      subscribers: BTreeMap::new(),
      layers_needed: Vec::new(),
      arrivals: [None; MAX_SIMULCAST_LAYERS as usize],
      frame_fragments: [0; MAX_SIMULCAST_LAYERS as usize],
      history: Default::default(),
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
    self.forget_keyframe_requests();
    Ok(())
  }

  /// Unpublishes every screen share of devices other than `sharer`'s: a call has one screen
  /// share at a time, and the latest one wins. A device's own earlier share stays (it publishes
  /// its share anew in another codec, then unpublishes the old one). Returns the owner and stream
  /// of each share that ended.
  pub fn end_other_screen_shares(&mut self, sharer: DeviceId) -> Vec<(DeviceId, StreamId)> {
    let ended: Vec<(DeviceId, StreamId)> = self
      .video_tracks
      .iter()
      .filter(|(_, route)| route.track.kind == VideoTrackKind::Screen && route.track.device_id != sharer)
      .map(|(stream_id, route)| (route.track.device_id, *stream_id))
      .collect();
    for (_, stream_id) in &ended {
      self.video_tracks.remove(stream_id);
    }
    if !ended.is_empty() {
      self.forget_keyframe_requests();
    }
    ended
  }

  /// The publisher's uplink carries `layers` of its track now (the lowest ones; it pauses the
  /// others): receivers are allocated no higher layer, so they move to one that is sent.
  pub fn set_uplink_layers(
    &mut self,
    owner: DeviceId,
    stream_id: StreamId,
    layers: &[u8],
    now: Instant,
  ) -> Result<Vec<Effect>, ForwardError> {
    let route = self
      .video_tracks
      .get_mut(&stream_id)
      .ok_or(ForwardError::UnknownTrack(stream_id))?;
    if route.track.device_id != owner {
      return Err(ForwardError::ForeignStream(stream_id));
    }
    // The lowest layers in a row: a layer above a gap is no use to a receiver either.
    let carried = route
      .track
      .layers
      .iter()
      .take_while(|info| layers.contains(&info.layer))
      .count();
    if carried == route.uplink_layers {
      return Ok(Vec::new());
    }
    route.uplink_layers = carried;
    let receivers: Vec<DeviceId> = route.subscribers.keys().copied().collect();
    let mut effects = Vec::new();
    for receiver in receivers {
      effects.extend(self.reallocate(receiver, now));
    }
    effects.extend(self.update_layers_needed(stream_id, now));
    Ok(effects)
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
    if receivable_layers(&route.track, &participant.capabilities) == 0 {
      return Err(ForwardError::UnsupportedCodec(route.track.codec.clone()));
    }
    let is_new = maybe_max_layer.is_some() && !route.subscribers.contains_key(&receiver);
    match maybe_max_layer {
      Some(max_layer) => {
        let subscription = route.subscribers.entry(receiver).or_insert(VideoSubscription {
          max_layer,
          target: None,
          current: None,
          maybe_last_request: None,
        });
        subscription.max_layer = max_layer;
      },
      None => {
        route.subscribers.remove(&receiver);
      },
    }
    if is_new {
      self.measure_downlink_afresh(receiver);
    }
    let mut effects = self.reallocate(receiver, now);
    // Subscribing again (a video shown anew) brings a new decoder that needs a keyframe, also
    // when the layer stays the same and the allocation asked for none.
    if let Some((publisher, layer)) = self.received_layer(receiver, stream_id) {
      let interval = self.config.keyframes.per_layer_interval;
      effects.extend(self.keyframe_request(publisher, stream_id, layer, interval, now));
    }
    effects.extend(self.update_layers_needed(stream_id, now));
    Ok(effects)
  }

  /// `receiver` can't decode `stream_id` until its next keyframe (a lost fragment, a frame that
  /// didn't decrypt, a failed decoder): asks the publisher for one on the layer the receiver
  /// gets. Rate-limited per receiver and per layer; nothing for a track it doesn't receive.
  pub fn request_keyframe(&mut self, receiver: DeviceId, stream_id: StreamId, now: Instant) -> Vec<Effect> {
    let interval = self.config.keyframes.per_receiver_interval;
    let Some(subscription) = self
      .video_tracks
      .get_mut(&stream_id)
      .and_then(|route| route.subscribers.get_mut(&receiver))
    else {
      return Vec::new();
    };
    let is_due = subscription
      .maybe_last_request
      .is_none_or(|last| now.saturating_duration_since(last) >= interval);
    if !is_due {
      return Vec::new();
    }
    subscription.maybe_last_request = Some(now);
    let Some((publisher, layer)) = self.received_layer(receiver, stream_id) else {
      return Vec::new();
    };
    let interval = self.config.keyframes.per_layer_interval;
    self
      .keyframe_request(publisher, stream_id, layer, interval, now)
      .into_iter()
      .collect()
  }

  /// Decides where one datagram from `sender` goes.
  pub fn route(&mut self, sender: DeviceId, datagram: &[u8], now: Instant) -> Result<Route, ForwardError> {
    let header = DatagramHeader::parse(datagram)?;
    let participant = self
      .participants
      .get_mut(&sender)
      .ok_or(ForwardError::NotJoined)?;
    if header.stream_id == PADDING_STREAM {
      // The sender probes its uplink: reported like any datagram, forwarded nowhere.
      if let Some(seq) = header.maybe_transport_seq {
        participant.uplink.on_arrival(seq, now);
      }
      return Ok(Route::default());
    }
    let is_audio = header.stream_id == participant.audio_stream;
    let is_screen_audio = participant.maybe_screen_audio == Some(header.stream_id);
    if !is_audio
      && !is_screen_audio
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
    let receivers = if is_screen_audio {
      self.others(sender)
    } else if is_audio {
      let level = rtp_header.audio_level().unwrap_or(AudioLevel::SILENCE);
      if let Some(speakers) = self.speakers.record(sender, level, now) {
        route.effects.push(Effect::ActiveSpeakers(speakers));
      }
      if self.speakers.is_selected(sender) {
        self.others(sender)
      } else {
        Vec::new()
      }
    } else {
      let receivers = self.video_receivers(header.stream_id, header.layer.value(), &rtp_header, now)?;
      if !receivers.is_empty() {
        self.keep_for_retransmission(header, rtp_header.sequence_number.0, datagram, now);
      }
      receivers
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

  /// Datagrams to resend to `receiver`, restamped for it: those of `sequence_numbers` (the RTP
  /// sequence numbers of `layer` of `stream_id`) that are still kept, while the receiver gets that
  /// layer and as far as its budget goes. Numbers beyond [`MAX_NACK_SEQUENCE_NUMBERS`] are
  /// ignored, and so is a NACK for a track or layer it doesn't receive.
  pub fn resend(
    &mut self,
    receiver: DeviceId,
    stream_id: StreamId,
    layer: u8,
    sequence_numbers: &[u16],
    now: Instant,
  ) -> Vec<Vec<u8>> {
    let config = self.config.retransmit;
    let Some(route) = self.video_tracks.get(&stream_id) else {
      return Vec::new();
    };
    let is_received = route
      .subscribers
      .get(&receiver)
      .is_some_and(|subscription| subscription.current == Some(layer));
    let (Some(history), Some(participant)) = (
      route.history.get(usize::from(layer)),
      self.participants.get_mut(&receiver),
    ) else {
      return Vec::new();
    };
    if !is_received || participant.is_suspended {
      return Vec::new();
    }
    let estimate_bps = participant.downlink.estimate_bps();
    let mut resent = Vec::new();
    for &sequence_number in sequence_numbers.iter().take(MAX_NACK_SEQUENCE_NUMBERS) {
      let Some(datagram) = history.get(sequence_number, now, config.window) else {
        continue;
      };
      if !participant
        .retransmit_budget
        .try_spend(datagram.len(), estimate_bps, now, &config)
      {
        break;
      }
      let seq = participant.downlink.on_send(datagram.len() + 2, now);
      // Invariant: the datagram parsed when it was forwarded, so restamping it can't fail; one
      // that does is skipped.
      if let Ok(stamped) = DatagramHeader::restamp(datagram, seq) {
        resent.push(stamped);
      }
    }
    resent
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
    participant.has_feedback = true;
    let mut effects = Vec::new();
    if let Some(measurement) = participant
      .maybe_join_probe
      .as_mut()
      .and_then(|probe| probe.on_feedback(feedback))
    {
      effects.push(Self::apply_join_probe(receiver, participant, measurement, now));
    }
    match participant.probe.on_feedback(&self.config.probe, feedback, now) {
      // During the fast start the estimate triples every second on its own; jumping ahead of it
      // switched a screen share's viewer to the sharp layer before the presenter's uplink
      // estimate (in its own fast start) carried that layer, and the presenter stopped it again
      // (screen_sharpness on CI). After congestion the estimate grows slowly: there the probe
      // lifts it at once (#1792).
      Some(ProbeOutcome::Carried { target_bps }) if !participant.downlink.is_in_fast_start() => {
        participant.downlink.raise_to(target_bps);
      },
      Some(ProbeOutcome::Carried { .. }) => {},
      // Short of the target: the estimate is no higher than that, and the cluster's queue is no
      // reason to cut it below what the receiver gets now.
      Some(ProbeOutcome::FellShort {
        target_bps,
        floor_bps,
      }) => {
        let estimate_bps = participant.downlink.estimate_bps();
        participant
          .downlink
          .limit_to(estimate_bps.min(target_bps).max(floor_bps), now);
      },
      None => {},
    }
    effects.extend(self.on_timer(receiver, now));
    effects
  }

  /// Re-runs a receiver's layer allocation; called periodically (every ~100 ms) so paused tracks
  /// get their resume probe even when no feedback arrives.
  pub fn on_timer(&mut self, receiver: DeviceId, now: Instant) -> Vec<Effect> {
    let mut effects: Vec<Effect> = self.run_join_probe(receiver, now).into_iter().collect();
    effects.extend(self.reallocate(receiver, now));
    effects.extend(self.probe_downlink(receiver, now));
    let streams: Vec<StreamId> = self
      .video_tracks
      .iter()
      .filter(|(_, route)| route.subscribers.contains_key(&receiver))
      .map(|(&stream_id, _)| stream_id)
      .collect();
    for stream_id in streams {
      effects.extend(self.update_layers_needed(stream_id, now));
    }
    effects
  }

  /// Feedback for the datagrams `sender` sent since the last call (its uplink estimation).
  pub fn take_uplink_feedback(&mut self, sender: DeviceId, now: Instant) -> Option<TransportFeedback> {
    self.participants.get_mut(&sender)?.uplink.take_feedback(now)
  }

  /// The current speaker set, loudest first.
  pub fn active_speakers(&self) -> &[DeviceId] {
    self.speakers.selected()
  }

  /// Published screen audio, for participants who join later.
  pub fn screen_audio_tracks(&self) -> Vec<ScreenAudioTrack> {
    self
      .participants
      .iter()
      .filter_map(|(&device_id, route)| {
        route.maybe_screen_audio.map(|stream_id| ScreenAudioTrack {
          device_id,
          stream_id: MediaStreamId(stream_id.0),
        })
      })
      .collect()
  }

  /// Published video tracks.
  pub fn video_tracks(&self) -> Vec<VideoTrack> {
    self
      .video_tracks
      .values()
      .map(|route| route.track.clone())
      .collect()
  }

  /// How many bytes of padding a client may send to probe its uplink (`joined`); `None` when
  /// probing is off.
  pub fn probe_bytes(&self) -> Option<u32> {
    (self.config.probe_bytes > 0).then_some(self.config.probe_bytes)
  }

  fn new_join_probe(&self, now: Instant) -> Option<JoinProbe> {
    JoinProbeConfig::default()
      .within_bytes(usize::try_from(self.config.probe_bytes).unwrap_or(usize::MAX))
      .map(|config| JoinProbe::new(config, now))
  }

  /// The train of `receiver`'s probe at the start that is due, or the probe's end when its last
  /// train's feedback didn't come.
  fn run_join_probe(&mut self, receiver: DeviceId, now: Instant) -> Option<Effect> {
    let participant = self.participants.get_mut(&receiver)?;
    // A receiver that reports no arrivals can't be measured (clients from before transport
    // feedback); it gets no padding, as from the probes before a switch up.
    if participant.is_suspended || !participant.has_feedback {
      return None;
    }
    let probe = participant.maybe_join_probe.as_mut()?;
    if let Some(measurement) = probe.on_timer(now) {
      return Some(Self::apply_join_probe(receiver, participant, measurement, now));
    }
    let sizes = probe.due_train(now)?;
    let mut datagrams = Vec::with_capacity(sizes.len());
    let mut maybe_first_seq = None;
    for bytes in &sizes {
      let seq = participant.downlink.on_send_probe(*bytes, now);
      maybe_first_seq.get_or_insert(seq);
      datagrams.push(padding_datagram(*bytes, seq));
    }
    let first_seq = maybe_first_seq?;
    if let Some(probe) = participant.maybe_join_probe.as_mut() {
      probe.sent(first_seq, &sizes, now);
    }
    Some(Effect::Padding { receiver, datagrams })
  }

  /// The downlink estimate starts from what the probe showed, and counts as shown for a switch up;
  /// the receiver is told for its stats.
  fn apply_join_probe(
    receiver: DeviceId,
    participant: &mut ParticipantRoute,
    measurement: LinkMeasurement,
    now: Instant,
  ) -> Effect {
    let estimate_bps = participant.downlink.apply_probe(measurement.capacity_bps);
    participant.probe.note_shown(estimate_bps, now);
    Effect::DownlinkMeasured {
      receiver,
      measurement,
    }
  }

  /// The next train of padding for the probe `receiver`'s allocation waits for, when one is due:
  /// a cluster starts when the allocation asks for one and the last is long enough ago.
  fn probe_downlink(&mut self, receiver: DeviceId, now: Instant) -> Option<Effect> {
    let config = self.config.probe;
    let participant = self.participants.get_mut(&receiver)?;
    participant.probe.on_timer(&config, now);
    if participant.is_suspended {
      return None;
    }
    if !participant.probe.is_running() {
      let request = participant.maybe_probe_request?;
      if !participant.probe.may_start(now) {
        return None;
      }
      // A step above what the link carries now: the estimate of a receiver that gets less than
      // it (its layers don't fill it) says nothing about the link, and a cluster at the whole
      // step of a 1440p share from there overflowed a mobile link's buffer (#1792).
      let sending_bps = participant.downlink.sent_bps(now);
      let shown_bps = participant.probe.shown_bps(&config, now);
      let carried_bps = participant
        .downlink
        .estimate_bps()
        .min(sending_bps.max(request.floor_bps).max(shown_bps));
      let target_bps = participant
        .probe
        .target_bps(&config, request.target_bps, carried_bps);
      participant
        .probe
        .start(target_bps, request.floor_bps, sending_bps, now);
    }
    let sizes = participant.probe.due_train(&config, now)?;
    let mut datagrams = Vec::with_capacity(sizes.len());
    let mut maybe_first_seq = None;
    for bytes in &sizes {
      let seq = participant.downlink.on_send_probe(*bytes, now);
      maybe_first_seq.get_or_insert(seq);
      datagrams.push(padding_datagram(*bytes, seq));
    }
    participant.probe.sent(maybe_first_seq?, &sizes, now);
    Some(Effect::Padding { receiver, datagrams })
  }

  /// Keeps a video datagram that went to receivers, to resend it to one that lost it.
  fn keep_for_retransmission(
    &mut self,
    header: DatagramHeader,
    sequence_number: u16,
    datagram: &[u8],
    now: Instant,
  ) {
    let config = self.config.retransmit;
    if let Some(history) = self
      .video_tracks
      .get_mut(&header.stream_id)
      .and_then(|route| route.history.get_mut(usize::from(header.layer.value())))
    {
      history.push(sequence_number, datagram, now, &config);
    }
  }

  /// How long video packets are kept for [`Self::resend`], as `joined` tells clients; `None`
  /// when nothing is resent.
  pub fn retransmit_window_ms(&self) -> Option<u32> {
    self.config.retransmit.announced_window_ms()
  }

  /// Current downlink estimate of a receiver.
  pub fn downlink_estimate_bps(&self, receiver: DeviceId) -> Option<u32> {
    self
      .participants
      .get(&receiver)
      .map(|participant| participant.downlink.estimate_bps())
  }

  /// Everyone reachable except `sender`.
  fn others(&self, sender: DeviceId) -> Vec<DeviceId> {
    self
      .participants
      .iter()
      .filter(|(id, route)| **id != sender && !route.is_suspended)
      .map(|(id, _)| *id)
      .collect()
  }

  /// Receivers of one video packet; switches subscriptions on a keyframe to the layer they should
  /// get: their target, or the best lower layer that flows while the target doesn't.
  fn video_receivers(
    &mut self,
    stream_id: StreamId,
    layer: u8,
    rtp_header: &RtpHeader,
    now: Instant,
  ) -> Result<Vec<DeviceId>, ForwardError> {
    let fragment = FrameFragment::from_header(rtp_header)?;
    let starts_keyframe = fragment.is_keyframe && fragment.index == 0;
    if starts_keyframe {
      // The keyframe asked for is here: a receiver that loses it may ask again at once.
      self.last_keyframe_requests.remove(&(stream_id, layer));
    }
    let stalled_after = self.config.layer_stalled_after;
    let Some(route) = self.video_tracks.get_mut(&stream_id) else {
      return Ok(Vec::new());
    };
    record_arrival(&mut route.arrivals, layer, now, stalled_after);
    if let Some(most) = route.frame_fragments.get_mut(usize::from(layer)) {
      *most = (*most).max(fragment.count);
    }
    let arrivals = route.arrivals;
    let is_camera = route.track.kind == VideoTrackKind::Camera;
    let mut receivers = Vec::new();
    for (&receiver, subscription) in &mut route.subscribers {
      let can_reassemble = self
        .participants
        .get(&receiver)
        .is_some_and(|participant| participant.capabilities.max_frame_fragments >= fragment.count);
      if !can_reassemble {
        // Its next allocation leaves this layer out.
        continue;
      }
      let maybe_live = subscription.target.map(|target| {
        stand_in_layer(
          &arrivals,
          is_camera,
          subscription.max_layer,
          target,
          stalled_after,
          now,
        )
      });
      if starts_keyframe && maybe_live == Some(layer) && subscription.current != Some(layer) {
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
    let Some(participant) = self
      .participants
      .get_mut(&receiver)
      .filter(|participant| !participant.is_suspended)
    else {
      return Vec::new();
    };
    // Without any feedback yet (older clients) the estimate is just the start value; it still
    // grants the lowest layers.
    let estimate = participant.downlink.estimate_bps();
    let max_frame_fragments = participant.capabilities.max_frame_fragments;
    let capabilities = &participant.capabilities;
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
          // Layers the publisher's uplink doesn't carry, with frames larger than the receiver
          // reassembles, or in a codec it doesn't decode (#1733), aren't there to have.
          layers: route
            .track
            .layers
            .get(
              ..route
                .uplink_layers
                .min(route.layers_within(max_frame_fragments))
                .min(receivable_layers(&route.track, capabilities)),
            )
            .unwrap_or_default(),
          can_pause: !is_screen || self.config.layers.can_pause_screen_shares,
          downswitch_hold: if is_screen {
            self.config.layers.screen_downswitch_hold
          } else {
            Duration::ZERO
          },
        })
      })
      .collect();
    let maybe_demand = (!demands.is_empty()).then(|| full_demand_bps(&self.config.layers, &demands));
    participant.downlink.set_demand_bps(maybe_demand);
    // While the downlink estimate is in its fast start, it only grows, from feedback of traffic
    // far below it: holding an upswitch for the usual time validates nothing, it only keeps a
    // viewer on a lower layer (a screen share's sharp layer came 2 s after the estimate covered
    // it; on a CI runner that missed the test's deadline, #939). A short hold still rides out a
    // step back.
    let layers_config = if participant.downlink.is_in_fast_start() {
      LayerAllocationConfig {
        upswitch_hold: FAST_START_UPSWITCH_HOLD.min(self.config.layers.upswitch_hold),
        ..self.config.layers
      }
    } else {
      self.config.layers
    };
    let proof = if participant.has_feedback {
      UpgradeProof::Shown {
        bps: participant.probe.shown_bps(&self.config.probe, now),
      }
    } else {
      UpgradeProof::NotRequired
    };
    let allocation =
      participant
        .allocator
        .allocate_with_proof(&layers_config, estimate, proof, &demands, now);
    participant.maybe_probe_request = allocation.maybe_probe;
    let allocation = allocation.layers;

    let stalled_after = self.config.layer_stalled_after;
    let mut effects = Vec::new();
    for (stream_id, target) in allocation {
      let Some(route) = self.video_tracks.get_mut(&stream_id) else {
        continue;
      };
      let Some(subscription) = route.subscribers.get_mut(&receiver) else {
        continue;
      };
      subscription.target = target;
      let Some(layer) = target else {
        subscription.current = None;
        continue;
      };
      let current = subscription.current;
      let publisher = route.track.device_id;
      // While the target doesn't flow, the best lower layer that does stands in (it needs a
      // keyframe as well); the target is still asked for, so it starts with one when it's back.
      let is_camera = route.track.kind == VideoTrackKind::Camera;
      let live = stand_in_layer(
        &route.arrivals,
        is_camera,
        subscription.max_layer,
        layer,
        stalled_after,
        now,
      );
      // Nothing of the camera flows: the layer the receiver gets is asked for again (a keyframe
      // restarts an encoder that stopped, #1719), as often as a switch that waits.
      let is_starved = is_starved(&route.arrivals, is_camera, stalled_after, now);
      let interval = self.config.keyframes.switch_retry_interval;
      for wanted in [live, layer] {
        if current != Some(wanted) || (is_starved && wanted == live) {
          effects.extend(self.keyframe_request(publisher, stream_id, wanted, interval, now));
        }
      }
    }
    effects
  }

  /// `receiver` is about to get far more than so far (a new video track, often after a call of
  /// sound only): its downlink estimate grew only as far as the smaller traffic let it, and one
  /// lossy moment cut it to what the sound got through. A screen share subscribed then came on
  /// its half-size layer and took minutes to the sharp one (issue #941). As for a publisher's new
  /// track (#935), the estimate is measured afresh: from at least the start value, without the
  /// old loss and delay, growing fast until the path shows congestion.
  fn measure_downlink_afresh(&mut self, receiver: DeviceId) {
    if let Some(participant) = self.participants.get_mut(&receiver) {
      participant.downlink.measure_afresh();
    }
  }

  /// The publisher of `stream_id` and the layer `receiver` gets (or is about to get) of it.
  fn received_layer(&self, receiver: DeviceId, stream_id: StreamId) -> Option<(DeviceId, u8)> {
    let route = self.video_tracks.get(&stream_id)?;
    let subscription = route.subscribers.get(&receiver)?;
    let layer = subscription.current.or(subscription.target)?;
    Some((route.track.device_id, layer))
  }

  /// Asks `publisher` for a keyframe on `layer`, unless one was asked for within the interval.
  fn keyframe_request(
    &mut self,
    publisher: DeviceId,
    stream_id: StreamId,
    layer: u8,
    interval: Duration,
    now: Instant,
  ) -> Option<Effect> {
    let key = (stream_id, layer);
    let is_due = self
      .last_keyframe_requests
      .get(&key)
      .is_none_or(|last| now.saturating_duration_since(*last) >= interval);
    if !is_due {
      return None;
    }
    self.last_keyframe_requests.insert(key, now);
    Some(Effect::KeyframeRequest {
      publisher,
      stream_id,
      layer,
    })
  }

  /// Drops the request times of tracks that are gone, so the map stays as small as the room.
  fn forget_keyframe_requests(&mut self) {
    let tracks = &self.video_tracks;
    self
      .last_keyframe_requests
      .retain(|(stream_id, _), _| tracks.contains_key(stream_id));
  }

  fn update_layers_needed(&mut self, stream_id: StreamId, now: Instant) -> Option<Effect> {
    let stalled_after = self.config.layer_stalled_after;
    let route = self.video_tracks.get_mut(&stream_id)?;
    let arrivals = route.arrivals;
    let is_camera = route.track.kind == VideoTrackKind::Camera;
    let is_starved = is_starved(&arrivals, is_camera, stalled_after, now);
    let published = &route.track.layers;
    // A layer standing in for a stopped target is needed as well, or it might stop too. While
    // nothing of a camera flows, every layer a receiver may take is needed ([`is_starved`]).
    let mut needed: Vec<u8> = route
      .subscribers
      .values()
      .flat_map(|subscription| {
        let maybe_live = subscription.target.map(|target| {
          stand_in_layer(
            &arrivals,
            is_camera,
            subscription.max_layer,
            target,
            stalled_after,
            now,
          )
        });
        let starved = published.iter().map(|info| info.layer).filter(move |layer| {
          is_starved && subscription.target.is_some() && *layer <= subscription.max_layer
        });
        [subscription.target, maybe_live, subscription.current]
          .into_iter()
          .flatten()
          .chain(starved)
      })
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

impl VideoTrackRoute {
  /// How many of the lowest layers have had no frame of more than `max_frame_fragments`.
  fn layers_within(&self, max_frame_fragments: u16) -> usize {
    self
      .frame_fragments
      .iter()
      .take_while(|most| **most <= max_frame_fragments)
      .count()
  }
}

/// How many of `track`'s lowest layers a receiver of `capabilities` takes (#1733): layers in a
/// codec it decodes, and in another codec than the track's only where it knows layer codecs
/// ([`teamcraft_proto::VideoFeature::LayerCodecs`]). The SFU switches a receiver between layers at keyframes, and
/// its decoder follows the codec there; a client from before takes the track's codec only.
fn receivable_layers(track: &VideoTrack, capabilities: &MediaCapabilities) -> usize {
  track
    .layers
    .iter()
    .take_while(|info| {
      let codec = info.codec(&track.codec);
      capabilities.video_decoders.contains(codec)
        && (codec == &track.codec || capabilities.has_layer_codecs())
    })
    .count()
}

/// Notes a packet of `layer`: it continues the layer's run, or starts a new one after a gap.
fn record_arrival(arrivals: &mut LayerArrivals, layer: u8, now: Instant, stalled_after: Duration) {
  if let Some(slot) = arrivals.get_mut(usize::from(layer)) {
    *slot = Some(match *slot {
      Some(run) if now.saturating_duration_since(run.last) <= stalled_after => LayerRun { last: now, ..run },
      _ => LayerRun {
        since: now,
        last: now,
      },
    });
  }
}

/// The layer a receiver that wants `target` should get: the target, or while it stopped arriving
/// the best lower layer that still flows. A track nothing arrives of keeps its target: there is
/// nothing better.
fn live_layer(arrivals: &LayerArrivals, target: u8, stalled_after: Duration) -> u8 {
  let mut layer = target;
  while layer > 0 && has_stopped(arrivals, layer, stalled_after) {
    layer -= 1;
  }
  layer
}

/// Whether `layer` stopped: a lower layer kept coming for more than `stalled_after` without it. A
/// layer that never came counts as stopped once a lower one did (one the publisher starts only
/// now, or never sends). A pause of the whole track (a still screen, a camera that hiccups) is
/// none: the lower layer's run starts anew with the first packet after it.
fn has_stopped(arrivals: &LayerArrivals, layer: u8, stalled_after: Duration) -> bool {
  (0..layer).any(|lower| has_outlasted(arrivals, lower, layer, stalled_after))
}

/// The layer a receiver that wants `target` (and takes up to `max_layer`) should get: the best
/// lower layer that flows ([`live_layer`]), or for a camera whose layer stopped while nothing lower
/// flows, the lowest higher one up to `max_layer` that kept arriving without it. In a call of two
/// the publisher encodes only what its viewer takes: when that layer's encoder stopped, nothing
/// lower was there to stand in, and the viewer's picture stood still for good (#1719). A screen's
/// layers stand still with the screen and come back with it.
fn stand_in_layer(
  arrivals: &LayerArrivals,
  is_camera: bool,
  max_layer: u8,
  target: u8,
  stalled_after: Duration,
  now: Instant,
) -> u8 {
  let live = live_layer(arrivals, target, stalled_after);
  if !is_camera {
    return live;
  }
  (live.saturating_add(1)..=max_layer)
    .find(|other| {
      is_flowing(arrivals, *other, stalled_after, now) && has_outlasted(arrivals, *other, live, stalled_after)
    })
    .unwrap_or(live)
}

/// Whether nothing of a camera that sent before arrived within `stalled_after`: the layer its
/// receivers get may have stopped with no other to stand in. That layer is asked for again, every
/// layer a receiver may take is needed until one flows (another encoder may still work), and
/// receivers move to one that does ([`stand_in_layer`]) until theirs is back (#1719). A camera
/// that paused as a whole costs no more than a keyframe when it goes on.
fn is_starved(arrivals: &LayerArrivals, is_camera: bool, stalled_after: Duration, now: Instant) -> bool {
  let mut runs = arrivals.iter().flatten().peekable();
  is_camera
    && runs.peek().is_some()
    && runs.all(|run| now.saturating_duration_since(run.last) > stalled_after)
}

/// Whether packets of `layer` arrived within `stalled_after` before `now`.
fn is_flowing(arrivals: &LayerArrivals, layer: u8, stalled_after: Duration, now: Instant) -> bool {
  arrivals
    .get(usize::from(layer))
    .copied()
    .flatten()
    .is_some_and(|run| now.saturating_duration_since(run.last) <= stalled_after)
}

/// Whether `other` kept coming for more than `stalled_after` without `layer` (since its own run
/// started, for a `layer` that never came).
fn has_outlasted(arrivals: &LayerArrivals, other: u8, layer: u8, stalled_after: Duration) -> bool {
  let Some(other_run) = arrivals.get(usize::from(other)).copied().flatten() else {
    return false;
  };
  let maybe_last = arrivals
    .get(usize::from(layer))
    .copied()
    .flatten()
    .map(|run| run.last);
  let without = maybe_last.map_or(other_run.since, |last| last.max(other_run.since));
  other_run.last.saturating_duration_since(without) > stalled_after
}

#[cfg(test)]
mod tests {
  use teamcraft_media::{
    datagram::{DATAGRAM_HEADER_BYTES, MediaDatagram, SimulcastLayer, TransportSeq},
    fragment::fragment_frame,
    rtp::{PayloadType, RtpPacket, RtpTimestamp, SequenceNumber, Ssrc},
  };
  use teamcraft_proto::{MAX_VIDEO_FRAME_FRAGMENTS, SHORT_VIDEO_FRAME_FRAGMENTS};

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
      let removal = forwarder.remove_participant(device(1), now + Duration::from_millis(300));
      assert_eq!(removal.effects, vec![Effect::ActiveSpeakers(Vec::new())]);
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

  mod screen_audio {
    use super::*;

    const SCREEN_AUDIO: StreamId = StreamId(50);

    #[test]
    fn reaches_everyone_without_being_a_speaker() {
      let (mut forwarder, now) = room(5);
      forwarder.publish_screen_audio(device(1), SCREEN_AUDIO).unwrap();
      // Silent, and four louder speakers exist: still forwarded to all others.
      for n in 2..=5u64 {
        forwarder.route(device(n), &audio(n, 10), now).unwrap();
      }
      let route = forwarder
        .route(device(1), &rtp_datagram(SCREEN_AUDIO.0, 0, header()), now)
        .unwrap();
      let mut receivers: Vec<DeviceId> = route.deliveries.iter().map(|(id, _)| *id).collect();
      receivers.sort();
      let mut expected = devices(&[2, 3, 4, 5]);
      expected.sort();
      assert_eq!(receivers, expected);
      assert!(!forwarder.active_speakers().contains(&device(1)));
    }

    #[test]
    fn one_per_participant_and_only_its_own() {
      let (mut forwarder, now) = room(2);
      forwarder.publish_screen_audio(device(1), SCREEN_AUDIO).unwrap();
      assert!(matches!(
        forwarder.publish_screen_audio(device(1), StreamId(51)),
        Err(ForwardError::InvalidPublication(_))
      ));
      assert_eq!(
        forwarder
          .route(device(2), &rtp_datagram(SCREEN_AUDIO.0, 0, header()), now)
          .unwrap_err(),
        ForwardError::ForeignStream(SCREEN_AUDIO)
      );
      assert_eq!(
        forwarder.unpublish_screen_audio(device(2), SCREEN_AUDIO),
        Err(ForwardError::ForeignStream(SCREEN_AUDIO))
      );
      forwarder.unpublish_screen_audio(device(1), SCREEN_AUDIO).unwrap();
      assert!(forwarder.screen_audio_tracks().is_empty());
      assert!(
        forwarder
          .route(device(1), &rtp_datagram(SCREEN_AUDIO.0, 0, header()), now)
          .is_err()
      );
    }

    #[test]
    fn leaves_with_its_publisher() {
      let (mut forwarder, now) = room(2);
      forwarder.publish_screen_audio(device(1), SCREEN_AUDIO).unwrap();
      assert_eq!(forwarder.screen_audio_tracks().len(), 1);
      let removal = forwarder.remove_participant(device(1), now);
      assert_eq!(removal.maybe_screen_audio, Some(SCREEN_AUDIO));
      assert!(forwarder.screen_audio_tracks().is_empty());
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
    fn a_receivers_request_reaches_the_publisher_on_its_layer_rate_limited() {
      let (mut forwarder, now) = video_room();
      forwarder.subscribe(device(2), VIDEO, Some(0), now).unwrap();
      send_frame(&mut forwarder, 0, true, now);
      let later = now + Duration::from_secs(2);
      // A lost fragment: the receiver can't decode until the next keyframe.
      assert_eq!(
        forwarder.request_keyframe(device(2), VIDEO, later),
        vec![keyframe_request(0)]
      );
      // Asking again at once does nothing: keyframes are large.
      let soon = later + Duration::from_millis(300);
      assert!(forwarder.request_keyframe(device(2), VIDEO, soon).is_empty());
      let after_interval = later + KeyframeRequestConfig::default().per_receiver_interval;
      assert_eq!(
        forwarder.request_keyframe(device(2), VIDEO, after_interval),
        vec![keyframe_request(0)]
      );
    }

    #[test]
    fn a_lost_first_keyframe_can_be_asked_for_again_at_once() {
      let (mut forwarder, now) = video_room();
      let effects = forwarder.subscribe(device(2), VIDEO, Some(0), now).unwrap();
      assert!(effects.contains(&keyframe_request(0)));
      // The keyframe passes the SFU but gets lost on the way to the receiver, which asks again
      // right away: not merged with the subscription's request, that one was answered.
      let soon = now + Duration::from_millis(100);
      assert_eq!(send_frame(&mut forwarder, 0, true, soon), devices(&[2]));
      assert_eq!(
        forwarder.request_keyframe(device(2), VIDEO, soon),
        vec![keyframe_request(0)]
      );
    }

    #[test]
    fn requests_for_tracks_not_received_do_nothing() {
      let (mut forwarder, now) = video_room();
      assert!(forwarder.request_keyframe(device(2), VIDEO, now).is_empty());
      assert!(
        forwarder
          .request_keyframe(device(2), StreamId(999), now)
          .is_empty()
      );
    }

    #[test]
    fn subscribing_again_with_the_same_layer_asks_for_a_keyframe() {
      let (mut forwarder, now) = video_room();
      forwarder.subscribe(device(2), VIDEO, Some(0), now).unwrap();
      send_frame(&mut forwarder, 0, true, now);
      // The video was shown anew: same layer, so no switch, but a new decoder.
      let later = now + Duration::from_secs(2);
      let effects = forwarder.subscribe(device(2), VIDEO, Some(0), later).unwrap();
      assert!(effects.contains(&keyframe_request(0)), "{effects:?}");
      // Still forwarded on the layer it had: nothing waits for a switch.
      assert_eq!(send_frame(&mut forwarder, 0, false, later), devices(&[2]));
    }

    #[test]
    fn requests_of_two_receivers_on_one_layer_are_merged() {
      // Device 3 of the test room decodes no AV1: four participants, 2 and 4 receive.
      let (mut forwarder, now) = room(4);
      forwarder
        .publish_video(
          device(1),
          VIDEO,
          VideoTrackKind::Camera,
          VideoCodec::Av1,
          three_layers(),
          None,
        )
        .unwrap();
      forwarder.subscribe(device(2), VIDEO, Some(0), now).unwrap();
      forwarder.subscribe(device(4), VIDEO, Some(0), now).unwrap();
      send_frame(&mut forwarder, 0, true, now);
      let later = now + Duration::from_secs(2);
      assert_eq!(
        forwarder.request_keyframe(device(2), VIDEO, later),
        vec![keyframe_request(0)]
      );
      // The keyframe on its way serves both.
      assert!(forwarder.request_keyframe(device(4), VIDEO, later).is_empty());
    }

    #[test]
    fn publisher_leaving_removes_the_track() {
      let (mut forwarder, now) = video_room();
      let removal = forwarder.remove_participant(device(1), now);
      assert_eq!(removal.video_tracks, vec![VIDEO]);
      assert!(forwarder.video_tracks().is_empty());
    }

    fn keyframe_request(layer: u8) -> Effect {
      Effect::KeyframeRequest {
        publisher: device(1),
        stream_id: VIDEO,
        layer,
      }
    }
  }

  mod retransmission {
    use super::*;

    #[test]
    fn a_lost_packet_is_resent_to_the_asker_only() {
      let (mut forwarder, now) = video_room();
      // Device 3 decodes no AV1; device 4 does.
      forwarder.add_participant(device(4), stream(4), capabilities_av1(), now);
      on_layer_zero(&mut forwarder, &[2, 4], now);
      let sent = send_numbered(&mut forwarder, 0, 7, now);
      assert_eq!(receivers_of(&sent), devices(&[2, 4]));
      let resent = forwarder.resend(device(2), VIDEO, 0, &[7], now);
      assert_eq!(resent.len(), 1);
      let original = &sent.deliveries[0].1;
      let header = DatagramHeader::parse(&resent[0]).unwrap();
      assert_eq!(
        &resent[0][header.encoded_len()..],
        &original[header.encoded_len()..],
        "the same (encrypted) packet"
      );
      // Numbered for the asker's estimation, after what it got so far.
      let first = DatagramHeader::parse(original)
        .unwrap()
        .maybe_transport_seq
        .unwrap();
      assert!(header.maybe_transport_seq.is_some_and(|seq| seq != first));
      // Packet 8 was never forwarded: nothing to resend.
      assert!(forwarder.resend(device(4), VIDEO, 0, &[8], now).is_empty());
    }

    #[test]
    fn nothing_for_a_layer_or_track_not_received() {
      let (mut forwarder, now) = video_room();
      on_layer_zero(&mut forwarder, &[2], now);
      send_numbered(&mut forwarder, 0, 7, now);
      assert!(
        forwarder.resend(device(2), VIDEO, 1, &[7], now).is_empty(),
        "another layer"
      );
      assert!(
        forwarder.resend(device(3), VIDEO, 0, &[7], now).is_empty(),
        "not subscribed"
      );
      assert!(
        forwarder
          .resend(device(2), StreamId(999), 0, &[7], now)
          .is_empty(),
        "unknown"
      );
      assert!(
        forwarder.resend(device(2), VIDEO, 9, &[7], now).is_empty(),
        "no such layer"
      );
    }

    #[test]
    fn nothing_after_the_window_or_while_suspended() {
      let (mut forwarder, now) = video_room();
      on_layer_zero(&mut forwarder, &[2], now);
      send_numbered(&mut forwarder, 0, 7, now);
      let window = ForwardingConfig::default().retransmit.window;
      let late = now + window + Duration::from_millis(1);
      assert!(forwarder.resend(device(2), VIDEO, 0, &[7], late).is_empty());
      send_numbered(&mut forwarder, 0, 8, now);
      forwarder.suspend(device(2), now);
      assert!(forwarder.resend(device(2), VIDEO, 0, &[8], now).is_empty());
    }

    #[test]
    fn packets_nobody_receives_are_not_kept() {
      let (mut forwarder, now) = video_room();
      send_numbered(&mut forwarder, 0, 7, now);
      assert!(forwarder.video_tracks[&VIDEO].history[0].is_empty());
    }

    #[test]
    fn a_receiver_that_keeps_losing_gets_no_more_than_its_budget() {
      let (mut forwarder, now) = video_room();
      on_layer_zero(&mut forwarder, &[2], now);
      let mut datagram_bytes = 0;
      for sequence_number in 0..200 {
        datagram_bytes = send_numbered(&mut forwarder, 0, sequence_number, now).deliveries[0]
          .1
          .len();
      }
      forwarder.config.retransmit.burst_bytes = u32::try_from(10 * datagram_bytes).unwrap();
      let all: Vec<u16> = (0..200).collect();
      let resent = forwarder.resend(device(2), VIDEO, 0, &all, now);
      assert_eq!(resent.len(), 10, "the burst");
      // Asking again at once gets (almost) nothing more: the budget refills with time.
      let again = forwarder.resend(device(2), VIDEO, 0, &all, now);
      assert!(again.is_empty(), "{} more resent", again.len());
    }

    #[test]
    fn no_more_numbers_than_a_nack_may_name_are_looked_at() {
      let (mut forwarder, now) = video_room();
      on_layer_zero(&mut forwarder, &[2], now);
      for sequence_number in 0..10 {
        send_numbered(&mut forwarder, 0, sequence_number, now);
      }
      // The packets kept come after a long run of numbers that aren't.
      let mut asked: Vec<u16> = (1000..1000 + MAX_NACK_SEQUENCE_NUMBERS as u16).collect();
      asked.extend(0..10);
      assert!(forwarder.resend(device(2), VIDEO, 0, &asked, now).is_empty());
    }

    /// `receivers` subscribe to [`VIDEO`] and receive its layer 0 from a keyframe on.
    fn on_layer_zero(forwarder: &mut Forwarder, receivers: &[u64], now: Instant) {
      for &receiver in receivers {
        forwarder
          .subscribe(device(receiver), VIDEO, Some(0), now)
          .unwrap();
      }
      assert_eq!(send_frame(forwarder, 0, true, now), devices(receivers));
    }

    /// One delta of a single fragment on `layer` of [`VIDEO`] with RTP `sequence_number`.
    fn send_numbered(forwarder: &mut Forwarder, layer: u8, sequence_number: u16, now: Instant) -> Route {
      let mut rtp = header();
      rtp.sequence_number = SequenceNumber(sequence_number);
      FrameFragment {
        frame_number: sequence_number,
        index: 0,
        count: 1,
        is_keyframe: false,
      }
      .write_to(&mut rtp);
      forwarder
        .route(device(1), &rtp_datagram(VIDEO.0, layer, rtp), now)
        .unwrap()
    }

    fn receivers_of(route: &Route) -> Vec<DeviceId> {
      route.deliveries.iter().map(|(receiver, _)| *receiver).collect()
    }
  }

  mod sent_layers {
    use super::*;

    #[test]
    fn a_layer_that_stops_falls_back_to_the_best_lower_one_that_flows() {
      let (mut forwarder, now) = wide_video_room();
      on_top_layer(&mut forwarder, now);
      // The publisher's uplink pauses layer 2; layers 0 and 1 keep coming.
      let mut at = now;
      for _ in 0..10 {
        at += Duration::from_millis(100);
        send_frame(&mut forwarder, 0, false, at);
        send_frame(&mut forwarder, 1, false, at);
      }
      // A second without layer 2 is no reason yet (a still screen sends once a second).
      assert!(!forwarder.on_timer(device(2), at).contains(&keyframe_request(1)));
      for _ in 0..6 {
        at += Duration::from_millis(100);
        send_frame(&mut forwarder, 0, false, at);
        send_frame(&mut forwarder, 1, false, at);
      }
      let effects = forwarder.on_timer(device(2), at);
      assert!(effects.contains(&keyframe_request(1)), "{effects:?}");
      assert!(effects.contains(&layers_needed(&[1, 2])), "{effects:?}");
      // The receiver moves to layer 1 on its keyframe, and gets nothing of layer 0.
      assert!(send_frame(&mut forwarder, 1, false, at).is_empty());
      assert_eq!(send_frame(&mut forwarder, 1, true, at), devices(&[2]));
      assert_eq!(send_frame(&mut forwarder, 1, false, at), devices(&[2]));
      assert!(send_frame(&mut forwarder, 0, false, at).is_empty());
      // Layer 2 is back: the receiver returns to it on its keyframe (asked for meanwhile).
      at += Duration::from_millis(500);
      assert!(forwarder.on_timer(device(2), at).contains(&keyframe_request(2)));
      assert!(send_frame(&mut forwarder, 2, false, at).is_empty());
      assert_eq!(send_frame(&mut forwarder, 2, true, at), devices(&[2]));
      assert!(send_frame(&mut forwarder, 1, false, at).is_empty());
    }

    #[test]
    fn a_new_receiver_starts_on_a_layer_that_flows() {
      let (mut forwarder, now) = wide_video_room();
      // Only layer 0 was needed so far (and sent); now someone wants layer 2.
      for step in 0..20 {
        send_frame(
          &mut forwarder,
          0,
          step == 0,
          now + Duration::from_millis(100 * step),
        );
      }
      let at = now + Duration::from_secs(2);
      let effects = forwarder.subscribe(device(2), VIDEO, Some(2), at).unwrap();
      assert!(effects.contains(&keyframe_request(0)), "{effects:?}");
      assert!(effects.contains(&keyframe_request(2)), "{effects:?}");
      assert!(effects.contains(&layers_needed(&[0, 2])), "{effects:?}");
      assert_eq!(send_frame(&mut forwarder, 0, true, at), devices(&[2]));
      // Layer 2 starts with the keyframe asked for: the receiver moves up.
      assert_eq!(send_frame(&mut forwarder, 2, true, at), devices(&[2]));
      assert!(send_frame(&mut forwarder, 0, false, at).is_empty());
    }

    #[test]
    fn a_still_track_keeps_its_layer() {
      let (mut forwarder, now) = wide_video_room();
      on_top_layer(&mut forwarder, now);
      // Nothing of any layer for a while, then the same picture again on every layer.
      let later = now + Duration::from_secs(5);
      send_frame(&mut forwarder, 0, false, later);
      send_frame(&mut forwarder, 1, false, later);
      let effects = forwarder.on_timer(device(2), later);
      assert!(
        !effects
          .iter()
          .any(|effect| matches!(effect, Effect::KeyframeRequest { .. })),
        "{effects:?}"
      );
      assert_eq!(
        send_frame(&mut forwarder, 2, false, later + Duration::from_millis(5)),
        devices(&[2])
      );
    }

    /// #1719: in a call of two the publisher encodes only the layer its viewer gets. When that
    /// layer's encoder stops, nothing lower flows to stand in: the viewer's picture stood still
    /// for good. Now the layer is asked for again, every layer the viewer may take is needed, and
    /// the viewer moves to one that flows until its own is back.
    #[test]
    fn a_camera_layer_that_stops_with_nothing_else_flowing_is_replaced() {
      let (mut forwarder, now) = wide_video_room();
      forwarder.subscribe(device(2), VIDEO, Some(1), now).unwrap();
      let mut at = now;
      assert_eq!(send_frame(&mut forwarder, 1, true, at), devices(&[2]));
      forwarder.on_timer(device(2), at);
      for _ in 0..10 {
        at += Duration::from_millis(100);
        assert_eq!(send_frame(&mut forwarder, 1, false, at), devices(&[2]));
        forwarder.on_timer(device(2), at);
      }
      // Layer 1's encoder stops; nothing of the camera arrives.
      at += Duration::from_millis(1000);
      let effects = forwarder.on_timer(device(2), at);
      assert!(
        !effects.contains(&layers_needed(&[0, 1])),
        "too soon: {effects:?}"
      );
      at += Duration::from_millis(600);
      let effects = forwarder.on_timer(device(2), at);
      assert!(effects.contains(&keyframe_request(1)), "{effects:?}");
      assert!(effects.contains(&layers_needed(&[0, 1])), "{effects:?}");
      // Layer 0's encoder works: once it outlasted layer 1, the viewer moves to it.
      send_frame(&mut forwarder, 0, true, at);
      for _ in 0..16 {
        at += Duration::from_millis(100);
        send_frame(&mut forwarder, 0, false, at);
        forwarder.on_timer(device(2), at);
      }
      assert_eq!(send_frame(&mut forwarder, 0, true, at), devices(&[2]));
      assert_eq!(send_frame(&mut forwarder, 0, false, at), devices(&[2]));
      // Layer 1 is back: the viewer returns to it on its keyframe.
      at += Duration::from_millis(100);
      assert!(send_frame(&mut forwarder, 1, false, at).is_empty());
      assert_eq!(send_frame(&mut forwarder, 1, true, at), devices(&[2]));
      assert!(send_frame(&mut forwarder, 0, false, at).is_empty());
    }

    #[test]
    fn the_publishers_uplink_layers_cap_the_allocation() {
      let (mut forwarder, now) = wide_video_room();
      on_top_layer(&mut forwarder, now);
      let effects = forwarder
        .set_uplink_layers(device(1), VIDEO, &[0, 1], now)
        .unwrap();
      assert!(effects.contains(&keyframe_request(1)), "{effects:?}");
      assert!(effects.contains(&layers_needed(&[1, 2])), "{effects:?}");
      assert_eq!(send_frame(&mut forwarder, 1, true, now), devices(&[2]));
      // Moved: layer 2 is no longer needed, so the publisher may keep it paused.
      assert!(forwarder.on_timer(device(2), now).contains(&layers_needed(&[1])));
      // The same report again changes nothing.
      assert!(
        forwarder
          .set_uplink_layers(device(1), VIDEO, &[0, 1], now)
          .unwrap()
          .is_empty()
      );
      // The uplink recovered: layer 2 comes back after the usual upswitch hold.
      let effects = forwarder
        .set_uplink_layers(device(1), VIDEO, &[0, 1, 2], now)
        .unwrap();
      assert!(!effects.contains(&keyframe_request(2)));
      let later = now + LayerAllocationConfig::default().upswitch_hold;
      // Layer 1 kept coming meanwhile (a camera that sends nothing is starved, #1719).
      send_frame(&mut forwarder, 1, false, later);
      let effects = forwarder.on_timer(device(2), later);
      assert!(effects.contains(&keyframe_request(2)), "{effects:?}");
      assert!(effects.contains(&layers_needed(&[1, 2])), "{effects:?}");
    }

    /// Issue #939: a switch waiting for its keyframe doesn't ask for another every half second:
    /// a large one takes longer than that to come on a slow machine, and each request cost one
    /// more keyframe.
    #[test]
    fn a_switch_waiting_for_its_keyframe_asks_again_only_after_a_while() {
      let (mut forwarder, now) = wide_video_room();
      on_top_layer(&mut forwarder, now);
      let effects = forwarder
        .set_uplink_layers(device(1), VIDEO, &[0, 1], now)
        .unwrap();
      assert!(effects.contains(&keyframe_request(1)), "{effects:?}");
      let retry = KeyframeRequestConfig::default().switch_retry_interval;
      // Not within one and a half seconds (it was every half second, `per_layer_interval`).
      let mut at = now;
      while at + Duration::from_millis(100) < now + Duration::from_millis(1_500) {
        at += Duration::from_millis(100);
        let effects = forwarder.on_timer(device(2), at);
        assert!(
          !effects.contains(&keyframe_request(1)),
          "asked again after {:?}",
          at - now
        );
      }
      let effects = forwarder.on_timer(device(2), now + retry);
      assert!(effects.contains(&keyframe_request(1)), "{effects:?}");
    }

    #[test]
    fn a_publisher_without_any_uplink_layer_pauses_its_receivers() {
      let (mut forwarder, now) = wide_video_room();
      on_top_layer(&mut forwarder, now);
      let effects = forwarder.set_uplink_layers(device(1), VIDEO, &[], now).unwrap();
      assert!(effects.contains(&layers_needed(&[])), "{effects:?}");
      assert!(send_frame(&mut forwarder, 2, true, now).is_empty());
      // Back: the lowest layer, after the usual upswitch hold.
      forwarder.set_uplink_layers(device(1), VIDEO, &[0], now).unwrap();
      let later = now + LayerAllocationConfig::default().upswitch_hold;
      let effects = forwarder.on_timer(device(2), later);
      assert!(effects.contains(&keyframe_request(0)), "{effects:?}");
      assert_eq!(send_frame(&mut forwarder, 0, true, later), devices(&[2]));
    }

    #[test]
    fn uplink_layers_of_others_and_unknown_tracks_are_rejected() {
      let (mut forwarder, now) = wide_video_room();
      assert_eq!(
        forwarder.set_uplink_layers(device(2), VIDEO, &[0], now),
        Err(ForwardError::ForeignStream(VIDEO))
      );
      assert_eq!(
        forwarder.set_uplink_layers(device(1), StreamId(999), &[0], now),
        Err(ForwardError::UnknownTrack(StreamId(999)))
      );
    }

    /// Issue #941: a keyframe of a text-heavy share has more than 255 fragments. A receiver
    /// from before the long form can't reassemble it: it gets the best layer without such
    /// frames instead of waiting for keyframes forever. A current one gets the layer.
    #[test]
    fn a_layer_with_frames_too_large_for_a_receiver_is_left_out_for_it() {
      let (mut forwarder, now) = wide_video_room();
      let older = MediaCapabilities {
        max_frame_fragments: SHORT_VIDEO_FRAME_FRAGMENTS,
        ..forwarder.participants[&device(2)].capabilities.clone()
      };
      forwarder.resume(device(2), older.clone(), now);
      on_top_layer(&mut forwarder, now);
      assert!(send_fragment(&mut forwarder, 2, true, 300, now).is_empty());
      let effects = forwarder.on_timer(device(2), now);
      assert!(effects.contains(&keyframe_request(1)), "{effects:?}");
      assert_eq!(send_frame(&mut forwarder, 1, true, now), devices(&[2]));
      assert!(send_frame(&mut forwarder, 2, false, now).is_empty());
      let effects = forwarder.on_timer(device(2), now);
      assert!(effects.contains(&layers_needed(&[1])), "{effects:?}");
      // A receiver that reassembles them gets the top layer.
      let current = MediaCapabilities {
        max_frame_fragments: MAX_VIDEO_FRAME_FRAGMENTS,
        ..older
      };
      forwarder.resume(device(2), current, now);
      let effects = forwarder.on_timer(device(2), now);
      assert!(effects.contains(&keyframe_request(2)), "{effects:?}");
      assert_eq!(send_fragment(&mut forwarder, 2, true, 300, now), devices(&[2]));
    }

    /// Device 2 receives layer 2 of [`VIDEO`] from `now` on.
    fn on_top_layer(forwarder: &mut Forwarder, now: Instant) {
      forwarder.subscribe(device(2), VIDEO, Some(2), now).unwrap();
      send_frame(forwarder, 0, true, now);
      send_frame(forwarder, 1, true, now);
      assert_eq!(send_frame(forwarder, 2, true, now), devices(&[2]));
    }

    /// [`video_room`] with downlinks that fit every layer from the start.
    fn wide_video_room() -> (Forwarder, Instant) {
      let mut config = ForwardingConfig::default();
      config.bwe.start_bps = 10_000_000;
      config.bwe.max_bps = config.bwe.max_bps.max(10_000_000);
      let (mut forwarder, now) = video_room();
      forwarder.config = config;
      for n in 1..=3 {
        let capabilities = forwarder.participants[&device(n)].capabilities.clone();
        forwarder.resume(device(n), capabilities, now);
      }
      (forwarder, now)
    }

    fn keyframe_request(layer: u8) -> Effect {
      Effect::KeyframeRequest {
        publisher: device(1),
        stream_id: VIDEO,
        layer,
      }
    }

    fn layers_needed(layers: &[u8]) -> Effect {
      Effect::LayersNeeded {
        publisher: device(1),
        stream_id: VIDEO,
        layers: layers.to_vec(),
      }
    }
  }

  mod stand_ins {
    use super::*;

    const STALLED_AFTER: Duration = Duration::from_millis(1500);

    #[test]
    fn a_camera_layer_that_stopped_is_stood_in_for_by_a_higher_one_that_kept_coming() {
      let start = Instant::now();
      let now = start + Duration::from_secs(4);
      // Layer 0 stopped after a second; layer 1 kept coming.
      let arrivals = runs(start, &[Some((0, 1000)), Some((0, 4000))]);
      assert_eq!(stand_in_layer(&arrivals, true, 1, 0, STALLED_AFTER, now), 1);
      // Not above what the receiver takes, and a screen's layers come back with the screen.
      assert_eq!(stand_in_layer(&arrivals, true, 0, 0, STALLED_AFTER, now), 0);
      assert_eq!(stand_in_layer(&arrivals, false, 1, 0, STALLED_AFTER, now), 0);
      // A higher layer that only just started is no reason yet (a camera that hiccups as a whole).
      let arrivals = runs(start, &[Some((0, 1000)), Some((3000, 4000))]);
      assert_eq!(stand_in_layer(&arrivals, true, 1, 0, STALLED_AFTER, now), 0);
    }

    #[test]
    fn a_camera_is_starved_once_nothing_of_it_arrived_for_a_while() {
      let start = Instant::now();
      let arrivals = runs(start, &[Some((0, 1000)), None]);
      assert!(!is_starved(
        &arrivals,
        true,
        STALLED_AFTER,
        start + Duration::from_millis(2500)
      ));
      assert!(is_starved(
        &arrivals,
        true,
        STALLED_AFTER,
        start + Duration::from_millis(2600)
      ));
      assert!(!is_starved(
        &arrivals,
        false,
        STALLED_AFTER,
        start + Duration::from_secs(5)
      ));
      // Nothing ever came: nothing to restart.
      assert!(!is_starved(
        &runs(start, &[]),
        true,
        STALLED_AFTER,
        start + Duration::from_secs(5)
      ));
      // One layer still flows.
      let arrivals = runs(start, &[Some((0, 1000)), Some((0, 4900))]);
      assert!(!is_starved(
        &arrivals,
        true,
        STALLED_AFTER,
        start + Duration::from_secs(5)
      ));
    }

    /// Arrivals with a run per layer, (since, last) in milliseconds after `start`.
    fn runs(start: Instant, layers: &[Option<(u64, u64)>]) -> LayerArrivals {
      let mut arrivals: LayerArrivals = [None; MAX_SIMULCAST_LAYERS as usize];
      for (slot, maybe_run) in arrivals.iter_mut().zip(layers) {
        *slot = maybe_run.map(|(since, last)| LayerRun {
          since: start + Duration::from_millis(since),
          last: start + Duration::from_millis(last),
        });
      }
      arrivals
    }
  }

  mod reconnecting {
    use super::*;

    #[test]
    fn a_resumed_publisher_hears_which_layers_are_needed() {
      let (mut forwarder, now) = video_room();
      forwarder.subscribe(device(2), VIDEO, Some(0), now).unwrap();
      forwarder.suspend(device(1), now);
      // Changed while the publisher is away: the room can't tell it.
      let effects = forwarder.subscribe(device(2), VIDEO, None, now).unwrap();
      assert!(
        effects
          .iter()
          .any(|effect| matches!(effect, Effect::LayersNeeded { .. }))
      );
      let effects = forwarder.resume(device(1), capabilities_av1_encoder(), now);
      assert_eq!(
        effects,
        vec![Effect::LayersNeeded {
          publisher: device(1),
          stream_id: VIDEO,
          layers: Vec::new(),
        }]
      );
      // A device without tracks hears nothing.
      assert!(forwarder.resume(device(2), capabilities_av1(), now).is_empty());
    }

    #[test]
    fn suspended_receiver_gets_nothing_until_it_resumes() {
      let (mut forwarder, now) = room(3);
      let speak = |forwarder: &mut Forwarder, at| {
        let mut receivers: Vec<DeviceId> = forwarder
          .route(device(1), &audio(1, 10), at)
          .unwrap()
          .deliveries
          .into_iter()
          .map(|(receiver, _)| receiver)
          .collect();
        receivers.sort();
        receivers
      };
      assert_eq!(speak(&mut forwarder, now), devices(&[2, 3]));
      forwarder.suspend(device(2), now);
      assert_eq!(speak(&mut forwarder, now), devices(&[3]));
      forwarder.resume(device(2), MediaCapabilities::default(), now);
      assert_eq!(speak(&mut forwarder, now), devices(&[2, 3]));
    }

    #[test]
    fn suspended_speaker_leaves_the_speaker_selection() {
      let (mut forwarder, now) = room(2);
      forwarder.route(device(1), &audio(1, 10), now).unwrap();
      assert_eq!(forwarder.active_speakers(), devices(&[1]));
      let effects = forwarder.suspend(device(1), now);
      assert!(effects.contains(&Effect::ActiveSpeakers(Vec::new())));
    }

    #[test]
    fn subscription_pauses_while_suspended_and_resumes_with_a_keyframe() {
      let (mut forwarder, now) = video_room();
      forwarder.subscribe(device(2), VIDEO, Some(0), now).unwrap();
      assert_eq!(send_frame(&mut forwarder, 0, true, now), devices(&[2]));
      let effects = forwarder.suspend(device(2), now);
      assert!(effects.contains(&Effect::LayersNeeded {
        publisher: device(1),
        stream_id: VIDEO,
        layers: Vec::new(),
      }));
      assert!(send_frame(&mut forwarder, 0, true, now).is_empty());
      // The allocation timer does nothing for a suspended receiver.
      assert!(forwarder.on_timer(device(2), now).is_empty());
      // Back a second later (keyframe requests are rate limited per layer).
      let later = now + Duration::from_secs(1);
      forwarder.resume(device(2), capabilities_av1(), later);
      let effects = forwarder.on_timer(device(2), later);
      assert!(effects.contains(&Effect::KeyframeRequest {
        publisher: device(1),
        stream_id: VIDEO,
        layer: 0,
      }));
      assert!(send_frame(&mut forwarder, 0, false, later).is_empty());
      assert_eq!(send_frame(&mut forwarder, 0, true, later), devices(&[2]));
    }

    #[test]
    fn tracks_of_a_suspended_publisher_stay_published() {
      let (mut forwarder, now) = video_room();
      forwarder.suspend(device(1), now);
      assert_eq!(forwarder.video_tracks().len(), 1);
    }

    #[test]
    fn resumed_receiver_starts_with_a_fresh_estimate() {
      let (mut forwarder, now) = room(2);
      let start = forwarder.downlink_estimate_bps(device(2)).unwrap();
      // Losing half of everything for a while drives the estimate down.
      for round in 0..30u16 {
        let at = now + Duration::from_millis(100 * u64::from(round));
        for _ in 0..10 {
          forwarder.route(device(1), &audio(1, 10), at).unwrap();
        }
        let arrivals_us = (0..10u64)
          .map(|i| (i % 2 == 0).then_some(u64::from(round) * 100_000 + i * 1_000))
          .collect();
        let feedback = TransportFeedback {
          base_seq: round * 10,
          arrivals_us,
        };
        forwarder.on_transport_feedback(device(2), &feedback, at);
      }
      assert!(forwarder.downlink_estimate_bps(device(2)).unwrap() < start);
      forwarder.suspend(device(2), now);
      forwarder.resume(device(2), MediaCapabilities::default(), now);
      assert_eq!(forwarder.downlink_estimate_bps(device(2)), Some(start));
    }

    /// Issue #941: a call of sound only, and a lossy moment cut the estimate of a receiver's
    /// downlink to what the sound got through. Subscribing to a share measures it afresh, so the
    /// share isn't held on its half-size layer for minutes; changing the layer asked for doesn't.
    #[test]
    fn a_new_subscription_measures_the_downlink_afresh() {
      let (mut forwarder, now) = video_room();
      let start = forwarder.downlink_estimate_bps(device(2)).unwrap();
      let mut at = lossy_sound(&mut forwarder, now, 0);
      let cut = forwarder.downlink_estimate_bps(device(2)).unwrap();
      assert!(cut < start, "{cut}");
      forwarder.subscribe(device(2), VIDEO, Some(2), at).unwrap();
      assert_eq!(forwarder.downlink_estimate_bps(device(2)), Some(start));
      at = lossy_sound(&mut forwarder, at, 300);
      let cut = forwarder.downlink_estimate_bps(device(2)).unwrap();
      assert!(cut < start, "{cut}");
      forwarder.subscribe(device(2), VIDEO, Some(1), at).unwrap();
      assert_eq!(forwarder.downlink_estimate_bps(device(2)), Some(cut));
    }

    /// Issue #939: while a receiver's downlink estimate is in its fast start, a layer comes
    /// about half a second after the estimate covers it, not after the usual two.
    #[test]
    fn during_the_fast_start_a_layer_comes_soon_after_the_estimate_covers_it() {
      let (mut forwarder, now) = video_room();
      forwarder.subscribe(device(2), VIDEO, Some(2), now).unwrap();
      let epoch = forwarder.participants[&device(2)].feedback_epoch;
      let config = LayerAllocationConfig::default();
      // What the middle layer (500 kbit/s) needs.
      let covered_bps = f64::from(config.audio_reserve_bps) + 500_000.0 * config.upswitch_headroom;
      let (mut maybe_covered_at, mut maybe_switched_at) = (None, None);
      // Everything sent to the receiver arrives, padding included, and is reported in order.
      let mut next_to_report = 0u16;
      let mut sent = 0u16;
      for round in 0..60u16 {
        let at = now + Duration::from_millis(100 * u64::from(round));
        for _ in 0..10 {
          let route = forwarder.route(device(1), &audio(1, 10), at).unwrap();
          sent += u16::try_from(route.deliveries.iter().filter(|(to, _)| *to == device(2)).count()).unwrap();
        }
        let since_epoch = u64::try_from(at.saturating_duration_since(epoch).as_micros()).unwrap();
        let feedback = TransportFeedback {
          base_seq: next_to_report,
          arrivals_us: (0..u64::from(sent - next_to_report))
            .map(|i| Some(since_epoch + i * 100))
            .collect(),
        };
        next_to_report = sent;
        let mut effects = forwarder.on_transport_feedback(device(2), &feedback, at);
        effects.extend(forwarder.on_timer(device(2), at));
        for effect in &effects {
          if let Effect::Padding { datagrams, .. } = effect {
            sent += u16::try_from(datagrams.len()).unwrap();
          }
        }
        let estimate = forwarder.downlink_estimate_bps(device(2)).unwrap();
        if maybe_covered_at.is_none() && f64::from(estimate) >= covered_bps {
          maybe_covered_at = Some(at);
        }
        let middle_keyframe = Effect::KeyframeRequest {
          publisher: device(1),
          stream_id: VIDEO,
          layer: 1,
        };
        if maybe_switched_at.is_none() && effects.contains(&middle_keyframe) {
          maybe_switched_at = Some(at);
        }
      }
      let (Some(covered_at), Some(switched_at)) = (maybe_covered_at, maybe_switched_at) else {
        panic!("covered {maybe_covered_at:?}, switched {maybe_switched_at:?}");
      };
      let delay = switched_at.saturating_duration_since(covered_at);
      assert!(
        delay <= Duration::from_millis(700),
        "switched {delay:?} after the estimate covered it"
      );
    }

    /// Three seconds of sound to device 2, numbered from `base`, of which half is lost; when
    /// they ended.
    fn lossy_sound(forwarder: &mut Forwarder, from: Instant, base: u16) -> Instant {
      let epoch = forwarder.participants[&device(2)].feedback_epoch;
      let mut at = from;
      for round in 0..30u16 {
        at = from + Duration::from_millis(100 * u64::from(round));
        for _ in 0..10 {
          forwarder.route(device(1), &audio(1, 10), at).unwrap();
        }
        let since_epoch = u64::try_from(at.saturating_duration_since(epoch).as_micros()).unwrap();
        let arrivals_us = (0..10u64)
          .map(|i| (i % 2 == 0).then_some(since_epoch + i * 1_000))
          .collect();
        let feedback = TransportFeedback {
          base_seq: base.wrapping_add(round * 10),
          arrivals_us,
        };
        forwarder.on_transport_feedback(device(2), &feedback, at);
      }
      at
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
    fn a_participant_publishes_a_bounded_number_of_tracks() {
      let (mut forwarder, _) = video_room();
      let layers = three_layers();
      // `video_room` already has one track of device 1.
      for id in 1..MAX_VIDEO_TRACKS_PER_PARTICIPANT {
        let stream = StreamId(100 + u32::try_from(id).unwrap());
        assert!(
          forwarder
            .publish_video(
              device(1),
              stream,
              VideoTrackKind::Camera,
              VideoCodec::Av1,
              layers.clone(),
              None
            )
            .is_ok()
        );
      }
      assert!(matches!(
        forwarder.publish_video(
          device(1),
          StreamId(999),
          VideoTrackKind::Camera,
          VideoCodec::Av1,
          layers.clone(),
          None
        ),
        Err(ForwardError::InvalidPublication(_))
      ));
      // Re-publishing an existing stream is not a new track.
      assert!(
        forwarder
          .publish_video(
            device(1),
            StreamId(100),
            VideoTrackKind::Camera,
            VideoCodec::Av1,
            layers,
            None
          )
          .is_ok()
      );
    }

    /// #978: up to four layers, numbered from 0; a fifth is refused.
    #[test]
    fn four_layers_are_taken_and_five_refused() {
      let (mut forwarder, _) = room(2);
      let publish = |forwarder: &mut Forwarder, stream: u32, layers: Vec<VideoLayerInfo>| {
        forwarder.publish_video(
          device(1),
          StreamId(stream),
          VideoTrackKind::Camera,
          VideoCodec::Av1,
          layers,
          None,
        )
      };
      let track = publish(&mut forwarder, 100, four_layers()).unwrap();
      assert_eq!(track.layers.len(), 4);
      let mut five = four_layers();
      five.push(VideoLayerInfo {
        layer: 4,
        ..five[3].clone()
      });
      assert_eq!(
        publish(&mut forwarder, 101, five),
        Err(ForwardError::InvalidPublication("needs 1 to 4 layers"))
      );
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
          layers.clone(),
          None
        ),
        Err(ForwardError::InvalidPublication(_))
      ));
      assert!(matches!(
        forwarder.publish_video(
          device(1),
          StreamId(7),
          VideoTrackKind::Camera,
          VideoCodec::Av1,
          layers[1..].to_vec(),
          None
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
    fn stream_ids_in_use_are_known() {
      let (mut forwarder, _) = video_room();
      // Microphones, the video track and screen sound all count.
      assert!(forwarder.is_stream_in_use(StreamId(2)));
      assert!(forwarder.is_stream_in_use(VIDEO));
      forwarder.publish_screen_audio(device(2), StreamId(77)).unwrap();
      assert!(forwarder.is_stream_in_use(StreamId(77)));
      assert!(!forwarder.is_stream_in_use(StreamId(78)));
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

  /// The probe of a participant's link at the start (#1762).
  mod join_probe {
    use super::*;

    #[test]
    fn a_senders_padding_is_reported_and_forwarded_nowhere() {
      let (mut forwarder, now) = room(3);
      for seq in 0..5u16 {
        let route = forwarder
          .route(device(1), &padding_datagram(1000, TransportSeq(seq)), now)
          .unwrap();
        assert!(route.deliveries.is_empty() && route.effects.is_empty());
      }
      let feedback = forwarder.take_uplink_feedback(device(1), now).unwrap();
      assert_eq!((feedback.base_seq, feedback.arrivals_us.len()), (0, 5));
      assert_eq!(forwarder.probe_bytes(), Some(15_000));
    }

    #[test]
    fn a_new_receivers_estimate_starts_from_its_trains() {
      let (mut forwarder, start) = room(2);
      assert!(
        paddings(&forwarder.on_timer(device(2), start)).is_empty(),
        "no feedback yet"
      );
      // The first feedback brings the first train.
      let mut first_train = Some(paddings(&give_feedback(&mut forwarder, 2, start)));
      let mut measured = Vec::new();
      let mut at = start;
      for _ in 0..3 {
        let trains = first_train
          .take()
          .unwrap_or_else(|| paddings(&forwarder.on_timer(device(2), at)));
        assert_eq!(trains.len(), 1, "one train at a time");
        assert_eq!(trains[0].len(), 5);
        assert!(paddings(&forwarder.on_timer(device(2), at + Duration::from_millis(50))).is_empty());
        // A 2 Mbit/s link spaces 1000 bytes 4 ms apart.
        let first = DatagramHeader::parse(&trains[0][0])
          .unwrap()
          .maybe_transport_seq
          .unwrap();
        let feedback = TransportFeedback {
          base_seq: first.0,
          arrivals_us: (0..5).map(|index| Some(index * 4000)).collect(),
        };
        measured.extend(
          forwarder
            .on_transport_feedback(device(2), &feedback, at + Duration::from_millis(60))
            .into_iter()
            .filter_map(|effect| match effect {
              Effect::DownlinkMeasured {
                receiver,
                measurement,
              } => Some((receiver, measurement)),
              _ => None,
            }),
        );
        at += Duration::from_millis(150);
      }
      assert_eq!(measured.len(), 1, "told once, after the last train");
      let (receiver, measurement) = measured[0];
      assert_eq!(receiver, device(2));
      assert_eq!(measurement.capacity_bps, 2_000_000);
      assert_eq!(forwarder.downlink_estimate_bps(device(2)), Some(1_900_000));
      assert!(paddings(&forwarder.on_timer(device(2), at + Duration::from_secs(1))).is_empty());
    }

    #[test]
    fn a_suspended_receiver_is_not_probed_and_a_resumed_one_afresh() {
      let (mut forwarder, now) = room(2);
      give_feedback(&mut forwarder, 2, now);
      forwarder.suspend(device(2), now);
      assert!(paddings(&forwarder.on_timer(device(2), now)).is_empty());
      forwarder.resume(device(2), capabilities_av1(), now);
      assert_eq!(paddings(&give_feedback(&mut forwarder, 2, now)).len(), 1);
    }

    #[test]
    fn nothing_is_probed_when_turned_off() {
      let now = Instant::now();
      let mut forwarder = Forwarder::new(ForwardingConfig {
        probe_bytes: 0,
        ..ForwardingConfig::default()
      });
      forwarder.add_participant(device(1), stream(1), capabilities_av1(), now);
      assert!(paddings(&give_feedback(&mut forwarder, 1, now)).is_empty());
      assert_eq!(forwarder.probe_bytes(), None);
    }

    /// `receiver` reports its arrivals (none yet), so it can be measured; what that brings.
    fn give_feedback(forwarder: &mut Forwarder, receiver: u64, now: Instant) -> Vec<Effect> {
      let feedback = TransportFeedback {
        base_seq: 0,
        arrivals_us: Vec::new(),
      };
      forwarder.on_transport_feedback(device(receiver), &feedback, now)
    }

    fn paddings(effects: &[Effect]) -> Vec<Vec<Vec<u8>>> {
      effects
        .iter()
        .filter_map(|effect| match effect {
          Effect::Padding { datagrams, .. } => Some(datagrams.clone()),
          _ => None,
        })
        .collect()
    }
  }

  /// #1733: a screen share's base layer in H.264 every device decodes, its large layers in AV1.
  mod layer_codecs {
    use teamcraft_proto::VideoFeature;

    use super::*;

    /// Each viewer gets the best layer it decodes and takes: AV1 for one that knows layer codecs
    /// and decodes AV1, the H.264 base for one without AV1 and for a client from before.
    #[test]
    fn each_viewer_gets_the_best_layer_in_a_codec_it_takes() {
      let (mut forwarder, now) = mixed_room();
      for viewer in 2..=4 {
        forwarder.subscribe(device(viewer), VIDEO, Some(2), now).unwrap();
      }
      for viewer in 2..=4 {
        forwarder.on_timer(device(viewer), now);
      }
      assert_eq!(send_frame(&mut forwarder, 0, true, now), devices(&[3, 4]));
      assert_eq!(send_frame(&mut forwarder, 1, true, now), Vec::<DeviceId>::new());
      assert_eq!(send_frame(&mut forwarder, 2, true, now), devices(&[2]));
      // Deltas follow the same way; nobody gets a layer it can't decode.
      assert_eq!(send_frame(&mut forwarder, 2, false, now), devices(&[2]));
      assert_eq!(send_frame(&mut forwarder, 0, false, now), devices(&[3, 4]));
      let effects = forwarder.on_timer(device(2), now);
      assert!(
        !effects.iter().any(|effect| matches!(
          effect,
          Effect::LayersNeeded { layers, .. } if layers != &[0, 2]
        )),
        "{effects:?}"
      );
    }

    /// A viewer held to the lowest layer (a small tile, or a budget the allocation, which knows
    /// nothing of codecs, fits only the base into) gets the H.264 base and back: no
    /// renegotiation, the SFU forwards another layer from its keyframe on.
    #[test]
    fn a_viewer_held_to_the_base_gets_h264_and_back() {
      let (mut forwarder, now) = mixed_room();
      forwarder.subscribe(device(2), VIDEO, Some(0), now).unwrap();
      forwarder.on_timer(device(2), now);
      assert_eq!(send_frame(&mut forwarder, 0, true, now), devices(&[2]));
      assert!(send_frame(&mut forwarder, 2, true, now).is_empty());
      // A larger tile: the AV1 top once the allocation's upswitch hold passed and its keyframe
      // came; the base until then.
      forwarder.subscribe(device(2), VIDEO, Some(2), now).unwrap();
      let mut maybe_switched_at = None;
      for step in 1..=100u32 {
        let at = now + Duration::from_millis(100) * step;
        forwarder.on_timer(device(2), at);
        let base = send_frame(&mut forwarder, 0, false, at);
        if send_frame(&mut forwarder, 2, true, at) == devices(&[2]) {
          maybe_switched_at = Some(at);
          break;
        }
        assert_eq!(base, devices(&[2]), "the base until the switch");
      }
      let at = maybe_switched_at.expect("switched to the AV1 top");
      assert!(send_frame(&mut forwarder, 0, false, at).is_empty());
    }

    /// Only clients that take layer codecs count for the codecs some decode; the codecs all
    /// decode still name what the base layer may be.
    #[test]
    fn the_publisher_hears_what_all_and_what_some_decode() {
      let (forwarder, _) = mixed_room();
      assert_eq!(
        forwarder.decodable_video_codecs(device(1)),
        Some(DecodableCodecs {
          by_all: vec![VideoCodec::H264],
          by_some: vec![VideoCodec::Av1, VideoCodec::H264],
        })
      );
    }

    #[test]
    fn a_track_whose_base_or_layers_the_publisher_cant_encode_is_refused() {
      let (mut forwarder, _) = mixed_room();
      let mut av1_base = mixed_layers();
      av1_base[0].maybe_codec = Some(VideoCodec::Av1);
      let mut vp9_top = mixed_layers();
      vp9_top[2].maybe_codec = Some(VideoCodec::Vp9);
      for layers in [av1_base, vp9_top] {
        assert!(matches!(
          forwarder.publish_video(
            device(1),
            StreamId(7),
            VideoTrackKind::Screen,
            VideoCodec::H264,
            layers,
            None
          ),
          Err(ForwardError::InvalidPublication(_))
        ));
      }
    }

    /// Device 1 shares its screen on [`VIDEO`] ([`mixed_layers`]) to: 2, which decodes AV1 and
    /// takes layer codecs; 3, which takes them but decodes H.264 only (a phone); 4, a client
    /// from before that decodes AV1 too. Downlinks fit every layer.
    fn mixed_room() -> (Forwarder, Instant) {
      let now = Instant::now();
      let mut config = ForwardingConfig::default();
      config.bwe.start_bps = 10_000_000;
      config.bwe.max_bps = config.bwe.max_bps.max(10_000_000);
      let mut forwarder = Forwarder::new(config);
      let both = vec![VideoCodec::Av1, VideoCodec::H264];
      let viewers = [
        (1, both.clone(), true),
        (2, both.clone(), true),
        (3, vec![VideoCodec::H264], true),
        (4, both.clone(), false),
      ];
      for (n, decoders, has_layer_codecs) in viewers {
        let capabilities = MediaCapabilities {
          video_encoders: both.clone(),
          video_decoders: decoders,
          max_simulcast_layers: 3,
          video_features: if has_layer_codecs {
            vec![VideoFeature::LayerCodecs]
          } else {
            Vec::new()
          },
          ..MediaCapabilities::default()
        };
        forwarder.add_participant(device(n), stream(n), capabilities, now);
      }
      forwarder
        .publish_video(
          device(1),
          VIDEO,
          VideoTrackKind::Screen,
          VideoCodec::H264,
          mixed_layers(),
          None,
        )
        .unwrap();
      (forwarder, now)
    }

    /// A 1440p share: H.264 at 480p, AV1 at 720p and 1440p.
    fn mixed_layers() -> Vec<VideoLayerInfo> {
      [
        (0, 854, 480, 500_000, None),
        (1, 1280, 720, 1_000_000, Some(VideoCodec::Av1)),
        (2, 2560, 1440, 3_000_000, Some(VideoCodec::Av1)),
      ]
      .into_iter()
      .map(
        |(layer, width, height, max_bitrate_bps, maybe_codec)| VideoLayerInfo {
          layer,
          width,
          height,
          max_framerate: 30,
          max_bitrate_bps,
          maybe_codec,
        },
      )
      .collect()
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
      // The gap is a loss once 3 arrived a reordering's while ago.
      let later = now + teamcraft_media::bwe::REORDER_WINDOW;
      let feedback = forwarder.take_uplink_feedback(device(1), later).unwrap();
      assert_eq!(feedback.base_seq, 0);
      assert_eq!(
        feedback
          .arrivals_us
          .iter()
          .map(Option::is_some)
          .collect::<Vec<_>>(),
        vec![true, true, false, true]
      );
      assert_eq!(forwarder.take_uplink_feedback(device(1), later), None);
    }

    /// #978: a 1080p camera's fourth layer (180p) keeps a receiver that can't take 360p in the
    /// picture; with the three layers from before, its lowest was 360p.
    #[test]
    fn a_weak_link_gets_the_fourth_layer_of_a_1080p_camera() {
      assert_eq!(settled_layer_of(&four_layers(), 350_000), Some(0));
      assert_eq!(settled_layer_of(&four_layers(), 900_000), Some(1));
    }

    /// #994: a receiver whose link carries only 180p is never switched up to 360p to find out
    /// (the switch dropped packets and stalled its picture for seconds): padding bursts find out,
    /// and 180p keeps flowing without a gap.
    #[test]
    fn a_weak_link_is_probed_with_padding_not_with_the_next_layer() {
      let run = simulate(
        &four_layers(),
        &[(Duration::ZERO, 250_000)],
        Duration::from_secs(60),
      );
      let settled = Duration::from_secs(10);
      let upper: Vec<&(Duration, Option<u8>)> = run
        .iter()
        .filter(|(at, maybe_layer)| *at >= settled && maybe_layer.is_some_and(|layer| layer > 0))
        .collect();
      assert!(upper.is_empty(), "layers above 180p after {settled:?}: {upper:?}");
      let padding = run
        .iter()
        .filter(|(_, maybe_layer)| maybe_layer.is_none())
        .count();
      assert!(padding > 0, "the link was never probed");
      let at_180p = run
        .iter()
        .filter(|(at, maybe_layer)| *at >= settled && *maybe_layer == Some(0))
        .count();
      // 30 frames of one datagram a second for 50 s.
      assert!(at_180p >= 1490, "only {at_180p} datagrams at 180p");
    }

    /// #994: once a weak link gets better, the probes show it soon and the receiver is switched
    /// up layer by layer: the top layer of a 1080p camera within 10 s of the change (6.6 s when the
    /// next layer was the probe, 4.7 s now).
    #[test]
    fn a_link_that_gets_better_is_used_soon() {
      let better_at = Duration::from_secs(30);
      let run = simulate(
        &four_layers(),
        &[(Duration::ZERO, 250_000), (better_at, 10_000_000)],
        Duration::from_secs(60),
      );
      let first_top = run
        .iter()
        .find(|(at, maybe_layer)| *at >= better_at && *maybe_layer == Some(3))
        .map(|(at, _)| *at - better_at);
      assert!(
        first_top.is_some_and(|after| after <= Duration::from_secs(10)),
        "top layer {first_top:?} after the link got better"
      );
    }

    /// #1792: a viewer on LTE on the motorway (`ltevary.sh`): the downlink swings between 2 and
    /// 3.5 Mbit/s every 2 to 4 s, then a cell has 25 Mbit/s for a minute, then it swings again.
    /// A 1440p screen share's 720p layer fits the swings, its top layer only the minute: it is
    /// forwarded within seconds of the room appearing, and within a second of the room going the
    /// viewer is back on what fits, without a picture that stands still.
    #[test]
    fn a_viewer_on_lte_follows_the_link_within_seconds() {
      let lte = Lte::default();
      let run = simulate_on(Link::lte(), &share_layers(), &lte.capacities(), lte.end());
      let report = LteReport::of(&run, &lte, |layer| layer == 2);
      eprintln!("{report:?}");
      assert!(
        report
          .up_after
          .is_some_and(|after| after <= Duration::from_secs(5)),
        "top layer {:?} after the room appeared",
        report.up_after
      );
      assert!(
        report.down_after <= Duration::from_secs(1),
        "top layer until {:?} after the room went",
        report.down_after
      );
      // Clusters that fall short queue their excess and may take a datagram of video with them,
      // as the probes before a switch up did; a layer sent to find out took a tenth of them.
      assert!(
        report.lost[0] * 100 <= report.sent[0] * 3,
        "{:?} of {:?} datagrams lost while the link swung",
        report.lost,
        report.sent
      );
      assert!(
        report.longest_gap <= Duration::from_millis(700),
        "the picture stood still for {:?} ({:?} of {:?} datagrams lost, padding {:?})",
        report.longest_gap,
        report.lost,
        report.sent,
        report.padding
      );
    }

    /// Runs a publisher sending all its layers at their bitrates and a receiver behind a link of
    /// `capacity_bps` that reports feedback every 100 ms; returns the receiver's dominant layer in
    /// the last 20 of 60 s (the estimate ramps up at 8 %/s from 300 kbit/s).
    fn settled_layer(capacity_bps: u64) -> Option<u8> {
      settled_layer_of(&three_layers(), capacity_bps)
    }

    /// [`settled_layer`] for a track of `layers`.
    fn settled_layer_of(layers: &[VideoLayerInfo], capacity_bps: u64) -> Option<u8> {
      let run = simulate(layers, &[(Duration::ZERO, capacity_bps)], Duration::from_secs(60));
      // The layer most datagrams arrived on in the last 20 s (probes upward are brief).
      let layers_seen: Vec<u8> = run
        .iter()
        .filter(|(at, _)| *at >= Duration::from_secs(40))
        .filter_map(|(_, maybe_layer)| *maybe_layer)
        .collect();
      if layers_seen.is_empty() {
        return None;
      }
      // Datagrams per frame grow with the layer's bitrate; normalized so time spent counts, not
      // bytes.
      let lowest_bps = layers.first().map_or(1, |info| info.max_bitrate_bps.max(1));
      layers
        .iter()
        .max_by_key(|info| {
          let weight = usize::try_from(info.max_bitrate_bps / lowest_bps)
            .unwrap_or(1)
            .max(1);
          layers_seen.iter().filter(|seen| **seen == info.layer).count() / weight
        })
        .map(|info| info.layer)
    }

    /// Runs a publisher sending all of `layers` at their bitrates (a keyframe every second) and a
    /// receiver behind a link whose capacity follows `capacities` (from when, bit/s), reporting
    /// feedback every 100 ms, for `duration`. Returns every datagram sent to the receiver: when,
    /// and its layer (`None`: padding).
    fn simulate(
      layers: &[VideoLayerInfo],
      capacities: &[(Duration, u64)],
      duration: Duration,
    ) -> Vec<(Duration, Option<u8>)> {
      let link = Link::new(capacities.first().map_or(u64::MAX, |(_, bps)| *bps));
      simulate_on(link, layers, capacities, duration)
        .into_iter()
        .map(|datagram| (datagram.at, datagram.maybe_layer))
        .collect()
    }

    /// [`simulate`] behind `link`, with each datagram's arrival.
    fn simulate_on(
      mut link: Link,
      layers: &[VideoLayerInfo],
      capacities: &[(Duration, u64)],
      duration: Duration,
    ) -> Vec<SimulatedDatagram> {
      let (mut forwarder, start) = room(3);
      forwarder
        .publish_video(
          device(1),
          VIDEO,
          VideoTrackKind::Camera,
          VideoCodec::Av1,
          layers.to_vec(),
          None,
        )
        .unwrap();
      let top = layers.last().map_or(0, |info| info.layer);
      forwarder.subscribe(device(2), VIDEO, Some(top), start).unwrap();
      let mut recorder = FeedbackRecorder::new(start);
      let mut in_flight: Vec<(Instant, TransportSeq)> = Vec::new();
      let mut frame_number = 0u16;
      let mut keyframes_asked: std::collections::HashSet<u8> = std::collections::HashSet::new();
      let mut now = start;
      let end = start + duration;
      let mut next_feedback = start + Duration::from_millis(100);
      let mut sent: Vec<SimulatedDatagram> = Vec::new();
      let mut send = |link: &mut Link, delivered: &[u8], now: Instant, in_flight: &mut Vec<_>| {
        let parsed = DatagramHeader::parse(delivered).unwrap();
        let is_padding = parsed.stream_id == teamcraft_media::datagram::PADDING_STREAM;
        let maybe_arrival = link.send(delivered.len(), now);
        sent.push(SimulatedDatagram {
          at: now - start,
          maybe_layer: (!is_padding).then_some(parsed.layer.value()),
          maybe_arrival: maybe_arrival.map(|arrival| arrival - start),
        });
        if let (Some(arrival), Some(seq)) = (maybe_arrival, parsed.maybe_transport_seq) {
          in_flight.push((arrival, seq));
        }
      };
      while now < end {
        if let Some((_, capacity_bps)) = capacities.iter().rev().find(|(from, _)| now - start >= *from) {
          link.capacity_bps = *capacity_bps;
        }
        // 30 fps; a keyframe every second, and on the SFU's request, as a publisher sends one.
        for info in layers {
          let is_keyframe = frame_number.is_multiple_of(30) || keyframes_asked.remove(&info.layer);
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
            let route = forwarder.route(device(1), &datagram, now).unwrap();
            for (receiver, delivered) in route.deliveries {
              assert_eq!(receiver, device(2));
              send(&mut link, &delivered, now, &mut in_flight);
            }
            note_keyframe_requests(&route.effects, &mut keyframes_asked);
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
          let effects = match recorder.take_feedback(now) {
            Some(feedback) => forwarder.on_transport_feedback(device(2), &feedback, now),
            None => forwarder.on_timer(device(2), now),
          };
          note_keyframe_requests(&effects, &mut keyframes_asked);
          for effect in effects {
            if let Effect::Padding { receiver, datagrams } = effect {
              assert_eq!(receiver, device(2));
              for datagram in &datagrams {
                send(&mut link, datagram, now, &mut in_flight);
              }
            }
          }
        }
      }
      sent
    }

    fn note_keyframe_requests(effects: &[Effect], keyframes_asked: &mut std::collections::HashSet<u8>) {
      for effect in effects {
        if let Effect::KeyframeRequest { layer, .. } = effect {
          keyframes_asked.insert(*layer);
        }
      }
    }
  }

  /// The LTE link of the test setup's `ltevary.sh` (#1792), deterministic: `swing` of rates
  /// between 2 and 3.5 Mbit/s held 2 to 4 s each, `fast` at 25 Mbit/s, `swing` again.
  #[derive(Debug, Clone, Copy)]
  struct Lte {
    swing: Duration,
    fast: Duration,
    fast_bps: u64,
  }

  impl Default for Lte {
    fn default() -> Self {
      Self {
        swing: Duration::from_secs(40),
        fast: Duration::from_secs(60),
        fast_bps: 25_000_000,
      }
    }
  }

  impl Lte {
    /// From when, which capacity.
    fn capacities(&self) -> Vec<(Duration, u64)> {
      const SWING_KBPS: [u64; 8] = [2_000, 3_500, 2_400, 3_100, 2_000, 2_800, 3_400, 2_200];
      const HOLD_SECONDS: [u64; 5] = [2, 3, 4, 3, 2];
      let mut capacities = Vec::new();
      let swing = |from: Duration, until: Duration, capacities: &mut Vec<(Duration, u64)>| {
        let mut at = from;
        let mut index = 0;
        while at < until {
          capacities.push((at, SWING_KBPS[index % SWING_KBPS.len()] * 1000));
          at += Duration::from_secs(HOLD_SECONDS[index % HOLD_SECONDS.len()]);
          index += 1;
        }
      };
      swing(Duration::ZERO, self.fast_from(), &mut capacities);
      capacities.push((self.fast_from(), self.fast_bps));
      swing(self.fast_until(), self.end(), &mut capacities);
      capacities
    }

    fn fast_from(&self) -> Duration {
      self.swing
    }

    fn fast_until(&self) -> Duration {
      self.swing + self.fast
    }

    fn end(&self) -> Duration {
      self.fast_until() + self.swing
    }
  }

  /// How a run on an [`Lte`] link followed it.
  #[derive(Debug)]
  struct LteReport {
    /// From the room appearing until the first datagram of the top layer.
    up_after: Option<Duration>,
    /// From the room going until the last datagram of the top layer.
    down_after: Duration,
    /// The longest time without a video datagram arriving, after the first 5 s.
    longest_gap: Duration,
    /// Video datagrams the link dropped, after the first 5 s, of those sent: while it swings
    /// before the minute, during it, and while it swings after it.
    lost: [usize; 3],
    sent: [usize; 3],
    /// Padding datagrams sent, and dropped.
    padding: (usize, usize),
  }

  impl LteReport {
    fn of(run: &[SimulatedDatagram], lte: &Lte, is_top: impl Fn(u8) -> bool) -> Self {
      let is_top_datagram = |datagram: &&SimulatedDatagram| datagram.maybe_layer.is_some_and(&is_top);
      let up_after = run
        .iter()
        .filter(is_top_datagram)
        .find(|datagram| datagram.at >= lte.fast_from())
        .map(|datagram| datagram.at - lte.fast_from());
      let down_after = run
        .iter()
        .filter(is_top_datagram)
        .filter(|datagram| datagram.at >= lte.fast_until())
        .map(|datagram| datagram.at - lte.fast_until())
        .max()
        .unwrap_or_default();
      let settled = Duration::from_secs(5);
      let mut arrivals: Vec<Duration> = run
        .iter()
        .filter(|datagram| datagram.at >= settled && datagram.maybe_layer.is_some())
        .filter_map(|datagram| datagram.maybe_arrival)
        .collect();
      arrivals.sort_unstable();
      let longest_gap = arrivals
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .max()
        .unwrap_or_default();
      let phases = [
        (settled, lte.fast_from()),
        (lte.fast_from(), lte.fast_until()),
        (lte.fast_until(), lte.end()),
      ];
      let video_in = |(from, until): (Duration, Duration)| {
        run.iter().filter(move |datagram| {
          datagram.at >= from && datagram.at < until && datagram.maybe_layer.is_some()
        })
      };
      let lost = phases.map(|phase| {
        video_in(phase)
          .filter(|datagram| datagram.maybe_arrival.is_none())
          .count()
      });
      let sent = phases.map(|phase| video_in(phase).count());
      let padding_sent: Vec<&SimulatedDatagram> = run
        .iter()
        .filter(|datagram| datagram.maybe_layer.is_none())
        .collect();
      let padding_lost = padding_sent
        .iter()
        .filter(|datagram| datagram.maybe_arrival.is_none())
        .count();
      Self {
        up_after,
        down_after,
        longest_gap,
        lost,
        sent,
        padding: (padding_sent.len(), padding_lost),
      }
    }
  }

  /// One datagram the simulated SFU sent to the receiver.
  #[derive(Debug, Clone, Copy)]
  struct SimulatedDatagram {
    /// When it was sent, from the simulation's start.
    at: Duration,
    /// Its layer; `None` for padding.
    maybe_layer: Option<u8>,
    /// When it arrived, from the simulation's start; `None` when the link dropped it.
    maybe_arrival: Option<Duration>,
  }

  /// A downlink bottleneck: capacity, 30 ms delay, a drop-tail queue (300 ms by default) and,
  /// like Linux's `tbf`, a token bucket of `bucket_bytes` that lets a burst through at once.
  struct Link {
    capacity_bps: u64,
    queue_limit: Duration,
    bucket_bytes: usize,
    /// When the bucket would be empty had nothing been sent since (the token bucket's clock).
    maybe_empty_at: Option<Instant>,
  }

  impl Link {
    fn new(capacity_bps: u64) -> Self {
      Self {
        capacity_bps,
        queue_limit: Duration::from_millis(300),
        bucket_bytes: 0,
        maybe_empty_at: None,
      }
    }

    /// The LTE test setup's link (`ltevary.sh`): `tbf` with a 32 kB bucket and 150 ms of queue.
    fn lte() -> Self {
      Self {
        queue_limit: Duration::from_millis(150),
        bucket_bytes: 32_000,
        ..Self::new(3_000_000)
      }
    }

    fn send(&mut self, bytes: usize, now: Instant) -> Option<Instant> {
      let seconds = |bytes: usize| Duration::from_secs_f64(bytes as f64 * 8.0 / self.capacity_bps as f64);
      let full_since = now.checked_sub(seconds(self.bucket_bytes)).unwrap_or(now);
      let empty_at = self.maybe_empty_at.map_or(full_since, |at| at.max(full_since));
      let departure = (empty_at + seconds(bytes)).max(now);
      if empty_at.saturating_duration_since(now) > self.queue_limit {
        return None;
      }
      self.maybe_empty_at = Some(empty_at + seconds(bytes));
      Some(departure + Duration::from_millis(30))
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
        None,
      )
      .unwrap();
    (forwarder, now)
  }

  fn capabilities_av1_encoder() -> MediaCapabilities {
    MediaCapabilities {
      video_encoders: vec![VideoCodec::Av1, VideoCodec::Vp9],
      ..capabilities_av1()
    }
  }

  fn capabilities_av1() -> MediaCapabilities {
    MediaCapabilities {
      video_decoders: vec![VideoCodec::Av1, VideoCodec::Vp9],
      max_simulcast_layers: 3,
      ..MediaCapabilities::default()
    }
  }

  /// Sends one single-fragment frame on `layer` of [`VIDEO`]; returns who receives it.
  fn send_frame(forwarder: &mut Forwarder, layer: u8, is_keyframe: bool, now: Instant) -> Vec<DeviceId> {
    send_fragment(forwarder, layer, is_keyframe, 1, now)
  }

  /// Sends the first fragment of a frame of `count` on `layer` of [`VIDEO`]; returns who
  /// receives it.
  fn send_fragment(
    forwarder: &mut Forwarder,
    layer: u8,
    is_keyframe: bool,
    count: u16,
    now: Instant,
  ) -> Vec<DeviceId> {
    let mut rtp = header();
    FrameFragment {
      frame_number: 1,
      index: 0,
      count,
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

  /// A 1080p camera's layers since #978 (`video::camera_layers`).
  fn four_layers() -> Vec<VideoLayerInfo> {
    [
      (0, 320, 180, 93_750),
      (1, 640, 360, 375_000),
      (2, 1280, 720, 1_500_000),
      (3, 1920, 1080, 3_000_000),
    ]
    .into_iter()
    .map(|(layer, width, height, max_bitrate_bps)| VideoLayerInfo {
      layer,
      width,
      height,
      max_framerate: 30,
      max_bitrate_bps,
      maybe_codec: None,
    })
    .collect()
  }

  /// A 1440p screen share's layers, about (#1733): 480p, 720p and 1440p.
  fn share_layers() -> Vec<VideoLayerInfo> {
    [
      (0, 852, 480, 500_000),
      (1, 1280, 720, 1_660_000),
      (2, 2560, 1440, 6_600_000),
    ]
    .into_iter()
    .map(|(layer, width, height, max_bitrate_bps)| VideoLayerInfo {
      layer,
      width,
      height,
      max_framerate: 30,
      max_bitrate_bps,
      maybe_codec: None,
    })
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
      maybe_codec: None,
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
