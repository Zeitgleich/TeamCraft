//! Login and enrollment: dev realm users get their roles, enroll their first device, and the
//! server refuses users without a TeamCraft role.

use reqwest::StatusCode;
use teamcraft_e2e::{Peer, Stack};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dev_realm_users_log_in_with_their_roles_and_enroll_a_device() {
  let Some(stack) = Stack::start().await else { return };

  for (user, role) in [
    ("alice", "owner"),
    ("bob", "admin"),
    ("carol", "member"),
    ("dave", "guest"),
  ] {
    let me = stack.get(&stack.tokens(user), "/me").await.expect(StatusCode::OK);
    assert_eq!(me.body["role"], role, "{user}: {}", me.body);
  }
  // frank has no TeamCraft role in the realm: his valid token is still refused.
  let frank = stack.get(&stack.tokens("frank"), "/me").await;
  assert!(
    frank.status == StatusCode::FORBIDDEN || frank.status == StatusCode::UNAUTHORIZED,
    "{frank:?}"
  );
  // No token at all.
  let anonymous = stack.request_with(None, reqwest::Method::GET, "/me", None).await;
  assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

  let alice = Peer::enroll(&stack, "alice").await;
  let carol = Peer::enroll(&stack, "carol").await;
  assert_ne!(
    alice.client.identity_fingerprint(),
    carol.client.identity_fingerprint()
  );

  // The server knows the enrolled device and only its public part.
  let devices = stack.get(&alice.tokens, "/devices").await.expect(StatusCode::OK);
  let listed = devices
    .body
    .as_array()
    .or_else(|| devices.body["devices"].as_array())
    .unwrap();
  assert_eq!(listed.len(), 1, "{}", devices.body);
  assert_eq!(
    listed[0]["id"].as_str().unwrap(),
    alice.client.device_id().to_string()
  );

  stack.stop().await;
}
