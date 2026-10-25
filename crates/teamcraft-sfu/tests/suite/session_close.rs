//! The SFU ends a WebTransport session with a CLOSE_WEBTRANSPORT_SESSION capsule, so a browser's
//! `WebTransport.closed` reports the close code (#459). Checked with the vendored `wtransport`
//! alone: a client reads the code from the capsule although the QUIC connection stays open.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{Ipv6Addr, SocketAddr};
use std::time::Duration;

use teamcraft_proto::SFU_GOING_AWAY_CLOSE_CODE;
use wtransport::endpoint::endpoint_side::Server;
use wtransport::error::ConnectionError;
use wtransport::{ClientConfig, Endpoint, Identity, ServerConfig};

const TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn the_peer_reads_the_code_from_the_session_close() {
  let identity = Identity::self_signed(["localhost", "::1"]).unwrap();
  let hash = *identity.certificate_chain().as_slice()[0].hash().as_ref();
  let server = server_endpoint(identity);
  let address = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), server.local_addr().unwrap().port());

  let accepting = tokio::spawn(async move {
    let connection = server.accept().await.await.unwrap().accept().await.unwrap();
    // Only the session ends; the QUIC connection is left alone, so the code can only come from
    // the capsule.
    connection
      .close_session(SFU_GOING_AWAY_CLOSE_CODE, "going away")
      .await;
    (server, connection)
  });
  let config = ClientConfig::builder()
    .with_bind_default()
    .with_server_certificate_hashes([hash.into()])
    .build();
  let client = Endpoint::client(config)
    .unwrap()
    .connect(format!("https://{address}/call"))
    .await
    .unwrap();

  let error = tokio::time::timeout(TIMEOUT, client.closed()).await.unwrap();
  let ConnectionError::ApplicationClosed(close) = error else {
    panic!("expected the session close, got {error:?}");
  };
  assert_eq!(close.code().into_inner(), u64::from(SFU_GOING_AWAY_CLOSE_CODE));
  assert_eq!(close.reason(), b"going away");
  drop(accepting.await.unwrap());
}

fn server_endpoint(identity: Identity) -> Endpoint<Server> {
  let config = ServerConfig::builder()
    .with_bind_address(SocketAddr::from((Ipv6Addr::LOCALHOST, 0)))
    .with_identity(identity)
    .build();
  Endpoint::server(config).unwrap()
}
