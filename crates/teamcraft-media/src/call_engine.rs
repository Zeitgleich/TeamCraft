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

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;

use teamcraft_proto::{
  Base64Bytes, CallClientMessage, CallEvent, CallId, CallParticipant, CallServerMessage, DaveKeyBinding,
  DeviceId, ErrorBody, MediaCapabilities, MediaStreamId, UserId, VideoCodec, VideoLayerInfo, VideoTrack,
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
  audio_streams: HashMap<StreamId, DeviceId>,
  /// Tracks this device publishes, by the stream id the SFU assigned.
  own_video: HashMap<StreamId, OutgoingVideo>,
  /// Tracks other devices publish.
  remote_video: HashMap<StreamId, IncomingVideo>,
  maybe_dave: Option<DaveParticipant>,
  maybe_trusted_roster: Option<BTreeSet<DeviceId>>,
  maybe_authenticator: Option<Arc<dyn CallKeyAuthenticator>>,
  /// DAVE keys of other members from verified call key bindings (e2ee.md §8).
  verified_keys: HashMap<DeviceId, Vec<u8>>,
  /// A member failed key verification: media stays off for this call.
  is_compromised: bool,
  has_reported_unauthenticated_keys: bool,
  uplink: SendSideEstimator,
  downlink_feedback: FeedbackRecorder,
  /// Base for the SFU's arrival times in uplink feedback (only differences matter).
  uplink_feedback_epoch: Instant,
  audio_sequence: SequenceNumber,
  audio_timestamp: RtpTimestamp,
  last_event_seq: u64,
  is_muted: bool,
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
  ActiveSpeakers(Vec<DeviceId>),
  /// The DAVE group (re)keyed; media is end-to-end encrypted. Compare `privacy_code` out of band.
  Secured {
    epoch: u64,
    privacy_code: String,
  },
  /// A decrypted audio frame for the jitter buffer.
  AudioFrame {
    sender: DeviceId,
    sequence_number: SequenceNumber,
    opus: Vec<u8>,
  },
  /// A video track was published; for this device's own tracks it carries the stream id to send
  /// on. Subscribe to other devices' tracks with [`CallEngine::subscribe_video`].
  VideoPublished(VideoTrack),
  VideoUnpublished(MediaStreamId),
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
  /// Estimated uplink bandwidth after new feedback.
  UplinkEstimate {
    bps: u32,
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
  pub frames_sent: u64,
  pub frames_received: u64,
  /// 20 ms mix frames with audible content.
  pub audible_frames_played: u64,
  pub maybe_privacy_code: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
  Idle,
  Joining,
  Joined { call_id: CallId, audio_stream: StreamId },
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
    Self {
      config,
      phase: Phase::Idle,
      participants: BTreeMap::new(),
      audio_streams: HashMap::new(),
      own_video: HashMap::new(),
      remote_video: HashMap::new(),
      maybe_dave: None,
      maybe_trusted_roster: None,
      maybe_authenticator: None,
      verified_keys: HashMap::new(),
      is_compromised: false,
      has_reported_unauthenticated_keys: false,
      uplink,
      downlink_feedback: FeedbackRecorder::new(now),
      uplink_feedback_epoch: now,
      audio_sequence: SequenceNumber(0),
      audio_timestamp: RtpTimestamp(0),
      last_event_seq: 0,
      is_muted: false,
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
    }
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
    if self.is_muted || self.is_compromised {
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
    let mut header = RtpHeader::new(
      PayloadType::OPUS,
      self.audio_sequence,
      self.audio_timestamp,
      Ssrc(audio_stream.0),
    );
    header.set_audio_level(frame.level);
    self.audio_sequence = self.audio_sequence.next();
    self.audio_timestamp = RtpTimestamp(self.audio_timestamp.0.wrapping_add(OPUS_FRAME_TICKS));
    let Ok(rtp) = (RtpPacket {
      header,
      payload: &encrypted,
    })
    .to_bytes() else {
      return;
    };
    self.send_media(audio_stream, SimulcastLayer::BASE, &rtp, now);
  }

  /// Starts publishing a video track; [`CallEngineEvent::VideoPublished`] brings its stream id.
  /// Screen shares use one layer (no simulcast).
  pub fn publish_video(&mut self, kind: VideoTrackKind, codec: VideoCodec, layers: Vec<VideoLayerInfo>) {
    self.signal(CallClientMessage::PublishVideo { kind, codec, layers });
  }

  pub fn unpublish_video(&mut self, stream_id: MediaStreamId) {
    if self.own_video.remove(&StreamId(stream_id.0)).is_some() {
      self.signal(CallClientMessage::UnpublishVideo { stream_id });
    }
  }

  /// Receives another device's track up to `max_layer`; `None` stops receiving it.
  pub fn subscribe_video(&mut self, stream_id: MediaStreamId, maybe_max_layer: Option<u8>) {
    self.signal(CallClientMessage::Subscribe {
      stream_id,
      maybe_max_layer,
    });
  }

  /// Encrypts, splits and sends one encoded frame of an own track. Dropped before DAVE is ready,
  /// like audio.
  pub fn send_video(&mut self, stream_id: MediaStreamId, frame: &EncodedVideoFrame, now: Instant) {
    let stream = StreamId(stream_id.0);
    if self.is_compromised || !self.own_video.contains_key(&stream) {
      return;
    }
    let Some(dave) = self.maybe_dave.as_mut().filter(|dave| dave.is_ready()) else {
      return;
    };
    let encrypted = match dave.encrypt_frame(MediaKind::Video, frame_codec(&frame.codec), &frame.data) {
      Ok(encrypted) => encrypted.into_owned(),
      Err(error) => {
        warn!(%error, "video frame not encrypted, dropped");
        return;
      },
    };
    let Some(track) = self.own_video.get_mut(&stream) else {
      return;
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
        return;
      },
    };
    for rtp in packets {
      self.send_media(stream, frame.layer, &rtp, now);
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
      } => {
        self.on_joined(call_id, participant, participants)?;
        for track in video_tracks {
          self.add_video_track(track);
        }
        Ok(())
      },
      CallEvent::ParticipantJoined { participant } => {
        self.add_participant(participant);
        self.event(CallEngineEvent::ParticipantJoined(participant));
        Ok(())
      },
      CallEvent::ParticipantLeft { device_id } => {
        self.participants.remove(&device_id);
        self.audio_streams.retain(|_, owner| *owner != device_id);
        self.remote_video.retain(|_, track| track.sender != device_id);
        self.event(CallEngineEvent::ParticipantLeft(device_id));
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
        let bps = self
          .uplink
          .on_feedback(&feedback, now, self.uplink_feedback_epoch);
        self.event(CallEngineEvent::UplinkEstimate { bps });
        Ok(())
      },
      CallEvent::VideoPublished { track } => {
        self.add_video_track(track);
        Ok(())
      },
      CallEvent::VideoUnpublished { stream_id } => {
        self.remote_video.remove(&StreamId(stream_id.0));
        self.own_video.remove(&StreamId(stream_id.0));
        self.event(CallEngineEvent::VideoUnpublished(stream_id));
        Ok(())
      },
      CallEvent::KeyframeRequested { stream_id, layer } => {
        self.event(CallEngineEvent::KeyframeRequested { stream_id, layer });
        Ok(())
      },
      CallEvent::LayersNeeded { stream_id, layers } => {
        self.event(CallEngineEvent::LayersNeeded { stream_id, layers });
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
      Phase::Joined { call_id, .. } => Some(call_id),
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
    let Some(&sender) = self.audio_streams.get(&datagram.header.stream_id) else {
      return Ok(());
    };
    if self.is_deafened {
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
      sequence_number: packet.header.sequence_number,
      opus,
    });
    Ok(())
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
      self.own_video.insert(
        stream,
        OutgoingVideo::new(track.kind.clone(), track.codec.clone()),
      );
    } else {
      self.remote_video.insert(stream, IncomingVideo::new(&track));
    }
    self.event(CallEngineEvent::VideoPublished(track));
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
    self
      .audio_streams
      .insert(StreamId(participant.audio_stream_id.0), participant.device_id);
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
      alice.send_video(MediaStreamId(50), &frame, now);
      assert!(drain(&mut alice).is_empty());
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

  fn joined(id: i64, others: &[i64]) -> CallEvent {
    CallEvent::Joined {
      call_id: call(),
      participant: participant(id),
      participants: others.iter().map(|other| participant(*other)).collect(),
      video_tracks: Vec::new(),
    }
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
