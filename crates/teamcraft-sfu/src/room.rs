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
//!
//! A connected participant can also move its session to another connection without leaving its
//! slot at all ([`Room::switch_link`], #1309): nothing re-keys and the others hear nothing of it.

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

use teamcraft_media::{
  datagram::{PADDING_STREAM, StreamId},
  dave::DaveEpoch,
  quic_send::QuicSender,
  send_queue::SendQueueConfig,
};
use teamcraft_proto::{
  Base64Bytes, CallClientMessage, CallEvent, CallId, CallParticipant, CallRosterEntry, ConversationId,
  DaveTransitionId, DaveTransitionMode, DeviceId, ErrorBody, ErrorCode, MAX_SIMULCAST_LAYERS,
  MediaCapabilities, MediaStreamId, SFU_GOING_AWAY_CLOSE_CODE, ScreenShareTakeover, SfuCallIncidents,
  SfuCallRoster, SfuControlAction, TransportFeedback, UnixMillis, UserId, VideoCodec, VideoLayerInfo,
  VideoPublicationId, VideoTrackKind,
};
use thiserror::Error;
use tokio::sync::Notify;
use tokio::sync::mpsc::{Sender, error::TrySendError};
use tracing::{debug, info, trace, warn};
use wtransport::Connection;

use crate::{
  auth::JoinGrant,
  config::SfuConfig,
  dave_coordinator::{DaveCoordinator, DaveCoordinatorError, DaveDeadlines, Outbound},
  forwarding::{Effect, ForwardError, Forwarder},
  quality::CallQuality,
  rate_limit::TokenBucket,
  signaling::Outgoing,
};

/// How long a closing participant has to receive the host's event before the connection drops.
const CLOSE_GRACE: Duration = Duration::from_millis(250);
/// How long a close waits for the peer to confirm the WebTransport session close.
pub(crate) const SESSION_CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
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
  /// How the calls go, in aggregates (#1648).
  quality: Arc<CallQuality>,
}

/// The rooms in numbers, for the metrics endpoint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RoomStats {
  pub rooms: usize,
  pub participants: usize,
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
  max_devices_per_user: usize,
  mute_report_burst: u32,
  mute_reports_per_second: u32,
  state: Mutex<RoomState>,
  /// Wakes the lost sessions waiting through the reconnect grace when a device joins: the one whose
  /// device took its slot back stops waiting ([`Room::wait_for_return`]).
  joins: Notify,
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
  /// The video codecs each device was last told the others decode (#565); a device without an
  /// entry hears them with its next (re)join.
  decodable_announced: HashMap<DeviceId, Vec<VideoCodec>>,
  /// Each device's last screen share another device's replaced, kept for the call (one per
  /// device, also after it left): a device that was away when it happened hears it with its next
  /// `joined` and doesn't publish that share again. Dropped when the device shares again.
  screen_shares_taken_over: HashMap<DeviceId, ScreenShareTakeover>,
  quality: Arc<CallQuality>,
}

/// What releasing a session's slot did.
struct Release {
  has_left: bool,
  is_room_empty: bool,
}

/// What taking back a slot on a new connection did.
#[derive(Default)]
struct Resumed {
  /// The others saw the device reconnecting.
  was_reconnecting: bool,
  /// Signaling for the device, after its `joined`.
  effects: Vec<Effect>,
}

/// A join on its way into the room.
struct Joining {
  grant: JoinGrant,
  capabilities: MediaCapabilities,
  dave_transition_mode: DaveTransitionMode,
  /// A rejoin's epoch of the DAVE group the device still holds.
  maybe_dave_resume_epoch: Option<DaveEpoch>,
  handle: ParticipantHandle,
  session: SessionId,
  maybe_switch_secret: Option<Base64Bytes>,
}

/// What a (re)joined participant's `joined` says besides the call's state.
struct Welcome {
  dave: WelcomeDave,
  reconnect_grace: Duration,
  maybe_switch_secret: Option<Base64Bytes>,
}

/// How a joined device gets into the call's DAVE group.
enum WelcomeDave {
  /// The external sender, for a new key package.
  ExternalSender(Vec<u8>),
  /// It carries on in the group it holds, at this epoch.
  Resumed(DaveEpoch),
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
  /// How the current connection applies DAVE transitions (its `join`).
  dave_transition_mode: DaveTransitionMode,
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
  #[error("the account already has {0} devices in the call")]
  TooManyDevices(usize),
  #[error("call {0} is not on this SFU")]
  CallNotFound(CallId),
  /// The session to move isn't the device's connected one any more (lost, replaced or left), or
  /// its signaling queue is full.
  #[error("participant {0:?} has no connection to move")]
  NotSwitchable(DeviceId),
}

/// A participant's QUIC connection with its media going through the queue of
/// `teamcraft_media::quic_send` (#1218): after a sudden drop of the receiver's downlink, what it
/// can't take in time is dropped here instead of being delivered seconds late.
pub struct QuicLink {
  connection: Connection,
  sender: QuicSender,
}

impl QuicLink {
  /// Starts the connection's sending task (on the current tokio runtime).
  pub fn new(connection: Connection) -> Self {
    let sender = QuicSender::spawn(connection.clone(), SendQueueConfig::downlink());
    Self { connection, sender }
  }
}

impl ParticipantLink for QuicLink {
  fn send_datagram(&self, bytes: &[u8]) {
    self.sender.send_datagram(bytes);
  }

  fn close(&self, code: u32, reason: &[u8]) {
    ParticipantLink::close(&self.connection, code, reason);
  }

  fn close_gracefully<'a>(
    &'a self,
    code: u32,
    reason: &'a [u8],
  ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
    ParticipantLink::close_gracefully(&self.connection, code, reason)
  }
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
      quality: Arc::new(CallQuality::new()),
    }
  }

  /// How the calls go, in aggregates, for the sessions and the metrics endpoint.
  pub fn quality(&self) -> &Arc<CallQuality> {
    &self.quality
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

  /// How many calls run here and how many devices are in them, reconnecting ones included. One pass
  /// over the rooms, for a scrape; nothing that forwards media calls this.
  pub fn stats(&self) -> RoomStats {
    // Lock order: rooms, then room state; the rooms lock is released first.
    let rooms: Vec<Arc<Room>> = lock(&self.rooms).values().cloned().collect();
    RoomStats {
      rooms: rooms.len(),
      participants: rooms.iter().map(|room| room.participant_count()).sum(),
    }
  }

  /// Adds a participant to a call, creating the room on first join. A device that is already in
  /// the call takes its slot back on the new connection; a locked call still lets it back in. Its
  /// `joined` carries `maybe_switch_secret` (#1309).
  pub fn join(
    &self,
    grant: JoinGrant,
    capabilities: MediaCapabilities,
    dave_transition_mode: DaveTransitionMode,
    maybe_dave_resume_epoch: Option<DaveEpoch>,
    handle: ParticipantHandle,
    maybe_switch_secret: Option<Base64Bytes>,
  ) -> Result<(Arc<Room>, SessionId, JoinOutcome), RoomError> {
    let session = SessionId(self.next_session.fetch_add(1, Ordering::Relaxed));
    // The rooms lock is held across the join so `leave` can't drop the room in between.
    // Lock order everywhere: rooms, then control, then room state.
    let mut rooms = lock(&self.rooms);
    let call_id = running_call_of(&rooms, &grant);
    if call_id != grant.call_id {
      // One conversation, one room: a server that lost track of the call (it restarted) handed out
      // a new call id. The device joins the running call; its `joined` names the call id it is in,
      // and the server learns it from the reports. The token was verified as usual; only the room
      // it opens is the one its signed conversation already has.
      info!(
        token_call_id = %grant.call_id,
        call_id = %call_id,
        "join for a new call of a conversation that has one: joins the running call"
      );
    }
    let grant = JoinGrant { call_id, ..grant };
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
        let room = Arc::new(Room::new(
          &grant,
          unix_now_millis(),
          &self.config,
          Arc::clone(&self.quality),
        )?);
        rooms.insert(call_id, Arc::clone(&room));
        info!(call_id = %call_id, "room created");
        room
      },
    };
    let joining = Joining {
      grant,
      capabilities,
      dave_transition_mode,
      maybe_dave_resume_epoch,
      handle,
      session,
      maybe_switch_secret,
    };
    let result = room.join(joining, Instant::now());
    if result.is_err() && room.is_empty() {
      rooms.remove(&call_id);
    }
    Ok((room, session, result?))
  }

  /// Moves `participant_id`'s session `from` to a new connection's `link` (#1309); returns the new
  /// session's id and the sender of the session's signaling queue, which goes along.
  pub fn switch(
    &self,
    room: &Room,
    participant_id: DeviceId,
    from: SessionId,
    link: Arc<dyn ParticipantLink>,
  ) -> Result<(SessionId, Sender<Outgoing>), RoomError> {
    let session = SessionId(self.next_session.fetch_add(1, Ordering::Relaxed));
    let signal_tx = room.switch_link(participant_id, from, link, session)?;
    Ok((session, signal_tx))
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
  fn new(
    first: &JoinGrant,
    now: UnixMillis,
    config: &SfuConfig,
    quality: Arc<CallQuality>,
  ) -> Result<Self, RoomError> {
    let call_id = first.call_id;
    let state = RoomState {
      participants: HashMap::new(),
      latest_sessions: HashMap::new(),
      forwarder: Forwarder::new(config.forwarding),
      dave: DaveCoordinator::new(
        call_id,
        DaveDeadlines {
          transition: config.dave_transition_deadline,
          commit: config.dave_commit_deadline,
          refusal: config.dave_refusal_window,
          resume: config.dave_resume_grace,
        },
      )?,
      next_stream_id: 1,
      decodable_announced: HashMap::new(),
      screen_shares_taken_over: HashMap::new(),
      quality,
    };
    Ok(Self {
      call_id,
      maybe_conversation_id: first.maybe_conversation_id,
      started_by: first.user_id,
      started_at: now,
      reconnect_grace: config.reconnect_grace,
      max_devices_per_user: config.max_devices_per_user_per_call,
      mute_report_burst: config.mute_report_burst,
      mute_reports_per_second: config.mute_reports_per_second,
      state: Mutex::new(state),
      joins: Notify::new(),
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
          now,
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
          now,
        );
        Ok(())
      },
      CallClientMessage::DaveTransitionReady { transition_id } => {
        self.on_transition_ready(participant_id, transition_id, now);
        Ok(())
      },
      CallClientMessage::Join { .. } => Err(ErrorBody::new(ErrorCode::ProtocolViolation, "already joined")),
      // Only the first message of a connection moves a session (#1309).
      CallClientMessage::Switch { .. } => Err(ErrorBody::new(
        ErrorCode::ProtocolViolation,
        "already in the call",
      )),
      CallClientMessage::TransportFeedback { feedback } => {
        self.on_transport_feedback(participant_id, &feedback, now);
        Ok(())
      },
      CallClientMessage::PublishVideo {
        kind,
        codec,
        layers,
        maybe_publication_id,
      } => rejected(
        participant_id,
        self.publish_video(participant_id, kind, codec, layers, maybe_publication_id),
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
      CallClientMessage::RequestKeyframe { stream_id } => {
        self.request_keyframe(participant_id, stream_id, now);
        Ok(())
      },
      CallClientMessage::Nack {
        stream_id,
        layer,
        sequence_numbers,
      } => {
        self.resend(participant_id, stream_id, layer, &sequence_numbers, now);
        Ok(())
      },
      CallClientMessage::UplinkLayers { stream_id, layers } => rejected(
        participant_id,
        self.set_uplink_layers(participant_id, stream_id, &layers, now),
      ),
      CallClientMessage::SetMuted { is_muted } => {
        self.set_muted(participant_id, is_muted, now);
        Ok(())
      },
      CallClientMessage::MoveHere => {
        self.close_other_devices_of(participant_id);
        Ok(())
      },
      // The session answers pings itself (`server.rs`): they are about its path, not the call.
      CallClientMessage::Ping { .. } => Ok(()),
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
      // One atomic add per datagram, whatever the number of receivers.
      let forwarded_bytes = route.deliveries.iter().map(|(_, bytes)| bytes.len()).sum();
      state.quality.forwarded(forwarded_bytes);
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

  /// Publishes a video track of `owner` and announces it to everyone. The track echoes the
  /// owner's `maybe_publication_id`, also in later `joined` events, so the owner knows which of its
  /// requests it answers (#929).
  pub fn publish_video(
    &self,
    owner: DeviceId,
    kind: VideoTrackKind,
    codec: VideoCodec,
    layers: Vec<VideoLayerInfo>,
    maybe_publication_id: Option<VideoPublicationId>,
  ) -> Result<(), RoomError> {
    let mut state = lock(&self.state);
    let stream_id = state.allocate_stream_id();
    let track = state
      .forwarder
      .publish_video(owner, stream_id, kind, codec, layers, maybe_publication_id)?;
    if track.kind == VideoTrackKind::Screen {
      state.end_other_screen_shares(self.call_id, owner, track.stream_id);
    }
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

  /// A receiver can't decode a track until its next keyframe; a request for a track it doesn't
  /// receive (unpublished meanwhile) is no error, there is just nothing to ask for.
  pub fn request_keyframe(&self, receiver: DeviceId, stream_id: MediaStreamId, now: Instant) {
    let mut state = lock(&self.state);
    let effects = state
      .forwarder
      .request_keyframe(receiver, StreamId(stream_id.0), now);
    state.apply(effects);
  }

  /// Resends the video packets `receiver` names that are still kept, to it alone (#1762). A NACK
  /// for a track it no longer receives is no error: it crossed a layer switch or an unpublish.
  pub fn resend(
    &self,
    receiver: DeviceId,
    stream_id: MediaStreamId,
    layer: u8,
    sequence_numbers: &[u16],
    now: Instant,
  ) {
    let (link, datagrams) = {
      let mut state = lock(&self.state);
      let datagrams = state
        .forwarder
        .resend(receiver, StreamId(stream_id.0), layer, sequence_numbers, now);
      if datagrams.is_empty() {
        return;
      }
      state.quality.retransmitted(datagrams.len());
      let Some(handle) = state.handle_of(receiver) else {
        return;
      };
      (Arc::clone(&handle.link), datagrams)
    };
    for datagram in &datagrams {
      link.send_datagram(datagram);
    }
  }

  /// The publisher's uplink carries only `layers` of its track. A track unpublished meanwhile is
  /// no error (the message crossed the unpublish).
  pub fn set_uplink_layers(
    &self,
    owner: DeviceId,
    stream_id: MediaStreamId,
    layers: &[u8],
    now: Instant,
  ) -> Result<(), RoomError> {
    let mut state = lock(&self.state);
    match state
      .forwarder
      .set_uplink_layers(owner, StreamId(stream_id.0), layers, now)
    {
      Ok(effects) => {
        state.apply(effects);
        Ok(())
      },
      Err(ForwardError::UnknownTrack(_)) => Ok(()),
      Err(error) => Err(error.into()),
    }
  }

  pub fn on_transport_feedback(&self, receiver: DeviceId, feedback: &TransportFeedback, now: Instant) {
    let mut state = lock(&self.state);
    let effects = state.forwarder.on_transport_feedback(receiver, feedback, now);
    state.apply(effects);
  }

  /// Periodic work for one participant (every ~100 ms): re-runs its layer allocation, and executes
  /// a DAVE transition or starts the DAVE group over when a deadline passed (any participant's
  /// timer does; the check is cheap).
  pub fn on_timer(&self, device_id: DeviceId, now: Instant) {
    let mut state = lock(&self.state);
    let effects = state.forwarder.on_timer(device_id, now);
    state.apply(effects);
    let outbound = state.dave.on_timer(now);
    state.dispatch(outbound);
    let incidents = state.dave.take_incidents();
    state.quality.incidents(incidents);
  }

  /// Feedback on the datagrams `sender` sent since the last call, for its uplink estimation.
  pub fn take_uplink_feedback(&self, sender: DeviceId, now: Instant) -> Option<TransportFeedback> {
    lock(&self.state).forwarder.take_uplink_feedback(sender, now)
  }

  pub fn on_key_package(
    &self,
    participant_id: DeviceId,
    key_package: Vec<u8>,
    maybe_key_binding: Option<Vec<u8>>,
    now: Instant,
  ) -> Result<(), RoomError> {
    let mut state = lock(&self.state);
    let transition_mode = state
      .participants
      .get(&participant_id)
      .map_or(DaveTransitionMode::AtOnce, |participant| {
        participant.dave_transition_mode
      });
    let outbound = state.dave.on_key_package(
      participant_id,
      key_package,
      maybe_key_binding,
      transition_mode,
      now,
    )?;
    state.dispatch(outbound);
    Ok(())
  }

  pub fn on_transition_ready(&self, participant_id: DeviceId, transition_id: DaveTransitionId, now: Instant) {
    let mut state = lock(&self.state);
    let outbound = state.dave.on_transition_ready(participant_id, transition_id, now);
    state.dispatch(outbound);
  }

  pub fn on_commit(
    &self,
    participant_id: DeviceId,
    epoch: u64,
    commit: Vec<u8>,
    maybe_welcome: Option<Vec<u8>>,
    now: Instant,
  ) {
    let mut state = lock(&self.state);
    let outbound = state
      .dave
      .on_commit(participant_id, DaveEpoch(epoch), commit, maybe_welcome, now);
    state.dispatch(outbound);
  }

  fn join(&self, joining: Joining, now: Instant) -> Result<JoinOutcome, RoomError> {
    let Joining {
      grant,
      capabilities,
      dave_transition_mode,
      maybe_dave_resume_epoch,
      handle,
      session,
      maybe_switch_secret,
    } = joining;
    let participant_id = grant.device_id;
    let mut state = lock(&self.state);
    let package = state.dave.external_sender_package();
    let outcome = match state.participants.get(&participant_id) {
      Some(existing) if existing.user_id != grant.user_id => {
        return Err(RoomError::AlreadyJoined(participant_id));
      },
      Some(_) => {
        let resumed = state.resume(participant_id, capabilities, handle, session, now);
        if let Some(participant) = state.participants.get_mut(&participant_id) {
          participant.dave_transition_mode = dave_transition_mode;
        }
        // Back with the group as it was: it carries on and nobody re-keys (e2ee.md §8).
        let (is_dave_resumed, outbound) = state.dave.on_resume(participant_id, maybe_dave_resume_epoch, now);
        state.dispatch(outbound);
        let dave = match maybe_dave_resume_epoch {
          Some(epoch) if is_dave_resumed => WelcomeDave::Resumed(epoch),
          _ => WelcomeDave::ExternalSender(package),
        };
        let welcome = Welcome {
          dave,
          reconnect_grace: self.reconnect_grace,
          maybe_switch_secret,
        };
        state.welcome(self.call_id, participant_id, welcome);
        // Its capabilities may differ on the new connection: the others' codecs may change too.
        state.announce_decodable_codecs(Some(participant_id));
        // After `joined`: what its tracks need now, which may have changed while it was away.
        state.apply(resumed.effects);
        if resumed.was_reconnecting {
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
        // A device that holds its slot while reconnecting counts: it comes back to it.
        let devices_of_user = state
          .participants
          .values()
          .filter(|participant| participant.user_id == grant.user_id)
          .count();
        if devices_of_user >= self.max_devices_per_user {
          return Err(RoomError::TooManyDevices(devices_of_user));
        }
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
            dave_transition_mode,
          },
        );
        let welcome = Welcome {
          dave: WelcomeDave::ExternalSender(package),
          reconnect_grace: self.reconnect_grace,
          maybe_switch_secret,
        };
        state.welcome(self.call_id, participant_id, welcome);
        let info = state.info(participant_id);
        if let Some(participant) = info {
          state.broadcast_except(participant_id, CallEvent::ParticipantJoined { participant });
        }
        state.announce_decodable_codecs(Some(participant_id));
        JoinOutcome::Joined
      },
    };
    state.latest_sessions.insert(participant_id, session);
    drop(state);
    self.joins.notify_waiters();
    info!(call_id = %self.call_id, participant_id = %participant_id, ?outcome, "participant joined");
    Ok(outcome)
  }

  /// Waits through the reconnect `grace` of `session`, a lost one of `participant_id`, but no longer
  /// than the session holds the slot: when the device is back on a newer one, there is nothing
  /// left to wait for.
  pub async fn wait_for_return(&self, participant_id: DeviceId, session: SessionId, grace: Duration) {
    let deadline = tokio::time::sleep(grace);
    tokio::pin!(deadline);
    loop {
      let joined = self.joins.notified();
      tokio::pin!(joined);
      // Registered before the check, so a join right after it still wakes this.
      joined.as_mut().enable();
      if !self.holds_slot(participant_id, session) {
        return;
      }
      tokio::select! {
        () = &mut deadline => return,
        () = &mut joined => {},
      }
    }
  }

  /// Moves a connected participant's session `from` to `link`, the connection of `to` (#1309): its
  /// slot, tracks, subscriptions and DAVE leaf stay as they are, and the others hear nothing of
  /// it. From now on media goes over `link`, and `to` is the session whose end counts, so the old
  /// connection's end is no leave and no loss any more. The signaling queue gets
  /// [`Outgoing::HandOver`]: the old connection's writer sends what was queued before it and then
  /// hands the queue on, so the events go on without a gap. Returns the queue's sender, for the
  /// new session's own messages.
  ///
  /// Refused unless `from` is the device's connected session: one that was lost meanwhile has left
  /// the DAVE group, and its device rejoins.
  pub fn switch_link(
    &self,
    participant_id: DeviceId,
    from: SessionId,
    link: Arc<dyn ParticipantLink>,
    to: SessionId,
  ) -> Result<Sender<Outgoing>, RoomError> {
    let mut state = lock(&self.state);
    let Some(participant) = state
      .participants
      .get_mut(&participant_id)
      .filter(|participant| participant.session == from)
    else {
      return Err(RoomError::NotSwitchable(participant_id));
    };
    let Presence::Connected(handle) = &mut participant.presence else {
      return Err(RoomError::NotSwitchable(participant_id));
    };
    // Full: the old connection doesn't read, and is about to be dropped for it.
    if handle.signal_tx.try_send(Outgoing::HandOver).is_err() {
      return Err(RoomError::NotSwitchable(participant_id));
    }
    // The old connection's datagrams still queued there may get lost: media tolerates that.
    handle.link = link;
    let signal_tx = handle.signal_tx.clone();
    participant.session = to;
    state.latest_sessions.insert(participant_id, to);
    drop(state);
    info!(call_id = %self.call_id, participant_id = %participant_id, "participant moved to another connection");
    Ok(signal_tx)
  }

  fn holds_slot(&self, participant_id: DeviceId, session: SessionId) -> bool {
    lock(&self.state)
      .participants
      .get(&participant_id)
      .is_some_and(|participant| participant.session == session)
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
    // The device keeps its DAVE leaf for a short while: back with the group as it was, nobody
    // re-keys; otherwise it leaves the group then (e2ee.md §8).
    let outbound = state.dave.on_lost(participant_id, now);
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

  fn participant_count(&self) -> usize {
    lock(&self.state).participants.len()
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
  fn welcome(&self, call_id: CallId, participant_id: DeviceId, welcome: Welcome) {
    let Welcome {
      dave,
      reconnect_grace,
      maybe_switch_secret,
    } = welcome;
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
        maybe_max_simulcast_layers: Some(MAX_SIMULCAST_LAYERS),
        maybe_switch_secret,
        maybe_screen_share_taken_over: self.screen_shares_taken_over.get(&participant_id).copied(),
        maybe_retransmit_window_ms: self.forwarder.retransmit_window_ms(),
      },
    );
    // Right after `joined`: a device that rejoins with a group held takes anything else as "not
    // resumed" (e2ee.md §8, "Short reconnects").
    let dave_event = match dave {
      WelcomeDave::ExternalSender(package) => CallEvent::DaveExternalSender {
        package: Base64Bytes::new(package),
      },
      WelcomeDave::Resumed(epoch) => CallEvent::DaveResumed { epoch: epoch.0 },
    };
    self.send_to(participant_id, dave_event);
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
  /// connection still thought alive is replaced (the device knows better: it is gone).
  fn resume(
    &mut self,
    participant_id: DeviceId,
    capabilities: MediaCapabilities,
    handle: ParticipantHandle,
    session: SessionId,
    now: Instant,
  ) -> Resumed {
    let Some(participant) = self.participants.get_mut(&participant_id) else {
      return Resumed::default();
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
        // The new connection may carry on in the group (`Room::join` asks the coordinator).
        let outbound = self.dave.on_lost(participant_id, now);
        self.dispatch(outbound);
        false
      },
      Presence::Reconnecting => true,
    };
    let effects = self.forwarder.resume(participant_id, capabilities, now);
    Resumed {
      was_reconnecting,
      effects,
    }
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
    let outbound = self.dave.on_leave(participant_id, now);
    self.dispatch(outbound);
    self.decodable_announced.remove(&participant_id);
    self.announce_decodable_codecs(None);
    info!(call_id = %call_id, participant_id = %participant_id, "participant left");
  }

  /// A call has one screen share at a time (ARCHITECTURE.md §6.3): `sharer`'s new share on
  /// `stream_id` ends everyone else's, with its sound. Each replaced device hears who took over
  /// before the others see its share go, so it stops capturing instead of publishing again; one
  /// that is away hears it with its next `joined`. Called under the room's lock, so of two shares
  /// started at about the same moment the one whose request came last wins.
  fn end_other_screen_shares(&mut self, call_id: CallId, sharer: DeviceId, stream_id: MediaStreamId) {
    let Some(by_user_id) = self
      .participants
      .get(&sharer)
      .map(|participant| participant.user_id)
    else {
      return;
    };
    // The sharer's own earlier takeover no longer matters: its new share is the current one.
    self.screen_shares_taken_over.remove(&sharer);
    let screen_audio: HashMap<DeviceId, StreamId> =
      self.forwarder.screen_audio_streams().into_iter().collect();
    for (owner, ended) in self.forwarder.end_other_screen_shares(sharer) {
      let takeover = ScreenShareTakeover {
        stream_id: MediaStreamId(ended.0),
        by_user_id,
        by_device_id: sharer,
      };
      info!(
        call_id = %call_id,
        participant_id = %owner,
        by = %sharer,
        stream_id = ended.0,
        new_stream_id = stream_id.0,
        "screen share taken over"
      );
      self.screen_shares_taken_over.insert(owner, takeover);
      self.send_to(owner, CallEvent::ScreenShareTakenOver(takeover));
      self.broadcast(CallEvent::VideoUnpublished {
        stream_id: takeover.stream_id,
      });
      if let Some(sound) = screen_audio.get(&owner)
        && self.forwarder.unpublish_screen_audio(owner, *sound).is_ok()
      {
        self.broadcast(CallEvent::ScreenAudioUnpublished {
          stream_id: MediaStreamId(sound.0),
        });
      }
    }
  }

  /// The next free stream id. The counter wraps after 2^32 publications; ids still in use are
  /// skipped, so a new stream never takes over someone's (their microphone, say).
  fn allocate_stream_id(&mut self) -> StreamId {
    loop {
      let stream_id = StreamId(self.next_stream_id);
      self.next_stream_id = self.next_stream_id.wrapping_add(1);
      if stream_id != PADDING_STREAM && !self.forwarder.is_stream_in_use(stream_id) {
        return stream_id;
      }
    }
  }

  /// Sends the signaling (and the probing padding) a forwarding decision caused.
  fn apply(&self, effects: Vec<Effect>) {
    for effect in effects {
      match effect {
        Effect::ActiveSpeakers(device_ids) => self.broadcast(CallEvent::ActiveSpeakers { device_ids }),
        Effect::KeyframeRequest {
          publisher,
          stream_id,
          layer,
        } => {
          self.quality.keyframe_requested();
          self.send_to(
            publisher,
            CallEvent::KeyframeRequested {
              stream_id: MediaStreamId(stream_id.0),
              layer,
            },
          );
        },
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
        // A few datagrams every few seconds at most, queued without blocking.
        Effect::Padding { receiver, datagrams } => {
          if let Some(handle) = self.handle_of(receiver) {
            for datagram in &datagrams {
              handle.link.send_datagram(datagram);
            }
          }
        },
      }
    }
  }

  /// Tells each connected participant that encodes video the video codecs the others decode
  /// (#565) when they changed since it was last told, and `maybe_fresh` (a device that just
  /// joined or rejoined) in any case. A reconnecting device hears them when it is back; one that
  /// sends no video (audio only) never does.
  fn announce_decodable_codecs(&mut self, maybe_fresh: Option<DeviceId>) {
    let devices: Vec<DeviceId> = self.participants.keys().copied().collect();
    for device_id in devices {
      let maybe_codecs = self.forwarder.decodable_video_codecs(device_id);
      let Some(codecs) = maybe_codecs.filter(|_| self.handle_of(device_id).is_some()) else {
        self.decodable_announced.remove(&device_id);
        continue;
      };
      if maybe_fresh != Some(device_id) && self.decodable_announced.get(&device_id) == Some(&codecs) {
        continue;
      }
      self.send_to(
        device_id,
        CallEvent::DecodableVideoCodecs {
          codecs: codecs.clone(),
        },
      );
      self.decodable_announced.insert(device_id, codecs);
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
      self.send_counted(handle, event);
    }
  }

  /// Sends, counting a participant dropped for a full signaling queue as an incident.
  fn send_counted(&self, handle: &ParticipantHandle, event: CallEvent) {
    if send(handle, event) == Sent::Overflowed {
      self.quality.incidents(SfuCallIncidents {
        signaling_overflows: 1,
        ..SfuCallIncidents::default()
      });
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
        Some(handle) => self.send_counted(handle, event),
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
    // They are closed anyway; an overflow says nothing new.
    let _ = send(handle, event.clone());
  }
  tokio::spawn(async move {
    tokio::time::sleep(CLOSE_GRACE).await;
    for handle in handles {
      handle.link.close(code, reason);
    }
  });
}

/// How a [`send`] went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sent {
  Queued,
  /// The participant's queue was full: it was disconnected.
  Overflowed,
  Closed,
}

#[must_use]
fn send(handle: &ParticipantHandle, event: CallEvent) -> Sent {
  match handle.signal_tx.try_send(Outgoing::Event(event)) {
    Ok(()) => Sent::Queued,
    // A participant that doesn't drain its signaling stream would make the queue grow without bound.
    Err(TrySendError::Full(_)) => {
      warn!("signaling queue full, disconnecting the participant");
      handle.link.close(SIGNALING_OVERFLOW, b"signaling queue overflow");
      Sent::Overflowed
    },
    // The receiver only disappears when the session ends, which also removes the participant.
    Err(TrySendError::Closed(_)) => {
      warn!("signaling channel closed");
      Sent::Closed
    },
  }
}

/// Recovers from poisoning: room state stays consistent because no invariant spans a panic point.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The call `grant` joins: its own if it runs here, else the running call of its (signed)
/// conversation, else its own (a new room).
fn running_call_of(rooms: &HashMap<CallId, Arc<Room>>, grant: &JoinGrant) -> CallId {
  if rooms.contains_key(&grant.call_id) {
    return grant.call_id;
  }
  let Some(conversation_id) = grant.maybe_conversation_id else {
    return grant.call_id;
  };
  rooms
    .values()
    .filter(|room| room.maybe_conversation_id == Some(conversation_id))
    // Deterministic should there ever be two (rooms opened by tokens of an older server).
    .map(|room| room.call_id)
    .min()
    .unwrap_or(grant.call_id)
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

  #[test]
  fn the_stats_count_rooms_and_devices() {
    let rooms = Rooms::new(SfuConfig::default());
    assert_eq!(rooms.stats(), RoomStats::default());
    let (_alice_rx, _) = join(&rooms, 1, 10);
    let (_bob_rx, _) = join(&rooms, 2, 20);
    let stats = rooms.stats();
    assert_eq!((stats.rooms, stats.participants), (1, 2));
  }

  mod one_room_per_conversation {
    use super::*;

    #[test]
    fn a_new_call_id_for_a_running_conversation_lands_in_its_room() {
      let rooms = Rooms::new(SfuConfig::default());
      let (_alice_rx, _) = join(&rooms, 1, 10);

      // A restarted server handed out a fresh call id for the same conversation.
      let (mut bob_rx, _, joined) = join_with(&rooms, grant(55, Some(CONVERSATION), 2, 20)).unwrap();

      assert_eq!(joined, id(CALL));
      let roster = rooms.roster();
      assert_eq!(roster.len(), 1, "{roster:?}");
      assert_eq!(roster[0].participants.len(), 2);
      let events = drain(&mut bob_rx);
      assert!(
        events
          .iter()
          .any(|event| matches!(event, CallEvent::Joined { call_id, .. } if *call_id == id(CALL))),
        "the client learns the call it is in: {events:?}"
      );
    }

    #[test]
    fn other_conversations_and_unnamed_tokens_get_their_own_room() {
      let rooms = Rooms::new(SfuConfig::default());
      join(&rooms, 1, 10);

      let (_, _, other) = join_with(&rooms, grant(56, Some(78), 2, 20)).unwrap();
      let (_, _, unnamed) = join_with(&rooms, grant(57, None, 3, 30)).unwrap();

      assert_eq!((other, unnamed), (id(56), id(57)));
      assert_eq!(rooms.roster().len(), 3);
    }

    #[tokio::test]
    async fn an_account_may_have_only_so_many_devices_in_one_call() {
      let rooms = Rooms::new(SfuConfig {
        max_devices_per_user_per_call: 2,
        ..SfuConfig::default()
      });
      join(&rooms, 1, 10);
      let (_, phone_session) = join(&rooms, 1, 11);

      let third = join_with(&rooms, grant(CALL, Some(CONVERSATION), 1, 12));
      let other_account = join_with(&rooms, grant(CALL, Some(CONVERSATION), 2, 20));

      assert!(matches!(third, Err(RoomError::TooManyDevices(2))));
      assert!(other_account.is_ok(), "the limit is per account");

      // A device that is in the call already rejoins, also while it holds its slot reconnecting.
      let room = lock(&rooms.rooms).get(&id(CALL)).cloned().unwrap();
      assert!(rooms.disconnect(&room, id(11), phone_session, Instant::now()));
      assert!(join_with(&rooms, grant(CALL, Some(CONVERSATION), 1, 11)).is_ok());
    }

    #[tokio::test]
    async fn host_controls_of_the_running_call_still_apply() {
      let rooms = Rooms::new(SfuConfig::default());
      join(&rooms, 1, 10);
      join(&rooms, 2, 20);
      rooms
        .control(
          id(CALL),
          id(1),
          &SfuControlAction::RemoveParticipant { user_id: id(2) },
        )
        .unwrap();
      rooms.control(id(CALL), id(1), &SfuControlAction::Lock).unwrap();

      let removed = join_with(&rooms, grant(55, Some(CONVERSATION), 2, 21));
      let locked_out = join_with(&rooms, grant(55, Some(CONVERSATION), 3, 30));

      assert!(matches!(removed, Err(RoomError::Removed)));
      assert!(matches!(locked_out, Err(RoomError::Locked)));
    }
  }

  /// A lost session waits through the grace period only while its device hasn't come back (#1227):
  /// a device that loops connect, join, drop leaves at most one waiting behind, not one per cycle.
  #[tokio::test(start_paused = true)]
  async fn a_lost_session_stops_waiting_once_its_device_is_back() {
    let grace = Duration::from_secs(30);
    let rooms = Rooms::new(SfuConfig::default());
    let (_rx, lost) = join(&rooms, 1, 10);
    let room = lock(&rooms.rooms).get(&id(CALL)).cloned().unwrap();
    assert!(rooms.disconnect(&room, id(10), lost, Instant::now()));
    let waiting = {
      let room = Arc::clone(&room);
      tokio::spawn(async move { room.wait_for_return(id(10), lost, grace).await })
    };
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!waiting.is_finished(), "nobody came back yet");

    let (_rx, _back) = join(&rooms, 1, 10);
    tokio::time::timeout(Duration::from_millis(10), waiting)
      .await
      .expect("the device is back: nothing left to wait for")
      .unwrap();
    // And the late cleanup leaves the device in its slot.
    assert!(!rooms.leave(&room, id(10), lost));
    assert_eq!(rooms.stats().participants, 1);
  }

  #[tokio::test(start_paused = true)]
  async fn a_lost_session_whose_device_stays_away_waits_the_whole_grace() {
    let grace = Duration::from_secs(30);
    let rooms = Rooms::new(SfuConfig::default());
    let (_rx, lost) = join(&rooms, 1, 10);
    // Somebody else coming and going doesn't end the wait.
    let (_bob_rx, _) = join(&rooms, 2, 20);
    let room = lock(&rooms.rooms).get(&id(CALL)).cloned().unwrap();
    assert!(rooms.disconnect(&room, id(10), lost, Instant::now()));
    let started = tokio::time::Instant::now();
    let waiting = {
      let room = Arc::clone(&room);
      tokio::spawn(async move { room.wait_for_return(id(10), lost, grace).await })
    };
    tokio::time::sleep(Duration::from_secs(1)).await;
    join(&rooms, 3, 30);
    waiting.await.unwrap();
    assert_eq!(started.elapsed(), grace);
    assert!(rooms.leave(&room, id(10), lost), "the slot goes after the grace");
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

  /// A session moves to another connection without leaving its slot (#1309).
  mod switch {
    use super::*;

    #[test]
    fn the_others_hear_nothing_and_the_queue_is_handed_over() {
      let rooms = Rooms::new(SfuConfig::default());
      let (mut alice_rx, alice_session) = join(&rooms, 1, 10);
      let (mut bob_rx, _) = join(&rooms, 2, 20);
      let room = room_of(&rooms);
      drain(&mut alice_rx);
      drain(&mut bob_rx);

      let (moved_to, _signal_tx) = rooms
        .switch(&room, id(10), alice_session, Arc::new(QuietLink))
        .unwrap();

      assert!(drain(&mut bob_rx).is_empty(), "no reconnect, no leave, no re-key");
      assert_eq!(alice_rx.try_recv().ok(), Some(Outgoing::HandOver));
      assert!(room.holds_slot(id(10), moved_to));
      // Events for alice keep coming through the same queue, after the hand-over.
      room.set_muted(id(20), true, Instant::now());
      assert_eq!(
        drain(&mut alice_rx),
        vec![CallEvent::ParticipantMuted {
          device_id: id(20),
          is_muted: true
        }]
      );
    }

    #[test]
    fn the_old_connections_end_is_no_leave_and_no_loss() {
      let rooms = Rooms::new(SfuConfig::default());
      let (_alice_rx, alice_session) = join(&rooms, 1, 10);
      let (mut bob_rx, _) = join(&rooms, 2, 20);
      let room = room_of(&rooms);
      let (moved_to, _signal_tx) = rooms
        .switch(&room, id(10), alice_session, Arc::new(QuietLink))
        .unwrap();
      drain(&mut bob_rx);

      assert!(!rooms.disconnect(&room, id(10), alice_session, Instant::now()));
      assert!(!rooms.leave(&room, id(10), alice_session));
      assert!(drain(&mut bob_rx).is_empty());
      assert_eq!(rooms.stats().participants, 2);
      // The new connection's end counts.
      assert!(rooms.leave(&room, id(10), moved_to));
      assert_eq!(rooms.stats().participants, 1);
    }

    /// The old connection is lost just as the switch comes: one of them wins, and the device keeps
    /// exactly one slot.
    #[test]
    fn a_switch_racing_a_loss_keeps_one_slot() {
      // The loss first: the session left the DAVE group, its device rejoins instead.
      let rooms = Rooms::new(SfuConfig::default());
      let (_alice_rx, alice_session) = join(&rooms, 1, 10);
      let room = room_of(&rooms);
      assert!(rooms.disconnect(&room, id(10), alice_session, Instant::now()));
      assert!(matches!(
        rooms.switch(&room, id(10), alice_session, Arc::new(QuietLink)),
        Err(RoomError::NotSwitchable(_))
      ));
      assert_eq!(rooms.stats().participants, 1);
      assert!(room.holds_slot(id(10), alice_session), "kept for the rejoin");

      // The switch first: the loss of the old connection comes too late to matter.
      let rooms = Rooms::new(SfuConfig::default());
      let (_alice_rx, alice_session) = join(&rooms, 1, 10);
      let room = room_of(&rooms);
      let (moved_to, _signal_tx) = rooms
        .switch(&room, id(10), alice_session, Arc::new(QuietLink))
        .unwrap();
      assert!(!rooms.disconnect(&room, id(10), alice_session, Instant::now()));
      assert!(!rooms.leave(&room, id(10), alice_session));
      assert_eq!(rooms.stats().participants, 1);
      assert!(room.holds_slot(id(10), moved_to));
    }

    #[tokio::test]
    async fn only_the_connected_session_moves() {
      let rooms = Rooms::new(SfuConfig::default());
      let (_alice_rx, first) = join(&rooms, 1, 10);
      let room = room_of(&rooms);
      // The device came back on another connection: the first session is no more.
      let (_alice_rx, _second) = join(&rooms, 1, 10);
      assert!(matches!(
        rooms.switch(&room, id(10), first, Arc::new(QuietLink)),
        Err(RoomError::NotSwitchable(_))
      ));
      assert!(matches!(
        rooms.switch(&room, id(99), first, Arc::new(QuietLink)),
        Err(RoomError::NotSwitchable(_))
      ));
    }

    #[test]
    fn a_full_queue_refuses_the_move_and_changes_nothing() {
      let rooms = Rooms::new(SfuConfig::default());
      let (signal_tx, _signal_rx) = mpsc::channel(1);
      let handle = ParticipantHandle {
        link: Arc::new(QuietLink),
        signal_tx,
      };
      let (room, session, _) = rooms
        .join(
          grant(CALL, Some(CONVERSATION), 1, 10),
          MediaCapabilities::default(),
          DaveTransitionMode::Coordinated,
          None,
          handle,
          None,
        )
        .unwrap();
      // `joined` and the rest filled it.
      assert!(matches!(
        rooms.switch(&room, id(10), session, Arc::new(QuietLink)),
        Err(RoomError::NotSwitchable(_))
      ));
      assert!(room.holds_slot(id(10), session));
    }

    #[test]
    fn joined_carries_the_sessions_secret() {
      let rooms = Rooms::new(SfuConfig::default());
      let (signal_tx, mut signal_rx) = mpsc::channel(64);
      let handle = ParticipantHandle {
        link: Arc::new(QuietLink),
        signal_tx,
      };
      rooms
        .join(
          grant(CALL, Some(CONVERSATION), 1, 10),
          MediaCapabilities::default(),
          DaveTransitionMode::Coordinated,
          None,
          handle,
          Some(Base64Bytes::new(vec![3; 32])),
        )
        .unwrap();
      assert!(drain(&mut signal_rx).iter().any(|event| matches!(
        event,
        CallEvent::Joined { maybe_switch_secret: Some(secret), .. } if secret.as_slice() == [3; 32]
      )));
    }

    fn room_of(rooms: &Rooms) -> Arc<Room> {
      lock(&rooms.rooms).get(&id(CALL)).cloned().unwrap()
    }
  }

  mod decodable_codecs {
    use teamcraft_proto::KNOWN_VIDEO_CODECS;

    use super::*;

    fn decoding(codecs: &[VideoCodec]) -> MediaCapabilities {
      MediaCapabilities {
        video_encoders: vec![VideoCodec::Av1, VideoCodec::H264],
        video_decoders: codecs.to_vec(),
        ..MediaCapabilities::default()
      }
    }

    fn announced(events: &[CallEvent]) -> Vec<Vec<VideoCodec>> {
      events
        .iter()
        .filter_map(|event| match event {
          CallEvent::DecodableVideoCodecs { codecs } => Some(codecs.clone()),
          _ => None,
        })
        .collect()
    }

    /// A publisher hears what everyone else decodes: a newcomer that decodes less makes it fall
    /// back, and its leaving lets it use the better codec again (#565).
    #[test]
    fn a_publisher_hears_what_the_others_decode() {
      let rooms = Rooms::new(SfuConfig::default());
      let both = [VideoCodec::Av1, VideoCodec::H264];
      let (mut alice_rx, room, _) = join_capable(&rooms, 1, 10, decoding(&both));
      assert_eq!(
        announced(&drain(&mut alice_rx)),
        vec![KNOWN_VIDEO_CODECS.to_vec()],
        "alone: anything goes"
      );

      let (mut bob_rx, _, _) = join_capable(&rooms, 2, 20, decoding(&both));
      assert_eq!(announced(&drain(&mut alice_rx)), vec![both.to_vec()]);
      assert_eq!(announced(&drain(&mut bob_rx)), vec![both.to_vec()]);

      let (mut carol_rx, _, carol_session) = join_capable(&rooms, 3, 30, decoding(&[VideoCodec::H264]));
      assert_eq!(announced(&drain(&mut alice_rx)), vec![vec![VideoCodec::H264]]);
      assert_eq!(announced(&drain(&mut bob_rx)), vec![vec![VideoCodec::H264]]);
      assert_eq!(announced(&drain(&mut carol_rx)), vec![both.to_vec()]);

      // Audio-only devices (no decoders: older clients, dial-ins) don't hold anyone back, and
      // hear nothing of it (they publish no video).
      let (mut dave_rx, _, _) = join_capable(&rooms, 4, 40, MediaCapabilities::default());
      assert!(
        announced(&drain(&mut alice_rx)).is_empty(),
        "nothing changed for alice"
      );
      assert!(announced(&drain(&mut dave_rx)).is_empty());

      assert!(rooms.leave(&room, id(30), carol_session));
      assert_eq!(announced(&drain(&mut alice_rx)), vec![both.to_vec()]);
      assert_eq!(announced(&drain(&mut bob_rx)), vec![both.to_vec()]);
    }
  }

  /// Joins user `user` with device `device` that has `capabilities`; returns its signaling queue,
  /// the room and the session.
  fn join_capable(
    rooms: &Rooms,
    user: i64,
    device: i64,
    capabilities: MediaCapabilities,
  ) -> (Receiver<Outgoing>, Arc<Room>, SessionId) {
    let (signal_tx, signal_rx) = mpsc::channel(64);
    let handle = ParticipantHandle {
      link: Arc::new(QuietLink),
      signal_tx,
    };
    let (room, session, _) = rooms
      .join(
        grant(CALL, Some(CONVERSATION), user, device),
        capabilities,
        DaveTransitionMode::Coordinated,
        None,
        handle,
        None,
      )
      .unwrap();
    (signal_rx, room, session)
  }

  /// Joins user `user` with device `device`; returns its signaling queue and session.
  fn join(rooms: &Rooms, user: i64, device: i64) -> (Receiver<Outgoing>, SessionId) {
    let (signal_rx, session, _) = join_with(rooms, grant(CALL, Some(CONVERSATION), user, device)).unwrap();
    (signal_rx, session)
  }

  /// Joins with `grant`; also returns the call joined.
  fn join_with(
    rooms: &Rooms,
    grant: JoinGrant,
  ) -> Result<(Receiver<Outgoing>, SessionId, CallId), RoomError> {
    let (signal_tx, signal_rx) = mpsc::channel(64);
    let handle = ParticipantHandle {
      link: Arc::new(QuietLink),
      signal_tx,
    };
    let (room, session, _) = rooms.join(
      grant,
      MediaCapabilities::default(),
      DaveTransitionMode::Coordinated,
      None,
      handle,
      None,
    )?;
    Ok((signal_rx, session, room.call_id()))
  }

  fn grant(call: i64, maybe_conversation: Option<i64>, user: i64, device: i64) -> JoinGrant {
    JoinGrant {
      call_id: id(call),
      user_id: id(user),
      device_id: id(device),
      maybe_conversation_id: maybe_conversation.map(id),
    }
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
