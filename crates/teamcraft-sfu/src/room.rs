//! Rooms (one per call): participants, datagram forwarding and DAVE coordination.
//!
//! State sits behind a `std::sync::Mutex` that is never held across an `.await`; the per-datagram
//! critical section parses headers and restamps the datagram per receiver. A lock-free forwarding
//! table is a later optimization (SPIKE.md).
//!
//! A participant whose connection is lost keeps its slot (audio stream, published tracks,
//! subscriptions) while it reconnects: the others hear `participant_reconnecting`, and a join of
//! the same device on a new connection takes the slot back ([`JoinOutcome::Resumed`]). Every
//! connection gets its own [`SessionId`], so a late cleanup of an old connection never touches the
//! slot a newer one took over.

use std::{
  collections::{HashMap, HashSet},
  future::Future,
  pin::Pin,
  sync::{
    Arc, Mutex, MutexGuard, PoisonError,
    atomic::{AtomicBool, AtomicU64, Ordering},
  },
  time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use teamcraft_media::{datagram::StreamId, dave::DaveEpoch};
use teamcraft_proto::{
  Base64Bytes, CallClientMessage, CallEvent, CallId, CallParticipant, CallRosterEntry, ConversationId,
  DeviceId, ErrorBody, ErrorCode, MediaCapabilities, MediaStreamId, SFU_GOING_AWAY_CLOSE_CODE, SfuCallRoster,
  SfuControlAction, TransportFeedback, UnixMillis, UserId, VideoCodec, VideoLayerInfo, VideoTrackKind,
};
use thiserror::Error;
use tokio::sync::mpsc::{Sender, error::TrySendError};
use tracing::{debug, info, trace, warn};
use wtransport::Connection;

use crate::{
  auth::JoinGrant,
  config::SfuConfig,
  dave_coordinator::{DaveCoordinator, DaveCoordinatorError, Outbound},
  forwarding::{Effect, ForwardError, Forwarder},
  rate_limit::TokenBucket,
  signaling::Outgoing,
};

/// How long a closing participant has to receive the host's event before the connection drops.
const CLOSE_GRACE: Duration = Duration::from_millis(250);
/// How long a close waits for the peer to confirm the WebTransport session close.
const SESSION_CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
/// QUIC application close code for connections a meeting host ended.
const CLOSED_BY_HOST: u32 = 0x4801;
const SIGNALING_OVERFLOW: u32 = 0x4802;
/// QUIC application close code for a connection the same device replaced with a newer one.
pub const REPLACED_BY_NEW_CONNECTION: u32 = 0x4804;

/// All rooms of this SFU.
pub struct Rooms {
  config: SfuConfig,
  rooms: Mutex<HashMap<CallId, Arc<Room>>>,
  /// Host controls that outlive a room's participants: locked calls, removed users.
  control: Mutex<ControlState>,
  next_session: AtomicU64,
  /// The SFU is shutting down; see [`Rooms::go_away`].
  is_going_away: AtomicBool,
}

#[derive(Default)]
struct ControlState {
  locked: HashSet<CallId>,
  removed: HashSet<(CallId, UserId)>,
}

/// One call.
pub struct Room {
  call_id: CallId,
  /// The call's conversation, from the first join token that named it (all come from the server).
  maybe_conversation_id: Option<ConversationId>,
  /// Who opened the room, and when: reported so a restarted server can show the call's start.
  started_by: UserId,
  started_at: UnixMillis,
  reconnect_grace: Duration,
  mute_report_burst: u32,
  mute_reports_per_second: u32,
  state: Mutex<RoomState>,
}

/// One connection of a participant; a device that reconnects gets a new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(u64);

/// Handle a session needs to talk to its participant.
#[derive(Clone)]
pub struct ParticipantHandle {
  pub link: Arc<dyn ParticipantLink>,
  pub signal_tx: Sender<Outgoing>,
}

/// The datagram side of a participant's connection (a trait so rooms are tested without network).
pub trait ParticipantLink: Send + Sync {
  /// Sends one datagram. Unreliable by design: a full buffer or a closed peer drops it.
  fn send_datagram(&self, bytes: &[u8]);
  /// Closes the connection with a QUIC application error code.
  fn close(&self, code: u32, reason: &[u8]);
  /// Like [`Self::close`], but first tells the peer's WebTransport session why (a
  /// `CLOSE_WEBTRANSPORT_SESSION` capsule with the code, which a browser reads as the `closeCode`
  /// of `WebTransport.closed`) and waits until the peer has it, up to [`SESSION_CLOSE_TIMEOUT`].
  fn close_gracefully<'a>(
    &'a self,
    code: u32,
    reason: &'a [u8],
  ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
    self.close(code, reason);
    Box::pin(std::future::ready(()))
  }
}

/// Whether a join started a new participant or took back a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinOutcome {
  Joined,
  /// The device was already in the call (reconnecting, or on a connection that is not yet known
  /// to be dead) and continues on the new connection.
  Resumed,
}

struct RoomState {
  participants: HashMap<DeviceId, Participant>,
  /// The latest session of every device that joined, also after it left: a session's cleanup
  /// releases the slot only if it is still that device's latest session.
  latest_sessions: HashMap<DeviceId, SessionId>,
  forwarder: Forwarder,
  dave: DaveCoordinator,
  next_stream_id: u32,
}

/// What releasing a session's slot did.
struct Release {
  has_left: bool,
  is_room_empty: bool,
}

struct Participant {
  user_id: UserId,
  /// The device's microphone is muted (its own report; metadata for the others' tiles).
  is_muted: bool,
  /// Limits how often the device may change it.
  mute_limit: TokenBucket,
  audio_stream_id: StreamId,
  session: SessionId,
  presence: Presence,
}

enum Presence {
  Connected(ParticipantHandle),
  /// The connection was lost; the slot waits for the device to join again.
  Reconnecting,
}

#[derive(Debug, Error)]
pub enum RoomError {
  #[error("participant {0:?} is already in the call")]
  AlreadyJoined(DeviceId),
  #[error(transparent)]
  Forward(#[from] ForwardError),
  #[error(transparent)]
  Dave(#[from] DaveCoordinatorError),
  #[error("the call is locked")]
  Locked,
  #[error("the meeting host removed this user from the call")]
  Removed,
  #[error("call {0} is not on this SFU")]
  CallNotFound(CallId),
}

impl ParticipantLink for Connection {
  fn send_datagram(&self, bytes: &[u8]) {
    if let Err(error) = Connection::send_datagram(self, bytes) {
      trace!(%error, "datagram dropped");
    }
  }

  /// Ends the session with its close capsule and the connection after it, in the background; a
  /// connection closed with no runtime to do that in is closed at once.
  fn close(&self, code: u32, reason: &[u8]) {
    if tokio::runtime::Handle::try_current().is_err() {
      Connection::close(self, code.into(), reason);
      return;
    }
    let (connection, reason) = (self.clone(), reason.to_vec());
    tokio::spawn(async move { ParticipantLink::close_gracefully(&connection, code, &reason).await });
  }

  fn close_gracefully<'a>(
    &'a self,
    code: u32,
    reason: &'a [u8],
  ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
      // Lossy: the capsule's reason is text, ours always is.
      let text = String::from_utf8_lossy(reason);
      if tokio::time::timeout(SESSION_CLOSE_TIMEOUT, self.close_session(code, &text))
        .await
        .is_err()
      {
        debug!("the peer did not confirm the session close in time");
      }
      Connection::close(self, code.into(), reason);
    })
  }
}

impl Rooms {
  pub fn new(config: SfuConfig) -> Self {
    Self {
      config,
      rooms: Mutex::new(HashMap::new()),
      control: Mutex::new(ControlState::default()),
      next_session: AtomicU64::new(1),
      is_going_away: AtomicBool::new(false),
    }
  }

  /// The SFU is shutting down (a restart, a rolling deploy): closes every participant's
  /// connection with [`SFU_GOING_AWAY_CLOSE_CODE`], which clients answer with a rejoin – on the
  /// restarted SFU, into a fresh room with a new DAVE group. From now on leaves are not reported
  /// to the server ([`Self::is_going_away`]): the participants are on their way back, and the
  /// server keeps the call meanwhile. Returns how many connections were closed.
  pub fn go_away(&self) -> usize {
    let handles = self.begin_going_away();
    for handle in &handles {
      handle.link.close(SFU_GOING_AWAY_CLOSE_CODE, b"going away");
    }
    info!(closed = handles.len(), "going away: closed every call connection");
    handles.len()
  }

  /// [`Self::go_away`] for a shutdown: returns once every client has the session close with the
  /// code (or [`SESSION_CLOSE_TIMEOUT`] passed), so the endpoint can be closed after it.
  pub async fn go_away_gracefully(&self) -> usize {
    let handles = self.begin_going_away();
    let closed = handles.len();
    let mut closes = tokio::task::JoinSet::new();
    for handle in handles {
      closes.spawn(async move {
        handle
          .link
          .close_gracefully(SFU_GOING_AWAY_CLOSE_CODE, b"going away")
          .await;
      });
    }
    while closes.join_next().await.is_some() {}
    info!(closed, "going away: closed every call connection");
    closed
  }

  fn begin_going_away(&self) -> Vec<ParticipantHandle> {
    self.is_going_away.store(true, Ordering::Relaxed);
    let rooms: Vec<Arc<Room>> = lock(&self.rooms).values().cloned().collect();
    rooms.iter().flat_map(|room| room.connected_handles()).collect()
  }

  pub fn is_going_away(&self) -> bool {
    self.is_going_away.load(Ordering::Relaxed)
  }

  /// Applies a meeting host's action (already authorized by the server).
  pub fn control(
    &self,
    call_id: CallId,
    by_user_id: UserId,
    action: &SfuControlAction,
  ) -> Result<(), RoomError> {
    let room = lock(&self.rooms)
      .get(&call_id)
      .cloned()
      .ok_or(RoomError::CallNotFound(call_id))?;
    info!(call_id = %call_id, by_user_id = %by_user_id, ?action, "host control");
    match action {
      SfuControlAction::MuteAll => {
        let mut state = lock(&room.state);
        state.broadcast(CallEvent::MutedByHost { by_user_id });
        // Microphones are muted by the clients (they may unmute); shared sound is stopped here,
        // so a client that ignores the request can't keep playing into the meeting.
        for (owner, stream) in state.forwarder.screen_audio_streams() {
          if state.forwarder.unpublish_screen_audio(owner, stream).is_ok() {
            state.broadcast(CallEvent::ScreenAudioUnpublished {
              stream_id: MediaStreamId(stream.0),
            });
          }
        }
      },
      SfuControlAction::RemoveParticipant { user_id } => {
        lock(&self.control).removed.insert((call_id, *user_id));
        room.close_user(*user_id, CallEvent::RemovedByHost { by_user_id });
      },
      SfuControlAction::Lock => {
        lock(&self.control).locked.insert(call_id);
      },
      SfuControlAction::Unlock => {
        lock(&self.control).locked.remove(&call_id);
      },
      SfuControlAction::End => {
        {
          let mut control = lock(&self.control);
          control.locked.remove(&call_id);
          control.removed.retain(|(call, _)| *call != call_id);
        }
        room.close_everyone(CallEvent::EndedByHost { by_user_id });
      },
      SfuControlAction::DisconnectUser { user_id } => room.disconnect_user(*user_id),
      SfuControlAction::Unknown => {},
    }
    Ok(())
  }

  /// Every call on this SFU and the devices in it, reconnecting ones included: what the server's
  /// roster is reconciled with (`SfuRosterReport`).
  pub fn roster(&self) -> Vec<SfuCallRoster> {
    // Lock order: rooms, then room state; the rooms lock is released first.
    let rooms: Vec<Arc<Room>> = lock(&self.rooms).values().cloned().collect();
    rooms
      .iter()
      .map(|room| SfuCallRoster {
        call_id: room.call_id,
        participants: room.roster(),
        maybe_conversation_id: room.maybe_conversation_id,
        maybe_started_by: Some(room.started_by),
        maybe_started_at: Some(room.started_at),
      })
      .collect()
  }

  /// Adds a participant to a call, creating the room on first join. A device that is already in
  /// the call takes its slot back on the new connection; a locked call still lets it back in.
  pub fn join(
    &self,
    grant: JoinGrant,
    capabilities: MediaCapabilities,
    handle: ParticipantHandle,
  ) -> Result<(Arc<Room>, SessionId, JoinOutcome), RoomError> {
    let call_id = grant.call_id;
    let session = SessionId(self.next_session.fetch_add(1, Ordering::Relaxed));
    // The rooms lock is held across the join so `leave` can't drop the room in between.
    // Lock order everywhere: rooms, then control, then room state.
    let mut rooms = lock(&self.rooms);
    let has_slot = rooms
      .get(&call_id)
      .is_some_and(|room| room.has_participant(grant.device_id));
    {
      let control = lock(&self.control);
      if control.removed.contains(&(call_id, grant.user_id)) {
        return Err(RoomError::Removed);
      }
      if control.locked.contains(&call_id) && !has_slot {
        return Err(RoomError::Locked);
      }
    }
    let room = match rooms.get(&call_id) {
      Some(room) => Arc::clone(room),
      None => {
        let room = Arc::new(Room::new(&grant, unix_now_millis(), &self.config)?);
        rooms.insert(call_id, Arc::clone(&room));
        info!(call_id = %call_id, "room created");
        room
      },
    };
    let result = room.join(grant, capabilities, handle, session, Instant::now());
    if result.is_err() && room.is_empty() {
      rooms.remove(&call_id);
    }
    Ok((room, session, result?))
  }

  /// The session's connection was lost: the participant keeps its slot while it reconnects.
  /// Returns `false` if the session no longer holds the slot (nothing to wait for).
  pub fn disconnect(&self, room: &Room, participant_id: DeviceId, session: SessionId, now: Instant) -> bool {
    room.disconnect(participant_id, session, now)
  }

  /// Releases the session's slot: the participant leaves the call (also after a reconnect grace
  /// period ran out). Returns whether the participant left because of this session, i.e. whether
  /// its leave is to be reported; a device that is back on a newer session stays.
  pub fn leave(&self, room: &Room, participant_id: DeviceId, session: SessionId) -> bool {
    let release = room.leave(participant_id, session);
    if release.is_room_empty {
      let mut rooms = lock(&self.rooms);
      // Only drop the room if no one rejoined in the meantime.
      if rooms.get(&room.call_id).is_some_and(|current| current.is_empty()) {
        rooms.remove(&room.call_id);
        // Host controls end with the call.
        let mut control = lock(&self.control);
        control.locked.remove(&room.call_id);
        control.removed.retain(|(call, _)| *call != room.call_id);
        info!(call_id = %room.call_id, "room closed");
      }
    }
    release.has_left
  }
}

impl Room {
  /// The room of `first`'s call, opened by its join at `now`.
  fn new(first: &JoinGrant, now: UnixMillis, config: &SfuConfig) -> Result<Self, RoomError> {
    let call_id = first.call_id;
    let state = RoomState {
      participants: HashMap::new(),
      latest_sessions: HashMap::new(),
      forwarder: Forwarder::new(config.forwarding),
      dave: DaveCoordinator::new(call_id)?,
      next_stream_id: 1,
    };
    Ok(Self {
      call_id,
      maybe_conversation_id: first.maybe_conversation_id,
      started_by: first.user_id,
      started_at: now,
      reconnect_grace: config.reconnect_grace,
      mute_report_burst: config.mute_report_burst,
      mute_reports_per_second: config.mute_reports_per_second,
      state: Mutex::new(state),
    })
  }

  pub fn call_id(&self) -> CallId {
    self.call_id
  }

  /// Handles one signaling message of a joined participant; an error goes back to it.
  pub fn on_client_message(
    &self,
    participant_id: DeviceId,
    message: CallClientMessage,
    now: Instant,
  ) -> Result<(), ErrorBody> {
    match message {
      CallClientMessage::DaveKeyPackage {
        key_package,
        maybe_key_binding,
      } => self
        .on_key_package(
          participant_id,
          key_package.into_vec(),
          maybe_key_binding.map(Base64Bytes::into_vec),
        )
        .map_err(|error| {
          warn!(participant_id = %participant_id, %error, "rejected key package");
          ErrorBody::new(ErrorCode::InvalidKeyPackage, error.to_string())
        }),
      CallClientMessage::DaveCommit {
        epoch,
        commit,
        maybe_welcome,
      } => {
        self.on_commit(
          participant_id,
          epoch,
          commit.into_vec(),
          maybe_welcome.map(Base64Bytes::into_vec),
        );
        Ok(())
      },
      CallClientMessage::Join { .. } => Err(ErrorBody::new(ErrorCode::ProtocolViolation, "already joined")),
      CallClientMessage::TransportFeedback { feedback } => {
        self.on_transport_feedback(participant_id, &feedback, now);
        Ok(())
      },
      CallClientMessage::PublishVideo { kind, codec, layers } => rejected(
        participant_id,
        self.publish_video(participant_id, kind, codec, layers),
      ),
      CallClientMessage::UnpublishVideo { stream_id } => {
        rejected(participant_id, self.unpublish_video(participant_id, stream_id))
      },
      CallClientMessage::PublishScreenAudio => {
        rejected(participant_id, self.publish_screen_audio(participant_id))
      },
      CallClientMessage::UnpublishScreenAudio { stream_id } => rejected(
        participant_id,
        self.unpublish_screen_audio(participant_id, stream_id),
      ),
      CallClientMessage::Subscribe {
        stream_id,
        maybe_max_layer,
      } => rejected(
        participant_id,
        self.subscribe(participant_id, stream_id, maybe_max_layer, now),
      ),
      CallClientMessage::SetMuted { is_muted } => {
        self.set_muted(participant_id, is_muted, now);
        Ok(())
      },
      CallClientMessage::MoveHere => {
        self.close_other_devices_of(participant_id);
        Ok(())
      },
      CallClientMessage::Unknown => {
        debug!(participant_id = %participant_id, "ignoring unknown message");
        Ok(())
      },
    }
  }

  /// Forwards one media datagram from `sender`: audio to everyone if the sender is among the
  /// loudest speakers, video to the subscribers receiving that layer. Only headers are read.
  pub fn forward_datagram(
    &self,
    sender: DeviceId,
    datagram: &[u8],
    now: Instant,
  ) -> Result<usize, RoomError> {
    let deliveries: Vec<(Arc<dyn ParticipantLink>, Vec<u8>)> = {
      let mut state = lock(&self.state);
      let route = state.forwarder.route(sender, datagram, now)?;
      state.apply(route.effects);
      route
        .deliveries
        .into_iter()
        .filter_map(|(receiver, bytes)| Some((Arc::clone(&state.handle_of(receiver)?.link), bytes)))
        .collect()
    };
    for (link, bytes) in &deliveries {
      link.send_datagram(bytes);
    }
    Ok(deliveries.len())
  }

  /// Publishes a video track of `owner` and announces it to everyone.
  pub fn publish_video(
    &self,
    owner: DeviceId,
    kind: VideoTrackKind,
    codec: VideoCodec,
    layers: Vec<VideoLayerInfo>,
  ) -> Result<(), RoomError> {
    let mut state = lock(&self.state);
    let stream_id = state.allocate_stream_id();
    let track = state
      .forwarder
      .publish_video(owner, stream_id, kind, codec, layers)?;
    state.broadcast(CallEvent::VideoPublished { track });
    Ok(())
  }

  /// Starts forwarding `owner`'s screen sound and announces it to everyone.
  pub fn publish_screen_audio(&self, owner: DeviceId) -> Result<(), RoomError> {
    let mut state = lock(&self.state);
    let stream_id = state.allocate_stream_id();
    let track = state.forwarder.publish_screen_audio(owner, stream_id)?;
    state.broadcast(CallEvent::ScreenAudioPublished { track });
    Ok(())
  }

  pub fn unpublish_screen_audio(&self, owner: DeviceId, stream_id: MediaStreamId) -> Result<(), RoomError> {
    let mut state = lock(&self.state);
    state
      .forwarder
      .unpublish_screen_audio(owner, StreamId(stream_id.0))?;
    state.broadcast(CallEvent::ScreenAudioUnpublished { stream_id });
    Ok(())
  }

  pub fn unpublish_video(&self, owner: DeviceId, stream_id: MediaStreamId) -> Result<(), RoomError> {
    let mut state = lock(&self.state);
    state.forwarder.unpublish_video(owner, StreamId(stream_id.0))?;
    state.broadcast(CallEvent::VideoUnpublished { stream_id });
    Ok(())
  }

  pub fn subscribe(
    &self,
    receiver: DeviceId,
    stream_id: MediaStreamId,
    maybe_max_layer: Option<u8>,
    now: Instant,
  ) -> Result<(), RoomError> {
    let mut state = lock(&self.state);
    let effects = state
      .forwarder
      .subscribe(receiver, StreamId(stream_id.0), maybe_max_layer, now)?;
    state.apply(effects);
    Ok(())
  }

  pub fn on_transport_feedback(&self, receiver: DeviceId, feedback: &TransportFeedback, now: Instant) {
    let mut state = lock(&self.state);
    let effects = state.forwarder.on_transport_feedback(receiver, feedback, now);
    state.apply(effects);
  }

  /// Periodic work for one participant (every ~100 ms): re-runs its layer allocation.
  pub fn on_timer(&self, device_id: DeviceId, now: Instant) {
    let mut state = lock(&self.state);
    let effects = state.forwarder.on_timer(device_id, now);
    state.apply(effects);
  }

  /// Feedback on the datagrams `sender` sent since the last call, for its uplink estimation.
  pub fn take_uplink_feedback(&self, sender: DeviceId) -> Option<TransportFeedback> {
    lock(&self.state).forwarder.take_uplink_feedback(sender)
  }

  pub fn on_key_package(
    &self,
    participant_id: DeviceId,
    key_package: Vec<u8>,
    maybe_key_binding: Option<Vec<u8>>,
  ) -> Result<(), RoomError> {
    let mut state = lock(&self.state);
    let outbound = state
      .dave
      .on_key_package(participant_id, key_package, maybe_key_binding)?;
    state.dispatch(outbound);
    Ok(())
  }

  pub fn on_commit(
    &self,
    participant_id: DeviceId,
    epoch: u64,
    commit: Vec<u8>,
    maybe_welcome: Option<Vec<u8>>,
  ) {
    let mut state = lock(&self.state);
    let outbound = state
      .dave
      .on_commit(participant_id, DaveEpoch(epoch), commit, maybe_welcome);
    state.dispatch(outbound);
  }

  fn join(
    &self,
    grant: JoinGrant,
    capabilities: MediaCapabilities,
    handle: ParticipantHandle,
    session: SessionId,
    now: Instant,
  ) -> Result<JoinOutcome, RoomError> {
    let participant_id = grant.device_id;
    let mut state = lock(&self.state);
    let package = state.dave.external_sender_package()?;
    let outcome = match state.participants.get(&participant_id) {
      Some(existing) if existing.user_id != grant.user_id => {
        return Err(RoomError::AlreadyJoined(participant_id));
      },
      Some(_) => {
        let was_reconnecting = state.resume(participant_id, capabilities, handle, session, now);
        state.welcome(self.call_id, participant_id, package, self.reconnect_grace);
        if was_reconnecting {
          state.broadcast_except(
            participant_id,
            CallEvent::ParticipantReconnected {
              device_id: participant_id,
            },
          );
        }
        JoinOutcome::Resumed
      },
      None => {
        let audio_stream_id = state.allocate_stream_id();
        state
          .forwarder
          .add_participant(participant_id, audio_stream_id, capabilities, now);
        state.participants.insert(
          participant_id,
          Participant {
            user_id: grant.user_id,
            is_muted: false,
            mute_limit: TokenBucket::new(self.mute_report_burst, self.mute_reports_per_second, now),
            audio_stream_id,
            session,
            presence: Presence::Connected(handle),
          },
        );
        state.welcome(self.call_id, participant_id, package, self.reconnect_grace);
        let info = state.info(participant_id);
        if let Some(participant) = info {
          state.broadcast_except(participant_id, CallEvent::ParticipantJoined { participant });
        }
        JoinOutcome::Joined
      },
    };
    state.latest_sessions.insert(participant_id, session);
    info!(call_id = %self.call_id, participant_id = %participant_id, ?outcome, "participant joined");
    Ok(outcome)
  }

  /// The participant's connection is gone; it keeps its slot. Returns whether `session` held it.
  fn disconnect(&self, participant_id: DeviceId, session: SessionId, now: Instant) -> bool {
    let mut state = lock(&self.state);
    let Some(participant) = state
      .participants
      .get_mut(&participant_id)
      .filter(|participant| participant.session == session)
    else {
      return false;
    };
    if matches!(participant.presence, Presence::Reconnecting) {
      return false;
    }
    participant.presence = Presence::Reconnecting;
    let effects = state.forwarder.suspend(participant_id, now);
    state.apply(effects);
    // The device comes back with a new DAVE key package; until then the group re-keys without it
    // (e2ee.md §8: a lost device must not hold a current media key).
    let outbound = state.dave.on_leave(participant_id);
    state.dispatch(outbound);
    state.broadcast(CallEvent::ParticipantReconnecting {
      device_id: participant_id,
    });
    info!(call_id = %self.call_id, participant_id = %participant_id, "participant reconnecting");
    true
  }

  fn leave(&self, participant_id: DeviceId, session: SessionId) -> Release {
    let mut state = lock(&self.state);
    let is_latest = state.latest_sessions.get(&participant_id) == Some(&session);
    let holds_slot = state
      .participants
      .get(&participant_id)
      .is_some_and(|participant| participant.session == session);
    if holds_slot {
      state.remove(self.call_id, participant_id, Instant::now());
    }
    // A slot the host dropped while it was reconnecting was this session's too.
    let has_left = holds_slot || (is_latest && !state.participants.contains_key(&participant_id));
    if has_left {
      state.latest_sessions.remove(&participant_id);
    }
    Release {
      has_left,
      is_room_empty: state.participants.is_empty(),
    }
  }

  fn roster(&self) -> Vec<CallRosterEntry> {
    lock(&self.state)
      .participants
      .iter()
      .map(|(device_id, participant)| CallRosterEntry {
        user_id: participant.user_id,
        device_id: *device_id,
      })
      .collect()
  }

  fn has_participant(&self, participant_id: DeviceId) -> bool {
    lock(&self.state).participants.contains_key(&participant_id)
  }

  fn is_empty(&self) -> bool {
    lock(&self.state).participants.is_empty()
  }

  /// Tells the user's devices why, then closes their connections (the sessions then leave as
  /// usual: DAVE re-keys and the server hears about it). Reconnecting devices of the user lose
  /// their slot at once.
  fn close_user(&self, user_id: UserId, event: CallEvent) {
    let mut state = lock(&self.state);
    let devices: Vec<DeviceId> = state
      .participants
      .iter()
      .filter(|(_, participant)| participant.user_id == user_id)
      .map(|(device_id, _)| *device_id)
      .collect();
    self.close(&mut state, &devices, event);
  }

  /// Records the device's mute state and tells the others when it changed.
  fn set_muted(&self, device_id: DeviceId, is_muted: bool, now: Instant) {
    let mut state = lock(&self.state);
    let Some(participant) = state.participants.get_mut(&device_id) else {
      return;
    };
    if participant.is_muted == is_muted {
      return;
    }
    // A flood of toggles is ignored, not stored: honest clients never come near the limit.
    if !participant.mute_limit.allow(now) {
      debug!(call_id = %self.call_id, device_id = %device_id, "mute report over the limit");
      return;
    }
    participant.is_muted = is_muted;
    state.broadcast_except(device_id, CallEvent::ParticipantMuted { device_id, is_muted });
  }

  /// The account's other devices leave for `device_id` (the call moves there). Reconnecting
  /// ones lose their slot at once; the DAVE group removes them like any leaver.
  fn close_other_devices_of(&self, device_id: DeviceId) {
    let mut state = lock(&self.state);
    let Some(user_id) = state
      .participants
      .get(&device_id)
      .map(|participant| participant.user_id)
    else {
      return;
    };
    let devices: Vec<DeviceId> = state
      .participants
      .iter()
      .filter(|(other, participant)| **other != device_id && participant.user_id == user_id)
      .map(|(other, _)| *other)
      .collect();
    let mut handles = Vec::new();
    for other in &devices {
      match state.handle_of(*other) {
        Some(handle) => handles.push(handle.clone()),
        None => state.remove(self.call_id, *other, Instant::now()),
      }
    }
    info!(call_id = %self.call_id, device_id = %device_id, moved = devices.len(), "call moved to a device");
    close_after_event(
      handles,
      CallEvent::ConnectionReplaced,
      REPLACED_BY_NEW_CONNECTION,
      b"call moved to another device",
    );
  }

  /// The user left the call on all devices: each of them hears that its connection was replaced
  /// (so it doesn't rejoin), then is closed; reconnecting ones lose their slot at once. Their
  /// sessions then leave as usual, and the server hears about it.
  fn disconnect_user(&self, user_id: UserId) {
    let mut state = lock(&self.state);
    let devices: Vec<DeviceId> = state
      .participants
      .iter()
      .filter(|(_, participant)| participant.user_id == user_id)
      .map(|(device_id, _)| *device_id)
      .collect();
    let mut handles = Vec::new();
    for device_id in &devices {
      match state.handle_of(*device_id) {
        Some(handle) => handles.push(handle.clone()),
        None => state.remove(self.call_id, *device_id, Instant::now()),
      }
    }
    info!(call_id = %self.call_id, user_id = %user_id, devices = devices.len(), "user left on all devices");
    close_after_event(
      handles,
      CallEvent::ConnectionReplaced,
      REPLACED_BY_NEW_CONNECTION,
      b"left the call on all devices",
    );
  }

  fn connected_handles(&self) -> Vec<ParticipantHandle> {
    let state = lock(&self.state);
    state
      .participants
      .keys()
      .filter_map(|device_id| state.handle_of(*device_id).cloned())
      .collect()
  }

  fn close_everyone(&self, event: CallEvent) {
    let mut state = lock(&self.state);
    let devices: Vec<DeviceId> = state.participants.keys().copied().collect();
    self.close(&mut state, &devices, event);
  }

  fn close(&self, state: &mut RoomState, devices: &[DeviceId], event: CallEvent) {
    let mut handles = Vec::new();
    for device_id in devices {
      match state.handle_of(*device_id) {
        Some(handle) => handles.push(handle.clone()),
        None => state.remove(self.call_id, *device_id, Instant::now()),
      }
    }
    close_after_event(handles, event, CLOSED_BY_HOST, b"closed by the meeting host");
  }
}

impl RoomState {
  /// Sends a (re)joined participant what it needs to start: the call's state, the DAVE external
  /// sender, the current speakers and who of the others is reconnecting.
  fn welcome(&self, call_id: CallId, participant_id: DeviceId, package: Vec<u8>, reconnect_grace: Duration) {
    let Some(participant) = self.info(participant_id) else {
      return;
    };
    let others: Vec<CallParticipant> = self
      .participants
      .keys()
      .filter(|device_id| **device_id != participant_id)
      .filter_map(|device_id| self.info(*device_id))
      .collect();
    self.send_to(
      participant_id,
      CallEvent::Joined {
        call_id,
        participant,
        participants: others,
        video_tracks: self.forwarder.video_tracks(),
        screen_audio_tracks: self.forwarder.screen_audio_tracks(),
        maybe_reconnect_grace_ms: Some(u64::try_from(reconnect_grace.as_millis()).unwrap_or(u64::MAX)),
      },
    );
    for (device_id, other) in &self.participants {
      if *device_id != participant_id && other.is_muted {
        self.send_to(
          participant_id,
          CallEvent::ParticipantMuted {
            device_id: *device_id,
            is_muted: true,
          },
        );
      }
    }
    self.send_to(
      participant_id,
      CallEvent::DaveExternalSender {
        package: Base64Bytes::new(package),
      },
    );
    let active_speakers = self.forwarder.active_speakers().to_vec();
    if !active_speakers.is_empty() {
      self.send_to(
        participant_id,
        CallEvent::ActiveSpeakers {
          device_ids: active_speakers,
        },
      );
    }
    for (device_id, other) in &self.participants {
      if matches!(other.presence, Presence::Reconnecting) {
        self.send_to(
          participant_id,
          CallEvent::ParticipantReconnecting {
            device_id: *device_id,
          },
        );
      }
    }
  }

  /// A device that is in the call joined on a new connection: it continues in its slot. A
  /// connection still thought alive is replaced (the device knows better: it is gone). Returns
  /// whether the others saw it reconnecting.
  fn resume(
    &mut self,
    participant_id: DeviceId,
    capabilities: MediaCapabilities,
    handle: ParticipantHandle,
    session: SessionId,
    now: Instant,
  ) -> bool {
    let Some(participant) = self.participants.get_mut(&participant_id) else {
      return false;
    };
    let previous = std::mem::replace(&mut participant.presence, Presence::Connected(handle));
    participant.session = session;
    let was_reconnecting = match previous {
      Presence::Connected(old) => {
        close_after_event(
          vec![old],
          CallEvent::ConnectionReplaced,
          REPLACED_BY_NEW_CONNECTION,
          b"replaced by a new connection",
        );
        // The new connection brings a new DAVE key package: the old leaf goes.
        let outbound = self.dave.on_leave(participant_id);
        self.dispatch(outbound);
        false
      },
      Presence::Reconnecting => true,
    };
    self.forwarder.resume(participant_id, capabilities, now);
    was_reconnecting
  }

  /// Removes a participant for good: its tracks and DAVE leaf go, everyone hears about it.
  fn remove(&mut self, call_id: CallId, participant_id: DeviceId, now: Instant) {
    if self.participants.remove(&participant_id).is_none() {
      return;
    }
    self.broadcast(CallEvent::ParticipantLeft {
      device_id: participant_id,
    });
    let removal = self.forwarder.remove_participant(participant_id, now);
    for stream_id in removal.video_tracks {
      self.broadcast(CallEvent::VideoUnpublished {
        stream_id: MediaStreamId(stream_id.0),
      });
    }
    if let Some(stream_id) = removal.maybe_screen_audio {
      self.broadcast(CallEvent::ScreenAudioUnpublished {
        stream_id: MediaStreamId(stream_id.0),
      });
    }
    self.apply(removal.effects);
    let outbound = self.dave.on_leave(participant_id);
    self.dispatch(outbound);
    info!(call_id = %call_id, participant_id = %participant_id, "participant left");
  }

  /// The next free stream id. The counter wraps after 2^32 publications; ids still in use are
  /// skipped, so a new stream never takes over someone's (their microphone, say).
  fn allocate_stream_id(&mut self) -> StreamId {
    loop {
      let stream_id = StreamId(self.next_stream_id);
      self.next_stream_id = self.next_stream_id.wrapping_add(1);
      if !self.forwarder.is_stream_in_use(stream_id) {
        return stream_id;
      }
    }
  }

  /// Sends the signaling a forwarding decision caused.
  fn apply(&self, effects: Vec<Effect>) {
    for effect in effects {
      match effect {
        Effect::ActiveSpeakers(device_ids) => self.broadcast(CallEvent::ActiveSpeakers { device_ids }),
        Effect::KeyframeRequest {
          publisher,
          stream_id,
          layer,
        } => self.send_to(
          publisher,
          CallEvent::KeyframeRequested {
            stream_id: MediaStreamId(stream_id.0),
            layer,
          },
        ),
        Effect::LayersNeeded {
          publisher,
          stream_id,
          layers,
        } => self.send_to(
          publisher,
          CallEvent::LayersNeeded {
            stream_id: MediaStreamId(stream_id.0),
            layers,
          },
        ),
      }
    }
  }

  /// The connection of a participant, unless it is reconnecting.
  fn handle_of(&self, device_id: DeviceId) -> Option<&ParticipantHandle> {
    match &self.participants.get(&device_id)?.presence {
      Presence::Connected(handle) => Some(handle),
      Presence::Reconnecting => None,
    }
  }

  fn info(&self, device_id: DeviceId) -> Option<CallParticipant> {
    let participant = self.participants.get(&device_id)?;
    Some(CallParticipant {
      user_id: participant.user_id,
      device_id,
      audio_stream_id: MediaStreamId(participant.audio_stream_id.0),
    })
  }

  fn send_to(&self, device_id: DeviceId, event: CallEvent) {
    if let Some(handle) = self.handle_of(device_id) {
      send(handle, event);
    }
  }

  /// Sends to every connected participant.
  fn broadcast(&self, event: CallEvent) {
    for device_id in self.participants.keys() {
      self.send_to(*device_id, event.clone());
    }
  }

  fn broadcast_except(&self, excluded: DeviceId, event: CallEvent) {
    for device_id in self
      .participants
      .keys()
      .filter(|device_id| **device_id != excluded)
    {
      self.send_to(*device_id, event.clone());
    }
  }

  fn dispatch(&self, outbound: Vec<Outbound>) {
    for Outbound { recipient, event } in outbound {
      match self.handle_of(recipient) {
        Some(handle) => send(handle, event),
        None => debug!(
          participant_id = %recipient,
          "dropping DAVE message for a departed or reconnecting participant"
        ),
      }
    }
  }
}

/// Turns a rejected request into a `protocol_violation` error for the client.
fn rejected(participant_id: DeviceId, result: Result<(), RoomError>) -> Result<(), ErrorBody> {
  result.map_err(|error| {
    debug!(participant_id = %participant_id, %error, "request rejected");
    ErrorBody::new(ErrorCode::ProtocolViolation, error.to_string())
  })
}

/// Sends `event` to each handle and closes the connections a moment later, so the event is
/// written first.
fn close_after_event(handles: Vec<ParticipantHandle>, event: CallEvent, code: u32, reason: &'static [u8]) {
  for handle in &handles {
    send(handle, event.clone());
  }
  tokio::spawn(async move {
    tokio::time::sleep(CLOSE_GRACE).await;
    for handle in handles {
      handle.link.close(code, reason);
    }
  });
}

fn send(handle: &ParticipantHandle, event: CallEvent) {
  match handle.signal_tx.try_send(Outgoing::Event(event)) {
    Ok(()) => {},
    // A participant that doesn't drain its signaling stream would make the queue grow without bound.
    Err(TrySendError::Full(_)) => {
      warn!("signaling queue full, disconnecting the participant");
      handle.link.close(SIGNALING_OVERFLOW, b"signaling queue overflow");
    },
    // The receiver only disappears when the session ends, which also removes the participant.
    Err(TrySendError::Closed(_)) => warn!("signaling channel closed"),
  }
}

/// Recovers from poisoning: room state stays consistent because no invariant spans a panic point.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The wall clock, only for the start time the roster reports (shown, never trusted for order).
fn unix_now_millis() -> UnixMillis {
  let millis = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or(0, |elapsed| elapsed.as_millis());
  UnixMillis(i64::try_from(millis).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};
  use tokio::sync::mpsc::{self, Receiver};

  use super::*;

  const CALL: i64 = 9;
  const CONVERSATION: i64 = 77;

  #[test]
  fn the_roster_lists_every_device_of_every_call() {
    let rooms = Rooms::new(SfuConfig::default());
    let (_alice_rx, _) = join(&rooms, 1, 10);
    let (_bob_rx, _) = join(&rooms, 2, 20);

    let roster = rooms.roster();

    assert_eq!(roster.len(), 1);
    assert_eq!(roster[0].call_id, id(CALL));
    // What a restarted server needs to take the call over.
    assert_eq!(roster[0].maybe_conversation_id, Some(id(CONVERSATION)));
    assert_eq!(roster[0].maybe_started_by, Some(id(1)));
    assert!(roster[0].maybe_started_at.is_some());
    let mut participants = roster[0].participants.clone();
    participants.sort_by_key(|entry| entry.device_id);
    assert_eq!(participants, vec![entry(1, 10), entry(2, 20)]);
  }

  #[tokio::test]
  async fn leaving_on_all_devices_replaces_only_that_users_connections() {
    let rooms = Rooms::new(SfuConfig::default());
    let (mut laptop_rx, _) = join(&rooms, 1, 10);
    let (mut phone_rx, phone_session) = join(&rooms, 1, 11);
    let (mut bob_rx, _) = join(&rooms, 2, 20);
    let room = lock(&rooms.rooms).get(&id(CALL)).cloned().unwrap();
    // The phone lost its connection: it holds its slot while it reconnects.
    assert!(rooms.disconnect(&room, id(11), phone_session, Instant::now()));
    drain(&mut laptop_rx);
    drain(&mut phone_rx);
    drain(&mut bob_rx);

    rooms
      .control(
        id(CALL),
        id(1),
        &SfuControlAction::DisconnectUser { user_id: id(1) },
      )
      .unwrap();

    assert!(drain(&mut laptop_rx).contains(&CallEvent::ConnectionReplaced));
    assert!(
      !drain(&mut bob_rx).contains(&CallEvent::ConnectionReplaced),
      "the others stay"
    );
    let devices: Vec<DeviceId> = rooms.roster()[0]
      .participants
      .iter()
      .map(|entry| entry.device_id)
      .collect();
    assert!(
      !devices.contains(&id(11)),
      "a reconnecting device loses its slot at once"
    );
    assert!(devices.contains(&id(20)));
  }

  /// Joins user `user` with device `device`; returns its signaling queue and session.
  fn join(rooms: &Rooms, user: i64, device: i64) -> (Receiver<Outgoing>, SessionId) {
    let (signal_tx, signal_rx) = mpsc::channel(64);
    let handle = ParticipantHandle {
      link: Arc::new(QuietLink),
      signal_tx,
    };
    let grant = JoinGrant {
      call_id: id(CALL),
      user_id: id(user),
      device_id: id(device),
      maybe_conversation_id: Some(id(CONVERSATION)),
    };
    let (_, session, _) = rooms.join(grant, MediaCapabilities::default(), handle).unwrap();
    (signal_rx, session)
  }

  fn drain(rx: &mut Receiver<Outgoing>) -> Vec<CallEvent> {
    let mut events = Vec::new();
    while let Ok(outgoing) = rx.try_recv() {
      if let Outgoing::Event(event) = outgoing {
        events.push(event);
      }
    }
    events
  }

  /// A link that drops everything.
  struct QuietLink;

  impl ParticipantLink for QuietLink {
    fn send_datagram(&self, _bytes: &[u8]) {}
    fn close(&self, _code: u32, _reason: &[u8]) {}
  }

  fn entry(user: i64, device: i64) -> CallRosterEntry {
    CallRosterEntry {
      user_id: id(user),
      device_id: id(device),
    }
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
