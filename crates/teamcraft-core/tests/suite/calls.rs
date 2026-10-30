//! What calls take from the client: join tokens, call key bindings and verified device certificates
//! (e2ee.md §8).
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::{Arc, Mutex};

use common::{Peer, settle};
use teamcraft_client_api::call_view::CallView;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_crypto::call_binding::{CallId, SignedCallKeyBinding};
use teamcraft_proto::{ConversationId, DeviceId, UserId};

const NOW_SECS: u64 = 1_800_000_000;
const DAVE_KEY: [u8; 65] = [4; 65];

#[tokio::test]
async fn dm_peers_verify_each_others_call_keys() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let dm = alice.client.open_dm(bob.client.user_id()).await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  let token = alice.client.call_join_token(dm).await.unwrap();
  assert!(!token.token.is_empty());

  let devices = bob.client.verified_call_devices(dm).await.unwrap();
  assert_eq!(devices.len(), 2, "both devices of the DM");
  let alice_device = devices
    .iter()
    .find(|device| device.device_id().get() == alice_device_id(&alice))
    .unwrap();

  let call = CallId::new(77);
  let signed = alice
    .client
    .call_key_signer()
    .sign_call_key_binding(call, &DAVE_KEY, NOW_SECS + 300)
    .unwrap();
  let decoded = SignedCallKeyBinding::from_bytes(&signed.to_bytes().unwrap()).unwrap();
  let key = decoded.verify(alice_device, call, NOW_SECS).unwrap();
  assert_eq!(key.dave_public_key(), DAVE_KEY);
  // Bob's certificate doesn't verify Alice's binding.
  let bob_device = devices
    .iter()
    .find(|device| device.device_id().get() != alice_device_id(&alice))
    .unwrap();
  assert!(decoded.verify(bob_device, call, NOW_SECS).is_err());
}

/// Issue #976: after a 1:1 call both left, the conversation's call bar ("You are in this call on
/// another device") must go on every device, also when a call event got lost on the way. The bar
/// shows while the call view the apps keep from the call events lists the conversation's call.
mod roster {
  use super::*;

  #[tokio::test]
  async fn both_leaving_ends_the_call_for_both_in_either_order() {
    for alice_leaves_first in [true, false] {
      let (server, mut alice, mut bob, dm) = dm_peers().await;
      let (alice_view, bob_view) = (watch_calls(&mut alice), watch_calls(&mut bob));
      join(&server, &alice, dm);
      join(&server, &bob, dm);
      settle(&mut [&mut alice, &mut bob]).await;
      let both = people(&[&alice, &bob]);
      assert_eq!(shown(&alice_view, dm), Some(both.clone()));
      assert_eq!(shown(&bob_view, dm), Some(both));

      let (alice_device, bob_device) = (device_of(&alice), device_of(&bob));
      let (first, second) = if alice_leaves_first {
        (alice_device, bob_device)
      } else {
        (bob_device, alice_device)
      };
      server.leave_call(dm, first.0, first.1);
      settle(&mut [&mut alice, &mut bob]).await;
      server.leave_call(dm, second.0, second.1);
      settle(&mut [&mut alice, &mut bob]).await;

      assert_eq!(shown(&alice_view, dm), None, "alice first: {alice_leaves_first}");
      assert_eq!(shown(&bob_view, dm), None, "alice first: {alice_leaves_first}");
    }
  }

  /// The server never sent the call's end (it lost the call in a restart, say): the next
  /// connection reads the running calls and the bar goes.
  #[tokio::test]
  async fn a_lost_call_end_is_repaired_by_the_next_connection() {
    let (server, mut alice, mut bob, dm) = dm_peers().await;
    let alice_view = watch_calls(&mut alice);
    join(&server, &alice, dm);
    join(&server, &bob, dm);
    settle(&mut [&mut alice, &mut bob]).await;
    leave(&server, &alice, dm);
    server.leave_call_unannounced(dm, bob.client.user_id(), bob.client.device_id());
    settle(&mut [&mut alice, &mut bob]).await;
    assert_eq!(
      shown(&alice_view, dm),
      Some(people(&[&bob])),
      "stale until checked"
    );

    server.drop_gateways(alice.client.user_id());
    reconnect(&mut alice).await;

    assert_eq!(shown(&alice_view, dm), None);
  }

  /// Both leave while this device is away long enough for its gateway session to be forgotten: the
  /// events in between are gone, and the fresh session's check removes the call.
  #[tokio::test]
  async fn a_call_that_ended_while_the_session_was_forgotten_is_gone_after_connecting() {
    let (server, mut alice, mut bob, dm) = dm_peers().await;
    let alice_view = watch_calls(&mut alice);
    join(&server, &alice, dm);
    join(&server, &bob, dm);
    settle(&mut [&mut alice, &mut bob]).await;

    server.drop_gateways(alice.client.user_id());
    server.expire_sessions();
    leave(&server, &bob, dm);
    leave(&server, &alice, dm);
    assert_eq!(shown(&alice_view, dm), Some(people(&[&alice, &bob])));
    reconnect(&mut alice).await;

    assert_eq!(shown(&alice_view, dm), None);
  }

  /// A reconnect between the two leaves: the first leave arrives with the resume or the check, the
  /// second as an event, and nothing is left over.
  #[tokio::test]
  async fn a_reconnect_between_the_leaves_leaves_nothing_behind() {
    for is_session_forgotten in [false, true] {
      let (server, mut alice, mut bob, dm) = dm_peers().await;
      let alice_view = watch_calls(&mut alice);
      join(&server, &alice, dm);
      join(&server, &bob, dm);
      settle(&mut [&mut alice, &mut bob]).await;

      server.drop_gateways(alice.client.user_id());
      if is_session_forgotten {
        server.expire_sessions();
      }
      leave(&server, &bob, dm);
      reconnect(&mut alice).await;
      settle(&mut [&mut alice]).await;
      assert_eq!(
        shown(&alice_view, dm),
        Some(people(&[&alice])),
        "session forgotten: {is_session_forgotten}"
      );
      leave(&server, &alice, dm);
      settle(&mut [&mut alice]).await;

      assert_eq!(
        shown(&alice_view, dm),
        None,
        "session forgotten: {is_session_forgotten}"
      );
    }
  }

  /// A call that started while the device was away shows once it is connected again, and a check
  /// that finds nothing new tells the call handling nothing.
  #[tokio::test]
  async fn the_check_adds_a_missed_call_and_is_quiet_when_nothing_changed() {
    let (server, mut alice, bob, dm) = dm_peers().await;
    let events = Arc::new(Mutex::new(0_usize));
    let (alice_view, counted) = (watch_calls(&mut alice), Arc::clone(&events));
    let view = Arc::clone(&alice_view);
    alice.client.set_call_event_sink(Box::new(move |event| {
      *counted.lock().unwrap() += 1;
      view.lock().unwrap().apply_gateway_event(event);
    }));
    server.drop_gateways(alice.client.user_id());
    server.expire_sessions();
    join(&server, &bob, dm);

    reconnect(&mut alice).await;
    assert_eq!(shown(&alice_view, dm), Some(people(&[&bob])));
    let heard = *events.lock().unwrap();

    alice.client.reconcile_calls().await.unwrap();
    assert_eq!(*events.lock().unwrap(), heard, "nothing changed");
  }

  async fn dm_peers() -> (FakeServer, Peer, Peer, ConversationId) {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    let dm = alice.client.open_dm(bob.client.user_id()).await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    (server, alice, bob, dm)
  }

  /// The call view an app keeps from the peer's call events.
  fn watch_calls(peer: &mut Peer) -> Arc<Mutex<CallView>> {
    let view = Arc::new(Mutex::new(CallView::default()));
    let sink = Arc::clone(&view);
    peer.client.set_call_event_sink(Box::new(move |event| {
      sink.lock().unwrap().apply_gateway_event(event);
    }));
    view
  }

  /// Who the call bar shows in the conversation, `None` without a bar.
  fn shown(view: &Mutex<CallView>, conversation: ConversationId) -> Option<Vec<UserId>> {
    view
      .lock()
      .unwrap()
      .running
      .get(&conversation)
      .map(|call| call.participants.clone())
  }

  fn people(peers: &[&Peer]) -> Vec<UserId> {
    let mut users: Vec<UserId> = peers.iter().map(|peer| peer.client.user_id()).collect();
    users.sort_unstable();
    users
  }

  fn device_of(peer: &Peer) -> (UserId, DeviceId) {
    (peer.client.user_id(), peer.client.device_id())
  }

  fn join(server: &FakeServer, peer: &Peer, conversation: ConversationId) {
    server.join_call(conversation, peer.client.user_id(), peer.client.device_id());
  }

  fn leave(server: &FakeServer, peer: &Peer, conversation: ConversationId) {
    server.leave_call(conversation, peer.client.user_id(), peer.client.device_id());
  }

  async fn reconnect(peer: &mut Peer) {
    let (gateway, _) = peer.client.connect_gateway().await.unwrap();
    peer.gateway = gateway;
  }
}

fn alice_device_id(alice: &Peer) -> u64 {
  u64::try_from(alice.client.device_id().to_string().parse::<i64>().unwrap()).unwrap()
}
