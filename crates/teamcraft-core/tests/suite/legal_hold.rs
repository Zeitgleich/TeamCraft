//! Legal hold on the members' devices (issue #211, data-export.md §4.2): a conversation shows as held
//! only when an owner signed it; a hold the server invents, or one an admin tries to sign, never
//! reaches the banner. A person's hold (issue #2135) shows in every conversation they are in, from
//! the same shared list every app reads, and a deletion of an own message names it to the server.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, settle};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::workspace::{RosterRole, legal_holds};
use teamcraft_proto::{
  ConversationId, Id, LegalHold, PersonHoldScope, PersonLegalHold, PolicyDocument, Snowflake, UnixMillis,
  UserId, WorkspaceLogKind, WorkspacePolicy,
};

#[tokio::test]
async fn members_see_a_hold_only_when_an_owner_signed_it() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner), (bob_id, RosterRole::Admin)])
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  assert!(
    legal_holds(carol.client.store()).is_empty(),
    "nothing held before a policy"
  );

  // An admin can't sign one.
  let refused = bob.client.publish_policy(held(&[(1, "Case 1")])).await;
  assert!(matches!(refused, Err(teamcraft_core::CoreError::NotAnOwner)));

  // An owner can't sign an invalid list either.
  let invalid = alice.client.publish_policy(held(&[(1, "")])).await;
  assert!(matches!(
    invalid,
    Err(teamcraft_core::CoreError::InvalidRequest(_))
  ));

  alice.client.publish_policy(held(&[(1, "Case 1")])).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  let holds = legal_holds(carol.client.store());
  assert_eq!(holds.len(), 1);
  assert_eq!(holds[0].conversation_id, conversation(1));
  assert_eq!(holds[0].reference, "Case 1");

  // The server appends a policy nobody signed that holds another conversation: ignored.
  let forged = PolicyDocument {
    version: 2,
    policy: held(&[(1, "Case 1"), (2, "Forged")]),
  };
  server.inject_workspace_entry(
    WorkspaceLogKind::Policy,
    serde_json::to_vec(&forged).unwrap(),
    vec![1, 2, 3],
  );
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  let holds = legal_holds(carol.client.store());
  assert_eq!(holds.len(), 1, "{holds:?}");
  assert_eq!(holds[0].conversation_id, conversation(1));
}

#[tokio::test]
async fn a_person_hold_shows_in_their_conversations_only() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner)])
    .await
    .unwrap();
  let with_bob = carol.client.open_dm(bob_id).await.unwrap();
  let with_alice = carol.client.open_dm(alice_id).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;

  alice
    .client
    .publish_policy(person_held(bob_id, PersonHoldScope::Direct, "Case 5"))
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  for peer in [&carol, &bob] {
    let holds = legal_holds(peer.client.store());
    assert_eq!(holds.len(), 1, "{}: {holds:?}", peer.name);
    assert_eq!(holds[0].conversation_id, with_bob);
    assert_eq!(holds[0].reference, "Case 5");
  }
  // The desktop's banner asks the same question per conversation.
  #[cfg(feature = "ui")]
  {
    use teamcraft_client_api::backend::ChatBackend;
    let backend = common::backend_of(&carol);
    assert_eq!(
      backend.legal_hold(with_bob).map(|hold| hold.reference),
      Some("Case 5".to_owned())
    );
    assert_eq!(backend.legal_hold(with_alice), None);
  }
  assert!(
    legal_holds(carol.client.store())
      .iter()
      .all(|hold| hold.conversation_id != with_alice)
  );

  // Released: the banner goes.
  alice
    .client
    .publish_policy(WorkspacePolicy::default())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  assert!(legal_holds(carol.client.store()).is_empty());
}

#[tokio::test]
async fn deleting_an_own_message_names_it_to_the_server() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let dm = alice.client.open_dm(bob.client.user_id()).await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  let kept = alice.client.send_text(dm, None, "stays").await.unwrap();
  let withdrawn = alice.client.send_text(dm, None, "goes").await.unwrap();
  let deletion = alice.client.delete_message(dm, withdrawn).await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  let stored = server.stored_client_ids(dm);
  assert!(stored.contains(&kept) && stored.contains(&deletion), "{stored:?}");
  assert!(!stored.contains(&withdrawn), "{stored:?}");
  assert_eq!(bob.texts(dm), vec!["stays".to_owned()]);
}

fn person_held(user: UserId, scope: PersonHoldScope, reference: &str) -> WorkspacePolicy {
  let mut policy = WorkspacePolicy::default();
  policy.compliance_archive.person_holds = vec![PersonLegalHold {
    user_id: user,
    scope,
    reference: reference.to_owned(),
    placed_at: UnixMillis(1_700_000_000_000),
  }];
  policy
}

fn held(holds: &[(i64, &str)]) -> WorkspacePolicy {
  let mut policy = WorkspacePolicy::default();
  policy.compliance_archive.legal_holds = holds
    .iter()
    .map(|(id, reference)| LegalHold {
      conversation_id: conversation(*id),
      reference: (*reference).to_owned(),
      placed_at: UnixMillis(1_700_000_000_000),
    })
    .collect();
  policy
}

fn conversation(id: i64) -> ConversationId {
  ConversationId::from_snowflake(Snowflake::new(id).unwrap())
}
