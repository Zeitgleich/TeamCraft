//! Smoke test of a deployed instance (`E2E_DEPLOYED_URL`; the deploy dry run in docs/operations.md):
//! health endpoints through the reverse proxy, login of two dev users, and an encrypted message in
//! each direction. Skips without the variable. Run it once per fresh deployment: it enrolls the
//! first device of `alice` and `carol`.

use reqwest::StatusCode;
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_deployed_instance_is_healthy_and_delivers_messages() {
  let Some(stack) = Stack::deployed() else { return };
  for path in ["/healthz", "/readyz"] {
    let response = stack
      .http
      .get(format!("{}{path}", stack.base_url))
      .send()
      .await
      .unwrap_or_else(|error| panic!("{path}: {error}"));
    assert_eq!(response.status(), StatusCode::OK, "{path}");
  }

  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let me = stack.get(&carol.tokens, "/me").await.expect(StatusCode::OK);
  assert!(
    me.body.to_string().contains(&carol.user_id().to_string()),
    "{}",
    me.body
  );

  let space = space_with(&stack, &alice, "Project A", &[(&carol, "member")]).await;
  let channel = channel_with(&mut alice, space, "general", &mut [&mut carol]).await;
  alice
    .client
    .send_text(channel, None, "hello from the deployed instance")
    .await
    .unwrap();
  carol
    .wait_for_text(channel, "hello from the deployed instance")
    .await;
  carol.client.send_text(channel, None, "hello back").await.unwrap();
  alice.wait_for_text(channel, "hello back").await;
  stack.stop().await;
}
