//! Calls that survive lost connections and lossy paths, without a network: the SFU's rooms and
//! real call engines exchange signaling and datagrams through in-memory links, and time is
//! passed in. Deterministic: losses follow fixed patterns.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use teamcraft_media::audio_frame::EncodedAudioFrame;
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_engine::{CallEngine, CallEngineConfig, CallEngineEvent, EngineOutput};
use teamcraft_media::rtp::AudioLevel;
use teamcraft_proto::{
  CallClientMessage, CallEvent, CallId, CallServerMessage, DeviceId, Id, MediaCapabilities, Snowflake, UserId,
};
use teamcraft_sfu::auth::JoinGrant;
use teamcraft_sfu::config::SfuConfig;
use teamcraft_sfu::room::{
  JoinOutcome, ParticipantHandle, ParticipantLink, REPLACED_BY_NEW_CONNECTION, Room, Rooms, SessionId,
};
use teamcraft_sfu::signaling::{EventSequence, Outgoing};
use tokio::sync::mpsc;

const CALL: i64 = 77;
const ALICE: i64 = 1;
const BOB: i64 = 2;

mod lost_connection {
  use super::*;

  #[test]
  fn rejoin_resumes_the_slot_with_new_keys_and_continuous_rtp_numbers() {
    let mut network = Network::with_secured(&[ALICE, BOB]);
    let bob_stream = network.audio_stream_of(BOB, ALICE);
    network.speak(BOB, 3);
    let before = network.heard(ALICE, BOB);
    assert_eq!(before.len(), 3);

    network.lose(BOB);
    assert!(network.saw(ALICE, &CallEngineEvent::ParticipantReconnecting(device(BOB))));
    assert!(network.saw(BOB, &CallEngineEvent::Reconnecting));
    // Media pauses both ways; nobody hears of a leave.
    network.speak(ALICE, 2);
    network.speak(BOB, 2);
    assert!(network.heard(BOB, ALICE).is_empty());
    assert!(network.heard(ALICE, BOB).is_empty());

    let outcome = network.rejoin(BOB);
    assert_eq!(outcome, JoinOutcome::Resumed);
    assert!(network.saw(BOB, &CallEngineEvent::Reconnected));
    assert!(network.saw(ALICE, &CallEngineEvent::ParticipantReconnected(device(BOB))));
    assert!(!network.saw(ALICE, &CallEngineEvent::ParticipantLeft(device(BOB))));
    assert!(network.is_secured(ALICE) && network.is_secured(BOB));
    // Same slot (audio stream), and Bob's RTP numbering simply continues.
    assert_eq!(network.audio_stream_of(BOB, ALICE), bob_stream);
    network.speak(BOB, 1);
    let after = network.heard(ALICE, BOB);
    assert_eq!(after, vec![before[2].wrapping_add(1)]);
    network.speak(ALICE, 1);
    assert_eq!(network.heard(BOB, ALICE).len(), 1);

    // The old connection's cleanup (its grace period running out) leaves the resumed slot alone.
    assert!(!network.expire_lost_sessions(BOB));
    assert!(!network.saw(ALICE, &CallEngineEvent::ParticipantLeft(device(BOB))));
  }

  #[test]
  fn slot_is_released_when_the_grace_period_runs_out() {
    let mut network = Network::with_secured(&[ALICE, BOB]);
    network.lose(BOB);
    assert!(network.expire_lost_sessions(BOB), "the leave is reported once");
    assert!(network.saw(ALICE, &CallEngineEvent::ParticipantLeft(device(BOB))));
    // Back too late: a new participant with a new audio stream.
    assert_eq!(network.rejoin(BOB), JoinOutcome::Joined);
    assert!(network.saw(
      ALICE,
      &CallEngineEvent::ParticipantJoined(network.participant_of(BOB, ALICE))
    ));
    assert!(network.is_secured(ALICE) && network.is_secured(BOB));
    network.speak(ALICE, 1);
    assert_eq!(network.heard(BOB, ALICE).len(), 1);
  }

  #[test]
  fn new_connection_takes_over_one_the_sfu_still_thinks_alive() {
    let mut network = Network::with_secured(&[ALICE, BOB]);
    // Bob noticed the dead path before the SFU's idle timeout did.
    let old_link = network.client(BOB).connection().link.clone();
    network.drop_connection_silently(BOB);
    assert_eq!(network.rejoin(BOB), JoinOutcome::Resumed);
    assert_eq!(old_link.close_code(), Some(REPLACED_BY_NEW_CONNECTION));
    assert!(!network.saw(ALICE, &CallEngineEvent::ParticipantLeft(device(BOB))));
    assert!(network.is_secured(ALICE) && network.is_secured(BOB));
    network.speak(BOB, 1);
    assert_eq!(network.heard(ALICE, BOB).len(), 1);
    // The replaced connection's session ends without touching the slot.
    assert!(!network.expire_lost_sessions(BOB));
  }

  #[test]
  fn everyone_reconnecting_at_once_still_gets_back_together() {
    // Both on the same Wi-Fi that went away.
    let mut network = Network::with_secured(&[ALICE, BOB]);
    network.lose(ALICE);
    network.lose(BOB);
    assert_eq!(network.rejoin(ALICE), JoinOutcome::Resumed);
    assert_eq!(network.rejoin(BOB), JoinOutcome::Resumed);
    assert!(network.is_secured(ALICE) && network.is_secured(BOB));
    network.speak(ALICE, 1);
    network.speak(BOB, 1);
    assert_eq!(network.heard(BOB, ALICE).len(), 1);
    assert_eq!(network.heard(ALICE, BOB).len(), 1);
  }
}

mod lossy_path {
  use super::*;

  #[test]
  fn uplink_loss_reaches_the_sender_as_planned_fec() {
    let mut network = Network::with_secured(&[ALICE, BOB]);
    // Every fourth datagram from Alice is lost on the way to the SFU.
    network.client_mut(ALICE).uplink_loss_every = Some(4);
    for _ in 0..20 {
      network.speak(ALICE, 5);
      network.advance(Duration::from_millis(100));
    }
    let heard = network.heard(BOB, ALICE).len();
    assert_eq!(heard, 75, "a quarter of 100 frames is lost");
    // Measured over the estimator's one-second window.
    let last_loss = network
      .client(ALICE)
      .events
      .iter()
      .rev()
      .find_map(|event| match event {
        CallEngineEvent::UplinkEstimate { loss_percent, .. } => Some(*loss_percent),
        _ => None,
      });
    assert!(
      last_loss.is_some_and(|loss| (20..=30).contains(&loss)),
      "{last_loss:?}"
    );
  }
}

/// Clients and the SFU's rooms, connected in memory.
struct Network {
  rooms: Rooms,
  clients: BTreeMap<i64, Client>,
  now: Instant,
}

struct Client {
  engine: CallEngine,
  maybe_connection: Option<MemoryConnection>,
  /// Sessions of connections lost (or silently dropped) and not released yet.
  lost_sessions: Vec<(Arc<Room>, SessionId)>,
  events: Vec<CallEngineEvent>,
  /// Drop every n-th datagram on the way to the SFU.
  uplink_loss_every: Option<u64>,
  uplink_sent: u64,
}

/// What sending a client's outputs did.
struct Drained {
  has_moved: bool,
  /// The SFU's answer to a join among them.
  maybe_outcome: Option<JoinOutcome>,
}

/// One connection of a client to the SFU.
struct MemoryConnection {
  link: Arc<MemoryLink>,
  signals: mpsc::Receiver<Outgoing>,
  signal_tx: mpsc::Sender<Outgoing>,
  sequence: EventSequence,
  room: Arc<Room>,
  session: SessionId,
}

/// The datagram side of a connection: what the SFU sends lands in the inbox.
#[derive(Default)]
struct MemoryLink {
  inbox: Mutex<VecDeque<Vec<u8>>>,
  is_dead: AtomicBool,
  close_code: Mutex<Option<u32>>,
}

impl ParticipantLink for MemoryLink {
  fn send_datagram(&self, bytes: &[u8]) {
    if !self.is_dead.load(Ordering::Relaxed) {
      self.inbox.lock().unwrap().push_back(bytes.to_vec());
    }
  }

  fn close(&self, code: u32, _reason: &[u8]) {
    *self.close_code.lock().unwrap() = Some(code);
  }
}

impl MemoryLink {
  fn close_code(&self) -> Option<u32> {
    *self.close_code.lock().unwrap()
  }
}

impl Network {
  /// The clients joined and in one DAVE group.
  fn with_secured(ids: &[i64]) -> Self {
    let mut network = Self {
      rooms: Rooms::new(SfuConfig::default()),
      clients: BTreeMap::new(),
      now: Instant::now(),
    };
    for &id in ids {
      let mut engine = CallEngine::new(engine_config(id), network.now);
      engine.join(format!("token-{id}"));
      network.clients.insert(
        id,
        Client {
          engine,
          maybe_connection: None,
          lost_sessions: Vec::new(),
          events: Vec::new(),
          uplink_loss_every: None,
          uplink_sent: 0,
        },
      );
      network.pump();
    }
    for &id in ids {
      assert!(network.is_secured(id), "client {id} is secured");
    }
    network
  }

  fn client(&self, id: i64) -> &Client {
    &self.clients[&id]
  }

  fn client_mut(&mut self, id: i64) -> &mut Client {
    self.clients.get_mut(&id).unwrap()
  }

  /// The connection is gone; the SFU notices and the client starts reconnecting.
  fn lose(&mut self, id: i64) {
    let connection = self.drop_connection_silently(id);
    let slot_kept = self
      .rooms
      .disconnect(&connection.0, device(id), connection.1, self.now);
    assert!(slot_kept);
    self.pump();
  }

  /// The client's connection dies without the SFU noticing (yet).
  fn drop_connection_silently(&mut self, id: i64) -> (Arc<Room>, SessionId) {
    let client = self.client_mut(id);
    let connection = client.maybe_connection.take().unwrap();
    connection.link.is_dead.store(true, Ordering::Relaxed);
    client.engine.connection_lost();
    let session = (Arc::clone(&connection.room), connection.session);
    client.lost_sessions.push(session.clone());
    self.pump();
    session
  }

  /// A new connection with a fresh token; returns how the SFU took the join.
  fn rejoin(&mut self, id: i64) -> JoinOutcome {
    let now = self.now;
    let client = self.client_mut(id);
    client.engine.rejoin(format!("token-{id}-again"), now);
    let outcome = self.drain_client(id).maybe_outcome.unwrap();
    self.pump();
    outcome
  }

  /// Releases the client's lost sessions, as their grace periods run out. Returns whether the
  /// participant left because of it.
  fn expire_lost_sessions(&mut self, id: i64) -> bool {
    let sessions = std::mem::take(&mut self.client_mut(id).lost_sessions);
    let mut has_left = false;
    for (room, session) in sessions {
      has_left |= self.rooms.leave(&room, device(id), session);
    }
    self.pump();
    has_left
  }

  fn advance(&mut self, by: Duration) {
    self.now += by;
    // What the SFU's per-session task and the clients' timers do every 100 ms.
    for (&id, client) in &mut self.clients {
      client.engine.on_timer(self.now);
      if let Some(connection) = &client.maybe_connection {
        connection.room.on_timer(device(id), self.now);
        if let Some(feedback) = connection.room.take_uplink_feedback(device(id)) {
          connection
            .signal_tx
            .try_send(Outgoing::Event(CallEvent::TransportFeedback { feedback }))
            .unwrap();
        }
      }
    }
    self.pump();
  }

  /// The client sends `frames` audio frames.
  fn speak(&mut self, id: i64, frames: usize) {
    let now = self.now;
    for _ in 0..frames {
      self.client_mut(id).engine.send_audio(&frame(), now);
    }
    self.pump();
  }

  /// RTP sequence numbers of the audio `listener` received from `speaker` since the last call.
  fn heard(&mut self, listener: i64, speaker: i64) -> Vec<u16> {
    let client = self.client_mut(listener);
    let mut heard = Vec::new();
    client.events.retain(|event| match event {
      CallEngineEvent::AudioFrame {
        sender,
        sequence_number,
        ..
      } if *sender == device(speaker) => {
        heard.push(sequence_number.0);
        false
      },
      _ => true,
    });
    heard
  }

  fn saw(&self, id: i64, expected: &CallEngineEvent) -> bool {
    self.client(id).events.contains(expected)
  }

  fn is_secured(&self, id: i64) -> bool {
    self.client(id).engine.is_secured()
  }

  fn participant_of(&self, id: i64, seen_by: i64) -> teamcraft_proto::CallParticipant {
    *self
      .client(seen_by)
      .engine
      .participants()
      .find(|participant| participant.device_id == device(id))
      .unwrap()
  }

  fn audio_stream_of(&self, id: i64, seen_by: i64) -> u32 {
    self.participant_of(id, seen_by).audio_stream_id.0
  }

  /// Delivers everything in flight until nothing moves.
  fn pump(&mut self) {
    for _ in 0..1000 {
      let ids: Vec<i64> = self.clients.keys().copied().collect();
      let mut has_moved = false;
      for id in ids {
        has_moved |= self.drain_client(id).has_moved;
        has_moved |= self.deliver_to(id);
      }
      if !has_moved {
        return;
      }
    }
    panic!("the network didn't settle");
  }

  /// Sends a client's engine outputs.
  fn drain_client(&mut self, id: i64) -> Drained {
    let now = self.now;
    let mut maybe_outcome = None;
    let mut has_moved = false;
    while let Some(output) = self.client_mut(id).engine.poll_output() {
      has_moved = true;
      match output {
        EngineOutput::Signal(CallClientMessage::Join { capabilities, .. }) => {
          maybe_outcome = Some(self.connect(id, capabilities));
        },
        EngineOutput::Signal(message) => {
          let Some(connection) = &self.client(id).maybe_connection else {
            continue;
          };
          if let Err(error) = connection.room.on_client_message(device(id), message, now) {
            connection.signal_tx.try_send(Outgoing::Error(error)).unwrap();
          }
        },
        EngineOutput::Datagram(bytes) => {
          let client = self.client_mut(id);
          client.uplink_sent += 1;
          let is_lost = client
            .uplink_loss_every
            .is_some_and(|every| client.uplink_sent.is_multiple_of(every));
          if let Some(connection) = client.maybe_connection.as_ref().filter(|_| !is_lost) {
            connection.room.forward_datagram(device(id), &bytes, now).unwrap();
          }
        },
        EngineOutput::Event(event) => self.client_mut(id).events.push(event),
      }
    }
    Drained {
      has_moved,
      maybe_outcome,
    }
  }

  /// Hands a client what the SFU sent it; returns whether there was anything.
  fn deliver_to(&mut self, id: i64) -> bool {
    let now = self.now;
    let client = self.client_mut(id);
    let Some(connection) = &mut client.maybe_connection else {
      return false;
    };
    let mut has_moved = false;
    while let Ok(outgoing) = connection.signals.try_recv() {
      has_moved = true;
      let message: CallServerMessage = connection.sequence.frame(outgoing);
      client.engine.handle_signal(message, now).unwrap();
    }
    let datagrams: Vec<Vec<u8>> = connection.link.inbox.lock().unwrap().drain(..).collect();
    for datagram in datagrams {
      has_moved = true;
      client.engine.handle_datagram(&datagram, now);
    }
    has_moved
  }

  fn connect(&mut self, id: i64, capabilities: MediaCapabilities) -> JoinOutcome {
    let link = Arc::new(MemoryLink::default());
    let (signal_tx, signals) = mpsc::channel(1024);
    let handle = ParticipantHandle {
      link: link.clone(),
      signal_tx: signal_tx.clone(),
    };
    let grant = JoinGrant {
      call_id: call(),
      user_id: user(id),
      device_id: device(id),
    };
    let (room, session, outcome) = self.rooms.join(grant, capabilities, handle).unwrap();
    self.client_mut(id).maybe_connection = Some(MemoryConnection {
      link,
      signals,
      signal_tx,
      sequence: EventSequence::default(),
      room,
      session,
    });
    outcome
  }
}

impl Client {
  fn connection(&self) -> &MemoryConnection {
    self.maybe_connection.as_ref().unwrap()
  }
}

fn engine_config(id: i64) -> CallEngineConfig {
  CallEngineConfig {
    user_id: user(id),
    device_id: device(id),
    capabilities: MediaCapabilities::default(),
    bwe: BweConfig::default(),
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

fn user(id: i64) -> UserId {
  UserId::from_snowflake(Snowflake::new(1000 + id).unwrap())
}

fn device(id: i64) -> DeviceId {
  DeviceId::from_snowflake(Snowflake::new(id).unwrap())
}
