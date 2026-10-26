//! The WebSocket gateway against a real server and Postgres.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::time::Duration;

use common::*;
use futures_util::{SinkExt, StreamExt};
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_proto::{
  DeviceActivity, DeviceId, GatewayClientMessage, GatewayEvent, GatewayServerMessage, PresenceState,
};
use teamcraft_server::testing::{TestApp, TestUser};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const WAIT: Duration = Duration::from_secs(5);

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

struct Gateway {
  socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
  session_id: String,
  last_seq: u64,
}

impl Gateway {
  async fn open(app: &TestApp) -> (WebSocketStream<MaybeTlsStream<TcpStream>>, GatewayServerMessage) {
    let (mut socket, _) = tokio_tungstenite::connect_async(&app.ws_url).await.unwrap();
    let hello = receive(&mut socket).await.unwrap();
    (socket, hello)
  }

  /// Connects and identifies; panics if the handshake fails.
  async fn connect(app: &TestApp, user: &TestUser, device: &str) -> Self {
    let (mut socket, hello) = Self::open(app).await;
    assert!(
      matches!(
        hello,
        GatewayServerMessage::Hello {
          heartbeat_interval_ms: 30_000
        }
      ),
      "{hello:?}"
    );
    transmit(
      &mut socket,
      &GatewayClientMessage::Identify {
        token: app.token(user),
        device_id: device_id(device),
        activity: DeviceActivity::Active,
      },
    )
    .await;
    match receive(&mut socket).await {
      Some(GatewayServerMessage::Ready { session_id, .. }) => Self {
        socket,
        session_id,
        last_seq: 0,
      },
      other => panic!("expected ready, got {other:?}"),
    }
  }

  async fn resume(
    app: &TestApp,
    user: &TestUser,
    device: &str,
    session_id: &str,
    last_seq: u64,
  ) -> (Self, GatewayServerMessage) {
    let (mut socket, _) = Self::open(app).await;
    transmit(
      &mut socket,
      &GatewayClientMessage::Resume {
        token: app.token(user),
        session_id: session_id.to_owned(),
        last_seq,
        device_id: device_id(device),
      },
    )
    .await;
    let answer = receive(&mut socket).await.unwrap();
    (
      Self {
        socket,
        session_id: session_id.to_owned(),
        last_seq,
      },
      answer,
    )
  }

  async fn send(&mut self, message: &GatewayClientMessage) {
    transmit(&mut self.socket, message).await;
  }

  /// The next event for which `matches` returns a value; other events are skipped.
  async fn event_where<T>(&mut self, matches: impl Fn(&GatewayEvent) -> Option<T>) -> T {
    loop {
      match receive(&mut self.socket).await {
        Some(GatewayServerMessage::Event { seq, event }) => {
          // Typing comes unnumbered (`0`) and leaves the resume position alone.
          if seq != 0 {
            assert_eq!(seq, self.last_seq + 1, "sequence numbers must be consecutive");
            self.last_seq = seq;
          }
          if let Some(found) = matches(&event) {
            return found;
          }
        },
        Some(_) => {},
        None => panic!("connection closed while waiting for an event"),
      }
    }
  }

  /// The next presence update as (user, state).
  async fn next_presence(&mut self) -> (String, PresenceState) {
    self
      .event_where(|event| match event {
        GatewayEvent::PresenceUpdate { user_id, state, .. } => Some((user_id.to_string(), state.clone())),
        _ => None,
      })
      .await
  }

  async fn presence_of(&mut self, user: &str) -> PresenceState {
    let user = user.to_owned();
    self
      .event_where(|event| match event {
        GatewayEvent::PresenceUpdate { user_id, state, .. } if user_id.to_string() == user => {
          Some(state.clone())
        },
        _ => None,
      })
      .await
  }

  /// `true` if nothing arrives within a short while (events are not skipped silently).
  async fn is_silent(&mut self) -> bool {
    tokio::time::timeout(Duration::from_millis(300), receive(&mut self.socket))
      .await
      .is_err()
  }
}

fn device_id(raw: &str) -> DeviceId {
  serde_json::from_value(json!(raw)).unwrap()
}

async fn transmit(socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>, message: &GatewayClientMessage) {
  socket
    .send(Message::Text(serde_json::to_string(message).unwrap().into()))
    .await
    .unwrap();
}

/// The next server message; `None` once the connection is closed.
async fn receive(socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>) -> Option<GatewayServerMessage> {
  loop {
    let next = tokio::time::timeout(WAIT, socket.next())
      .await
      .expect("gateway did not answer in time");
    match next? {
      Ok(Message::Text(text)) => return Some(serde_json::from_str(text.as_str()).unwrap()),
      Ok(Message::Close(_)) | Err(_) => return None,
      Ok(_) => {},
    }
  }
}

/// alice and carol in channel "general" of a space; both with a registered device.
struct Scene {
  channel: String,
  alice: TestUser,
  carol: TestUser,
  alice_device: String,
  carol_device: String,
  alice_id: String,
  carol_id: String,
}

async fn scene(app: &TestApp) -> Scene {
  let (alice, carol) = (owner(), member("carol"));
  let (alice_device, carol_device) = (
    register_device(app, &alice).await,
    register_device(app, &carol).await,
  );
  let (alice_id, carol_id) = (user_id(app, &alice).await, user_id(app, &carol).await);
  let space = create_space(app, &alice, "Team", "open").await;
  app
    .post(&carol, &format!("/spaces/{space}/join"), json!({}))
    .await;
  let channel = create_channel(app, &alice, &space, "general", "public").await;
  app
    .post(&carol, &format!("/conversations/{channel}/join"), json!({}))
    .await;
  Scene {
    channel,
    alice,
    carol,
    alice_device,
    carol_device,
    alice_id,
    carol_id,
  }
}

mod handshake {
  use super::*;

  #[tokio::test]
  async fn bad_tokens_and_foreign_devices_are_rejected() {
    let app = app!();
    let s = scene(&app).await;

    let (mut socket, _) = Gateway::open(&app).await;
    transmit(
      &mut socket,
      &GatewayClientMessage::Identify {
        token: "not-a-token".into(),
        device_id: device_id(&s.alice_device),
        activity: DeviceActivity::Active,
      },
    )
    .await;
    let answer = receive(&mut socket).await.unwrap();
    assert!(
      matches!(answer, GatewayServerMessage::Error { ref error } if error.error_code.as_str() == "invalid_token"),
      "{answer:?}"
    );
    assert!(
      receive(&mut socket).await.is_none(),
      "the connection is closed after a failed handshake"
    );

    // carol's token with alice's device.
    let (mut socket, _) = Gateway::open(&app).await;
    transmit(
      &mut socket,
      &GatewayClientMessage::Identify {
        token: app.token(&s.carol),
        device_id: device_id(&s.alice_device),
        activity: DeviceActivity::Active,
      },
    )
    .await;
    let answer = receive(&mut socket).await.unwrap();
    assert!(
      matches!(answer, GatewayServerMessage::Error { ref error } if error.error_code.as_str() == "device_not_found"),
      "{answer:?}"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn heartbeats_are_answered_and_unknown_frames_ignored() {
    let app = app!();
    let s = scene(&app).await;
    let mut gateway = Gateway::connect(&app, &s.alice, &s.alice_device).await;

    gateway
      .socket
      .send(Message::Text(r#"{"type":"teleport","x":1}"#.into()))
      .await
      .unwrap();
    gateway
      .socket
      .send(Message::Text("not json".into()))
      .await
      .unwrap();
    gateway.send(&GatewayClientMessage::Heartbeat).await;
    loop {
      match receive(&mut gateway.socket).await {
        Some(GatewayServerMessage::HeartbeatAck) => break,
        Some(_) => {},
        None => panic!("closed"),
      }
    }
    app.stop().await;
  }
}

mod events {
  use super::*;

  #[tokio::test]
  async fn messages_reach_every_member_connection_but_nobody_else() {
    let app = app!();
    let s = scene(&app).await;
    let outsider = member("dave");
    let outsider_device = register_device(&app, &outsider).await;
    let mut alice = Gateway::connect(&app, &s.alice, &s.alice_device).await;
    let mut dave = Gateway::connect(&app, &outsider, &outsider_device).await;

    let sent = send(&app, &s.carol, &s.channel, &s.carol_device, 1).await;
    let message = alice
      .event_where(|event| match event {
        GatewayEvent::MessageCreate { message } => Some(message.clone()),
        _ => None,
      })
      .await;
    assert_eq!(message.id.to_string(), sent);
    assert_eq!(message.ciphertext.as_slice(), b"ciphertext");
    assert_eq!(message.sender_user_id.to_string(), s.carol_id);

    // dave is no member: he sees no message, whatever presence updates may arrive.
    let leaked = tokio::time::timeout(Duration::from_millis(300), async {
      loop {
        if let Some(GatewayServerMessage::Event {
          event: GatewayEvent::MessageCreate { .. },
          ..
        }) = receive(&mut dave.socket).await
        {
          return;
        }
      }
    })
    .await;
    assert!(leaked.is_err(), "a non-member received a message");
    app.stop().await;
  }

  #[tokio::test]
  async fn typing_goes_to_the_other_members_only_and_only_from_members() {
    let app = app!();
    let s = scene(&app).await;
    let mut alice = Gateway::connect(&app, &s.alice, &s.alice_device).await;
    let mut carol = Gateway::connect(&app, &s.carol, &s.carol_device).await;
    let conversation = serde_json::from_value(json!(s.channel)).unwrap();

    carol
      .send(&GatewayClientMessage::Typing {
        conversation_id: conversation,
      })
      .await;
    let typist = alice
      .event_where(|event| match event {
        GatewayEvent::TypingStart { user_id, .. } => Some(user_id.to_string()),
        _ => None,
      })
      .await;
    assert_eq!(typist, s.carol_id);

    // Somebody who can't even see the channel gets an error frame instead.
    let outsider = member("dave");
    let outsider_device = register_device(&app, &outsider).await;
    let mut dave = Gateway::connect(&app, &outsider, &outsider_device).await;
    dave
      .send(&GatewayClientMessage::Typing {
        conversation_id: conversation,
      })
      .await;
    loop {
      match receive(&mut dave.socket).await {
        Some(GatewayServerMessage::Error { error }) => {
          assert_eq!(error.error_code.as_str(), "channel_not_found");
          break;
        },
        Some(_) => {},
        None => panic!("closed"),
      }
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn membership_changes_are_announced_and_cut_off_the_event_stream() {
    let app = app!();
    let s = scene(&app).await;
    let mut alice = Gateway::connect(&app, &s.alice, &s.alice_device).await;
    let mut carol = Gateway::connect(&app, &s.carol, &s.carol_device).await;

    let removed = app
      .delete(
        &s.alice,
        &format!("/conversations/{}/members/{}", s.channel, s.carol_id),
      )
      .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT, "{}", removed.body);
    let (left, left_user) = carol
      .event_where(|event| match event {
        GatewayEvent::MembershipRemove {
          conversation_id,
          user_id,
        } => Some((conversation_id.to_string(), user_id.to_string())),
        _ => None,
      })
      .await;
    assert_eq!((left, left_user), (s.channel.clone(), s.carol_id.clone()));
    alice
      .event_where(|event| matches!(event, GatewayEvent::MembershipRemove { .. }).then_some(()))
      .await;

    // After removal carol no longer receives the channel's messages.
    send(&app, &s.alice, &s.channel, &s.alice_device, 1).await;
    alice
      .event_where(|event| matches!(event, GatewayEvent::MessageCreate { .. }).then_some(()))
      .await;
    let mut leaked = false;
    while let Ok(Some(message)) =
      tokio::time::timeout(Duration::from_millis(300), receive(&mut carol.socket)).await
    {
      leaked |= matches!(
        message,
        GatewayServerMessage::Event {
          event: GatewayEvent::MessageCreate { .. },
          ..
        }
      );
    }
    assert!(!leaked);

    // Joining again is announced to the members.
    app
      .post(&s.carol, &format!("/conversations/{}/join", s.channel), json!({}))
      .await;
    let joined = alice
      .event_where(|event| match event {
        GatewayEvent::MembershipAdd { user, .. } => Some(user.display_name.clone()),
        _ => None,
      })
      .await;
    assert_eq!(joined, "carol");
    app.stop().await;
  }
}

mod resuming {
  use super::*;

  #[tokio::test]
  async fn a_reconnecting_client_receives_exactly_what_it_missed() {
    let app = app!();
    let s = scene(&app).await;
    let mut alice = Gateway::connect(&app, &s.alice, &s.alice_device).await;
    let first = send(&app, &s.carol, &s.channel, &s.carol_device, 1).await;
    alice
      .event_where(|event| matches!(event, GatewayEvent::MessageCreate { .. }).then_some(()))
      .await;
    let (session_id, seen) = (alice.session_id.clone(), alice.last_seq);
    drop(alice);
    tokio::time::sleep(Duration::from_millis(200)).await;

    let second = send(&app, &s.carol, &s.channel, &s.carol_device, 2).await;
    let third = send(&app, &s.carol, &s.channel, &s.carol_device, 3).await;
    let (mut alice, answer) = Gateway::resume(&app, &s.alice, &s.alice_device, &session_id, seen).await;
    assert!(
      matches!(answer, GatewayServerMessage::Resumed { .. }),
      "{answer:?}"
    );

    let mut replayed = Vec::new();
    while replayed.len() < 2 {
      let id = alice
        .event_where(|event| match event {
          GatewayEvent::MessageCreate { message } => Some(message.id.to_string()),
          _ => None,
        })
        .await;
      replayed.push(id);
    }
    assert_eq!(replayed, vec![second, third]);
    assert!(!replayed.contains(&first));
    app.stop().await;
  }

  #[tokio::test]
  async fn a_shutdown_closes_sockets_with_1012_and_clients_resume() {
    let mut app = app!();
    let s = scene(&app).await;
    let mut alice = Gateway::connect(&app, &s.alice, &s.alice_device).await;
    send(&app, &s.carol, &s.channel, &s.carol_device, 1).await;
    alice
      .event_where(|event| matches!(event, GatewayEvent::MessageCreate { .. }).then_some(()))
      .await;
    let (session_id, seen) = (alice.session_id.clone(), alice.last_seq);
    assert_eq!(readiness(&app).await, StatusCode::OK);

    // SIGTERM: not ready any more, and every socket gets "service restart".
    app.state.drain.begin();
    assert_eq!(readiness(&app).await, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(close_code(&mut alice.socket).await, Some(1012));
    assert!(app.restart_after_drain(WAIT).await, "every socket closed in time");

    // The client comes back and picks up where it was, nothing lost and nothing twice.
    let missed = send(&app, &s.carol, &s.channel, &s.carol_device, 2).await;
    let (mut alice, answer) = Gateway::resume(&app, &s.alice, &s.alice_device, &session_id, seen).await;
    assert!(
      matches!(answer, GatewayServerMessage::Resumed { .. }),
      "{answer:?}"
    );
    // Presence updates of the reconnect may come along; the message is the one that matters.
    let replayed = alice
      .event_where(|event| match event {
        GatewayEvent::MessageCreate { message } => Some(message.id.to_string()),
        _ => None,
      })
      .await;
    assert_eq!(replayed, missed);
    assert_eq!(readiness(&app).await, StatusCode::OK);
    app.stop().await;
  }

  #[tokio::test]
  async fn after_a_real_restart_a_resume_gets_session_invalid_and_the_client_syncs_afresh() {
    let mut app = app!();
    let s = scene(&app).await;
    let mut alice = Gateway::connect(&app, &s.alice, &s.alice_device).await;
    send(&app, &s.carol, &s.channel, &s.carol_device, 1).await;
    alice
      .event_where(|event| matches!(event, GatewayEvent::MessageCreate { .. }).then_some(()))
      .await;
    let (session_id, seen) = (alice.session_id.clone(), alice.last_seq);

    // SIGTERM, and a new process: its hub knows no session.
    app.state.drain.begin();
    assert_eq!(close_code(&mut alice.socket).await, Some(1012));
    assert!(
      app.restart_after_drain_with_fresh_memory(WAIT).await,
      "every socket closed in time"
    );
    let missed = send(&app, &s.carol, &s.channel, &s.carol_device, 2).await;
    let (_, answer) = Gateway::resume(&app, &s.alice, &s.alice_device, &session_id, seen).await;
    assert!(
      matches!(answer, GatewayServerMessage::SessionInvalid),
      "{answer:?}"
    );

    // The client identifies again and refetches over REST: the missed message is there.
    let mut alice = Gateway::connect(&app, &s.alice, &s.alice_device).await;
    let history = app
      .get(&s.alice, &format!("/conversations/{}/messages", s.channel))
      .await;
    assert_eq!(history.status, StatusCode::OK);
    let ids: Vec<&str> = history.body["messages"]
      .as_array()
      .unwrap()
      .iter()
      .filter_map(|message| message["id"].as_str())
      .collect();
    assert!(ids.contains(&missed.as_str()), "{ids:?}");
    // And the fresh session gets new events.
    let next = send(&app, &s.carol, &s.channel, &s.carol_device, 3).await;
    let received = alice
      .event_where(|event| match event {
        GatewayEvent::MessageCreate { message } => Some(message.id.to_string()),
        _ => None,
      })
      .await;
    assert_eq!(received, next);
    app.stop().await;
  }

  async fn readiness(app: &TestApp) -> StatusCode {
    app
      .http
      .get(format!("{}/readyz", app.base_url))
      .send()
      .await
      .unwrap()
      .status()
  }

  /// The close code the server ends the connection with (`None`: closed without a frame).
  async fn close_code(socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>) -> Option<u16> {
    loop {
      let next = tokio::time::timeout(WAIT, socket.next())
        .await
        .expect("the gateway did not close in time");
      match next? {
        Ok(Message::Close(frame)) => return frame.map(|frame| u16::from(frame.code)),
        Ok(_) => {},
        Err(_) => return None,
      }
    }
  }

  #[tokio::test]
  async fn unknown_sessions_and_foreign_users_get_session_invalid() {
    let app = app!();
    let s = scene(&app).await;
    let alice = Gateway::connect(&app, &s.alice, &s.alice_device).await;

    let (_, answer) = Gateway::resume(&app, &s.alice, &s.alice_device, "does-not-exist", 0).await;
    assert!(
      matches!(answer, GatewayServerMessage::SessionInvalid),
      "{answer:?}"
    );
    // carol can't take over alice's session, even with her own device.
    let (_, answer) = Gateway::resume(&app, &s.carol, &s.carol_device, &alice.session_id, 0).await;
    assert!(
      matches!(answer, GatewayServerMessage::SessionInvalid),
      "{answer:?}"
    );
    app.stop().await;
  }
}

mod presence {
  use super::*;

  #[tokio::test]
  async fn presence_follows_connections_activity_and_manual_choices() {
    let app = app!();
    let s = scene(&app).await;
    let mut alice = Gateway::connect(&app, &s.alice, &s.alice_device).await;
    // Alice learns about her own state and, once carol connects, about carol.
    assert_eq!(alice.presence_of(&s.alice_id).await, PresenceState::Online);
    let carol = Gateway::connect(&app, &s.carol, &s.carol_device).await;
    assert_eq!(alice.presence_of(&s.carol_id).await, PresenceState::Online);

    let mut carol = carol;
    carol
      .send(&GatewayClientMessage::SetActivity {
        activity: DeviceActivity::Idle,
      })
      .await;
    assert_eq!(alice.presence_of(&s.carol_id).await, PresenceState::Away);

    let dnd = app.put(&s.carol, "/me/presence", json!({"manual": "dnd"})).await;
    assert_eq!(dnd.status, StatusCode::OK, "{}", dnd.body);
    assert_eq!(alice.presence_of(&s.carol_id).await, PresenceState::DoNotDisturb);

    let invisible = app
      .put(&s.carol, "/me/presence", json!({"manual": "invisible"}))
      .await;
    assert_eq!(invisible.status, StatusCode::OK);
    assert_eq!(alice.presence_of(&s.carol_id).await, PresenceState::Offline);

    app.put(&s.carol, "/me/presence", json!({"manual": "auto"})).await;
    assert_eq!(alice.presence_of(&s.carol_id).await, PresenceState::Away);

    // Disconnecting makes carol offline (the test server has no grace period).
    carol.socket.close(None).await.unwrap();
    assert_eq!(alice.presence_of(&s.carol_id).await, PresenceState::Offline);

    let listed = app
      .get(
        &s.alice,
        &format!("/presence?user_ids={},{}", s.alice_id, s.carol_id),
      )
      .await;
    let states: Vec<_> = listed.body["presences"]
      .as_array()
      .unwrap()
      .iter()
      .map(|p| p["state"].clone())
      .collect();
    assert_eq!(states, vec![json!("online"), json!("offline")]);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_manual_away_sticks_until_another_status_is_chosen() {
    let app = app!();
    let s = scene(&app).await;
    let mut alice = Gateway::connect(&app, &s.alice, &s.alice_device).await;
    let mut carol = Gateway::connect(&app, &s.carol, &s.carol_device).await;
    assert_eq!(alice.presence_of(&s.carol_id).await, PresenceState::Online);

    let away = app.put(&s.carol, "/me/presence", json!({"manual": "away"})).await;
    assert_eq!(away.status, StatusCode::OK, "{}", away.body);
    assert_eq!(away.body["manual"], "away");
    assert_eq!(alice.presence_of(&s.carol_id).await, PresenceState::Away);

    // Activity doesn't end a manual away.
    carol
      .send(&GatewayClientMessage::SetActivity {
        activity: DeviceActivity::Active,
      })
      .await;
    assert!(alice.is_silent().await);

    app.put(&s.carol, "/me/presence", json!({"manual": "auto"})).await;
    assert_eq!(alice.presence_of(&s.carol_id).await, PresenceState::Online);
    app.stop().await;
  }

  #[tokio::test]
  async fn becoming_active_again_is_never_throttled() {
    let app = app!();
    let s = scene(&app).await;
    let mut alice = Gateway::connect(&app, &s.alice, &s.alice_device).await;
    let mut carol = Gateway::connect(&app, &s.carol, &s.carol_device).await;
    assert_eq!(alice.presence_of(&s.carol_id).await, PresenceState::Online);
    // Two changes right after one another, well within the throttle interval.
    carol
      .send(&GatewayClientMessage::SetActivity {
        activity: DeviceActivity::Idle,
      })
      .await;
    assert_eq!(alice.presence_of(&s.carol_id).await, PresenceState::Away);
    carol
      .send(&GatewayClientMessage::SetActivity {
        activity: DeviceActivity::Active,
      })
      .await;
    assert_eq!(alice.presence_of(&s.carol_id).await, PresenceState::Online);
    app.stop().await;
  }

  #[tokio::test]
  async fn sending_a_message_makes_the_sender_online_unless_they_chose_otherwise() {
    let app = app!();
    let s = scene(&app).await;
    let mut alice = Gateway::connect(&app, &s.alice, &s.alice_device).await;
    let mut carol = Gateway::connect(&app, &s.carol, &s.carol_device).await;
    assert_eq!(alice.presence_of(&s.carol_id).await, PresenceState::Online);
    // The device said it is idle (a stale lock reading, say), yet carol writes from it.
    carol
      .send(&GatewayClientMessage::SetActivity {
        activity: DeviceActivity::Idle,
      })
      .await;
    assert_eq!(alice.presence_of(&s.carol_id).await, PresenceState::Away);
    send(&app, &s.carol, &s.channel, &s.carol_device, 1).await;
    assert_eq!(alice.presence_of(&s.carol_id).await, PresenceState::Online);

    // A manual away is never ended by activity, also not by sending.
    app.put(&s.carol, "/me/presence", json!({"manual": "away"})).await;
    assert_eq!(alice.presence_of(&s.carol_id).await, PresenceState::Away);
    carol
      .send(&GatewayClientMessage::SetActivity {
        activity: DeviceActivity::Idle,
      })
      .await;
    send(&app, &s.carol, &s.channel, &s.carol_device, 2).await;
    let listed = app
      .get(&s.alice, &format!("/presence?user_ids={}", s.carol_id))
      .await;
    assert_eq!(listed.body["presences"][0]["state"], json!("away"));
    app.stop().await;
  }

  #[tokio::test]
  async fn a_user_with_several_devices_is_online_while_any_is_active() {
    let app = app!();
    let s = scene(&app).await;
    let phone = app
      .post(
        &s.carol,
        "/devices",
        json!({"name": "phone", "public_key": "cHVibGlj", "cross_signature": "c2ln"}),
      )
      .await
      .id();
    let mut alice = Gateway::connect(&app, &s.alice, &s.alice_device).await;
    let mut desktop = Gateway::connect(&app, &s.carol, &s.carol_device).await;
    // Both updates arrive, in either order.
    let mut seen = [alice.next_presence().await, alice.next_presence().await];
    seen.sort_by(|a, b| a.0.cmp(&b.0));
    let mut expected = [
      (s.alice_id.clone(), PresenceState::Online),
      (s.carol_id.clone(), PresenceState::Online),
    ];
    expected.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(seen, expected);
    let mut mobile = Gateway::connect(&app, &s.carol, &phone).await;

    desktop
      .send(&GatewayClientMessage::SetActivity {
        activity: DeviceActivity::Idle,
      })
      .await;
    // The phone is still active, so nothing changes: no further update arrives.
    assert!(alice.is_silent().await);
    mobile
      .send(&GatewayClientMessage::SetActivity {
        activity: DeviceActivity::Idle,
      })
      .await;
    assert_eq!(alice.presence_of(&s.carol_id).await, PresenceState::Away);
    app.stop().await;
  }

  #[tokio::test]
  async fn do_not_disturb_validates_its_end_time() {
    let app = app!();
    let s = scene(&app).await;
    let past = app
      .put(&s.carol, "/me/presence", json!({"manual": "dnd", "until": 1000}))
      .await;
    assert_eq!(past.status, StatusCode::BAD_REQUEST);
    let misplaced = app
      .put(
        &s.carol,
        "/me/presence",
        json!({"manual": "invisible", "until": 4_102_444_800_000_i64}),
      )
      .await;
    assert_eq!(misplaced.status, StatusCode::BAD_REQUEST);
    let timed = app
      .put(
        &s.carol,
        "/me/presence",
        json!({"manual": "dnd", "until": 4_102_444_800_000_i64}),
      )
      .await;
    assert_eq!(timed.status, StatusCode::OK);
    assert_eq!(timed.body["until"], 4_102_444_800_000_i64);
    app.stop().await;
  }

  #[tokio::test]
  async fn guests_only_learn_the_presence_of_people_they_share_a_conversation_with() {
    let app = app!();
    let s = scene(&app).await;
    let mut alice = Gateway::connect(&app, &s.alice, &s.alice_device).await;
    let _ = alice.presence_of(&s.alice_id).await;

    let ext = guest("ext");
    let ext_id = user_id(&app, &ext).await;
    let listed = app
      .get(&ext, &format!("/presence?user_ids={},{}", s.alice_id, ext_id))
      .await;
    let ids: Vec<_> = listed.body["presences"]
      .as_array()
      .unwrap()
      .iter()
      .map(|p| p["user_id"].clone())
      .collect();
    assert_eq!(ids, vec![json!(ext_id)]);
    app.stop().await;
  }
}
