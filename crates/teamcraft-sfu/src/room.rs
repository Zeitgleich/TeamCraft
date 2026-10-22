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

use teamcraft_media::{
  datagram::{DatagramError, DatagramHeader, StreamId},
  dave::DaveEpoch,
  rtp::{AudioLevel, RtpError, RtpHeader},
};
use thiserror::Error;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, info, trace, warn};
use wtransport::Connection;

use crate::{
  config::SfuConfig,
  dave_coordinator::{DaveCoordinator, DaveCoordinatorError, Outbound},
  ids::{CallId, DeviceId},
  signaling::{ParticipantInfo, ServerMessage},
  speakers::SpeakerSelector,
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
  pub signal_tx: UnboundedSender<ServerMessage>,
}

struct RoomState {
  participants: HashMap<DeviceId, Participant>,
  speakers: SpeakerSelector,
  dave: DaveCoordinator,
  next_stream_id: u32,
}

struct Participant {
  audio_stream_id: StreamId,
  handle: ParticipantHandle,
}

#[derive(Debug, Error)]
pub enum RoomError {
  #[error("participant {0:?} is already in the call")]
  AlreadyJoined(DeviceId),
  #[error("participant {0:?} is not in the call")]
  NotJoined(DeviceId),
  #[error("stream {actual:?} does not belong to the sender (expected {expected:?})")]
  ForeignStream { expected: StreamId, actual: StreamId },
  #[error("invalid datagram: {0}")]
  Datagram(#[from] DatagramError),
  #[error("invalid RTP header: {0}")]
  Rtp(#[from] RtpError),
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
  pub fn join(
    &self,
    call_id: CallId,
    participant_id: DeviceId,
    handle: ParticipantHandle,
  ) -> Result<Arc<Room>, RoomError> {
    // The rooms lock is held across the join so `leave` can't drop the room in between.
    // Lock order everywhere: rooms, then room state.
    let mut rooms = lock(&self.rooms);
    let room = match rooms.get(&call_id) {
      Some(room) => Arc::clone(room),
      None => {
        let room = Arc::new(Room::new(call_id, &self.config)?);
        rooms.insert(call_id, Arc::clone(&room));
        info!(call_id = call_id.0, "room created");
        room
      },
    };
    let result = room.join(participant_id, handle);
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
        info!(call_id = room.call_id.0, "room closed");
      }
    }
  }
}

impl Room {
  fn new(call_id: CallId, config: &SfuConfig) -> Result<Self, RoomError> {
    let state = RoomState {
      participants: HashMap::new(),
      speakers: SpeakerSelector::new(config.max_forwarded_speakers, config.speaker_timeout),
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
      let Some(participant) = state.participants.get(&sender) else {
        return Err(RoomError::NotJoined(sender));
      };
      let header = DatagramHeader::parse(datagram)?;
      if header.stream_id != participant.audio_stream_id {
        return Err(RoomError::ForeignStream {
          expected: participant.audio_stream_id,
          actual: header.stream_id,
        });
      }
      let (rtp_header, _) = RtpHeader::parse(&datagram[teamcraft_media::datagram::DATAGRAM_HEADER_BYTES..])?;
      let level = rtp_header.audio_level().unwrap_or(AudioLevel::SILENCE);
      state.speakers.record(sender, level, now);
      if !state.speakers.is_selected(sender, now) {
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

  fn join(&self, participant_id: DeviceId, handle: ParticipantHandle) -> Result<(), RoomError> {
    let mut state = lock(&self.state);
    if state.participants.contains_key(&participant_id) {
      return Err(RoomError::AlreadyJoined(participant_id));
    }
    let audio_stream_id = StreamId(state.next_stream_id);
    state.next_stream_id = state.next_stream_id.wrapping_add(1);
    let others: Vec<ParticipantInfo> = state
      .participants
      .iter()
      .map(|(&id, participant)| ParticipantInfo {
        participant_id: id,
        audio_stream_id: participant.audio_stream_id.0,
      })
      .collect();
    let package = state.dave.external_sender_package()?;
    send(
      &handle,
      ServerMessage::Joined {
        call_id: self.call_id,
        participant_id,
        audio_stream_id: audio_stream_id.0,
        participants: others,
      },
    );
    send(&handle, ServerMessage::DaveExternalSender { package });
    let info = ParticipantInfo {
      participant_id,
      audio_stream_id: audio_stream_id.0,
    };
    for participant in state.participants.values() {
      send(&participant.handle, ServerMessage::ParticipantJoined(info));
    }
    state.participants.insert(
      participant_id,
      Participant {
        audio_stream_id,
        handle,
      },
    );
    info!(
      call_id = self.call_id.0,
      participant_id = %participant_id,
      "participant joined"
    );
    Ok(())
  }

  /// Returns whether the room is now empty.
  fn leave(&self, participant_id: DeviceId) -> bool {
    let mut state = lock(&self.state);
    if state.participants.remove(&participant_id).is_none() {
      return state.participants.is_empty();
    }
    state.speakers.remove(participant_id);
    for participant in state.participants.values() {
      send(
        &participant.handle,
        ServerMessage::ParticipantLeft { participant_id },
      );
    }
    let outbound = state.dave.on_leave(participant_id);
    state.dispatch(outbound);
    info!(
      call_id = self.call_id.0,
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
  fn dispatch(&self, outbound: Vec<Outbound>) {
    for Outbound { recipient, message } in outbound {
      match self.participants.get(&recipient) {
        Some(participant) => send(&participant.handle, message),
        None => debug!(
          participant_id = %recipient,
          "dropping DAVE message for departed participant"
        ),
      }
    }
  }
}

fn send(handle: &ParticipantHandle, message: ServerMessage) {
  // The receiver only disappears when the session ends, which also removes the participant.
  if handle.signal_tx.send(message).is_err() {
    warn!("signaling channel closed");
  }
}

/// Recovers from poisoning: room state stays consistent because no invariant spans a panic point.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
