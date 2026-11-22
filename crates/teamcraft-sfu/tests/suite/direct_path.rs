//! Direct media for 1:1 calls (#127), the SFU's part: `reflect` answers a connection with the
//! address it came from, for a running session's secret only; `direct_path` stops the voices of the
//! two devices going through the SFU to each other only while both say their direct path carries
//! them, and either one's `false` brings them back at once.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use teamcraft_media::datagram::{DatagramHeader, MediaDatagram, SimulcastLayer, StreamId};
use teamcraft_media::rtp::{
  AudioLevel, PayloadType, RtpHeader, RtpPacket, RtpTimestamp, SequenceNumber, Ssrc,
};
use teamcraft_proto::{
  Base64Bytes, CallClientMessage, CallEvent, CallServerMessage, DaveTransitionMode, ErrorCode,
  MediaCapabilities,
};
use teamcraft_sfu::auth::DevTokenVerifier;
use teamcraft_sfu::config::SfuConfig;
use teamcraft_sfu::server::{CALL_PATH, Sfu};
use teamcraft_sfu::signaling::{read_message, write_message};
use wtransport::error::ConnectionError;
use wtransport::{ClientConfig, Connection, Endpoint, Identity, RecvStream, SendStream};

const CALL_ID: u64 = 127;
const ALICE: u64 = 1;
const BOB: u64 = 2;
const MAX_SIGNALING_BYTES: usize = 1024 * 1024;
const OPUS_PAYLOAD_TYPE: u8 = 111;
const TIMEOUT: Duration = Duration::from_secs(10);

/// The SFU answers with the address and port the connection came from, and closes.
#[tokio::test]
async fn reflect_answers_the_address_the_connection_came_from() {
  let sfu = start_sfu().await;
  let mut alice = Session::join(&sfu, ALICE).await;
  let secret = alice.audio_secret().await;
  let (endpoint, connection) = connect_with_endpoint(&sfu).await;
  let (mut send, mut recv) = connection.open_bi().await.unwrap().await.unwrap();
  write_message(&mut send, &CallClientMessage::Reflect { secret })
    .await
    .unwrap();
  let reply: CallServerMessage = read_message(&mut recv, MAX_SIGNALING_BYTES).await.unwrap();
  let CallServerMessage::Reflected { address } = reply else {
    panic!("expected the reflection, got {reply:?}");
  };
  // The socket is bound to the wildcard address; it reached the SFU from loopback.
  assert_eq!(
    address.port(),
    endpoint.local_addr().unwrap().port(),
    "the socket's own port"
  );
  assert!(address.ip().is_loopback());
  connection.close(0u32.into(), b"done");
}

/// The second port (#127, "Hard NATs") is told with the audio secret and reflects the same socket
/// to the same address: without a NAT in between, both reflections agree.
#[tokio::test]
async fn the_second_port_reflects_the_same_socket_alike() {
  let sfu = start_sfu_with_reflect_port().await;
  let mut alice = Session::join(&sfu, ALICE).await;
  let (secret, maybe_port) = alice.audio_secret_and_port().await;
  let reflect_port = maybe_port.expect("the second port is told");
  assert_eq!(Some(reflect_port), sfu.maybe_reflect.map(|addr| addr.port()));
  let (endpoint, first) = connect_with_endpoint(&sfu).await;
  let to_first = reflect_on(&first, secret.clone()).await;
  let second_sfu = RunningSfu {
    address: SocketAddr::new(sfu.address.ip(), reflect_port),
    ..sfu
  };
  let second = endpoint
    .connect(format!("https://{}{CALL_PATH}", second_sfu.address))
    .await
    .unwrap();
  let to_second = reflect_on(&second, secret).await;
  assert_eq!(to_first, to_second);
  assert_eq!(to_first.port(), endpoint.local_addr().unwrap().port());
}

/// The second port answers nothing but `reflect`: a join there is refused.
#[tokio::test]
async fn the_second_port_refuses_a_join() {
  let sfu = start_sfu_with_reflect_port().await;
  let second_sfu = RunningSfu {
    address: sfu.maybe_reflect.unwrap(),
    ..sfu
  };
  let connection = connect(&second_sfu).await;
  let (mut send, mut recv) = connection.open_bi().await.unwrap().await.unwrap();
  let join = CallClientMessage::Join {
    maybe_dave_resume_epoch: None,
    token: format!("dev:{CALL_ID}:{ALICE}:{ALICE}"),
    capabilities: MediaCapabilities::default(),
    dave_transition_mode: DaveTransitionMode::Coordinated,
  };
  write_message(&mut send, &join).await.unwrap();
  let reply: CallServerMessage = read_message(&mut recv, MAX_SIGNALING_BYTES).await.unwrap();
  let CallServerMessage::Error { error } = reply else {
    panic!("expected a refusal, got {reply:?}");
  };
  assert_eq!(error.error_code, ErrorCode::ProtocolViolation);
}

/// Without a second port, sessions are told none.
#[tokio::test]
async fn no_second_port_is_told_by_default() {
  let sfu = start_sfu().await;
  let mut alice = Session::join(&sfu, ALICE).await;
  assert_eq!(alice.audio_secret_and_port().await.1, None);
}

#[tokio::test]
async fn reflect_refuses_an_unknown_secret() {
  let sfu = start_sfu().await;
  let (_endpoint, connection) = connect_with_endpoint(&sfu).await;
  let (mut send, mut recv) = connection.open_bi().await.unwrap().await.unwrap();
  write_message(
    &mut send,
    &CallClientMessage::Reflect {
      secret: Base64Bytes::new(vec![7; 32]),
    },
  )
  .await
  .unwrap();
  let reply: CallServerMessage = read_message(&mut recv, MAX_SIGNALING_BYTES).await.unwrap();
  let CallServerMessage::Error { error } = reply else {
    panic!("expected a refusal, got {reply:?}");
  };
  assert_eq!(error.error_code, ErrorCode::InvalidJoinToken);
  assert_eq!(close_code(&connection).await, 0);
}

/// Alice's voice stops reaching Bob through the SFU only while both report a direct path, and
/// comes back as soon as one of them reports it gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_voices_stop_only_while_both_report_a_direct_path() {
  let sfu = start_sfu().await;
  let mut alice = Session::join(&sfu, ALICE).await;
  let mut bob = Session::join(&sfu, BOB).await;
  let datagram = audio_datagram(&alice.connection, alice.audio_stream_id);
  assert!(
    reaches(&alice, &bob, &datagram).await,
    "forwarded before any report"
  );

  alice.report(true).await;
  assert!(
    reaches(&alice, &bob, &datagram).await,
    "one side alone cuts nothing"
  );
  bob.report(true).await;
  // The report and the datagrams race: wait until the SFU took it.
  let mut stopped = false;
  for _ in 0..50 {
    if !reaches(&alice, &bob, &datagram).await {
      stopped = true;
      break;
    }
  }
  assert!(stopped, "both said so: the SFU forwards no voice between them");

  bob.report(false).await;
  assert!(
    reaches_eventually(&alice, &bob, &datagram).await,
    "one side's false brings it back"
  );
}

impl Session {
  async fn report(&mut self, is_active: bool) {
    write_message(&mut self.send, &CallClientMessage::DirectPath { is_active })
      .await
      .unwrap();
  }
}

/// Whether one of a few datagrams of Alice's reaches Bob within a short wait.
async fn reaches(alice: &Session, bob: &Session, datagram: &[u8]) -> bool {
  for _ in 0..5 {
    alice.connection.send_datagram(datagram).unwrap();
    if let Ok(Ok(_)) =
      tokio::time::timeout(Duration::from_millis(60), bob.connection.receive_datagram()).await
    {
      // Drain what else came, so the next check starts empty.
      while let Ok(Ok(_)) =
        tokio::time::timeout(Duration::from_millis(20), bob.connection.receive_datagram()).await
      {}
      return true;
    }
  }
  false
}

async fn reaches_eventually(alice: &Session, bob: &Session, datagram: &[u8]) -> bool {
  for _ in 0..50 {
    if reaches(alice, bob, datagram).await {
      return true;
    }
  }
  false
}

async fn connect_with_endpoint(
  sfu: &RunningSfu,
) -> (Endpoint<wtransport::endpoint::endpoint_side::Client>, Connection) {
  let config = ClientConfig::builder()
    .with_bind_default()
    .with_server_certificate_hashes([sfu.certificate_hash.into()])
    .build();
  let endpoint = Endpoint::client(config).unwrap();
  let connection = endpoint
    .connect(format!("https://{}{CALL_PATH}", sfu.address))
    .await
    .unwrap();
  (endpoint, connection)
}

#[derive(Clone, Copy)]
struct RunningSfu {
  address: SocketAddr,
  certificate_hash: [u8; 32],
  maybe_reflect: Option<SocketAddr>,
}

async fn reflect_on(connection: &Connection, secret: Base64Bytes) -> SocketAddr {
  let (mut send, mut recv) = connection.open_bi().await.unwrap().await.unwrap();
  write_message(&mut send, &CallClientMessage::Reflect { secret })
    .await
    .unwrap();
  let reply: CallServerMessage = read_message(&mut recv, MAX_SIGNALING_BYTES).await.unwrap();
  connection.close(0u32.into(), b"done");
  let CallServerMessage::Reflected { address } = reply else {
    panic!("expected the reflection, got {reply:?}");
  };
  address
}

/// A joined raw session.
struct Session {
  connection: Connection,
  send: SendStream,
  recv: RecvStream,
  audio_stream_id: u32,
}

impl Session {
  async fn join(sfu: &RunningSfu, device: u64) -> Self {
    let connection = connect(sfu).await;
    let (mut send, mut recv) = connection.open_bi().await.unwrap().await.unwrap();
    let join = CallClientMessage::Join {
      maybe_dave_resume_epoch: None,
      token: format!("dev:{CALL_ID}:{device}:{device}"),
      capabilities: MediaCapabilities::default(),
      dave_transition_mode: DaveTransitionMode::Coordinated,
    };
    write_message(&mut send, &join).await.unwrap();
    let CallServerMessage::Event {
      event: CallEvent::Joined { participant, .. },
      ..
    } = read_message(&mut recv, MAX_SIGNALING_BYTES).await.unwrap()
    else {
      panic!("expected joined first");
    };
    Self {
      connection,
      send,
      recv,
      audio_stream_id: participant.audio_stream_id.0,
    }
  }

  /// The secret the SFU offers after the join, between the call's events.
  async fn audio_secret(&mut self) -> Base64Bytes {
    self.audio_secret_and_port().await.0
  }

  /// The secret and the second reflection port, as the SFU offers them after the join.
  async fn audio_secret_and_port(&mut self) -> (Base64Bytes, Option<u16>) {
    tokio::time::timeout(TIMEOUT, async {
      loop {
        if let CallServerMessage::AudioSocket {
          secret,
          maybe_reflect_port,
        } = read_message::<CallServerMessage>(&mut self.recv, MAX_SIGNALING_BYTES)
          .await
          .unwrap()
        {
          return (secret, maybe_reflect_port);
        }
      }
    })
    .await
    .expect("an audio connection offered in time")
  }
}

async fn start_sfu() -> RunningSfu {
  start_sfu_with(None).await
}

async fn start_sfu_with_reflect_port() -> RunningSfu {
  start_sfu_with(Some(SocketAddr::from((Ipv6Addr::LOCALHOST, 0)))).await
}

async fn start_sfu_with(maybe_reflect_bind_addr: Option<SocketAddr>) -> RunningSfu {
  let config = SfuConfig {
    bind_addr: SocketAddr::from((Ipv6Addr::LOCALHOST, 0)),
    maybe_reflect_bind_addr,
    ..SfuConfig::default()
  };
  let identity = Identity::self_signed(["localhost", "::1"]).unwrap();
  let sfu = Sfu::bind(config, identity, Arc::new(DevTokenVerifier)).unwrap();
  let address = sfu.local_addr().unwrap();
  let maybe_reflect = sfu.reflect_addr().unwrap();
  let certificate_hash = *sfu.certificate_hash().as_ref();
  tokio::spawn(sfu.run());
  RunningSfu {
    address,
    certificate_hash,
    maybe_reflect,
  }
}

async fn connect(sfu: &RunningSfu) -> Connection {
  let config = ClientConfig::builder()
    .with_bind_default()
    .with_server_certificate_hashes([sfu.certificate_hash.into()])
    .build();
  Endpoint::client(config)
    .unwrap()
    .connect(format!("https://{}{CALL_PATH}", sfu.address))
    .await
    .unwrap()
}

/// A second connection that names `secret` first; its streams stay open with it.
#[allow(dead_code)]
async fn attach(sfu: &RunningSfu, secret: Base64Bytes) -> (Connection, (SendStream, RecvStream)) {
  let connection = connect(sfu).await;
  let (mut send, recv) = connection.open_bi().await.unwrap().await.unwrap();
  write_message(&mut send, &CallClientMessage::AttachAudio { secret })
    .await
    .unwrap();
  (connection, (send, recv))
}

async fn close_code(connection: &Connection) -> u64 {
  match tokio::time::timeout(TIMEOUT, connection.closed())
    .await
    .expect("closed in time")
  {
    ConnectionError::ApplicationClosed(close) => close.code().into_inner(),
    other => panic!("expected the SFU's close, got {other:?}"),
  }
}

/// A loud Opus datagram of `stream_id`, as the SFU forwards it (the payload is opaque to it).
fn audio_datagram(connection: &Connection, stream_id: u32) -> Vec<u8> {
  let mut header = RtpHeader::new(
    PayloadType::new(OPUS_PAYLOAD_TYPE).unwrap(),
    SequenceNumber(0),
    RtpTimestamp(0),
    Ssrc(stream_id),
  );
  header.set_audio_level(AudioLevel::new(20, true).unwrap());
  let rtp = RtpPacket {
    header,
    payload: &[0x5A; 60],
  }
  .to_bytes()
  .unwrap();
  MediaDatagram {
    header: DatagramHeader {
      stream_id: StreamId(stream_id),
      layer: SimulcastLayer::BASE,
      maybe_transport_seq: None,
    },
    rtp: &rtp,
  }
  .to_bytes(connection.max_datagram_size().unwrap())
  .unwrap()
}
