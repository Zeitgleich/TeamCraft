//! Rooms (one per call): participants, datagram forwarding and DAVE coordination.
//!
//! State sits behind a `std::sync::Mutex` that is never held across an `.await`; the per-datagram
//! critical section parses headers and restamps the datagram per receiver. A lock-free forwarding
//! table is a later optimization (SPIKE.md).

use std::{
  collections::{HashMap, HashSet},
  sync::{Arc, Mutex, MutexGuard, PoisonError},
  time::Instant,
};

use teamcraft_media::{datagram::StreamId, dave::DaveEpoch};
use teamcraft_proto::{
  Base64Bytes, CallEvent, CallId, CallParticipant, DeviceId, MediaCapabilities, MediaStreamId,
  SfuControlAction, TransportFeedback, UserId, VideoCodec, VideoLayerInfo, VideoTrackKind,
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
  signaling::Outgoing,
};

/// How long a closing participant has to receive the host's event before the connection drops.
const CLOSE_GRACE: std::time::Duration = std::time::Duration::from_millis(250);
/// QUIC application close code for connections a meeting host ended.
const CLOSED_BY_HOST: u32 = 0x4801;
const SIGNALING_OVERFLOW: u32 = 0x4802;

/// All rooms of this SFU.
pub struct Rooms {
  config: SfuConfig,
  rooms: Mutex<HashMap<CallId, Arc<Room>>>,
  /// Host controls that outlive a room's participants: locked calls, removed users.
  control: Mutex<ControlState>,
}

#[derive(Default)]
struct ControlState {
  locked: HashSet<CallId>,
  removed: HashSet<(CallId, UserId)>,
}

/// One call.
pub struct Room {
  call_id: CallId,
  state: Mutex<RoomState>,
}

/// Handle a session needs to talk to its participant.
#[derive(Clone)]
pub struct ParticipantHandle {
  pub connection: Connection,
  pub signal_tx: Sender<Outgoing>,
}

struct RoomState {
  participants: HashMap<DeviceId, Participant>,
  forwarder: Forwarder,
  dave: DaveCoordinator,
  next_stream_id: u32,
}

struct Participant {
  user_id: UserId,
  audio_stream_id: StreamId,
  handle: ParticipantHandle,
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

impl Rooms {
  pub fn new(config: SfuConfig) -> Self {
    Self {
      config,
      rooms: Mutex::new(HashMap::new()),
      control: Mutex::new(ControlState::default()),
    }
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
      SfuControlAction::MuteAll => lock(&room.state).broadcast(CallEvent::MutedByHost { by_user_id }),
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
      SfuControlAction::Unknown => {},
    }
    Ok(())
  }

  /// Adds a participant to a call, creating the room on first join.
  pub fn join(
    &self,
    grant: JoinGrant,
    capabilities: MediaCapabilities,
    handle: ParticipantHandle,
  ) -> Result<Arc<Room>, RoomError> {
    let call_id = grant.call_id;
    {
      let control = lock(&self.control);
      if control.removed.contains(&(call_id, grant.user_id)) {
        return Err(RoomError::Removed);
      }
      if control.locked.contains(&call_id) {
        return Err(RoomError::Locked);
      }
    }
    // The rooms lock is held across the join so `leave` can't drop the room in between.
    // Lock order everywhere: rooms, then room state.
    let mut rooms = lock(&self.rooms);
    let room = match rooms.get(&call_id) {
      Some(room) => Arc::clone(room),
      None => {
        let room = Arc::new(Room::new(call_id, &self.config)?);
        rooms.insert(call_id, Arc::clone(&room));
        info!(call_id = %call_id, "room created");
        room
      },
    };
    let result = room.join(grant.user_id, grant.device_id, capabilities, handle);
    if result.is_err() && room.is_empty() {
      rooms.remove(&call_id);
    }
    result?;
    Ok(room)
  }

  /// Removes a participant; the room goes away with its last participant.
  pub fn leave(&self, room: &Room, participant_id: DeviceId) {
    let is_empty = room.leave(participant_id);
    if is_empty {
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
  }
}

impl Room {
  fn new(call_id: CallId, config: &SfuConfig) -> Result<Self, RoomError> {
    let state = RoomState {
      participants: HashMap::new(),
      forwarder: Forwarder::new(config.forwarding),
      dave: DaveCoordinator::new(call_id)?,
      next_stream_id: 1,
    };
    Ok(Self {
      call_id,
      state: Mutex::new(state),
    })
  }

  pub fn call_id(&self) -> CallId {
    self.call_id
  }

  /// Forwards one media datagram from `sender`: audio to everyone if the sender is among the
  /// loudest speakers, video to the subscribers receiving that layer. Only headers are read.
  pub fn forward_datagram(
    &self,
    sender: DeviceId,
    datagram: &[u8],
    now: Instant,
  ) -> Result<usize, RoomError> {
    let deliveries: Vec<(Connection, Vec<u8>)> = {
      let mut state = lock(&self.state);
      let route = state.forwarder.route(sender, datagram, now)?;
      state.apply(route.effects);
      route
        .deliveries
        .into_iter()
        .filter_map(|(receiver, bytes)| {
          let participant = state.participants.get(&receiver)?;
          Some((participant.handle.connection.clone(), bytes))
        })
        .collect()
    };
    for (connection, bytes) in &deliveries {
      // Datagrams are unreliable by design; a full send buffer or closed peer just drops this one.
      if let Err(error) = connection.send_datagram(bytes) {
        trace!(%error, "datagram dropped");
      }
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
    user_id: UserId,
    participant_id: DeviceId,
    capabilities: MediaCapabilities,
    handle: ParticipantHandle,
  ) -> Result<(), RoomError> {
    let mut state = lock(&self.state);
    if state.participants.contains_key(&participant_id) {
      return Err(RoomError::AlreadyJoined(participant_id));
    }
    let audio_stream_id = state.allocate_stream_id();
    let others: Vec<CallParticipant> = state
      .participants
      .iter()
      .map(|(&device_id, participant)| participant.info(device_id))
      .collect();
    let package = state.dave.external_sender_package()?;
    let participant = Participant {
      user_id,
      audio_stream_id,
      handle,
    };
    let info = participant.info(participant_id);
    send(
      &participant.handle,
      CallEvent::Joined {
        call_id: self.call_id,
        participant: info,
        participants: others,
        video_tracks: state.forwarder.video_tracks(),
        screen_audio_tracks: state.forwarder.screen_audio_tracks(),
      },
    );
    send(
      &participant.handle,
      CallEvent::DaveExternalSender {
        package: Base64Bytes::new(package),
      },
    );
    let active_speakers = state.forwarder.active_speakers().to_vec();
    if !active_speakers.is_empty() {
      send(
        &participant.handle,
        CallEvent::ActiveSpeakers {
          device_ids: active_speakers,
        },
      );
    }
    state.broadcast(CallEvent::ParticipantJoined { participant: info });
    state
      .forwarder
      .add_participant(participant_id, audio_stream_id, capabilities, Instant::now());
    state.participants.insert(participant_id, participant);
    info!(call_id = %self.call_id, participant_id = %participant_id, "participant joined");
    Ok(())
  }

  /// Returns whether the room is now empty.
  fn leave(&self, participant_id: DeviceId) -> bool {
    let mut state = lock(&self.state);
    if state.participants.remove(&participant_id).is_none() {
      return state.participants.is_empty();
    }
    state.broadcast(CallEvent::ParticipantLeft {
      device_id: participant_id,
    });
    let removal = state.forwarder.remove_participant(participant_id, Instant::now());
    for stream_id in removal.video_tracks {
      state.broadcast(CallEvent::VideoUnpublished {
        stream_id: MediaStreamId(stream_id.0),
      });
    }
    if let Some(stream_id) = removal.maybe_screen_audio {
      state.broadcast(CallEvent::ScreenAudioUnpublished {
        stream_id: MediaStreamId(stream_id.0),
      });
    }
    state.apply(removal.effects);
    let outbound = state.dave.on_leave(participant_id);
    state.dispatch(outbound);
    info!(
      call_id = %self.call_id,
      participant_id = %participant_id,
      "participant left"
    );
    state.participants.is_empty()
  }

  fn is_empty(&self) -> bool {
    lock(&self.state).participants.is_empty()
  }

  /// Tells the user's devices why, then closes their connections (the sessions then leave as
  /// usual: DAVE re-keys and the server hears about it).
  fn close_user(&self, user_id: UserId, event: CallEvent) {
    let state = lock(&self.state);
    let handles: Vec<ParticipantHandle> = state
      .participants
      .values()
      .filter(|participant| participant.user_id == user_id)
      .map(|participant| participant.handle.clone())
      .collect();
    close_after_event(handles, event);
  }

  fn close_everyone(&self, event: CallEvent) {
    let state = lock(&self.state);
    let handles = state
      .participants
      .values()
      .map(|participant| participant.handle.clone())
      .collect();
    close_after_event(handles, event);
  }
}

impl RoomState {
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

  fn send_to(&self, device_id: DeviceId, event: CallEvent) {
    if let Some(participant) = self.participants.get(&device_id) {
      send(&participant.handle, event);
    }
  }

  fn broadcast(&self, event: CallEvent) {
    for participant in self.participants.values() {
      send(&participant.handle, event.clone());
    }
  }

  fn dispatch(&self, outbound: Vec<Outbound>) {
    for Outbound { recipient, event } in outbound {
      match self.participants.get(&recipient) {
        Some(participant) => send(&participant.handle, event),
        None => debug!(
          participant_id = %recipient,
          "dropping DAVE message for departed participant"
        ),
      }
    }
  }
}

impl Participant {
  fn info(&self, device_id: DeviceId) -> CallParticipant {
    CallParticipant {
      user_id: self.user_id,
      device_id,
      audio_stream_id: MediaStreamId(self.audio_stream_id.0),
    }
  }
}

/// Sends `event` to each handle and closes the connections a moment later, so the event is
/// written first.
fn close_after_event(handles: Vec<ParticipantHandle>, event: CallEvent) {
  for handle in &handles {
    send(handle, event.clone());
  }
  tokio::spawn(async move {
    tokio::time::sleep(CLOSE_GRACE).await;
    for handle in handles {
      handle
        .connection
        .close(CLOSED_BY_HOST.into(), b"closed by the meeting host");
    }
  });
}

fn send(handle: &ParticipantHandle, event: CallEvent) {
  match handle.signal_tx.try_send(Outgoing::Event(event)) {
    Ok(()) => {},
    // A participant that doesn't drain its signaling stream would make the queue grow without bound.
    Err(TrySendError::Full(_)) => {
      warn!("signaling queue full, disconnecting the participant");
      handle
        .connection
        .close(SIGNALING_OVERFLOW.into(), b"signaling queue overflow");
    },
    // The receiver only disappears when the session ends, which also removes the participant.
    Err(TrySendError::Closed(_)) => warn!("signaling channel closed"),
  }
}

/// Recovers from poisoning: room state stays consistent because no invariant spans a panic point.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
