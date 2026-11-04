//! Owners' apps signing the admin roster on what the identity provider signed (e2ee.md §3.1.3, issue
//! #1276), against the fake server as a hostile or an honest one and a fake provider.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::SigningKey;
use p256::ecdsa::signature::Signer as _;
use serde_json::{Value, json};
use teamcraft_core::JwksSource;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::RoleSigningProblem;
use teamcraft_core::workspace::RosterRole;
use teamcraft_proto::{
  RoleSigningMode, RoleSigningPolicy, UserId, WithdrawnReason, WithdrawnRole, WorkspaceLogKind,
  WorkspacePolicy,
};

use crate::common::{Peer, settle};

const ISSUER: &str = "https://id.example.com/realms/company";
const CLIENT: &str = "teamcraft";
/// The provider's clock in these tests (seconds).
const T0: i64 = 1_000_000;

mod first_roster {
  use super::*;

  #[tokio::test]
  async fn the_first_owners_app_signs_itself_with_nobody_asked() {
    let world = World::new();
    let mut alice = world.peer("Alice").await;
    world.give_token(&alice, &["owner"], T0);

    alice.client.run_role_signing().await.unwrap();

    let workspace = alice.client.workspace().unwrap();
    assert_eq!(role_of(&alice, alice.client.user_id()), Some(RosterRole::Owner));
    assert_eq!(
      workspace.roster.members.len(),
      1,
      "only what its own token proves"
    );
    let root = workspace.maybe_root.unwrap();
    assert_eq!(root.user_id, alice.client.user_id());
    // The provider is pinned in the signed policy from then on.
    let provider = workspace
      .policy
      .maybe_role_signing
      .unwrap()
      .maybe_provider
      .unwrap();
    assert_eq!(provider.issuer, ISSUER);
    assert_eq!(provider.client_id, CLIENT);
  }

  #[tokio::test]
  async fn a_hostile_server_can_not_make_a_member_or_an_admin_the_root() {
    let world = World::new();
    let mut carol = world.peer("Carol").await;
    let mut bob = world.peer("Bob").await;
    world.give_token(&carol, &["member"], T0);
    world.give_token(&bob, &["admin"], T0);

    // The server has no roster to show, and says what it likes about roles. Only tokens count.
    carol.client.run_role_signing().await.unwrap();
    bob.client.run_role_signing().await.unwrap();

    assert_eq!(carol.client.workspace().unwrap().roster.version, 0);
    assert_eq!(bob.client.workspace().unwrap().roster.version, 0);
  }

  #[tokio::test]
  async fn a_token_a_hostile_server_made_up_starts_nothing() {
    let world = World::new();
    let mut mallory = world.peer("Mallory").await;
    let impostor = Provider::new();
    let claim = mallory.client.identity_attestation_claim();
    world.server.set_provider_token(
      mallory.client.user_id(),
      impostor.token(&with_roles(&claim, &["owner"], T0)),
    );

    mallory.client.run_role_signing().await.unwrap();

    assert_eq!(mallory.client.workspace().unwrap().roster.version, 0);
  }

  #[tokio::test]
  async fn a_device_that_pinned_a_root_starts_no_new_one() {
    let world = World::new();
    let mut alice = world.peer("Alice").await;
    world.give_token(&alice, &["owner"], T0);
    alice.client.run_role_signing().await.unwrap();
    let root_before = alice.client.workspace().unwrap().maybe_root.unwrap();

    // The server pretends the log is empty.
    world.server.clear_workspace_logs();
    alice.client.note_role_evidence_update();
    world.give_token(&alice, &["owner"], T0 + 10);
    alice.client.run_role_signing().await.unwrap();

    assert_eq!(world.server.workspace_log_len(&WorkspaceLogKind::Roster), 0);
    assert_eq!(alice.client.workspace().unwrap().maybe_root.unwrap(), root_before);
    // Somebody who never saw a root and really is an owner may start one: they appoint only themselves,
    // and every device that pinned the real root refuses it.
    let mut new_device = world.peer("Dora").await;
    world.give_token(&new_device, &["owner"], T0 + 10);
    new_device.client.run_role_signing().await.unwrap();
    let members = new_device.client.workspace().unwrap().roster.members;
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].user_id, new_device.client.user_id());
  }
}

mod additions {
  use super::*;

  #[tokio::test]
  async fn an_instance_with_unsigned_roles_ends_with_rights_in_force_and_nobody_asked() {
    // An instance that updated: nothing signed, the provider lists an owner, a second owner, an admin.
    let world = World::new();
    let mut alice = world.peer("Alice").await;
    let mut dora = world.peer("Dora").await;
    let mut bob = world.peer("Bob").await;
    let mut carol = world.peer("Carol").await;
    world.give_token(&alice, &["owner"], T0);
    world.give_token(&dora, &["owner"], T0);
    world.give_token(&bob, &["admin", "member"], T0);
    world.give_token(&carol, &["member"], T0);

    for peer in [&mut alice, &mut dora, &mut bob, &mut carol] {
      peer.client.run_role_signing().await.unwrap();
    }
    settle(&mut [&mut alice, &mut dora, &mut bob, &mut carol]).await;
    alice.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut dora, &mut bob, &mut carol]).await;

    let (alice_id, dora_id, bob_id, carol_id) = (
      alice.client.user_id(),
      dora.client.user_id(),
      bob.client.user_id(),
      carol.client.user_id(),
    );
    for peer in [&carol, &bob, &alice, &dora] {
      assert_eq!(role_of(peer, alice_id), Some(RosterRole::Owner), "{}", peer.name);
      assert_eq!(role_of(peer, dora_id), Some(RosterRole::Owner), "{}", peer.name);
      assert_eq!(role_of(peer, bob_id), Some(RosterRole::Admin), "{}", peer.name);
      assert_eq!(role_of(peer, carol_id), None, "{}", peer.name);
    }
    assert_eq!(carol.client.role_signing_record().unwrap().maybe_note, None);
    // A member's app has nothing to do with this: it published nothing.
    assert_eq!(world.server.role_evidence_of(carol_id), None);
  }

  #[tokio::test]
  async fn the_audit_line_says_what_changed_and_why() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    world.give_token(&bob, &["admin"], T0);
    bob.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.run_role_signing().await.unwrap();

    let roster = alice.client.workspace().unwrap().roster;
    let change = roster
      .automatic
      .iter()
      .find(|change| change.user_id == bob.client.user_id())
      .unwrap();
    assert_eq!(
      change.change,
      teamcraft_core::role_signing::AutomaticChangeKind::Admin
    );
    assert_eq!(
      change.basis,
      teamcraft_core::role_signing::AutomaticBasis::ProviderToken
    );
    assert_eq!(change.maybe_issued_at, Some(T0));
  }

  #[tokio::test]
  async fn several_owners_at_once_end_on_one_roster() {
    let world = World::new();
    let (mut alice, mut dora) = world.owner_and_peer("Dora").await;
    world.give_token(&dora, &["owner"], T0);
    dora.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut dora]).await;
    alice.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut dora]).await;
    let mut bob = world.peer("Bob").await;
    world.give_token(&bob, &["admin"], T0 + 1);
    bob.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut dora, &mut bob]).await;

    // Both see the evidence and race for the version. One wins; the other's try fails, and its next
    // round finds nothing left to do.
    let (first, second) =
      futures_util::join!(alice.client.run_role_signing(), dora.client.run_role_signing());
    assert!(first.is_ok() || second.is_ok());
    settle(&mut [&mut alice, &mut dora, &mut bob]).await;
    alice.client.note_role_evidence_update();
    dora.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();
    dora.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut dora, &mut bob]).await;

    let bob_id = bob.client.user_id();
    assert_eq!(role_of(&alice, bob_id), Some(RosterRole::Admin));
    assert_eq!(role_of(&dora, bob_id), Some(RosterRole::Admin));
    assert_eq!(
      alice.client.workspace().unwrap().heads,
      dora.client.workspace().unwrap().heads,
      "one log"
    );
    assert!(alice.client.workspace().unwrap().maybe_alarm.is_none());
  }

  #[tokio::test]
  async fn with_no_owner_online_it_waits_and_nobody_is_told_anything() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    world.give_token(&bob, &["admin"], T0);
    bob.client.run_role_signing().await.unwrap();
    assert!(world.server.role_evidence_of(bob.client.user_id()).is_some());
    let bob_id = bob.client.user_id();

    // Nobody signs; Bob's app says nothing and asks the server nothing while it waits.
    let calls = world.server.rest_call_count(bob_id);
    for _ in 0..3 {
      bob.client.run_role_signing().await.unwrap();
    }
    assert_eq!(world.server.rest_call_count(bob_id), calls);
    assert_eq!(bob.client.role_signing_record().unwrap().maybe_note, None);
    assert_eq!(role_of(&bob, bob_id), None);

    // The owner comes back, however late: the role is in force with no click.
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    assert_eq!(role_of(&bob, bob_id), Some(RosterRole::Admin));
  }

  #[tokio::test]
  async fn while_waiting_for_weeks_an_app_keeps_its_evidence_recent() {
    let world = World::new();
    let (_alice, mut bob) = world.owner_and_peer("Bob").await;
    let bob_id = bob.client.user_id();
    world.give_token(&bob, &["admin"], T0);
    bob.client.run_role_signing().await.unwrap();
    let first = world.server.role_evidence_of(bob_id).unwrap();

    // A refresh shortly after publishes nothing; one a day later does.
    world.give_token(&bob, &["admin"], T0 + 300);
    bob.client.run_role_signing().await.unwrap();
    assert_eq!(world.server.role_evidence_of(bob_id).unwrap(), first);
    world.give_token(&bob, &["admin"], T0 + 24 * 60 * 60);
    bob.client.run_role_signing().await.unwrap();
    assert_ne!(world.server.role_evidence_of(bob_id).unwrap(), first);
  }

  #[tokio::test]
  async fn an_admin_can_not_sign() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    world.give_token(&bob, &["admin"], T0);
    bob.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    let mut carol = world.peer("Carol").await;
    world.give_token(&carol, &["admin"], T0 + 1);
    carol.client.run_role_signing().await.unwrap();
    settle(&mut [&mut bob, &mut carol]).await;
    let version = bob.client.workspace().unwrap().roster.version;

    bob.client.note_role_evidence_update();
    bob.client.run_role_signing().await.unwrap();

    assert_eq!(bob.client.workspace().unwrap().roster.version, version);
  }

  #[tokio::test]
  async fn nothing_goes_out_while_nothing_changes() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    world.give_token(&bob, &["admin"], T0);
    bob.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    bob.client.run_role_signing().await.unwrap();
    alice.client.run_role_signing().await.unwrap();
    let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
    let (alice_calls, bob_calls) = (
      world.server.rest_call_count(alice_id),
      world.server.rest_call_count(bob_id),
    );

    // Tokens refresh every few minutes all day. That costs a local comparison and no request.
    for minute in 1..5 {
      world.give_token(&alice, &["owner"], T0 + 300 * minute);
      world.give_token(&bob, &["admin"], T0 + 300 * minute);
      alice.client.run_role_signing().await.unwrap();
      bob.client.run_role_signing().await.unwrap();
    }

    assert_eq!(world.server.rest_call_count(alice_id), alice_calls);
    assert_eq!(world.server.rest_call_count(bob_id), bob_calls);
  }
}

mod hostile_server {
  use super::*;

  #[tokio::test]
  async fn a_forged_token_appoints_nobody() {
    let world = World::new();
    let (mut alice, mallory) = world.owner_and_peer("Mallory").await;
    let claim = mallory.client.identity_attestation_claim();
    let forged = Provider::new().token(&with_roles(&claim, &["owner"], T0 + 5));
    world.server.forge_role_evidence(
      mallory.client.user_id(),
      forged,
      mallory.client.identity_public_key_bytes().to_vec(),
    );
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();

    assert_eq!(role_of(&alice, mallory.client.user_id()), None);
  }

  #[tokio::test]
  async fn a_token_of_another_issuer_or_client_appoints_nobody() {
    let world = World::new();
    let (mut alice, mallory) = world.owner_and_peer("Mallory").await;
    let claim = mallory.client.identity_attestation_claim();
    let key = mallory.client.identity_public_key_bytes().to_vec();
    let user = mallory.client.user_id();
    for token in [
      with_roles(&claim, &["owner"], T0 + 5).with("iss", json!("https://evil.example")),
      with_roles(&claim, &["owner"], T0 + 5).with("aud", json!("other-client")),
    ] {
      world
        .server
        .forge_role_evidence(user, world.provider.token(&token), key.clone());
    }
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();

    assert_eq!(role_of(&alice, user), None);
  }

  #[tokio::test]
  async fn a_real_token_shown_with_the_servers_own_key_appoints_nobody() {
    // The server takes Bob's real token and offers its own key in his place.
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    world.give_token(&bob, &["admin"], T0);
    bob.client.run_role_signing().await.unwrap();
    let real = world.server.role_evidence_of(bob.client.user_id()).unwrap();
    world.server.clear_role_evidence();
    world
      .server
      .forge_role_evidence(bob.client.user_id(), real, vec![7; 32]);
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();

    assert_eq!(role_of(&alice, bob.client.user_id()), None);
  }

  #[tokio::test]
  async fn a_real_token_shown_for_another_person_appoints_nobody() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    let mallory = world.peer("Mallory").await;
    world.give_token(&bob, &["admin"], T0);
    bob.client.run_role_signing().await.unwrap();
    let real = world.server.role_evidence_of(bob.client.user_id()).unwrap();
    world.server.clear_role_evidence();
    world.server.forge_role_evidence(
      mallory.client.user_id(),
      real,
      mallory.client.identity_public_key_bytes().to_vec(),
    );
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();

    assert_eq!(role_of(&alice, mallory.client.user_id()), None);
  }

  #[tokio::test]
  async fn once_pinned_a_token_of_another_provider_stops_the_owners_app() {
    // Tokens of another issuer stop the app rather than count, and a person hears of it.
    let world = World::new();
    let (mut alice, _bob) = world.owner_and_peer("Bob").await;
    let impostor = Provider::new();
    let claim = alice.client.identity_attestation_claim();
    world.server.set_provider_token(
      alice.client.user_id(),
      impostor.token(&with_roles(&claim, &["owner"], T0 + 5).with("iss", json!("https://evil.example"))),
    );
    let version = alice.client.workspace().unwrap().roster.version;

    alice.client.run_role_signing().await.unwrap();

    assert!(matches!(
      alice.client.role_signing_record().unwrap().maybe_note,
      Some(RoleSigningProblem::ProviderChanged { .. })
    ));
    assert_eq!(alice.client.workspace().unwrap().roster.version, version);
  }

  #[tokio::test]
  async fn the_servers_word_takes_an_admin_off_and_puts_nobody_on() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    world.give_token(&bob, &["admin"], T0);
    bob.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    let bob_id = bob.client.user_id();
    assert_eq!(role_of(&alice, bob_id), Some(RosterRole::Admin));

    world.server.set_withdrawn_roles(vec![WithdrawnRole {
      user_id: bob_id,
      reason: WithdrawnReason::AccountGone,
    }]);
    world.give_token(&alice, &["owner"], T0 + 100);
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();
    assert_eq!(role_of(&alice, bob_id), None);

    // Reporting the person back isn't enough, and the token still stored isn't either.
    world.server.set_withdrawn_roles(Vec::new());
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();
    assert_eq!(role_of(&alice, bob_id), None);

    // Bob's next token is.
    settle(&mut [&mut alice, &mut bob]).await;
    world.give_token(&bob, &["admin"], T0 + 200);
    world.give_token(&alice, &["owner"], T0 + 200);
    bob.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.run_role_signing().await.unwrap();
    assert_eq!(role_of(&alice, bob_id), Some(RosterRole::Admin));
  }
}

mod demotion {
  use super::*;

  #[tokio::test]
  async fn a_newer_token_without_the_role_removes_it_and_an_old_one_does_not_bring_it_back() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    world.give_token(&bob, &["admin"], T0);
    bob.client.run_role_signing().await.unwrap();
    let appointing = world.server.role_evidence_of(bob.client.user_id()).unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    let bob_id = bob.client.user_id();

    // Bob's app publishes the token that shows the demotion, unasked.
    world.give_token(&bob, &["member"], T0 + 100);
    world.give_token(&alice, &["owner"], T0 + 100);
    bob.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    assert_eq!(role_of(&alice, bob_id), None);
    assert_eq!(role_of(&bob, bob_id), None);

    // The server replays the token from when Bob was an admin.
    world.server.clear_role_evidence();
    world.server.forge_role_evidence(
      bob_id,
      appointing,
      bob.client.identity_public_key_bytes().to_vec(),
    );
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();
    assert_eq!(role_of(&alice, bob_id), None);

    // On another owner's device too: the floor is part of the signed roster.
    let mut dora = world.peer("Dora").await;
    dora.client.sync_workspace().await.unwrap();
    assert_eq!(
      dora.client.workspace().unwrap().provider_marks[&bob_id].issued_at,
      T0 + 100
    );
  }

  #[tokio::test]
  async fn a_members_token_naming_an_admin_removes_nobody() {
    // User ID and key are public: Mallory writes them into her own attribute and gets a real token.
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    world.give_token(&bob, &["admin"], T0);
    bob.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;

    let claim = bob.client.identity_attestation_claim();
    let token = world
      .provider
      .token(&with_roles(&claim, &["member"], T0 + 50).with("sub", json!("sub-of-mallory")));
    world.server.clear_role_evidence();
    world.server.forge_role_evidence(
      bob.client.user_id(),
      token,
      bob.client.identity_public_key_bytes().to_vec(),
    );
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();

    assert_eq!(role_of(&alice, bob.client.user_id()), Some(RosterRole::Admin));
    assert!(matches!(
      alice.client.role_signing_record().unwrap().maybe_note,
      Some(RoleSigningProblem::Contradictory { .. })
    ));
  }

  #[tokio::test]
  async fn an_owner_the_provider_no_longer_calls_owner_signs_nothing_by_itself() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    world.give_token(&bob, &["admin"], T0);
    bob.client.run_role_signing().await.unwrap();
    world.give_token(&alice, &["admin"], T0 + 10);
    settle(&mut [&mut alice, &mut bob]).await;
    let version = alice.client.workspace().unwrap().roster.version;
    alice.client.run_role_signing().await.unwrap();
    assert_eq!(alice.client.workspace().unwrap().roster.version, version);
    assert_eq!(role_of(&alice, bob.client.user_id()), None);
  }
}

mod degraded {
  use super::*;

  #[tokio::test]
  async fn without_the_role_mapper_the_admin_hears_of_it_quietly_and_nothing_is_signed() {
    let world = World::new();
    let mut bob = world.peer("Bob").await;
    let claim = bob.client.identity_attestation_claim();
    let id_token = world.provider.token(&base_claims(&claim, T0));
    world.server.set_provider_token(bob.client.user_id(), id_token);
    // The access token has always carried roles, so the app knows whom this concerns.
    world.server.set_provider_access_token(
      bob.client.user_id(),
      world
        .provider
        .token(&json!({ "resource_access": { CLIENT: { "roles": ["admin"] } } })),
    );

    bob.client.run_role_signing().await.unwrap();

    assert_eq!(
      bob.client.role_signing_record().unwrap().maybe_note,
      Some(RoleSigningProblem::NoRoleClaim)
    );
    assert_eq!(world.server.role_evidence_of(bob.client.user_id()), None);
    assert_eq!(bob.client.workspace().unwrap().roster.version, 0);
  }

  #[tokio::test]
  async fn without_the_role_mapper_a_member_hears_nothing() {
    let world = World::new();
    let mut carol = world.peer("Carol").await;
    let claim = carol.client.identity_attestation_claim();
    world.server.set_provider_token(
      carol.client.user_id(),
      world.provider.token(&base_claims(&claim, T0)),
    );
    carol.client.run_role_signing().await.unwrap();
    assert_eq!(carol.client.role_signing_record().unwrap().maybe_note, None);
  }

  #[tokio::test]
  async fn once_the_mapper_is_there_the_note_goes_and_the_roster_follows() {
    let world = World::new();
    let mut alice = world.peer("Alice").await;
    let claim = alice.client.identity_attestation_claim();
    let alice_id = alice.client.user_id();
    world
      .server
      .set_provider_token(alice_id, world.provider.token(&base_claims(&claim, T0)));
    world.server.set_provider_access_token(
      alice_id,
      world
        .provider
        .token(&json!({ "resource_access": { CLIENT: { "roles": ["owner"] } } })),
    );
    alice.client.run_role_signing().await.unwrap();
    assert_eq!(
      alice.client.role_signing_record().unwrap().maybe_note,
      Some(RoleSigningProblem::NoRoleClaim)
    );

    world.give_token(&alice, &["owner"], T0 + 1);
    alice.client.run_role_signing().await.unwrap();

    assert_eq!(alice.client.role_signing_record().unwrap().maybe_note, None);
    assert_eq!(role_of(&alice, alice_id), Some(RosterRole::Owner));
  }

  #[tokio::test]
  async fn the_provider_being_down_is_retried_and_says_nothing() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    world.give_token(&bob, &["admin"], T0);
    bob.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.set_jwks_source(Box::new(NoKeys));

    assert!(alice.client.run_role_signing().await.is_err());
    assert_eq!(alice.client.role_signing_record().unwrap().maybe_note, None);
    assert_eq!(role_of(&alice, bob.client.user_id()), None);
  }
}

mod manual {
  use super::*;

  #[tokio::test]
  async fn a_signed_policy_can_require_an_owner_to_sign_by_hand() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    let mut policy = alice.client.workspace().unwrap().policy;
    policy.maybe_role_signing = Some(RoleSigningPolicy {
      mode: RoleSigningMode::Manual,
      ..policy.maybe_role_signing.unwrap()
    });
    alice.client.publish_policy(policy).await.unwrap();
    world.give_token(&bob, &["admin"], T0);
    bob.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;

    alice.client.run_role_signing().await.unwrap();

    assert_eq!(role_of(&alice, bob.client.user_id()), None);
    // Owners sign by hand as ever.
    let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner), (bob_id, RosterRole::Admin)])
      .await
      .unwrap();
    assert_eq!(role_of(&alice, bob_id), Some(RosterRole::Admin));
  }

  #[tokio::test]
  async fn a_policy_that_does_not_name_role_signing_does_not_change_it() {
    // An older owner's app signs the policy without the field it doesn't know.
    let world = World::new();
    let (mut alice, _bob) = world.owner_and_peer("Bob").await;
    let mut policy = alice.client.workspace().unwrap().policy;
    policy.maybe_role_signing = Some(RoleSigningPolicy {
      mode: RoleSigningMode::Manual,
      ..policy.maybe_role_signing.unwrap()
    });
    alice.client.publish_policy(policy).await.unwrap();

    let without = WorkspacePolicy {
      maybe_role_signing: None,
      ..alice.client.workspace().unwrap().policy
    };
    alice.client.publish_policy(without).await.unwrap();

    let kept = alice
      .client
      .workspace()
      .unwrap()
      .policy
      .maybe_role_signing
      .unwrap();
    assert_eq!(kept.mode, RoleSigningMode::Manual);
    assert!(kept.maybe_provider.is_some());
  }
}

/// A server, a provider and how they meet the apps.
struct World {
  server: FakeServer,
  provider: Arc<Provider>,
}

impl World {
  fn new() -> Self {
    Self {
      server: FakeServer::new(),
      provider: Arc::new(Provider::new()),
    }
  }

  async fn peer(&self, name: &str) -> Peer {
    let mut peer = Peer::enroll(&self.server, name).await;
    peer.client.set_jwks_source(Box::new(Jwks(self.provider.clone())));
    peer
  }

  /// An owner whose first roster is signed, and another person.
  async fn owner_and_peer(&self, name: &str) -> (Peer, Peer) {
    let mut owner = self.peer("Alice").await;
    self.give_token(&owner, &["owner"], T0);
    owner.client.run_role_signing().await.unwrap();
    assert_eq!(role_of(&owner, owner.client.user_id()), Some(RosterRole::Owner));
    let mut other = self.peer(name).await;
    settle(&mut [&mut owner, &mut other]).await;
    (owner, other)
  }

  /// `peer`'s app has just got this ID token from the provider.
  fn give_token(&self, peer: &Peer, roles: &[&str], issued_at: i64) {
    let claim = peer.client.identity_attestation_claim();
    self.server.set_provider_token(
      peer.client.user_id(),
      self
        .provider
        .token(&with_roles(&claim, roles, issued_at).with("sub", json!(format!("sub-of-{}", peer.name)))),
    );
  }
}

fn base_claims(claim: &str, issued_at: i64) -> Value {
  json!({
    "iss": ISSUER, "aud": CLIENT, "azp": CLIENT, "typ": "ID", "sub": "sub", "iat": issued_at,
    "tc_identity_key": claim,
  })
}

fn with_roles(claim: &str, roles: &[&str], issued_at: i64) -> Value {
  base_claims(claim, issued_at).with("resource_access", json!({ CLIENT: { "roles": roles } }))
}

trait With {
  fn with(self, name: &str, value: Value) -> Self;
}

impl With for Value {
  fn with(mut self, name: &str, value: Value) -> Self {
    self[name] = value;
    self
  }
}

fn role_of(peer: &Peer, user: UserId) -> Option<RosterRole> {
  peer
    .client
    .workspace()
    .unwrap()
    .roster
    .members
    .iter()
    .find(|member| member.user_id == user)
    .map(|member| member.role.clone())
}

/// Signs ID tokens with ES256, as the identity provider would.
struct Provider {
  key: SigningKey,
}

impl Provider {
  fn new() -> Self {
    Self {
      key: SigningKey::random(&mut rand_core::OsRng),
    }
  }

  fn token(&self, claims: &Value) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"ES256","kid":"k1"}"#);
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
    let input = format!("{header}.{payload}");
    let signature: p256::ecdsa::Signature = self.key.sign(input.as_bytes());
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()))
  }
}

struct Jwks(Arc<Provider>);

impl JwksSource for Jwks {
  fn fetch(&self) -> Pin<Box<dyn Future<Output = Option<Vec<u8>>> + '_>> {
    let point = self.0.key.verifying_key().to_encoded_point(false);
    let jwks = json!({ "keys": [{
      "kty": "EC", "crv": "P-256", "kid": "k1",
      "x": URL_SAFE_NO_PAD.encode(point.x().unwrap()),
      "y": URL_SAFE_NO_PAD.encode(point.y().unwrap()),
    }]});
    Box::pin(async move { Some(serde_json::to_vec(&jwks).unwrap()) })
  }
}

/// The provider is down.
struct NoKeys;

impl JwksSource for NoKeys {
  fn fetch(&self) -> Pin<Box<dyn Future<Output = Option<Vec<u8>>> + '_>> {
    Box::pin(async { None })
  }
}
