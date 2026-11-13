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
/// The provider the owners moved to.
const MOVED_ISSUER: &str = "https://new.example";
/// The provider's clock in these tests (seconds).
const T0: i64 = 1_000_000;
const WEEK: i64 = 7 * 24 * 60 * 60;

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
  async fn the_servers_word_can_not_strip_the_roster() {
    // It reports every other owner and every admin gone, on and on.
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    let mut dora = world.peer("Dora").await;
    world.give_token(&bob, &["admin"], T0);
    world.give_token(&dora, &["owner"], T0);
    bob.client.run_role_signing().await.unwrap();
    dora.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut dora]).await;
    alice.client.run_role_signing().await.unwrap();
    let (bob_id, dora_id) = (bob.client.user_id(), dora.client.user_id());
    assert_eq!(role_of(&alice, bob_id), Some(RosterRole::Admin));
    assert_eq!(role_of(&alice, dora_id), Some(RosterRole::Owner));

    world.server.set_withdrawn_roles(vec![
      WithdrawnRole {
        user_id: bob_id,
        reason: WithdrawnReason::RoleGone,
      },
      WithdrawnRole {
        user_id: dora_id,
        reason: WithdrawnReason::AccountGone,
      },
    ]);
    world.give_token(&alice, &["owner"], T0 + 100);
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();

    // Nobody is off yet, and the owner hears of it.
    assert_eq!(role_of(&alice, bob_id), Some(RosterRole::Admin));
    assert_eq!(role_of(&alice, dora_id), Some(RosterRole::Owner));
    assert!(matches!(
      alice.client.role_signing_record().unwrap().maybe_note,
      Some(RoleSigningProblem::Contradictory { .. })
    ));

    // Weeks of it: the admin goes, the owner never does.
    world.give_token(&alice, &["owner"], T0 + WEEK + 200);
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();
    assert_eq!(role_of(&alice, bob_id), None);
    world.give_token(&alice, &["owner"], T0 + 20 * WEEK);
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();
    assert_eq!(role_of(&alice, dora_id), Some(RosterRole::Owner));
  }

  #[tokio::test]
  async fn a_newer_token_beats_a_report_that_goes_on() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    world.give_token(&bob, &["admin"], T0);
    bob.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    let bob_id = bob.client.user_id();
    world.server.set_withdrawn_roles(vec![WithdrawnRole {
      user_id: bob_id,
      reason: WithdrawnReason::RoleGone,
    }]);
    world.give_token(&alice, &["owner"], T0 + 100);
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();

    // Bob's token, issued after the report began, shows he still holds the role.
    let token = world.provider.token(
      &with_roles(&bob.client.identity_attestation_claim(), &["admin"], T0 + 200)
        .with("sub", json!("sub-of-Bob")),
    );
    world.server.clear_role_evidence();
    world
      .server
      .forge_role_evidence(bob_id, token, bob.client.identity_public_key_bytes().to_vec());
    world.give_token(&alice, &["owner"], T0 + WEEK + 300);
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();

    assert_eq!(role_of(&alice, bob_id), Some(RosterRole::Admin));
  }

  #[tokio::test]
  async fn the_servers_word_puts_nobody_on() {
    let world = World::new();
    let (mut alice, bob) = world.owner_and_peer("Bob").await;
    world.server.set_withdrawn_roles(vec![WithdrawnRole {
      user_id: bob.client.user_id(),
      reason: WithdrawnReason::RoleGone,
    }]);
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();
    world.server.set_withdrawn_roles(Vec::new());
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();
    assert_eq!(role_of(&alice, bob.client.user_id()), None);
  }

  #[tokio::test]
  async fn a_token_of_a_member_naming_an_unnoted_owner_under_her_key_changes_nothing() {
    // Mallory's attribute says `Bob:MalloryKey`, and the server lists it under Bob. Bob is on the roster
    // from before tokens counted.
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    let mallory = world.peer("Mallory").await;
    let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
    settle(&mut [&mut alice, &mut bob]).await;
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner), (bob_id, RosterRole::Owner)])
      .await
      .unwrap();
    let claim = format!(
      "{}:{}",
      bob_id,
      mallory
        .client
        .identity_attestation_claim()
        .split(':')
        .nth(1)
        .unwrap()
    );
    let token = world
      .provider
      .token(&with_roles(&claim, &["admin"], T0 + 50).with("sub", json!("sub-of-Mallory")));
    world
      .server
      .forge_role_evidence(bob_id, token, mallory.client.identity_public_key_bytes().to_vec());
    let before = alice.client.workspace().unwrap().roster.members;
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();

    assert_eq!(alice.client.workspace().unwrap().roster.members, before);
    assert!(
      !alice
        .client
        .workspace()
        .unwrap()
        .provider_marks
        .contains_key(&bob_id)
        || alice.client.workspace().unwrap().provider_marks[&bob_id]
          .maybe_subject
          .is_none()
    );
  }
}

mod the_pin {
  use super::*;

  #[tokio::test]
  async fn an_owner_on_the_roster_pins_the_provider_also_without_the_mapper() {
    let world = World::new();
    let mut alice = world.peer("Alice").await;
    let alice_id = alice.client.user_id();
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner)])
      .await
      .unwrap();
    assert!(
      alice
        .client
        .workspace()
        .unwrap()
        .policy
        .maybe_role_signing
        .is_none()
    );

    let claim = alice.client.identity_attestation_claim();
    world
      .server
      .set_provider_token(alice_id, world.provider.token(&base_claims(&claim, T0)));
    alice.client.run_role_signing().await.unwrap();

    let signing = alice
      .client
      .workspace()
      .unwrap()
      .policy
      .maybe_role_signing
      .unwrap();
    assert_eq!(signing.maybe_provider.unwrap().issuer, ISSUER);
  }

  #[tokio::test]
  async fn a_device_does_not_take_another_issuer_than_it_used_before_for_the_first_pin() {
    // No roster yet to pin under; the server now announces another issuer, which verifies against its
    // own keys, and the owner signs in on its page.
    let world = World::new();
    let mut alice = world.peer("Alice").await;
    let claim = alice.client.identity_attestation_claim();
    let alice_id = alice.client.user_id();
    world
      .server
      .set_provider_token(alice_id, world.provider.token(&base_claims(&claim, T0)));
    alice.client.run_role_signing().await.unwrap();
    let impostor = Arc::new(Provider::new());
    alice.client.set_jwks_source(Box::new(Jwks(impostor.clone())));
    world.server.set_provider_token(
      alice_id,
      impostor.token(&with_roles(&claim, &["owner"], T0 + 5).with("iss", json!("https://evil.example"))),
    );

    alice.client.run_role_signing().await.unwrap();

    assert_eq!(alice.client.workspace().unwrap().roster.version, 0);
    assert!(matches!(
      alice.client.role_signing_record().unwrap().maybe_note,
      Some(RoleSigningProblem::ProviderChanged { .. })
    ));
  }

  #[tokio::test]
  async fn a_move_an_owner_signed_takes_the_new_provider() {
    let world = World::new();
    let (mut alice, _bob) = world.owner_and_peer("Bob").await;
    let mut policy = alice.client.workspace().unwrap().policy;
    let mut signing = policy.maybe_role_signing.take().unwrap();
    signing.maybe_provider = None;
    signing.accepts_new_provider = true;
    policy.maybe_role_signing = Some(signing);
    alice.client.publish_policy(policy).await.unwrap();

    let moved = Arc::new(Provider::new());
    alice.client.set_jwks_source(Box::new(Jwks(moved.clone())));
    let claim = alice.client.identity_attestation_claim();
    world.server.set_provider_token(
      alice.client.user_id(),
      moved.token(&with_roles(&claim, &["owner"], T0 + 5).with("iss", json!("https://new.example"))),
    );
    alice.client.run_role_signing().await.unwrap();

    let signing = alice
      .client
      .workspace()
      .unwrap()
      .policy
      .maybe_role_signing
      .unwrap();
    assert_eq!(signing.maybe_provider.unwrap().issuer, "https://new.example");
    assert!(!signing.accepts_new_provider);
  }
}

mod provider_move {
  use teamcraft_core::api::ServerApi;
  use teamcraft_core::workspace::ChainedPolicyDocument;
  use teamcraft_proto::ListWorkspaceLogQuery;

  use super::*;

  #[tokio::test]
  async fn the_accounts_noted_before_give_way_to_the_new_providers_also_after_an_edit_by_hand() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
    note_admin(&world, &mut alice, &mut bob).await;
    // By hand, Bob becomes an owner too: his mark keeps the old account.
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner), (bob_id, RosterRole::Owner)])
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    assert_eq!(subject_of(&alice, bob_id).as_deref(), Some("sub-of-Bob"));

    let moved = move_provider(&world, &mut alice, &mut bob).await;
    assert_eq!(alice.client.workspace().unwrap().roster.provider_moves, 1);
    assert_eq!(subject_of(&alice, bob_id), None, "the old account is forgotten");
    world.give_moved_token(&moved, &bob, &["owner"], T0 + 11);
    bob.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;

    assert_eq!(role_of(&alice, bob_id), Some(RosterRole::Owner));
    assert_eq!(subject_of(&alice, bob_id).as_deref(), Some("new-sub-of-Bob"));
    assert_eq!(subject_of(&alice, alice_id).as_deref(), Some("new-sub-of-Alice"));
    let state = alice.client.workspace().unwrap();
    assert_eq!(
      state.provider_marks[&bob_id].maybe_by_hand, None,
      "the new provider agreed"
    );
    assert!(!matches!(
      alice.client.role_signing_record().unwrap().maybe_note,
      Some(RoleSigningProblem::Contradictory { .. })
    ));
    assert_eq!(bob.client.workspace().unwrap().provider_moves, 1);
  }

  #[tokio::test]
  async fn a_server_can_not_forget_anybodys_account_without_the_owners() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    let bob_id = bob.client.user_id();
    note_admin(&world, &mut alice, &mut bob).await;
    let state = bob.client.workspace().unwrap();

    // An unsigned policy that counts a move, and an unsigned roster that forgets the accounts.
    let mut policy = state.policy.clone();
    let mut signing = policy.maybe_role_signing.take().unwrap();
    signing.provider_moves = 7;
    policy.maybe_role_signing = Some(signing);
    let forged_policy = ChainedPolicyDocument {
      version: state.policy_version + 1,
      policy,
      roster_version: state.roster.version,
      maybe_prev_hash: state.heads.maybe_policy_hash.clone(),
      maybe_signed_at_ms: None,
    };
    world.server.inject_workspace_entry(
      WorkspaceLogKind::Policy,
      serde_json::to_vec(&forged_policy).unwrap(),
      vec![1; 64],
    );
    let mut forged_roster = state.roster.clone();
    forged_roster.version += 1;
    forged_roster.maybe_prev_hash = state.heads.maybe_roster_hash.clone();
    forged_roster.provider_moves = 7;
    for mark in forged_roster.provider_marks.values_mut() {
      mark.maybe_subject = None;
    }
    world.server.inject_workspace_entry(
      WorkspaceLogKind::Roster,
      serde_json::to_vec(&forged_roster).unwrap(),
      vec![1; 64],
    );
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.note_role_evidence_update();
    // The chain stops at the forged entries; what the round says about that doesn't matter here.
    let _ = alice.client.run_role_signing().await;

    for peer in [&alice, &bob] {
      let state = peer.client.workspace().unwrap();
      assert_eq!(state.provider_moves, 0, "{}", peer.name);
      assert_eq!(
        subject_of(peer, bob_id).as_deref(),
        Some("sub-of-Bob"),
        "{}",
        peer.name
      );
    }
  }

  #[tokio::test]
  async fn a_replayed_move_forgets_nothing_again() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    let bob_id = bob.client.user_id();
    note_admin(&world, &mut alice, &mut bob).await;
    let moved = move_provider(&world, &mut alice, &mut bob).await;
    world.give_moved_token(&moved, &bob, &["admin"], T0 + 11);
    bob.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    assert_eq!(subject_of(&alice, bob_id).as_deref(), Some("new-sub-of-Bob"));

    // The server appends the owner's signed move once more.
    let log = alice
      .api
      .list_workspace_log(&WorkspaceLogKind::Policy, &ListWorkspaceLogQuery::default())
      .await
      .unwrap();
    let signed_move = log
      .entries
      .iter()
      .find(|entry| {
        let document: ChainedPolicyDocument = serde_json::from_slice(entry.payload.as_slice()).unwrap();
        document
          .policy
          .maybe_role_signing
          .is_some_and(|signing| signing.accepts_new_provider)
      })
      .unwrap()
      .clone();
    world.server.inject_workspace_entry(
      WorkspaceLogKind::Policy,
      signed_move.payload.as_slice().to_vec(),
      signed_move.signature.as_slice().to_vec(),
    );
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.note_role_evidence_update();
    let _ = alice.client.run_role_signing().await;

    let state = alice.client.workspace().unwrap();
    assert_eq!(state.provider_moves, 1);
    let signing = state.policy.maybe_role_signing.unwrap();
    assert_eq!(signing.maybe_provider.unwrap().issuer, MOVED_ISSUER);
    assert_eq!(subject_of(&alice, bob_id).as_deref(), Some("new-sub-of-Bob"));
  }

  #[tokio::test]
  async fn trusting_the_same_provider_again_forgets_nothing() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    let bob_id = bob.client.user_id();
    note_admin(&world, &mut alice, &mut bob).await;
    trust_the_next_provider(&mut alice).await;
    world.give_token(&alice, &["owner"], T0 + 10);
    alice.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;

    let state = alice.client.workspace().unwrap();
    let signing = state.policy.maybe_role_signing.unwrap();
    assert_eq!(signing.maybe_provider.unwrap().issuer, ISSUER);
    assert!(!signing.accepts_new_provider);
    assert_eq!(signing.provider_moves, 0);
    assert_eq!(state.provider_moves, 0);
    assert_eq!(subject_of(&alice, bob_id).as_deref(), Some("sub-of-Bob"));
  }

  /// Bob holds the admin role at the first provider, and Alice's app noted his account there.
  async fn note_admin(world: &World, alice: &mut Peer, bob: &mut Peer) {
    world.give_token(bob, &["admin"], T0);
    bob.client.run_role_signing().await.unwrap();
    settle(&mut [&mut *alice, &mut *bob]).await;
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();
    settle(&mut [&mut *alice, &mut *bob]).await;
    assert_eq!(
      subject_of(alice, bob.client.user_id()).as_deref(),
      Some("sub-of-Bob")
    );
  }

  /// Alice signs the move to another provider and signs in there; her app pins it.
  async fn move_provider(world: &World, alice: &mut Peer, bob: &mut Peer) -> Arc<Provider> {
    trust_the_next_provider(alice).await;
    let moved = Arc::new(Provider::new());
    alice.client.set_jwks_source(Box::new(Jwks(moved.clone())));
    bob.client.set_jwks_source(Box::new(Jwks(moved.clone())));
    world.give_moved_token(&moved, alice, &["owner"], T0 + 10);
    alice.client.run_role_signing().await.unwrap();
    settle(&mut [&mut *alice, &mut *bob]).await;
    moved
  }

  /// Alice signs that the provider she signs in with next counts ("Trust the provider I sign in with next").
  async fn trust_the_next_provider(alice: &mut Peer) {
    let mut policy = alice.client.workspace().unwrap().policy;
    let mut signing = policy.maybe_role_signing.take().unwrap();
    signing.maybe_provider = None;
    signing.accepts_new_provider = true;
    policy.maybe_role_signing = Some(signing);
    alice.client.publish_policy(policy).await.unwrap();
  }

  fn subject_of(peer: &Peer, user: UserId) -> Option<String> {
    let state = peer.client.workspace().unwrap();
    state.provider_marks.get(&user)?.maybe_subject.clone()
  }
}

mod by_hand {
  use super::*;

  #[tokio::test]
  async fn a_removal_by_hand_is_not_undone_by_a_token_stored_before_it() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    world.give_token(&bob, &["admin"], T0);
    bob.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
    // A newer token with the role reaches the server, and only then the owner removes him.
    world.server.forge_role_evidence(
      bob_id,
      world.provider.token(
        &with_roles(&bob.client.identity_attestation_claim(), &["admin"], T0 + 9)
          .with("sub", json!("sub-of-Bob")),
      ),
      bob.client.identity_public_key_bytes().to_vec(),
    );
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner)])
      .await
      .unwrap();

    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();

    assert_eq!(role_of(&alice, bob_id), None);
  }

  #[tokio::test]
  async fn an_older_token_can_not_settle_an_edit_by_hand() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    world.give_token(&bob, &["admin"], T0);
    bob.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
    world.give_token(&alice, &["owner"], T0 + 100);
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner)])
      .await
      .unwrap();
    assert_eq!(
      alice.client.workspace().unwrap().provider_marks[&bob_id].issued_at,
      T0 + 100,
      "the floor is the edit"
    );

    // A token from before the edit, without the role, would settle it, and a later one bring him back.
    let claim = bob.client.identity_attestation_claim();
    let key = bob.client.identity_public_key_bytes().to_vec();
    world.server.forge_role_evidence(
      bob_id,
      world
        .provider
        .token(&with_roles(&claim, &["member"], T0 + 50).with("sub", json!("sub-of-Bob"))),
      key.clone(),
    );
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();
    world.server.clear_role_evidence();
    world.server.forge_role_evidence(
      bob_id,
      world
        .provider
        .token(&with_roles(&claim, &["admin"], T0 + 200).with("sub", json!("sub-of-Bob"))),
      key,
    );
    world.give_token(&alice, &["owner"], T0 + 200);
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();

    assert_eq!(role_of(&alice, bob_id), None);
  }

  #[tokio::test]
  async fn somebody_added_by_hand_without_the_provider_role_stays() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
    settle(&mut [&mut alice, &mut bob]).await;
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner), (bob_id, RosterRole::Admin)])
      .await
      .unwrap();
    world.server.set_withdrawn_roles(vec![WithdrawnRole {
      user_id: bob_id,
      reason: WithdrawnReason::RoleGone,
    }]);
    world.give_token(&bob, &["member"], T0 + 10);
    settle(&mut [&mut alice, &mut bob]).await;
    bob.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    world.give_token(&alice, &["owner"], T0 + 5 * WEEK);
    alice.client.note_role_evidence_update();
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
  async fn the_last_owner_demoted_at_the_provider_stays_and_hears_about_it() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    world.give_token(&alice, &["admin"], T0 + 10);
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.run_role_signing().await.unwrap();
    assert_eq!(role_of(&alice, alice.client.user_id()), Some(RosterRole::Owner));
    assert!(matches!(
      alice.client.role_signing_record().unwrap().maybe_note,
      Some(RoleSigningProblem::Contradictory { .. })
    ));
  }

  #[tokio::test]
  async fn a_handover_at_the_provider_goes_through_the_old_owners_app() {
    // Bob's owner role goes to Carol, who isn't a verified owner: only Bob's app can sign her.
    let world = World::new();
    let (mut bob, mut carol) = world.owner_and_peer("Carol").await;
    let (bob_id, carol_id) = (bob.client.user_id(), carol.client.user_id());
    world.give_token(&carol, &["owner"], T0 + 10);
    carol.client.run_role_signing().await.unwrap();
    assert_eq!(
      carol.client.role_signing_record().unwrap().maybe_note,
      Some(RoleSigningProblem::AwaitingOwner),
      "she hears why nothing happened yet"
    );
    settle(&mut [&mut bob, &mut carol]).await;
    world.give_token(&bob, &["member"], T0 + 20);
    bob.client.note_role_evidence_update();
    bob.client.run_role_signing().await.unwrap();
    settle(&mut [&mut bob, &mut carol]).await;

    assert_eq!(role_of(&carol, carol_id), Some(RosterRole::Owner));
    assert_eq!(role_of(&carol, bob_id), None);

    // Carol's app signs the policy under the new roster, which Bob couldn't any more, on the roster's
    // own event, with nothing else to wake it.
    carol.client.run_role_signing().await.unwrap();
    let workspace = carol.client.workspace().unwrap();
    assert_eq!(workspace.policy_roster_version, workspace.roster.version);
    assert_eq!(carol.client.role_signing_record().unwrap().maybe_note, None);
  }

  #[tokio::test]
  async fn somebody_with_no_role_left_is_taken_off_by_what_the_server_saw() {
    // Keycloak's mapper adds no claim then, and the server refuses the person's tokens: their app can
    // publish nothing, however honest. The server kept the access token it refused.
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    world.give_token(&bob, &["owner"], T0);
    bob.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.run_role_signing().await.unwrap();
    let bob_id = bob.client.user_id();
    assert_eq!(role_of(&alice, bob_id), Some(RosterRole::Owner));

    world.server.add_role_loss(
      bob_id,
      world.provider.token(&no_role_claims("sub-of-Bob", T0 + 50)),
    );
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();

    assert_eq!(role_of(&alice, bob_id), None);
  }

  #[tokio::test]
  async fn the_server_can_not_demote_an_owner_with_no_noted_account_with_somebody_elses_token() {
    // An owner from before tokens counted. The server lists a provider admin's expired access token
    // under them.
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
    settle(&mut [&mut alice, &mut bob]).await;
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner), (bob_id, RosterRole::Owner)])
      .await
      .unwrap();
    // The hand edit is what marks him here; a roster from before has no mark at all.
    world.server.add_role_loss(
      bob_id,
      world
        .provider
        .token(&access_claims("sub-of-Mallory", &["admin"], T0 + 50)),
    );
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();

    assert_eq!(role_of(&alice, bob_id), Some(RosterRole::Owner));
    let workspace = alice.client.workspace().unwrap();
    assert!(
      workspace
        .provider_marks
        .get(&bob_id)
        .is_none_or(|mark| mark.maybe_subject.is_none()),
      "nothing is noted for him"
    );
  }

  #[tokio::test]
  async fn what_the_server_saw_counts_only_for_the_noted_account_and_when_newer() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    world.give_token(&bob, &["admin"], T0 + 20);
    bob.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.run_role_signing().await.unwrap();
    let bob_id = bob.client.user_id();

    for token in [
      world.provider.token(&no_role_claims("sub-of-Mallory", T0 + 50)),
      world.provider.token(&no_role_claims("sub-of-Bob", T0 + 10)),
      Provider::new().token(&no_role_claims("sub-of-Bob", T0 + 50)),
      // A role that isn't lower.
      world
        .provider
        .token(&access_claims("sub-of-Bob", &["admin"], T0 + 50)),
    ] {
      world.server.add_role_loss(bob_id, token);
    }
    alice.client.note_role_evidence_update();
    alice.client.run_role_signing().await.unwrap();

    assert_eq!(role_of(&alice, bob_id), Some(RosterRole::Admin));
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
  async fn somebody_with_no_role_left_hears_no_false_note_and_shows_the_server_their_token() {
    // Keycloak's mapper adds no claim once the last client role is gone. The access token shows it.
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    world.give_token(&bob, &["admin"], T0);
    bob.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    let bob_id = bob.client.user_id();
    let claim = bob.client.identity_attestation_claim();
    world
      .server
      .set_provider_token(bob_id, world.provider.token(&base_claims(&claim, T0 + 50)));
    world.server.set_provider_access_token(
      bob_id,
      world.provider.token(&no_role_claims("sub-of-Bob", T0 + 50)),
    );
    let calls = world.server.rest_call_count(bob_id);

    bob.client.run_role_signing().await.unwrap();

    assert_eq!(bob.client.role_signing_record().unwrap().maybe_note, None);
    assert!(
      world.server.rest_call_count(bob_id) > calls,
      "an idle app would send nothing"
    );
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

mod roles_claim {
  //! Roles from a claim the signed policy names, such as Dex's `groups` (issue #1742).
  use super::*;

  /// An ID token naming `groups`, and client roles that say `owner` to show they aren't read.
  fn give_groups(world: &World, peer: &Peer, groups: &[&str], issued_at: i64) {
    let claim = peer.client.identity_attestation_claim();
    let claims = with_roles(&claim, &["owner"], issued_at)
      .with("sub", json!(format!("sub-of-{}", peer.name)))
      .with("groups", json!(groups));
    world
      .server
      .set_provider_token(peer.client.user_id(), world.provider.token(&claims));
  }

  #[tokio::test]
  async fn a_signed_roles_claim_makes_a_plain_providers_groups_count() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    let mut policy = alice.client.workspace().unwrap().policy;
    policy.maybe_role_signing = Some(Box::new(RoleSigningPolicy {
      roles_claim: vec!["groups".into()],
      owner_role: "teamcraft-owner".into(),
      admin_role: "teamcraft-admin".into(),
      ..*policy.maybe_role_signing.unwrap()
    }));
    alice.client.publish_policy(policy).await.unwrap();
    give_groups(&world, &alice, &["teamcraft-owner"], T0);
    give_groups(&world, &bob, &["teamcraft-admin", "staff"], T0);

    bob.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.run_role_signing().await.unwrap();

    assert_eq!(
      role_of(&alice, bob.client.user_id()),
      Some(RosterRole::Admin),
      "the groups, not the client roles"
    );
    assert_eq!(role_of(&alice, alice.client.user_id()), Some(RosterRole::Owner));
  }
}

mod manual {
  use super::*;

  #[tokio::test]
  async fn a_signed_policy_can_require_an_owner_to_sign_by_hand() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    let mut policy = alice.client.workspace().unwrap().policy;
    policy.maybe_role_signing = Some(Box::new(RoleSigningPolicy {
      mode: RoleSigningMode::Manual,
      ..*policy.maybe_role_signing.unwrap()
    }));
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
  async fn an_edit_of_role_signing_from_a_draft_the_server_stripped_is_refused() {
    let world = World::new();
    let (mut alice, _bob) = world.owner_and_peer("Bob").await;
    let verified = alice.client.workspace().unwrap().policy;
    let stripped = WorkspacePolicy {
      maybe_role_signing: None,
      ..verified.clone()
    };
    let edited = WorkspacePolicy {
      maybe_role_signing: Some(Box::new(RoleSigningPolicy {
        owner_role: "chief".into(),
        ..RoleSigningPolicy::default()
      })),
      ..verified.clone()
    };
    assert!(matches!(
      alice.client.publish_policy_based_on(&stripped, edited).await,
      Err(teamcraft_core::CoreError::PolicyBaseMismatch)
    ));
    // Not touching it goes through and keeps what is verified.
    alice
      .client
      .publish_policy_based_on(&stripped, stripped.clone())
      .await
      .unwrap();
    assert_eq!(
      alice.client.workspace().unwrap().policy.maybe_role_signing,
      verified.maybe_role_signing
    );
  }

  #[tokio::test]
  async fn a_policy_that_does_not_name_role_signing_does_not_change_it() {
    // An older owner's app signs the policy without the field it doesn't know.
    let world = World::new();
    let (mut alice, _bob) = world.owner_and_peer("Bob").await;
    let mut policy = alice.client.workspace().unwrap().policy;
    policy.maybe_role_signing = Some(Box::new(RoleSigningPolicy {
      mode: RoleSigningMode::Manual,
      ..*policy.maybe_role_signing.unwrap()
    }));
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

mod key_attestation {
  //! Identity keys attested by the provider the owners pinned (e2ee.md §3.1, issue #1279).
  use teamcraft_core::ClientSettings;
  use teamcraft_core::api::ServerApi as _;
  use teamcraft_core::records::{PinnedIdentity, pinned_identities};
  use teamcraft_proto::{ConversationId, Id as _, KeyAttestationMode};

  use super::*;

  #[tokio::test]
  async fn every_members_app_publishes_its_keys_attestation_once() {
    let world = World::new();
    let (_alice, mut bob) = world.owner_and_peer("Bob").await;
    world.give_token(&bob, &[], T0);

    bob.client.run_role_signing().await.unwrap();

    let token = world
      .server
      .identity_attestation_of(bob.client.user_id())
      .unwrap();
    let claims = teamcraft_crypto::attestation::unverified_claims(&token).unwrap();
    assert_eq!(
      claims.maybe_key_claim.unwrap(),
      bob.client.identity_attestation_claim()
    );
    let uploads = world.server.attestation_uploads();
    world.give_token(&bob, &[], T0 + 60);
    bob.client.run_role_signing().await.unwrap();
    assert_eq!(world.server.attestation_uploads(), uploads, "once per key");
  }

  #[tokio::test]
  async fn an_admins_attested_key_is_pinned_as_attested() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    make_admin(&world, &mut alice, &mut bob).await;

    add_to_new_channel(&mut alice, bob.client.user_id())
      .await
      .unwrap();

    assert!(pin_of(&alice, bob.client.user_id()).is_attested);
  }

  #[tokio::test]
  async fn a_members_attestation_confirms_nothing_without_a_signed_account() {
    // Their claim is their own word; nobody signed which account is theirs (e2ee.md §3.1).
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    world.give_token(&bob, &[], T0);
    bob.client.run_role_signing().await.unwrap();
    assert!(
      world
        .server
        .identity_attestation_of(bob.client.user_id())
        .is_some()
    );

    add_to_new_channel(&mut alice, bob.client.user_id())
      .await
      .unwrap();

    assert!(!pin_of(&alice, bob.client.user_id()).is_attested);
  }

  #[tokio::test]
  async fn another_account_naming_somebody_in_its_claim_is_refused() {
    // Mallory writes "<Bob's user ID>:<a key>" into her own profile attribute and gets a genuine token
    // for it; a server hands it out as Bob's attestation.
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    make_admin(&world, &mut alice, &mut bob).await;
    let mallorys = world
      .provider
      .token(&base_claims(&bob.client.identity_attestation_claim(), T0).with("sub", json!("sub-of-Mallory")));
    world
      .server
      .set_identity_attestation(bob.client.user_id(), Some(mallorys));

    add_to_new_channel(&mut alice, bob.client.user_id())
      .await
      .unwrap();
    assert!(!pin_of(&alice, bob.client.user_id()).is_attested);

    // Where it is required, a colleague who never pinned Bob doesn't pin it either; the key the owners
    // signed in the roster is the one that counts for an admin.
    let mut erin = world.peer("Erin").await;
    require_attestation(&mut alice).await;
    settle(&mut [&mut alice, &mut erin]).await;
    add_to_new_channel(&mut erin, bob.client.user_id()).await.unwrap();
    assert!(
      !pinned_identities(&*erin.store)
        .unwrap()
        .contains_key(&bob.client.user_id())
    );
  }

  #[tokio::test]
  async fn a_forged_or_missing_attestation_is_trust_on_first_use_when_optional() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    let mut carol = world.peer("Carol").await;
    make_admin(&world, &mut alice, &mut bob).await;
    make_admin(&world, &mut alice, &mut carol).await;
    world
      .server
      .set_identity_attestation(carol.client.user_id(), None);
    let impostor = Provider::new();
    let forged = impostor.token(&base_claims(&bob.client.identity_attestation_claim(), T0));
    world
      .server
      .set_identity_attestation(bob.client.user_id(), Some(forged));

    for user in [bob.client.user_id(), carol.client.user_id()] {
      add_to_new_channel(&mut alice, user).await.unwrap();
      assert!(!pin_of(&alice, user).is_attested);
    }
  }

  #[tokio::test]
  async fn an_attestation_of_another_persons_key_is_not_taken() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    let carol = world.peer("Carol").await;
    make_admin(&world, &mut alice, &mut bob).await;
    // Validly signed, but for Carol's key: the server hands it out as Bob's.
    let carols = world
      .provider
      .token(&base_claims(&carol.client.identity_attestation_claim(), T0));
    world
      .server
      .set_identity_attestation(bob.client.user_id(), Some(carols));

    add_to_new_channel(&mut alice, bob.client.user_id())
      .await
      .unwrap();

    assert!(!pin_of(&alice, bob.client.user_id()).is_attested);
  }

  #[tokio::test]
  async fn required_by_the_signed_policy_only_a_signed_accounts_key_needs_attestation() {
    let world = World::new();
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    let mut carol = world.peer("Carol").await;
    let dora = world.peer("Dora").await;
    make_admin(&world, &mut alice, &mut bob).await;
    make_admin(&world, &mut alice, &mut carol).await;
    world
      .server
      .set_identity_attestation(carol.client.user_id(), None);
    require_attestation(&mut alice).await;

    add_to_new_channel(&mut alice, bob.client.user_id())
      .await
      .unwrap();
    // Not pinned; the roster's key, which the owners signed, is used for her.
    add_to_new_channel(&mut alice, carol.client.user_id())
      .await
      .unwrap();
    // Somebody whose account nobody signed is pinned on first use, required or not.
    add_to_new_channel(&mut alice, dora.client.user_id())
      .await
      .unwrap();
    let pinned = pinned_identities(&*alice.store).unwrap();
    assert!(pinned[&bob.client.user_id()].is_attested);
    assert!(!pinned.contains_key(&carol.client.user_id()));
    assert!(!pinned[&dora.client.user_id()].is_attested);
  }

  #[tokio::test]
  async fn keys_pinned_before_stay_pinned_when_attestation_becomes_required() {
    // Existing instances: turning it on locks nobody out.
    let world = World::new();
    let (mut alice, bob) = world.owner_and_peer("Bob").await;
    add_to_new_channel(&mut alice, bob.client.user_id())
      .await
      .unwrap();
    assert!(!pin_of(&alice, bob.client.user_id()).is_attested);

    require_attestation(&mut alice).await;

    add_to_new_channel(&mut alice, bob.client.user_id())
      .await
      .unwrap();
    assert!(!pin_of(&alice, bob.client.user_id()).is_attested);
  }

  #[tokio::test]
  async fn a_key_pinned_on_first_use_counts_as_attested_once_its_owner_attests_it() {
    let world = World::new();
    let mut alice = world
      .peer_with(
        "Alice",
        ClientSettings {
          attestation_recheck: std::time::Duration::ZERO,
          ..crate::common::settings()
        },
      )
      .await;
    world.give_token(&alice, &["owner"], T0);
    alice.client.run_role_signing().await.unwrap();
    let mut bob = world.peer("Bob").await;
    settle(&mut [&mut alice, &mut bob]).await;
    add_to_new_channel(&mut alice, bob.client.user_id())
      .await
      .unwrap();
    assert!(!pin_of(&alice, bob.client.user_id()).is_attested);

    make_admin(&world, &mut alice, &mut bob).await;
    add_to_new_channel(&mut alice, bob.client.user_id())
      .await
      .unwrap();

    let pin = pin_of(&alice, bob.client.user_id());
    assert!(pin.is_attested);
    assert_eq!(pin.public_key, bob.client.identity_public_key_bytes());
  }

  #[tokio::test]
  async fn an_attested_new_key_is_a_key_change_not_a_silent_swap() {
    let world = World::new();
    let mut alice = world
      .peer_with(
        "Alice",
        ClientSettings {
          attestation_recheck: std::time::Duration::ZERO,
          ..crate::common::settings()
        },
      )
      .await;
    world.give_token(&alice, &["owner"], T0);
    alice.client.run_role_signing().await.unwrap();
    let mut bob = world.peer("Bob").await;
    settle(&mut [&mut alice, &mut bob]).await;
    add_to_new_channel(&mut alice, bob.client.user_id())
      .await
      .unwrap();
    let pinned_key = pin_of(&alice, bob.client.user_id()).public_key;
    // The server swaps in another key, with a valid attestation of it.
    let other_key = teamcraft_crypto::identity::IdentityKeyPair::generate(&mut rand_core::OsRng).public_key();
    let claim = teamcraft_crypto::attestation::attestation_claim(
      teamcraft_crypto::ids::UserId::new(u64::try_from(bob.client.user_id().snowflake().get()).unwrap()),
      &other_key,
    );
    world
      .server
      .replace_identity_key(bob.client.user_id(), other_key.as_bytes().to_vec());
    world.server.set_identity_attestation(
      bob.client.user_id(),
      Some(world.provider.token(&base_claims(&claim, T0))),
    );

    let _ = add_to_new_channel(&mut alice, bob.client.user_id()).await;

    let pin = pin_of(&alice, bob.client.user_id());
    assert_eq!(pin.public_key, pinned_key);
    assert_eq!(pin.maybe_changed_to, Some(*other_key.as_bytes()));
    assert!(!pin.is_attested);
  }

  #[tokio::test]
  async fn a_server_without_the_endpoint_doesnt_stop_owners_from_signing() {
    let world = World::new();
    world.server.lack_attestation_endpoint(true);
    let (mut alice, mut bob) = world.owner_and_peer("Bob").await;
    world.give_token(&bob, &["admin"], T0);
    bob.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    world.give_token(&alice, &["owner"], T0 + 60);
    alice.client.note_role_evidence_update();

    alice.client.run_role_signing().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;

    assert_eq!(role_of(&alice, bob.client.user_id()), Some(RosterRole::Admin));
    assert_eq!(world.server.attestation_uploads(), 0);
  }

  #[tokio::test]
  async fn an_attestation_the_server_lost_is_published_again_after_a_restart() {
    let world = World::new();
    let (_alice, mut bob) = world.owner_and_peer("Bob").await;
    world.give_token(&bob, &[], T0);
    bob.client.run_role_signing().await.unwrap();
    let uploads = world.server.attestation_uploads();
    // A database restored from before it.
    world.server.set_identity_attestation(bob.client.user_id(), None);

    restart(&world, &mut bob);
    world.give_token(&bob, &[], T0 + 60);
    bob.client.run_role_signing().await.unwrap();

    assert_eq!(world.server.attestation_uploads(), uploads + 1);
    assert!(
      world
        .server
        .identity_attestation_of(bob.client.user_id())
        .is_some()
    );
  }

  /// The provider makes `peer` an admin; their app publishes its token and attestation, the owner's app
  /// signs them with their account.
  async fn make_admin(world: &World, owner: &mut Peer, peer: &mut Peer) {
    world.give_token(peer, &["admin"], T0);
    peer.client.run_role_signing().await.unwrap();
    settle(&mut [&mut *owner, &mut *peer]).await;
    world.give_token(owner, &["owner"], T0 + 60);
    owner.client.note_role_evidence_update();
    owner.client.run_role_signing().await.unwrap();
    settle(&mut [&mut *owner, &mut *peer]).await;
    assert_eq!(role_of(owner, peer.client.user_id()), Some(RosterRole::Admin));
  }

  async fn require_attestation(owner: &mut Peer) {
    let mut policy = owner.client.workspace().unwrap().policy;
    policy.maybe_role_signing = Some(Box::new(RoleSigningPolicy {
      maybe_key_attestation: Some(KeyAttestationMode::Required),
      ..*policy.maybe_role_signing.unwrap()
    }));
    owner.client.publish_policy(policy).await.unwrap();
  }

  async fn add_to_new_channel(
    peer: &mut Peer,
    user: UserId,
  ) -> Result<ConversationId, teamcraft_core::CoreError> {
    let space = peer.api.list_spaces().await.unwrap().spaces[0].id;
    let channel = peer.client.create_channel(space, "attested").await?;
    peer.client.add_member(channel, user).await?;
    Ok(channel)
  }

  /// The app is started again on the same store.
  fn restart(world: &World, peer: &mut Peer) {
    peer.client = teamcraft_core::Client::open(
      peer.api.clone(),
      peer.store.clone(),
      crate::common::clock(),
      crate::common::settings(),
    )
    .unwrap()
    .unwrap();
    peer
      .client
      .set_jwks_source(Box::new(Jwks(world.provider.clone())));
  }

  fn pin_of(peer: &Peer, user: UserId) -> PinnedIdentity {
    pinned_identities(&*peer.store).unwrap().remove(&user).unwrap()
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
    self.peer_with(name, crate::common::settings()).await
  }

  async fn peer_with(&self, name: &str, settings: teamcraft_core::ClientSettings) -> Peer {
    let mut peer = Peer::enroll_with_settings(&self.server, name, settings).await;
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

  /// `peer`'s app has just got this ID token from `moved`, the provider the owners moved to.
  fn give_moved_token(&self, moved: &Provider, peer: &Peer, roles: &[&str], issued_at: i64) {
    let claim = peer.client.identity_attestation_claim();
    let claims = with_roles(&claim, roles, issued_at)
      .with("iss", json!(MOVED_ISSUER))
      .with("sub", json!(format!("new-sub-of-{}", peer.name)));
    self
      .server
      .set_provider_token(peer.client.user_id(), moved.token(&claims));
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

/// As Keycloak signs it for somebody with no client role left: `account`'s roles, none of this client.
fn no_role_claims(subject: &str, issued_at: i64) -> Value {
  json!({
    "iss": ISSUER, "aud": "account", "azp": CLIENT, "typ": "Bearer", "sub": subject, "iat": issued_at,
    "resource_access": { "account": { "roles": ["manage-account"] } },
  })
}

fn access_claims(subject: &str, roles: &[&str], issued_at: i64) -> Value {
  json!({
    "iss": ISSUER, "aud": "account", "azp": CLIENT, "typ": "Bearer", "sub": subject, "iat": issued_at,
    "resource_access": { CLIENT: { "roles": roles }, "account": { "roles": ["manage-account"] } },
  })
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
