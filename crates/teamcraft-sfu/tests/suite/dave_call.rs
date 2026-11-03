//! End to end: native clients join a call over WebTransport, establish a DAVE group through the
//! SFU and exchange encrypted audio frames as datagrams. The quietest participant is filtered out by
//! top-3 speaker selection. The SFU listens dual-stack on `[::]`; clients connect over IPv4, IPv6
//! or a mix of both (ARCHITECTURE.md §1.2). Run with `--nocapture` to see latency and overhead.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::{
  collections::HashMap,
  net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
  sync::{Arc, Mutex},
  time::{Duration, Instant},
};

use teamcraft_media::{
  datagram::{DatagramHeader, MediaDatagram, SimulcastLayer, StreamId},
  dave::{DaveEpoch, DaveParticipant, DaveParticipantId, FrameCodec, MediaKind},
  rtp::{AudioLevel, PayloadType, RtpHeader, RtpPacket, RtpTimestamp, SequenceNumber, Ssrc},
};
use teamcraft_proto::{
  Base64Bytes, CallClientMessage, CallEvent, CallId, CallServerMessage, DeviceId, ErrorCode,
  MediaCapabilities,
};
use teamcraft_sfu::{
  auth::DevTokenVerifier,
  config::SfuConfig,
  ids::{dave_call_id, dave_participant_id, device_id_from_dave},
  server::{CALL_PATH, Sfu},
  signaling::{read_message, write_message},
};
use tokio::sync::{Barrier, Notify, mpsc};
use wtransport::{ClientConfig, Connection, Endpoint, Identity, tls::Sha256Digest};

const CALL_ID: u64 = 42;
const FRAMES_PER_SENDER: u32 = 50;
const FRAME_INTERVAL: Duration = Duration::from_millis(20);
/// Typical size of a 20 ms Opus voice frame at ~24 kbit/s.
const PLAINTEXT_FRAME_BYTES: usize = 60;
const OPUS_PAYLOAD_TYPE: u8 = 111;
const MAX_SIGNALING_BYTES: usize = 1024 * 1024;
const TEST_TIMEOUT: Duration = Duration::from_secs(30);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(3);
/// (participant id, audio level in -dBov). Participant 4 is silent and must never be forwarded.
const PARTICIPANTS: [(u64, u8); 4] = [(1, 20), (2, 30), (3, 40), (4, 127)];
const SILENT_PARTICIPANT: u64 = 4;
const V4: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const V6: IpAddr = IpAddr::V6(Ipv6Addr::LOCALHOST);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn call_over_ipv4() {
  run_call([V4; 4]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn call_over_ipv6() {
  run_call([V6; 4]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn call_with_mixed_address_families() {
  run_call([V4, V6, V4, V6]).await;
}

#[tokio::test]
async fn invalid_join_token_is_rejected() {
  let (port, certificate_hash) = start_sfu().await;
  let connection = connect(SocketAddr::new(V6, port), certificate_hash).await;
  let (mut send, mut recv) = connection.open_bi().await.unwrap().await.unwrap();
  write_message(
    &mut send,
    &CallClientMessage::Join {
      token: "forged".into(),
      capabilities: MediaCapabilities::default(),
    },
  )
  .await
  .unwrap();

  let reply: CallServerMessage = read_message(&mut recv, MAX_SIGNALING_BYTES).await.unwrap();

  let CallServerMessage::Error { error } = reply else {
    panic!("expected an error, got {reply:?}");
  };
  assert_eq!(error.error_code, ErrorCode::InvalidJoinToken);
}

/// Browsers prove a quiet session's path with pings (#841): the SFU answers each one, outside the
/// event numbering.
#[tokio::test]
async fn a_ping_is_answered_without_taking_an_event_number() {
  let (port, certificate_hash) = start_sfu().await;
  let connection = connect(SocketAddr::new(V4, port), certificate_hash).await;
  let (mut send, mut recv) = connection.open_bi().await.unwrap().await.unwrap();
  let token = format!("dev:{CALL_ID}:{id}:{id}", id = device(9));
  write_message(
    &mut send,
    &CallClientMessage::Join {
      token,
      capabilities: MediaCapabilities::default(),
    },
  )
  .await
  .unwrap();
  let joined: CallServerMessage = read_message(&mut recv, MAX_SIGNALING_BYTES).await.unwrap();
  assert!(
    matches!(joined, CallServerMessage::Event { seq: 1, .. }),
    "{joined:?}"
  );

  for id in [7, 8] {
    write_message(&mut send, &CallClientMessage::Ping { id })
      .await
      .unwrap();
  }
  let mut last_seq = 1;
  let mut answers = Vec::new();
  tokio::time::timeout(TEST_TIMEOUT, async {
    while answers.len() < 2 {
      match read_message::<CallServerMessage>(&mut recv, MAX_SIGNALING_BYTES)
        .await
        .unwrap()
      {
        // Events of the call (the DAVE setup) go on being numbered without a gap.
        CallServerMessage::Event { seq, .. } => {
          assert_eq!(seq, last_seq + 1);
          last_seq = seq;
        },
        other => answers.push(other),
      }
    }
  })
  .await
  .expect("no answer");
  assert_eq!(
    answers,
    vec![
      CallServerMessage::Pong { id: 7 },
      CallServerMessage::Pong { id: 8 }
    ]
  );
}

/// Runs one call; `client_ips[i]` is the SFU address participant `i` connects to.
async fn run_call(client_ips: [IpAddr; 4]) {
  let (port, certificate_hash) = start_sfu().await;
  let start = Instant::now();
  let ready = Arc::new(Barrier::new(PARTICIPANTS.len()));

  let clients: Vec<_> = PARTICIPANTS
    .iter()
    .zip(client_ips)
    .map(|(&(id, level_dbov), ip)| {
      let ready = Arc::clone(&ready);
      let certificate_hash = certificate_hash.clone();
      tokio::spawn(run_client(
        SocketAddr::new(ip, port),
        certificate_hash,
        device(id),
        level_dbov,
        ready,
        start,
      ))
    })
    .collect();
  let mut reports = Vec::new();
  for client in clients {
    reports.push(
      tokio::time::timeout(TEST_TIMEOUT, client)
        .await
        .expect("client timed out")
        .unwrap(),
    );
  }

  let mut latencies = Vec::new();
  for report in &reports {
    for &(sender, _) in PARTICIPANTS
      .iter()
      .filter(|(id, _)| device(*id) != report.participant_id)
    {
      let received = report
        .decrypted_per_sender
        .get(&device(sender))
        .copied()
        .unwrap_or(0);
      if sender == SILENT_PARTICIPANT {
        assert_eq!(
          received, 0,
          "silent participant must not be forwarded to {:?}",
          report.participant_id
        );
      } else {
        assert!(
          received >= FRAMES_PER_SENDER * 9 / 10,
          "{:?} decrypted only {received} frames from {sender}",
          report.participant_id
        );
      }
    }
    assert_eq!(
      report.decrypt_failures, 0,
      "{:?} failed to decrypt frames",
      report.participant_id
    );
    let mut speakers = report.active_speakers.clone();
    speakers.sort();
    assert_eq!(
      speakers,
      vec![device(1), device(2), device(3)],
      "active speakers seen by {:?}",
      report.participant_id
    );
    latencies.extend_from_slice(&report.latencies);
  }
  println!("client address families: {client_ips:?}");
  print_summary(&reports, &mut latencies);
}

/// What one client observed.
struct ClientReport {
  participant_id: DeviceId,
  decrypted_per_sender: HashMap<DeviceId, u32>,
  decrypt_failures: u32,
  latencies: Vec<Duration>,
  encrypted_frame_bytes: usize,
  datagram_bytes: usize,
  privacy_code: String,
  active_speakers: Vec<DeviceId>,
}

/// Frames a client decrypted, per sender.
#[derive(Default)]
struct ReceiveResults {
  decrypted_per_sender: HashMap<DeviceId, u32>,
  decrypt_failures: u32,
  latencies: Vec<Duration>,
}

impl ReceiveResults {
  fn has_all_frames_from(&self, senders: &[DeviceId]) -> bool {
    senders
      .iter()
      .all(|sender| self.decrypted_per_sender.get(sender).copied().unwrap_or(0) >= FRAMES_PER_SENDER)
  }
}

/// State shared between a client's signaling and media tasks.
struct ClientState {
  dave: DaveParticipant,
  senders_by_stream: HashMap<StreamId, DeviceId>,
  /// Last `active_speakers` event.
  active_speakers: Vec<DeviceId>,
}

/// Starts an SFU on a dual-stack `[::]` socket with an ephemeral port.
async fn start_sfu() -> (u16, Sha256Digest) {
  let config = SfuConfig {
    bind_addr: SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    ..SfuConfig::default()
  };
  let identity = Identity::self_signed(["localhost", "127.0.0.1", "::1"]).unwrap();
  let sfu = Sfu::bind(config, identity, Arc::new(DevTokenVerifier)).unwrap();
  let port = sfu.local_addr().unwrap().port();
  let certificate_hash = sfu.certificate_hash();
  tokio::spawn(sfu.run());
  (port, certificate_hash)
}

async fn run_client(
  addr: SocketAddr,
  certificate_hash: Sha256Digest,
  participant_id: DeviceId,
  level_dbov: u8,
  ready: Arc<Barrier>,
  start: Instant,
) -> ClientReport {
  let connection = connect(addr, certificate_hash).await;
  let (mut send, mut recv) = connection.open_bi().await.unwrap().await.unwrap();
  // One account per participant: the SFU limits the devices of one account in a call.
  let token = format!("dev:{CALL_ID}:{participant_id}:{participant_id}");
  write_message(
    &mut send,
    &CallClientMessage::Join {
      token,
      capabilities: MediaCapabilities::default(),
    },
  )
  .await
  .unwrap();

  let CallServerMessage::Event {
    seq: 1,
    event: CallEvent::Joined {
      participant,
      participants,
      ..
    },
  } = read_message(&mut recv, MAX_SIGNALING_BYTES).await.unwrap()
  else {
    panic!("expected joined as first event");
  };
  assert_eq!(participant.device_id, participant_id);
  let audio_stream_id = participant.audio_stream_id.0;
  let call_id: CallId = CALL_ID.to_string().parse().unwrap();
  let dave = DaveParticipant::new(dave_participant_id(participant_id), dave_call_id(call_id)).unwrap();
  let senders_by_stream = participants
    .iter()
    .map(|info| (StreamId(info.audio_stream_id.0), info.device_id))
    .collect();
  let state = Arc::new(Mutex::new(ClientState {
    dave,
    senders_by_stream,
    active_speakers: Vec::new(),
  }));

  // Signaling: a writer task fed by a channel, and a reader task driving DAVE.
  let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<CallClientMessage>();
  tokio::spawn(async move {
    while let Some(message) = outgoing_rx.recv().await {
      write_message(&mut send, &message).await.unwrap();
    }
  });
  let (group_ready_tx, mut group_ready_rx) = mpsc::unbounded_channel::<()>();
  let signaling_state = Arc::clone(&state);
  tokio::spawn(async move {
    let mut last_seq = 1;
    while let Ok(message) = read_message::<CallServerMessage>(&mut recv, MAX_SIGNALING_BYTES).await {
      let CallServerMessage::Event { seq, event } = message else {
        panic!("unexpected frame {message:?}");
      };
      assert_eq!(seq, last_seq + 1, "event sequence must increase by one");
      last_seq = seq;
      handle_event(&signaling_state, event, &outgoing_tx, &group_ready_tx);
    }
  });

  // Wait until our group contains everyone, then until everyone's does.
  group_ready_rx.recv().await.unwrap();
  ready.wait().await;

  let results = Arc::new(Mutex::new(ReceiveResults::default()));
  let frame_received = Arc::new(Notify::new());
  let receiver = tokio::spawn(receive_frames(
    connection.clone(),
    Arc::clone(&state),
    Arc::clone(&results),
    Arc::clone(&frame_received),
    start,
  ));
  let (encrypted_frame_bytes, datagram_bytes) =
    send_frames(&connection, &state, StreamId(audio_stream_id), level_dbov, start).await;
  // Everyone has sent; take the speaker set and the privacy code before any client leaves: a leave
  // rekeys the group, so a code read later may already belong to the next epoch.
  ready.wait().await;
  let (active_speakers, privacy_code) = {
    let state = state.lock().unwrap();
    let code = state.dave.maybe_privacy_code().unwrap_or_default().to_owned();
    (state.active_speakers.clone(), code)
  };
  ready.wait().await;
  // Wait for every audible sender's last frame; datagrams may be lost, so give up after a while.
  let expected_senders = audible_senders_except(participant_id);
  let _ = tokio::time::timeout(DRAIN_TIMEOUT, async {
    loop {
      let notified = frame_received.notified();
      if results.lock().unwrap().has_all_frames_from(&expected_senders) {
        return;
      }
      notified.await;
    }
  })
  .await;
  receiver.abort();
  let ReceiveResults {
    decrypted_per_sender,
    decrypt_failures,
    latencies,
  } = std::mem::take(&mut *results.lock().unwrap());
  ClientReport {
    participant_id,
    decrypted_per_sender,
    decrypt_failures,
    latencies,
    encrypted_frame_bytes,
    datagram_bytes,
    privacy_code,
    active_speakers,
  }
}

async fn connect(addr: SocketAddr, certificate_hash: Sha256Digest) -> Connection {
  let config = ClientConfig::builder()
    .with_bind_default()
    .with_server_certificate_hashes([certificate_hash])
    .build();
  let endpoint = Endpoint::client(config).unwrap();
  endpoint
    .connect(format!("https://{addr}{CALL_PATH}"))
    .await
    .unwrap()
}

fn handle_event(
  state: &Mutex<ClientState>,
  event: CallEvent,
  outgoing: &mpsc::UnboundedSender<CallClientMessage>,
  group_ready: &mpsc::UnboundedSender<()>,
) {
  let mut state = state.lock().unwrap();
  match event {
    CallEvent::DaveExternalSender { package } => {
      state.dave.set_external_sender(package.as_slice()).unwrap();
      let key_package = state.dave.create_key_package().unwrap();
      outgoing
        .send(CallClientMessage::DaveKeyPackage {
          key_package: Base64Bytes::new(key_package),
          maybe_key_binding: None,
        })
        .unwrap();
    },
    CallEvent::DaveProposals {
      epoch,
      proposals,
      expected_participants,
      ..
    } => {
      let expected: Vec<_> = expected_participants
        .iter()
        .map(|id| dave_participant_id(*id))
        .collect();
      if let Some(commit) = state
        .dave
        .process_proposals(proposals.as_slice(), &expected)
        .unwrap()
      {
        outgoing
          .send(CallClientMessage::DaveCommit {
            epoch,
            commit: Base64Bytes::new(commit.commit),
            maybe_welcome: commit.maybe_welcome.map(Base64Bytes::new),
          })
          .unwrap();
      }
    },
    CallEvent::DaveAnnounceCommit { epoch, commit } => {
      assert_eq!(state.dave.maybe_epoch(), Some(DaveEpoch(epoch)));
      state.dave.process_commit(commit.as_slice()).unwrap();
    },
    CallEvent::DaveWelcome { welcome, .. } => state.dave.process_welcome(welcome.as_slice()).unwrap(),
    CallEvent::ParticipantJoined { participant } => {
      state
        .senders_by_stream
        .insert(StreamId(participant.audio_stream_id.0), participant.device_id);
    },
    CallEvent::ActiveSpeakers { device_ids } => state.active_speakers = device_ids,
    CallEvent::ParticipantLeft { .. } => {},
    other => panic!("unexpected event {other:?}"),
  }
  if state.dave.is_ready() && state.dave.participant_ids().len() == PARTICIPANTS.len() {
    // The receiver only stops listening after the first signal; later sends are harmless.
    let _ = group_ready.send(());
  }
}

/// Sends FRAMES_PER_SENDER encrypted frames; returns (encrypted frame size, datagram size).
async fn send_frames(
  connection: &Connection,
  state: &Mutex<ClientState>,
  stream_id: StreamId,
  level_dbov: u8,
  start: Instant,
) -> (usize, usize) {
  let mut interval = tokio::time::interval(FRAME_INTERVAL);
  let mut sizes = (0, 0);
  let mut sequence_number = SequenceNumber(0);
  for frame_number in 0..FRAMES_PER_SENDER {
    interval.tick().await;
    let mut plaintext = vec![0x5A; PLAINTEXT_FRAME_BYTES];
    let sent_at_micros = u64::try_from(start.elapsed().as_micros()).unwrap();
    plaintext[..8].copy_from_slice(&sent_at_micros.to_be_bytes());
    let encrypted = state
      .lock()
      .unwrap()
      .dave
      .encrypt_frame(MediaKind::Audio, FrameCodec::Opus, &plaintext)
      .unwrap()
      .into_owned();

    let mut header = RtpHeader::new(
      PayloadType::new(OPUS_PAYLOAD_TYPE).unwrap(),
      sequence_number,
      RtpTimestamp(frame_number * 960),
      Ssrc(stream_id.0),
    );
    header.set_audio_level(AudioLevel::new(level_dbov, level_dbov < 127).unwrap());
    let rtp = RtpPacket {
      header,
      payload: &encrypted,
    }
    .to_bytes()
    .unwrap();
    let max_bytes = connection.max_datagram_size().unwrap();
    let datagram = MediaDatagram {
      header: DatagramHeader {
        stream_id,
        layer: SimulcastLayer::BASE,
        maybe_transport_seq: None,
      },
      rtp: &rtp,
    }
    .to_bytes(max_bytes)
    .unwrap();
    connection.send_datagram(&datagram).unwrap();
    sizes = (encrypted.len(), datagram.len());
    sequence_number = sequence_number.next();
  }
  sizes
}

/// Receives and decrypts frames until aborted.
async fn receive_frames(
  connection: Connection,
  state: Arc<Mutex<ClientState>>,
  results: Arc<Mutex<ReceiveResults>>,
  frame_received: Arc<Notify>,
  start: Instant,
) {
  loop {
    let datagram = connection.receive_datagram().await.unwrap();
    let received_at = start.elapsed();
    let payload = datagram.payload();
    let media = MediaDatagram::parse(&payload).unwrap();
    let rtp = RtpPacket::parse(media.rtp).unwrap();
    let (sender, result) = {
      let mut state = state.lock().unwrap();
      let sender = *state
        .senders_by_stream
        .get(&media.header.stream_id)
        .expect("unknown stream");
      (
        sender,
        state
          .dave
          .decrypt_frame(dave_participant_id(sender), MediaKind::Audio, rtp.payload),
      )
    };
    let mut results = results.lock().unwrap();
    match result {
      Ok(plaintext) => {
        let sent_at = Duration::from_micros(u64::from_be_bytes(plaintext[..8].try_into().unwrap()));
        *results.decrypted_per_sender.entry(sender).or_default() += 1;
        results.latencies.push(received_at.saturating_sub(sent_at));
      },
      Err(_) => results.decrypt_failures += 1,
    }
    frame_received.notify_one();
  }
}

fn audible_senders_except(participant_id: DeviceId) -> Vec<DeviceId> {
  PARTICIPANTS
    .iter()
    .map(|&(id, _)| device(id))
    .filter(|&id| id != participant_id && id != device(SILENT_PARTICIPANT))
    .collect()
}

fn print_summary(reports: &[ClientReport], latencies: &mut [Duration]) {
  latencies.sort();
  let percentile = |p: usize| latencies[(latencies.len() - 1) * p / 100];
  let report = &reports[0];
  println!("frames decrypted: {}", latencies.len());
  println!(
    "one-way latency (encrypt + SFU + decrypt, loopback): p50 {:?}, p99 {:?}, max {:?}",
    percentile(50),
    percentile(99),
    percentile(100)
  );
  println!(
    "plaintext frame {PLAINTEXT_FRAME_BYTES} B, DAVE frame {} B (+{} B), datagram {} B (+{} B header overhead incl. DAVE)",
    report.encrypted_frame_bytes,
    report.encrypted_frame_bytes - PLAINTEXT_FRAME_BYTES,
    report.datagram_bytes,
    report.datagram_bytes - PLAINTEXT_FRAME_BYTES
  );
  let codes: Vec<&str> = reports
    .iter()
    .map(|report| report.privacy_code.as_str())
    .collect();
  assert!(
    codes.windows(2).all(|pair| pair[0] == pair[1]),
    "privacy codes differ: {codes:?}"
  );
  println!("privacy code (equal for all): {}", codes[0]);
}

fn device(raw: u64) -> DeviceId {
  device_id_from_dave(DaveParticipantId(raw)).unwrap()
}
