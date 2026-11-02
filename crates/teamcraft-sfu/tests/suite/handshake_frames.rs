//! The SFU's HTTP/3 handshake holds when a client's frames arrive in pieces (#1139). `wtransport`
//! writes a frame's type, length and payload one by one, so a picked-up QUIC connection may send
//! them in separate packets; its driver stops and restarts the control streams' reads whenever
//! anything else happens on the connection. A read that forgot what it had read took the rest of
//! the frame for new frames and closed the connection (H3_FRAME_UNEXPECTED, 261): now and then a
//! call could not be joined, or ended as it began. Checked with a raw QUIC client, which alone can
//! choose where a frame is cut.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use teamcraft_sfu::auth::DevTokenVerifier;
use teamcraft_sfu::config::SfuConfig;
use teamcraft_sfu::server::{CALL_PATH, Sfu};
use wtransport::proto::bytes::BufferWriter;
use wtransport::proto::frame::{Frame, FrameKind};
use wtransport::proto::headers::Headers;
use wtransport::proto::session::SessionRequest;
use wtransport::proto::settings::Settings;
use wtransport::quinn::{Connection, Endpoint};
use wtransport::{ClientConfig, Identity};

/// Gives the SFU time to read what was just sent. Too short only makes the test miss the bug: it
/// can't fail a working handshake.
const SETTLE: Duration = Duration::from_millis(5);
const DEADLINE: Duration = Duration::from_secs(10);
/// The stream type of an HTTP/3 control stream (RFC 9114, section 6.2.1).
const CONTROL_STREAM: u8 = 0x00;
/// An HTTP datagram of session 0: something else happening on the connection.
const DATAGRAM: [u8; 2] = [0, 0];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_settings_frame_arriving_byte_by_byte_does_not_break_the_handshake() {
  let (address, certificate_hash) = start_sfu();
  let connection = connect(address, certificate_hash).await;

  let mut control = connection.open_uni().await.unwrap();
  control.write_all(&[CONTROL_STREAM]).await.unwrap();
  for byte in settings_frame() {
    control.write_all(&[byte]).await.unwrap();
    tokio::time::sleep(SETTLE).await;
    // The SFU's driver hears of it while it waits for the frame's next byte.
    connection.send_datagram(DATAGRAM.to_vec().into()).unwrap();
    tokio::time::sleep(SETTLE).await;
  }

  let status = tokio::select! {
    status = connect_status(&connection, address) => status,
    reason = connection.closed() => panic!("the SFU closed the connection: {reason}"),
    () = tokio::time::sleep(DEADLINE) => panic!("the SFU never answered the session request"),
  };
  assert_eq!(status, "200");
}

/// Asks for the call's WebTransport session; the status the SFU answers with.
async fn connect_status(connection: &Connection, address: SocketAddr) -> String {
  let request = SessionRequest::new(format!("https://{address}{CALL_PATH}")).unwrap();
  let (mut send, mut recv) = connection.open_bi().await.unwrap();
  send
    .write_all(&encoded(&request.headers().generate_frame()))
    .await
    .unwrap();
  let mut received = Vec::new();
  loop {
    if let Some(frame) = Frame::read(&mut received.as_slice()).unwrap() {
      assert!(matches!(frame.kind(), FrameKind::Headers), "{:?}", frame.kind());
      let headers = Headers::with_frame(&frame).unwrap();
      return headers.get(":status").unwrap().to_owned();
    }
    let chunk = recv.read_chunk(1024, true).await.unwrap().unwrap();
    received.extend_from_slice(&chunk.bytes);
  }
}

/// What a client's driver announces (`driver/streams/settings.rs` in the vendored crate).
fn settings_frame() -> Vec<u8> {
  let settings = Settings::builder()
    .qpack_max_table_capacity(0u32.into())
    .qpack_blocked_streams(0u32.into())
    .enable_connect_protocol()
    .enable_webtransport()
    .enable_h3_datagrams()
    .webtransport_max_sessions(1u32.into())
    .build();
  encoded(&settings.generate_frame())
}

fn encoded(frame: &Frame<'_>) -> Vec<u8> {
  let mut bytes = vec![0; frame.write_size()];
  frame.write_to_buffer(&mut BufferWriter::new(&mut bytes)).unwrap();
  bytes
}

async fn connect(address: SocketAddr, certificate_hash: [u8; 32]) -> Connection {
  let config = ClientConfig::builder()
    .with_bind_default()
    .with_server_certificate_hashes([certificate_hash.into()])
    .build();
  let mut endpoint = Endpoint::client(SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))).unwrap();
  endpoint.set_default_client_config(config.quic_config().clone());
  endpoint.connect(address, "localhost").unwrap().await.unwrap()
}

fn start_sfu() -> (SocketAddr, [u8; 32]) {
  let config = SfuConfig {
    bind_addr: SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    ..SfuConfig::default()
  };
  let identity = Identity::self_signed(["localhost", "127.0.0.1", "::1"]).unwrap();
  let sfu = Sfu::bind(config, identity, Arc::new(DevTokenVerifier)).unwrap();
  let address = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), sfu.local_addr().unwrap().port());
  let certificate_hash = *sfu.certificate_hash().as_ref();
  tokio::spawn(sfu.run());
  (address, certificate_hash)
}
