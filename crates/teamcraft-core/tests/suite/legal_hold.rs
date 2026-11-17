//! Legal hold on the members' devices (issue #211, data-export.md §4.2): a conversation shows as held
//! only when an owner signed it; a hold the server invents, or one an admin tries to sign, never
//! reaches the banner.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, settle};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::workspace::{RosterRole, legal_holds};
use teamcraft_proto::{
  ConversationId, Id, LegalHold, PolicyDocument, Snowflake, UnixMillis, WorkspaceLogKind, WorkspacePolicy,
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
