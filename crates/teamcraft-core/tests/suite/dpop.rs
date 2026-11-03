//! The core's side of DPoP on what can be scripted (e2ee.md §3.1.2): one more connection after a
//! refused first frame and never a loop, on the gateway and the relay. The real server's side is
//! `teamcraft-server`'s `tests/suite/dpop_clients.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use teamcraft_core::CoreError;
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::{LinkOffer, client::LinkWait};
use teamcraft_proto::ErrorCode;

use crate::common::Peer;

mod gateway {
  use super::*;

  #[tokio::test]
  async fn a_nonce_challenge_gets_one_more_connection() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let connects = server.gateway_connect_count();
    server.challenge_first_frames(1);

    alice.client.connect_gateway().await.unwrap();

    assert_eq!(server.gateway_connect_count(), connects + 2);
    assert_eq!(server.noted_dpop_nonces(), 1);
  }

  #[tokio::test]
  async fn a_challenge_that_never_ends_is_no_loop_and_no_sign_out() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let connects = server.gateway_connect_count();
    server.challenge_first_frames(usize::MAX);

    let Err(error) = alice.client.connect_gateway().await else {
      panic!("the gateway let it in");
    };

    assert_eq!(server.gateway_connect_count(), connects + 2);
    assert!(!error.is_sign_in_required(), "{error}");
    let CoreError::Api(error) = error else {
      panic!("{error}");
    };
    assert!(error.is_transient());
    assert_eq!(error.error_code(), Some(&ErrorCode::UseDpopNonce));
  }

  /// A frame has no `Date`: a request corrects the clock, and then the connection is tried once more.
  #[tokio::test]
  async fn a_refused_date_is_corrected_by_a_request_first() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let connects = server.gateway_connect_count();
    let calls = server.rest_call_count(alice.client.user_id());
    server.refuse_first_frame_proofs(1);

    alice.client.connect_gateway().await.unwrap();

    assert_eq!(server.gateway_connect_count(), connects + 2);
    assert!(server.rest_call_count(alice.client.user_id()) > calls);
  }
}

mod relay {
  use super::*;

  #[tokio::test]
  async fn a_nonce_challenge_gets_one_more_connection() {
    let server = FakeServer::new();
    let alice = Peer::enroll(&server, "Alice").await;
    server.challenge_first_frames(1);

    let joined = LinkWait::join(&alice.api, &LinkOffer::generate()).await;

    assert!(joined.is_ok(), "{:?}", joined.err());
    assert_eq!(server.noted_dpop_nonces(), 1);
  }

  #[tokio::test]
  async fn a_challenge_that_never_ends_is_an_error() {
    let server = FakeServer::new();
    let alice = Peer::enroll(&server, "Alice").await;
    server.challenge_first_frames(usize::MAX);

    let joined = LinkWait::join(&alice.api, &LinkOffer::generate()).await;

    assert!(joined.is_err());
    assert_eq!(server.noted_dpop_nonces(), 2);
    // Still alive for a request of its own.
    alice.api.me().await.unwrap();
  }
}
