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

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use teamcraft_proto::{
  Base64Bytes, CallClientMessage, CallEvent, CallId, CallParticipant, CallServerMessage, DaveKeyBinding,
  DeviceId, ErrorBody, MediaCapabilities, MediaStreamId, ScreenAudioTrack, TransportFeedback, UserId,
  VideoCodec, VideoLayerInfo, VideoTrack, VideoTrackKind,
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
use crate::uplink_layers::{OwnTrack, UplinkLayers, UplinkPolicy, demand_bps};
use crate::video::EncodedVideoFrame;

/// RTP timestamp increment per 20 ms Opus frame at 48 kHz.
const OPUS_FRAME_TICKS: u32 = 960;
/// Largest datagram the engine produces; audio frames are far below it.
const MAX_DATAGRAM_BYTES: usize = 1200;

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
  /// Publications asked for whose stream id hasn't arrived yet.
  pending_publications: VecDeque<(VideoTrackKind, VideoCodec, Vec<VideoLayerInfo>)>,
  /// Layers of own tracks the SFU asked for; without an entry, all of them.
  sfu_layers: HashMap<StreamId, Vec<u8>>,
  /// Layers of own tracks the driver was last told to send.
  sent_layers: HashMap<StreamId, Vec<u8>>,
  uplink_layers: UplinkLayers,
  uplink_policy: UplinkPolicy,
  /// Other devices' tracks this device receives, with the highest layer asked for.
  subscriptions: HashMap<StreamId, u8>,
  /// How long the SFU keeps this device's slot after the connection was lost.
  maybe_reconnect_grace: Option<Duration>,
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
  /// The SFU rejected a request.
  Error(ErrorBody),
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
/// captured frame becomes up to three pictures with simulcast.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoStats {
  /// Frames the own capture delivered.
  pub frames_captured: u64,
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
      uplink_policy: UplinkPolicy::default(),
      subscriptions: HashMap::new(),
      maybe_reconnect_grace: None,
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

  /// Starts publishing a video track; [`CallEngineEvent::VideoPublished`] brings its stream id.
  /// Screen shares use one layer (no simulcast).
  pub fn publish_video(&mut self, kind: VideoTrackKind, codec: VideoCodec, layers: Vec<VideoLayerInfo>) {
    self
      .pending_publications
      .push_back((kind.clone(), codec.clone(), layers.clone()));
    self.signal(CallClientMessage::PublishVideo { kind, codec, layers });
  }

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
    self.signal(CallClientMessage::Subscribe {
      stream_id,
      maybe_max_layer,
    });
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
      return VideoSendOutcome::NotSecured;
    };
    let encrypted = match dave.encrypt_frame(MediaKind::Video, frame_codec(&frame.codec), &frame.data) {
      Ok(encrypted) => encrypted.into_owned(),
      Err(error) => {
        warn!(%error, "video frame not encrypted, dropped");
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
        return VideoSendOutcome::Failed;
      },
    };
    for rtp in packets {
      self.send_media(stream, frame.layer, &rtp, now);
    }
    VideoSendOutcome::Sent
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
      } => {
        self.maybe_reconnect_grace = maybe_reconnect_grace_ms.map(Duration::from_millis);
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
    self.reconcile_own_video(&state.video_tracks);
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
    let mut unanswered = std::mem::take(&mut self.pending_publications);
    let announced: Vec<&VideoTrack> = tracks.iter().filter(|track| track.device_id == own).collect();
    for track in &announced {
      let stream = StreamId(track.stream_id.0);
      if self.own_tracks.contains_key(&stream) {
        continue;
      }
      // Published just before the loss, the answer got lost: take it.
      if let Some(position) = unanswered.iter().position(|(kind, ..)| *kind == track.kind) {
        unanswered.remove(position);
        self.add_video_track((*track).clone());
      } else {
        // Unpublished while away.
        self.signal(CallClientMessage::UnpublishVideo {
          stream_id: track.stream_id,
        });
      }
    }
    // Tracks the SFU dropped with the slot: publish them again; the driver learns the new id.
    let lost: Vec<VideoTrack> = self
      .own_tracks
      .iter()
      .filter(|(stream, _)| !announced.iter().any(|track| track.stream_id.0 == stream.0))
      .map(|(_, track)| track.clone())
      .collect();
    for track in lost {
      self.forget_own_track(StreamId(track.stream_id.0));
      self.event(CallEngineEvent::VideoUnpublished(track.stream_id));
      self.publish_video(track.kind, track.codec, track.layers);
    }
    for (kind, codec, layers) in unanswered {
      self.publish_video(kind, codec, layers);
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
    self.uplink_layers.remove(stream);
    self.update_uplink_demand();
    was_published
  }

  /// New uplink feedback: the estimate decides which own video layers fit next to the audio, the
  /// loss how much forward error correction the audio carries.
  fn on_uplink_feedback(&mut self, feedback: &TransportFeedback, now: Instant) {
    let bps = self.uplink.on_feedback(feedback, now, self.uplink_feedback_epoch);
    let is_sending_screen_audio = self.screen_audio != ScreenAudioState::Off;
    let tracks: Vec<OwnTrack<'_>> = self
      .own_tracks
      .iter()
      .map(|(stream, track)| OwnTrack {
        stream: *stream,
        kind: &track.kind,
        layers: &track.layers,
      })
      .collect();
    let has_changed =
      self
        .uplink_layers
        .on_estimate(bps, &tracks, is_sending_screen_audio, &self.uplink_policy);
    if has_changed {
      self.announce_layers(true);
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
      let layers: Vec<u8> = wanted
        .into_iter()
        .filter(|layer| self.uplink_layers.allows(stream, *layer))
        .collect();
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
  }

  /// Lets the uplink estimate grow to what all own layers need, even while some are held back.
  fn update_uplink_demand(&mut self) {
    let is_sending_screen_audio = self.screen_audio != ScreenAudioState::Off;
    let tracks: Vec<OwnTrack<'_>> = self
      .own_tracks
      .iter()
      .map(|(stream, track)| OwnTrack {
        stream: *stream,
        kind: &track.kind,
        layers: &track.layers,
      })
      .collect();
    let maybe_demand =
      (!tracks.is_empty()).then(|| demand_bps(&tracks, is_sending_screen_audio, &self.uplink_policy));
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
      return self.receive_video(datagram.header.stream_id, datagram.header.layer, datagram.rtp);
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

  fn receive_video(
    &mut self,
    stream: StreamId,
    layer: SimulcastLayer,
    rtp: &[u8],
  ) -> Result<(), CallEngineError> {
    let packet = RtpPacket::parse(rtp).map_err(|_| CallEngineError::Protocol("bad RTP"))?;
    let Some(track) = self.remote_video.get_mut(&stream) else {
      return Ok(());
    };
    let Some(complete) = track
      .push(layer, &packet)
      .map_err(|_| CallEngineError::Protocol("bad video fragment"))?
    else {
      return Ok(());
    };
    let (sender, codec) = (track.sender, track.codec.clone());
    let data = self.dave()?.decrypt_frame(
      DaveParticipantId::from_device(sender),
      MediaKind::Video,
      &complete.data,
    )?;
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

  fn add_video_track(&mut self, track: VideoTrack) {
    let stream = StreamId(track.stream_id.0);
    if track.device_id == self.config.device_id {
      if let Some(position) = self
        .pending_publications
        .iter()
        .position(|(kind, ..)| *kind == track.kind)
      {
        self.pending_publications.remove(position);
      }
      self.own_video.insert(
        stream,
        OutgoingVideo::new(track.kind.clone(), track.codec.clone()),
      );
      // Drivers start with every layer. A track without layer list isn't limited.
      if !track.layers.is_empty() {
        self
          .sent_layers
          .insert(stream, track.layers.iter().map(|info| info.layer).collect());
      }
      self.own_tracks.insert(stream, track.clone());
      self.update_uplink_demand();
    } else {
      self.remote_video.insert(stream, IncomingVideo::new(&track));
    }
    let is_own = track.device_id == self.config.device_id;
    let own_layers: Vec<u8> = track.layers.iter().map(|info| info.layer).collect();
    let stream_id = track.stream_id;
    self.event(CallEngineEvent::VideoPublished(track));
    // The driver dropped the pictures encoded before this confirmation, the first keyframe
    // among them: receivers would see nothing until the next one (issue #683).
    if is_own {
      self.request_own_keyframes(stream_id, &own_layers);
    }
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
      };
      alice.publish_video(VideoTrackKind::Screen, VideoCodec::Vp9, Vec::new());
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
      alice.publish_video(VideoTrackKind::Camera, VideoCodec::H264, layers(2));
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
      alice.publish_video(VideoTrackKind::Camera, VideoCodec::H264, layers(1));
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
      };
      alice.handle_signal(event(1, kept), now).unwrap();
      assert_eq!(
        drain(&mut alice),
        vec![
          EngineOutput::Signal(CallClientMessage::Subscribe {
            stream_id: MediaStreamId(60),
            maybe_max_layer: Some(0),
          }),
          EngineOutput::Event(CallEngineEvent::Reconnected),
        ]
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
    fn layers_the_sfu_needs_are_narrowed_to_what_the_uplink_carries() {
      let now = Instant::now();
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_engine(1, &[], &gateway, None, now);
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
    }
  }

  fn layers(count: u8) -> Vec<VideoLayerInfo> {
    [(320, 180, 150_000), (640, 360, 500_000), (1280, 720, 1_500_000)]
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
