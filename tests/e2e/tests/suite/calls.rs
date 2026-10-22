//! A 1:1 call through the real server and SFU processes: join tokens signed by the server, device
//! bound DAVE keys from the client core, and Opus test tones decoded by the other side.

use std::time::Duration;

use reqwest::StatusCode;
use teamcraft_e2e::call::{join_call, run_call};
use teamcraft_e2e::{Peer, Stack};

const CALL_DURATION: Duration = Duration::from_secs(4);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dm_call_is_end_to_end_encrypted_through_the_sfu() {
  let Some(stack) = Stack::builder().with_calls().start().await else {
    return;
  };
  let certificate_hash = stack.maybe_sfu.as_ref().unwrap().certificate_hash;
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let dm = alice.client.open_dm(carol.user_id()).await.unwrap();
  carol.wait_joined(dm).await;

  // Nobody is in the call yet.
  let before = stack
    .get(&alice.tokens, &format!("/conversations/{dm}/call"))
    .await;
  assert!(
    before.status == StatusCode::NOT_FOUND
      || before.body["participants"].as_array().is_none_or(Vec::is_empty),
    "{before:?}"
  );

  let alice_call = join_call(&mut alice, dm, certificate_hash, 330.0).await;
  let carol_call = join_call(&mut carol, dm, certificate_hash, 550.0).await;
  // While the call runs, the server's roster (reported by the SFU) lists both.
  let observe = async {
    tokio::time::sleep(CALL_DURATION / 2).await;
    stack
      .get(&alice.tokens, &format!("/conversations/{dm}/call"))
      .await
  };
  let (alice_stats, carol_stats, during) = tokio::join!(
    run_call(alice_call, CALL_DURATION),
    run_call(carol_call, CALL_DURATION),
    observe
  );
  let during = during.expect(StatusCode::OK);
  assert!(
    during.body.to_string().contains(&alice.user_id().to_string()),
    "{}",
    during.body
  );
  assert!(
    during.body.to_string().contains(&carol.user_id().to_string()),
    "{}",
    during.body
  );

  for stats in [&alice_stats, &carol_stats] {
    assert!(stats.frames_sent > 50, "{stats:?}");
    assert!(stats.frames_received > 50, "{stats:?}");
    assert!(stats.audible_frames_played > 30, "{stats:?}");
  }
  // Both sides derived the same privacy code from the DAVE group.
  assert!(alice_stats.maybe_privacy_code.is_some());
  assert_eq!(alice_stats.maybe_privacy_code, carol_stats.maybe_privacy_code);

  stack.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_non_member_gets_no_join_token_for_a_dm_call() {
  let Some(stack) = Stack::builder().with_calls().start().await else {
    return;
  };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let bob = Peer::enroll(&stack, "bob").await;
  let dm = alice.client.open_dm(carol.user_id()).await.unwrap();
  carol.wait_joined(dm).await;

  let refused = bob.client.call_join_token(dm).await;
  assert!(
    refused.is_err(),
    "bob got a join token for a call he isn't part of"
  );

  stack.stop().await;
}
