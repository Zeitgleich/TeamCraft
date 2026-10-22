//! Rooms (one per call): participants, datagram forwarding and DAVE coordination.
//!
//! State sits behind a `std::sync::Mutex` that is never held across an `.await`; the per-datagram
//! critical section only parses headers and copies connection handles. A lock-free forwarding table
//! is a later optimization (SPIKE.md).

use std::{
  collections::HashMap,
  sync::{Arc, Mutex, MutexGuard, PoisonError},
  time::Instant,
};

use teamcraft_media::{datagram::StreamId, dave::DaveEpoch};
use teamcraft_proto::{Base64Bytes, CallEvent, CallId, CallParticipant, DeviceId, MediaStreamId, UserId};
use thiserror::Error;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, info, trace, warn};
use wtransport::Connection;

use crate::{
  auth::JoinGrant,
  config::SfuConfig,
  dave_coordinator::{DaveCoordinator, DaveCoordinatorError, Outbound},
  forwarding::{ForwardError, Forwarder},
  signaling::Outgoing,
};

/// All rooms of this SFU.
pub struct Rooms {
  config: SfuConfig,
  rooms: Mutex<HashMap<CallId, Arc<Room>>>,
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
  pub signal_tx: UnboundedSender<Outgoing>,
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
}

impl Rooms {
  pub fn new(config: SfuConfig) -> Self {
    Self {
      config,
      rooms: Mutex::new(HashMap::new()),
    }
  }

  /// Adds a participant to a call, creating the room on first join.
  pub fn join(&self, grant: JoinGrant, handle: ParticipantHandle) -> Result<Arc<Room>, RoomError> {
    let call_id = grant.call_id;
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
    let result = room.join(grant.user_id, grant.device_id, handle);
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
        info!(call_id = %room.call_id, "room closed");
      }
    }
  }
}

impl Room {
  fn new(call_id: CallId, config: &SfuConfig) -> Result<Self, RoomError> {
    let state = RoomState {
      participants: HashMap::new(),
      forwarder: Forwarder::new(config.speakers),
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

  /// Forwards one media datagram from `sender` to the other participants if the sender is among
  /// the loudest speakers. The payload stays encrypted; only headers are read.
  pub fn forward_datagram(
    &self,
    sender: DeviceId,
    datagram: &[u8],
    now: Instant,
  ) -> Result<usize, RoomError> {
    let receivers: Vec<Connection> = {
      let mut state = lock(&self.state);
      let route = state.forwarder.route(sender, datagram, now)?;
      if let Some(speakers) = route.maybe_active_speakers {
        state.broadcast(CallEvent::ActiveSpeakers { device_ids: speakers });
      }
      if !route.should_forward {
        return Ok(0);
      }
      state
        .participants
        .iter()
        .filter(|(id, _)| **id != sender)
        .map(|(_, participant)| participant.handle.connection.clone())
        .collect()
    };
    for connection in &receivers {
      // Datagrams are unreliable by design; a full send buffer or closed peer just drops this one.
      if let Err(error) = connection.send_datagram(datagram) {
        trace!(%error, "datagram dropped");
      }
    }
    Ok(receivers.len())
  }

  pub fn on_key_package(&self, participant_id: DeviceId, key_package: Vec<u8>) -> Result<(), RoomError> {
    let mut state = lock(&self.state);
    let outbound = state.dave.on_key_package(participant_id, key_package)?;
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
    handle: ParticipantHandle,
  ) -> Result<(), RoomError> {
    let mut state = lock(&self.state);
    if state.participants.contains_key(&participant_id) {
      return Err(RoomError::AlreadyJoined(participant_id));
    }
    let audio_stream_id = StreamId(state.next_stream_id);
    state.next_stream_id = state.next_stream_id.wrapping_add(1);
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
    state.forwarder.add(participant_id, audio_stream_id);
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
    if let Some(speakers) = state.forwarder.remove(participant_id, Instant::now()) {
      state.broadcast(CallEvent::ActiveSpeakers { device_ids: speakers });
    }
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
}

impl RoomState {
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

fn send(handle: &ParticipantHandle, event: CallEvent) {
  // The receiver only disappears when the session ends, which also removes the participant.
  if handle.signal_tx.send(Outgoing::Event(event)).is_err() {
    warn!("signaling channel closed");
  }
}

/// Recovers from poisoning: room state stays consistent because no invariant spans a panic point.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
