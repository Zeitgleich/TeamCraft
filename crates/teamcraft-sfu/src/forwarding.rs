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
  datagram::{DatagramError, DatagramHeader, StreamId},
  fragment::{FragmentError, FrameFragment},
  rtp::{AudioLevel, RtpError, RtpHeader},
};
use teamcraft_proto::{
  DeviceId, MAX_SIMULCAST_LAYERS, MediaCapabilities, MediaStreamId, ScreenAudioTrack, TransportFeedback,
  VideoCodec, VideoLayerInfo, VideoPublicationId, VideoTrack, VideoTrackKind,
};
use thiserror::Error;

use crate::layers::{LayerAllocationConfig, LayerAllocator, TrackDemand, full_demand_bps};
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
  /// A layer counts as stopped when a lower layer of its track kept arriving this long without
  /// it; receivers get the best lower layer that still flows. Above the gaps of a
  /// layer that runs (a still screen sends its picture again every second, `SCREEN_IDLE_REFRESH`),
  /// short enough that a frozen picture doesn't last.
  pub layer_stalled_after: Duration,
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
}

/// What a leaving participant took with it.
#[derive(Debug, Default)]
pub struct Removal {
  pub effects: Vec<Effect>,
  /// Its video tracks, now unpublished.
  pub video_tracks: Vec<StreamId>,
  pub maybe_screen_audio: Option<StreamId>,
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
  /// The connection was lost: nothing is sent to it until it is back.
  is_suspended: bool,
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
      layer_stalled_after: Duration::from_millis(1500),
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
      is_suspended: false,
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
    let mut effects = Vec::new();
    if let Some(speakers) = self.speakers.remove(device_id, now) {
      effects.push(Effect::ActiveSpeakers(speakers));
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
      effects.extend(self.update_layers_needed(stream_id));
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
    let Some(participant) = self.participants.get_mut(&device_id) else {
      return Vec::new();
    };
    participant.capabilities = capabilities;
    participant.downlink = SendSideEstimator::new(bwe);
    participant.uplink = FeedbackRecorder::new(now);
    participant.feedback_epoch = now;
    participant.allocator = LayerAllocator::new();
    participant.is_suspended = false;
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
      effects.extend(self.update_layers_needed(stream_id));
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
    effects.extend(self.update_layers_needed(stream_id));
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
    if !participant
      .capabilities
      .video_decoders
      .contains(&route.track.codec)
    {
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
    effects.extend(self.update_layers_needed(stream_id));
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
      self.video_receivers(header.stream_id, header.layer.value(), &rtp_header, now)?
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
      let maybe_live = subscription
        .target
        .map(|target| live_layer(&arrivals, target, stalled_after));
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
          // Layers the publisher's uplink doesn't carry, or with frames larger than the receiver
          // reassembles, aren't there to have.
          layers: route
            .track
            .layers
            .get(..route.uplink_layers.min(route.layers_within(max_frame_fragments)))
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
    let allocation = participant
      .allocator
      .allocate(&layers_config, estimate, &demands, now);

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
      let live = live_layer(&route.arrivals, layer, stalled_after);
      let interval = self.config.keyframes.switch_retry_interval;
      for wanted in [live, layer] {
        if current != Some(wanted) {
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

  fn update_layers_needed(&mut self, stream_id: StreamId) -> Option<Effect> {
    let stalled_after = self.config.layer_stalled_after;
    let route = self.video_tracks.get_mut(&stream_id)?;
    let arrivals = route.arrivals;
    // A layer standing in for a stopped target is needed as well, or it might stop too.
    let mut needed: Vec<u8> = route
      .subscribers
      .values()
      .flat_map(|subscription| {
        let maybe_live = subscription
          .target
          .map(|target| live_layer(&arrivals, target, stalled_after));
        [subscription.target, maybe_live, subscription.current]
      })
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
  let index = usize::from(layer);
  let maybe_last = arrivals.get(index).copied().flatten().map(|run| run.last);
  arrivals
    .get(..index)
    .unwrap_or_default()
    .iter()
    .flatten()
    .any(|lower| {
      let without = maybe_last.map_or(lower.since, |last| last.max(lower.since));
      lower.last.saturating_duration_since(without) > stalled_after
    })
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
      for round in 0..60u16 {
        let at = now + Duration::from_millis(100 * u64::from(round));
        for _ in 0..10 {
          forwarder.route(device(1), &audio(1, 10), at).unwrap();
        }
        let since_epoch = u64::try_from(at.saturating_duration_since(epoch).as_micros()).unwrap();
        let feedback = TransportFeedback {
          base_seq: round * 10,
          arrivals_us: (0..10u64).map(|i| Some(since_epoch + i * 1_000)).collect(),
        };
        let mut effects = forwarder.on_transport_feedback(device(2), &feedback, at);
        effects.extend(forwarder.on_timer(device(2), at));
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
        let is_keyframe = frame_number.is_multiple_of(30);
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
