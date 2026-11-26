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
//! - **Repairing the publisher's uplink (#2189):** a gap in a layer's RTP sequence numbers that
//!   someone receives is asked of the publisher at once, and the packets it sends again are
//!   forwarded like any; one still missing after a short while gets a keyframe asked for, rate
//!   limited ([`crate::uplink_repair`]).
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
  uplink_layers::UplinkPolicy,
  voice_guard::{ArrivingVoice, VoiceGuard, VoiceGuardConfig},
};
use teamcraft_proto::{
  AudioFeature, DeviceId, DirectPathMedia, KNOWN_VIDEO_CODECS, MAX_NACK_SEQUENCE_NUMBERS,
  MAX_SIMULCAST_LAYERS, MediaCapabilities, MediaStreamId, ScreenAudioTrack, TransportFeedback, UplinkFloor,
  VideoCodec, VideoLayerInfo, VideoPublicationId, VideoTrack, VideoTrackKind,
};
use thiserror::Error;
use tracing::{debug, info};

use crate::audio_layers::{
  AudioLayerConfig, RecoveryStage, THIN_VOICE_LAYER, ThinRecovery, VoiceLayerChoice, VoiceSequence,
};
use crate::layers::{
  LayerAllocationConfig, LayerAllocator, ProbeRequest, TrackDemand, UpgradeProof, full_demand_bps,
};
use crate::speakers::{SpeakerSelectionConfig, SpeakerSelector};
use crate::uplink_repair::{KeyframeFragment, RepairStep, UplinkRepair, UplinkRepairConfig};
use teamcraft_media::retransmit::{PacketHistory, RetransmitBudget, RetransmitConfig};

/// An estimate below the thin bound makes a downlink thin only after this long; a probe at the
/// join that measured it thin does at once.
const THIN_DOWNLINK_SUSTAINED: Duration = Duration::from_secs(10);
/// A voice went to a receiver this recently: what its feedback says counts for its recovery from a
/// thin downlink.
const VOICE_FLOWING_WITHIN: Duration = Duration::from_secs(1);
/// Where an RTP header keeps its sequence number.
const RTP_SEQUENCE_OFFSET: usize = 2;
/// Video tracks (camera, screen share) one participant may publish at the same time.
const MAX_VIDEO_TRACKS_PER_PARTICIPANT: usize = 4;
/// The upswitch hold while a receiver's downlink estimate is in its fast start.
const FAST_START_UPSWITCH_HOLD: Duration = Duration::from_millis(500);
/// How often a receiver's layer allocation logs its inputs (debug).
const ALLOCATION_LOG_INTERVAL: Duration = Duration::from_secs(1);
/// How long video kept at its lowest layer has to clear the link before voice still lost to a
/// receiver pauses it: what the higher layers queued drains, the lowest layer's keyframe arrives.
const VOICE_HURT_AT_LOWEST_AFTER: Duration = Duration::from_millis(1500);

/// Routing state of one room.
#[derive(Debug)]
pub struct Forwarder {
  /// A receiver takes the voices' thin copies: senders send them (#1893 step 3).
  is_thin_voice_wanted: bool,
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
  /// Asking publishers for video packets their uplink lost (#2189); only while `retransmit` is on
  /// (publishers keep their packets for its window).
  pub uplink_repair: UplinkRepairConfig,
  /// When a receiver's voice counts as hurt and its video yields (#1871).
  pub voice_guard: VoiceGuardConfig,
  /// Bytes of padding the probe of a participant's link at the start may send, each direction:
  /// the SFU's of the downlink, and what it tells clients they may send for the uplink. Zero
  /// turns both off.
  pub probe_bytes: u32,
  /// Which receivers get the thin copies of the voices (#1893).
  pub audio_layers: AudioLayerConfig,
  /// The audio-only floor of the clients' uplinks, told in `joined` (#1891).
  pub uplink_floor: UplinkFloor,
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
  /// Packets of `layer` didn't arrive from the publisher: it should send them again (#2189).
  RetransmitRequest {
    publisher: DeviceId,
    stream_id: StreamId,
    layer: u8,
    sequence_numbers: Vec<u16>,
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
  /// Whether a receiver takes the voices' thin copies now (to everyone, #1893 step 3).
  ThinVoiceWanted(bool),
  /// A publisher's uplink carries none of the layers of `stream_id` now, or does again (to
  /// everyone but the publisher, #1891).
  VideoPausedByPublisher {
    publisher: DeviceId,
    stream_id: StreamId,
    is_paused: bool,
  },
  /// The video to `receiver` yields to its voice, or no longer does (to that receiver, #1891).
  VideoYieldedForVoice { receiver: DeviceId, is_yielded: bool },
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
  /// What becomes of the voice sent to it: while it is lost, its video yields (#1871).
  voice: VoiceGuard,
  /// Since when its video yields to its voice at the lowest layers: voice still lost
  /// [`VOICE_HURT_AT_LOWEST_AFTER`] later pauses the video.
  maybe_video_yielded_at: Option<Instant>,
  /// It was told its video yields to its voice (#1891).
  is_video_yielded_for_voice: bool,
  /// How its own voice arrives: while its upload loses it, it gets no video either, whose
  /// acknowledgements share that upload (#1889).
  uplink_voice: ArrivingVoice,
  /// The connection was lost: nothing is sent to it until it is back.
  is_suspended: bool,
  /// What may still be resent to it.
  retransmit_budget: RetransmitBudget,
  /// The probe of its downlink at the start, while it runs.
  maybe_join_probe: Option<JoinProbe>,
  /// What its connection drains, where the transport measures that itself (the WebSocket
  /// fallback's pacing rate, #1810): its downlink is taken as no more than that.
  maybe_link_capacity_bps: Option<u32>,
  /// When its allocation's inputs were last logged (at most once a second, debug).
  maybe_allocation_logged_at: Option<Instant>,
  /// Whether it takes the others' thin voices (#1893).
  voice_layer: VoiceLayerChoice,
  /// The others' voices as it gets them: numbered on across their layers.
  voice_sequences: HashMap<DeviceId, VoiceSequence>,
  /// When its own thin voice last arrived: while it flows, receivers on thin links get it.
  maybe_thin_voice_at: Option<Instant>,
  /// When a voice last went to it: its recovery from a thin downlink counts only then.
  maybe_voice_sent_at: Option<Instant>,
  /// It says its own upload's voice is hurt (#1889): it gets no video meanwhile.
  is_uplink_reported_hurt: bool,
  /// On a thin downlink: its way back by its voices, without padding (#1893 step 3).
  thin_recovery: ThinRecovery,
  is_thin_downlink: bool,
  /// What the probe at its join showed its downlink carries, until it took the full voices again.
  maybe_measured_downlink_bps: Option<u32>,
  /// Since when its downlink estimate is below the thin bound.
  maybe_estimate_thin_since: Option<Instant>,
  /// When its own voice (layer 0) last arrived, and when it came back after a pause.
  maybe_voice_at: Option<Instant>,
  maybe_voice_resumed_at: Option<Instant>,
  /// While it says its direct path to the other device of a 1:1 call carries its media (#127):
  /// what the path carries.
  maybe_direct: Option<DirectPathMedia>,
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
  /// What the publisher's uplink lost per layer, to ask it for (#2189).
  repair: [UplinkRepair; MAX_SIMULCAST_LAYERS as usize],
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
      uplink_repair: UplinkRepairConfig::default(),
      voice_guard: VoiceGuardConfig::default(),
      probe_bytes: 15_000,
      audio_layers: AudioLayerConfig::default(),
      uplink_floor: UplinkPolicy::default().floor(),
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
      is_thin_voice_wanted: false,
    }
  }

  /// Whether a receiver takes the voices' thin copies now (#1893 step 3).
  pub fn is_thin_voice_wanted(&self) -> bool {
    self.is_thin_voice_wanted
  }

  /// What a participant says of its direct path to the other device of a 1:1 call (#127,
  /// ARCHITECTURE.md §6.7). The two devices' audio stops going through the SFU to each other only
  /// while both say it is active ([`Forwarder::is_direct_pair`]), their video only while both say
  /// it carries the video as well ([`Forwarder::is_direct_video_pair`]); neither can cut the other
  /// off. When the video starts going directly, neither publisher is asked for layers only the
  /// other device took; when it stops, the SFU switches the two back to their layers, with
  /// keyframes asked for.
  pub fn set_direct_path(
    &mut self,
    device_id: DeviceId,
    maybe_media: Option<DirectPathMedia>,
    now: Instant,
  ) -> Vec<Effect> {
    let maybe_before = self.direct_video_pair();
    let Some(participant) = self.participants.get_mut(&device_id) else {
      return Vec::new();
    };
    participant.maybe_direct = maybe_media;
    let maybe_after = self.direct_video_pair();
    if maybe_before == maybe_after {
      return Vec::new();
    }
    if let Some((first, second)) = maybe_after {
      self.start_direct_video(first, second, now)
    } else {
      self.end_direct_video(maybe_before, now)
    }
  }

  /// Whether `sender` and `receiver` are the only two participants and both say their direct path
  /// carries the media: then the SFU forwards neither one's audio to the other.
  pub fn is_direct_pair(&self, sender: DeviceId, receiver: DeviceId) -> bool {
    let is_active = |device: DeviceId| {
      self
        .participants
        .get(&device)
        .is_some_and(|route| route.maybe_direct.is_some() && !route.is_suspended)
    };
    self.participants.len() == 2 && sender != receiver && is_active(sender) && is_active(receiver)
  }

  /// Whether `sender` and `receiver` are a direct pair whose direct path carries the video too:
  /// then the SFU forwards neither one's video to the other.
  pub fn is_direct_video_pair(&self, sender: DeviceId, receiver: DeviceId) -> bool {
    let has_video = |device: DeviceId| {
      self
        .participants
        .get(&device)
        .and_then(|route| route.maybe_direct)
        .is_some_and(|media| media.has_video())
    };
    self.is_direct_pair(sender, receiver) && has_video(sender) && has_video(receiver)
  }

  /// The two devices whose video goes directly now, if any.
  fn direct_video_pair(&self) -> Option<(DeviceId, DeviceId)> {
    let mut devices = self.participants.keys().copied();
    let (first, second) = (devices.next()?, devices.next()?);
    self
      .is_direct_video_pair(first, second)
      .then_some((first, second))
  }

  /// The pair's video goes directly from now on: what each received from the SFU stops, and
  /// neither publisher keeps layers only the other took.
  fn start_direct_video(&mut self, first: DeviceId, second: DeviceId, now: Instant) -> Vec<Effect> {
    for route in self.video_tracks.values_mut() {
      for (receiver, subscription) in &mut route.subscribers {
        if *receiver == first || *receiver == second {
          subscription.current = None;
        }
      }
    }
    self.update_all_layers_needed(now)
  }

  /// The video of `maybe_pair` came through the SFU again: the two switch to their layers at the
  /// next keyframes, which are asked for, and the publishers send the layers needed for them.
  fn end_direct_video(&mut self, maybe_pair: Option<(DeviceId, DeviceId)>, now: Instant) -> Vec<Effect> {
    let Some((first, second)) = maybe_pair else {
      return Vec::new();
    };
    let mut effects = Vec::new();
    for receiver in [first, second] {
      for route in self.video_tracks.values_mut() {
        if let Some(subscription) = route.subscribers.get_mut(&receiver) {
          subscription.current = None;
        }
      }
      effects.extend(self.reallocate(receiver, now));
    }
    effects.extend(self.update_all_layers_needed(now));
    effects
  }

  fn update_all_layers_needed(&mut self, now: Instant) -> Vec<Effect> {
    let streams: Vec<StreamId> = self.video_tracks.keys().copied().collect();
    streams
      .into_iter()
      .filter_map(|stream_id| self.update_layers_needed(stream_id, now))
      .collect()
  }

  pub fn add_participant(
    &mut self,
    device_id: DeviceId,
    audio_stream: StreamId,
    capabilities: MediaCapabilities,
    now: Instant,
  ) {
    // A new session's voice numbers from scratch: the others start over with it. A third device
    // ends any direct path (#127): everyone gets the voices from the SFU again.
    for route in self.participants.values_mut() {
      route.voice_sequences.remove(&device_id);
      route.maybe_direct = None;
    }
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
      voice: VoiceGuard::new(self.config.voice_guard),
      maybe_video_yielded_at: None,
      is_video_yielded_for_voice: false,
      uplink_voice: ArrivingVoice::new(self.config.voice_guard),
      voice_layer: VoiceLayerChoice::default(),
      voice_sequences: HashMap::new(),
      maybe_thin_voice_at: None,
      maybe_voice_sent_at: None,
      is_uplink_reported_hurt: false,
      thin_recovery: ThinRecovery::default(),
      is_thin_downlink: false,
      maybe_measured_downlink_bps: None,
      maybe_estimate_thin_since: None,
      maybe_voice_at: None,
      maybe_voice_resumed_at: None,
      is_suspended: false,
      retransmit_budget: RetransmitBudget::default(),
      maybe_join_probe: self.new_join_probe(now),
      maybe_link_capacity_bps: None,
      maybe_allocation_logged_at: None,
      maybe_direct: None,
    };
    self.participants.insert(device_id, route);
  }

  /// A participant's connection was lost: it stops receiving and drops out of the speaker
  /// selection; its tracks stay published and its subscriptions stay (paused) for its return.
  pub fn suspend(&mut self, device_id: DeviceId, now: Instant) -> Vec<Effect> {
    let maybe_direct_video = self.direct_video_pair();
    let Some(participant) = self.participants.get_mut(&device_id) else {
      return Vec::new();
    };
    participant.is_suspended = true;
    // Its direct path counts again only once it says so on its new connection (#127).
    participant.maybe_direct = None;
    participant.maybe_probe_request = None;
    let mut effects = self.end_direct_video(maybe_direct_video, now);
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
    participant.maybe_direct = None;
    participant.retransmit_budget = RetransmitBudget::default();
    // Its new connection measures its own, if it does.
    participant.maybe_link_capacity_bps = None;
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

  /// The audio features every other participant has (#1893): what `device_id`'s voice may use.
  /// With nobody else, every feature there is. `None` for a participant that isn't there.
  pub fn audio_features_of_others(&self, device_id: DeviceId) -> Option<Vec<AudioFeature>> {
    if !self.participants.contains_key(&device_id) {
      return None;
    }
    let mut features = vec![AudioFeature::LongFrames];
    for (other, participant) in &self.participants {
      if *other != device_id {
        features.retain(|feature| participant.capabilities.audio_features.contains(feature));
      }
    }
    Some(features)
  }

  /// Removes a participant with its tracks and subscriptions.
  pub fn remove_participant(&mut self, device_id: DeviceId, now: Instant) -> Removal {
    let maybe_screen_audio = self
      .participants
      .remove(&device_id)
      .and_then(|route| route.maybe_screen_audio);
    // Its voice numbers from scratch if it comes back. Whoever stays gets the voices from the
    // SFU until it reports a direct path again (#127).
    for route in self.participants.values_mut() {
      route.voice_sequences.remove(&device_id);
      route.maybe_direct = None;
    }
    let mut effects: Vec<Effect> = self.follow_thin_voice().into_iter().collect();
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

  /// Ends `owner`'s screen sound on `stream_id`. Another device's screen sound is
  /// [`ForwardError::ForeignStream`]; one that is no longer published (a takeover or a host ended
  /// it meanwhile, #2442) is [`ForwardError::UnknownTrack`].
  pub fn unpublish_screen_audio(&mut self, owner: DeviceId, stream_id: StreamId) -> Result<(), ForwardError> {
    let is_foreign = self
      .participants
      .iter()
      .any(|(device, route)| *device != owner && route.maybe_screen_audio == Some(stream_id));
    let participant = self.participants.get_mut(&owner).ok_or(ForwardError::NotJoined)?;
    if participant.maybe_screen_audio != Some(stream_id) {
      return Err(if is_foreign {
        ForwardError::ForeignStream(stream_id)
      } else {
        ForwardError::UnknownTrack(stream_id)
      });
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
    self.check_publication(owner, &codec, &layers)?;
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
      repair: std::array::from_fn(|_| UplinkRepair::new(self.uplink_repair_config(), &track.kind)),
    };
    self.video_tracks.insert(stream_id, route);
    Ok(track)
  }

  /// Takes `layers` for `owner`'s track `stream_id` in place (#2417): a screen share whose layers
  /// changed keeps its stream and its receivers, who switch to the new layers at their keyframes
  /// and show the last picture meanwhile. The layers' state starts over as for a new track; every
  /// receiver is allocated afresh, and the publisher is asked for keyframes and told the layers
  /// needed. `Ok(None)` when the stream is no track of `owner` of `kind` in `codec` (it ended
  /// meanwhile, or another codec): the caller publishes a new track instead.
  #[allow(clippy::too_many_arguments)]
  pub fn replace_video(
    &mut self,
    owner: DeviceId,
    stream_id: StreamId,
    kind: &VideoTrackKind,
    codec: &VideoCodec,
    layers: Vec<VideoLayerInfo>,
    maybe_publication_id: Option<VideoPublicationId>,
    now: Instant,
  ) -> Result<Option<(VideoTrack, Vec<Effect>)>, ForwardError> {
    self.check_publication(owner, codec, &layers)?;
    let repair_config = self.uplink_repair_config();
    let Some(route) = self.video_tracks.get_mut(&stream_id).filter(|route| {
      route.track.device_id == owner && route.track.kind == *kind && route.track.codec == *codec
    }) else {
      return Ok(None);
    };
    route.track.layers = layers;
    route.track.maybe_publication_id = maybe_publication_id;
    // The new layers count as carried until the publisher says otherwise, as a new track's.
    let was_paused = route.uplink_layers == 0;
    route.uplink_layers = route.track.layers.len();
    route.layers_needed = Vec::new();
    route.arrivals = [None; MAX_SIMULCAST_LAYERS as usize];
    route.frame_fragments = [0; MAX_SIMULCAST_LAYERS as usize];
    route.history = Default::default();
    route.repair = std::array::from_fn(|_| UplinkRepair::new(repair_config, &route.track.kind));
    // Each receiver waits for a keyframe of the new layers; its decoder keeps the last picture.
    for subscription in route.subscribers.values_mut() {
      subscription.target = None;
      subscription.current = None;
      subscription.maybe_last_request = None;
    }
    let receivers: Vec<DeviceId> = route.subscribers.keys().copied().collect();
    let track = route.track.clone();
    // The old layers' request pacing says nothing about the new encoders.
    self
      .last_keyframe_requests
      .retain(|(stream, _), _| *stream != stream_id);
    let mut effects = Vec::new();
    // Viewers that were told the publisher held the track (#1891) hear that it goes on.
    if was_paused {
      effects.push(Effect::VideoPausedByPublisher {
        publisher: owner,
        stream_id,
        is_paused: false,
      });
    }
    for receiver in receivers {
      effects.extend(self.reallocate(receiver, now));
    }
    effects.extend(self.update_layers_needed(stream_id, now));
    Ok(Some((track, effects)))
  }

  /// Whether `owner` may publish a track in `codec` with `layers`.
  fn check_publication(
    &self,
    owner: DeviceId,
    codec: &VideoCodec,
    layers: &[VideoLayerInfo],
  ) -> Result<(), ForwardError> {
    let participant = self.participants.get(&owner).ok_or(ForwardError::NotJoined)?;
    let encoders = &participant.capabilities.video_encoders;
    if !encoders.contains(codec) || layers.iter().any(|info| !encoders.contains(info.codec(codec))) {
      return Err(ForwardError::InvalidPublication(
        "codec not in the publisher's encoders",
      ));
    }
    // #1733: the lowest layer is in the track's codec, so a receiver that knows only the track's
    // codec (a client from before layer codecs) has a layer to get.
    if layers.first().is_some_and(|info| info.codec(codec) != codec) {
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
    Ok(())
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
    let was_paused = route.uplink_layers == 0;
    route.uplink_layers = carried;
    let receivers: Vec<DeviceId> = route.subscribers.keys().copied().collect();
    let mut effects = Vec::new();
    // Viewers say why the picture stopped (#1891).
    if was_paused != (carried == 0) {
      effects.push(Effect::VideoPausedByPublisher {
        publisher: owner,
        stream_id,
        is_paused: carried == 0,
      });
    }
    for receiver in receivers {
      effects.extend(self.reallocate(receiver, now));
    }
    effects.extend(self.update_layers_needed(stream_id, now));
    Ok(effects)
  }

  /// The video tracks whose publisher's uplink carries none of their layers now (#1891): a device
  /// that joins hears of them.
  pub fn paused_video_tracks(&self) -> Vec<StreamId> {
    self
      .video_tracks
      .iter()
      .filter(|(_, route)| route.uplink_layers == 0)
      .map(|(&stream_id, _)| stream_id)
      .collect()
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

  /// Whether some receiver takes the thin voices, when that changed (#1893 step 3).
  fn follow_thin_voice(&mut self) -> Option<Effect> {
    let is_wanted = self.participants.values().any(|participant| {
      participant.voice_layer.is_thin() && participant.capabilities.has_long_audio_frames()
    });
    if is_wanted == self.is_thin_voice_wanted {
      return None;
    }
    self.is_thin_voice_wanted = is_wanted;
    // Rare: a receiver's downlink turned thin, or the last thin one left or recovered.
    info!(
      is_wanted,
      "receivers on thin downlinks take the voices' thin copies"
    );
    Some(Effect::ThinVoiceWanted(is_wanted))
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
    let was_uplink_hurt = participant.is_uplink_hurt(now);
    // A voice's thin copy (#1893) counts for neither its upload's check nor the speakers: the
    // sender's own layer does.
    let voice_layer = if is_audio { header.layer.value() } else { 0 };
    let audio_layers = self.config.audio_layers;
    if is_audio && voice_layer == THIN_VOICE_LAYER {
      participant.maybe_thin_voice_at = Some(now);
    } else if is_audio {
      participant
        .uplink_voice
        .on_arrival(rtp_header.sequence_number.0, now);
      // A voice back after a pause (a new turn) starts both copies at once, its thin one a packet
      // or two later: a thin receiver waits for that rather than taking the full voice it can't
      // carry.
      let is_resumed = participant
        .maybe_voice_at
        .is_none_or(|at| now.saturating_duration_since(at) >= audio_layers.thin_stale_after);
      if is_resumed {
        participant.maybe_voice_resumed_at = Some(now);
      }
      participant.maybe_voice_at = Some(now);
    }
    let is_thin_flowing = participant
      .maybe_thin_voice_at
      .is_some_and(|at| now.saturating_duration_since(at) < audio_layers.thin_stale_after);
    let is_thin_expected = participant
      .maybe_thin_voice_at
      .is_some_and(|at| now.saturating_duration_since(at) < audio_layers.thin_known_for)
      && participant
        .maybe_voice_resumed_at
        .is_some_and(|at| now.saturating_duration_since(at) < audio_layers.thin_resume_grace);
    let has_thin_voice = is_thin_flowing || is_thin_expected;
    let has_uplink_become_hurt = !was_uplink_hurt && participant.is_uplink_hurt(now);

    let mut route = Route::default();
    if has_uplink_become_hurt {
      // Its video stops now, not with its next feedback.
      route.effects.extend(self.reallocate(sender, now));
    }
    let receivers = if is_screen_audio {
      self.others(sender)
    } else if is_audio {
      if voice_layer != THIN_VOICE_LAYER {
        let level = rtp_header.audio_level().unwrap_or(AudioLevel::SILENCE);
        if let Some(speakers) = self.speakers.record(sender, level, now) {
          route.effects.push(Effect::ActiveSpeakers(speakers));
        }
      }
      if self.speakers.is_selected(sender) {
        self.others(sender)
      } else {
        Vec::new()
      }
    } else {
      let layer = header.layer.value();
      let sequence_number = rtp_header.sequence_number.0;
      let receivers = self.video_receivers(header.stream_id, layer, &rtp_header, now)?;
      let is_wanted = !receivers.is_empty() || self.is_layer_wanted(header.stream_id, layer);
      // `video_receivers` read the fragment already: a packet without one went no further.
      let maybe_keyframe = FrameFragment::from_header(&rtp_header)
        .ok()
        .filter(|fragment| fragment.is_keyframe)
        .map(|fragment| KeyframeFragment {
          index: fragment.index,
          count: fragment.count,
        });
      let step = self.note_uplink_arrival(
        header.stream_id,
        layer,
        sequence_number,
        maybe_keyframe,
        is_wanted,
        now,
      );
      route
        .effects
        .extend(self.repair_effects(sender, header.stream_id, layer, step, now));
      // A packet sent again although it had arrived (late, not lost) goes out again too: rare, and
      // receivers drop duplicates before reassembly.
      if !receivers.is_empty() {
        self.keep_for_retransmission(header, sequence_number, datagram, now);
      }
      receivers
    };
    let is_voice = is_audio || is_screen_audio;
    for receiver in receivers {
      // The two devices of a 1:1 call that both carry their audio, or their video too, directly
      // (#127).
      let is_direct = if is_voice {
        self.is_direct_pair(sender, receiver)
      } else {
        self.is_direct_video_pair(sender, receiver)
      };
      if is_direct {
        continue;
      }
      if let Some(target) = self.participants.get_mut(&receiver) {
        let maybe_voice_sequence = if is_audio {
          let wanted = target.wanted_voice_layer(&audio_layers, has_thin_voice, now);
          if wanted != voice_layer {
            continue;
          }
          let sequence = target.voice_sequences.entry(sender).or_default();
          Some(sequence.map(voice_layer, rtp_header.sequence_number.0))
        } else {
          None
        };
        let seq = target.downlink.on_send(datagram.len() + 2, now);
        if is_voice {
          target.voice.on_send(seq, now);
          target.maybe_voice_sent_at = Some(now);
        }
        let mut delivery = DatagramHeader::restamp(datagram, seq)?;
        if let Some(sequence_number) = maybe_voice_sequence {
          renumber_rtp(&mut delivery, sequence_number)?;
        }
        route.deliveries.push((receiver, delivery));
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
    let estimate_bps = participant.downlink_bps();
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
    participant.voice.on_feedback(feedback, now);
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
        participant.downlink.after_short_probe(target_bps, floor_bps, now);
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
    if let Some(participant) = self.participants.get_mut(&receiver) {
      participant.update_voice_layer(&self.config.audio_layers, now);
      participant.follow_thin_recovery(&self.config.audio_layers, now);
    }
    effects.extend(self.follow_thin_voice());
    effects.extend(self.reallocate(receiver, now));
    effects.extend(self.probe_downlink(receiver, now));
    effects.extend(self.repair_uplink(receiver, now));
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

  /// `device` says its own upload's voice is hurt, or recovered (#1889): while it is, it gets no
  /// video, whose acknowledgements share that upload. Its arriving voice shows the same only once
  /// the upload's queue overflowed, seconds later.
  pub fn set_uplink_hurt(&mut self, device: DeviceId, is_hurt: bool, now: Instant) -> Vec<Effect> {
    let Some(participant) = self.participants.get_mut(&device) else {
      return Vec::new();
    };
    if participant.is_uplink_reported_hurt == is_hurt {
      return Vec::new();
    }
    participant.is_uplink_reported_hurt = is_hurt;
    self.reallocate(device, now)
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

  /// The audio-only floor of the clients' uplinks (`joined`, #1891).
  pub fn uplink_floor(&self) -> UplinkFloor {
    self.config.uplink_floor
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
    participant.maybe_measured_downlink_bps = Some(measurement.after_burst_bps);
    Effect::DownlinkMeasured {
      receiver,
      measurement,
    }
  }

  /// The next train of padding for the probe `receiver`'s allocation waits for, when one is due:
  /// a cluster starts when the allocation asks for one and the last is long enough ago.
  fn probe_downlink(&mut self, receiver: DeviceId, now: Instant) -> Option<Effect> {
    let participant = self.participants.get_mut(&receiver)?;
    let config = self.config.probe;
    participant.probe.on_timer(&config, now);
    // A thin downlink gets no padding at all, not even the rest of a cluster: its bursts queued in
    // front of the voices (#1893 step 3). Its voices show when it recovered.
    if participant.is_suspended || participant.is_thin_downlink {
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
        .downlink_bps()
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

  /// Follows the publisher's numbering of `layer` of `stream_id` (#2189): a gap is asked for when
  /// someone receives the layer (`is_wanted`).
  fn note_uplink_arrival(
    &mut self,
    stream_id: StreamId,
    layer: u8,
    sequence_number: u16,
    maybe_keyframe: Option<KeyframeFragment>,
    is_wanted: bool,
    now: Instant,
  ) -> RepairStep {
    self
      .video_tracks
      .get_mut(&stream_id)
      .and_then(|route| route.repair.get_mut(usize::from(layer)))
      .map(|repair| repair.on_arrival(sequence_number, maybe_keyframe, is_wanted, now))
      .unwrap_or_default()
  }

  /// Whether someone receives `layer` of `stream_id` or is to switch to it.
  fn is_layer_wanted(&self, stream_id: StreamId, layer: u8) -> bool {
    self
      .video_tracks
      .get(&stream_id)
      .is_some_and(|route| route.is_layer_wanted(layer))
  }

  /// Retries and give-ups of the repair of `publisher`'s uplink; runs on its timer.
  fn repair_uplink(&mut self, publisher: DeviceId, now: Instant) -> Vec<Effect> {
    let mut steps = Vec::new();
    for (&stream_id, route) in &mut self.video_tracks {
      if route.track.device_id != publisher {
        continue;
      }
      let wanted: Vec<bool> = (0..MAX_SIMULCAST_LAYERS)
        .map(|layer| route.is_layer_wanted(layer))
        .collect();
      for ((layer, repair), is_wanted) in (0..MAX_SIMULCAST_LAYERS).zip(&mut route.repair).zip(wanted) {
        if !is_wanted && !repair.is_missing_packets() {
          continue;
        }
        steps.push((stream_id, layer, repair.on_timer(is_wanted, now)));
      }
    }
    let mut effects = Vec::new();
    for (stream_id, layer, step) in steps {
      effects.extend(self.repair_effects(publisher, stream_id, layer, step, now));
    }
    effects
  }

  /// The requests a repair step asks of `publisher`: the packets, and a keyframe (shared with the
  /// receivers' requests, so the publisher gets one at most per [`KeyframeRequestConfig::per_layer_interval`]).
  fn repair_effects(
    &mut self,
    publisher: DeviceId,
    stream_id: StreamId,
    layer: u8,
    step: RepairStep,
    now: Instant,
  ) -> Vec<Effect> {
    let mut effects = Vec::new();
    if !step.requests.is_empty() {
      effects.push(Effect::RetransmitRequest {
        publisher,
        stream_id,
        layer,
        sequence_numbers: step.requests,
      });
    }
    if step.needs_keyframe {
      // The repair's own wait (#2246) decided already: its hold is for the other requests.
      let interval = self.config.keyframes.per_layer_interval;
      effects.extend(self.limited_keyframe_request(publisher, stream_id, layer, interval, now));
    }
    effects
  }

  /// The repair's settings for new tracks: `None` while nothing is kept to resend.
  fn uplink_repair_config(&self) -> Option<UplinkRepairConfig> {
    self
      .config
      .retransmit
      .is_enabled()
      .then_some(self.config.uplink_repair)
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

  /// What a receiver's connection drains, as its transport measured it (`None`: it doesn't).
  /// The layer allocation, resends and probes take its downlink as no more than that from the next
  /// timer on.
  pub fn set_link_capacity(&mut self, receiver: DeviceId, maybe_capacity_bps: Option<u32>) {
    if let Some(participant) = self.participants.get_mut(&receiver) {
      participant.maybe_link_capacity_bps = maybe_capacity_bps;
    }
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
    // The other device of a direct video pair sends its tracks straight to the receiver (#127):
    // they take none of its downlink here.
    let maybe_direct_publisher = self
      .direct_video_pair()
      .and_then(|(first, second)| match receiver {
        device if device == first => Some(second),
        device if device == second => Some(first),
        _ => None,
      });
    let Some(participant) = self
      .participants
      .get_mut(&receiver)
      .filter(|participant| !participant.is_suspended)
    else {
      return Vec::new();
    };
    // Without any feedback yet (older clients) the estimate is just the start value; it still
    // grants the lowest layers.
    let estimate = participant.downlink_bps();
    let max_frame_fragments = participant.capabilities.max_frame_fragments;
    let capabilities = &participant.capabilities;
    let speakers = self.speakers.selected();
    let demands: Vec<TrackDemand<'_>> = self
      .video_tracks
      .iter()
      .filter_map(|(&stream_id, route)| {
        if Some(route.track.device_id) == maybe_direct_publisher {
          return None;
        }
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
    // A connection that measures what it drains proves an upgrade by draining it: probes over TCP
    // would only queue behind the media.
    let proof = if participant.has_feedback && participant.maybe_link_capacity_bps.is_none() {
      UpgradeProof::Shown {
        bps: participant.probe.shown_bps(&self.config.probe, now),
      }
    } else {
      UpgradeProof::NotRequired
    };
    // Its voice is being lost: the video yields at once to the lowest layers, and no probe adds to
    // the link meanwhile; voice still lost there (a link that collapsed) pauses it (#1871). A link
    // that merely dropped below the top layer keeps a picture (paused, a viewer whose link fell to
    // 500 kbit/s stood still for 5 s). The video to a participant whose own voice arrives with gaps pauses at
    // once: the acknowledgements of what it receives share its collapsed upload (#1889).
    let mut is_yielding = false;
    // A receiver that takes the thin voices has no room for video next to them: the link's budget
    // went to probes and video bursts, and its voice paid for each (#1893 step 3).
    let is_thin = participant.is_thin_downlink;
    let allocation = if is_thin || participant.is_uplink_hurt(now) {
      participant.allocator.pause_all(&demands, now)
    } else if participant.voice.is_hurt(now) {
      let yielded_at = *participant.maybe_video_yielded_at.get_or_insert(now);
      let is_hurt_at_lowest = participant
        .voice
        .last_hurt_at()
        .is_some_and(|at| at >= yielded_at + VOICE_HURT_AT_LOWEST_AFTER);
      if is_hurt_at_lowest {
        participant.allocator.pause_all(&demands, now)
      } else {
        is_yielding = true;
        participant.allocator.keep_lowest_layers(&demands, now)
      }
    } else {
      participant.maybe_video_yielded_at = None;
      participant
        .allocator
        .allocate_with_proof(&layers_config, estimate, proof, &demands, now)
    };
    participant.maybe_probe_request = if is_thin { None } else { allocation.maybe_probe };
    let allocation = allocation.layers;
    // The receiver says why its pictures stopped or went coarse (#1891): only while it gets video.
    let is_yielded =
      !demands.is_empty() && (is_thin || participant.is_uplink_hurt(now) || participant.voice.is_hurt(now));
    let maybe_yield_change = (is_yielded != participant.is_video_yielded_for_voice).then(|| {
      participant.is_video_yielded_for_voice = is_yielded;
      Effect::VideoYieldedForVoice { receiver, is_yielded }
    });
    if participant
      .maybe_allocation_logged_at
      .is_none_or(|at| now.saturating_duration_since(at) >= ALLOCATION_LOG_INTERVAL)
    {
      participant.maybe_allocation_logged_at = Some(now);
      debug!(
        receiver = %receiver,
        estimate_bps = participant.downlink.estimate_bps(),
        maybe_link_capacity_bps = ?participant.maybe_link_capacity_bps,
        sent_bps = participant.downlink.sent_bps(now),
        shown_bps = ?proof,
        demand_bps = ?maybe_demand,
        layers = ?allocation.iter().map(|(stream, layer)| (stream.0, *layer)).collect::<Vec<_>>(),
        "layer allocation"
      );
    }

    let stalled_after = self.config.layer_stalled_after;
    let mut effects: Vec<Effect> = maybe_yield_change.into_iter().collect();
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
      // Yielding to lost voice, a higher layer stops at once instead of until the lowest layer's
      // keyframe: that is what loses the voice.
      if is_yielding && subscription.current.is_some_and(|current| current > layer) {
        subscription.current = None;
      }
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

  /// Asks `publisher` for a keyframe on `layer`, unless one was asked for within the interval or
  /// the layer's uplink repair holds them ([`UplinkRepair::holds_keyframes`]).
  fn keyframe_request(
    &mut self,
    publisher: DeviceId,
    stream_id: StreamId,
    layer: u8,
    interval: Duration,
    now: Instant,
  ) -> Option<Effect> {
    // The uplink lost the layer's latest keyframes: its repair asks for the next one once the
    // link drained them (#2246), another now would overflow its queue again.
    let is_held = self
      .video_tracks
      .get(&stream_id)
      .and_then(|route| route.repair.get(usize::from(layer)))
      .is_some_and(|repair| repair.holds_keyframes(now));
    if is_held {
      return None;
    }
    self.limited_keyframe_request(publisher, stream_id, layer, interval, now)
  }

  /// A keyframe request for `layer` of `stream_id`, unless one went out within `interval`.
  fn limited_keyframe_request(
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
    let publisher = self.video_tracks.get(&stream_id)?.track.device_id;
    // The other device of a direct video pair gets the track straight from the publisher (#127).
    let maybe_direct_receiver = self
      .direct_video_pair()
      .and_then(|(first, second)| match publisher {
        device if device == first => Some(second),
        device if device == second => Some(first),
        _ => None,
      });
    let route = self.video_tracks.get_mut(&stream_id)?;
    let arrivals = route.arrivals;
    let is_camera = route.track.kind == VideoTrackKind::Camera;
    let is_starved = is_starved(&arrivals, is_camera, stalled_after, now);
    let published = &route.track.layers;
    // A layer standing in for a stopped target is needed as well, or it might stop too. While
    // nothing of a camera flows, every layer a receiver may take is needed ([`is_starved`]).
    let mut needed: Vec<u8> = route
      .subscribers
      .iter()
      .filter(|(receiver, _)| Some(**receiver) != maybe_direct_receiver)
      .flat_map(|(_, subscription)| {
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

impl ParticipantRoute {
  /// Its downlink as far as anything measured: what its connection drains where the transport
  /// measures that (the WebSocket fallback, #1810), else the estimate from its feedback. Over TCP
  /// the feedback misleads (#1845): the fallback queue's drops look like loss and TCP's bursts like
  /// a growing delay, and with the estimate as a second cap the layers stayed at the lowest one on
  /// a link of several Mbit/s.
  fn downlink_bps(&self) -> u32 {
    self
      .maybe_link_capacity_bps
      .unwrap_or_else(|| self.downlink.estimate_bps())
  }

  /// Its own upload loses its voice (the SFU sees gaps, #1889) or it says so itself.
  fn is_uplink_hurt(&self, now: Instant) -> bool {
    self.is_uplink_reported_hurt || self.uplink_voice.is_hurt(now)
  }

  /// It takes the thin voices because its downlink is thin: the probe at its join said so, or the
  /// estimate stayed below the bound for [`THIN_DOWNLINK_SUSTAINED`]. Not because a voice to it
  /// was lost a moment on a fast link: #1871 handles that, video yields to the lowest layers first
  /// and comes back within seconds. Then it gets no video and no padding (#1893 step 3).
  fn is_on_thin_downlink(&self, audio_layers: &AudioLayerConfig, now: Instant) -> bool {
    let is_measured_thin = self
      .maybe_measured_downlink_bps
      .is_some_and(|bps| bps < audio_layers.thin_below_bps);
    let is_estimate_thin = self
      .maybe_estimate_thin_since
      .is_some_and(|since| now.saturating_duration_since(since) >= THIN_DOWNLINK_SUSTAINED);
    self.voice_layer.is_thin() && (is_measured_thin || is_estimate_thin)
  }

  /// Follows its voices while its downlink is thin, without padding (#1893 step 3): clean thin
  /// voices for a hold bring the full voices back, clean full voices for another the video and the
  /// usual probing. Entered whenever its downlink became thin.
  /// Off a thin downlink, it watches whether the link proves its recovery.
  fn follow_thin_recovery(&mut self, config: &AudioLayerConfig, now: Instant) {
    let is_thin = self.is_on_thin_downlink(config, now);
    if is_thin && !self.is_thin_downlink {
      self.thin_recovery.enter(config, now);
    }
    self.is_thin_downlink = is_thin;
    if !is_thin {
      // A recovered link that carries video proves its recovery (#2139).
      let carried_bps = self.downlink.sent_bps(now);
      self
        .thin_recovery
        .follow_recovered(config, carried_bps, self.voice.is_hurt(now), now);
      return;
    }
    let is_voice_flowing = self
      .maybe_voice_sent_at
      .is_some_and(|at| now.saturating_duration_since(at) < VOICE_FLOWING_WITHIN);
    let stage = self
      .thin_recovery
      .update(config, is_voice_flowing, self.voice.is_hurt(now), now);
    if stage == RecoveryStage::Recovered {
      info!("a thin downlink carried the full voices: video and probing come back");
      self.voice_layer.set_full();
      self.maybe_measured_downlink_bps = None;
      self.maybe_estimate_thin_since = None;
      self.is_thin_downlink = false;
      // At the thin bound, not below it: the usual probing finds out how much more it carries.
      self.downlink.raise_to(config.thin_below_bps);
    }
  }

  /// Follows its downlink: whether it takes the thin voices (#1893 step 3).
  fn update_voice_layer(&mut self, config: &AudioLayerConfig, now: Instant) -> bool {
    if self.is_thin_downlink {
      // Its estimate grows on the voices alone, which show nothing beyond them: only its
      // recovery by the voices leads off a thin downlink ([`Self::follow_thin_recovery`]).
      let is_voice_hurt = self.voice.is_hurt(now);
      return self.voice_layer.update(config, 0, is_voice_hurt, now);
    }
    let estimate_bps = self.downlink_bps();
    if estimate_bps < config.thin_below_bps {
      self.maybe_estimate_thin_since.get_or_insert(now);
    } else {
      self.maybe_estimate_thin_since = None;
    }
    let is_voice_hurt = self.voice.is_hurt(now);
    let is_thin = self.voice_layer.update(config, estimate_bps, is_voice_hurt, now);
    if !is_thin {
      // Back on the full voices: what the join's probe showed is past.
      self.maybe_measured_downlink_bps = None;
    }
    is_thin
  }

  /// The layer of a sender's voice it gets now (#1893): the thin copy while its downlink is thin
  /// or its voice is lost, if it plays long packets and the sender sends that copy.
  fn wanted_voice_layer(&mut self, config: &AudioLayerConfig, has_thin_voice: bool, now: Instant) -> u8 {
    let is_thin = self.update_voice_layer(config, now);
    let is_trying_full_voices =
      self.is_thin_downlink && self.thin_recovery.stage() == RecoveryStage::FullVoices;
    if is_thin && !is_trying_full_voices && has_thin_voice && self.capabilities.has_long_audio_frames() {
      THIN_VOICE_LAYER
    } else {
      0
    }
  }
}

/// Writes `sequence_number` into the RTP header of a datagram (#1893): DAVE covers only the
/// payload, the header is the SFU's to rewrite.
fn renumber_rtp(datagram: &mut [u8], sequence_number: u16) -> Result<(), ForwardError> {
  let header = DatagramHeader::parse(datagram)?;
  let at = header.encoded_len() + RTP_SEQUENCE_OFFSET;
  let actual_bytes = datagram.len();
  let Some(field) = datagram.get_mut(at..at + 2) else {
    // The header was parsed before: this can't be short, but the datagram is no reason to panic.
    return Err(ForwardError::Rtp(RtpError::Truncated {
      needed_bytes: at + 2,
      actual_bytes,
    }));
  };
  field.copy_from_slice(&sequence_number.to_be_bytes());
  Ok(())
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

  /// Whether someone receives `layer` or is to switch to it (its keyframe is awaited).
  fn is_layer_wanted(&self, layer: u8) -> bool {
    self
      .subscribers
      .values()
      .any(|subscription| subscription.current == Some(layer) || subscription.target == Some(layer))
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

  /// #127: the SFU stops forwarding the voices of a 1:1 call's two devices to each other only while
  /// both say their direct path carries them, and their video only while both say it carries that.
  mod direct_path {
    use super::*;

    const AUDIO: Option<DirectPathMedia> = Some(DirectPathMedia::Audio);
    const WITH_VIDEO: Option<DirectPathMedia> = Some(DirectPathMedia::AudioAndVideo);

    #[test]
    fn only_both_saying_so_stops_the_voices_and_either_one_resumes_them() {
      let (mut forwarder, now) = room(2);
      forwarder.set_direct_path(device(1), AUDIO, now);
      assert_eq!(
        delivered(&mut forwarder, 1, now),
        1,
        "one side alone cuts nothing"
      );
      forwarder.set_direct_path(device(2), AUDIO, now);
      assert_eq!(delivered(&mut forwarder, 1, now), 0);
      assert_eq!(delivered(&mut forwarder, 2, now), 0);
      forwarder.set_direct_path(device(2), None, now);
      assert_eq!(delivered(&mut forwarder, 1, now), 1);
      assert_eq!(delivered(&mut forwarder, 2, now), 1);
    }

    #[test]
    fn a_third_device_or_a_lost_connection_ends_it_until_reported_again() {
      let (mut forwarder, now) = both_direct();
      forwarder.add_participant(device(3), stream(3), MediaCapabilities::default(), now);
      assert_eq!(
        delivered(&mut forwarder, 1, now),
        2,
        "the third and the second get it"
      );
      forwarder.remove_participant(device(3), now);
      assert_eq!(
        delivered(&mut forwarder, 1, now),
        1,
        "back to two: reported anew first"
      );

      let (mut forwarder, now) = both_direct();
      forwarder.suspend(device(2), now);
      forwarder.resume(device(2), MediaCapabilities::default(), now);
      assert_eq!(
        delivered(&mut forwarder, 1, now),
        1,
        "a new connection reports anew"
      );
    }

    #[test]
    fn only_a_one_to_one_call_is_a_pair() {
      let (mut forwarder, now) = both_direct();
      assert!(forwarder.is_direct_pair(device(1), device(2)));
      assert!(!forwarder.is_direct_pair(device(1), device(1)));
      let (mut three, _) = room(3);
      for n in 1..=3 {
        three.set_direct_path(device(n), WITH_VIDEO, now);
      }
      assert!(!three.is_direct_pair(device(1), device(2)), "only a 1:1 call");
      assert!(!three.is_direct_video_pair(device(1), device(2)));
      forwarder.set_direct_path(device(1), WITH_VIDEO, now);
      assert!(
        !forwarder.is_direct_video_pair(device(1), device(2)),
        "one side alone"
      );
    }

    #[test]
    fn video_goes_through_the_sfu_unless_both_say_their_path_carries_it() {
      let (mut forwarder, now) = video_pair();
      forwarder.set_direct_path(device(1), WITH_VIDEO, now);
      forwarder.set_direct_path(device(2), AUDIO, now);
      assert_eq!(send_frame(&mut forwarder, 0, false, now), devices(&[2]));
      assert_eq!(delivered(&mut forwarder, 1, now), 0, "the voices go directly");
    }

    /// The publisher stops sending what only the other device took, and starts again with a
    /// keyframe for it when the video comes through the SFU again.
    #[test]
    fn the_pair_s_video_stops_and_resumes_with_keyframes() {
      let (mut forwarder, now) = video_pair();
      forwarder.set_direct_path(device(1), WITH_VIDEO, now);
      let effects = forwarder.set_direct_path(device(2), WITH_VIDEO, now);
      assert!(
        effects.contains(&Effect::LayersNeeded {
          publisher: device(1),
          stream_id: VIDEO,
          layers: Vec::new(),
        }),
        "{effects:?}"
      );
      assert!(send_frame(&mut forwarder, 0, true, now).is_empty());

      let effects = forwarder.set_direct_path(device(2), AUDIO, now);
      assert!(
        effects.iter().any(|effect| matches!(
          effect,
          Effect::LayersNeeded { stream_id: VIDEO, layers, .. } if layers.contains(&0)
        )),
        "{effects:?}"
      );
      assert!(
        effects.iter().any(|effect| matches!(
          effect,
          Effect::KeyframeRequest {
            stream_id: VIDEO,
            layer: 0,
            ..
          }
        )),
        "{effects:?}"
      );
      assert_eq!(send_frame(&mut forwarder, 0, true, now), devices(&[2]));
    }

    #[test]
    fn a_lost_connection_brings_the_video_back_through_the_sfu() {
      let (mut forwarder, now) = video_pair();
      forwarder.set_direct_path(device(1), WITH_VIDEO, now);
      forwarder.set_direct_path(device(2), WITH_VIDEO, now);
      let effects = forwarder.suspend(device(1), now);
      assert!(
        effects.iter().any(|effect| matches!(
          effect,
          Effect::LayersNeeded { stream_id: VIDEO, layers, .. } if layers.contains(&0)
        )),
        "{effects:?}"
      );
    }

    fn both_direct() -> (Forwarder, Instant) {
      let (mut forwarder, now) = room(2);
      forwarder.set_direct_path(device(1), AUDIO, now);
      forwarder.set_direct_path(device(2), AUDIO, now);
      (forwarder, now)
    }

    /// Two devices; the second receives layer 0 of the first's camera.
    fn video_pair() -> (Forwarder, Instant) {
      let (mut forwarder, now) = room(2);
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
      assert_eq!(send_frame(&mut forwarder, 0, true, now), devices(&[2]));
      (forwarder, now)
    }

    /// How many receivers a loud voice of device `n` reaches.
    fn delivered(forwarder: &mut Forwarder, n: u64, now: Instant) -> usize {
      forwarder
        .route(device(n), &audio(n, 10), now)
        .unwrap()
        .deliveries
        .len()
    }
  }

  /// #2417: a screen share whose layers change goes on as the same stream.
  mod replacement {
    use super::*;

    /// The viewer keeps its subscription: once the new layers' keyframe comes it gets them, with
    /// no subscribe of its own, and the publisher is asked for that keyframe and told the layers
    /// needed.
    #[test]
    fn a_share_with_new_layers_keeps_its_stream_and_viewers() {
      let (mut forwarder, now) = shared_screen();
      let mut two_layers = three_layers();
      two_layers.truncate(2);
      let (track, effects) = forwarder
        .replace_video(
          device(1),
          VIDEO,
          &VideoTrackKind::Screen,
          &VideoCodec::Av1,
          two_layers.clone(),
          Some(VideoPublicationId(2)),
          now,
        )
        .unwrap()
        .unwrap();
      assert_eq!(track.stream_id, MediaStreamId(VIDEO.0));
      assert_eq!(track.layers, two_layers);
      assert_eq!(track.maybe_publication_id, Some(VideoPublicationId(2)));
      assert!(
        effects
          .iter()
          .any(|effect| matches!(effect, Effect::KeyframeRequest { stream_id: VIDEO, .. })),
        "{effects:?}"
      );
      assert!(
        effects
          .iter()
          .any(|effect| matches!(effect, Effect::LayersNeeded { stream_id: VIDEO, .. })),
        "{effects:?}"
      );
      // The old layers' deltas don't reach it; the new keyframe does.
      let later = now + Duration::from_millis(40);
      assert!(send_frame(&mut forwarder, 0, false, later).is_empty());
      assert_eq!(send_frame(&mut forwarder, 0, true, later), devices(&[2]));
      assert_eq!(forwarder.video_tracks().len(), 1);
    }

    /// A stream that isn't the owner's track of the same kind and codec is not replaced: the room
    /// publishes a new track then.
    #[test]
    fn only_the_owners_own_kind_and_codec_is_replaced() {
      let (mut forwarder, now) = shared_screen();
      let mut replace = |owner: u64, stream: StreamId, kind: VideoTrackKind, codec: VideoCodec| {
        forwarder
          .replace_video(device(owner), stream, &kind, &codec, three_layers(), None, now)
          .unwrap()
          .is_some()
      };
      assert!(!replace(2, VIDEO, VideoTrackKind::Screen, VideoCodec::Av1));
      assert!(!replace(
        1,
        StreamId(101),
        VideoTrackKind::Screen,
        VideoCodec::Av1
      ));
      assert!(!replace(1, VIDEO, VideoTrackKind::Camera, VideoCodec::Av1));
      assert!(!replace(1, VIDEO, VideoTrackKind::Screen, VideoCodec::Vp9));
      assert!(replace(1, VIDEO, VideoTrackKind::Screen, VideoCodec::Av1));
    }

    /// A track its publisher held (#1891) goes on with the new layers: viewers hear it isn't held.
    #[test]
    fn a_held_share_goes_on_unheld() {
      let (mut forwarder, now) = shared_screen();
      forwarder.set_uplink_layers(device(1), VIDEO, &[], now).unwrap();
      let (_, effects) = forwarder
        .replace_video(
          device(1),
          VIDEO,
          &VideoTrackKind::Screen,
          &VideoCodec::Av1,
          three_layers(),
          None,
          now,
        )
        .unwrap()
        .unwrap();
      assert!(
        effects.contains(&Effect::VideoPausedByPublisher {
          publisher: device(1),
          stream_id: VIDEO,
          is_paused: false,
        }),
        "{effects:?}"
      );
    }

    /// New layers are checked as a publication's are.
    #[test]
    fn invalid_layers_are_refused() {
      let (mut forwarder, now) = shared_screen();
      assert!(matches!(
        forwarder.replace_video(
          device(1),
          VIDEO,
          &VideoTrackKind::Screen,
          &VideoCodec::Av1,
          Vec::new(),
          None,
          now
        ),
        Err(ForwardError::InvalidPublication(_))
      ));
    }

    /// Device 1 shares its screen in three layers; device 2 receives its base layer.
    fn shared_screen() -> (Forwarder, Instant) {
      let (mut forwarder, now) = room(2);
      forwarder
        .publish_video(
          device(1),
          VIDEO,
          VideoTrackKind::Screen,
          VideoCodec::Av1,
          three_layers(),
          Some(VideoPublicationId(1)),
        )
        .unwrap();
      forwarder.subscribe(device(2), VIDEO, Some(0), now).unwrap();
      assert_eq!(send_frame(&mut forwarder, 0, true, now), devices(&[2]));
      assert_eq!(send_frame(&mut forwarder, 0, false, now), devices(&[2]));
      (forwarder, now)
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
      // Ended already (a request that crossed a takeover, #2442): nothing to end.
      assert_eq!(
        forwarder.unpublish_screen_audio(device(1), SCREEN_AUDIO),
        Err(ForwardError::UnknownTrack(SCREEN_AUDIO))
      );
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

    /// #2189: a packet lost on the publisher's uplink is asked of the publisher at once, and its
    /// second copy goes to the receivers like any packet.
    #[test]
    fn a_gap_in_the_publishers_packets_is_asked_of_it_at_once() {
      let (mut forwarder, now) = video_room();
      on_layer_zero(&mut forwarder, &[2], now);
      send_numbered(&mut forwarder, 0, 2, now);
      let after_gap = send_numbered(&mut forwarder, 0, 5, now);
      assert_eq!(after_gap.effects, vec![retransmit_request(&[3, 4])]);
      let repaired = send_numbered(&mut forwarder, 0, 3, now + Duration::from_millis(40));
      assert_eq!(receivers_of(&repaired), devices(&[2]));
      assert!(repaired.effects.is_empty());
      // Kept for the receiver's own NACK like any packet.
      assert_eq!(forwarder.resend(device(2), VIDEO, 0, &[3], now).len(), 1);
      // Packet 4 is asked for again after the publisher's round trip, not packet 3.
      let retry = forwarder.on_timer(device(1), now + Duration::from_millis(200));
      assert!(retry.contains(&retransmit_request(&[4])), "{retry:?}");
    }

    #[test]
    fn an_unrepaired_gap_asks_for_one_keyframe_and_no_storm() {
      let (mut forwarder, now) = video_room();
      on_layer_zero(&mut forwarder, &[2], now);
      let mut sequence_number = 2;
      let mut keyframes = 0;
      // Every 100 ms a packet is lost and never sent again, for five seconds.
      for tick in 0..50u32 {
        let at = now + Duration::from_millis(100) * tick;
        send_numbered(&mut forwarder, 0, sequence_number, at);
        sequence_number += 2;
        let effects = forwarder.on_timer(device(1), at);
        keyframes += effects
          .iter()
          .filter(|effect| matches!(effect, Effect::KeyframeRequest { .. }))
          .count();
      }
      assert!((4..=5).contains(&keyframes), "{keyframes} keyframes in 5 s");
    }

    /// #2246: the uplink lost the tail of a keyframe and nothing repairs it. The repair asks for
    /// the next one; the receiver, which lost the same packets, asks only once the uplink had time
    /// to drain what it queued, not with the next per-layer interval.
    #[test]
    fn a_keyframe_the_uplink_lost_holds_the_receivers_requests() {
      let (mut forwarder, now) = video_room();
      on_layer_zero(&mut forwarder, &[2], now);
      // A keyframe of ten packets numbered 2 to 11: the last two never arrive.
      for index in 0..8u16 {
        send_keyframe_fragment(&mut forwarder, 2 + index, index, 10, now);
      }
      send_numbered(&mut forwarder, 0, 12, now);
      let maybe_asked_at = (1..=10u32)
        .map(|tick| now + Duration::from_millis(100) * tick)
        .find(|at| forwarder.on_timer(device(1), *at).contains(&keyframe_request(0)));
      let asked_at = maybe_asked_at.expect("the repair asks for a keyframe");
      let later = asked_at + Duration::from_millis(600);
      assert!(forwarder.request_keyframe(device(2), VIDEO, later).is_empty());
      let after_wait = asked_at + Duration::from_secs(2);
      assert_eq!(
        forwarder.request_keyframe(device(2), VIDEO, after_wait),
        vec![keyframe_request(0)]
      );
    }

    #[test]
    fn a_camera_waits_longer_for_a_lost_packet_than_a_screen() {
      let config = UplinkRepairConfig::default();
      for (kind, keyframe_after) in [
        (VideoTrackKind::Camera, config.camera_keyframe_after),
        (VideoTrackKind::Screen, config.screen_keyframe_after),
      ] {
        let (mut forwarder, now) = video_room();
        forwarder.unpublish_video(device(1), VIDEO).unwrap();
        forwarder
          .publish_video(
            device(1),
            VIDEO,
            kind.clone(),
            VideoCodec::Av1,
            three_layers(),
            None,
          )
          .unwrap();
        on_layer_zero(&mut forwarder, &[2], now);
        send_numbered(&mut forwarder, 0, 3, now);
        let before = now + keyframe_after - Duration::from_millis(50);
        assert!(
          !forwarder
            .on_timer(device(1), before)
            .contains(&keyframe_request(0))
        );
        let after = now + keyframe_after;
        assert!(
          forwarder
            .on_timer(device(1), after)
            .contains(&keyframe_request(0)),
          "{kind:?}"
        );
      }
    }

    #[test]
    fn layers_nobody_receives_are_not_repaired() {
      let (mut forwarder, now) = video_room();
      on_layer_zero(&mut forwarder, &[2], now);
      send_numbered(&mut forwarder, 1, 1, now);
      assert!(send_numbered(&mut forwarder, 1, 5, now).effects.is_empty());
      let later = now + Duration::from_secs(1);
      assert!(
        !forwarder
          .on_timer(device(1), later)
          .iter()
          .any(|effect| matches!(effect, Effect::RetransmitRequest { .. }))
      );
    }

    #[test]
    fn nothing_is_asked_while_retransmission_is_off() {
      let (mut forwarder, now) = video_room();
      forwarder.config.retransmit.window = Duration::ZERO;
      forwarder.unpublish_video(device(1), VIDEO).unwrap();
      forwarder
        .publish_video(
          device(1),
          VIDEO,
          VideoTrackKind::Screen,
          VideoCodec::Av1,
          three_layers(),
          None,
        )
        .unwrap();
      on_layer_zero(&mut forwarder, &[2], now);
      assert!(send_numbered(&mut forwarder, 0, 5, now).effects.is_empty());
      let later = now + Duration::from_secs(1);
      assert!(
        !forwarder
          .on_timer(device(1), later)
          .contains(&keyframe_request(0))
      );
    }

    fn retransmit_request(sequence_numbers: &[u16]) -> Effect {
      Effect::RetransmitRequest {
        publisher: device(1),
        stream_id: VIDEO,
        layer: 0,
        sequence_numbers: sequence_numbers.to_vec(),
      }
    }

    fn keyframe_request(layer: u8) -> Effect {
      Effect::KeyframeRequest {
        publisher: device(1),
        stream_id: VIDEO,
        layer,
      }
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
        is_capture_stalled: false,
      }
      .write_to(&mut rtp);
      forwarder
        .route(device(1), &rtp_datagram(VIDEO.0, layer, rtp), now)
        .unwrap()
    }

    /// Fragment `index` of `count` of a keyframe on layer 0 of [`VIDEO`] with RTP
    /// `sequence_number`.
    fn send_keyframe_fragment(
      forwarder: &mut Forwarder,
      sequence_number: u16,
      index: u16,
      count: u16,
      now: Instant,
    ) -> Route {
      let mut rtp = header();
      rtp.sequence_number = SequenceNumber(sequence_number);
      FrameFragment {
        frame_number: 100,
        index,
        count,
        is_keyframe: true,
        is_capture_stalled: false,
      }
      .write_to(&mut rtp);
      forwarder
        .route(device(1), &rtp_datagram(VIDEO.0, 0, rtp), now)
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

  /// #1893 step 3: a receiver on a thin downlink gets the others' thin voices, numbered on.
  mod voice_layers {
    use teamcraft_proto::AudioFeature;

    use super::*;
    use crate::audio_layers::THIN_VOICE_LAYER;

    #[test]
    fn each_receiver_gets_the_voice_its_downlink_carries() {
      let (mut forwarder, now) = three_listeners();
      let mut fast = Vec::new();
      let mut thin = Vec::new();
      for index in 0..9u16 {
        let at = now + Duration::from_millis(20) * u32::from(index);
        let mut sent = vec![voice(0, 100 + index)];
        if index % 3 == 0 {
          sent.push(voice(THIN_VOICE_LAYER, 50 + index / 3));
        }
        for datagram in sent {
          for (receiver, delivered) in forwarder.route(device(1), &datagram, at).unwrap().deliveries {
            let packet = received(&delivered);
            if receiver == device(2) {
              fast.push(packet);
            } else if receiver == device(3) {
              thin.push(packet);
            }
          }
        }
      }
      assert!(fast.iter().all(|(layer, _)| *layer == 0), "{fast:?}");
      assert_eq!(fast.len(), 9);
      // Its voice as it is until the first thin copy came, then the thin one, numbered on.
      assert_eq!(
        thin,
        vec![
          (0, 100),
          (THIN_VOICE_LAYER, 101),
          (THIN_VOICE_LAYER, 102),
          (THIN_VOICE_LAYER, 103)
        ]
      );
    }

    /// While the sender sends no thin copy, a thin receiver gets its voice as it is; when the copy
    /// starts, the numbers go on from the last one the receiver got.
    #[test]
    fn a_thin_receiver_gets_the_voice_as_it_is_until_the_thin_copy_comes() {
      let (mut forwarder, now) = three_listeners();
      let mut thin = Vec::new();
      let mut at = now;
      for index in 0..3u16 {
        at = now + Duration::from_millis(20) * u32::from(index);
        thin.extend(to(&mut forwarder, 3, &voice(0, 700 + index), at));
      }
      at += Duration::from_millis(20);
      thin.extend(to(&mut forwarder, 3, &voice(THIN_VOICE_LAYER, 5), at));
      thin.extend(to(&mut forwarder, 3, &voice(0, 703), at));
      thin.extend(to(
        &mut forwarder,
        3,
        &voice(THIN_VOICE_LAYER, 6),
        at + Duration::from_millis(60),
      ));
      assert_eq!(
        thin,
        vec![
          (0, 700),
          (0, 701),
          (0, 702),
          (THIN_VOICE_LAYER, 703),
          (THIN_VOICE_LAYER, 704)
        ]
      );

      // The copy stops while the voice goes on: back to the voice as it is, numbered on, once the
      // copy counts as stopped.
      let mut got = Vec::new();
      for index in 1..=40u16 {
        let later = at + Duration::from_millis(20) * u32::from(index);
        got.extend(to(&mut forwarder, 3, &voice(0, 703 + index), later));
      }
      assert_eq!(got.first(), Some(&(0, 705)), "{got:?}");
      assert_eq!(got.last(), Some(&(0, 705 + got.len() as u16 - 1)));
    }

    /// A new turn: the voice comes back with its full copy a packet or two before the thin one. A
    /// thin receiver waits for the thin one instead of taking the full voice it can't carry.
    #[test]
    fn a_new_turn_reaches_a_thin_receiver_as_its_thin_copy() {
      let (mut forwarder, now) = three_listeners();
      to(&mut forwarder, 3, &voice(0, 100), now);
      assert_eq!(
        to(&mut forwarder, 3, &voice(THIN_VOICE_LAYER, 10), now),
        vec![(THIN_VOICE_LAYER, 101)]
      );
      // A pause of two seconds, then the next turn.
      let turn = now + Duration::from_secs(2);
      assert!(
        to(&mut forwarder, 3, &voice(0, 101), turn).is_empty(),
        "the thin copy is coming"
      );
      assert!(
        to(
          &mut forwarder,
          3,
          &voice(0, 102),
          turn + Duration::from_millis(20)
        )
        .is_empty()
      );
      assert_eq!(
        to(
          &mut forwarder,
          3,
          &voice(THIN_VOICE_LAYER, 11),
          turn + Duration::from_millis(60)
        ),
        vec![(THIN_VOICE_LAYER, 102)]
      );
      // A sender that stopped its thin copy: after the grace, the voice as it is.
      let later = turn + Duration::from_secs(40);
      to(&mut forwarder, 3, &voice(0, 200), later);
      assert_eq!(
        to(
          &mut forwarder,
          3,
          &voice(0, 201),
          later + Duration::from_millis(400)
        ),
        vec![(0, 104)]
      );
    }

    /// The thin copy counts for neither the speakers nor the sender's upload check: a lone copy
    /// doesn't make anybody a speaker.
    #[test]
    fn a_thin_copy_makes_nobody_a_speaker() {
      let (mut forwarder, now) = three_listeners();
      let route = forwarder
        .route(device(1), &voice(THIN_VOICE_LAYER, 1), now)
        .unwrap();
      assert!(
        !route
          .effects
          .iter()
          .any(|effect| matches!(effect, Effect::ActiveSpeakers(_)))
      );
    }

    /// Senders hear that a receiver takes thin copies as soon as one does, and that none does
    /// once it left.
    #[test]
    fn senders_hear_when_a_receiver_takes_thin_voices() {
      let (mut forwarder, now) = three_listeners();
      let wanted = |effects: &[Effect]| {
        effects.iter().find_map(|effect| match effect {
          Effect::ThinVoiceWanted(is_wanted) => Some(*is_wanted),
          _ => None,
        })
      };
      assert_eq!(
        wanted(&forwarder.on_timer(device(2), now)),
        None,
        "a fast receiver"
      );
      assert_eq!(wanted(&forwarder.on_timer(device(3), now)), Some(true));
      assert_eq!(wanted(&forwarder.on_timer(device(3), now)), None, "said once");
      assert!(forwarder.is_thin_voice_wanted());
      let removal = forwarder.remove_participant(device(3), now);
      assert_eq!(wanted(&removal.effects), Some(false));
    }

    /// A receiver on thin voices gets no video, and hears why: the others' video yields for its
    /// voice.
    #[test]
    fn a_thin_receiver_gets_no_video_and_hears_why() {
      let (mut forwarder, now) = video_room();
      forwarder.subscribe(device(2), VIDEO, Some(2), now).unwrap();
      forwarder
        .participants
        .get_mut(&device(2))
        .unwrap()
        .maybe_link_capacity_bps = Some(64_000);
      forwarder
        .participants
        .get_mut(&device(2))
        .unwrap()
        .maybe_measured_downlink_bps = Some(64_000);
      let effects = forwarder.on_timer(device(2), now);
      assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::VideoYieldedForVoice { receiver, is_yielded: true } if *receiver == device(2)
      )));
      for layer in 0..3 {
        assert!(
          !send_frame(&mut forwarder, layer, true, now).contains(&device(2)),
          "layer {layer}"
        );
      }
    }

    /// A receiver on a thin downlink gets no padding: not next to a voice, not in a pause, not
    /// while it speaks itself, not after a long silence.
    #[test]
    fn a_thin_receiver_gets_no_padding() {
      let (mut forwarder, now) = thin_video_receiver();
      let mut at = now;
      for step in 0..600u16 {
        at += Duration::from_millis(100);
        // Device 1 speaks the first 10 s, device 2 itself the next 10 s, then silence.
        if step < 100 {
          forwarder.route(device(1), &voice(0, step), at).unwrap();
        } else if step < 200 {
          forwarder.route(device(2), &audio(2, 20), at).unwrap();
        }
        assert!(
          padding_to(&forwarder.on_timer(device(2), at), 2).is_empty(),
          "padding after {:?}",
          at.saturating_duration_since(now)
        );
      }
    }

    /// A thin receiver whose link turned fast gets the full voices back within a hold of clean
    /// voices, and its video within another: under 30 s, without a single padding packet.
    #[test]
    fn a_thin_receiver_whose_link_turned_fast_recovers() {
      let (mut forwarder, now) = thin_video_receiver();
      let outcome = talk_to(&mut forwarder, now, Duration::from_secs(60), |_, _| true);
      let config = AudioLayerConfig::default();
      let full_voices_after = outcome
        .maybe_full_voices_at
        .expect("never the full voices")
        .saturating_duration_since(now);
      assert!(
        full_voices_after <= config.recovery_hold + Duration::from_secs(1),
        "full voices after {full_voices_after:?}"
      );
      let video_after = outcome
        .maybe_video_at
        .expect("never back to video")
        .saturating_duration_since(now);
      assert!(
        video_after <= Duration::from_secs(30),
        "video after {video_after:?}"
      );
      assert!(!forwarder.participants[&device(2)].voice_layer.is_thin());
      assert_eq!(outcome.padding_before_video, 0);
    }

    /// A thin receiver whose link stays thin (a quarter of its voice lost) stays on the thin
    /// voices without video, and gets no padding.
    #[test]
    fn a_thin_receiver_whose_link_stays_thin_stays() {
      let (mut forwarder, now) = thin_video_receiver();
      let outcome = talk_to(&mut forwarder, now, Duration::from_secs(120), |seq, _| {
        seq % 4 != 0
      });
      assert_eq!(outcome.maybe_full_voices_at, None);
      assert_eq!(outcome.maybe_video_at, None);
      assert_eq!(outcome.padding_before_video, 0);
      assert!(forwarder.participants[&device(2)].is_thin_downlink);
    }

    /// Full voices that a thin link can't carry (a quarter of them lost, the thin ones arrive) go
    /// back to the thin ones, and the next trial waits twice the hold. No video, no padding.
    #[test]
    fn full_voices_a_thin_link_loses_go_back_to_thin() {
      let (mut forwarder, now) = thin_video_receiver();
      let config = AudioLayerConfig::default();
      let outcome = talk_to(&mut forwarder, now, Duration::from_secs(60), |seq, stage| {
        stage != RecoveryStage::FullVoices || seq % 4 != 0
      });
      assert_eq!(outcome.maybe_video_at, None);
      assert_eq!(outcome.padding_before_video, 0);
      let [first, second, ..] = outcome.trials_at[..] else {
        panic!("trials at {:?}", outcome.trials_at);
      };
      assert!(first.saturating_duration_since(now) <= config.recovery_hold + Duration::from_secs(1));
      assert!(second.saturating_duration_since(first) >= 2 * config.recovery_hold);
    }

    fn padding_to(effects: &[Effect], receiver: u64) -> Vec<Vec<u8>> {
      effects
        .iter()
        .filter_map(|effect| match effect {
          Effect::Padding {
            receiver: to,
            datagrams,
          } if *to == device(receiver) => Some(datagrams.clone()),
          _ => None,
        })
        .flatten()
        .collect()
    }

    /// Device 1 speaks; device 2 has a fast downlink, device 3 a thin one. All play long packets.
    fn three_listeners() -> (Forwarder, Instant) {
      let now = Instant::now();
      let mut forwarder = Forwarder::new(ForwardingConfig::default());
      let capabilities = MediaCapabilities {
        audio_features: vec![AudioFeature::LongFrames],
        ..MediaCapabilities::default()
      };
      for n in 1..=3 {
        forwarder.add_participant(device(n), stream(n), capabilities.clone(), now);
      }
      let fast = forwarder.participants.get_mut(&device(2)).unwrap();
      fast.downlink.apply_probe(10_000_000);
      fast.maybe_link_capacity_bps = Some(10_000_000);
      forwarder
        .participants
        .get_mut(&device(3))
        .unwrap()
        .maybe_link_capacity_bps = Some(64_000);
      forwarder
        .participants
        .get_mut(&device(3))
        .unwrap()
        .maybe_measured_downlink_bps = Some(64_000);
      (forwarder, now)
    }

    /// Device 1 publishes a camera; device 2 watches it on a downlink its join measured thin.
    fn thin_video_receiver() -> (Forwarder, Instant) {
      let (mut forwarder, now) = video_room();
      forwarder.subscribe(device(2), VIDEO, Some(2), now).unwrap();
      let receiver = forwarder.participants.get_mut(&device(2)).unwrap();
      receiver.downlink.apply_probe(64_000);
      receiver.maybe_measured_downlink_bps = Some(64_000);
      // The probe at the join is another matter (#1762).
      receiver.maybe_join_probe = None;
      forwarder.on_timer(device(2), now);
      assert!(forwarder.participants[&device(2)].is_thin_downlink);
      (forwarder, now)
    }

    /// What became of a thin receiver while device 1 spoke to it for `talk`.
    struct Recovery {
      maybe_full_voices_at: Option<Instant>,
      maybe_video_at: Option<Instant>,
      padding_before_video: usize,
      /// When it started each trial of the full voices.
      trials_at: Vec<Instant>,
    }

    /// Device 1 speaks to device 2 in 20 ms packets for `talk`; device 2 reports every 100 ms which
    /// of the datagrams it got arrived (`arrives` by transport sequence number and the stage of its
    /// recovery when it was sent), at once.
    fn talk_to(
      forwarder: &mut Forwarder,
      now: Instant,
      talk: Duration,
      arrives: impl Fn(u16, RecoveryStage) -> bool,
    ) -> Recovery {
      let epoch = forwarder.participants[&device(2)].feedback_epoch;
      let mut outcome = Recovery {
        maybe_full_voices_at: None,
        maybe_video_at: None,
        padding_before_video: 0,
        trials_at: Vec::new(),
      };
      let (mut sent, mut next_to_report) = (0u16, 0u16);
      let mut stage_of: HashMap<u16, RecoveryStage> = HashMap::new();
      let mut last_stage = RecoveryStage::Thin;
      let steps = u32::try_from(talk.as_millis() / 20).unwrap();
      for step in 1..=steps {
        let at = now + Duration::from_millis(20) * step;
        let packet = u16::try_from(step % 60_000).unwrap();
        let route = forwarder.route(device(1), &voice(0, packet), at).unwrap();
        let stage = forwarder.participants[&device(2)].thin_recovery.stage();
        for (_, delivered) in route.deliveries.iter().filter(|(to, _)| *to == device(2)) {
          let seq = DatagramHeader::parse(delivered)
            .unwrap()
            .maybe_transport_seq
            .unwrap();
          assert_eq!(seq.0, sent, "numbered in order");
          stage_of.insert(sent, stage);
          sent = sent.wrapping_add(1);
        }
        if step % 5 != 0 {
          continue;
        }
        let since_epoch = u64::try_from(at.saturating_duration_since(epoch).as_micros()).unwrap();
        let feedback = TransportFeedback {
          base_seq: next_to_report,
          arrivals_us: (next_to_report..sent)
            .map(|seq| {
              let stage = stage_of.remove(&seq).unwrap_or(RecoveryStage::Thin);
              arrives(seq, stage).then_some(since_epoch)
            })
            .collect(),
        };
        next_to_report = sent;
        let mut effects = forwarder.on_transport_feedback(device(2), &feedback, at);
        effects.extend(forwarder.on_timer(device(2), at));
        let padding = u16::try_from(padding_to(&effects, 2).len()).unwrap();
        sent = sent.wrapping_add(padding);
        let receiver = &forwarder.participants[&device(2)];
        let stage = receiver.thin_recovery.stage();
        if stage == RecoveryStage::FullVoices && last_stage != stage {
          outcome.trials_at.push(at);
        }
        last_stage = stage;
        if (stage == RecoveryStage::FullVoices && receiver.is_thin_downlink) || !receiver.is_thin_downlink {
          outcome.maybe_full_voices_at.get_or_insert(at);
        }
        if outcome.maybe_video_at.is_none() {
          let is_unyielded = effects.iter().any(|effect| {
            matches!(
              effect,
              Effect::VideoYieldedForVoice {
                is_yielded: false,
                ..
              }
            )
          });
          if is_unyielded {
            // The usual probing comes back with the video, in the same tick.
            outcome.maybe_video_at = Some(at);
          } else {
            outcome.padding_before_video += usize::from(padding);
          }
        }
      }
      outcome
    }

    fn voice(layer: u8, sequence_number: u16) -> Vec<u8> {
      let mut rtp = header();
      rtp.sequence_number = SequenceNumber(sequence_number);
      rtp.set_audio_level(AudioLevel::new(20, true).unwrap());
      stamped(rtp_datagram(1, layer, rtp), sequence_number)
    }

    /// What `receiver` got of `datagram`: layer and RTP sequence number.
    fn to(forwarder: &mut Forwarder, receiver: u64, datagram: &[u8], at: Instant) -> Vec<(u8, u16)> {
      forwarder
        .route(device(1), datagram, at)
        .unwrap()
        .deliveries
        .into_iter()
        .filter(|(to, _)| *to == device(receiver))
        .map(|(_, delivered)| received(&delivered))
        .collect()
    }

    fn received(datagram: &[u8]) -> (u8, u16) {
      let header = DatagramHeader::parse(datagram).unwrap();
      let (rtp, _) = RtpHeader::parse(&datagram[header.encoded_len()..]).unwrap();
      (header.layer.value(), rtp.sequence_number.0)
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

    /// #1810: on the WebSocket fallback, what the connection drains caps the layers whatever the
    /// feedback says; without the cap the feedback decides again.
    #[test]
    fn the_link_capacity_caps_the_layers() {
      let (mut forwarder, now) = video_room();
      forwarder.subscribe(device(2), VIDEO, Some(2), now).unwrap();
      // A probe at the start showed a wide downlink.
      forwarder
        .participants
        .get_mut(&device(2))
        .unwrap()
        .downlink
        .apply_probe(10_000_000);
      let up_at = switched_to(&mut forwarder, 2, now).expect("never switched up");

      forwarder.set_link_capacity(device(2), Some(400_000));
      let down_at = switched_to(&mut forwarder, 0, up_at).expect("the cap didn't switch down");
      assert!(
        down_at - up_at <= Duration::from_millis(100),
        "{:?}",
        down_at - up_at
      );
      assert_eq!(forwarder.participants[&device(2)].downlink_bps(), 400_000);

      forwarder.set_link_capacity(device(2), None);
      assert!(
        switched_to(&mut forwarder, 2, down_at).is_some(),
        "never switched up again"
      );
    }

    /// #1871: voice lost to a receiver takes its video down to the lowest layer at once; voice
    /// still lost there (a collapsed link) pauses it.
    #[test]
    fn lost_voice_takes_the_video_to_the_lowest_layer_then_pauses_it() {
      let (mut forwarder, now) = video_room();
      forwarder.subscribe(device(2), VIDEO, Some(2), now).unwrap();
      forwarder
        .participants
        .get_mut(&device(2))
        .unwrap()
        .downlink
        .apply_probe(10_000_000);
      let up_at = switched_to(&mut forwarder, 2, now).expect("never switched up");
      assert!(send_frame(&mut forwarder, 2, true, up_at).contains(&device(2)));

      // Device 1 speaks; of what reaches device 2, a third is lost.
      let reported_at = up_at + Duration::from_millis(600);
      lose_a_third_of_the_voice(&mut forwarder, up_at, reported_at);
      assert!(send_frame(&mut forwarder, 2, true, reported_at).is_empty());
      assert!(send_frame(&mut forwarder, 0, true, reported_at).contains(&device(2)));

      // Still lost at the lowest layer: the video pauses.
      let start = reported_at + VOICE_HURT_AT_LOWEST_AFTER;
      let reported_at = start + Duration::from_millis(600);
      lose_a_third_of_the_voice(&mut forwarder, start, reported_at);
      assert!(send_frame(&mut forwarder, 0, true, reported_at).is_empty());

      // The hold and a while after: no layer comes back on its own.
      let mut at = reported_at;
      while at < reported_at + Duration::from_secs(10) {
        at += Duration::from_millis(100);
        forwarder.on_timer(device(2), at);
        for layer in 0..3 {
          assert!(
            send_frame(&mut forwarder, layer, true, at).is_empty(),
            "layer {layer} came back at {:?}",
            at - reported_at
          );
        }
      }
    }

    /// #1891: a receiver whose video yields to its voice hears so, and hears when it ends.
    #[test]
    fn a_receiver_is_told_while_its_video_yields_to_its_voice() {
      let (mut forwarder, now) = video_room();
      forwarder.subscribe(device(2), VIDEO, Some(2), now).unwrap();
      let reported_at = now + Duration::from_millis(600);
      lose_a_third_of_the_voice(&mut forwarder, now, reported_at);
      assert!(forwarder.participants[&device(2)].is_video_yielded_for_voice);
      let later = reported_at + Duration::from_secs(5);
      let effects = forwarder.on_timer(device(2), later);
      assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::VideoYieldedForVoice { receiver, is_yielded: false } if *receiver == device(2)
      )));
    }

    /// #1889: a receiver that says its own upload's voice is hurt gets no video until it says
    /// it recovered: the acknowledgements of that video share its upload. At once, before its
    /// arriving voice shows gaps.
    #[test]
    fn a_receiver_whose_upload_is_hurt_gets_no_video_until_it_recovers() {
      let (mut forwarder, now) = video_room();
      forwarder.subscribe(device(2), VIDEO, Some(2), now).unwrap();
      forwarder.on_timer(device(2), now);
      assert!(send_frame(&mut forwarder, 0, true, now).contains(&device(2)));
      let effects = forwarder.set_uplink_hurt(device(2), true, now);
      assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::VideoYieldedForVoice { receiver, is_yielded: true } if *receiver == device(2)
      )));
      for layer in 0..3 {
        assert!(
          !send_frame(&mut forwarder, layer, true, now).contains(&device(2)),
          "layer {layer}"
        );
      }
      assert!(
        forwarder.set_uplink_hurt(device(2), true, now).is_empty(),
        "said once"
      );
      let later = now + Duration::from_secs(1);
      let effects = forwarder.set_uplink_hurt(device(2), false, later);
      assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::VideoYieldedForVoice { receiver, is_yielded: false } if *receiver == device(2)
      )));
    }

    /// #1891: viewers hear when a publisher's uplink carries none of its layers, and when it does
    /// again.
    #[test]
    fn viewers_hear_when_a_publisher_holds_its_video() {
      let (mut forwarder, now) = video_room();
      forwarder.subscribe(device(2), VIDEO, Some(2), now).unwrap();
      let paused = |effects: &[Effect]| {
        effects.iter().find_map(|effect| match effect {
          Effect::VideoPausedByPublisher {
            publisher,
            stream_id,
            is_paused,
          } if *publisher == device(1) && *stream_id == VIDEO => Some(*is_paused),
          _ => None,
        })
      };
      let effects = forwarder.set_uplink_layers(device(1), VIDEO, &[0], now).unwrap();
      assert_eq!(paused(&effects), None, "fewer layers, still a picture");
      let effects = forwarder.set_uplink_layers(device(1), VIDEO, &[], now).unwrap();
      assert_eq!(paused(&effects), Some(true));
      let effects = forwarder
        .set_uplink_layers(device(1), VIDEO, &[0, 1], now)
        .unwrap();
      assert_eq!(paused(&effects), Some(false));
    }

    /// #1889: a participant whose upload collapsed loses its own voice on the way to the SFU, also
    /// after its camera stopped: the acknowledgements of the video it receives share that upload.
    /// It gets no video from the first gaps on.
    #[test]
    fn gaps_in_a_participants_own_voice_pause_its_video() {
      let (mut forwarder, now) = video_room();
      forwarder.subscribe(device(2), VIDEO, Some(2), now).unwrap();
      forwarder
        .participants
        .get_mut(&device(2))
        .unwrap()
        .downlink
        .apply_probe(10_000_000);
      let up_at = switched_to(&mut forwarder, 2, now).expect("never switched up");
      assert!(send_frame(&mut forwarder, 2, true, up_at).contains(&device(2)));

      // Device 2 speaks; a third of its voice doesn't arrive.
      let mut at = up_at;
      for sequence_number in (0..40u16).filter(|seq| seq % 3 != 0) {
        at = up_at + Duration::from_millis(20) * u32::from(sequence_number);
        let mut rtp = header();
        rtp.sequence_number = SequenceNumber(sequence_number);
        rtp.set_audio_level(AudioLevel::new(20, true).unwrap());
        forwarder.route(device(2), &rtp_datagram(2, 0, rtp), at).unwrap();
      }
      for layer in 0..3 {
        assert!(
          send_frame(&mut forwarder, layer, true, at).is_empty(),
          "layer {layer}"
        );
      }
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
      // as the probes before a switch up did; on main (a burst's spread for a measure) a quarter.
      assert!(
        report.lost[0] * 100 <= report.sent[0] * 4,
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

    /// #1792: a viewer's downlink as it really is, not a fixed limit: seeded walks around
    /// 2.8 Mbit/s with dips, handovers and minutes of 15–30 Mbit/s ([`Walk`]). After every dip and
    /// every end of room the forwarded layer fits the link again within about a second; within
    /// 5 s of room appearing the top layer is forwarded; video never stands still outside a
    /// handover.
    #[test]
    fn a_viewer_follows_a_fluctuating_downlink_within_seconds() {
      let layers = share_layers();
      let end = Duration::from_secs(200);
      for seed in [1, 7, 42] {
        let walk = Walk::new(
          seed,
          end,
          (2_000_000, 2_800_000, 3_800_000),
          (15_000_000, 30_000_000),
        );
        let link = Link {
          holds: walk.outages.clone(),
          ..Link::lte()
        };
        let run = simulate_on(link, &layers, &walk.steps, end);
        let video: Vec<&SimulatedDatagram> = run
          .iter()
          .filter(|datagram| datagram.maybe_layer.is_some())
          .collect();
        let bitrate = |layer: u8| {
          layers
            .iter()
            .find(|info| info.layer == layer)
            .map_or(0, |info| info.max_bitrate_bps)
        };
        let after = |from: Duration, is_done: &dyn Fn(&SimulatedDatagram) -> bool| {
          video
            .iter()
            .filter(|datagram| datagram.at >= from)
            .find(|datagram| is_done(datagram))
            .map_or(end.saturating_sub(from), |datagram| datagram.at - from)
        };
        let slowest_top = walk
          .rooms
          .iter()
          .map(|(from, _)| after(*from, &|datagram| datagram.maybe_layer == Some(2)))
          .max()
          .unwrap_or_default();
        // Within a step of the estimate. A layer a little above the link (a fifth) fills its
        // bucket and queue for seconds without a datagram lost or late: no estimate sees that
        // within a dip, and no picture stands still, so only drops whose excess did harm before
        // the switch count (`absorbed` the others).
        let fits = |datagram: &SimulatedDatagram| {
          let layer_bps = datagram.maybe_layer.map_or(0, bitrate);
          f64::from(layer_bps) <= 1.15 * walk.capacity_at(datagram.at) as f64
        };
        let is_harmed = |from: Duration, until: Duration| {
          video
            .iter()
            .filter(|datagram| datagram.at >= from && datagram.at < until)
            .any(|datagram| {
              datagram
                .maybe_arrival
                .is_none_or(|arrival| arrival - datagram.at > Duration::from_millis(300))
            })
        };
        let fit_times: Vec<(Duration, bool)> = walk
          .drops
          .iter()
          .map(|from| {
            let fit = after(*from, &fits);
            (fit, is_harmed(*from, *from + fit))
          })
          .collect();
        let absorbed = fit_times.iter().filter(|(_, is_harmed)| !is_harmed).count();
        let slowest_fit = fit_times
          .iter()
          .filter(|(_, is_harmed)| *is_harmed)
          .map(|(fit, _)| *fit)
          .max()
          .unwrap_or_default();
        let mut arrivals: Vec<Duration> = video
          .iter()
          .filter(|datagram| datagram.at >= Duration::from_secs(5))
          .filter_map(|datagram| datagram.maybe_arrival)
          .collect();
        arrivals.sort_unstable();
        let longest_gap = arrivals
          .windows(2)
          .filter(|pair| !walk.is_near_outage(pair[0], pair[1]))
          .map(|pair| pair[1] - pair[0])
          .max()
          .unwrap_or_default();
        eprintln!(
          "seed {seed}: rooms {}, drops {} ({absorbed} absorbed), slowest top {slowest_top:?}, slowest fit {slowest_fit:?}, longest gap {longest_gap:?}",
          walk.rooms.len(),
          walk.drops.len()
        );
        assert!(
          !walk.rooms.is_empty() && walk.drops.len() > 5,
          "seed {seed}: a walk without room or dips"
        );
        assert!(
          slowest_top <= Duration::from_secs(5),
          "seed {seed}: top layer {slowest_top:?} after room appeared"
        );
        // About a second. A layer a fifth above the link shows only once the link's bucket
        // (32 kB) and queue (150 ms) are full, about a second into the dip; a deeper dip shows
        // sooner (most drops fit within 0.75 s).
        assert!(
          slowest_fit <= Duration::from_millis(1300),
          "seed {seed}: the layer fit {slowest_fit:?} after a drop"
        );
        assert!(
          longest_gap <= Duration::from_millis(700),
          "seed {seed}: video stood still for {longest_gap:?}"
        );
      }
    }

    /// #2139: a viewer whose downlink collapses to 80–200 kbit/s for 16 s and comes back to
    /// 2–5 Mbit/s for 20 s, three times, both swinging every 2 s (the call test rig's
    /// `inner-down.sh`), while the publisher speaks. Its video yields in each collapse and comes
    /// back within seconds of each recovery, and stays until the next collapse: a collapse after a
    /// recovery the link proved is a new one, not a reason to wait longer.
    #[test]
    fn a_viewer_gets_video_back_after_every_collapse() {
      const GOOD_KBPS: [u64; 5] = [2_000, 4_000, 2_500, 5_000, 3_000];
      const BAD_KBPS: [u64; 5] = [120, 80, 200, 100, 150];
      let swing = |from: Duration, seconds: u64, kbps: &[u64], capacities: &mut Vec<(Duration, u64)>| {
        for (index, step) in (0..seconds / 2).enumerate() {
          capacities.push((
            from + Duration::from_secs(step * 2),
            kbps[index % kbps.len()] * 1000,
          ));
        }
      };
      let mut capacities = Vec::new();
      swing(Duration::ZERO, 20, &GOOD_KBPS, &mut capacities);
      // (collapse from, link back from, link good until)
      let mut cycles = Vec::new();
      let mut at = Duration::from_secs(20);
      for _ in 0..3 {
        swing(at, 16, &BAD_KBPS, &mut capacities);
        swing(at + Duration::from_secs(16), 20, &GOOD_KBPS, &mut capacities);
        cycles.push((at, at + Duration::from_secs(16), at + Duration::from_secs(36)));
        at += Duration::from_secs(36);
      }
      let run = simulate_call_on(Link::new(3_000_000), &three_layers(), &capacities, at, true);
      let video_at: Vec<Duration> = run
        .iter()
        .filter(|datagram| datagram.maybe_layer.is_some() && datagram.maybe_arrival.is_some())
        .map(|datagram| datagram.at)
        .collect();
      for (collapse_from, back_from, good_until) in cycles {
        let in_collapse = video_at
          .iter()
          .filter(|at| **at >= collapse_from + Duration::from_secs(4) && **at < back_from)
          .count();
        assert_eq!(in_collapse, 0, "video in the collapse from {collapse_from:?}");
        let after: Vec<Duration> = video_at
          .iter()
          .filter(|at| **at >= back_from && **at < good_until)
          .copied()
          .collect();
        let back_after = after.first().map(|at| *at - back_from);
        eprintln!("link back at {back_from:?}: video after {back_after:?}");
        assert!(
          back_after.is_some_and(|after| after <= Duration::from_secs(12)),
          "video {back_after:?} after the link came back at {back_from:?}"
        );
        let longest_gap = after
          .windows(2)
          .map(|pair| pair[1] - pair[0])
          .max()
          .unwrap_or_default();
        assert!(
          longest_gap <= Duration::from_secs(2),
          "video stood still for {longest_gap:?} on the good link from {back_from:?}"
        );
        // The video that came back doesn't push the voice out.
        let voice_arrivals: Vec<Duration> = run
          .iter()
          .filter(|datagram| datagram.is_voice && datagram.at >= back_from && datagram.at < good_until)
          .filter_map(|datagram| datagram.maybe_arrival)
          .collect();
        let longest_silence = voice_arrivals
          .windows(2)
          .map(|pair| pair[1].saturating_sub(pair[0]))
          .max()
          .unwrap_or_default();
        assert!(
          longest_silence <= Duration::from_millis(500),
          "the voice stopped for {longest_silence:?} on the good link from {back_from:?}"
        );
      }
    }

    /// Runs the receiver's timer and keyframes of every layer until it is switched to `layer`;
    /// when, if within 30 s.
    fn switched_to(forwarder: &mut Forwarder, layer: u8, from: Instant) -> Option<Instant> {
      let request = Effect::KeyframeRequest {
        publisher: device(1),
        stream_id: VIDEO,
        layer,
      };
      let mut at = from;
      while at < from + Duration::from_secs(30) {
        at += Duration::from_millis(100);
        if forwarder.on_timer(device(2), at).contains(&request) {
          return Some(at);
        }
        for layer in 0..3 {
          send_frame(forwarder, layer, true, at);
        }
      }
      None
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
      link: Link,
      layers: &[VideoLayerInfo],
      capacities: &[(Duration, u64)],
      duration: Duration,
    ) -> Vec<SimulatedDatagram> {
      simulate_call_on(link, layers, capacities, duration, false)
    }

    /// [`simulate_on`]; `is_speaking`: the publisher speaks all along as well, a voice packet every
    /// 20 ms.
    fn simulate_call_on(
      mut link: Link,
      layers: &[VideoLayerInfo],
      capacities: &[(Duration, u64)],
      duration: Duration,
      is_speaking: bool,
    ) -> Vec<SimulatedDatagram> {
      let (mut forwarder, start) = room(3);
      link.maybe_origin = Some(start);
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
      let mut next_voice = start;
      let mut voice_number = 0u16;
      let mut sent: Vec<SimulatedDatagram> = Vec::new();
      let mut send = |link: &mut Link, delivered: &[u8], now: Instant, in_flight: &mut Vec<_>| {
        let parsed = DatagramHeader::parse(delivered).unwrap();
        let is_padding = parsed.stream_id == teamcraft_media::datagram::PADDING_STREAM;
        let is_voice = parsed.stream_id == stream(1);
        let maybe_arrival = link.send(delivered.len(), now);
        sent.push(SimulatedDatagram {
          at: now - start,
          maybe_layer: (!is_padding && !is_voice).then_some(parsed.layer.value()),
          is_voice,
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
        // About 64 kbit/s on the wire, as a full voice.
        while is_speaking && next_voice <= now {
          let mut rtp = header();
          rtp.sequence_number = SequenceNumber(voice_number);
          rtp.set_audio_level(AudioLevel::new(20, true).unwrap());
          let packet = RtpPacket {
            header: rtp,
            payload: &[0; 120],
          }
          .to_bytes()
          .unwrap();
          let datagram = MediaDatagram {
            header: DatagramHeader {
              stream_id: stream(1),
              layer: SimulcastLayer::new(0).unwrap(),
              maybe_transport_seq: None,
            },
            rtp: &packet,
          }
          .to_bytes(1200)
          .unwrap();
          let route = forwarder.route(device(1), &datagram, now).unwrap();
          for (receiver, delivered) in route.deliveries {
            if receiver == device(2) {
              send(&mut link, &delivered, now, &mut in_flight);
            }
          }
          voice_number = voice_number.wrapping_add(1);
          next_voice += Duration::from_millis(20);
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

  /// A mobile link as a user has it (#1792), seeded: every 500 ms the rate walks by up to
  /// ±15 % within `(low, typical, high)`; now and then (4 % of the steps) a dip to 40 % for 1 to
  /// 2 s, a tunnel or a crowded cell, or (2 %) a handover that holds everything for 200 to
  /// 500 ms ([`Link::holds`]); and every 60 to 90 s a cell with room, 30 to 60 s within `rooms`.
  /// The sender's side has the same in `teamcraft_media::rate_probe`'s tests.
  struct Walk {
    /// From when, which capacity.
    steps: Vec<(Duration, u64)>,
    /// When a dip began or room ended: the layer has to fit the link again.
    drops: Vec<Duration>,
    outages: Vec<(Duration, Duration)>,
    rooms: Vec<(Duration, Duration)>,
  }

  impl Walk {
    const STEP: Duration = Duration::from_millis(500);

    fn new(seed: u64, end: Duration, (low, typical, high): (u64, u64, u64), rooms: (u64, u64)) -> Self {
      let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
      let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
      };
      let mut walk = Self {
        steps: Vec::new(),
        drops: Vec::new(),
        outages: Vec::new(),
        rooms: Vec::new(),
      };
      let mut rate = typical as f64;
      let mut at = Duration::ZERO;
      let mut next_room = Duration::from_secs(30 + next() % 20);
      while at < end {
        if at >= next_room && at + Duration::from_secs(45) < end {
          let length = Duration::from_secs(30 + next() % 31).min(end - at - Duration::from_secs(15));
          let mut room_rate = (rooms.0 + next() % (rooms.1 - rooms.0)) as f64;
          let until = at + length;
          walk.rooms.push((at, until));
          while at < until {
            walk.steps.push((at, room_rate as u64));
            room_rate =
              (room_rate * (0.9 + (next() % 200) as f64 / 1000.0)).clamp(rooms.0 as f64, rooms.1 as f64);
            at += Self::STEP;
          }
          walk.drops.push(at);
          next_room = at + Duration::from_secs(60 + next() % 30);
          continue;
        }
        let roll = next() % 100;
        if roll < 2 {
          let length = Duration::from_millis(200 + next() % 300);
          walk.outages.push((at, at + length));
          at += length;
        } else if roll < 6 {
          walk.steps.push((at, (rate * 0.4) as u64));
          walk.drops.push(at);
          at += Duration::from_millis(1000 + next() % 1000);
        } else {
          rate = (rate * (0.85 + (next() % 300) as f64 / 1000.0)).clamp(low as f64, high as f64);
          walk.steps.push((at, rate as u64));
          at += Self::STEP;
        }
      }
      walk
    }

    fn capacity_at(&self, at: Duration) -> u64 {
      self
        .steps
        .iter()
        .rev()
        .find(|(from, _)| *from <= at)
        .map_or(1_000_000, |(_, bps)| *bps)
    }

    /// Whether a gap from `from` to `until` touches a handover or the second after it.
    fn is_near_outage(&self, from: Duration, until: Duration) -> bool {
      self
        .outages
        .iter()
        .any(|(start, end)| from < *end + Duration::from_secs(1) && until > *start)
    }
  }

  /// One datagram the simulated SFU sent to the receiver.
  #[derive(Debug, Clone, Copy)]
  struct SimulatedDatagram {
    /// When it was sent, from the simulation's start.
    at: Duration,
    /// Its video layer; `None` for padding and voice.
    maybe_layer: Option<u8>,
    is_voice: bool,
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
    /// Handovers, from the simulation's start (`maybe_origin`): the link holds what it gets and
    /// hands it over once they end.
    holds: Vec<(Duration, Duration)>,
    maybe_origin: Option<Instant>,
    maybe_last_arrival: Option<Instant>,
  }

  impl Link {
    fn new(capacity_bps: u64) -> Self {
      Self {
        capacity_bps,
        queue_limit: Duration::from_millis(300),
        bucket_bytes: 0,
        maybe_empty_at: None,
        holds: Vec::new(),
        maybe_origin: None,
        maybe_last_arrival: None,
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
      let through = match self.maybe_origin {
        Some(origin) => self
          .holds
          .iter()
          .find(|(from, until)| departure >= origin + *from && departure < origin + *until)
          .map_or(departure, |(_, until)| origin + *until),
        None => departure,
      };
      let arrival = through + Duration::from_millis(30);
      let arrival = self.maybe_last_arrival.map_or(arrival, |last| arrival.max(last));
      self.maybe_last_arrival = Some(arrival);
      Some(arrival)
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
  /// Device 1 speaks for half a second from `from`; device 2 reports a third of it lost at
  /// `reported_at`.
  fn lose_a_third_of_the_voice(forwarder: &mut Forwarder, from: Instant, reported_at: Instant) {
    let mut voice = Vec::new();
    for index in 0..25u32 {
      let at = from + Duration::from_millis(20) * index;
      let route = forwarder.route(device(1), &audio(1, 20), at).unwrap();
      for (receiver, delivered) in route.deliveries {
        if receiver == device(2) {
          voice.push(
            DatagramHeader::parse(&delivered)
              .unwrap()
              .maybe_transport_seq
              .unwrap(),
          );
        }
      }
    }
    assert_eq!(voice.len(), 25);
    let first = voice[0].0;
    let feedback = TransportFeedback {
      base_seq: first,
      arrivals_us: voice
        .iter()
        .enumerate()
        .map(|(index, seq)| (index % 3 != 0).then_some(u64::from(seq.0.wrapping_sub(first)) * 20_000))
        .collect(),
    };
    forwarder.on_transport_feedback(device(2), &feedback, reported_at);
  }

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
      is_capture_stalled: false,
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
