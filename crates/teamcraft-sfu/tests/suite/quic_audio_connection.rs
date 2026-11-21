//! A QUIC session's own audio on a connection of its own (#2141): the client marks that one DSCP
//! Expedited Forwarding. The SFU offers a secret after the join; a second connection that starts
//! with `attach_audio` and the secret carries the session's datagrams; it ends with its session or
//! when another one attaches, and an unknown secret is refused.

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

const CALL_ID: u64 = 2141;
const ALICE: u64 = 1;
const BOB: u64 = 2;
const MAX_SIGNALING_BYTES: usize = 1024 * 1024;
const OPUS_PAYLOAD_TYPE: u8 = 111;
const TIMEOUT: Duration = Duration::from_secs(10);

/// The audio a session sends on its audio connection reaches the others as its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audio_on_the_attached_connection_reaches_the_others() {
  let sfu = start_sfu().await;
  let mut alice = Session::join(&sfu, ALICE).await;
  let bob = Session::join(&sfu, BOB).await;
  let secret = alice.audio_secret().await;
  let (audio, _streams) = attach(&sfu, secret).await;

  let datagram = audio_datagram(&audio, alice.audio_stream_id);
  let received = tokio::time::timeout(TIMEOUT, async {
    loop {
      audio.send_datagram(&datagram).unwrap();
      if let Ok(Ok(received)) =
        tokio::time::timeout(Duration::from_millis(100), bob.connection.receive_datagram()).await
      {
        return received.payload();
      }
    }
  })
  .await
  .expect("Alice's audio reached Bob");
  let media = MediaDatagram::parse(&received).unwrap();
  assert_eq!(media.header.stream_id, StreamId(alice.audio_stream_id));
}

/// A later audio connection replaces an earlier one, and the audio connection goes with its
/// session; neither is to be retried (code 0).
#[tokio::test]
async fn the_audio_connection_ends_with_its_session_or_a_later_one() {
  let sfu = start_sfu().await;
  let mut alice = Session::join(&sfu, ALICE).await;
  let secret = alice.audio_secret().await;
  let (first, _first_streams) = attach(&sfu, secret.clone()).await;
  let (second, _second_streams) = attach(&sfu, secret).await;
  assert_eq!(close_code(&first).await, 0, "replaced");

  alice.connection.close(0u32.into(), b"leave");
  assert_eq!(close_code(&second).await, 0, "gone with its session");
}

#[tokio::test]
async fn an_unknown_secret_is_refused() {
  let sfu = start_sfu().await;
  let (connection, (_send, mut recv)) = attach(&sfu, Base64Bytes::new(vec![7; 32])).await;
  let reply: CallServerMessage = read_message(&mut recv, MAX_SIGNALING_BYTES).await.unwrap();
  let CallServerMessage::Error { error } = reply else {
    panic!("expected a refusal, got {reply:?}");
  };
  assert_eq!(error.error_code, ErrorCode::InvalidJoinToken);
  assert_eq!(close_code(&connection).await, 0);
}

struct RunningSfu {
  address: SocketAddr,
  certificate_hash: [u8; 32],
}

/// A joined raw session.
struct Session {
  connection: Connection,
  _send: SendStream,
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
      _send: send,
      recv,
      audio_stream_id: participant.audio_stream_id.0,
    }
  }

  /// The secret the SFU offers after the join, between the call's events.
  async fn audio_secret(&mut self) -> Base64Bytes {
    tokio::time::timeout(TIMEOUT, async {
      loop {
        if let CallServerMessage::AudioSocket { secret } =
          read_message::<CallServerMessage>(&mut self.recv, MAX_SIGNALING_BYTES)
            .await
            .unwrap()
        {
          return secret;
        }
      }
    })
    .await
    .expect("an audio connection offered in time")
  }
}

async fn start_sfu() -> RunningSfu {
  let config = SfuConfig {
    bind_addr: SocketAddr::from((Ipv6Addr::LOCALHOST, 0)),
    ..SfuConfig::default()
  };
  let identity = Identity::self_signed(["localhost", "::1"]).unwrap();
  let sfu = Sfu::bind(config, identity, Arc::new(DevTokenVerifier)).unwrap();
  let address = sfu.local_addr().unwrap();
  let certificate_hash = *sfu.certificate_hash().as_ref();
  tokio::spawn(sfu.run());
  RunningSfu {
    address,
    certificate_hash,
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
