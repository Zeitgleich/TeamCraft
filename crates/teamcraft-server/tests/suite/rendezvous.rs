//! The device-linking relay against a real server and Postgres.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::time::Duration;

use common::*;
use futures_util::{SinkExt, StreamExt};
use teamcraft_proto::RendezvousServerMessage;
use teamcraft_server::testing::{TestApp, TestUser};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

const ID: &str = "0123456789abcdef0123456789abcdef";

/// What a device receives from the relay.
#[derive(Debug, PartialEq, Eq)]
enum Received {
  Control(RendezvousServerMessage),
  Frame(Vec<u8>),
  Closed,
}

struct Device {
  socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl Device {
  /// Connects and sends the join frame; returns the first answer.
  async fn join(app: &TestApp, user: &TestUser, id: &str) -> (Self, Received) {
    let url = app.ws_url.replace("/gateway", "/rendezvous");
    let (socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let mut device = Self { socket };
    let join = serde_json::json!({"type": "join", "token": app.token(user), "rendezvous_id": id});
    device
      .socket
      .send(Message::Text(join.to_string().into()))
      .await
      .unwrap();
    let answer = device.recv().await;
    (device, answer)
  }

  async fn send(&mut self, frame: &[u8]) {
    self
      .socket
      .send(Message::Binary(frame.to_vec().into()))
      .await
      .unwrap();
  }

  async fn recv(&mut self) -> Received {
    loop {
      let next = tokio::time::timeout(Duration::from_secs(5), self.socket.next())
        .await
        .expect("relay did not answer");
      match next {
        Some(Ok(Message::Text(text))) => {
          return Received::Control(serde_json::from_str(text.as_str()).unwrap());
        },
        Some(Ok(Message::Binary(frame))) => return Received::Frame(frame.to_vec()),
        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return Received::Closed,
        Some(Ok(_)) => {},
      }
    }
  }
}

fn control(message: RendezvousServerMessage) -> Received {
  Received::Control(message)
}

/// The server's answer to a join, with the default relay lifetime.
fn joined(peer_present: bool) -> Received {
  control(RendezvousServerMessage::Joined {
    peer_present,
    maybe_expires_in_secs: Some(300),
  })
}

#[tokio::test]
async fn two_devices_of_one_user_exchange_binary_frames_in_order() {
  let app = app!();
  let carol = member("carol");
  let (mut phone, answer) = Device::join(&app, &carol, ID).await;
  assert_eq!(answer, joined(false));
  let (mut laptop, answer) = Device::join(&app, &carol, ID).await;
  assert_eq!(answer, joined(true));
  assert_eq!(phone.recv().await, control(RendezvousServerMessage::PeerJoined));

  phone.send(&[1, 2, 3]).await;
  phone.send(&[4]).await;
  laptop.send(&[9, 9]).await;
  assert_eq!(laptop.recv().await, Received::Frame(vec![1, 2, 3]));
  assert_eq!(laptop.recv().await, Received::Frame(vec![4]));
  assert_eq!(phone.recv().await, Received::Frame(vec![9, 9]));
  app.stop().await;
}

#[tokio::test]
async fn frames_sent_before_the_second_device_arrives_are_held() {
  let app = app!();
  let carol = member("carol");
  let (mut phone, _) = Device::join(&app, &carol, ID).await;
  phone.send(&[7]).await;
  phone.send(&[8]).await;
  tokio::time::sleep(Duration::from_millis(100)).await;
  let (mut laptop, _) = Device::join(&app, &carol, ID).await;
  assert_eq!(laptop.recv().await, Received::Frame(vec![7]));
  assert_eq!(laptop.recv().await, Received::Frame(vec![8]));
  app.stop().await;
}

#[tokio::test]
async fn somebody_else_cannot_join_and_the_owner_is_unaffected() {
  let app = app!();
  let (carol, dave) = (member("carol"), member("dave"));
  let (mut phone, _) = Device::join(&app, &carol, ID).await;

  let (mut intruder, answer) = Device::join(&app, &dave, ID).await;
  match answer {
    Received::Control(RendezvousServerMessage::Error { error }) => {
      assert_eq!(error.error_code.as_str(), "forbidden")
    },
    other => panic!("expected an error, got {other:?}"),
  }
  assert_eq!(intruder.recv().await, Received::Closed);

  // A third device of the owner is refused once two are connected.
  let (_laptop, _) = Device::join(&app, &carol, ID).await;
  assert_eq!(phone.recv().await, control(RendezvousServerMessage::PeerJoined));
  let (_third, answer) = Device::join(&app, &carol, ID).await;
  assert!(
    matches!(answer, Received::Control(RendezvousServerMessage::Error { .. })),
    "{answer:?}"
  );
  app.stop().await;
}

#[tokio::test]
async fn invalid_joins_are_refused() {
  let app = app!();
  let carol = member("carol");
  let (_, bad_id) = Device::join(&app, &carol, "short").await;
  assert!(
    matches!(bad_id, Received::Control(RendezvousServerMessage::Error { .. })),
    "{bad_id:?}"
  );

  let url = app.ws_url.replace("/gateway", "/rendezvous");
  let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
  let join = serde_json::json!({"type": "join", "token": "garbage", "rendezvous_id": ID});
  socket.send(Message::Text(join.to_string().into())).await.unwrap();
  let mut device = Device { socket };
  match device.recv().await {
    Received::Control(RendezvousServerMessage::Error { error }) => {
      assert_eq!(error.error_code.as_str(), "invalid_token")
    },
    other => panic!("expected an error, got {other:?}"),
  }
  app.stop().await;
}

#[tokio::test]
async fn leaving_closes_the_other_side() {
  let app = app!();
  let carol = member("carol");
  let (mut phone, _) = Device::join(&app, &carol, ID).await;
  let (mut laptop, _) = Device::join(&app, &carol, ID).await;
  phone.recv().await;

  laptop.socket.close(None).await.unwrap();
  assert_eq!(phone.recv().await, control(RendezvousServerMessage::PeerLeft));
  assert_eq!(phone.recv().await, Received::Closed);
  // The ID is free again.
  let (_, answer) = Device::join(&app, &carol, ID).await;
  assert_eq!(answer, joined(false));
  app.stop().await;
}

#[tokio::test]
async fn oversized_frames_and_exhausted_quotas_end_the_relay() {
  let Some(app) = TestApp::start_with(|s| {
    s.rendezvous.max_frame_bytes = 4;
    s.rendezvous.max_total_bytes = 6;
  })
  .await
  else {
    return;
  };
  let carol = member("carol");
  let (mut phone, _) = Device::join(&app, &carol, ID).await;
  let (mut laptop, _) = Device::join(&app, &carol, ID).await;
  phone.recv().await;

  phone.send(&[0; 5]).await;
  match phone.recv().await {
    Received::Control(RendezvousServerMessage::Error { error }) => {
      assert_eq!(error.error_code.as_str(), "payload_too_large")
    },
    other => panic!("expected an error, got {other:?}"),
  }
  assert_eq!(laptop.recv().await, control(RendezvousServerMessage::PeerLeft));

  let other_id = "ffffffffffffffffffffffffffffffff";
  let (mut a, _) = Device::join(&app, &carol, other_id).await;
  let (mut b, _) = Device::join(&app, &carol, other_id).await;
  a.recv().await;
  a.send(&[1; 4]).await;
  assert_eq!(b.recv().await, Received::Frame(vec![1; 4]));
  a.send(&[1; 4]).await;
  assert!(matches!(
    a.recv().await,
    Received::Control(RendezvousServerMessage::Error { .. })
  ));
  app.stop().await;
}

#[tokio::test]
async fn relays_expire() {
  let Some(app) = TestApp::start_with(|s| s.rendezvous.ttl_secs = 1).await else {
    return;
  };
  let carol = member("carol");
  let (mut phone, _) = Device::join(&app, &carol, ID).await;
  assert_eq!(phone.recv().await, control(RendezvousServerMessage::Expired));
  assert_eq!(phone.recv().await, Received::Closed);
  app.stop().await;
}

#[tokio::test]
async fn text_frames_after_joining_are_never_relayed() {
  let app = app!();
  let carol = member("carol");
  let (mut phone, _) = Device::join(&app, &carol, ID).await;
  let (mut laptop, _) = Device::join(&app, &carol, ID).await;
  phone.recv().await;
  phone
    .socket
    .send(Message::Text(r#"{"type":"error"}"#.into()))
    .await
    .unwrap();
  phone.send(&[5]).await;
  assert_eq!(laptop.recv().await, Received::Frame(vec![5]));
  app.stop().await;
}

#[tokio::test]
async fn users_have_a_limit_of_open_relays() {
  let Some(app) = TestApp::start_with(|s| s.rendezvous.max_per_user = 1).await else {
    return;
  };
  let carol = member("carol");
  let (_first, _) = Device::join(&app, &carol, ID).await;
  let (_second, answer) = Device::join(&app, &carol, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").await;
  match answer {
    Received::Control(RendezvousServerMessage::Error { error }) => {
      assert_eq!(error.error_code.as_str(), "rate_limited")
    },
    other => panic!("expected an error, got {other:?}"),
  }
  app.stop().await;
}
