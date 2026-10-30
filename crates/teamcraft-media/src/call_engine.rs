//! Platform-neutral call client: signaling, DAVE group, audio and video framing and bandwidth
//! feedback.
//!
//! Sans I/O: the engine never touches sockets, clocks or devices. The embedding driver (the native
//! driver in [`crate::call_native`], the browser worker, `teamcraft-core`) feeds it signaling
//! messages, datagrams, encoded audio and video and the current time, and drains [`EngineOutput`]s: messages
//! to send on the signaling stream, datagrams to send, and events for the app. That keeps one
//! implementation for desktop, web and mobile and makes it testable without a network.
//!
//! Trust: DAVE proposals name the participants the SFU wants to add. If the app has a roster it
//! trusts (from `teamcraft-server`, verified like chat membership), it passes it with
//! [`CallEngine::set_trusted_roster`] and proposals for anyone else are rejected; without one the
//! engine falls back to the SFU's list and says so with [`CallEngineEvent::UntrustedRoster`].
//!
//! Lost connections: the driver reports [`CallEngine::connection_lost`] and later
//! [`CallEngine::rejoin`]s on a new connection. The engine keeps what survives a rejoin (RTP
//! numbering, own tracks, subscriptions, mute state), starts DAVE over with a new key package and
//! reconciles everything else with the SFU's fresh `joined` (ARCHITECTURE.md §6.5). Video layers
//! of own tracks follow the uplink estimate, so audio keeps flowing when bandwidth drops.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "call-view")]
use teamcraft_client_api::video_stats::VideoStatsReport;
use teamcraft_proto::{
  Base64Bytes, CallClientMessage, CallEvent, CallId, CallParticipant, CallServerMessage, DaveKeyBinding,
  DeviceId, ErrorBody, LEGACY_SIMULCAST_LAYERS, MAX_SIMULCAST_LAYERS, MediaCapabilities, MediaStreamId,
  ScreenAudioTrack, TransportFeedback, UserId, VideoCodec, VideoLayerInfo, VideoPublicationId, VideoTrack,
  VideoTrackKind,
};
use thiserror::Error;
use tracing::{debug, warn};
// `std::time::Instant` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

use crate::audio_frame::EncodedAudioFrame;
use crate::bwe::{BweConfig, FeedbackRecorder, SendSideEstimator};
use crate::call_auth::{CallKeyAuthenticator, CallKeyError};
use crate::call_video::{IncomingVideo, OutgoingVideo, ReceivedVideoFrame};
use crate::datagram::{DatagramHeader, MediaDatagram, SimulcastLayer, StreamId};
use crate::dave::{DaveCallId, DaveError, DaveParticipant, DaveParticipantId, FrameCodec, MediaKind};
use crate::rtp::{PayloadType, RtpHeader, RtpPacket, RtpTimestamp, SequenceNumber, Ssrc};
use crate::uplink_layers::{OwnTrack, UplinkLayers, UplinkPolicy, probe_demand_bps};
use crate::video::EncodedVideoFrame;

/// RTP timestamp increment per 20 ms Opus frame at 48 kHz.
const OPUS_FRAME_TICKS: u32 = 960;
/// Largest datagram the engine produces; audio frames are far below it.
const MAX_DATAGRAM_BYTES: usize = 1200;
/// Keyframe requests per received track go out at most this often: the SFU's default limit per
/// receiver and track (`KeyframeRequestConfig`), so none is wasted. A keyframe of a big screen
/// takes a while to arrive; asking faster would only produce more of them.
const KEYFRAME_REQUEST_INTERVAL: Duration = Duration::from_secs(1);

/// One call from this device's point of view.
pub struct CallEngine {
  config: CallEngineConfig,
  phase: Phase,
  participants: BTreeMap<DeviceId, CallParticipant>,
  /// Other devices' audio streams: microphones and shared screen sound.
  audio_streams: HashMap<StreamId, (DeviceId, AudioOrigin)>,
  /// This device's shared screen sound.
  screen_audio: ScreenAudioState,
  /// Tracks this device publishes, by the stream id the SFU assigned.
  own_video: HashMap<StreamId, OutgoingVideo>,
  /// The same tracks as announced (kind, codec, layers): to fit them into the uplink and to
  /// publish them again after a rejoin.
  own_tracks: HashMap<StreamId, VideoTrack>,
  /// Publications asked for whose answer hasn't arrived yet, in the order they were asked for.
  pending_publications: VecDeque<PendingPublication>,
  /// Layers of own tracks the SFU asked for; without an entry, all of them.
  sfu_layers: HashMap<StreamId, Vec<u8>>,
  /// Layers of own tracks the driver was last told to send.
  sent_layers: HashMap<StreamId, Vec<u8>>,
  uplink_layers: UplinkLayers,
  /// Own tracks whose encoders follow the uplink's bitrate per layer
  /// ([`CallEngine::publish_adaptive_video`]).
  adaptive_tracks: HashSet<StreamId>,
  /// Bitrates per layer the driver was last told to encode adaptive own tracks at (it starts
  /// with the layers' own).
  bitrates_announced: HashMap<StreamId, Vec<(u8, u32)>>,
  /// Layers of own tracks the SFU was last told the uplink carries (it starts with all of them);
  /// without an entry, unknown to it (after a rejoin).
  uplink_announced: HashMap<StreamId, Vec<u8>>,
  uplink_policy: UplinkPolicy,
  /// Other devices' tracks this device receives, with the highest layer asked for.
  subscriptions: HashMap<StreamId, u8>,
  /// How long the SFU keeps this device's slot after the connection was lost.
  maybe_reconnect_grace: Option<Duration>,
  /// The video codecs every other device that receives video decodes, as the SFU last said
  /// (#565); `None` before it said (an older SFU never does): H.264 only.
  maybe_decodable: Option<Vec<VideoCodec>>,
  /// The most simulcast layers per track the SFU takes, as its last `joined` said (#978;
  /// [`LEGACY_SIMULCAST_LAYERS`] from an SFU that says nothing); `None` before the first.
  maybe_sfu_layer_limit: Option<u8>,
  /// Tracks other devices publish.
  remote_video: HashMap<StreamId, IncomingVideo>,
  maybe_dave: Option<DaveParticipant>,
  maybe_trusted_roster: Option<BTreeSet<DeviceId>>,
  maybe_authenticator: Option<Arc<dyn CallKeyAuthenticator>>,
  /// DAVE keys of other members from verified call key bindings (e2ee.md §8).
  verified_keys: HashMap<DeviceId, Vec<u8>>,
  /// A member failed key verification: media stays off for this call.
  is_compromised: bool,
  /// The call is over for this device whatever happens to the connection: the SFU refused the
  /// join, a host removed the user or ended the call, or the device joined on another connection.
  is_over: bool,
  has_reported_unauthenticated_keys: bool,
  uplink: SendSideEstimator,
  downlink_feedback: FeedbackRecorder,
  /// Base for the SFU's arrival times in uplink feedback (only differences matter).
  uplink_feedback_epoch: Instant,
  /// When the engine was made: the base of arrival times for the jitter of received video.
  started_at: Instant,
  microphone_rtp: AudioRtpState,
  screen_rtp: AudioRtpState,
  last_event_seq: u64,
  is_muted: bool,
  /// The mute state the SFU was last told (it starts a slot unmuted).
  is_mute_announced: bool,
  is_deafened: bool,
  outputs: VecDeque<EngineOutput>,
}

/// Static settings of a call.
#[derive(Debug, Clone)]
pub struct CallEngineConfig {
  pub user_id: UserId,
  pub device_id: DeviceId,
  pub capabilities: MediaCapabilities,
  pub bwe: BweConfig,
}

/// Something the driver has to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineOutput {
  /// Send on the signaling stream.
  Signal(CallClientMessage),
  /// Send as a QUIC datagram.
  Datagram(Vec<u8>),
  /// Tell the app.
  Event(CallEngineEvent),
}

/// What the app hears about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallEngineEvent {
  Joined {
    call_id: CallId,
    participants: Vec<CallParticipant>,
  },
  ParticipantJoined(CallParticipant),
  ParticipantLeft(DeviceId),
  /// This device's connection was lost; the driver is getting it back into the call. Media is
  /// paused meanwhile.
  Reconnecting,
  /// Back in the call on a new connection; changes while away came as the usual events.
  Reconnected,
  /// Another device's connection was lost; it keeps its place while it reconnects.
  ParticipantReconnecting(DeviceId),
  ParticipantReconnected(DeviceId),
  ActiveSpeakers(Vec<DeviceId>),
  /// Another device muted or unmuted its microphone.
  ParticipantMuted {
    device_id: DeviceId,
    is_muted: bool,
  },
  /// The DAVE group (re)keyed; media is end-to-end encrypted. Compare `privacy_code` out of band.
  Secured {
    epoch: u64,
    privacy_code: String,
  },
  /// A decrypted audio frame for the jitter buffer (one per sender and origin).
  AudioFrame {
    sender: DeviceId,
    origin: AudioOrigin,
    sequence_number: SequenceNumber,
    opus: Vec<u8>,
  },
  /// A video track was published; for this device's own tracks it carries the stream id to send
  /// on. Subscribe to other devices' tracks with [`CallEngine::subscribe_video`].
  VideoPublished(VideoTrack),
  VideoUnpublished(MediaStreamId),
  /// Not from the engine but from the media driver: an own track ended by itself (a camera
  /// unplugged or taken away by the system, a share ended from the system's indicator or the
  /// browser's bar, an encoder that kept failing). The driver already stopped and unpublished it;
  /// the UI shows it off. A track the user stopped or replaced is not reported.
  OwnVideoEnded {
    kind: VideoTrackKind,
    end: OwnVideoEnd,
  },
  /// A device (this one included) started or stopped sending its shared screen's sound.
  ScreenAudioChanged {
    device_id: DeviceId,
    is_shared: bool,
  },
  /// A decrypted, complete video frame for the decoder.
  VideoFrame(ReceivedVideoFrame),
  /// Encode a keyframe on this layer of an own track soon (a receiver switched to it).
  KeyframeRequested {
    stream_id: MediaStreamId,
    layer: u8,
  },
  /// The layers of an own track someone receives; the others may be paused.
  LayersNeeded {
    stream_id: MediaStreamId,
    layers: Vec<u8>,
  },
  /// Encode the layers of an own track of [`CallEngine::publish_adaptive_video`] at these
  /// bitrates, (layer, bit/s) for every layer: a camera's top sent layer at less than its own
  /// while the uplink can't carry all of it (#939), each layer at its own again once it can.
  LayerBitrates {
    stream_id: MediaStreamId,
    bitrates: Vec<(u8, u32)>,
  },
  MuteChanged(bool),
  DeafenChanged(bool),
  /// The meeting host muted everyone (the engine muted itself; `MuteChanged` follows).
  MutedByHost {
    by_user_id: UserId,
  },
  /// The meeting host removed this user; the SFU closes the connection.
  RemovedByHost {
    by_user_id: UserId,
  },
  /// The meeting host ended the call for everyone; the SFU closes the connection.
  EndedByHost {
    by_user_id: UserId,
  },
  /// This device joined the call on another connection (another window); this one ends.
  ConnectionReplaced,
  /// Estimated uplink bandwidth and loss after new feedback. Audio encoders plan their forward
  /// error correction for `loss_percent`.
  UplinkEstimate {
    bps: u32,
    loss_percent: u8,
  },
  /// The SFU proposed members without a trusted roster to check them against.
  UntrustedRoster,
  /// No authenticator: DAVE members are not bound to device identities (development only).
  UnauthenticatedKeys,
  /// The video codecs every other device that receives video decodes changed (#565): a driver
  /// checks its own tracks against [`CallEngine::video_codec_for`] and publishes a track anew in
  /// another codec where the answer changed.
  DecodableVideoCodecs(Vec<VideoCodec>),
  /// The SFU rejected a request.
  Error(ErrorBody),
}

/// When an own track that ended by itself ([`CallEngineEvent::OwnVideoEnded`]) ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnVideoEnd {
  /// Before its first encoded picture: a start that failed. A native caller waiting for the
  /// first picture (`FirstPicture`) hears about it there too, with the cause.
  BeforeFirstPicture,
  /// After it sent pictures: the camera or share went away mid-call.
  WhileSending,
}

/// A driver's view of the publication of one of its own tracks (#929): the id it published the
/// track under ([`CallEngine::publish_video`]) and the stream the SFU confirmed for it. Following
/// the engine's events, it takes exactly the answer to its own request, never one meant for an
/// earlier or later track of the same kind. Stopping the track, confirmed or not, goes through
/// [`CallEngine::withdraw_video`] with the id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnPublication {
  pub id: VideoPublicationId,
  /// `None` until the SFU confirmed the publication, and again once it dropped the stream (a
  /// rejoin without the slot) until the engine's renewed request under the same id is confirmed.
  pub maybe_stream_id: Option<MediaStreamId>,
}

/// What a driver counts during a call, for tests and diagnostics.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CallStats {
  /// Audio frames (20 ms) sent and received; video is counted per kind below.
  pub frames_sent: u64,
  pub frames_received: u64,
  /// 20 ms mix frames with audible content.
  pub audible_frames_played: u64,
  pub camera: VideoStats,
  pub screen: VideoStats,
  pub maybe_privacy_code: Option<String>,
}

/// One kind of video (camera or screen) during a call. Encoded pictures count per layer: a
/// captured frame becomes up to four pictures with simulcast.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoStats {
  /// Frames the own capture delivered.
  pub frames_captured: u64,
  /// Of those, frames that showed nothing but black (`picture_check`).
  pub frames_black: u64,
  /// Pictures the own encoders produced.
  pub pictures_encoded: u64,
  /// Own pictures sent to the SFU.
  pub pictures_sent: u64,
  /// Own pictures not sent: the track not (yet) confirmed, the call not yet secured, or
  /// encryption or packetization failed.
  pub pictures_dropped: u64,
  /// Own pictures of layers nobody receives or the uplink can't carry (not a failure).
  pub pictures_paused: u64,
  /// Pictures of others' tracks of this kind that arrived (decrypted).
  pub pictures_received: u64,
}

/// What became of an own video picture handed to [`CallEngine::send_video`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoSendOutcome {
  Sent,
  /// The SFU hasn't confirmed the track (yet), or it was unpublished.
  NotPublished,
  /// Nobody receives this layer, or the uplink can't carry it.
  LayerPaused,
  /// DAVE isn't ready yet: nothing leaves unencrypted.
  NotSecured,
  /// Encryption or packetization failed (logged by the engine).
  Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
  Idle,
  Joining,
  Joined {
    call_id: CallId,
    audio_stream: StreamId,
  },
  /// The connection was lost; waiting for a new one.
  Reconnecting {
    call_id: CallId,
  },
  /// Sent `join` on a new connection; the SFU's `joined` resumes the call.
  Rejoining {
    call_id: CallId,
  },
}

/// Which of a device's audio streams a frame belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AudioOrigin {
  Microphone,
  /// The sound of a shared screen or window (system audio).
  Screen,
}

/// This device's screen sound: requested from the SFU, then sent on the stream it assigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreenAudioState {
  Off,
  Requested,
  Publishing(StreamId),
}

/// A video publication asked for whose answer (the SFU's `video_published`) hasn't arrived yet.
struct PendingPublication {
  id: VideoPublicationId,
  kind: VideoTrackKind,
  codec: VideoCodec,
  layers: Vec<VideoLayerInfo>,
  /// The driver stopped the track before the answer came: the track the answer brings has no
  /// sender and is unpublished at once (#929).
  is_withdrawn: bool,
  /// The driver's encoders follow [`CallEngineEvent::LayerBitrates`].
  adapts_bitrate: bool,
}

/// What the SFU's `joined` says about the call when this device comes back.
struct RejoinedState {
  participants: Vec<CallParticipant>,
  video_tracks: Vec<VideoTrack>,
  screen_audio_tracks: Vec<ScreenAudioTrack>,
}

/// RTP numbering of one outgoing audio stream.
#[derive(Debug, Clone, Copy)]
struct AudioRtpState {
  sequence: SequenceNumber,
  timestamp: RtpTimestamp,
}

#[derive(Debug, Error)]
pub enum CallEngineError {
  #[error("event sequence jumped from {last} to {received}")]
  SequenceGap { last: u64, received: u64 },
  #[error("protocol violation: {0}")]
  Protocol(&'static str),
  #[error("DAVE: {0}")]
  Dave(#[from] DaveError),
  #[error("call key: {0}")]
  CallKey(#[from] CallKeyError),
  #[error("device {0} is in the DAVE group without a verified call key")]
  UnverifiedMember(DeviceId),
}

impl CallEngine {
  pub fn new(config: CallEngineConfig, now: Instant) -> Self {
    let uplink = SendSideEstimator::new(config.bwe);
    let uplink_layers = UplinkLayers::new(config.bwe.start_bps);
    Self {
      config,
      phase: Phase::Idle,
      participants: BTreeMap::new(),
      audio_streams: HashMap::new(),
      screen_audio: ScreenAudioState::Off,
      own_video: HashMap::new(),
      own_tracks: HashMap::new(),
      pending_publications: VecDeque::new(),
      sfu_layers: HashMap::new(),
      sent_layers: HashMap::new(),
      uplink_layers,
      adaptive_tracks: HashSet::new(),
      bitrates_announced: HashMap::new(),
      uplink_announced: HashMap::new(),
      uplink_policy: UplinkPolicy::default(),
      subscriptions: HashMap::new(),
      maybe_reconnect_grace: None,
      maybe_decodable: None,
      maybe_sfu_layer_limit: None,
      remote_video: HashMap::new(),
      maybe_dave: None,
      maybe_trusted_roster: None,
      maybe_authenticator: None,
      verified_keys: HashMap::new(),
      is_compromised: false,
      is_over: false,
      has_reported_unauthenticated_keys: false,
      uplink,
      downlink_feedback: FeedbackRecorder::new(now),
      uplink_feedback_epoch: now,
      started_at: now,
      microphone_rtp: AudioRtpState::new(),
      screen_rtp: AudioRtpState::new(),
      last_event_seq: 0,
      is_muted: false,
      is_mute_announced: false,
      is_deafened: false,
      outputs: VecDeque::new(),
    }
  }

  /// Starts joining with a join token from `teamcraft-server`.
  pub fn join(&mut self, token: String) {
    self.phase = Phase::Joining;
    self.signal(CallClientMessage::Join {
      token,
      capabilities: self.config.capabilities.clone(),
    });
  }

  /// The connection to the SFU is gone. Media pauses (the others' DAVE group re-keys without
  /// this device) until [`Self::rejoin`] brings the device back into its slot. Also call it when
  /// a rejoin attempt's connection fails.
  pub fn connection_lost(&mut self) {
    match self.phase {
      Phase::Joined { call_id, .. } => {
        self.phase = Phase::Reconnecting { call_id };
        self.maybe_dave = None;
        self.event(CallEngineEvent::Reconnecting);
      },
      Phase::Rejoining { call_id } => {
        self.phase = Phase::Reconnecting { call_id };
        self.maybe_dave = None;
      },
      Phase::Idle | Phase::Joining | Phase::Reconnecting { .. } => {},
    }
  }

  /// Joins the call again on a new connection after [`Self::connection_lost`], with a fresh
  /// join token. The RTP numbering continues and own tracks and subscriptions are kept or
  /// renewed; DAVE starts over with a new key package. [`CallEngineEvent::Reconnected`] follows
  /// the SFU's answer.
  pub fn rejoin(&mut self, token: String, now: Instant) {
    match self.phase {
      Phase::Reconnecting { call_id } => self.phase = Phase::Rejoining { call_id },
      // Lost before the first `joined`: join again from scratch.
      Phase::Joining => {},
      Phase::Idle | Phase::Joined { .. } | Phase::Rejoining { .. } => return,
    }
    self.last_event_seq = 0;
    self.verified_keys.clear();
    // Another network: its capacity is unknown, measure afresh.
    self.uplink = SendSideEstimator::new(self.config.bwe);
    self.uplink_layers = UplinkLayers::new(self.config.bwe.start_bps);
    self.update_uplink_demand();
    self.downlink_feedback = FeedbackRecorder::new(now);
    self.uplink_feedback_epoch = now;
    self.signal(CallClientMessage::Join {
      token,
      capabilities: self.config.capabilities.clone(),
    });
  }

  /// Over for good (refused, removed, ended, or replaced by another connection of this device):
  /// a lost connection is not to be answered with a rejoin.
  pub fn is_over(&self) -> bool {
    self.is_over
  }

  /// Joined, and not reconnecting.
  pub fn is_in_call(&self) -> bool {
    matches!(self.phase, Phase::Joined { .. })
  }

  /// Between losing the connection and being back in the call.
  pub fn is_reconnecting(&self) -> bool {
    matches!(self.phase, Phase::Reconnecting { .. } | Phase::Rejoining { .. })
  }

  /// How long the SFU keeps this device's slot after a lost connection, as it announced.
  pub fn maybe_reconnect_grace(&self) -> Option<Duration> {
    self.maybe_reconnect_grace
  }

  /// Bandwidth kept for audio when the uplink limits video layers.
  pub fn set_uplink_policy(&mut self, policy: UplinkPolicy) {
    self.uplink_policy = policy;
  }

  /// The participants the app trusts to be in this call (devices). Proposals for others are
  /// rejected.
  pub fn set_trusted_roster(&mut self, devices: impl IntoIterator<Item = DeviceId>) {
    self.maybe_trusted_roster = Some(devices.into_iter().collect());
  }

  /// Signs this device's DAVE key and verifies other members' keys (e2ee.md §8). Without one,
  /// members are not bound to device identities and [`CallEngineEvent::UnauthenticatedKeys`] says so.
  pub fn set_authenticator(&mut self, authenticator: Arc<dyn CallKeyAuthenticator>) {
    self.maybe_authenticator = Some(authenticator);
  }

  pub fn set_muted(&mut self, is_muted: bool) {
    if self.is_muted != is_muted {
      self.is_muted = is_muted;
      self.event(CallEngineEvent::MuteChanged(is_muted));
      self.announce_mute();
    }
  }

  /// Tells the SFU (and through it the others) whether this device is muted, once in the call.
  fn announce_mute(&mut self) {
    if matches!(self.phase, Phase::Joined { .. }) && self.is_muted != self.is_mute_announced {
      self.is_mute_announced = self.is_muted;
      self.signal(CallClientMessage::SetMuted {
        is_muted: self.is_muted,
      });
    }
  }

  /// Takes the call over from the account's other devices: the SFU closes them (#505).
  pub fn move_here(&mut self) {
    self.signal(CallClientMessage::MoveHere);
  }

  pub fn set_deafened(&mut self, is_deafened: bool) {
    if self.is_deafened != is_deafened {
      self.is_deafened = is_deafened;
      self.event(CallEngineEvent::DeafenChanged(is_deafened));
    }
  }

  pub fn device_id(&self) -> DeviceId {
    self.config.device_id
  }

  /// The codec to publish a track in that the driver can encode as any of `candidates`, best
  /// first (#565): the first one this device announced as an encoder and every other device that
  /// receives video decodes ([`CallEngineEvent::DecodableVideoCodecs`]). H.264, which every
  /// device decodes, when none of them is, and before the SFU said (an older SFU never does).
  pub fn video_codec_for(&self, candidates: &[VideoCodec]) -> VideoCodec {
    let Some(decodable) = &self.maybe_decodable else {
      return VideoCodec::H264;
    };
    candidates
      .iter()
      .find(|codec| decodable.contains(codec) && self.config.capabilities.video_encoders.contains(codec))
      .cloned()
      .unwrap_or(VideoCodec::H264)
  }

  /// The most simulcast layers a track may be planned with (#978): what the SFU said it takes,
  /// three from an SFU from before that says nothing. Before the SFU's first answer, the most
  /// there are: publications wait for that answer, and one planned with more layers than the
  /// SFU takes then goes out with its lowest ones only (a 1080p camera's 180p to 720p), which
  /// [`crate::video::within_layer_limit`] avoids for plans made once it is known.
  pub fn simulcast_layer_limit(&self) -> u8 {
    self.maybe_sfu_layer_limit.unwrap_or(MAX_SIMULCAST_LAYERS)
  }

  pub fn is_muted(&self) -> bool {
    self.is_muted
  }

  pub fn is_deafened(&self) -> bool {
    self.is_deafened
  }

  /// Whether media is end-to-end encrypted and can flow.
  pub fn is_secured(&self) -> bool {
    !self.is_compromised && self.maybe_dave.as_ref().is_some_and(DaveParticipant::is_ready)
  }

  /// Code to compare with `other` out of band (DAVE verification code).
  pub fn verification_code(&self, other: DeviceId) -> Option<String> {
    self
      .maybe_dave
      .as_ref()?
      .verification_code(DaveParticipantId::from_device(other))
      .ok()
  }

  /// Group-wide code for the current epoch, to compare out of band.
  pub fn privacy_code(&self) -> Option<String> {
    self.maybe_dave.as_ref()?.maybe_privacy_code().map(str::to_owned)
  }

  pub fn participants(&self) -> impl Iterator<Item = &CallParticipant> {
    self.participants.values()
  }

  /// Handles one message from the signaling stream.
  pub fn handle_signal(&mut self, message: CallServerMessage, now: Instant) -> Result<(), CallEngineError> {
    match message {
      CallServerMessage::Event { seq, event } => {
        if seq != self.last_event_seq + 1 {
          return Err(CallEngineError::SequenceGap {
            last: self.last_event_seq,
            received: seq,
          });
        }
        self.last_event_seq = seq;
        self.handle_event(event, now)
      },
      CallServerMessage::Error { error } => {
        // An error in answer to `join` means the SFU refused it and closes the connection.
        if matches!(self.phase, Phase::Joining | Phase::Rejoining { .. }) {
          self.is_over = true;
        }
        self.event(CallEngineEvent::Error(error));
        Ok(())
      },
      // Answers to the driver's pings (browsers watch their session's path with them); the
      // browser driver takes them before they get here.
      CallServerMessage::Pong { .. } => Ok(()),
      CallServerMessage::Unknown => {
        debug!("ignoring unknown signaling frame");
        Ok(())
      },
    }
  }

  /// Handles one received datagram (decrypts audio for [`CallEngineEvent::AudioFrame`]).
  pub fn handle_datagram(&mut self, bytes: &[u8], now: Instant) {
    if let Err(error) = self.receive_datagram(bytes, now) {
      debug!(%error, "dropping datagram");
    }
  }

  /// Encrypts and sends one 20 ms audio frame. Dropped while muted or before DAVE is ready
  /// (there is no unencrypted fallback, ARCHITECTURE.md §6.2).
  pub fn send_audio(&mut self, frame: &EncodedAudioFrame, now: Instant) {
    let Phase::Joined { audio_stream, .. } = self.phase else {
      return;
    };
    if self.is_muted {
      return;
    }
    self.send_audio_frame(audio_stream, AudioOrigin::Microphone, frame, now);
  }

  /// Asks the SFU for a stream for the shared screen's sound; frames flow after
  /// [`CallEngineEvent::ScreenAudioChanged`] for this device. Not affected by muting the microphone.
  pub fn publish_screen_audio(&mut self) {
    if self.screen_audio == ScreenAudioState::Off {
      self.screen_audio = ScreenAudioState::Requested;
      self.signal(CallClientMessage::PublishScreenAudio);
    }
  }

  /// Screen sound is asked for or being sent (not after the SFU stopped it, e.g. a host's
  /// "mute all").
  pub fn is_sharing_screen_audio(&self) -> bool {
    self.screen_audio != ScreenAudioState::Off
  }

  pub fn unpublish_screen_audio(&mut self) {
    if let ScreenAudioState::Publishing(stream) = self.screen_audio {
      self.signal(CallClientMessage::UnpublishScreenAudio {
        stream_id: MediaStreamId(stream.0),
      });
    }
    // A request still in flight is unpublished when its answer arrives.
    self.screen_audio = ScreenAudioState::Off;
  }

  /// Encrypts and sends one 20 ms frame of the shared screen's sound.
  pub fn send_screen_audio(&mut self, frame: &EncodedAudioFrame, now: Instant) {
    if let ScreenAudioState::Publishing(stream) = self.screen_audio {
      self.send_audio_frame(stream, AudioOrigin::Screen, frame, now);
    }
  }

  /// Starts publishing a video track under the driver's `publication` id, which must be unused in
  /// this call (a counter does); a reused one is logged and ignored.
  /// [`CallEngineEvent::VideoPublished`] brings the stream id, with `publication` in the track's
  /// `maybe_publication_id` ([`OwnPublication`] follows it). Screen shares use one layer (no
  /// simulcast).
  pub fn publish_video(
    &mut self,
    publication: VideoPublicationId,
    kind: VideoTrackKind,
    codec: VideoCodec,
    layers: Vec<VideoLayerInfo>,
  ) {
    self.publish(publication, kind, codec, layers, false);
  }

  /// [`Self::publish_video`] for a track whose encoders follow [`CallEngineEvent::LayerBitrates`]:
  /// a camera's top sent layer is then encoded at less than its bitrate on a limited uplink,
  /// before it stops and the picture drops to the next lower layer (#939). Other kinds are sent
  /// as by [`Self::publish_video`].
  pub fn publish_adaptive_video(
    &mut self,
    publication: VideoPublicationId,
    kind: VideoTrackKind,
    codec: VideoCodec,
    layers: Vec<VideoLayerInfo>,
  ) {
    self.publish(publication, kind, codec, layers, true);
  }

  fn publish(
    &mut self,
    publication: VideoPublicationId,
    kind: VideoTrackKind,
    codec: VideoCodec,
    layers: Vec<VideoLayerInfo>,
    adapts_bitrate: bool,
  ) {
    let is_in_use = self
      .pending_publications
      .iter()
      .any(|pending| pending.id == publication)
      || self.own_stream_of(publication).is_some();
    if is_in_use {
      warn!(?publication, "video publication id used twice; ignored");
      return;
    }
    self.request_publication(PendingPublication {
      id: publication,
      kind,
      codec,
      layers,
      is_withdrawn: false,
      adapts_bitrate,
    });
  }

  /// Stops the own track published under `publication`, whether the SFU confirmed it or not
  /// (#929): a confirmed track is unpublished, and the answer to one still on its way is
  /// unpublished as soon as it arrives, without a [`CallEngineEvent::VideoPublished`]. An unknown
  /// id (stopped already) changes nothing.
  pub fn withdraw_video(&mut self, publication: VideoPublicationId) {
    if let Some(pending) = self
      .pending_publications
      .iter_mut()
      .find(|pending| pending.id == publication)
    {
      pending.is_withdrawn = true;
    } else if let Some(stream) = self.own_stream_of(publication) {
      self.unpublish_video(MediaStreamId(stream.0));
    }
  }

  /// Unpublishes a confirmed own track by its stream id ([`Self::withdraw_video`] also stops one
  /// that isn't confirmed yet).
  pub fn unpublish_video(&mut self, stream_id: MediaStreamId) {
    if self.forget_own_track(StreamId(stream_id.0)) {
      self.signal(CallClientMessage::UnpublishVideo { stream_id });
    }
  }

  /// Receives another device's track up to `max_layer`; `None` stops receiving it.
  pub fn subscribe_video(&mut self, stream_id: MediaStreamId, maybe_max_layer: Option<u8>) {
    let stream = StreamId(stream_id.0);
    match maybe_max_layer {
      Some(max_layer) => self.subscriptions.insert(stream, max_layer),
      None => self.subscriptions.remove(&stream),
    };
    // A new subscription feeds a new decoder: deltas wait for the keyframe the SFU asks for.
    if maybe_max_layer.is_some()
      && let Some(track) = self.remote_video.get_mut(&stream)
    {
      track.wait_for_keyframe();
    }
    self.signal(CallClientMessage::Subscribe {
      stream_id,
      maybe_max_layer,
    });
  }

  /// Changes how sharp a received track may come (its tile was resized): unlike
  /// [`CallEngine::subscribe_video`] the decoder stays, so deltas keep flowing until the SFU
  /// switches layers on a keyframe. Nothing happens for a track not received or already capped
  /// there.
  pub fn set_video_max_layer(&mut self, stream_id: MediaStreamId, max_layer: u8) {
    let Some(current) = self.subscriptions.get_mut(&StreamId(stream_id.0)) else {
      return;
    };
    if *current == max_layer {
      return;
    }
    *current = max_layer;
    self.signal(CallClientMessage::Subscribe {
      stream_id,
      maybe_max_layer: Some(max_layer),
    });
  }

  /// The app's decoder of a received track failed or was replaced: deltas are dropped until the
  /// next keyframe, which is asked for (rate-limited).
  pub fn request_keyframe(&mut self, stream_id: MediaStreamId, now: Instant) {
    let stream = StreamId(stream_id.0);
    let Some(track) = self.remote_video.get_mut(&stream) else {
      return;
    };
    track.wait_for_keyframe();
    self.ask_for_keyframe(stream, now);
  }

  /// Encrypts, splits and sends one encoded frame of an own track. Dropped before DAVE is ready,
  /// like audio, and on layers nobody receives or the uplink can't carry.
  pub fn send_video(
    &mut self,
    stream_id: MediaStreamId,
    frame: &EncodedVideoFrame,
    now: Instant,
  ) -> VideoSendOutcome {
    let stream = StreamId(stream_id.0);
    if self.is_compromised {
      return VideoSendOutcome::NotSecured;
    }
    if !self.own_video.contains_key(&stream) {
      return VideoSendOutcome::NotPublished;
    }
    let is_layer_sent = self
      .sent_layers
      .get(&stream)
      .is_none_or(|layers| layers.contains(&frame.layer.value()));
    if !is_layer_sent {
      return VideoSendOutcome::LayerPaused;
    }
    let Some(dave) = self.maybe_dave.as_mut().filter(|dave| dave.is_ready()) else {
      self.count_dropped_video(stream);
      return VideoSendOutcome::NotSecured;
    };
    let encrypted = match dave.encrypt_frame(MediaKind::Video, frame_codec(&frame.codec), &frame.data) {
      Ok(encrypted) => encrypted.into_owned(),
      Err(error) => {
        warn!(%error, "video frame not encrypted, dropped");
        self.count_dropped_video(stream);
        return VideoSendOutcome::Failed;
      },
    };
    let Some(track) = self.own_video.get_mut(&stream) else {
      return VideoSendOutcome::NotPublished;
    };
    let packets = match track.packetize(
      stream,
      frame.layer,
      frame.timestamp,
      frame.is_keyframe,
      &encrypted,
    ) {
      Ok(packets) => packets,
      Err(error) => {
        warn!(%error, "video frame dropped");
        track.count_dropped();
        return VideoSendOutcome::Failed;
      },
    };
    track.count_sent(frame.layer, &packets);
    for rtp in packets {
      self.send_media(stream, frame.layer, &rtp, now);
    }
    VideoSendOutcome::Sent
  }

  /// What the call's video does (stats for nerds, #938): the counters of every own and received
  /// track, read at `at_ms`. What only the driver knows (encoders, capture, round trip) it adds.
  #[cfg(feature = "call-view")]
  pub fn video_stats(&self, at_ms: u64) -> VideoStatsReport {
    use teamcraft_client_api::video_stats::{
      OfferedLayer, ReceivedVideoStats, SentLayerStats, SentVideoStats, UplinkStats, VideoSize,
      VideoTrackStats,
    };

    let size = |info: &VideoLayerInfo| VideoSize {
      width: info.width,
      height: info.height,
    };
    let mut own: Vec<(&StreamId, &OutgoingVideo)> = self.own_video.iter().collect();
    own.sort_unstable_by_key(|(stream, _)| **stream);
    let sent = own.into_iter().map(|(stream, track)| {
      let infos = self
        .own_tracks
        .get(stream)
        .map(|announced| announced.layers.as_slice())
        .unwrap_or_default();
      let maybe_target = self.sent_layers.get(stream);
      let layers = infos
        .iter()
        .map(|info| {
          let counted = track
            .counters
            .layers
            .iter()
            .find(|counted| counted.layer == info.layer)
            .copied()
            .unwrap_or_default();
          SentLayerStats {
            layer: info.layer,
            size: size(info),
            is_target: maybe_target.is_none_or(|target| target.contains(&info.layer)),
            pictures: counted.pictures,
            bytes: counted.bytes,
          }
        })
        .collect();
      VideoTrackStats::Sent(SentVideoStats {
        stream_id: MediaStreamId(stream.0),
        kind: track.kind.clone(),
        codec: track.codec.clone(),
        maybe_encoder: None,
        maybe_capture: None,
        layers,
        packets: track.counters.packets,
        bytes: track.counters.bytes,
        maybe_frames_captured: None,
        pictures_dropped: track.counters.pictures_dropped,
        maybe_frames_black: None,
      })
    });
    let mut remote: Vec<(&StreamId, &IncomingVideo)> = self.remote_video.iter().collect();
    remote.sort_unstable_by_key(|(stream, _)| **stream);
    let received = remote.into_iter().map(|(stream, track)| {
      let counters = &track.counters;
      VideoTrackStats::Received(ReceivedVideoStats {
        stream_id: MediaStreamId(stream.0),
        kind: track.kind.clone(),
        codec: track.codec.clone(),
        layers: track
          .layers
          .iter()
          .map(|info| OfferedLayer {
            layer: info.layer,
            size: size(info),
          })
          .collect(),
        maybe_layer: track.layer().map(|layer| layer.value()),
        maybe_max_layer: self.subscriptions.get(stream).copied(),
        packets: counters.packets,
        bytes: counters.bytes,
        packets_lost: counters.packets_lost,
        frames: counters.frames,
        frames_discarded: counters.frames_discarded,
        // Milliseconds don't need more than f32.
        maybe_jitter_ms: counters.jitter_ms().map(|jitter| jitter as f32),
      })
    });
    VideoStatsReport {
      at_ms,
      maybe_rtt_ms: None,
      maybe_uplink: Some(UplinkStats {
        estimate_bps: self.uplink.estimate_bps(),
        loss_percent: loss_percent(self.uplink.loss_ratio()),
      }),
      tracks: sent.chain(received).collect(),
    }
  }

  /// Periodic work (every ~100 ms): reports arrival times of received datagrams to the SFU.
  pub fn on_timer(&mut self, _now: Instant) {
    if let Some(feedback) = self.downlink_feedback.take_feedback() {
      self.signal(CallClientMessage::TransportFeedback { feedback });
    }
  }

  /// The next thing to do, in order.
  pub fn poll_output(&mut self) -> Option<EngineOutput> {
    self.outputs.pop_front()
  }

  fn handle_event(&mut self, event: CallEvent, now: Instant) -> Result<(), CallEngineError> {
    match event {
      CallEvent::Joined {
        call_id,
        participant,
        participants,
        video_tracks,
        screen_audio_tracks,
        maybe_reconnect_grace_ms,
        maybe_max_simulcast_layers,
      } => {
        self.maybe_reconnect_grace = maybe_reconnect_grace_ms.map(Duration::from_millis);
        // An SFU from before #978 says nothing and takes three; more than this version plans
        // would be no use.
        let limit = maybe_max_simulcast_layers.unwrap_or(LEGACY_SIMULCAST_LAYERS);
        self.maybe_sfu_layer_limit = Some(limit.clamp(1, MAX_SIMULCAST_LAYERS));
        if let Phase::Rejoining { call_id: previous } = self.phase {
          let state = RejoinedState {
            participants,
            video_tracks,
            screen_audio_tracks,
          };
          return self.on_rejoined(previous, call_id, participant, state);
        }
        self.on_joined(call_id, participant, participants)?;
        for track in video_tracks {
          self.add_video_track(track);
        }
        for track in screen_audio_tracks {
          self.on_screen_audio_published(track);
        }
        Ok(())
      },
      CallEvent::ParticipantJoined { participant } => {
        self.add_participant(participant);
        self.event(CallEngineEvent::ParticipantJoined(participant));
        Ok(())
      },
      CallEvent::ParticipantLeft { device_id } => {
        self.remove_participant(device_id);
        Ok(())
      },
      CallEvent::ParticipantReconnecting { device_id } => {
        self.event(CallEngineEvent::ParticipantReconnecting(device_id));
        Ok(())
      },
      CallEvent::ParticipantReconnected { device_id } => {
        self.event(CallEngineEvent::ParticipantReconnected(device_id));
        Ok(())
      },
      CallEvent::ParticipantMuted { device_id, is_muted } => {
        self.event(CallEngineEvent::ParticipantMuted { device_id, is_muted });
        Ok(())
      },
      CallEvent::ActiveSpeakers { device_ids } => {
        self.event(CallEngineEvent::ActiveSpeakers(device_ids));
        Ok(())
      },
      CallEvent::DaveExternalSender { package } => self.on_external_sender(package.as_slice()),
      CallEvent::DaveProposals {
        epoch,
        proposals,
        expected_participants,
        key_bindings,
      } => {
        self.verify_bindings(&key_bindings)?;
        self.on_proposals(epoch, proposals.as_slice(), &expected_participants)
      },
      CallEvent::DaveAnnounceCommit { commit, .. } => {
        self.dave()?.process_commit(commit.as_slice())?;
        self.verify_members()?;
        self.report_secured();
        Ok(())
      },
      CallEvent::DaveWelcome {
        welcome,
        key_bindings,
      } => {
        self.verify_bindings(&key_bindings)?;
        self.dave()?.process_welcome(welcome.as_slice())?;
        self.verify_members()?;
        self.report_secured();
        Ok(())
      },
      CallEvent::TransportFeedback { feedback } => {
        self.on_uplink_feedback(&feedback, now);
        Ok(())
      },
      CallEvent::VideoPublished { track } => {
        self.add_video_track(track);
        Ok(())
      },
      CallEvent::VideoUnpublished { stream_id } => {
        let stream = StreamId(stream_id.0);
        self.remote_video.remove(&stream);
        self.subscriptions.remove(&stream);
        self.forget_own_track(stream);
        self.event(CallEngineEvent::VideoUnpublished(stream_id));
        Ok(())
      },
      CallEvent::ScreenAudioPublished { track } => {
        self.on_screen_audio_published(track);
        Ok(())
      },
      CallEvent::ScreenAudioUnpublished { stream_id } => {
        self.on_screen_audio_unpublished(stream_id);
        Ok(())
      },
      CallEvent::KeyframeRequested { stream_id, layer } => {
        self.event(CallEngineEvent::KeyframeRequested { stream_id, layer });
        Ok(())
      },
      CallEvent::LayersNeeded { stream_id, layers } => {
        let stream = StreamId(stream_id.0);
        if self.own_tracks.contains_key(&stream) {
          self.sfu_layers.insert(stream, layers);
          // The SFU asks for keyframes itself where receivers switch.
          self.announce_layers(false);
        } else {
          self.event(CallEngineEvent::LayersNeeded { stream_id, layers });
        }
        Ok(())
      },
      CallEvent::MutedByHost { by_user_id } => {
        // The host can ask; the user can unmute again afterwards. The reason comes first.
        self.event(CallEngineEvent::MutedByHost { by_user_id });
        self.set_muted(true);
        Ok(())
      },
      CallEvent::RemovedByHost { by_user_id } => {
        self.is_over = true;
        self.event(CallEngineEvent::RemovedByHost { by_user_id });
        Ok(())
      },
      CallEvent::EndedByHost { by_user_id } => {
        self.is_over = true;
        self.event(CallEngineEvent::EndedByHost { by_user_id });
        Ok(())
      },
      CallEvent::ConnectionReplaced => {
        self.is_over = true;
        self.event(CallEngineEvent::ConnectionReplaced);
        Ok(())
      },
      CallEvent::DecodableVideoCodecs { codecs } => {
        if self.maybe_decodable.as_ref() != Some(&codecs) {
          self.maybe_decodable = Some(codecs.clone());
          self.event(CallEngineEvent::DecodableVideoCodecs(codecs));
        }
        Ok(())
      },
      CallEvent::Unknown => Ok(()),
    }
  }

  fn on_joined(
    &mut self,
    call_id: CallId,
    participant: CallParticipant,
    participants: Vec<CallParticipant>,
  ) -> Result<(), CallEngineError> {
    if self.phase != Phase::Joining {
      return Err(CallEngineError::Protocol("unexpected joined"));
    }
    if participant.device_id != self.config.device_id {
      return Err(CallEngineError::Protocol("joined as another device"));
    }
    self.phase = Phase::Joined {
      call_id,
      audio_stream: StreamId(participant.audio_stream_id.0),
    };
    // Muted from the start: the others see it from the first moment.
    self.announce_mute();
    self.maybe_dave = Some(DaveParticipant::new(
      DaveParticipantId::from_device(self.config.device_id),
      DaveCallId::from_call(call_id),
    )?);
    for other in &participants {
      self.add_participant(*other);
    }
    self.event(CallEngineEvent::Joined {
      call_id,
      participants,
    });
    // Publications asked for while joining go out now that the SFU said how many layers it takes.
    for pending in std::mem::take(&mut self.pending_publications) {
      if !pending.is_withdrawn {
        self.request_publication(pending);
      }
    }
    Ok(())
  }

  /// Back in the call after a rejoin: everything that changed while away is reconciled with the
  /// SFU's view, and own tracks and subscriptions are renewed where the SFU no longer has them.
  fn on_rejoined(
    &mut self,
    previous: CallId,
    call_id: CallId,
    participant: CallParticipant,
    state: RejoinedState,
  ) -> Result<(), CallEngineError> {
    if call_id != previous {
      return Err(CallEngineError::Protocol("rejoined another call"));
    }
    if participant.device_id != self.config.device_id {
      return Err(CallEngineError::Protocol("joined as another device"));
    }
    self.phase = Phase::Joined {
      call_id,
      audio_stream: StreamId(participant.audio_stream_id.0),
    };
    // The slot is the SFU's last state of this device, or a fresh unmuted one after an SFU
    // restart: say the current state whenever either could differ from it.
    if self.is_muted || self.is_mute_announced {
      self.is_mute_announced = !self.is_muted;
      self.announce_mute();
    }
    self.maybe_dave = Some(DaveParticipant::new(
      DaveParticipantId::from_device(self.config.device_id),
      DaveCallId::from_call(call_id),
    )?);
    self.reconcile_participants(state.participants);
    self.reconcile_remote_video(&state.video_tracks);
    self.reconcile_screen_audio(&state.screen_audio_tracks);
    // What the SFU needed and what the uplink carried belong to the old connection (#921): the
    // tracks the SFU kept send every layer the new uplink carries until the SFU says which it
    // needs (it does so right after `joined`), and the SFU hears what the uplink carries.
    self.sfu_layers.clear();
    self.sent_layers.clear();
    self.uplink_announced.clear();
    self.reconcile_own_video(&state.video_tracks);
    // No keyframes yet: nothing goes out before the call is secured, which asks for them.
    self.announce_layers(false);
    self.announce_uplink_layers();
    // Subscriptions to tracks that are still there; the SFU may have kept them, which is fine.
    let mut subscriptions: Vec<(StreamId, u8)> = self.subscriptions.iter().map(|(s, l)| (*s, *l)).collect();
    subscriptions.sort_unstable();
    for (stream, max_layer) in subscriptions {
      self.signal(CallClientMessage::Subscribe {
        stream_id: MediaStreamId(stream.0),
        maybe_max_layer: Some(max_layer),
      });
    }
    self.event(CallEngineEvent::Reconnected);
    Ok(())
  }

  fn reconcile_participants(&mut self, participants: Vec<CallParticipant>) {
    let current: BTreeMap<DeviceId, CallParticipant> = participants
      .into_iter()
      .map(|participant| (participant.device_id, participant))
      .collect();
    let known: Vec<DeviceId> = self.participants.keys().copied().collect();
    for device_id in known {
      // Gone, or back with another audio stream (it rejoined without a slot): left, then joined.
      if current.get(&device_id) != self.participants.get(&device_id) {
        self.remove_participant(device_id);
      }
    }
    for participant in current.into_values() {
      if !self.participants.contains_key(&participant.device_id) {
        self.add_participant(participant);
        self.event(CallEngineEvent::ParticipantJoined(participant));
      }
    }
  }

  fn reconcile_remote_video(&mut self, tracks: &[VideoTrack]) {
    let own = self.config.device_id;
    let current: BTreeMap<StreamId, &VideoTrack> = tracks
      .iter()
      .filter(|track| track.device_id != own)
      .map(|track| (StreamId(track.stream_id.0), track))
      .collect();
    let gone: Vec<StreamId> = self
      .remote_video
      .keys()
      .filter(|stream| !current.contains_key(stream))
      .copied()
      .collect();
    for stream in gone {
      self.remote_video.remove(&stream);
      self.subscriptions.remove(&stream);
      self.event(CallEngineEvent::VideoUnpublished(MediaStreamId(stream.0)));
    }
    for (stream, track) in current {
      if !self.remote_video.contains_key(&stream) {
        self.add_video_track(track.clone());
      }
    }
  }

  fn reconcile_screen_audio(&mut self, tracks: &[ScreenAudioTrack]) {
    let own = self.config.device_id;
    let others: BTreeMap<StreamId, ScreenAudioTrack> = tracks
      .iter()
      .filter(|track| track.device_id != own)
      .map(|track| (StreamId(track.stream_id.0), *track))
      .collect();
    let gone: Vec<(StreamId, DeviceId)> = self
      .audio_streams
      .iter()
      .filter(|(stream, (_, origin))| *origin == AudioOrigin::Screen && !others.contains_key(stream))
      .map(|(stream, (device_id, _))| (*stream, *device_id))
      .collect();
    for (stream, device_id) in gone {
      self.audio_streams.remove(&stream);
      self.event(CallEngineEvent::ScreenAudioChanged {
        device_id,
        is_shared: false,
      });
    }
    for (stream, track) in others {
      if !self.audio_streams.contains_key(&stream) {
        self.on_screen_audio_published(track);
      }
    }
    let maybe_own = tracks
      .iter()
      .find(|track| track.device_id == own)
      .map(|track| StreamId(track.stream_id.0));
    match (self.screen_audio, maybe_own) {
      (ScreenAudioState::Publishing(_), Some(stream)) => {
        self.screen_audio = ScreenAudioState::Publishing(stream)
      },
      (ScreenAudioState::Requested, Some(stream)) => {
        self.screen_audio = ScreenAudioState::Publishing(stream);
        self.event(CallEngineEvent::ScreenAudioChanged {
          device_id: own,
          is_shared: true,
        });
      },
      // Stopped while away: the SFU still forwards it.
      (ScreenAudioState::Off, Some(stream)) => self.signal(CallClientMessage::UnpublishScreenAudio {
        stream_id: MediaStreamId(stream.0),
      }),
      // The SFU dropped it with the slot (or never got the request): ask again.
      (ScreenAudioState::Publishing(_) | ScreenAudioState::Requested, None) => {
        self.screen_audio = ScreenAudioState::Requested;
        self.signal(CallClientMessage::PublishScreenAudio);
      },
      (ScreenAudioState::Off, None) => {},
    }
  }

  fn reconcile_own_video(&mut self, tracks: &[VideoTrack]) {
    let own = self.config.device_id;
    let mut announced: Vec<&VideoTrack> = tracks.iter().filter(|track| track.device_id == own).collect();
    // Stream ids grow with every publication: in this order an SFU that echoes no publication ids
    // answered the requests (`take_answered_publication`).
    announced.sort_by_key(|track| track.stream_id);
    for track in &announced {
      // Kept tracks stay. Others answer a request whose answer got lost with the connection, or
      // have no sender any more (withdrawn or unpublished while away) and are unpublished.
      self.on_own_track_published((*track).clone());
    }
    let unanswered = std::mem::take(&mut self.pending_publications);
    // Tracks the SFU dropped with the slot: asked for again under their id; the driver learns
    // the new stream id.
    let mut lost: Vec<(VideoPublicationId, VideoTrack)> = self
      .own_tracks
      .values()
      .filter(|track| !announced.iter().any(|kept| kept.stream_id == track.stream_id))
      // Always set for own tracks (`add_own_track`).
      .filter_map(|track| Some((track.maybe_publication_id?, track.clone())))
      .collect();
    lost.sort_by_key(|(id, _)| *id);
    for (id, track) in lost {
      let adapts_bitrate = self.adaptive_tracks.contains(&StreamId(track.stream_id.0));
      self.forget_own_track(StreamId(track.stream_id.0));
      self.event(CallEngineEvent::VideoUnpublished(track.stream_id));
      self.request_publication(PendingPublication {
        id,
        kind: track.kind,
        codec: track.codec,
        layers: track.layers,
        is_withdrawn: false,
        adapts_bitrate,
      });
    }
    // Requests the SFU never answered (lost with the connection, or dropped with the slot): asked
    // for again, unless the track was stopped meanwhile.
    for pending in unanswered {
      if !pending.is_withdrawn {
        self.request_publication(pending);
      }
    }
  }

  fn remove_participant(&mut self, device_id: DeviceId) {
    self.participants.remove(&device_id);
    self.audio_streams.retain(|_, (owner, _)| *owner != device_id);
    let streams: Vec<StreamId> = self
      .remote_video
      .iter()
      .filter(|(_, track)| track.sender == device_id)
      .map(|(stream, _)| *stream)
      .collect();
    for stream in streams {
      self.remote_video.remove(&stream);
      self.subscriptions.remove(&stream);
    }
    self.event(CallEngineEvent::ParticipantLeft(device_id));
  }

  /// Drops an own track's state; returns whether it was published.
  fn forget_own_track(&mut self, stream: StreamId) -> bool {
    let was_published = self.own_video.remove(&stream).is_some();
    self.own_tracks.remove(&stream);
    self.sfu_layers.remove(&stream);
    self.sent_layers.remove(&stream);
    self.uplink_announced.remove(&stream);
    self.uplink_layers.remove(stream);
    self.adaptive_tracks.remove(&stream);
    self.bitrates_announced.remove(&stream);
    self.update_uplink_demand();
    was_published
  }

  /// New uplink feedback: the estimate decides which own video layers fit next to the audio, the
  /// loss how much forward error correction the audio carries.
  fn on_uplink_feedback(&mut self, feedback: &TransportFeedback, now: Instant) {
    let bps = self.uplink.on_feedback(feedback, now, self.uplink_feedback_epoch);
    let is_sending_screen_audio = self.screen_audio != ScreenAudioState::Off;
    let tracks = own_track_views(&self.own_tracks, &self.adaptive_tracks);
    let has_changed =
      self
        .uplink_layers
        .on_estimate(bps, &tracks, is_sending_screen_audio, &self.uplink_policy, now);
    if has_changed {
      self.announce_layers(true);
      self.announce_uplink_layers();
    }
    let loss_percent = loss_percent(self.uplink.loss_ratio());
    self.event(CallEngineEvent::UplinkEstimate { bps, loss_percent });
  }

  /// Tells the driver which layers of each own track to encode: those the SFU needs and the uplink
  /// carries. Layers the uplink takes back get a keyframe, so receivers can decode them at once.
  fn announce_layers(&mut self, with_keyframes: bool) {
    let mut streams: Vec<StreamId> = self.own_tracks.keys().copied().collect();
    streams.sort_unstable();
    for stream in streams {
      let Some(track) = self
        .own_tracks
        .get(&stream)
        .filter(|track| !track.layers.is_empty())
      else {
        continue;
      };
      let wanted: Vec<u8> = match self.sfu_layers.get(&stream) {
        Some(layers) => layers.clone(),
        None => track.layers.iter().map(|info| info.layer).collect(),
      };
      let mut layers: Vec<u8> = wanted
        .iter()
        .copied()
        .filter(|layer| self.uplink_layers.allows(stream, *layer))
        .collect();
      // Receivers of a layer the uplink stopped move to the highest one it carries (#921): sent
      // at once, it doesn't wait for the SFU to ask for it while their picture stands still (#939).
      let maybe_carried_top = track
        .layers
        .iter()
        .map(|info| info.layer)
        .filter(|layer| self.uplink_layers.allows(stream, *layer))
        .max();
      if let Some(top) = maybe_carried_top
        && layers.len() < wanted.len()
        && !layers.contains(&top)
      {
        layers.push(top);
        layers.sort_unstable();
      }
      let previous = self.sent_layers.get(&stream).cloned().unwrap_or_default();
      if layers == previous {
        continue;
      }
      self.sent_layers.insert(stream, layers.clone());
      let stream_id = MediaStreamId(stream.0);
      let resumed: Vec<u8> = layers
        .iter()
        .copied()
        .filter(|layer| !previous.contains(layer))
        .collect();
      self.event(CallEngineEvent::LayersNeeded { stream_id, layers });
      if with_keyframes {
        for layer in resumed {
          self.event(CallEngineEvent::KeyframeRequested { stream_id, layer });
        }
      }
    }
    self.announce_bitrates();
  }

  /// Tells the driver the bitrate per layer of each adaptive own track, where that changed.
  fn announce_bitrates(&mut self) {
    let mut streams: Vec<StreamId> = self.adaptive_tracks.iter().copied().collect();
    streams.sort_unstable();
    for stream in streams {
      let Some(track) = self.own_tracks.get(&stream) else {
        continue;
      };
      let bitrates: Vec<(u8, u32)> = track
        .layers
        .iter()
        .map(|info| (info.layer, self.uplink_layers.bitrate_bps(stream, info)))
        .collect();
      if self.bitrates_announced.get(&stream) == Some(&bitrates) {
        continue;
      }
      self.bitrates_announced.insert(stream, bitrates.clone());
      self.event(CallEngineEvent::LayerBitrates {
        stream_id: MediaStreamId(stream.0),
        bitrates,
      });
    }
  }

  /// Tells the SFU which layers of each own track the uplink carries, where that changed: it then
  /// keeps receivers off the paused ones, which would freeze their picture (#921). Only while in
  /// the call; a rejoin tells it about every track.
  fn announce_uplink_layers(&mut self) {
    if !self.is_in_call() {
      return;
    }
    let mut streams: Vec<StreamId> = self.own_tracks.keys().copied().collect();
    streams.sort_unstable();
    for stream in streams {
      let Some(track) = self
        .own_tracks
        .get(&stream)
        .filter(|track| !track.layers.is_empty())
      else {
        continue;
      };
      let layers: Vec<u8> = track
        .layers
        .iter()
        .map(|info| info.layer)
        .filter(|layer| self.uplink_layers.allows(stream, *layer))
        .collect();
      if self.uplink_announced.get(&stream) == Some(&layers) {
        continue;
      }
      self.uplink_announced.insert(stream, layers.clone());
      self.signal(CallClientMessage::UplinkLayers {
        stream_id: MediaStreamId(stream.0),
        layers,
      });
    }
  }

  /// Lets the uplink estimate grow to what all own layers need, even while some are held back.
  fn update_uplink_demand(&mut self) {
    let is_sending_screen_audio = self.screen_audio != ScreenAudioState::Off;
    let tracks = own_track_views(&self.own_tracks, &self.adaptive_tracks);
    let maybe_demand =
      (!tracks.is_empty()).then(|| probe_demand_bps(&tracks, is_sending_screen_audio, &self.uplink_policy));
    self.uplink.set_demand_bps(maybe_demand);
  }

  fn on_external_sender(&mut self, package: &[u8]) -> Result<(), CallEngineError> {
    let dave = self.dave()?;
    dave.set_external_sender(package)?;
    let key_package = dave.create_key_package()?;
    let dave_public_key = dave.signature_public_key().to_vec();
    let maybe_key_binding = match (&self.maybe_authenticator, self.call_id()) {
      (Some(authenticator), Some(call_id)) => {
        Some(Base64Bytes::new(authenticator.sign(call_id, &dave_public_key)?))
      },
      _ => None,
    };
    self.signal(CallClientMessage::DaveKeyPackage {
      key_package: Base64Bytes::new(key_package),
      maybe_key_binding,
    });
    Ok(())
  }

  /// Verifies relayed call key bindings and remembers the bound keys.
  fn verify_bindings(&mut self, bindings: &[DaveKeyBinding]) -> Result<(), CallEngineError> {
    let (Some(authenticator), Some(call_id)) = (self.maybe_authenticator.clone(), self.call_id()) else {
      return Ok(());
    };
    for DaveKeyBinding { device_id, binding } in bindings {
      if *device_id == self.config.device_id {
        continue;
      }
      let key = authenticator.verify(call_id, *device_id, binding.as_slice())?;
      self.verified_keys.insert(*device_id, key);
    }
    Ok(())
  }

  /// Every other member's leaf key must be the key from its verified binding; otherwise media
  /// stops for this call (e2ee.md §8).
  fn verify_members(&mut self) -> Result<(), CallEngineError> {
    if self.maybe_authenticator.is_none() {
      self.report_unauthenticated_keys();
      return Ok(());
    }
    let own = DaveParticipantId::from_device(self.config.device_id);
    let members = self.dave()?.member_signature_keys();
    for (member, leaf_key) in members.into_iter().filter(|(member, _)| *member != own) {
      let device = member
        .to_device()
        .map_err(|_| CallEngineError::Protocol("member id"))?;
      if self.verified_keys.get(&device) != Some(&leaf_key) {
        self.is_compromised = true;
        return Err(CallEngineError::UnverifiedMember(device));
      }
    }
    Ok(())
  }

  /// Members the pending commit would add must have verified keys; otherwise don't commit.
  fn verify_pending_additions(&self) -> Result<(), CallEngineError> {
    if self.maybe_authenticator.is_none() {
      return Ok(());
    }
    let Some(dave) = &self.maybe_dave else {
      return Ok(());
    };
    for (member, leaf_key) in dave.pending_additions() {
      let device = member
        .to_device()
        .map_err(|_| CallEngineError::Protocol("member id"))?;
      if self.verified_keys.get(&device) != Some(&leaf_key) {
        return Err(CallEngineError::UnverifiedMember(device));
      }
    }
    Ok(())
  }

  fn report_unauthenticated_keys(&mut self) {
    if !self.has_reported_unauthenticated_keys {
      self.has_reported_unauthenticated_keys = true;
      self.event(CallEngineEvent::UnauthenticatedKeys);
    }
  }

  fn call_id(&self) -> Option<CallId> {
    match self.phase {
      Phase::Joined { call_id, .. } | Phase::Reconnecting { call_id } | Phase::Rejoining { call_id } => {
        Some(call_id)
      },
      Phase::Idle | Phase::Joining => None,
    }
  }

  fn on_proposals(
    &mut self,
    epoch: u64,
    proposals: &[u8],
    sfu_expected: &[DeviceId],
  ) -> Result<(), CallEngineError> {
    let expected: Vec<DaveParticipantId> = match &self.maybe_trusted_roster {
      Some(roster) => roster
        .iter()
        .chain(std::iter::once(&self.config.device_id))
        .map(|device| DaveParticipantId::from_device(*device))
        .collect(),
      None => {
        self.event(CallEngineEvent::UntrustedRoster);
        sfu_expected
          .iter()
          .map(|device| DaveParticipantId::from_device(*device))
          .collect()
      },
    };
    if let Some(commit) = self.dave()?.process_proposals(proposals, &expected)? {
      // The commit stays unsent if it would add a device with an unverified key.
      self.verify_pending_additions()?;
      self.signal(CallClientMessage::DaveCommit {
        epoch,
        commit: Base64Bytes::new(commit.commit),
        maybe_welcome: commit.maybe_welcome.map(Base64Bytes::new),
      });
    }
    Ok(())
  }

  fn receive_datagram(&mut self, bytes: &[u8], now: Instant) -> Result<(), CallEngineError> {
    let datagram = MediaDatagram::parse(bytes).map_err(|_| CallEngineError::Protocol("bad datagram"))?;
    if let Some(seq) = datagram.header.maybe_transport_seq {
      self.downlink_feedback.on_arrival(seq, now);
    }
    if self.is_compromised {
      return Ok(());
    }
    if self.remote_video.contains_key(&datagram.header.stream_id) {
      return self.receive_video(
        datagram.header.stream_id,
        datagram.header.layer,
        datagram.rtp,
        now,
      );
    }
    let Some(&(sender, origin)) = self.audio_streams.get(&datagram.header.stream_id) else {
      return Ok(());
    };
    if self.is_deafened || self.is_own_other_device(sender, origin) {
      return Ok(());
    }
    let packet = RtpPacket::parse(datagram.rtp).map_err(|_| CallEngineError::Protocol("bad RTP"))?;
    let opus = self.dave()?.decrypt_frame(
      DaveParticipantId::from_device(sender),
      MediaKind::Audio,
      packet.payload,
    )?;
    self.event(CallEngineEvent::AudioFrame {
      sender,
      origin,
      sequence_number: packet.header.sequence_number,
      opus,
    });
    Ok(())
  }

  /// The microphone of another device of this account: the user is in the room with it already,
  /// so playing it here would only echo the room back (#505).
  fn is_own_other_device(&self, sender: DeviceId, origin: AudioOrigin) -> bool {
    origin == AudioOrigin::Microphone
      && self
        .participants
        .get(&sender)
        .is_some_and(|participant| participant.user_id == self.config.user_id)
  }

  /// Reassembles and decrypts a received track's frames. After a lost packet or a frame that
  /// didn't decrypt, its deltas are dropped until the next keyframe, which is asked for.
  fn receive_video(
    &mut self,
    stream: StreamId,
    layer: SimulcastLayer,
    rtp: &[u8],
    now: Instant,
  ) -> Result<(), CallEngineError> {
    let packet = RtpPacket::parse(rtp).map_err(|_| CallEngineError::Protocol("bad RTP"))?;
    let Some(track) = self.remote_video.get_mut(&stream) else {
      return Ok(());
    };
    track.count_arrival(
      rtp.len(),
      packet.header.timestamp,
      now.saturating_duration_since(self.started_at),
    );
    let maybe_complete = track
      .push(layer, &packet)
      .map_err(|_| CallEngineError::Protocol("bad video fragment"))?;
    let mut needs_keyframe = track.take_loss();
    let maybe_frame = match maybe_complete {
      Some(complete) if track.accepts(complete.is_keyframe) => {
        // A keyframe makes whatever went missing before it irrelevant.
        needs_keyframe &= !complete.is_keyframe;
        Some(complete)
      },
      // A delta whose reference went missing: the decoder can't use it.
      Some(_) => {
        track.count_frame(false);
        needs_keyframe = true;
        None
      },
      None => None,
    };
    let (sender, codec) = (track.sender, track.codec.clone());
    if needs_keyframe {
      self.ask_for_keyframe(stream, now);
    }
    let Some(complete) = maybe_frame else {
      return Ok(());
    };
    let decrypted = self.dave()?.decrypt_frame(
      DaveParticipantId::from_device(sender),
      MediaKind::Video,
      &complete.data,
    );
    let is_decrypted = decrypted.is_ok();
    if let Some(track) = self.remote_video.get_mut(&stream) {
      track.count_frame(is_decrypted);
      if !is_decrypted {
        track.wait_for_keyframe();
      }
    }
    let data = match decrypted {
      // DAVE sends AV1 without its temporal delimiters and last OBU sizes (#565).
      Ok(data) if codec == VideoCodec::Av1 => crate::video::av1_temporal_unit(&data),
      Ok(data) => data,
      Err(error) => {
        self.ask_for_keyframe(stream, now);
        return Err(error.into());
      },
    };
    self.event(CallEngineEvent::VideoFrame(ReceivedVideoFrame {
      sender,
      stream_id: MediaStreamId(stream.0),
      codec,
      layer,
      timestamp: packet.header.timestamp,
      is_keyframe: complete.is_keyframe,
      data,
    }));
    Ok(())
  }

  /// Counts an own picture that was meant to go out but didn't (stats).
  fn count_dropped_video(&mut self, stream: StreamId) {
    if let Some(track) = self.own_video.get_mut(&stream) {
      track.count_dropped();
    }
  }

  /// Asks the SFU for a keyframe of a received track, at most once per
  /// [`KEYFRAME_REQUEST_INTERVAL`].
  fn ask_for_keyframe(&mut self, stream: StreamId, now: Instant) {
    let Some(track) = self.remote_video.get_mut(&stream) else {
      return;
    };
    if !track.is_keyframe_request_due(now, KEYFRAME_REQUEST_INTERVAL) {
      return;
    }
    debug!(stream_id = stream.0, "asking for a keyframe");
    self.signal(CallClientMessage::RequestKeyframe {
      stream_id: MediaStreamId(stream.0),
    });
  }

  fn add_video_track(&mut self, track: VideoTrack) {
    if track.device_id == self.config.device_id {
      self.on_own_track_published(track);
      return;
    }
    self
      .remote_video
      .insert(StreamId(track.stream_id.0), IncomingVideo::new(&track));
    self.event(CallEngineEvent::VideoPublished(track));
  }

  /// The SFU announced a track of this device. One that answers a publication still wanted is
  /// sent from now on; one that answers a withdrawn publication, or none at all, has no sender and
  /// is unpublished at once, rather than staying a black tile with the camera shown on for the
  /// others (#929). A track known already (kept over a rejoin) stays as it is.
  fn on_own_track_published(&mut self, track: VideoTrack) {
    if self.own_tracks.contains_key(&StreamId(track.stream_id.0)) {
      return;
    }
    match self.take_answered_publication(&track) {
      Some(pending) if !pending.is_withdrawn => {
        if pending.adapts_bitrate {
          let stream = StreamId(track.stream_id.0);
          self.adaptive_tracks.insert(stream);
          // Drivers start with the layers' own bitrates.
          let own: Vec<(u8, u32)> = track
            .layers
            .iter()
            .map(|info| (info.layer, info.max_bitrate_bps))
            .collect();
          self.bitrates_announced.insert(stream, own);
        }
        let asked: Vec<u8> = pending.layers.iter().map(|info| info.layer).collect();
        self.add_own_track(pending.id, track, asked);
      },
      maybe_withdrawn => {
        debug!(
          stream_id = track.stream_id.0,
          kind = ?track.kind,
          is_withdrawn = maybe_withdrawn.is_some(),
          "own video track without a sender; unpublishing it"
        );
        self.signal(CallClientMessage::UnpublishVideo {
          stream_id: track.stream_id,
        });
      },
    }
  }

  /// Takes the pending publication an announced own track answers: the one whose id the SFU
  /// echoed (of the same kind), or, from an SFU that echoes none, the oldest one of its kind, as
  /// such an SFU answers requests in the order they came.
  fn take_answered_publication(&mut self, track: &VideoTrack) -> Option<PendingPublication> {
    let position = self.pending_publications.iter().position(|pending| {
      pending.kind == track.kind
        && track
          .maybe_publication_id
          .is_none_or(|publication| publication == pending.id)
    })?;
    self.pending_publications.remove(position)
  }

  /// An own track the SFU confirmed for the driver's `publication`: frames go out on it. `asked`
  /// are the layers the driver encodes; those the SFU didn't take (#978: more than it takes)
  /// are paused at once.
  fn add_own_track(&mut self, publication: VideoPublicationId, mut track: VideoTrack, asked: Vec<u8>) {
    let stream = StreamId(track.stream_id.0);
    // Drivers find their track by it, also with an SFU that doesn't echo it.
    track.maybe_publication_id = Some(publication);
    self.own_video.insert(
      stream,
      OutgoingVideo::new(track.kind.clone(), track.codec.clone()),
    );
    // Drivers start with every layer, and the SFU takes every layer as carried. A track without
    // layer list isn't limited.
    let own_layers: Vec<u8> = track.layers.iter().map(|info| info.layer).collect();
    if !own_layers.is_empty() {
      // Layers the driver encodes beyond the track's are announced as paused below.
      let encoded = if asked.len() > own_layers.len() {
        asked
      } else {
        own_layers.clone()
      };
      self.sent_layers.insert(stream, encoded);
      self.uplink_announced.insert(stream, own_layers.clone());
    }
    self.own_tracks.insert(stream, track.clone());
    self.update_uplink_demand();
    self.measure_uplink_afresh();
    let stream_id = track.stream_id;
    self.event(CallEngineEvent::VideoPublished(track));
    // The driver dropped the pictures encoded before this confirmation, the first keyframe
    // among them: receivers would see nothing until the next one (issue #683).
    self.request_own_keyframes(stream_id, &own_layers);
  }

  /// A new own track sends far more than the call did so far: the uplink is measured afresh, as
  /// at the start of the call, and nothing is held back until the estimator cuts the rate with the
  /// track sending. A call that had sent only sound left an estimate behind that one lossy moment
  /// had cut below every camera layer: the camera started with all of them held back, nothing was
  /// sent, the others saw black and the camera was turned off ("none could be encoded"); only a
  /// rejoin, with a fresh estimate, helped (#935).
  fn measure_uplink_afresh(&mut self) {
    self.uplink.measure_afresh();
    self.uplink_layers.measure_afresh(self.uplink.estimate_bps());
    // Tracks that were held back come back as well, with keyframes.
    self.announce_layers(true);
    self.announce_uplink_layers();
  }

  /// Asks the SFU for a publication and waits for its answer. Only while in the call: a
  /// publication asked for before the SFU's `joined` goes out with it, within the layers the SFU
  /// takes (#978), and one asked for while away goes out on the rejoin. Layers beyond what the
  /// SFU takes are left out, the top ones: their numbers stay those the driver encodes.
  fn request_publication(&mut self, pending: PendingPublication) {
    if self.is_in_call() {
      let limit = usize::from(self.simulcast_layer_limit());
      if pending.layers.len() > limit {
        debug!(
          layers = pending.layers.len(),
          limit, "the SFU takes fewer layers than the track has; the top ones are left out"
        );
      }
      self.signal(CallClientMessage::PublishVideo {
        kind: pending.kind.clone(),
        codec: pending.codec.clone(),
        layers: pending.layers.iter().take(limit).copied().collect(),
        maybe_publication_id: Some(pending.id),
      });
    }
    self.pending_publications.push_back(pending);
  }

  /// The stream of the confirmed own track published under `publication`.
  fn own_stream_of(&self, publication: VideoPublicationId) -> Option<StreamId> {
    self
      .own_tracks
      .iter()
      .find(|(_, track)| track.maybe_publication_id == Some(publication))
      .map(|(stream, _)| *stream)
  }

  /// Asks the driver for a keyframe on each of `layers` of the own track `stream_id` (every
  /// layer when the track has no layer list).
  fn request_own_keyframes(&mut self, stream_id: MediaStreamId, layers: &[u8]) {
    let layers = if layers.is_empty() { &[0][..] } else { layers };
    for layer in layers {
      self.event(CallEngineEvent::KeyframeRequested {
        stream_id,
        layer: *layer,
      });
    }
  }

  /// Encrypts one audio frame and sends it on `stream`. Dropped before DAVE is ready (there is no
  /// unencrypted fallback, ARCHITECTURE.md §6.2). Both of a device's audio streams share its DAVE
  /// audio encryptor, so their nonces never repeat.
  fn send_audio_frame(
    &mut self,
    stream: StreamId,
    origin: AudioOrigin,
    frame: &EncodedAudioFrame,
    now: Instant,
  ) {
    if self.is_compromised {
      return;
    }
    let Some(dave) = self.maybe_dave.as_mut().filter(|dave| dave.is_ready()) else {
      return;
    };
    let encrypted = match dave.encrypt_frame(MediaKind::Audio, FrameCodec::Opus, &frame.opus) {
      Ok(encrypted) => encrypted.into_owned(),
      Err(error) => {
        warn!(%error, "audio frame not encrypted, dropped");
        return;
      },
    };
    let rtp_state = match origin {
      AudioOrigin::Microphone => &mut self.microphone_rtp,
      AudioOrigin::Screen => &mut self.screen_rtp,
    };
    let mut header = RtpHeader::new(
      PayloadType::OPUS,
      rtp_state.sequence,
      rtp_state.timestamp,
      Ssrc(stream.0),
    );
    header.set_audio_level(frame.level);
    rtp_state.advance();
    let Ok(rtp) = (RtpPacket {
      header,
      payload: &encrypted,
    })
    .to_bytes() else {
      return;
    };
    self.send_media(stream, SimulcastLayer::BASE, &rtp, now);
  }

  /// The SFU announced screen sound; ours starts sending (or is withdrawn if no longer wanted).
  fn on_screen_audio_published(&mut self, track: ScreenAudioTrack) {
    let stream = StreamId(track.stream_id.0);
    if track.device_id == self.config.device_id {
      if self.screen_audio == ScreenAudioState::Requested {
        self.screen_audio = ScreenAudioState::Publishing(stream);
        self.event(CallEngineEvent::ScreenAudioChanged {
          device_id: track.device_id,
          is_shared: true,
        });
      } else {
        self.signal(CallClientMessage::UnpublishScreenAudio {
          stream_id: track.stream_id,
        });
      }
      return;
    }
    // The SFU's word is checked where it can be: a stream id already in use (someone's
    // microphone) or a device that isn't in the call, or a second screen sound of one device,
    // would let it relabel or cut off audio.
    let is_taken = self.audio_streams.contains_key(&stream) || self.remote_video.contains_key(&stream);
    let has_screen_audio = self
      .audio_streams
      .values()
      .any(|(owner, origin)| *owner == track.device_id && *origin == AudioOrigin::Screen);
    if is_taken || has_screen_audio || !self.participants.contains_key(&track.device_id) {
      tracing::warn!(
        stream_id = stream.0,
        "ignored an inconsistent screen sound announcement"
      );
      return;
    }
    self
      .audio_streams
      .insert(stream, (track.device_id, AudioOrigin::Screen));
    self.event(CallEngineEvent::ScreenAudioChanged {
      device_id: track.device_id,
      is_shared: true,
    });
  }

  fn on_screen_audio_unpublished(&mut self, stream_id: MediaStreamId) {
    let stream = StreamId(stream_id.0);
    let maybe_device = if self.screen_audio == ScreenAudioState::Publishing(stream) {
      self.screen_audio = ScreenAudioState::Off;
      Some(self.config.device_id)
    } else {
      self.audio_streams.remove(&stream).map(|(device_id, _)| device_id)
    };
    if let Some(device_id) = maybe_device {
      self.event(CallEngineEvent::ScreenAudioChanged {
        device_id,
        is_shared: false,
      });
    }
  }

  /// Wraps one RTP packet in a datagram with a transport sequence number for uplink estimation.
  fn send_media(&mut self, stream: StreamId, layer: SimulcastLayer, rtp: &[u8], now: Instant) {
    let datagram_len = rtp.len() + crate::datagram::DATAGRAM_HEADER_BYTES + 2;
    let transport_seq = self.uplink.on_send(datagram_len, now);
    let datagram = MediaDatagram {
      header: DatagramHeader {
        stream_id: stream,
        layer,
        maybe_transport_seq: Some(transport_seq),
      },
      rtp,
    };
    match datagram.to_bytes(MAX_DATAGRAM_BYTES) {
      Ok(bytes) => self.outputs.push_back(EngineOutput::Datagram(bytes)),
      Err(error) => warn!(%error, "media datagram too large"),
    }
  }

  fn add_participant(&mut self, participant: CallParticipant) {
    self.audio_streams.insert(
      StreamId(participant.audio_stream_id.0),
      (participant.device_id, AudioOrigin::Microphone),
    );
    self.participants.insert(participant.device_id, participant);
  }

  fn report_secured(&mut self) {
    let Some(dave) = &self.maybe_dave else {
      return;
    };
    if let (Some(epoch), Some(code)) = (dave.maybe_epoch(), dave.maybe_privacy_code()) {
      let event = CallEngineEvent::Secured {
        epoch: epoch.0,
        privacy_code: code.to_owned(),
      };
      self.event(event);
      // Pictures before this were dropped (nothing leaves unencrypted), and receivers that just
      // got the new epoch's keys start with a keyframe.
      let own: Vec<(MediaStreamId, Vec<u8>)> = self
        .own_tracks
        .values()
        .map(|track| {
          (
            track.stream_id,
            track.layers.iter().map(|info| info.layer).collect(),
          )
        })
        .collect();
      for (stream_id, layers) in own {
        self.request_own_keyframes(stream_id, &layers);
      }
    }
  }

  fn dave(&mut self) -> Result<&mut DaveParticipant, CallEngineError> {
    self
      .maybe_dave
      .as_mut()
      .ok_or(CallEngineError::Protocol("DAVE message before joined"))
  }

  fn signal(&mut self, message: CallClientMessage) {
    self.outputs.push_back(EngineOutput::Signal(message));
  }

  fn event(&mut self, event: CallEngineEvent) {
    self.outputs.push_back(EngineOutput::Event(event));
  }
}

impl OwnPublication {
  /// A publication just asked for: no stream yet.
  pub fn new(id: VideoPublicationId) -> Self {
    Self {
      id,
      maybe_stream_id: None,
    }
  }

  /// Follows one of the engine's events: the confirmation of this publication brings its stream,
  /// and the stream's end (the SFU dropped it) takes it away again. Returns whether the event
  /// brought the stream (receivers then start with a keyframe).
  pub fn follow(&mut self, own_device: DeviceId, event: &CallEngineEvent) -> bool {
    match event {
      CallEngineEvent::VideoPublished(track)
        if track.device_id == own_device && track.maybe_publication_id == Some(self.id) =>
      {
        self.maybe_stream_id = Some(track.stream_id);
        true
      },
      CallEngineEvent::VideoUnpublished(stream_id) if self.maybe_stream_id == Some(*stream_id) => {
        self.maybe_stream_id = None;
        false
      },
      _ => false,
    }
  }
}

/// DAVE's codec for a video codec (it leaves codec headers such as AV1 OBU headers readable).
impl AudioRtpState {
  fn new() -> Self {
    Self {
      sequence: SequenceNumber(0),
      timestamp: RtpTimestamp(0),
    }
  }

  /// Moves on by one 20 ms frame.
  fn advance(&mut self) {
    self.sequence = self.sequence.next();
    self.timestamp = RtpTimestamp(self.timestamp.0.wrapping_add(OPUS_FRAME_TICKS));
  }
}

/// Measured loss as a whole percentage.
/// The own tracks as the uplink allocation sees them.
fn own_track_views<'a>(
  own_tracks: &'a HashMap<StreamId, VideoTrack>,
  adaptive_tracks: &HashSet<StreamId>,
) -> Vec<OwnTrack<'a>> {
  own_tracks
    .iter()
    .map(|(stream, track)| OwnTrack {
      stream: *stream,
      kind: &track.kind,
      layers: &track.layers,
      adapts_bitrate: adaptive_tracks.contains(stream),
    })
    .collect()
}

fn loss_percent(ratio: f64) -> u8 {
  // Invariant: clamped to 0..=100 first, so the cast is lossless.
  (ratio * 100.0).round().clamp(0.0, 100.0) as u8
}

fn frame_codec(codec: &VideoCodec) -> FrameCodec {
  match codec {
    VideoCodec::Av1 => FrameCodec::Av1,
    VideoCodec::Vp9 => FrameCodec::Vp9,
    VideoCodec::H264 => FrameCodec::H264,
    // Unknown codecs are never negotiated (choose_video_codec skips them); encrypt the whole frame.
    VideoCodec::Unknown(_) => FrameCodec::Vp9,
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, MediaStreamId, Snowflake};

  use super::*;
  use crate::call_auth::DeviceCallKeyAuthenticator;
  use crate::dave::{DaveEpoch, DaveExternalSender};
  use crate::rtp::AudioLevel;

  const CALL: i64 = 99;

  mod signaling {
    use super::*;

    /// A track goes out in the best codec the device encodes and every other device decodes; H.264
    /// until the SFU says (an older SFU never does), and when it falls out of the list (#565).
    #[test]
    fn the_codec_follows_what_the_others_decode() {
      let now = Instant::now();
      let mut engine = CallEngine::new(
        CallEngineConfig {
          capabilities: MediaCapabilities {
            video_encoders: vec![VideoCodec::Av1, VideoCodec::H264],
            ..MediaCapabilities::default()
          },
          ..engine(1, now).config
        },
        now,
      );
      engine.join("t".into());
      engine.handle_signal(event(1, joined(1, &[2])), now).unwrap();
      let screen = [VideoCodec::Av1, VideoCodec::H264];
      assert_eq!(engine.video_codec_for(&screen), VideoCodec::H264, "older SFU");

      let all = CallEvent::DecodableVideoCodecs {
        codecs: vec![VideoCodec::Av1, VideoCodec::Vp9, VideoCodec::H264],
      };
      engine.handle_signal(event(2, all.clone()), now).unwrap();
      assert_eq!(engine.video_codec_for(&screen), VideoCodec::Av1);
      assert_eq!(engine.video_codec_for(&[VideoCodec::H264]), VideoCodec::H264);
      // VP9 is decoded by all, but this device doesn't encode it.
      assert_eq!(engine.video_codec_for(&[VideoCodec::Vp9]), VideoCodec::H264);
      assert!(drain(&mut engine).iter().any(|output| matches!(
        output,
        EngineOutput::Event(CallEngineEvent::DecodableVideoCodecs(codecs)) if codecs.len() == 3
      )));

      let h264_only = CallEvent::DecodableVideoCodecs {
        codecs: vec![VideoCodec::H264],
      };
      engine.handle_signal(event(3, h264_only.clone()), now).unwrap();
      assert_eq!(engine.video_codec_for(&screen), VideoCodec::H264);
      engine.handle_signal(event(4, h264_only), now).unwrap();
      let repeated = drain(&mut engine);
      assert_eq!(
        repeated
          .iter()
          .filter(|output| matches!(
            output,
            EngineOutput::Event(CallEngineEvent::DecodableVideoCodecs(_))
          ))
          .count(),
        1,
        "told once per change: {repeated:?}"
      );
    }

    #[test]
    fn join_sends_token_and_capabilities() {
      let now = Instant::now();
      let mut engine = engine(1, now);
      engine.join("token".into());
      assert!(matches!(
        engine.poll_output(),
        Some(EngineOutput::Signal(CallClientMessage::Join { token, .. })) if token == "token"
      ));
    }

    #[test]
    fn event_sequence_gaps_are_detected() {
      let now = Instant::now();
      let mut engine = engine(1, now);
      engine.join("t".into());
      let result = engine.handle_signal(
        CallServerMessage::Event {
          seq: 2,
          event: CallEvent::ActiveSpeakers {
            device_ids: Vec::new(),
          },
        },
        now,
      );
      assert!(matches!(
        result,
        Err(CallEngineError::SequenceGap { last: 0, received: 2 })
      ));
    }

    #[test]
    fn error_in_answer_to_join_is_a_refusal() {
      let now = Instant::now();
      let mut engine = engine(1, now);
      engine.join("t".into());
      let refusal = CallServerMessage::Error {
        error: ErrorBody::new(teamcraft_proto::ErrorCode::CallLocked, "locked"),
      };
      engine.handle_signal(refusal.clone(), now).unwrap();
      assert!(engine.is_over());
      // Errors for requests during the call are no refusal.
      let mut joined_engine = engine_joined(2, now);
      joined_engine.handle_signal(refusal, now).unwrap();
      assert!(!joined_engine.is_over());
      // Replaced by another connection of this device: over too.
      joined_engine
        .handle_signal(event(2, CallEvent::ConnectionReplaced), now)
        .unwrap();
      assert!(joined_engine.is_over());
    }

    #[test]
    fn joined_as_someone_else_is_rejected() {
      let now = Instant::now();
      let mut engine = engine(1, now);
      engine.join("t".into());
      let result = engine.handle_signal(event(1, joined(2, &[])), now);
      assert!(matches!(result, Err(CallEngineError::Protocol(_))));
    }
  }

  /// Received video that can't be decoded until the next keyframe asks for one (issue #683).
  mod keyframe_recovery {
    use super::*;

    const STREAM: MediaStreamId = MediaStreamId(50);

    #[test]
    fn a_lost_keyframe_fragment_drops_the_deltas_and_asks_once() {
      let now = Instant::now();
      let (mut alice, mut bob) = watching_pair(now);
      let mut keyframe = datagrams_of(&mut alice, true, now);
      assert!(keyframe.len() >= 3);
      keyframe.remove(1);
      deliver(&mut bob, &keyframe, now);
      deliver(&mut bob, &datagrams_of(&mut alice, false, now), now);
      let outputs = drain(&mut bob);
      assert!(frames(&outputs).is_empty(), "nothing decodable yet");
      assert_eq!(requests(&outputs), vec![STREAM]);
      // More deltas before the interval passed: still dropped, no second request.
      let soon = now + Duration::from_millis(300);
      deliver(&mut bob, &datagrams_of(&mut alice, false, soon), soon);
      let outputs = drain(&mut bob);
      assert!(frames(&outputs).is_empty());
      assert!(requests(&outputs).is_empty());
      // The keyframe that answers the request, and the deltas after it, go to the decoder.
      deliver(&mut bob, &datagrams_of(&mut alice, true, soon), soon);
      deliver(&mut bob, &datagrams_of(&mut alice, false, soon), soon);
      let decoded = frames(&drain(&mut bob));
      assert_eq!(decoded, vec![true, false]);
    }

    #[test]
    fn a_wholly_lost_delta_is_noticed_from_the_sequence_numbers() {
      let now = Instant::now();
      let (mut alice, mut bob) = watching_pair(now);
      deliver(&mut bob, &datagrams_of(&mut alice, true, now), now);
      drain(&mut bob);
      // One delta never arrives at all; the next one can't follow it.
      datagrams_of(&mut alice, false, now);
      deliver(&mut bob, &datagrams_of(&mut alice, false, now), now);
      let outputs = drain(&mut bob);
      assert!(frames(&outputs).is_empty());
      assert_eq!(requests(&outputs), vec![STREAM]);
    }

    #[test]
    fn reordered_fragments_of_a_frame_are_no_loss() {
      let now = Instant::now();
      let (mut alice, mut bob) = watching_pair(now);
      let mut keyframe = datagrams_of(&mut alice, true, now);
      keyframe.swap(1, 2);
      deliver(&mut bob, &keyframe, now);
      deliver(&mut bob, &datagrams_of(&mut alice, false, now), now);
      let outputs = drain(&mut bob);
      assert_eq!(frames(&outputs), vec![true, false]);
      assert!(requests(&outputs).is_empty());
    }

    #[test]
    fn an_app_whose_decoder_failed_asks_at_most_once_per_interval() {
      let now = Instant::now();
      let (mut alice, mut bob) = watching_pair(now);
      deliver(&mut bob, &datagrams_of(&mut alice, true, now), now);
      drain(&mut bob);
      bob.request_keyframe(STREAM, now);
      bob.request_keyframe(STREAM, now + Duration::from_millis(500));
      assert_eq!(requests(&drain(&mut bob)), vec![STREAM]);
      // Its deltas wait for the keyframe too.
      deliver(&mut bob, &datagrams_of(&mut alice, false, now), now);
      assert!(frames(&drain(&mut bob)).is_empty());
      bob.request_keyframe(STREAM, now + KEYFRAME_REQUEST_INTERVAL);
      assert_eq!(requests(&drain(&mut bob)), vec![STREAM]);
    }

    #[test]
    fn subscribing_again_waits_for_the_keyframe_the_sfu_asks_for() {
      let now = Instant::now();
      let (mut alice, mut bob) = watching_pair(now);
      deliver(&mut bob, &datagrams_of(&mut alice, true, now), now);
      drain(&mut bob);
      bob.subscribe_video(STREAM, Some(0));
      deliver(&mut bob, &datagrams_of(&mut alice, false, now), now);
      let outputs = drain(&mut bob);
      assert!(frames(&outputs).is_empty(), "a new decoder starts at a keyframe");
      // The SFU asked the publisher on the subscribe already and merges this one with it.
      assert_eq!(requests(&outputs), vec![STREAM]);
      deliver(&mut bob, &datagrams_of(&mut alice, true, now), now);
      assert_eq!(frames(&drain(&mut bob)), vec![true]);
    }

    /// Alice publishes a screen track (one layer) that Bob receives.
    fn watching_pair(now: Instant) -> (CallEngine, CallEngine) {
      let (mut alice, mut bob) = secured_pair(now);
      let track = VideoTrack {
        stream_id: STREAM,
        device_id: device(1),
        kind: VideoTrackKind::Screen,
        codec: VideoCodec::H264,
        layers: Vec::new(),
        maybe_publication_id: None,
      };
      alice.publish_video(
        VideoPublicationId(0),
        VideoTrackKind::Screen,
        VideoCodec::H264,
        Vec::new(),
      );
      let published = CallEvent::VideoPublished { track };
      alice.handle_signal(event(6, published.clone()), now).unwrap();
      bob.handle_signal(event(4, published), now).unwrap();
      drain(&mut alice);
      drain(&mut bob);
      (alice, bob)
    }

    /// The datagrams of one frame Alice sends: keyframes in several fragments, deltas in one.
    fn datagrams_of(alice: &mut CallEngine, is_keyframe: bool, now: Instant) -> Vec<Vec<u8>> {
      let size = if is_keyframe { 4000 } else { 300 };
      let frame = EncodedVideoFrame {
        codec: VideoCodec::H264,
        layer: SimulcastLayer::BASE,
        timestamp: RtpTimestamp(0),
        is_keyframe,
        data: [
          &[0, 0, 0, 1, if is_keyframe { 0x65 } else { 0x41 }][..],
          &vec![0x88; size],
        ]
        .concat(),
      };
      assert_eq!(alice.send_video(STREAM, &frame, now), VideoSendOutcome::Sent);
      drain(alice)
        .into_iter()
        .filter_map(|output| match output {
          EngineOutput::Datagram(bytes) => Some(bytes),
          _ => None,
        })
        .collect()
    }

    fn deliver(bob: &mut CallEngine, datagrams: &[Vec<u8>], now: Instant) {
      for datagram in datagrams {
        bob.handle_datagram(datagram, now);
      }
    }

    /// Whether each frame given to the app was a keyframe.
    fn frames(outputs: &[EngineOutput]) -> Vec<bool> {
      outputs
        .iter()
        .filter_map(|output| match output {
          EngineOutput::Event(CallEngineEvent::VideoFrame(frame)) => Some(frame.is_keyframe),
          _ => None,
        })
        .collect()
    }

    fn requests(outputs: &[EngineOutput]) -> Vec<MediaStreamId> {
      outputs
        .iter()
        .filter_map(|output| match output {
          EngineOutput::Signal(CallClientMessage::RequestKeyframe { stream_id }) => Some(*stream_id),
          _ => None,
        })
        .collect()
    }
  }

  mod media {
    use super::*;

    #[test]
    fn two_engines_exchange_encrypted_audio() {
      let now = Instant::now();
      let (mut alice, mut bob) = secured_pair(now);
      alice.send_audio(&frame(), now);
      let datagram = take_datagram(&mut alice);
      // The payload on the wire is not the Opus frame.
      assert!(
        !datagram
          .windows(frame().opus.len())
          .any(|window| window == frame().opus)
      );
      bob.handle_datagram(&datagram, now);
      let received = drain(&mut bob)
        .into_iter()
        .find_map(|output| match output {
          EngineOutput::Event(CallEngineEvent::AudioFrame { sender, opus, .. }) => Some((sender, opus)),
          _ => None,
        })
        .unwrap();
      assert_eq!(received, (device(1), frame().opus));
      assert_eq!(
        alice.verification_code(device(2)),
        bob.verification_code(device(1))
      );
    }

    #[test]
    fn screen_audio_announcements_cannot_relabel_or_cut_off_audio() {
      let now = Instant::now();
      let (_alice, mut bob) = secured_pair(now);
      for (seq, device_id, stream) in [
        // Alice's microphone stream: taking it over would cut her off.
        (4, device(1), 1),
        // A device that is not in the call.
        (5, device(9), 41),
      ] {
        let track = ScreenAudioTrack {
          device_id,
          stream_id: MediaStreamId(stream),
        };
        bob
          .handle_signal(event(seq, CallEvent::ScreenAudioPublished { track }), now)
          .unwrap();
      }
      let first = ScreenAudioTrack {
        device_id: device(1),
        stream_id: MediaStreamId(40),
      };
      let second = ScreenAudioTrack {
        device_id: device(1),
        stream_id: MediaStreamId(42),
      };
      bob
        .handle_signal(event(6, CallEvent::ScreenAudioPublished { track: first }), now)
        .unwrap();
      bob
        .handle_signal(event(7, CallEvent::ScreenAudioPublished { track: second }), now)
        .unwrap();
      let shared = drain(&mut bob)
        .into_iter()
        .filter(|output| {
          matches!(
            output,
            EngineOutput::Event(CallEngineEvent::ScreenAudioChanged { .. })
          )
        })
        .count();
      assert_eq!(shared, 1, "only the first consistent announcement counts");
      assert_eq!(
        bob.audio_streams.get(&StreamId(1)),
        Some(&(device(1), AudioOrigin::Microphone))
      );
    }

    #[test]
    fn screen_audio_has_its_own_stream_and_ignores_the_microphone_mute() {
      let now = Instant::now();
      let (mut alice, mut bob) = secured_pair(now);
      alice.publish_screen_audio();
      alice.send_screen_audio(&frame(), now);
      let requested = drain(&mut alice);
      assert!(requested.contains(&EngineOutput::Signal(CallClientMessage::PublishScreenAudio)));
      assert!(
        !requested
          .iter()
          .any(|output| matches!(output, EngineOutput::Datagram(_))),
        "nothing is sent before the SFU assigned a stream"
      );

      let track = ScreenAudioTrack {
        device_id: device(1),
        stream_id: MediaStreamId(40),
      };
      alice
        .handle_signal(event(6, CallEvent::ScreenAudioPublished { track }), now)
        .unwrap();
      bob
        .handle_signal(event(4, CallEvent::ScreenAudioPublished { track }), now)
        .unwrap();
      let shared = CallEngineEvent::ScreenAudioChanged {
        device_id: device(1),
        is_shared: true,
      };
      assert!(drain(&mut alice).contains(&EngineOutput::Event(shared.clone())));
      assert!(drain(&mut bob).contains(&EngineOutput::Event(shared)));

      alice.set_muted(true);
      alice.send_screen_audio(&frame(), now);
      bob.handle_datagram(&take_datagram(&mut alice), now);
      let origin = drain(&mut bob).into_iter().find_map(|output| match output {
        EngineOutput::Event(CallEngineEvent::AudioFrame { sender, origin, .. }) => Some((sender, origin)),
        _ => None,
      });
      assert_eq!(origin, Some((device(1), AudioOrigin::Screen)));

      alice.unpublish_screen_audio();
      assert!(
        drain(&mut alice).contains(&EngineOutput::Signal(CallClientMessage::UnpublishScreenAudio {
          stream_id: MediaStreamId(40)
        }))
      );
      alice.send_screen_audio(&frame(), now);
      assert!(drain(&mut alice).is_empty(), "nothing is sent after unpublishing");
      bob
        .handle_signal(
          event(
            5,
            CallEvent::ScreenAudioUnpublished {
              stream_id: MediaStreamId(40),
            },
          ),
          now,
        )
        .unwrap();
      assert!(
        drain(&mut bob).contains(&EngineOutput::Event(CallEngineEvent::ScreenAudioChanged {
          device_id: device(1),
          is_shared: false,
        }))
      );
    }

    #[test]
    fn screen_audio_stopped_before_the_answer_is_withdrawn() {
      let now = Instant::now();
      let (mut alice, _) = secured_pair(now);
      alice.publish_screen_audio();
      alice.unpublish_screen_audio();
      drain(&mut alice);
      let track = ScreenAudioTrack {
        device_id: device(1),
        stream_id: MediaStreamId(40),
      };
      alice
        .handle_signal(event(6, CallEvent::ScreenAudioPublished { track }), now)
        .unwrap();
      assert_eq!(
        drain(&mut alice),
        vec![EngineOutput::Signal(CallClientMessage::UnpublishScreenAudio {
          stream_id: MediaStreamId(40)
        })]
      );
      alice.send_screen_audio(&frame(), now);
      assert!(drain(&mut alice).is_empty());
    }

    #[test]
    fn muted_engine_sends_nothing_and_deafened_engine_drops_audio() {
      let now = Instant::now();
      let (mut alice, mut bob) = secured_pair(now);
      alice.set_muted(true);
      alice.send_audio(&frame(), now);
      assert!(
        drain(&mut alice)
          .iter()
          .all(|output| !matches!(output, EngineOutput::Datagram(_)))
      );
      alice.set_muted(false);
      alice.send_audio(&frame(), now);
      let datagram = take_datagram(&mut alice);
      bob.set_deafened(true);
      drain(&mut bob);
      bob.handle_datagram(&datagram, now);
      assert!(drain(&mut bob).is_empty());
    }

    #[test]
    fn audio_before_the_group_is_ready_is_dropped() {
      let now = Instant::now();
      let mut engine = engine(1, now);
      engine.join("t".into());
      engine.handle_signal(event(1, joined(1, &[])), now).unwrap();
      engine.send_audio(&frame(), now);
      assert!(
        drain(&mut engine)
          .iter()
          .all(|output| !matches!(output, EngineOutput::Datagram(_)))
      );
    }

    #[test]
    fn screen_share_frames_arrive_encrypted_and_whole() {
      let now = Instant::now();
      let (mut alice, mut bob) = secured_pair(now);
      let track = VideoTrack {
        stream_id: MediaStreamId(50),
        device_id: device(1),
        kind: VideoTrackKind::Screen,
        codec: VideoCodec::Vp9,
        layers: Vec::new(),
        maybe_publication_id: None,
      };
      alice.publish_video(
        VideoPublicationId(0),
        VideoTrackKind::Screen,
        VideoCodec::Vp9,
        Vec::new(),
      );
      assert!(matches!(
        alice.poll_output(),
        Some(EngineOutput::Signal(CallClientMessage::PublishVideo { .. }))
      ));
      let published = CallEvent::VideoPublished { track: track.clone() };
      alice.handle_signal(event(6, published.clone()), now).unwrap();
      bob.handle_signal(event(4, published), now).unwrap();
      drain(&mut alice);
      assert_eq!(
        drain(&mut bob),
        vec![EngineOutput::Event(CallEngineEvent::VideoPublished(track))]
      );

      let data: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
      let frame = EncodedVideoFrame {
        codec: VideoCodec::Vp9,
        layer: SimulcastLayer::BASE,
        timestamp: RtpTimestamp(3000),
        is_keyframe: true,
        data: data.clone(),
      };
      alice.send_video(MediaStreamId(50), &frame, now);
      let datagrams: Vec<Vec<u8>> = drain(&mut alice)
        .into_iter()
        .filter_map(|output| match output {
          EngineOutput::Datagram(bytes) => Some(bytes),
          _ => None,
        })
        .collect();
      assert!(datagrams.len() >= 5, "5000 bytes need several datagrams");
      assert!(
        datagrams
          .iter()
          .all(|datagram| datagram.len() <= MAX_DATAGRAM_BYTES)
      );
      for datagram in datagrams.iter().rev() {
        bob.handle_datagram(datagram, now);
      }
      let frames: Vec<ReceivedVideoFrame> = drain(&mut bob)
        .into_iter()
        .filter_map(|output| match output {
          EngineOutput::Event(CallEngineEvent::VideoFrame(frame)) => Some(frame),
          _ => None,
        })
        .collect();
      assert_eq!(frames.len(), 1);
      assert_eq!(frames[0].data, data);
      assert!(frames[0].is_keyframe);
      assert_eq!(frames[0].sender, device(1));
    }

    /// Stats for nerds (#938): both sides count what went out and came in, per track and layer.
    #[cfg(feature = "call-view")]
    #[test]
    fn video_stats_count_what_each_side_sent_and_received() {
      use teamcraft_client_api::video_stats::VideoTrackStats;

      let now = Instant::now();
      let (mut alice, mut bob) = secured_pair(now);
      let track = video_track(60, 1, 2);
      ask_for(&mut alice, &track);
      let published = CallEvent::VideoPublished { track: track.clone() };
      alice.handle_signal(event(6, published.clone()), now).unwrap();
      bob.handle_signal(event(4, published), now).unwrap();
      bob.subscribe_video(track.stream_id, Some(1));
      drain(&mut alice);
      drain(&mut bob);

      for timestamp in [0, 3000] {
        let frame = EncodedVideoFrame {
          timestamp: RtpTimestamp(timestamp),
          ..video_frame()
        };
        alice.send_video(track.stream_id, &frame, now);
      }
      let datagrams: Vec<Vec<u8>> = drain(&mut alice)
        .into_iter()
        .filter_map(|output| match output {
          EngineOutput::Datagram(bytes) => Some(bytes),
          _ => None,
        })
        .collect();
      for datagram in &datagrams {
        bob.handle_datagram(datagram, now + Duration::from_millis(5));
      }

      let sent_report = alice.video_stats(1_000);
      assert_eq!(sent_report.at_ms, 1_000);
      assert!(sent_report.maybe_uplink.is_some());
      let sent = sent_report.sent(&VideoTrackKind::Camera).expect("the own camera");
      assert_eq!(sent.stream_id, track.stream_id);
      assert_eq!(sent.packets, 2);
      let layers: Vec<(u8, u64, bool)> = sent
        .layers
        .iter()
        .map(|layer| (layer.layer, layer.pictures, layer.is_target))
        .collect();
      assert_eq!(layers, [(0, 2, true), (1, 0, true)]);
      assert_eq!(sent.layers[1].size.width, 640);
      assert!(matches!(
        sent_report.tracks.as_slice(),
        [VideoTrackStats::Sent(_)]
      ));

      let received_report = bob.video_stats(1_000);
      let received = received_report.received(track.stream_id).expect("alice's camera");
      assert_eq!(
        (received.packets, received.frames, received.packets_lost),
        (2, 2, 0)
      );
      assert_eq!(received.bytes, sent.bytes);
      assert_eq!(received.maybe_layer, Some(0));
      assert_eq!(received.maybe_max_layer, Some(1));
      assert_eq!(received.layers.len(), 2);
      assert!(received.maybe_jitter_ms.is_some());
    }

    #[test]
    fn video_of_an_unpublished_track_is_not_sent() {
      let now = Instant::now();
      let (mut alice, _) = secured_pair(now);
      let frame = EncodedVideoFrame {
        codec: VideoCodec::Vp9,
        layer: SimulcastLayer::BASE,
        timestamp: RtpTimestamp(0),
        is_keyframe: true,
        data: vec![1; 100],
      };
      assert_eq!(
        alice.send_video(MediaStreamId(50), &frame, now),
        VideoSendOutcome::NotPublished
      );
      assert!(drain(&mut alice).is_empty());
    }

    #[test]
    fn a_confirmed_own_track_asks_for_a_keyframe_on_every_layer() {
      let now = Instant::now();
      let (mut alice, _bob) = secured_pair(now);
      let track = own_camera(layers(2));
      alice.publish_video(
        VideoPublicationId(0),
        VideoTrackKind::Camera,
        VideoCodec::H264,
        layers(2),
      );
      drain(&mut alice);
      let published = CallEvent::VideoPublished { track: track.clone() };
      alice.handle_signal(event(6, published), now).unwrap();
      // Pictures encoded before the confirmation were dropped, the first keyframe among them.
      assert_eq!(
        keyframe_requests(&mut alice),
        vec![(MediaStreamId(50), 0), (MediaStreamId(50), 1)]
      );

      let mut frame = EncodedVideoFrame {
        codec: VideoCodec::H264,
        layer: SimulcastLayer::BASE,
        timestamp: RtpTimestamp(0),
        is_keyframe: true,
        // An IDR slice header (first macroblock 0, slice type, picture parameter set 0) and data.
        data: [&[0, 0, 0, 1, 0x65][..], &[0x88; 64]].concat(),
      };
      assert_eq!(
        alice.send_video(MediaStreamId(50), &frame, now),
        VideoSendOutcome::Sent
      );
      frame.layer = SimulcastLayer::new(2).unwrap();
      assert_eq!(
        alice.send_video(MediaStreamId(50), &frame, now),
        VideoSendOutcome::LayerPaused
      );
    }

    #[test]
    fn securing_the_call_asks_for_keyframes_of_own_tracks() {
      let now = Instant::now();
      let (mut alice, _bob) = secured_pair(now);
      alice.publish_video(
        VideoPublicationId(0),
        VideoTrackKind::Camera,
        VideoCodec::H264,
        layers(1),
      );
      let published = CallEvent::VideoPublished {
        track: own_camera(layers(1)),
      };
      alice.handle_signal(event(6, published), now).unwrap();
      drain(&mut alice);
      // A new epoch (someone joined, or the group just became ready): pictures before it were
      // dropped or can't be decrypted by the newcomers.
      alice.report_secured();
      assert_eq!(keyframe_requests(&mut alice), vec![(MediaStreamId(50), 0)]);
    }

    fn own_camera(layers: Vec<VideoLayerInfo>) -> VideoTrack {
      VideoTrack {
        stream_id: MediaStreamId(50),
        device_id: device(1),
        kind: VideoTrackKind::Camera,
        codec: VideoCodec::H264,
        layers,
        maybe_publication_id: None,
      }
    }

    fn keyframe_requests(engine: &mut CallEngine) -> Vec<(MediaStreamId, u8)> {
      drain(engine)
        .into_iter()
        .filter_map(|output| match output {
          EngineOutput::Event(CallEngineEvent::KeyframeRequested { stream_id, layer }) => {
            Some((stream_id, layer))
          },
          _ => None,
        })
        .collect()
    }

    #[test]
    fn received_datagrams_produce_transport_feedback() {
      let now = Instant::now();
      let (mut alice, mut bob) = secured_pair(now);
      alice.send_audio(&frame(), now);
      bob.handle_datagram(&take_datagram(&mut alice), now);
      drain(&mut bob);
      bob.on_timer(now);
      assert!(matches!(
        bob.poll_output(),
        Some(EngineOutput::Signal(CallClientMessage::TransportFeedback { .. }))
      ));
    }

    /// #994: the SFU probes a receiver's downlink with padding on a stream it never assigns; the
    /// receiver reports its arrival like any datagram's and otherwise drops it.
    #[test]
    fn padding_is_reported_and_dropped() {
      let now = Instant::now();
      let (_alice, mut bob) = secured_pair(now);
      drain(&mut bob);
      for seq in 0..4 {
        let padding = MediaDatagram {
          header: DatagramHeader {
            stream_id: crate::datagram::PADDING_STREAM,
            layer: SimulcastLayer::BASE,
            maybe_transport_seq: Some(crate::datagram::TransportSeq(seq)),
          },
          rtp: &[0; 993],
        }
        .to_bytes(MAX_DATAGRAM_BYTES)
        .unwrap();
        bob.handle_datagram(&padding, now + Duration::from_millis(u64::from(seq) * 32));
      }
      assert!(drain(&mut bob).is_empty(), "padding is no media");
      bob.on_timer(now);
      let Some(EngineOutput::Signal(CallClientMessage::TransportFeedback { feedback })) = bob.poll_output()
      else {
        panic!("no feedback for the padding");
      };
      assert_eq!(feedback.base_seq, 0);
      assert_eq!(
        feedback
          .arrivals_us
          .iter()
          .filter(|arrival| arrival.is_some())
          .count(),
        4
      );
    }
  }

  mod reconnect {
    use super::*;

    #[test]
    fn lost_connection_pauses_media_and_the_rejoin_keeps_rtp_numbering() {
      let now = Instant::now();
      let (mut alice, _bob) = secured_pair(now);
      alice.send_audio(&frame(), now);
      take_datagram(&mut alice);
      let sequence = alice.microphone_rtp.sequence;

      alice.connection_lost();
      assert_eq!(
        drain(&mut alice),
        vec![EngineOutput::Event(CallEngineEvent::Reconnecting)]
      );
      assert!(alice.is_reconnecting() && !alice.is_secured());
      alice.send_audio(&frame(), now);
      assert!(drain(&mut alice).is_empty(), "nothing is sent while away");

      alice.rejoin("fresh".into(), now);
      assert!(matches!(
        alice.poll_output(),
        Some(EngineOutput::Signal(CallClientMessage::Join { token, .. })) if token == "fresh"
      ));
      // The new connection numbers its events from 1 again.
      alice.handle_signal(event(1, joined(1, &[2])), now).unwrap();
      assert_eq!(
        drain(&mut alice),
        vec![EngineOutput::Event(CallEngineEvent::Reconnected)]
      );
      assert!(!alice.is_reconnecting());
      assert_eq!(alice.microphone_rtp.sequence, sequence);
      assert_eq!(alice.maybe_reconnect_grace(), Some(Duration::from_secs(30)));
      // DAVE starts over: a new key package once the external sender arrives.
      let gateway = DaveExternalSender::new().unwrap();
      let package = Base64Bytes::new(gateway.package().unwrap());
      alice
        .handle_signal(event(2, CallEvent::DaveExternalSender { package }), now)
        .unwrap();
      let (key_package, maybe_binding) = key_package_of(&mut alice);
      assert!(!key_package.is_empty() && maybe_binding.is_some());
    }

    #[test]
    fn rejoin_reconciles_what_changed_while_away() {
      let now = Instant::now();
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_engine(1, &[2, 3], &gateway, None, now);
      ask_for(&mut alice, &video_track(50, 1, 3));
      let mut seq = 3;
      for signal in [
        CallEvent::VideoPublished {
          track: video_track(60, 2, 1),
        },
        CallEvent::VideoPublished {
          track: video_track(50, 1, 3),
        },
      ] {
        alice.handle_signal(event(seq, signal), now).unwrap();
        seq += 1;
      }
      alice.subscribe_video(MediaStreamId(60), Some(2));
      alice.publish_screen_audio();
      let own_sound = ScreenAudioTrack {
        device_id: device(1),
        stream_id: MediaStreamId(70),
      };
      alice
        .handle_signal(
          event(seq, CallEvent::ScreenAudioPublished { track: own_sound }),
          now,
        )
        .unwrap();
      drain(&mut alice);

      alice.connection_lost();
      alice.rejoin("fresh".into(), now);
      drain(&mut alice);
      // The SFU lost the slot: 3 left, 4 came, nothing is published any more.
      alice.handle_signal(event(1, joined(1, &[2, 4])), now).unwrap();
      let outputs = drain(&mut alice);
      for expected in [
        EngineOutput::Event(CallEngineEvent::ParticipantLeft(device(3))),
        EngineOutput::Event(CallEngineEvent::ParticipantJoined(participant(4))),
        EngineOutput::Event(CallEngineEvent::VideoUnpublished(MediaStreamId(60))),
        EngineOutput::Event(CallEngineEvent::VideoUnpublished(MediaStreamId(50))),
        EngineOutput::Signal(CallClientMessage::PublishVideo {
          kind: VideoTrackKind::Camera,
          codec: VideoCodec::H264,
          layers: layers(3),
          maybe_publication_id: Some(VideoPublicationId(50)),
        }),
        EngineOutput::Signal(CallClientMessage::PublishScreenAudio),
        EngineOutput::Event(CallEngineEvent::Reconnected),
      ] {
        assert!(outputs.contains(&expected), "missing {expected:?} in {outputs:?}");
      }
      assert!(
        !outputs
          .iter()
          .any(|output| matches!(output, EngineOutput::Signal(CallClientMessage::Subscribe { .. }))),
        "no subscription to a track that is gone"
      );
      assert_eq!(
        outputs.last(),
        Some(&EngineOutput::Event(CallEngineEvent::Reconnected))
      );
    }

    #[test]
    fn rejoin_into_the_kept_slot_keeps_tracks_and_renews_subscriptions() {
      let now = Instant::now();
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_engine(1, &[2], &gateway, None, now);
      let remote = video_track(60, 2, 1);
      let own = video_track(50, 1, 3);
      ask_for(&mut alice, &own);
      alice
        .handle_signal(
          event(
            3,
            CallEvent::VideoPublished {
              track: remote.clone(),
            },
          ),
          now,
        )
        .unwrap();
      alice
        .handle_signal(event(4, CallEvent::VideoPublished { track: own.clone() }), now)
        .unwrap();
      alice.subscribe_video(MediaStreamId(60), Some(0));
      drain(&mut alice);

      alice.connection_lost();
      alice.rejoin("fresh".into(), now);
      drain(&mut alice);
      let CallEvent::Joined {
        call_id,
        participant,
        participants,
        screen_audio_tracks,
        maybe_reconnect_grace_ms,
        maybe_max_simulcast_layers,
        ..
      } = joined(1, &[2])
      else {
        unreachable!()
      };
      let kept = CallEvent::Joined {
        call_id,
        participant,
        participants,
        video_tracks: vec![remote, own],
        screen_audio_tracks,
        maybe_reconnect_grace_ms,
        maybe_max_simulcast_layers,
      };
      alice.handle_signal(event(1, kept), now).unwrap();
      assert_eq!(
        drain(&mut alice),
        vec![
          // The kept track starts over with every layer, and the SFU hears they are carried.
          EngineOutput::Event(CallEngineEvent::LayersNeeded {
            stream_id: MediaStreamId(50),
            layers: vec![0, 1, 2],
          }),
          EngineOutput::Signal(CallClientMessage::UplinkLayers {
            stream_id: MediaStreamId(50),
            layers: vec![0, 1, 2],
          }),
          EngineOutput::Signal(CallClientMessage::Subscribe {
            stream_id: MediaStreamId(60),
            maybe_max_layer: Some(0),
          }),
          EngineOutput::Event(CallEngineEvent::Reconnected),
        ]
      );
    }

    #[test]
    fn a_resized_tile_changes_the_layer_once_and_keeps_it_across_a_rejoin() {
      let now = Instant::now();
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_engine(1, &[2], &gateway, None, now);
      let remote = video_track(60, 2, 3);
      alice
        .handle_signal(
          event(
            3,
            CallEvent::VideoPublished {
              track: remote.clone(),
            },
          ),
          now,
        )
        .unwrap();
      drain(&mut alice);
      // Not received: nothing to change.
      alice.set_video_max_layer(MediaStreamId(60), 2);
      assert!(drain(&mut alice).is_empty());

      alice.subscribe_video(MediaStreamId(60), Some(0));
      drain(&mut alice);
      alice.set_video_max_layer(MediaStreamId(60), 2);
      alice.set_video_max_layer(MediaStreamId(60), 2);
      assert_eq!(
        drain(&mut alice),
        vec![EngineOutput::Signal(CallClientMessage::Subscribe {
          stream_id: MediaStreamId(60),
          maybe_max_layer: Some(2),
        })]
      );

      alice.connection_lost();
      alice.rejoin("fresh".into(), now);
      drain(&mut alice);
      let CallEvent::Joined {
        call_id,
        participant,
        participants,
        screen_audio_tracks,
        maybe_reconnect_grace_ms,
        maybe_max_simulcast_layers,
        ..
      } = joined(1, &[2])
      else {
        unreachable!()
      };
      let kept = CallEvent::Joined {
        call_id,
        participant,
        participants,
        video_tracks: vec![remote],
        screen_audio_tracks,
        maybe_reconnect_grace_ms,
        maybe_max_simulcast_layers,
      };
      alice.handle_signal(event(1, kept), now).unwrap();
      assert!(
        drain(&mut alice).contains(&EngineOutput::Signal(CallClientMessage::Subscribe {
          stream_id: MediaStreamId(60),
          maybe_max_layer: Some(2),
        }))
      );
    }

    #[test]
    fn layers_paused_before_the_loss_come_back_after_the_rejoin() {
      let now = Instant::now();
      let (mut alice, _bob) = secured_pair(now);
      let own = video_track(50, 1, 3);
      ask_for(&mut alice, &own);
      alice
        .handle_signal(event(6, CallEvent::VideoPublished { track: own.clone() }), now)
        .unwrap();
      // The uplink degrades before the network goes: the top layer is paused.
      let mut seq = 7;
      degrade_uplink(&mut alice, &mut seq, now);
      drain(&mut alice);
      let top = EncodedVideoFrame {
        layer: SimulcastLayer::new(2).unwrap(),
        ..video_frame()
      };
      assert_eq!(
        alice.send_video(MediaStreamId(50), &top, now),
        VideoSendOutcome::LayerPaused
      );

      alice.connection_lost();
      alice.rejoin("fresh".into(), now);
      drain(&mut alice);
      let CallEvent::Joined {
        call_id,
        participant,
        participants,
        screen_audio_tracks,
        maybe_reconnect_grace_ms,
        maybe_max_simulcast_layers,
        ..
      } = joined(1, &[2])
      else {
        unreachable!()
      };
      let kept = CallEvent::Joined {
        call_id,
        participant,
        participants,
        video_tracks: vec![own],
        screen_audio_tracks,
        maybe_reconnect_grace_ms,
        maybe_max_simulcast_layers,
      };
      alice.handle_signal(event(1, kept), now).unwrap();
      let outputs = drain(&mut alice);
      // A good network now: every layer goes out again, without waiting for the uplink estimate
      // to change, and the SFU stops holding receivers below them.
      assert!(
        outputs.contains(&EngineOutput::Event(CallEngineEvent::LayersNeeded {
          stream_id: MediaStreamId(50),
          layers: vec![0, 1, 2],
        })),
        "{outputs:?}"
      );
      assert!(
        outputs.contains(&EngineOutput::Signal(CallClientMessage::UplinkLayers {
          stream_id: MediaStreamId(50),
          layers: vec![0, 1, 2],
        })),
        "{outputs:?}"
      );
      // The SFU's resent `layers_needed` narrows it again.
      let needed = CallEvent::LayersNeeded {
        stream_id: MediaStreamId(50),
        layers: vec![0, 2],
      };
      alice.handle_signal(event(2, needed), now).unwrap();
      assert_eq!(
        drain(&mut alice),
        vec![EngineOutput::Event(CallEngineEvent::LayersNeeded {
          stream_id: MediaStreamId(50),
          layers: vec![0, 2],
        })]
      );
    }

    #[test]
    fn connection_lost_before_the_first_joined_joins_again() {
      let now = Instant::now();
      let mut alice = engine(1, now);
      alice.join("first".into());
      drain(&mut alice);
      alice.connection_lost();
      assert!(drain(&mut alice).is_empty());
      alice.rejoin("second".into(), now);
      assert!(matches!(
        alice.poll_output(),
        Some(EngineOutput::Signal(CallClientMessage::Join { token, .. })) if token == "second"
      ));
      alice.handle_signal(event(1, joined(1, &[])), now).unwrap();
      assert!(alice.is_in_call());
    }

    #[test]
    fn rejoining_another_call_is_rejected() {
      let now = Instant::now();
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_engine(1, &[], &gateway, None, now);
      alice.connection_lost();
      alice.rejoin("fresh".into(), now);
      let CallEvent::Joined {
        participant,
        participants,
        video_tracks,
        screen_audio_tracks,
        maybe_reconnect_grace_ms,
        maybe_max_simulcast_layers,
        ..
      } = joined(1, &[])
      else {
        unreachable!()
      };
      let other_call = CallEvent::Joined {
        call_id: CallId::from_snowflake(Snowflake::new(CALL + 1).unwrap()),
        participant,
        participants,
        video_tracks,
        screen_audio_tracks,
        maybe_reconnect_grace_ms,
        maybe_max_simulcast_layers,
      };
      assert!(matches!(
        alice.handle_signal(event(1, other_call), now),
        Err(CallEngineError::Protocol(_))
      ));
    }

    #[test]
    fn a_failed_attempt_goes_back_to_reconnecting_quietly() {
      let now = Instant::now();
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_engine(1, &[], &gateway, None, now);
      alice.connection_lost();
      alice.rejoin("first".into(), now);
      drain(&mut alice);
      alice.connection_lost();
      assert!(drain(&mut alice).is_empty(), "Reconnecting is reported once");
      alice.rejoin("second".into(), now);
      assert!(matches!(
        alice.poll_output(),
        Some(EngineOutput::Signal(CallClientMessage::Join { token, .. })) if token == "second"
      ));
    }

    #[test]
    fn other_devices_reconnecting_are_reported() {
      let now = Instant::now();
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_engine(1, &[2], &gateway, None, now);
      drain(&mut alice);
      alice
        .handle_signal(
          event(3, CallEvent::ParticipantReconnecting { device_id: device(2) }),
          now,
        )
        .unwrap();
      alice
        .handle_signal(
          event(4, CallEvent::ParticipantReconnected { device_id: device(2) }),
          now,
        )
        .unwrap();
      assert_eq!(
        drain(&mut alice),
        vec![
          EngineOutput::Event(CallEngineEvent::ParticipantReconnecting(device(2))),
          EngineOutput::Event(CallEngineEvent::ParticipantReconnected(device(2))),
        ]
      );
      assert_eq!(alice.participants().count(), 1);
    }
  }

  mod uplink {
    use super::*;

    #[test]
    fn lossy_uplink_stops_video_before_audio_and_raises_the_loss_for_fec() {
      let now = Instant::now();
      let (mut alice, _bob) = secured_pair(now);
      ask_for(&mut alice, &video_track(50, 1, 3));
      alice
        .handle_signal(
          event(
            6,
            CallEvent::VideoPublished {
              track: video_track(50, 1, 3),
            },
          ),
          now,
        )
        .unwrap();
      drain(&mut alice);
      // Two seconds of a link that loses every other datagram.
      let mut outputs = Vec::new();
      for round in 0..20u16 {
        let at = now + Duration::from_millis(100 * u64::from(round));
        for _ in 0..10 {
          alice.send_audio(&frame(), at);
        }
        drain(&mut alice);
        let arrivals_us = (0..10u64)
          .map(|i| (i % 2 == 0).then_some(u64::from(round) * 100_000 + i * 2_000))
          .collect();
        let feedback = TransportFeedback {
          base_seq: round * 10,
          arrivals_us,
        };
        alice
          .handle_signal(
            event(7 + u64::from(round), CallEvent::TransportFeedback { feedback }),
            at,
          )
          .unwrap();
        outputs.extend(drain(&mut alice));
      }
      assert!(
        outputs.contains(&EngineOutput::Event(CallEngineEvent::LayersNeeded {
          stream_id: MediaStreamId(50),
          layers: Vec::new(),
        }))
      );
      let max_loss = outputs
        .iter()
        .filter_map(|output| match output {
          EngineOutput::Event(CallEngineEvent::UplinkEstimate { loss_percent, .. }) => Some(*loss_percent),
          _ => None,
        })
        .max();
      assert_eq!(max_loss, Some(50));
      // Video is held back, audio still goes out.
      let later = now + Duration::from_secs(3);
      let video = EncodedVideoFrame {
        codec: VideoCodec::H264,
        layer: SimulcastLayer::BASE,
        timestamp: RtpTimestamp(0),
        is_keyframe: true,
        data: vec![1; 100],
      };
      alice.send_video(MediaStreamId(50), &video, later);
      assert!(drain(&mut alice).is_empty());
      alice.send_audio(&frame(), later);
      take_datagram(&mut alice);
    }

    #[test]
    fn the_sfu_hears_which_layers_the_uplink_carries_when_that_changes() {
      let now = Instant::now();
      let (mut alice, _bob) = secured_pair(now);
      ask_for(&mut alice, &video_track(50, 1, 3));
      alice
        .handle_signal(
          event(
            6,
            CallEvent::VideoPublished {
              track: video_track(50, 1, 3),
            },
          ),
          now,
        )
        .unwrap();
      // A new track is taken as carried whole: nothing to say yet.
      assert!(!drain(&mut alice).iter().any(is_uplink_layers));
      let mut seq = 7;
      let outputs = degrade_uplink(&mut alice, &mut seq, now);
      let announced: Vec<Vec<u8>> = outputs
        .iter()
        .filter_map(|output| match output {
          EngineOutput::Signal(CallClientMessage::UplinkLayers { stream_id, layers }) => {
            assert_eq!(*stream_id, MediaStreamId(50));
            Some(layers.clone())
          },
          _ => None,
        })
        .collect();
      // Told once per change, down to nothing on this link.
      assert_eq!(announced.last(), Some(&Vec::new()), "{announced:?}");
      let mut deduped = announced.clone();
      deduped.dedup();
      assert_eq!(deduped, announced);
    }

    /// Issue #939: the SFU forwards the top layer, which the uplink doesn't carry. Its receivers
    /// move to the highest layer the uplink carries, so that layer is encoded at once rather than
    /// after the SFU asked for it, with nothing sent meanwhile.
    #[test]
    fn the_highest_layer_carried_stands_in_for_the_one_the_sfu_forwards() {
      let now = Instant::now();
      let (mut alice, _bob) = secured_pair(now);
      ask_for(&mut alice, &video_track(50, 1, 3));
      alice
        .handle_signal(
          event(
            6,
            CallEvent::VideoPublished {
              track: video_track(50, 1, 3),
            },
          ),
          now,
        )
        .unwrap();
      let needed = CallEvent::LayersNeeded {
        stream_id: MediaStreamId(50),
        layers: vec![2],
      };
      alice.handle_signal(event(7, needed), now).unwrap();
      drain(&mut alice);
      let mut seq = 8;
      let mut outputs = degrade_uplink(&mut alice, &mut seq, now);
      // The uplink recovers: layers come back from the lowest.
      outputs.extend(recover_uplink(&mut alice, &mut seq, now + Duration::from_secs(2)));
      let carried: Vec<Vec<u8>> = outputs
        .iter()
        .filter_map(|output| match output {
          EngineOutput::Signal(CallClientMessage::UplinkLayers { layers, .. }) => Some(layers.clone()),
          _ => None,
        })
        .collect();
      let encoded: Vec<Vec<u8>> = outputs
        .iter()
        .filter_map(|output| match output {
          EngineOutput::Event(CallEngineEvent::LayersNeeded { layers, .. }) => Some(layers.clone()),
          _ => None,
        })
        .collect();
      assert!(
        carried
          .iter()
          .any(|layers| layers.contains(&0) && !layers.contains(&2)),
        "{carried:?}"
      );
      // Whatever the uplink carries, its highest layer is encoded; nothing only while it carries
      // nothing.
      let tops: Vec<Vec<u8>> = carried
        .iter()
        .map(|layers| layers.iter().max().map(|top| vec![*top]).unwrap_or_default())
        .collect();
      assert_eq!(encoded, tops, "carried: {carried:?}");
    }

    /// Issue #939: the driver of an adaptive camera hears the bitrate per layer whenever it
    /// changes: on the way back up, the top sent layer at a share of its own, the ones below
    /// whole. A camera the app encodes itself hears none.
    #[test]
    fn an_adaptive_camera_hears_its_layer_bitrates() {
      let bitrates_heard = |is_adaptive: bool| {
        let now = Instant::now();
        let (mut alice, _bob) = secured_pair(now);
        let track = video_track(50, 1, 3);
        let publish = if is_adaptive {
          CallEngine::publish_adaptive_video
        } else {
          CallEngine::publish_video
        };
        publish(
          &mut alice,
          VideoPublicationId(50),
          track.kind.clone(),
          track.codec.clone(),
          track.layers.clone(),
        );
        alice
          .handle_signal(event(6, CallEvent::VideoPublished { track }), now)
          .unwrap();
        drain(&mut alice);
        let mut seq = 7;
        let mut outputs = degrade_uplink(&mut alice, &mut seq, now);
        outputs.extend(recover_uplink(&mut alice, &mut seq, now + Duration::from_secs(2)));
        outputs
          .into_iter()
          .filter_map(|output| match output {
            EngineOutput::Event(CallEngineEvent::LayerBitrates { bitrates, .. }) => Some(bitrates),
            _ => None,
          })
          .collect::<Vec<_>>()
      };
      let heard = bitrates_heard(true);
      assert!(
        heard
          .iter()
          .any(|bitrates| bitrates.iter().any(|(layer, bps)| *layer == 1 && *bps < 500_000)),
        "{heard:?}"
      );
      // The top layer is back, at half its bitrate.
      assert_eq!(
        heard.last(),
        Some(&vec![(0, 150_000), (1, 500_000), (2, 750_000)]),
        "{heard:?}"
      );
      assert!(bitrates_heard(false).is_empty());
    }

    /// Issue #935: a call that sent only sound through a lossy moment left an estimate cut below
    /// every camera layer. A camera turned on then starts with every layer and sends, instead of
    /// having them all held back (nothing was encoded, and the camera was turned off).
    #[test]
    fn a_camera_after_a_lossy_moment_of_sound_starts_with_every_layer() {
      let now = Instant::now();
      let (mut alice, _bob) = secured_pair(now);
      let mut seq = 6;
      let cut_to = degrade_uplink(&mut alice, &mut seq, now)
        .iter()
        .rev()
        .find_map(|output| match output {
          EngineOutput::Event(CallEngineEvent::UplinkEstimate { bps, .. }) => Some(*bps),
          _ => None,
        })
        .unwrap();
      assert!(cut_to < UplinkPolicy::default().audio_reserve_bps, "{cut_to}");

      let later = now + Duration::from_secs(3);
      ask_for(&mut alice, &video_track(50, 1, 3));
      let published = CallEvent::VideoPublished {
        track: video_track(50, 1, 3),
      };
      alice.handle_signal(event(seq, published), later).unwrap();
      seq += 1;
      drain(&mut alice);
      // The next feedback, without loss, reports sound sent before the camera.
      for _ in 0..10 {
        alice.send_audio(&frame(), later);
      }
      drain(&mut alice);
      let feedback = TransportFeedback {
        base_seq: 200,
        arrivals_us: (0..10u64).map(|i| Some(3_000_000 + i * 2_000)).collect(),
      };
      let at = later + Duration::from_millis(100);
      alice
        .handle_signal(event(seq, CallEvent::TransportFeedback { feedback }), at)
        .unwrap();
      let outputs = drain(&mut alice);
      assert!(
        !outputs
          .iter()
          .any(|output| matches!(output, EngineOutput::Event(CallEngineEvent::LayersNeeded { .. }))),
        "{outputs:?}"
      );
      let top_layer = EncodedVideoFrame {
        layer: SimulcastLayer::new(2).unwrap(),
        ..video_frame()
      };
      assert_eq!(
        alice.send_video(MediaStreamId(50), &top_layer, at),
        VideoSendOutcome::Sent
      );
    }

    #[test]
    fn layers_the_sfu_needs_are_narrowed_to_what_the_uplink_carries() {
      let now = Instant::now();
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_engine(1, &[], &gateway, None, now);
      ask_for(&mut alice, &video_track(50, 1, 3));
      alice
        .handle_signal(
          event(
            3,
            CallEvent::VideoPublished {
              track: video_track(50, 1, 3),
            },
          ),
          now,
        )
        .unwrap();
      drain(&mut alice);
      let needed = CallEvent::LayersNeeded {
        stream_id: MediaStreamId(50),
        layers: vec![0, 1],
      };
      alice.handle_signal(event(4, needed), now).unwrap();
      assert_eq!(
        drain(&mut alice),
        vec![EngineOutput::Event(CallEngineEvent::LayersNeeded {
          stream_id: MediaStreamId(50),
          layers: vec![0, 1],
        })]
      );
      // The same again changes nothing for the driver.
      let needed = CallEvent::LayersNeeded {
        stream_id: MediaStreamId(50),
        layers: vec![0, 1],
      };
      alice.handle_signal(event(5, needed), now).unwrap();
      assert!(drain(&mut alice).is_empty());
    }
  }

  /// Issue #929: every answer of the SFU belongs to exactly the publication that asked for it, and
  /// one whose track was stopped before it came is unpublished at once.
  mod own_publications {
    use super::*;

    const PUBLICATION: VideoPublicationId = VideoPublicationId(7);

    #[test]
    fn a_track_stopped_before_its_answer_is_unpublished_when_it_comes() {
      let now = Instant::now();
      let mut alice = engine_joined(1, now);
      alice.publish_video(PUBLICATION, VideoTrackKind::Camera, VideoCodec::H264, layers(1));
      alice.withdraw_video(PUBLICATION);
      drain(&mut alice);
      answer(&mut alice, 2, own_track(50, Some(PUBLICATION)), now);
      // No sender, no event: the SFU drops the track before anyone sees a black tile.
      assert_eq!(drain(&mut alice), vec![unpublish(50)]);
      assert_eq!(
        alice.send_video(MediaStreamId(50), &video_frame(), now),
        VideoSendOutcome::NotPublished
      );
      // Its announcement to everyone comes back like any other.
      let gone = CallEvent::VideoUnpublished {
        stream_id: MediaStreamId(50),
      };
      alice.handle_signal(event(3, gone), now).unwrap();
      assert!(
        !drain(&mut alice)
          .iter()
          .any(|output| matches!(output, EngineOutput::Signal(_)))
      );
    }

    #[test]
    fn a_camera_stopped_and_started_again_gets_the_answer_to_its_own_request() {
      for is_echoed in [true, false] {
        let now = Instant::now();
        let mut alice = engine_joined(1, now);
        let (first, second) = (VideoPublicationId(1), VideoPublicationId(2));
        alice.publish_video(first, VideoTrackKind::Camera, VideoCodec::H264, layers(1));
        alice.withdraw_video(first);
        alice.publish_video(second, VideoTrackKind::Camera, VideoCodec::H264, layers(1));
        // A screen share asked for in between takes no camera's answer.
        let share = VideoPublicationId(3);
        alice.publish_video(share, VideoTrackKind::Screen, VideoCodec::H264, layers(1));
        drain(&mut alice);
        // An SFU that echoes no ids answers in order: the first camera answer is the first's.
        let echo = |publication: VideoPublicationId| is_echoed.then_some(publication);
        let screen = VideoTrack {
          kind: VideoTrackKind::Screen,
          ..own_track(52, echo(share))
        };
        answer(&mut alice, 2, screen, now);
        answer(&mut alice, 3, own_track(50, echo(first)), now);
        answer(&mut alice, 4, own_track(51, echo(second)), now);
        let outputs = drain(&mut alice);
        assert!(outputs.contains(&unpublish(50)), "{outputs:?}");
        let confirmed: Vec<(u32, Option<VideoPublicationId>)> = outputs
          .iter()
          .filter_map(|output| match output {
            EngineOutput::Event(CallEngineEvent::VideoPublished(track)) => {
              Some((track.stream_id.0, track.maybe_publication_id))
            },
            _ => None,
          })
          .collect();
        // The driver finds its track by the id also when the SFU didn't echo it.
        assert_eq!(
          confirmed,
          vec![(52, Some(share)), (51, Some(second))],
          "echoed: {is_echoed}"
        );
        assert_eq!(outputs.iter().filter(|output| is_unpublish(output)).count(), 1);
      }
    }

    #[test]
    fn an_echoed_id_decides_even_when_answers_come_out_of_order() {
      let now = Instant::now();
      let mut alice = engine_joined(1, now);
      let (first, second) = (VideoPublicationId(1), VideoPublicationId(2));
      alice.publish_video(first, VideoTrackKind::Camera, VideoCodec::H264, layers(1));
      alice.publish_video(second, VideoTrackKind::Camera, VideoCodec::H264, layers(1));
      alice.withdraw_video(first);
      drain(&mut alice);
      answer(&mut alice, 2, own_track(51, Some(second)), now);
      answer(&mut alice, 3, own_track(50, Some(first)), now);
      let outputs = drain(&mut alice);
      assert!(outputs.contains(&unpublish(50)), "{outputs:?}");
      assert!(!outputs.contains(&unpublish(51)), "{outputs:?}");
      assert_ne!(
        alice.send_video(MediaStreamId(51), &video_frame(), now),
        VideoSendOutcome::NotPublished
      );
    }

    #[test]
    fn own_tracks_nobody_asked_for_are_unpublished() {
      let now = Instant::now();
      let mut alice = engine_joined(1, now);
      drain(&mut alice);
      answer(&mut alice, 2, own_track(50, None), now);
      answer(&mut alice, 3, own_track(51, Some(PUBLICATION)), now);
      assert_eq!(drain(&mut alice), vec![unpublish(50), unpublish(51)]);
      // An id the SFU echoes for another kind than asked for answers nothing either.
      alice.publish_video(PUBLICATION, VideoTrackKind::Screen, VideoCodec::H264, layers(1));
      drain(&mut alice);
      answer(&mut alice, 4, own_track(52, Some(PUBLICATION)), now);
      assert_eq!(drain(&mut alice), vec![unpublish(52)]);
    }

    #[test]
    fn a_confirmed_track_is_withdrawn_by_its_publication_once() {
      let now = Instant::now();
      let mut alice = engine_joined(1, now);
      alice.publish_video(PUBLICATION, VideoTrackKind::Camera, VideoCodec::H264, layers(1));
      answer(&mut alice, 2, own_track(50, Some(PUBLICATION)), now);
      drain(&mut alice);
      alice.withdraw_video(PUBLICATION);
      alice.withdraw_video(PUBLICATION);
      assert_eq!(drain(&mut alice), vec![unpublish(50)]);
      // An id in use is not taken twice: its answers would be ambiguous.
      alice.publish_video(PUBLICATION, VideoTrackKind::Camera, VideoCodec::H264, layers(1));
      alice.publish_video(PUBLICATION, VideoTrackKind::Camera, VideoCodec::H264, layers(1));
      assert_eq!(drain(&mut alice).len(), 1);
    }

    /// The issue's long window: the camera toggled while the connection is gone. Only the request
    /// still wanted goes out again, and a stopped track the SFU kept with the slot (its answer was
    /// lost with the connection) is unpublished, with or without the SFU echoing ids.
    #[test]
    fn two_toggles_while_reconnecting_leave_one_camera() {
      for is_echoed in [true, false] {
        let now = Instant::now();
        let mut alice = engine_joined(1, now);
        let (first, second, third) = (
          VideoPublicationId(1),
          VideoPublicationId(2),
          VideoPublicationId(3),
        );
        // Asked for just before the loss; the SFU answered, but the answer was lost.
        alice.publish_video(first, VideoTrackKind::Camera, VideoCodec::H264, layers(1));
        alice.connection_lost();
        // Off and on, twice, while away.
        alice.withdraw_video(first);
        alice.publish_video(second, VideoTrackKind::Camera, VideoCodec::H264, layers(1));
        alice.withdraw_video(second);
        alice.publish_video(third, VideoTrackKind::Camera, VideoCodec::H264, layers(1));
        alice.rejoin("fresh".into(), now);
        drain(&mut alice);

        let kept = own_track(50, is_echoed.then_some(first));
        alice
          .handle_signal(event(1, joined_with(1, vec![kept])), now)
          .unwrap();
        let outputs = drain(&mut alice);
        let requests: Vec<Option<VideoPublicationId>> = outputs
          .iter()
          .filter_map(|output| match output {
            EngineOutput::Signal(CallClientMessage::PublishVideo {
              maybe_publication_id, ..
            }) => Some(*maybe_publication_id),
            _ => None,
          })
          .collect();
        assert_eq!(requests, vec![Some(third)], "echoed: {is_echoed}");
        assert!(outputs.contains(&unpublish(50)), "{outputs:?}");
        assert!(
          !outputs
            .iter()
            .any(|output| matches!(output, EngineOutput::Event(CallEngineEvent::VideoPublished(_)))),
          "{outputs:?}"
        );

        answer(&mut alice, 2, own_track(51, is_echoed.then_some(third)), now);
        let outputs = drain(&mut alice);
        assert!(
          outputs.iter().any(|output| matches!(
            output,
            EngineOutput::Event(CallEngineEvent::VideoPublished(track))
              if track.stream_id == MediaStreamId(51) && track.maybe_publication_id == Some(third)
          )),
          "{outputs:?}"
        );
      }
    }

    #[test]
    fn a_driver_follows_only_its_own_publication() {
      let own = device(1);
      let mut camera = OwnPublication::new(PUBLICATION);
      let mut other = own_track(60, Some(PUBLICATION));
      other.device_id = device(2);
      // Another device's track with the same number is not ours.
      assert!(!camera.follow(own, &CallEngineEvent::VideoPublished(other)));
      assert!(!camera.follow(
        own,
        &CallEngineEvent::VideoPublished(own_track(49, Some(VideoPublicationId(8))))
      ));
      assert_eq!(camera.maybe_stream_id, None);
      assert!(camera.follow(
        own,
        &CallEngineEvent::VideoPublished(own_track(50, Some(PUBLICATION)))
      ));
      assert_eq!(camera.maybe_stream_id, Some(MediaStreamId(50)));
      assert!(!camera.follow(own, &CallEngineEvent::VideoUnpublished(MediaStreamId(60))));
      assert_eq!(camera.maybe_stream_id, Some(MediaStreamId(50)));
      // Dropped by the SFU (a rejoin without the slot): the engine's renewed request is followed.
      assert!(!camera.follow(own, &CallEngineEvent::VideoUnpublished(MediaStreamId(50))));
      assert_eq!(camera.maybe_stream_id, None);
      assert!(camera.follow(
        own,
        &CallEngineEvent::VideoPublished(own_track(51, Some(PUBLICATION)))
      ));
      assert_eq!(camera.maybe_stream_id, Some(MediaStreamId(51)));
    }

    fn answer(engine: &mut CallEngine, seq: u64, track: VideoTrack, now: Instant) {
      engine
        .handle_signal(event(seq, CallEvent::VideoPublished { track }), now)
        .unwrap();
    }

    fn own_track(stream: u32, maybe_publication_id: Option<VideoPublicationId>) -> VideoTrack {
      VideoTrack {
        maybe_publication_id,
        ..video_track(stream, 1, 1)
      }
    }

    fn joined_with(id: i64, video_tracks: Vec<VideoTrack>) -> CallEvent {
      let CallEvent::Joined {
        call_id,
        participant,
        participants,
        screen_audio_tracks,
        maybe_reconnect_grace_ms,
        maybe_max_simulcast_layers,
        ..
      } = joined(id, &[])
      else {
        unreachable!()
      };
      CallEvent::Joined {
        call_id,
        participant,
        participants,
        video_tracks,
        screen_audio_tracks,
        maybe_reconnect_grace_ms,
        maybe_max_simulcast_layers,
      }
    }

    fn unpublish(stream: u32) -> EngineOutput {
      EngineOutput::Signal(CallClientMessage::UnpublishVideo {
        stream_id: MediaStreamId(stream),
      })
    }

    fn is_unpublish(output: &EngineOutput) -> bool {
      matches!(
        output,
        EngineOutput::Signal(CallClientMessage::UnpublishVideo { .. })
      )
    }
  }

  /// #978: how many layers a track goes out with depends on what the SFU takes.
  mod layer_limit {
    use super::*;

    const PUBLICATION: VideoPublicationId = VideoPublicationId(7);

    #[test]
    fn the_limit_is_the_sfus_and_three_from_an_sfu_that_says_none() {
      let now = Instant::now();
      let mut alice = engine(1, now);
      // Before the SFU's answer: the most there are.
      assert_eq!(alice.simulcast_layer_limit(), MAX_SIMULCAST_LAYERS);
      alice.join("t".into());
      alice.handle_signal(event(1, joined_by(1, None)), now).unwrap();
      assert_eq!(alice.simulcast_layer_limit(), LEGACY_SIMULCAST_LAYERS);
      let mut bob = engine(2, now);
      bob.join("t".into());
      bob.handle_signal(event(1, joined_by(2, Some(4))), now).unwrap();
      assert_eq!(bob.simulcast_layer_limit(), 4);
      // A newer SFU that takes more: no more than this version plans.
      let mut carol = engine(3, now);
      carol.join("t".into());
      carol.handle_signal(event(1, joined_by(3, Some(9))), now).unwrap();
      assert_eq!(carol.simulcast_layer_limit(), MAX_SIMULCAST_LAYERS);
    }

    /// A camera started while joining is published once the SFU said how many layers it takes.
    #[test]
    fn a_publication_asked_for_while_joining_goes_out_with_joined() {
      let now = Instant::now();
      let mut alice = engine(1, now);
      alice.join("t".into());
      drain(&mut alice);
      alice.publish_video(PUBLICATION, VideoTrackKind::Camera, VideoCodec::H264, layers(4));
      // Withdrawn before the answer: never asked for.
      alice.publish_video(
        VideoPublicationId(8),
        VideoTrackKind::Screen,
        VideoCodec::H264,
        layers(1),
      );
      alice.withdraw_video(VideoPublicationId(8));
      assert!(published_layers(&drain(&mut alice)).is_empty());
      alice.handle_signal(event(1, joined_by(1, Some(4))), now).unwrap();
      assert_eq!(published_layers(&drain(&mut alice)), vec![4]);
    }

    /// An SFU from before #978 takes three layers: a four-layer track goes out with its lowest
    /// three, and the driver pauses its top one (a camera planned before the SFU answered).
    #[test]
    fn an_sfu_from_before_gets_three_layers_and_the_top_one_pauses() {
      let now = Instant::now();
      let mut alice = engine(1, now);
      alice.join("t".into());
      alice.handle_signal(event(1, joined_by(1, None)), now).unwrap();
      drain(&mut alice);
      alice.publish_adaptive_video(PUBLICATION, VideoTrackKind::Camera, VideoCodec::H264, layers(4));
      assert_eq!(published_layers(&drain(&mut alice)), vec![3]);
      let track = VideoTrack {
        maybe_publication_id: Some(PUBLICATION),
        ..video_track(50, 1, 3)
      };
      alice
        .handle_signal(event(2, CallEvent::VideoPublished { track }), now)
        .unwrap();
      let outputs = drain(&mut alice);
      assert!(
        outputs.contains(&EngineOutput::Event(CallEngineEvent::LayersNeeded {
          stream_id: MediaStreamId(50),
          layers: vec![0, 1, 2],
        })),
        "{outputs:?}"
      );
      let top = EncodedVideoFrame {
        layer: SimulcastLayer::new(3).unwrap(),
        ..video_frame()
      };
      assert_eq!(
        alice.send_video(MediaStreamId(50), &top, now),
        VideoSendOutcome::LayerPaused
      );
    }

    /// An SFU that takes four gets all four.
    #[test]
    fn a_current_sfu_gets_all_four_layers() {
      let now = Instant::now();
      let mut alice = engine_joined(1, now);
      drain(&mut alice);
      alice.publish_adaptive_video(PUBLICATION, VideoTrackKind::Camera, VideoCodec::H264, layers(4));
      assert_eq!(published_layers(&drain(&mut alice)), vec![4]);
      let track = VideoTrack {
        maybe_publication_id: Some(PUBLICATION),
        ..video_track(50, 1, 4)
      };
      alice
        .handle_signal(event(2, CallEvent::VideoPublished { track }), now)
        .unwrap();
      let outputs = drain(&mut alice);
      assert!(
        !outputs
          .iter()
          .any(|output| matches!(output, EngineOutput::Event(CallEngineEvent::LayersNeeded { .. }))),
        "{outputs:?}"
      );
    }

    /// The number of layers of each publication asked for in `outputs`.
    fn published_layers(outputs: &[EngineOutput]) -> Vec<usize> {
      outputs
        .iter()
        .filter_map(|output| match output {
          EngineOutput::Signal(CallClientMessage::PublishVideo { layers, .. }) => Some(layers.len()),
          _ => None,
        })
        .collect()
    }

    fn joined_by(id: i64, maybe_max_simulcast_layers: Option<u8>) -> CallEvent {
      let CallEvent::Joined {
        call_id,
        participant,
        participants,
        video_tracks,
        screen_audio_tracks,
        maybe_reconnect_grace_ms,
        ..
      } = joined(id, &[])
      else {
        unreachable!()
      };
      CallEvent::Joined {
        call_id,
        participant,
        participants,
        video_tracks,
        screen_audio_tracks,
        maybe_reconnect_grace_ms,
        maybe_max_simulcast_layers,
      }
    }
  }

  mod trust {
    use super::*;

    #[test]
    fn proposals_outside_the_trusted_roster_are_rejected() {
      let now = Instant::now();
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_engine(1, &[], &gateway, None, now);
      alice.set_trusted_roster([device(2)]);
      let mut mallory = joined_engine(3, &[], &gateway, None, now);
      let (key_package, _) = key_package_of(&mut mallory);
      let proposals = propose(&gateway, 3, &key_package);
      let result = alice.handle_signal(event(3, dave_proposals(0, proposals, &[1, 3], Vec::new())), now);
      assert!(matches!(result, Err(CallEngineError::Dave(_))));
    }

    #[test]
    fn missing_roster_and_authenticator_are_reported() {
      let now = Instant::now();
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_engine(1, &[], &gateway, None, now);
      let mut bob = joined_engine(2, &[], &gateway, None, now);
      let (key_package, maybe_binding) = key_package_of(&mut bob);
      assert_eq!(maybe_binding, None);
      let proposals = propose(&gateway, 2, &key_package);
      alice
        .handle_signal(event(3, dave_proposals(0, proposals, &[1, 2], Vec::new())), now)
        .unwrap();
      let outputs = drain(&mut alice);
      assert!(outputs.contains(&EngineOutput::Event(CallEngineEvent::UntrustedRoster)));
      let commit = outputs
        .into_iter()
        .find_map(|output| match output {
          EngineOutput::Signal(CallClientMessage::DaveCommit { commit, .. }) => Some(commit),
          _ => None,
        })
        .unwrap();
      alice
        .handle_signal(event(4, CallEvent::DaveAnnounceCommit { epoch: 0, commit }), now)
        .unwrap();
      assert!(drain(&mut alice).contains(&EngineOutput::Event(CallEngineEvent::UnauthenticatedKeys)));
    }

    #[test]
    fn swapped_dave_key_is_not_committed() {
      let now = Instant::now();
      let authenticators = authenticators(&[1, 2]);
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_engine(1, &[], &gateway, Some(&authenticators[&1]), now);
      alice.set_trusted_roster([device(2)]);
      let mut bob = joined_engine(2, &[1], &gateway, Some(&authenticators[&2]), now);
      let (_, bob_binding) = key_package_of(&mut bob);
      // The SFU relays Bob's genuine binding but proposes a key package it made itself for
      // device 2 (its own DAVE key).
      let mut impostor = joined_engine(2, &[1], &gateway, None, now);
      let (impostor_key_package, _) = key_package_of(&mut impostor);
      let proposals = propose(&gateway, 2, &impostor_key_package);
      let bindings = vec![binding(2, bob_binding.unwrap())];
      let result = alice.handle_signal(event(3, dave_proposals(0, proposals, &[1, 2], bindings)), now);
      assert!(matches!(result, Err(CallEngineError::UnverifiedMember(member)) if member == device(2)));
      assert!(
        drain(&mut alice)
          .iter()
          .all(|output| !matches!(output, EngineOutput::Signal(CallClientMessage::DaveCommit { .. }))),
        "no commit for an unverified key"
      );
    }

    #[test]
    fn binding_of_another_device_is_rejected() {
      let now = Instant::now();
      let authenticators = authenticators(&[1, 2, 3]);
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_engine(1, &[], &gateway, Some(&authenticators[&1]), now);
      let mut mallory = joined_engine(3, &[], &gateway, Some(&authenticators[&3]), now);
      let (_, mallory_binding) = key_package_of(&mut mallory);
      // Mallory's binding relayed as if it were device 2's.
      let bindings = vec![binding(2, mallory_binding.unwrap())];
      let result = alice.handle_signal(event(3, dave_proposals(0, Vec::new(), &[1, 2], bindings)), now);
      assert!(matches!(result, Err(CallEngineError::CallKey(_))));
    }

    #[test]
    fn welcome_without_verified_members_stops_media() {
      let now = Instant::now();
      let authenticators = authenticators(&[1, 2]);
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_engine(1, &[], &gateway, Some(&authenticators[&1]), now);
      alice.set_trusted_roster([device(2)]);
      let mut bob = joined_engine(2, &[1], &gateway, Some(&authenticators[&2]), now);
      let (key_package, bob_binding) = key_package_of(&mut bob);
      let proposals = propose(&gateway, 2, &key_package);
      let bindings = vec![binding(2, bob_binding.unwrap())];
      alice
        .handle_signal(event(3, dave_proposals(0, proposals, &[1, 2], bindings)), now)
        .unwrap();
      let (_, welcome) = take_commit(&mut alice);
      // The SFU withholds Alice's binding from Bob.
      let welcome = CallEvent::DaveWelcome {
        welcome,
        key_bindings: Vec::new(),
      };
      let result = bob.handle_signal(event(3, welcome), now);
      assert!(matches!(result, Err(CallEngineError::UnverifiedMember(member)) if member == device(1)));
      assert!(!bob.is_secured());
      bob.send_audio(&frame(), now);
      assert!(
        drain(&mut bob)
          .iter()
          .all(|output| !matches!(output, EngineOutput::Datagram(_)))
      );
    }

    fn propose(gateway: &DaveExternalSender, id: u64, key_package: &[u8]) -> Vec<u8> {
      gateway
        .propose(
          DaveCallId::from_call(call()),
          DaveEpoch(0),
          &[(DaveParticipantId(id), key_package)],
          &[],
        )
        .unwrap()
    }
  }

  /// Alice and Bob in one DAVE group, as the SFU's coordinator would set it up, with call key
  /// bindings verified on both sides.
  /// Two seconds of an uplink that loses every other datagram, with feedback events from `seq`
  /// on; returns everything the engine put out meanwhile.
  fn degrade_uplink(engine: &mut CallEngine, seq: &mut u64, now: Instant) -> Vec<EngineOutput> {
    let mut outputs = Vec::new();
    for round in 0..20u16 {
      let at = now + Duration::from_millis(100 * u64::from(round));
      for _ in 0..10 {
        engine.send_audio(&frame(), at);
      }
      drain(engine);
      let arrivals_us = (0..10u64)
        .map(|i| (i % 2 == 0).then_some(u64::from(round) * 100_000 + i * 2_000))
        .collect();
      let feedback = TransportFeedback {
        base_seq: round * 10,
        arrivals_us,
      };
      engine
        .handle_signal(event(*seq, CallEvent::TransportFeedback { feedback }), at)
        .unwrap();
      *seq += 1;
      outputs.extend(drain(engine));
    }
    outputs
  }

  /// Feedback of an uplink that delivers everything, after [`degrade_uplink`] (which sent 200
  /// datagrams), from `from` on, until it carries the top layer again (at most a minute).
  fn recover_uplink(engine: &mut CallEngine, seq: &mut u64, from: Instant) -> Vec<EngineOutput> {
    let mut outputs = Vec::new();
    for round in 0..600u16 {
      let at = from + Duration::from_millis(100 * u64::from(round));
      for _ in 0..10 {
        engine.send_audio(&frame(), at);
      }
      drain(engine);
      let arrivals_us = (0..10u64)
        .map(|i| Some(2_000_000 + u64::from(round) * 100_000 + i * 2_000))
        .collect();
      let feedback = TransportFeedback {
        base_seq: 200u16.wrapping_add(round.wrapping_mul(10)),
        arrivals_us,
      };
      engine
        .handle_signal(event(*seq, CallEvent::TransportFeedback { feedback }), at)
        .unwrap();
      *seq += 1;
      let round_outputs = drain(engine);
      let is_whole = round_outputs.iter().any(|output| {
        matches!(
          output,
          EngineOutput::Signal(CallClientMessage::UplinkLayers { layers, .. }) if layers.contains(&2)
        )
      });
      outputs.extend(round_outputs);
      if is_whole {
        break;
      }
    }
    outputs
  }

  fn is_uplink_layers(output: &EngineOutput) -> bool {
    matches!(
      output,
      EngineOutput::Signal(CallClientMessage::UplinkLayers { .. })
    )
  }

  fn video_frame() -> EncodedVideoFrame {
    EncodedVideoFrame {
      codec: VideoCodec::H264,
      layer: SimulcastLayer::BASE,
      timestamp: RtpTimestamp(0),
      is_keyframe: true,
      data: vec![1; 100],
    }
  }

  fn secured_pair(now: Instant) -> (CallEngine, CallEngine) {
    let authenticators = authenticators(&[1, 2]);
    let gateway = DaveExternalSender::new().unwrap();
    let mut alice = joined_engine(1, &[], &gateway, Some(&authenticators[&1]), now);
    let mut bob = joined_engine(2, &[1], &gateway, Some(&authenticators[&2]), now);
    let (_, alice_binding) = key_package_of(&mut alice);
    alice
      .handle_signal(
        event(
          3,
          CallEvent::ParticipantJoined {
            participant: participant(2),
          },
        ),
        now,
      )
      .unwrap();
    alice.set_trusted_roster([device(2)]);
    bob.set_trusted_roster([device(1)]);
    let (key_package, bob_binding) = key_package_of(&mut bob);
    let proposals = gateway
      .propose(
        DaveCallId::from_call(call()),
        DaveEpoch(0),
        &[(DaveParticipantId(2), &key_package)],
        &[],
      )
      .unwrap();
    let bindings = vec![binding(2, bob_binding.unwrap())];
    alice
      .handle_signal(event(4, dave_proposals(0, proposals, &[1, 2], bindings)), now)
      .unwrap();
    let (commit, welcome) = take_commit(&mut alice);
    alice
      .handle_signal(event(5, CallEvent::DaveAnnounceCommit { epoch: 0, commit }), now)
      .unwrap();
    let welcome = CallEvent::DaveWelcome {
      welcome,
      key_bindings: vec![binding(1, alice_binding.unwrap())],
    };
    bob.handle_signal(event(3, welcome), now).unwrap();
    assert!(alice.is_secured() && bob.is_secured());
    drain(&mut alice);
    drain(&mut bob);
    (alice, bob)
  }

  /// An engine that joined (events 1 and 2: joined, external sender) with `others` present.
  fn joined_engine(
    id: i64,
    others: &[i64],
    gateway: &DaveExternalSender,
    maybe_authenticator: Option<&Arc<dyn CallKeyAuthenticator>>,
    now: Instant,
  ) -> CallEngine {
    let mut engine = engine(id, now);
    if let Some(authenticator) = maybe_authenticator {
      engine.set_authenticator(Arc::clone(authenticator));
    }
    engine.join("t".into());
    engine.handle_signal(event(1, joined(id, others)), now).unwrap();
    let package = Base64Bytes::new(gateway.package().unwrap());
    engine
      .handle_signal(event(2, CallEvent::DaveExternalSender { package }), now)
      .unwrap();
    engine
  }

  /// Device identities (one user each) whose certificates every authenticator trusts.
  fn authenticators(ids: &[i64]) -> HashMap<i64, Arc<dyn CallKeyAuthenticator>> {
    use rand_core::OsRng;
    use teamcraft_crypto::identity::{DeviceKeyPair, IdentityKeyPair};
    use teamcraft_crypto::ids::{DeviceId as CryptoDeviceId, UserId as CryptoUserId};

    let mut key_pairs = Vec::new();
    let mut verified = HashMap::new();
    for &id in ids {
      let identity = IdentityKeyPair::generate(&mut OsRng);
      let user = CryptoUserId::new(u64::try_from(1000 + id).unwrap());
      let key_pair =
        DeviceKeyPair::generate(user, CryptoDeviceId::new(u64::try_from(id).unwrap()), &mut OsRng);
      let certificate = identity.certify_device(user, &key_pair.public_keys()).unwrap();
      verified.insert(device(id), certificate.verify(&identity.public_key()).unwrap());
      key_pairs.push((id, key_pair));
    }
    key_pairs
      .into_iter()
      .map(|(id, key_pair)| {
        let authenticator: Arc<dyn CallKeyAuthenticator> = Arc::new(DeviceCallKeyAuthenticator::new(
          Arc::new(key_pair),
          verified.clone(),
        ));
        (id, authenticator)
      })
      .collect()
  }

  fn key_package_of(engine: &mut CallEngine) -> (Vec<u8>, Option<Vec<u8>>) {
    drain(engine)
      .into_iter()
      .find_map(|output| match output {
        EngineOutput::Signal(CallClientMessage::DaveKeyPackage {
          key_package,
          maybe_key_binding,
        }) => Some((
          key_package.into_vec(),
          maybe_key_binding.map(Base64Bytes::into_vec),
        )),
        _ => None,
      })
      .unwrap()
  }

  fn take_commit(engine: &mut CallEngine) -> (Base64Bytes, Base64Bytes) {
    drain(engine)
      .into_iter()
      .find_map(|output| match output {
        EngineOutput::Signal(CallClientMessage::DaveCommit {
          commit,
          maybe_welcome,
          ..
        }) => Some((commit, maybe_welcome.unwrap())),
        _ => None,
      })
      .unwrap()
  }

  fn binding(id: i64, bytes: Vec<u8>) -> DaveKeyBinding {
    DaveKeyBinding {
      device_id: device(id),
      binding: Base64Bytes::new(bytes),
    }
  }

  fn take_datagram(engine: &mut CallEngine) -> Vec<u8> {
    drain(engine)
      .into_iter()
      .find_map(|output| match output {
        EngineOutput::Datagram(bytes) => Some(bytes),
        _ => None,
      })
      .unwrap()
  }

  fn drain(engine: &mut CallEngine) -> Vec<EngineOutput> {
    std::iter::from_fn(|| engine.poll_output()).collect()
  }

  fn engine(id: i64, now: Instant) -> CallEngine {
    CallEngine::new(
      CallEngineConfig {
        user_id: UserId::from_snowflake(Snowflake::new(1000 + id).unwrap()),
        device_id: device(id),
        capabilities: MediaCapabilities::default(),
        bwe: BweConfig::default(),
      },
      now,
    )
  }

  fn engine_joined(id: i64, now: Instant) -> CallEngine {
    let mut engine = engine(id, now);
    engine.join("t".into());
    engine.handle_signal(event(1, joined(id, &[])), now).unwrap();
    engine
  }

  fn joined(id: i64, others: &[i64]) -> CallEvent {
    CallEvent::Joined {
      call_id: call(),
      participant: participant(id),
      participants: others.iter().map(|other| participant(*other)).collect(),
      video_tracks: Vec::new(),
      screen_audio_tracks: Vec::new(),
      maybe_reconnect_grace_ms: Some(30_000),
      maybe_max_simulcast_layers: Some(MAX_SIMULCAST_LAYERS),
    }
  }

  /// A track of `owner` with `layer_count` layers on `stream`.
  fn video_track(stream: u32, owner: i64, layer_count: u8) -> VideoTrack {
    VideoTrack {
      stream_id: MediaStreamId(stream),
      device_id: device(owner),
      kind: VideoTrackKind::Camera,
      codec: VideoCodec::H264,
      layers: layers(layer_count),
      maybe_publication_id: None,
    }
  }

  /// The driver asks for an own track like `track`, under its stream id as publication id; the
  /// request's signal is left in the outputs.
  fn ask_for(engine: &mut CallEngine, track: &VideoTrack) {
    engine.publish_video(
      VideoPublicationId(track.stream_id.0),
      track.kind.clone(),
      track.codec.clone(),
      track.layers.clone(),
    );
  }

  fn layers(count: u8) -> Vec<VideoLayerInfo> {
    [
      (320, 180, 150_000),
      (640, 360, 500_000),
      (1280, 720, 1_500_000),
      (1920, 1080, 3_000_000),
    ]
    .into_iter()
    .zip(0..count)
    .map(|((width, height, max_bitrate_bps), layer)| VideoLayerInfo {
      layer,
      width,
      height,
      max_framerate: 30,
      max_bitrate_bps,
    })
    .collect()
  }

  fn dave_proposals(
    epoch: u64,
    proposals: Vec<u8>,
    expected: &[i64],
    key_bindings: Vec<DaveKeyBinding>,
  ) -> CallEvent {
    CallEvent::DaveProposals {
      epoch,
      proposals: Base64Bytes::new(proposals),
      expected_participants: expected.iter().map(|id| device(*id)).collect(),
      key_bindings,
    }
  }

  fn event(seq: u64, event: CallEvent) -> CallServerMessage {
    CallServerMessage::Event { seq, event }
  }

  fn participant(id: i64) -> CallParticipant {
    CallParticipant {
      user_id: UserId::from_snowflake(Snowflake::new(1000 + id).unwrap()),
      device_id: device(id),
      // Stream ids mirror device ids in these tests.
      audio_stream_id: MediaStreamId(u32::try_from(id).unwrap()),
    }
  }

  fn frame() -> EncodedAudioFrame {
    EncodedAudioFrame {
      opus: vec![0x78, 1, 2, 3, 4, 5, 6, 7, 8, 9],
      level: AudioLevel::new(20, true).unwrap(),
    }
  }

  fn call() -> CallId {
    CallId::from_snowflake(Snowflake::new(CALL).unwrap())
  }

  fn device(id: i64) -> DeviceId {
    DeviceId::from_snowflake(Snowflake::new(id).unwrap())
  }
}
