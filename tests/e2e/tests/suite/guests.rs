//! What a guest (dave) can and can't see: only the conversations he was added to, no directory
//! browsing, no DMs with strangers, no workspace administration.

use std::time::Duration;

use reqwest::StatusCode;
use serde_json::json;
use teamcraft_e2e::peer::settle;
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::GuestDmStart;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_guest_sees_only_the_conversations_he_was_added_to() {
  let Some(stack) = Stack::start().await else { return };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let mut dave = Peer::enroll(&stack, "dave").await;
  let space = space_with(
    &stack,
    &alice,
    "Project A",
    &[(&carol, "member"), (&dave, "guest")],
  )
  .await;
  let shared = channel_with(&mut alice, space, "shared", &mut [&mut carol, &mut dave]).await;
  let internal = channel_with(&mut alice, space, "internal", &mut [&mut carol]).await;

  alice
    .client
    .send_text(shared, None, "for everybody")
    .await
    .unwrap();
  alice
    .client
    .send_text(internal, None, "members only")
    .await
    .unwrap();
  dave.wait_for_text(shared, "for everybody").await;
  carol.wait_for_text(internal, "members only").await;

  // The guest's conversation list holds the shared channel and nothing else.
  let listed = stack
    .get(&dave.tokens, "/conversations")
    .await
    .expect(StatusCode::OK)
    .body
    .to_string();
  assert!(listed.contains(&shared.to_string()), "{listed}");
  assert!(!listed.contains(&internal.to_string()), "{listed}");

  // The internal channel doesn't exist for him: not its details, not its messages, not its members.
  for path in [
    format!("/conversations/{internal}"),
    format!("/conversations/{internal}/messages"),
    format!("/conversations/{internal}/members"),
  ] {
    let reply = stack.get(&dave.tokens, &path).await;
    assert!(
      matches!(reply.status, StatusCode::NOT_FOUND | StatusCode::FORBIDDEN),
      "GET {path}: {reply:?}"
    );
  }
  let join = stack
    .post(
      &dave.tokens,
      &format!("/conversations/{internal}/join"),
      json!({}),
    )
    .await;
  assert!(
    !join.status.is_success(),
    "a guest joined a private channel: {join:?}"
  );
  settle(&mut [&mut dave], Duration::from_secs(1)).await;
  assert!(dave.texts(internal).is_empty());

  // He is not told about other spaces' content either and can't create anything.
  let channel = stack
    .post(
      &dave.tokens,
      &format!("/spaces/{space}/channels"),
      json!({"name": "mine", "visibility": "private"}),
    )
    .await;
  assert_eq!(channel.status, StatusCode::FORBIDDEN, "{channel:?}");
  let admin = stack.get(&dave.tokens, "/admin/users").await;
  assert_eq!(admin.status, StatusCode::FORBIDDEN, "{admin:?}");

  // By default a guest starts no conversation on his own (#887), neither with a stranger (bob) nor
  // with somebody he shares a channel with (carol); carol can always write to him.
  let bob_id = stack.user_id(&stack.tokens("bob")).await;
  let stranger = stack.post(&dave.tokens, "/dms", json!({"user_id": bob_id})).await;
  assert!(
    !stranger.status.is_success(),
    "a guest opened a DM with a stranger: {stranger:?}"
  );
  let colleague = stack
    .post(
      &dave.tokens,
      "/dms",
      json!({"user_id": carol.user_id().to_string()}),
    )
    .await;
  assert_eq!(colleague.status, StatusCode::FORBIDDEN, "{colleague:?}");
  let from_member = carol.client.open_dm(dave.user_id()).await;
  assert!(from_member.is_ok(), "{from_member:?}");

  stack.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_guest_starts_conversations_only_with_people_he_shares_one_with() {
  let Some(stack) = Stack::start().await else { return };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let mut erin = Peer::enroll(&stack, "erin").await;
  let mut dave = Peer::enroll(&stack, "dave").await;
  let space = space_with(
    &stack,
    &alice,
    "Project A",
    &[(&carol, "member"), (&erin, "member"), (&dave, "guest")],
  )
  .await;
  // Dave shares a channel with Alice and Carol, but not with Erin.
  channel_with(&mut alice, space, "customer", &mut [&mut carol, &mut dave]).await;
  let user = |peer: &Peer| peer.user_id().to_string();
  let (alice_id, carol_id, erin_id, dave_id) = (user(&alice), user(&carol), user(&erin), user(&dave));

  // The workspace lets guests start conversations with people they share one with (#887; the
  // default is that they start none).
  alice
    .set_policy(|policy| policy.guests.set_dm_start(Some(GuestDmStart::Shared)))
    .await;

  // One to one: only along a shared conversation.
  let from_guest = stack
    .post(&dave.tokens, "/dms", json!({"user_id": erin_id}))
    .await;
  assert!(
    !from_guest.status.is_success(),
    "a guest DMed a stranger: {from_guest:?}"
  );
  stack
    .post(&dave.tokens, "/dms", json!({"user_id": carol_id}))
    .await
    .expect(StatusCode::CREATED);

  // Group DMs: the guest must know *everybody* in it, not only one of them.
  let by_guest = stack
    .post(
      &dave.tokens,
      "/group-dms",
      json!({"user_ids": [carol_id, erin_id]}),
    )
    .await;
  assert!(!by_guest.status.is_success(), "{by_guest:?}");
  stack
    .post(
      &dave.tokens,
      "/group-dms",
      json!({"user_ids": [carol_id, alice_id]}),
    )
    .await
    .expect(StatusCode::CREATED);

  // Once Dave and Erin share a channel, the DM is fine.
  channel_with(&mut alice, space, "joint", &mut [&mut erin, &mut dave]).await;
  stack
    .post(&dave.tokens, "/dms", json!({"user_id": erin_id}))
    .await
    .expect(StatusCode::CREATED);

  // Members can always write to a guest, also without a shared conversation (Bob shares none).
  stack
    .post(&stack.tokens("bob"), "/dms", json!({"user_id": dave_id}))
    .await
    .expect(StatusCode::CREATED);

  stack.stop().await;
}
