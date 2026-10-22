//! v1 acceptance, PLAN.md §8 Calls, signalling side: the server rings a DM, announces the call
//! start, roster changes and end through the gateway, and lists the call to members only. (Media and
//! DAVE are covered by calls.rs.)
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use reqwest::StatusCode;
use teamcraft_e2e::call::{join_call, run_call};
use teamcraft_e2e::peer::settle;
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::GatewayEvent;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dm_call_rings_starts_updates_and_ends_through_the_gateway() {
  let Some(stack) = Stack::builder().with_calls().start().await else {
    return;
  };
  let hash = stack.maybe_sfu.as_ref().unwrap().certificate_hash;
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let erin = Peer::enroll(&stack, "erin").await;
  let dm = alice.client.open_dm(carol.user_id()).await.unwrap();
  carol.wait_joined(dm).await;
  let events = Rc::new(RefCell::new(Vec::<String>::new()));
  let sink = events.clone();
  carol
    .client
    .set_call_event_sink(Box::new(move |event: &GatewayEvent| {
      let name = match event {
        GatewayEvent::CallRing { .. } => "ring",
        GatewayEvent::CallStart { .. } => "start",
        GatewayEvent::CallUpdate { .. } => "update",
        GatewayEvent::CallEnd { .. } => "end",
        _ => return,
      };
      sink.borrow_mut().push(name.to_owned());
    }));

  // Alice starts the call: Carol's devices ring and see the call start.
  let alice_call = join_call(&mut alice, dm, hash, 330.0).await;
  carol
    .wait_until("the ring arrives", |_| {
      events.borrow().iter().any(|name| name == "ring")
    })
    .await;
  assert!(
    events.borrow().iter().any(|name| name == "start"),
    "{:?}",
    events.borrow()
  );

  // Carol joins; both talk for a moment, then everybody leaves and the call ends.
  let carol_call = join_call(&mut carol, dm, hash, 550.0).await;
  let (_, _) = tokio::join!(
    run_call(alice_call, Duration::from_secs(3)),
    run_call(carol_call, Duration::from_secs(3))
  );
  settle(&mut [&mut carol], Duration::from_secs(3)).await;
  let seen = events.borrow().clone();
  assert!(
    seen.iter().any(|name| name == "update"),
    "roster changes: {seen:?}"
  );
  assert!(seen.iter().any(|name| name == "end"), "the end: {seen:?}");

  // The call history lists the finished call to both participants (PLAN.md §8): Alice started
  // it, Carol joined; it was not missed, and an outsider sees nothing.
  for viewer in [&alice, &carol] {
    let history = stack
      .get(&viewer.tokens, "/calls/history")
      .await
      .expect(StatusCode::OK);
    let calls = history.body["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 1, "{}", history.body);
    assert_eq!(calls[0]["conversation_id"], dm.to_string());
    assert_eq!(calls[0]["is_missed"], false, "{}", history.body);
    assert_eq!(calls[0]["participants"].as_array().unwrap().len(), 2);
    assert_eq!(calls[0]["started_by"], alice.user_id().to_string());
    assert!(calls[0].get("ended_at").is_some(), "{}", history.body);
  }
  let outsider_history = stack
    .get(&erin.tokens, "/calls/history")
    .await
    .expect(StatusCode::OK);
  assert_eq!(outsider_history.body["calls"], serde_json::json!([]));

  // Only the conversation's members see or join a call.
  let outsider = stack
    .get(&erin.tokens, &format!("/conversations/{dm}/call"))
    .await;
  assert!(
    matches!(outsider.status, StatusCode::NOT_FOUND | StatusCode::FORBIDDEN),
    "{outsider:?}"
  );
  assert!(erin.client.call_join_token(dm).await.is_err());

  stack.stop().await;
}
