//! Calls on the WebSocket fallback for networks that block UDP (ARCHITECTURE.md §6.6), with the real
//! native client and the real SFU.
//!
//! The tests marked `#[ignore]` need UDP really gone: `scripts/sfu-no-udp-test.sh` runs them in a
//! network namespace of their own with an nft rule that drops every UDP packet. The others stand in
//! a black-holed QUIC port with a UDP socket that swallows everything, so they run anywhere.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::field_reassign_with_default)]

use std::{
  collections::HashMap,
  net::{Ipv6Addr, SocketAddr, UdpSocket},
  sync::{Arc, Mutex},
  time::{Duration, Instant},
};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use futures_util::{SinkExt, StreamExt};
use rand_core::OsRng;
use teamcraft_crypto::{
  identity::{DeviceKeyPair, IdentityKeyPair},
  ids::{DeviceId as CryptoDeviceId, UserId as CryptoUserId},
};
use teamcraft_media::{
  audio_io::{AudioSink, AudioSource},
  bwe::BweConfig,
  call_auth::{CallKeyAuthenticator, DeviceCallKeyAuthenticator},
  call_engine::{CallEngineConfig, CallEngineEvent},
  call_native::{NativeCall, NativeCallConfig},
  call_resilience::{CallTransport, ResilienceConfig},
  datagram::SimulcastLayer,
  rtp::RtpTimestamp,
  video::EncodedVideoFrame,
};
use teamcraft_proto::{
  AudioCodec, CALL_FALLBACK_PATH, CallClientMessage, CallServerMessage, DeviceId, ErrorCode, FallbackFrame,
  MediaCapabilities, VideoCodec, VideoLayerInfo, VideoTrackKind,
};
use teamcraft_sfu::{
  auth::DevTokenVerifier,
  config::SfuConfig,
  server::{CALL_PATH, Sfu},
};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Bytes, Message};
use wtransport::Identity;

const CALL_ID: u64 = 21;
const ALICE: u64 = 1;
const BOB: u64 = 2;
/// Short enough for a test; the default is checked by `connection_race` in `teamcraft-media`.
const FALLBACK_AFTER: Duration = Duration::from_millis(500);
const CALL_DURATION: Duration = Duration::from_secs(4);
const FRAME_INTERVAL: Duration = Duration::from_millis(33);
/// Starts every video frame in these tests. DAVE must keep it from the SFU and the wire.
const PLAINTEXT_MARKER: &[u8; 16] = b"PLAINTEXT-MARKER";
const VIDEO_FRAME_BYTES: usize = 800;

mod transport_choice {
  use super::*;

  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn clients_stay_on_quic_when_udp_works() {
    let sfu = start_sfu(SfuConfig::default()).await;
    let quic_url = sfu.quic_url();
    let (mut alice, mut bob) = two_clients(&sfu, &quic_url).await;
    assert_eq!(wait_for_transport(&mut alice).await, CallTransport::Quic);
    assert_eq!(wait_for_transport(&mut bob).await, CallTransport::Quic);
    assert_eq!(sfu.control.fallback_connections(), 0, "nobody opened the socket");
    run_call(alice, bob).await.assert_media_flowed();
    assert_eq!(sfu.control.fallback_connections(), 0);
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn a_black_holed_quic_port_moves_to_the_fallback_and_media_flows() {
    let sfu = start_sfu(SfuConfig::default()).await;
    let black_hole = UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
    let quic_url = format!("https://{}{CALL_PATH}", black_hole.local_addr().unwrap());
    let started_at = Instant::now();
    let (mut alice, mut bob) = two_clients(&sfu, &quic_url).await;
    let joined_after = started_at.elapsed();
    assert_eq!(wait_for_transport(&mut alice).await, CallTransport::Fallback);
    assert_eq!(wait_for_transport(&mut bob).await, CallTransport::Fallback);
    assert!(joined_after >= FALLBACK_AFTER, "{joined_after:?}");
    assert!(joined_after < FALLBACK_AFTER * 4, "{joined_after:?}");
    let report = run_call(alice, bob).await;
    report.assert_media_flowed();
    println!("black-holed QUIC port: {report:?}");
  }

  /// What goes over the wire on the fallback is DAVE's work: a viewer reads the frame, the bytes on
  /// the socket toward it don't show it. (The client's direction is masked by the WebSocket anyway.)
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn media_on_the_fallback_stays_encrypted() {
    let sfu = start_sfu(SfuConfig::default()).await;
    let tap = WireTap::start(sfu.fallback_addr).await;
    let black_hole = UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
    let quic_url = format!("https://{}{CALL_PATH}", black_hole.local_addr().unwrap());
    let mut authenticators = authenticators();
    let alice = sfu
      .native_client_via(&quic_url, tap.addr, ALICE, 440.0, authenticators.remove(&ALICE))
      .await;
    let bob = sfu
      .native_client_via(&quic_url, tap.addr, BOB, 550.0, authenticators.remove(&BOB))
      .await;
    let report = run_call(alice, bob).await;
    report.assert_media_flowed();
    assert!(
      report.is_marker_seen,
      "the viewer must read what was sent: {report:?}"
    );
    let wire = tap.recorded();
    assert!(wire.len() > 10_000, "the tap saw only {} bytes", wire.len());
    assert!(
      !wire
        .windows(PLAINTEXT_MARKER.len())
        .any(|window| window == PLAINTEXT_MARKER),
      "plaintext on the wire"
    );
  }
}

mod udp_really_blocked {
  use super::*;

  /// Run by `scripts/sfu-no-udp-test.sh` with every UDP packet dropped.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  #[ignore = "needs UDP blocked: scripts/sfu-no-udp-test.sh"]
  async fn two_clients_call_with_audio_and_video() {
    assert_udp_is_blocked().await;
    let sfu = start_sfu(SfuConfig::default()).await;
    let quic_url = sfu.quic_url();
    let (mut alice, mut bob) = two_clients(&sfu, &quic_url).await;
    assert_eq!(wait_for_transport(&mut alice).await, CallTransport::Fallback);
    assert_eq!(wait_for_transport(&mut bob).await, CallTransport::Fallback);
    let report = run_call(alice, bob).await;
    report.assert_media_flowed();
    println!("UDP blocked: {report:?}");
  }

  /// A UDP echo through the namespace's loopback gets no answer. Proves the test's premise.
  async fn assert_udp_is_blocked() {
    let receiver = tokio::net::UdpSocket::bind((Ipv6Addr::LOCALHOST, 0))
      .await
      .unwrap();
    let sender = tokio::net::UdpSocket::bind((Ipv6Addr::LOCALHOST, 0))
      .await
      .unwrap();
    // The rule may answer with an error or drop silently: either way nothing may arrive.
    let _ = sender.send_to(b"ping", receiver.local_addr().unwrap()).await;
    let mut buffer = [0u8; 8];
    let received = tokio::time::timeout(Duration::from_millis(500), receiver.recv_from(&mut buffer)).await;
    assert!(received.is_err(), "UDP got through: this run proves nothing");
  }
}

mod first_frame {
  use super::*;

  #[tokio::test]
  async fn a_bad_token_is_refused_as_on_quic() {
    let sfu = start_sfu(SfuConfig::default()).await;
    let mut socket = sfu.connect_socket().await;
    send_join(&mut socket, "not a token").await;
    let (error_code, close_code) = read_refusal(&mut socket).await;
    assert_eq!(error_code, Some(ErrorCode::InvalidJoinToken));
    assert_eq!(close_code, Some(0));
  }

  #[tokio::test]
  async fn anything_but_join_first_is_refused() {
    let sfu = start_sfu(SfuConfig::default()).await;
    let mut socket = sfu.connect_socket().await;
    let ping = serde_json::to_vec(&CallClientMessage::Ping { id: 1 }).unwrap();
    send_frame(&mut socket, FallbackFrame::Signal(&ping)).await;
    let (error_code, close_code) = read_refusal(&mut socket).await;
    assert_eq!(error_code, Some(ErrorCode::ProtocolViolation));
    assert_eq!(close_code, Some(0));
  }

  #[tokio::test]
  async fn media_before_the_join_goes_nowhere() {
    let sfu = start_sfu(SfuConfig::default()).await;
    let mut socket = sfu.connect_socket().await;
    send_frame(&mut socket, FallbackFrame::Datagram(&[0; 32])).await;
    let (_, close_code) = read_refusal(&mut socket).await;
    assert_eq!(close_code, Some(0));
    assert_eq!(sfu.control.room_stats().participants, 0);
  }

  #[tokio::test]
  async fn a_silent_connection_is_closed_at_the_join_timeout() {
    let mut config = SfuConfig::default();
    config.join_timeout = Duration::from_millis(100);
    let sfu = start_sfu(config).await;
    let mut socket = sfu.connect_socket().await;
    let (_, close_code) = read_refusal(&mut socket).await;
    assert_eq!(close_code, Some(0));
  }

  #[tokio::test]
  async fn a_connection_that_never_upgrades_is_closed_at_the_handshake_timeout() {
    let mut config = SfuConfig::default();
    config.fallback.handshake_timeout = Duration::from_millis(100);
    config.join_timeout = Duration::from_secs(60);
    let sfu = start_sfu(config).await;
    let mut stream = tokio::net::TcpStream::connect(sfu.fallback_addr).await.unwrap();
    let mut buffer = [0u8; 16];
    // Half a request, and then nothing.
    stream
      .write_all(b"GET /call/fallback HTTP/1.1\r\n")
      .await
      .unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buffer)).await;
    assert!(matches!(closed, Ok(Ok(0) | Err(_))), "{closed:?}");
    wait_until(|| sfu.control.fallback_connections() == 0).await;
  }

  /// Not a trusted proxy's: refused before a byte is read.
  #[tokio::test]
  async fn unjoined_connections_are_limited_per_address() {
    let mut config = SfuConfig::default();
    config.fallback.trusted_proxies = Vec::new();
    config.max_prejoin_sessions_per_ip = 2;
    let sfu = start_sfu(config).await;
    let _first = sfu.connect_socket().await;
    let _second = sfu.connect_socket().await;
    assert!(sfu.try_connect_socket(None).await.is_err());
    drop(_first);
    wait_until(|| sfu.control.fallback_connections() == 1).await;
    assert!(sfu.try_connect_socket(None).await.is_ok());
  }

  #[tokio::test]
  async fn another_path_is_not_a_call_socket() {
    let sfu = start_sfu(SfuConfig::default()).await;
    let url = format!("ws://{}/other", sfu.fallback_addr);
    assert!(tokio_tungstenite::connect_async(url).await.is_err());
  }
}

mod hostile_clients {
  use super::*;

  #[tokio::test]
  async fn invalid_frames_after_the_join_end_the_session_without_keeping_the_slot() {
    for frame in [
      vec![],
      vec![0x03, 0x00],
      b"\x01not json".to_vec(),
      [vec![0x02], vec![0u8; 2000]].concat(),
    ] {
      let sfu = start_sfu(SfuConfig::default()).await;
      let mut socket = sfu.connect_socket().await;
      send_join(&mut socket, &token(ALICE)).await;
      wait_for_joined(&mut socket).await;
      assert_eq!(sfu.control.room_stats().participants, 1);
      socket.send(Message::Binary(Bytes::from(frame))).await.unwrap();
      wait_until(|| sfu.control.room_stats().participants == 0).await;
    }
  }

  #[tokio::test]
  async fn a_message_over_the_limit_ends_the_session() {
    let mut config = SfuConfig::default();
    config.max_signaling_message_bytes = 4096;
    let sfu = start_sfu(config).await;
    let mut socket = sfu.connect_socket().await;
    send_join(&mut socket, &token(ALICE)).await;
    wait_for_joined(&mut socket).await;
    let oversized = FallbackFrame::Signal(&vec![b' '; 8192]).encode();
    socket
      .send(Message::Binary(Bytes::from(oversized)))
      .await
      .unwrap();
    wait_until(|| sfu.control.room_stats().participants == 0).await;
  }

  #[tokio::test]
  async fn a_frame_of_a_newer_client_is_ignored() {
    let sfu = start_sfu(SfuConfig::default()).await;
    let mut socket = sfu.connect_socket().await;
    send_join(&mut socket, &token(ALICE)).await;
    wait_for_joined(&mut socket).await;
    socket
      .send(Message::Binary(Bytes::from_static(b"\x7F?")))
      .await
      .unwrap();
    let ping = serde_json::to_vec(&CallClientMessage::Ping { id: 9 }).unwrap();
    send_frame(&mut socket, FallbackFrame::Signal(&ping)).await;
    wait_for_pong(&mut socket, 9).await;
  }

  #[tokio::test]
  async fn unjoined_connections_have_a_cap_that_leaves_joined_ones_alone() {
    let mut config = SfuConfig::default();
    config.fallback.max_unjoined_connections = 2;
    let sfu = start_sfu(config).await;
    let mut joined = sfu.connect_socket().await;
    send_join(&mut joined, &token(ALICE)).await;
    wait_for_joined(&mut joined).await;
    let _first = sfu.connect_socket().await;
    let _second = sfu.connect_socket().await;
    assert!(sfu.try_connect_socket(None).await.is_err());
    let ping = serde_json::to_vec(&CallClientMessage::Ping { id: 3 }).unwrap();
    send_frame(&mut joined, FallbackFrame::Signal(&ping)).await;
    wait_for_pong(&mut joined, 3).await;
  }

  /// Unjoined ones can't take a joined place either way round: a full call still has its own slots.
  #[tokio::test]
  async fn a_join_over_the_limit_goes_without_a_verdict_on_the_call() {
    let mut config = SfuConfig::default();
    config.fallback.max_connections = 1;
    let sfu = start_sfu(config).await;
    let mut first = sfu.connect_socket().await;
    send_join(&mut first, &token(ALICE)).await;
    wait_for_joined(&mut first).await;
    let mut second = sfu.connect_socket().await;
    send_join(&mut second, &token(BOB)).await;
    // No close frame and no error: the client tries again, as after a lost connection.
    assert_eq!(read_refusal(&mut second).await, (None, None));
    assert_eq!(sfu.control.room_stats().participants, 1);
  }
}

mod slots {
  use super::*;

  /// A lost connection's place in the call waits for the reconnect grace period, and its connection
  /// slot doesn't: else a member looping connect, join, reset fills the limit, and a proxy restart
  /// with many calls refuses their rejoins for as long as the grace lasts.
  #[tokio::test]
  async fn a_lost_connection_gives_up_its_slot_at_once_and_keeps_its_place() {
    let mut config = SfuConfig::default();
    config.fallback.max_connections = 1;
    config.reconnect_grace = Duration::from_secs(60);
    let sfu = start_sfu(config).await;
    for _ in 0..5 {
      let mut socket = sfu.connect_socket().await;
      send_join(&mut socket, &token(ALICE)).await;
      wait_for_joined(&mut socket).await;
      assert_eq!(sfu.control.fallback_connections(), 1);
      drop(socket);
      wait_until(|| sfu.control.fallback_connections() == 0).await;
      assert_eq!(sfu.control.room_stats().participants, 1, "the place stays");
    }
    // Another device gets the one slot while the first's grace period runs.
    let mut other = sfu.connect_socket().await;
    send_join(&mut other, &token(BOB)).await;
    wait_for_joined(&mut other).await;
  }
}

mod forwarded_addresses {
  use super::*;

  const HOLDER: &str = "198.51.100.1";
  const OTHER: &str = "198.51.100.2";

  /// Through a proxy, each client has its own limit.
  #[tokio::test]
  async fn a_trusted_proxy_names_its_clients() {
    let sfu = start_sfu(limited_to_one(vec!["::1/128".parse().unwrap()])).await;
    let _holder = sfu.try_connect_socket(Some(HOLDER)).await.unwrap();
    let mut same = sfu.try_connect_socket(Some(HOLDER)).await.unwrap();
    assert_eq!(read_refusal(&mut same).await, (None, None), "over its limit");
    let mut other = sfu.try_connect_socket(Some(OTHER)).await.unwrap();
    send_join(&mut other, &token(ALICE)).await;
    wait_for_joined(&mut other).await;
  }

  /// A client that isn't a trusted proxy can't claim to be anybody: it is limited by its own address
  /// whatever it writes.
  #[tokio::test]
  async fn from_anybody_else_the_header_is_ignored() {
    let sfu = start_sfu(limited_to_one(Vec::new())).await;
    let _holder = sfu.try_connect_socket(Some(HOLDER)).await.unwrap();
    assert!(sfu.try_connect_socket(Some(OTHER)).await.is_err());
  }

  /// Forging a victim's address on the fallback, or filling the fallback's slots, leaves QUIC alone.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn the_fallbacks_limit_is_not_quics() {
    let sfu = start_sfu(limited_to_one(Vec::new())).await;
    let _holder = sfu.connect_socket().await;
    let quic_url = sfu.quic_url();
    let mut alice = sfu
      .native_client_via(&quic_url, sfu.fallback_addr, ALICE, 440.0, None)
      .await;
    assert_eq!(wait_for_transport(&mut alice).await, CallTransport::Quic);
    alice.leave().await.unwrap();
  }

  fn limited_to_one(trusted_proxies: Vec<ipnet::IpNet>) -> SfuConfig {
    let mut config = SfuConfig::default();
    config.max_prejoin_sessions_per_ip = 1;
    config.fallback.trusted_proxies = trusted_proxies;
    config
  }
}

mod session_end {
  use super::*;

  #[tokio::test]
  async fn a_broken_connection_keeps_the_slot_for_the_grace_period() {
    let mut config = SfuConfig::default();
    config.reconnect_grace = Duration::from_secs(60);
    let sfu = start_sfu(config).await;
    let mut socket = sfu.connect_socket().await;
    send_join(&mut socket, &token(ALICE)).await;
    wait_for_joined(&mut socket).await;
    drop(socket);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
      sfu.control.room_stats().participants,
      1,
      "the client is expected back"
    );
    // And it takes the slot back on a new connection.
    let mut socket = sfu.connect_socket().await;
    send_join(&mut socket, &token(ALICE)).await;
    wait_for_joined(&mut socket).await;
    assert_eq!(sfu.control.room_stats().participants, 1);
  }

  #[tokio::test]
  async fn a_close_frame_is_a_leave() {
    let sfu = start_sfu(SfuConfig::default()).await;
    let mut socket = sfu.connect_socket().await;
    send_join(&mut socket, &token(ALICE)).await;
    wait_for_joined(&mut socket).await;
    send_frame(
      &mut socket,
      FallbackFrame::Close {
        code: 0,
        reason: b"leave",
      },
    )
    .await;
    wait_until(|| sfu.control.room_stats().participants == 0).await;
  }

  #[tokio::test]
  async fn a_client_that_went_silent_is_lost_after_the_idle_timeout() {
    let mut config = SfuConfig::default();
    config.idle_timeout = Duration::from_millis(300);
    config.reconnect_grace = Duration::ZERO;
    // The peer never reads, so it never answers a ping.
    config.fallback.ping_interval = Duration::from_secs(3600);
    let sfu = start_sfu(config).await;
    let mut socket = sfu.connect_socket().await;
    send_join(&mut socket, &token(ALICE)).await;
    wait_for_joined(&mut socket).await;
    wait_until(|| sfu.control.room_stats().participants == 0).await;
  }

  #[tokio::test]
  async fn going_away_tells_the_client_with_the_close_code() {
    let (shutdown, shutting_down) = tokio::sync::oneshot::channel::<()>();
    let sfu = start_sfu_until(SfuConfig::default(), async move {
      let _ = shutting_down.await;
    })
    .await;
    let mut socket = sfu.connect_socket().await;
    send_join(&mut socket, &token(ALICE)).await;
    wait_for_joined(&mut socket).await;
    shutdown.send(()).unwrap();
    let (_, close_code) = read_refusal(&mut socket).await;
    assert_eq!(close_code, Some(teamcraft_proto::SFU_GOING_AWAY_CLOSE_CODE));
  }
}

mod slow_reader {
  use super::*;

  /// A participant that sends pings and never reads the answers: once its socket's buffers are full
  /// the writer stalls, and the write timeout drops the session. That the media queue's bound holds
  /// meanwhile is tested with paused time in `teamcraft-media`'s `fallback_socket`.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn a_client_that_stops_reading_is_dropped() {
    let mut config = SfuConfig::default();
    config.fallback.write_timeout = Duration::from_millis(500);
    config.fallback.socket_send_buffer_bytes = 4096;
    config.reconnect_grace = Duration::ZERO;
    let sfu = start_sfu(config).await;
    let mut sluggard = sfu.connect_socket().await;
    send_join(&mut sluggard, &token(BOB)).await;
    wait_for_joined(&mut sluggard).await;
    let ping = serde_json::to_vec(&CallClientMessage::Ping { id: 1 }).unwrap();
    let frame = Bytes::from(FallbackFrame::Signal(&ping).encode());
    let deadline = Instant::now() + Duration::from_secs(30);
    while sfu.control.room_stats().participants == 1 && Instant::now() < deadline {
      for _ in 0..256 {
        let _ = sluggard.feed(Message::Binary(frame.clone())).await;
      }
      let _ = sluggard.flush().await;
      tokio::task::yield_now().await;
    }
    assert!(
      Instant::now() < deadline,
      "the SFU kept the client that doesn't read"
    );
  }
}

mod behind_a_reverse_proxy {
  use super::*;

  /// With a real reverse proxy in the way: `FALLBACK_PROXY_ADDR` is its address, passing
  /// `CALL_FALLBACK_PATH` to the SFU's listener at `[::1]:18082` (docs/operations.md, its settings).
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  #[ignore = "needs a reverse proxy, FALLBACK_PROXY_ADDR"]
  async fn a_call_goes_through_it() {
    let proxy: SocketAddr = std::env::var("FALLBACK_PROXY_ADDR").unwrap().parse().unwrap();
    let config = SfuConfig::default();
    let sfu = start_sfu_on(config, SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 18082)).await;
    let black_hole = UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
    let quic_url = format!("https://{}{CALL_PATH}", black_hole.local_addr().unwrap());
    let mut authenticators = authenticators();
    let mut alice = sfu
      .native_client_via(&quic_url, proxy, ALICE, 440.0, authenticators.remove(&ALICE))
      .await;
    let mut bob = sfu
      .native_client_via(&quic_url, proxy, BOB, 550.0, authenticators.remove(&BOB))
      .await;
    assert_eq!(wait_for_transport(&mut alice).await, CallTransport::Fallback);
    assert_eq!(wait_for_transport(&mut bob).await, CallTransport::Fallback);
    let report = run_call_for(alice, bob, Duration::from_secs(15)).await;
    report.assert_media_flowed();
    println!("through the proxy: {}", report.summary());
  }
}

mod quality {
  use super::*;

  /// The numbers behind ARCHITECTURE.md §6.6: with `NETEM` set (`scripts/sfu-no-udp-test.sh`) they
  /// are the fallback's on that link. Asserts only that the call survives; the numbers are the point.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  #[ignore = "a measurement: scripts/sfu-no-udp-test.sh -- quality::"]
  async fn measure_a_call_on_the_fallback() {
    let sfu = start_sfu(SfuConfig::default()).await;
    let black_hole = UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
    let quic_url = format!("https://{}{CALL_PATH}", black_hole.local_addr().unwrap());
    let (alice, bob) = two_clients(&sfu, &quic_url).await;
    let report = run_call_for(alice, bob, Duration::from_secs(30)).await;
    println!("{}", report.summary());
    assert!(report.was_secured);
  }

  /// The same over QUIC, to compare with.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  #[ignore = "a measurement: KEEP_UDP=1 scripts/sfu-no-udp-test.sh -- quality::"]
  async fn measure_a_call_on_quic() {
    let sfu = start_sfu(SfuConfig::default()).await;
    let quic_url = sfu.quic_url();
    let (alice, bob) = two_clients(&sfu, &quic_url).await;
    let report = run_call_for(alice, bob, Duration::from_secs(30)).await;
    println!("{}", report.summary());
    assert!(report.was_secured);
  }
}

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct RunningSfu {
  quic_port: u16,
  fallback_addr: SocketAddr,
  certificate_hash: [u8; 32],
  control: teamcraft_sfu::server::SfuControl,
}

/// What reached whom in a call.
#[derive(Debug, Default)]
struct CallReport {
  alice_audio_sent: u64,
  alice_audio_received: u64,
  bob_audio_sent: u64,
  bob_audio_received: u64,
  video_frames_sent: u64,
  video_frames_received: u64,
  /// From handing a frame to the sender's engine to the viewer's engine handing it out, sorted.
  video_latencies: Vec<Duration>,
  is_marker_seen: bool,
  was_secured: bool,
}

impl CallReport {
  fn assert_media_flowed(&self) {
    assert!(self.was_secured, "DAVE keys were not agreed: {self:?}");
    assert!(self.alice_audio_received > self.bob_audio_sent / 2, "{self:?}");
    assert!(self.bob_audio_received > self.alice_audio_sent / 2, "{self:?}");
    assert!(self.video_frames_sent > 10, "{self:?}");
    assert!(
      self.video_frames_received > self.video_frames_sent / 2,
      "{self:?}"
    );
  }

  fn summary(&self) -> String {
    let percentile = |share: usize| {
      let index = (self.video_latencies.len().saturating_sub(1)) * share / 100;
      self.video_latencies.get(index).copied().unwrap_or_default()
    };
    format!(
      "audio {}/{} and {}/{} frames, video {}/{} frames, video latency p50 {:?} p95 {:?} max {:?}",
      self.alice_audio_received,
      self.bob_audio_sent,
      self.bob_audio_received,
      self.alice_audio_sent,
      self.video_frames_received,
      self.video_frames_sent,
      percentile(50),
      percentile(95),
      percentile(100),
    )
  }
}

/// Copies between clients and an upstream, and keeps what flows toward the clients.
struct WireTap {
  addr: SocketAddr,
  recorded: Arc<Mutex<Vec<u8>>>,
}

impl WireTap {
  async fn start(upstream: SocketAddr) -> Self {
    let listener = tokio::net::TcpListener::bind((Ipv6Addr::LOCALHOST, 0))
      .await
      .unwrap();
    let addr = listener.local_addr().unwrap();
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let keeping = Arc::clone(&recorded);
    tokio::spawn(async move {
      loop {
        let (client, _) = listener.accept().await.unwrap();
        let server = tokio::net::TcpStream::connect(upstream).await.unwrap();
        let (mut client_read, mut client_write) = client.into_split();
        let (mut server_read, mut server_write) = server.into_split();
        tokio::spawn(async move { tokio::io::copy(&mut client_read, &mut server_write).await });
        let keeping = Arc::clone(&keeping);
        tokio::spawn(async move {
          let mut buffer = vec![0u8; 16 * 1024];
          while let Ok(length) = server_read.read(&mut buffer).await {
            if length == 0 || client_write.write_all(&buffer[..length]).await.is_err() {
              return;
            }
            keeping.lock().unwrap().extend_from_slice(&buffer[..length]);
          }
        });
      }
    });
    Self { addr, recorded }
  }

  fn recorded(&self) -> Vec<u8> {
    self.recorded.lock().unwrap().clone()
  }
}

impl RunningSfu {
  fn quic_url(&self) -> String {
    format!(
      "https://{}{CALL_PATH}",
      SocketAddr::new(Ipv6Addr::LOCALHOST.into(), self.quic_port)
    )
  }

  async fn connect_socket(&self) -> Socket {
    self.try_connect_socket(None).await.unwrap()
  }

  async fn try_connect_socket(
    &self,
    maybe_forwarded_for: Option<&str>,
  ) -> Result<Socket, tokio_tungstenite::tungstenite::Error> {
    let url = format!("ws://{}{CALL_FALLBACK_PATH}", self.fallback_addr);
    let mut request = url.into_client_request()?;
    if let Some(address) = maybe_forwarded_for {
      request
        .headers_mut()
        .insert("x-forwarded-for", address.parse().unwrap());
    }
    Ok(tokio_tungstenite::connect_async(request).await?.0)
  }

  async fn native_client(
    &self,
    quic_url: &str,
    device: u64,
    frequency_hz: f32,
    authenticator: Option<Arc<dyn CallKeyAuthenticator>>,
  ) -> NativeCall {
    self
      .native_client_via(quic_url, self.fallback_addr, device, frequency_hz, authenticator)
      .await
  }

  async fn native_client_via(
    &self,
    quic_url: &str,
    fallback_addr: SocketAddr,
    device: u64,
    frequency_hz: f32,
    maybe_authenticator: Option<Arc<dyn CallKeyAuthenticator>>,
  ) -> NativeCall {
    let mut config = NativeCallConfig::new(
      quic_url.to_owned(),
      token(device),
      CallEngineConfig {
        user_id: id_of(device),
        device_id: id_of(device),
        capabilities: MediaCapabilities {
          audio_codecs: vec![AudioCodec::Opus],
          video_encoders: vec![VideoCodec::Vp9],
          video_decoders: vec![VideoCodec::Vp9],
          ..MediaCapabilities::default()
        },
        bwe: BweConfig::default(),
      },
    );
    config.maybe_certificate_hash = Some(self.certificate_hash);
    config.maybe_fallback_url = Some(format!("ws://{fallback_addr}{CALL_FALLBACK_PATH}"));
    config.resilience = ResilienceConfig {
      fallback_after: FALLBACK_AFTER,
      ..ResilienceConfig::default()
    };
    config.source = AudioSource::Tone {
      frequency_hz,
      amplitude: 0.3,
    };
    config.sink = AudioSink::Discard;
    config.maybe_trusted_roster = Some(
      [ALICE, BOB]
        .into_iter()
        .filter(|other| *other != device)
        .map(id_of)
        .collect(),
    );
    config.maybe_authenticator = maybe_authenticator;
    NativeCall::join(config).await.unwrap()
  }
}

async fn start_sfu(config: SfuConfig) -> RunningSfu {
  start_sfu_until(config, std::future::pending()).await
}

async fn start_sfu_on(mut config: SfuConfig, fallback_addr: SocketAddr) -> RunningSfu {
  config.fallback.maybe_bind_addr = Some(fallback_addr);
  start_sfu_until(config, std::future::pending()).await
}

async fn start_sfu_until(
  mut config: SfuConfig,
  shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> RunningSfu {
  config.bind_addr = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 0);
  config
    .fallback
    .maybe_bind_addr
    .get_or_insert(SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 0));
  let identity = Identity::self_signed(["localhost", "::1"]).unwrap();
  let sfu = Sfu::bind(config, identity, Arc::new(DevTokenVerifier)).unwrap();
  let running = RunningSfu {
    quic_port: sfu.local_addr().unwrap().port(),
    fallback_addr: sfu.fallback_addr().unwrap().unwrap(),
    certificate_hash: *sfu.certificate_hash().as_ref(),
    control: sfu.control(),
  };
  tokio::spawn(sfu.run_until(shutdown));
  running
}

async fn two_clients(sfu: &RunningSfu, quic_url: &str) -> (NativeCall, NativeCall) {
  let mut authenticators = authenticators();
  tokio::join!(
    sfu.native_client(quic_url, ALICE, 440.0, authenticators.remove(&ALICE)),
    sfu.native_client(quic_url, BOB, 550.0, authenticators.remove(&BOB)),
  )
}

async fn run_call(alice: NativeCall, bob: NativeCall) -> CallReport {
  run_call_for(alice, bob, CALL_DURATION).await
}

/// Alice sends video and both send audio for `duration`; counts what arrived and leaves.
async fn run_call_for(mut alice: NativeCall, mut bob: NativeCall, duration: Duration) -> CallReport {
  let mut report = CallReport::default();
  let layers = vec![VideoLayerInfo {
    layer: 0,
    width: 640,
    height: 360,
    max_framerate: 30,
    max_bitrate_bps: 300_000,
  }];
  let start = Instant::now();
  let mut maybe_stream = None;
  let deadline = tokio::time::Instant::now() + duration;
  let mut frames = tokio::time::interval(FRAME_INTERVAL);
  let mut frame_number = 0u32;
  loop {
    tokio::select! {
      () = tokio::time::sleep_until(deadline) => break,
      _ = frames.tick() => {
        if let Some(stream_id) = maybe_stream {
          frame_number += 1;
          let sent_at_micros = start.elapsed().as_micros() as u64;
          alice.send_video(stream_id, EncodedVideoFrame {
            codec: VideoCodec::Vp9,
            layer: SimulcastLayer::BASE,
            timestamp: RtpTimestamp(frame_number.wrapping_mul(3000)),
            is_keyframe: frame_number == 1 || frame_number.is_multiple_of(10),
            data: video_frame(sent_at_micros),
          });
          report.video_frames_sent += 1;
        }
      },
      Some(event) = alice.next_event() => match event {
        CallEngineEvent::VideoPublished(track) if track.device_id == id_of(ALICE) => maybe_stream = Some(track.stream_id),
        CallEngineEvent::Secured { .. } => {
          // Whichever client gets here first, media is end-to-end encrypted from now on.
          report.was_secured = true;
          alice.publish_video(VideoTrackKind::Camera, VideoCodec::Vp9, layers.clone());
        },
        _ => {},
      },
      Some(event) = bob.next_event() => match event {
        CallEngineEvent::VideoPublished(track) if track.device_id == id_of(ALICE) => bob.subscribe_video(track.stream_id, Some(0)),
        CallEngineEvent::VideoFrame(frame) => {
          report.video_frames_received += 1;
          report.is_marker_seen |= frame.data.starts_with(PLAINTEXT_MARKER);
          let mut sent_at = [0u8; 8];
          sent_at.copy_from_slice(&frame.data[PLAINTEXT_MARKER.len()..PLAINTEXT_MARKER.len() + 8]);
          let sent_at = Duration::from_micros(u64::from_be_bytes(sent_at));
          report.video_latencies.push(start.elapsed().saturating_sub(sent_at));
        },
        _ => {},
      },
    }
  }
  report.video_latencies.sort();
  let alice_stats = alice.leave().await.unwrap();
  let bob_stats = bob.leave().await.unwrap();
  report.alice_audio_sent = alice_stats.frames_sent;
  report.alice_audio_received = alice_stats.frames_received;
  report.bob_audio_sent = bob_stats.frames_sent;
  report.bob_audio_received = bob_stats.frames_received;
  report
}

/// A frame the viewer can check: the marker, when it was sent, then filler.
fn video_frame(sent_at_micros: u64) -> Vec<u8> {
  let mut data = PLAINTEXT_MARKER.to_vec();
  data.extend_from_slice(&sent_at_micros.to_be_bytes());
  data.resize(VIDEO_FRAME_BYTES, 0x55);
  data
}

async fn wait_for_transport(call: &mut NativeCall) -> CallTransport {
  let wait = async {
    loop {
      match call.next_event().await {
        Some(CallEngineEvent::TransportChanged(transport)) => return transport,
        Some(_) => {},
        None => panic!("the call ended"),
      }
    }
  };
  tokio::time::timeout(Duration::from_secs(10), wait).await.unwrap()
}

async fn wait_until(condition: impl Fn() -> bool) {
  let wait = async {
    while !condition() {
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  };
  tokio::time::timeout(Duration::from_secs(10), wait)
    .await
    .expect("condition not met in time");
}

async fn send_join(socket: &mut Socket, token: &str) {
  let join = CallClientMessage::Join {
    token: token.to_owned(),
    capabilities: MediaCapabilities::default(),
  };
  let body = serde_json::to_vec(&join).unwrap();
  send_frame(socket, FallbackFrame::Signal(&body)).await;
}

async fn send_frame(socket: &mut Socket, frame: FallbackFrame<'_>) {
  socket
    .send(Message::Binary(Bytes::from(frame.encode())))
    .await
    .unwrap();
}

/// Reads until the socket ends: what the SFU refused with, and its close code.
async fn read_refusal(socket: &mut Socket) -> (Option<ErrorCode>, Option<u32>) {
  let mut error_code = None;
  let mut close_code = None;
  let reading = async {
    while let Some(Ok(message)) = socket.next().await {
      let Message::Binary(bytes) = message else {
        continue;
      };
      match FallbackFrame::decode(&bytes).unwrap() {
        FallbackFrame::Signal(body) => {
          if let CallServerMessage::Error { error } = serde_json::from_slice(body).unwrap() {
            error_code = Some(error.error_code);
          }
        },
        FallbackFrame::Close { code, .. } => close_code = Some(code),
        _ => {},
      }
    }
  };
  tokio::time::timeout(Duration::from_secs(5), reading)
    .await
    .expect("the socket stayed open");
  (error_code, close_code)
}

async fn wait_for_joined(socket: &mut Socket) {
  wait_for_signal(socket, |message| {
    matches!(
      message,
      CallServerMessage::Event {
        event: teamcraft_proto::CallEvent::Joined { .. },
        ..
      }
    )
  })
  .await;
}

async fn wait_for_pong(socket: &mut Socket, wanted: u64) {
  wait_for_signal(
    socket,
    |message| matches!(message, CallServerMessage::Pong { id } if *id == wanted),
  )
  .await;
}

async fn wait_for_signal(socket: &mut Socket, is_wanted: impl Fn(&CallServerMessage) -> bool) {
  let reading = async {
    loop {
      let message = socket.next().await.expect("socket ended").expect("socket failed");
      if let Message::Binary(bytes) = message
        && let FallbackFrame::Signal(body) = FallbackFrame::decode(&bytes).unwrap()
        && is_wanted(&serde_json::from_slice(body).unwrap())
      {
        return;
      }
    }
  };
  tokio::time::timeout(Duration::from_secs(5), reading)
    .await
    .expect("no such signal in time");
}

fn token(device: u64) -> String {
  format!("dev:{CALL_ID}:{device}:{device}")
}

fn id_of<T: std::str::FromStr>(raw: u64) -> T
where
  T::Err: std::fmt::Debug,
{
  raw.to_string().parse().unwrap()
}

/// One identity per device; each client trusts both devices' certificates, as in a real call.
fn authenticators() -> HashMap<u64, Arc<dyn CallKeyAuthenticator>> {
  let mut key_pairs = Vec::new();
  let mut verified = HashMap::new();
  for device in [ALICE, BOB] {
    let identity = IdentityKeyPair::generate(&mut OsRng);
    let user = CryptoUserId::new(1000 + device);
    let key_pair = DeviceKeyPair::generate(user, CryptoDeviceId::new(device), &mut OsRng);
    let certificate = identity.certify_device(user, &key_pair.public_keys()).unwrap();
    verified.insert(
      id_of::<DeviceId>(device),
      certificate.verify(&identity.public_key()).unwrap(),
    );
    key_pairs.push((device, key_pair));
  }
  key_pairs
    .into_iter()
    .map(|(device, key_pair)| {
      let authenticator: Arc<dyn CallKeyAuthenticator> = Arc::new(DeviceCallKeyAuthenticator::new(
        Arc::new(key_pair),
        verified.clone(),
      ));
      (device, authenticator)
    })
    .collect()
}
