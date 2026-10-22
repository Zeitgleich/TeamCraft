//! Signed workspace roster and policy, admin actions on groups the admin isn't in, and removals the
//! server asks for, through the fake server.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

mod common;

use common::{Peer, settle};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::workspace::RosterRole;
use teamcraft_proto::{AdminSelfJoin, HistoryPolicy, PolicyDocument, WorkspaceLogKind, WorkspacePolicy};

#[tokio::test]
async fn roster_and_policy_are_verified_and_forgeries_ignored() {
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
  alice
    .client
    .publish_policy(policy(AdminSelfJoin::Disabled))
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;

  let workspace = carol.client.workspace().unwrap();
  assert_eq!(
    workspace.maybe_root.as_ref().map(|root| root.user_id),
    Some(alice_id)
  );
  assert!(workspace.is_admin(bob_id));
  assert_eq!(workspace.policy.admin_self_join, AdminSelfJoin::Disabled);

  // The server appends a policy nobody signed: clients keep the verified one.
  let forged = PolicyDocument {
    version: 2,
    policy: policy(AdminSelfJoin::Visible),
  };
  server.inject_workspace_entry(
    WorkspaceLogKind::Policy,
    serde_json::to_vec(&forged).unwrap(),
    vec![1, 2, 3],
  );
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  let workspace = carol.client.workspace().unwrap();
  assert_eq!(workspace.policy.admin_self_join, AdminSelfJoin::Disabled);
  assert!(workspace.maybe_rejection.is_some());
}

#[tokio::test]
async fn a_policy_edited_from_something_unverified_is_not_signed() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let alice_id = alice.client.user_id();
  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner)])
    .await
    .unwrap();
  alice
    .client
    .publish_policy(policy(AdminSelfJoin::Disabled))
    .await
    .unwrap();
  let verified = alice.client.workspace().unwrap().policy;

  // The console showed a policy the server altered (self-join visible); the owner edits something else.
  let mut shown = verified.clone();
  shown.admin_self_join = AdminSelfJoin::Visible;
  let mut edited = shown.clone();
  edited.history_default = HistoryPolicy::FromJoin;
  let refused = alice.client.publish_policy_based_on(&shown, edited).await;
  assert!(matches!(
    refused,
    Err(teamcraft_core::CoreError::PolicyBaseMismatch)
  ));
  assert_eq!(
    alice.client.workspace().unwrap().policy_version,
    1,
    "nothing was signed"
  );

  // Edited from what the core verified: signed.
  let mut edited = verified.clone();
  edited.history_default = HistoryPolicy::FromJoin;
  alice
    .client
    .publish_policy_based_on(&verified, edited)
    .await
    .unwrap();
  assert_eq!(alice.client.workspace().unwrap().policy_version, 2);
}

#[tokio::test]
async fn admins_cannot_sign_policy() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner), (bob_id, RosterRole::Admin)])
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  // The fake server accepts it (the real one checks the role, but must not be trusted to).
  bob
    .client
    .publish_policy(policy(AdminSelfJoin::Visible))
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  let workspace = alice.client.workspace().unwrap();
  assert_eq!(workspace.policy_version, 0);
  assert!(
    workspace
      .maybe_rejection
      .unwrap()
      .contains("not validly signed by an owner")
  );
}

#[tokio::test]
async fn admin_adds_people_to_a_channel_they_are_not_in() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let mut dave = Peer::enroll(&server, "Dave").await;
  let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner), (bob_id, RosterRole::Admin)])
    .await
    .unwrap();
  server.set_admin(bob_id);
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

  let channel = carol
    .client
    .create_channel(carol.space().await, "carols-channel")
    .await
    .unwrap();
  bob
    .client
    .admin_add_member(channel, dave.client.user_id())
    .await
    .unwrap();
  // Carol's client sees Bob's signed change, checks him against the roster and commits it.
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
  assert!(dave.client.has_group(channel).unwrap());

  carol
    .client
    .send_text(channel, None, "welcome Dave")
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
  assert_eq!(dave.texts(channel), vec!["welcome Dave"]);
  assert!(!bob.client.has_group(channel).unwrap());
}

#[tokio::test]
async fn admin_self_join_follows_the_signed_policy() {
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
  alice
    .client
    .publish_policy(policy(AdminSelfJoin::Disabled))
    .await
    .unwrap();
  server.set_admin(bob_id);
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  let channel = carol
    .client
    .create_channel(carol.space().await, "private")
    .await
    .unwrap();

  bob.client.admin_add_member(channel, bob_id).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  assert!(!bob.client.has_group(channel).unwrap(), "self-join is disabled");

  alice
    .client
    .publish_policy(policy(AdminSelfJoin::Visible))
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  bob.client.admin_add_member(channel, bob_id).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  assert!(bob.client.has_group(channel).unwrap(), "self-join is visible");
}

#[tokio::test]
async fn a_lost_device_is_revoked_by_its_owner_and_keys_rotate() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "devices")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  alice.client.upload_key_bag().await.unwrap();
  let mut laptop = Peer::enroll_as(alice.api.clone(), "Alice laptop").await;
  alice.client.add_missing_devices(channel).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut laptop]).await;
  assert!(laptop.client.has_group(channel).unwrap());
  let listed_with_laptop = server.device_list_of(alice.client.user_id()).unwrap();

  // The laptop is lost: Alice's phone revokes it, which signs its removal from the group and a
  // device list without it (security review A4, A8).
  alice
    .client
    .revoke_device(laptop.client.device_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut laptop]).await;
  bob
    .client
    .send_text(channel, None, "after the laptop was lost")
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut laptop]).await;

  assert_eq!(alice.texts(channel), vec!["after the laptop was lost"]);
  assert!(laptop.texts(channel).is_empty());
  assert!(!laptop.client.has_group(channel).unwrap());
  let newest = server.device_list_of(alice.client.user_id()).unwrap();
  assert!(newest.sequence > listed_with_laptop.sequence);

  // The server goes back to the list that still had the laptop: Bob's client refuses it, and
  // adding devices goes on with the newest list it saw.
  server.serve_device_list(alice.client.user_id(), listed_with_laptop);
  assert_eq!(bob.client.add_missing_devices(channel).await.unwrap(), 0);
}

#[tokio::test]
async fn a_removal_the_server_reports_is_not_signed_by_members() {
  // Security review A8: the server can't make an honest client sign "Alice removed Carol".
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let channel = three_member_channel(&mut alice, &mut bob, &mut carol).await;
  let commits_before = commits(&alice, channel).await;

  server.remove_member_as_admin(channel, carol.client.user_id());
  settle(&mut [&mut alice, &mut bob]).await;
  alice.client.apply_pending_changes().await.unwrap();
  bob.client.apply_pending_changes().await.unwrap();

  assert_eq!(commits(&alice, channel).await, commits_before);
}

#[tokio::test]
async fn an_admin_removes_someone_with_a_signed_change() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let mut admin = Peer::enroll(&server, "Admin").await;
  let (alice_id, admin_id) = (alice.client.user_id(), admin.client.user_id());
  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner), (admin_id, RosterRole::Admin)])
    .await
    .unwrap();
  server.set_admin(admin_id);
  let channel = three_member_channel(&mut alice, &mut bob, &mut carol).await;
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut admin]).await;

  // The admin isn't in the channel; a member's client commits the admin's signed removal.
  admin
    .client
    .admin_remove_member(channel, carol.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  bob.client.apply_pending_changes().await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  alice
    .client
    .send_text(channel, None, "after Carol left")
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;

  assert_eq!(bob.texts(channel), vec!["after Carol left"]);
  assert!(!carol.client.has_group(channel).unwrap());
}

async fn three_member_channel(
  alice: &mut Peer,
  bob: &mut Peer,
  carol: &mut Peer,
) -> teamcraft_proto::ConversationId {
  let channel = alice
    .client
    .create_channel(alice.space().await, "team")
    .await
    .unwrap();
  for user in [bob.client.user_id(), carol.client.user_id()] {
    alice.client.add_member(channel, user).await.unwrap();
  }
  settle(&mut [alice, bob, carol]).await;
  channel
}

async fn commits(peer: &Peer, conversation: teamcraft_proto::ConversationId) -> usize {
  use teamcraft_core::api::ServerApi;
  peer
    .api
    .list_mls_messages(conversation, &teamcraft_proto::ListMlsMessagesQuery::default())
    .await
    .unwrap()
    .messages
    .iter()
    .filter(|message| message.kind == teamcraft_proto::MlsMessageKind::Commit)
    .count()
}

#[tokio::test]
async fn implicit_full_history_only_for_users_the_roster_vouches_for() {
  // Security review A9: the server's channel history policy and "not a guest" are unsigned.
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
  let channel = alice
    .client
    .create_channel(alice.space().await, "history")
    .await
    .unwrap();
  alice
    .client
    .send_text(channel, None, "before anyone joined")
    .await
    .unwrap();

  for peer in [&bob, &carol] {
    alice
      .client
      .add_member(channel, peer.client.user_id())
      .await
      .unwrap();
  }
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  for peer in [&mut bob, &mut carol] {
    peer.client.load_older_messages(channel).await.unwrap();
  }

  assert_eq!(bob.texts(channel), vec!["before anyone joined"]);
  assert!(carol.texts(channel).is_empty(), "Carol isn't vouched for");
}

fn policy(admin_self_join: AdminSelfJoin) -> WorkspacePolicy {
  WorkspacePolicy {
    admin_self_join,
    history_default: HistoryPolicy::FromJoin,
    ..WorkspacePolicy::default()
  }
}

mod attestation {
  use std::future::Future;
  use std::pin::Pin;
  use std::sync::Arc;

  use base64::Engine as _;
  use base64::engine::general_purpose::URL_SAFE_NO_PAD;
  use p256::ecdsa::SigningKey;
  use p256::ecdsa::signature::Signer as _;
  use teamcraft_core::store::MemoryStore;
  use teamcraft_core::{AttestationSettings, ClientSettings, JwksSource, PendingEnrollment};

  use super::*;

  const ISSUER: &str = "https://id.example.com/realms/company";

  #[tokio::test]
  async fn only_attested_identity_keys_are_pinned_when_required() {
    // Security review A1: with attestation required, the server can't hand out an identity key
    // the identity provider didn't attest.
    let server = FakeServer::new();
    let provider = Arc::new(SigningKey::random(&mut rand_core::OsRng));
    let mut alice = enroll_attested(&server, "Alice", &provider, true).await;
    let bob = enroll_attested(&server, "Bob", &provider, true).await;
    let carol = enroll_attested(&server, "Carol", &provider, false).await;
    let channel = alice
      .create_channel(
        teamcraft_core::api::ServerApi::list_spaces(&server.add_user("Admin"))
          .await
          .unwrap()
          .spaces[0]
          .id,
        "attested",
      )
      .await
      .unwrap();

    alice.add_member(channel, bob.user_id()).await.unwrap();
    // Carol's key has no attestation: it isn't pinned, so she can't be added.
    assert!(matches!(
      alice.add_member(channel, carol.user_id()).await,
      Err(teamcraft_core::CoreError::Mls(
        teamcraft_crypto::mls::MlsError::UnknownUser(_)
      ))
    ));
  }

  async fn enroll_attested(
    server: &FakeServer,
    name: &str,
    provider: &Arc<SigningKey>,
    with_token: bool,
  ) -> common::TestClient {
    let api = server.add_user(name);
    let pending = PendingEnrollment::new();
    let mut options = common::options(common::PASSPHRASE);
    if with_token {
      options.maybe_attestation = Some(token(provider, &pending.attestation_claim(api.user_id())));
    }
    let settings = ClientSettings {
      maybe_attestation: Some(AttestationSettings {
        issuer: ISSUER.into(),
        audience: "teamcraft".into(),
        is_required: true,
      }),
      ..common::settings()
    };
    let mut client = pending
      .complete(
        api,
        Arc::new(MemoryStore::new()),
        common::clock(),
        settings,
        options,
      )
      .await
      .unwrap();
    client.set_jwks_source(Box::new(Jwks(provider.clone())));
    client
  }

  fn token(provider: &SigningKey, claim: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"ES256","kid":"k1"}"#);
    let claims =
      serde_json::json!({ "iss": ISSUER, "aud": "teamcraft", "sub": claim, "tc_identity_key": claim });
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    let input = format!("{header}.{payload}");
    let signature: p256::ecdsa::Signature = provider.sign(input.as_bytes());
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()))
  }

  struct Jwks(Arc<SigningKey>);

  impl JwksSource for Jwks {
    fn fetch(&self) -> Pin<Box<dyn Future<Output = Option<Vec<u8>>> + '_>> {
      let point = self.0.verifying_key().to_encoded_point(false);
      let jwks = serde_json::json!({ "keys": [{
        "kty": "EC", "crv": "P-256", "kid": "k1",
        "x": URL_SAFE_NO_PAD.encode(point.x().unwrap()),
        "y": URL_SAFE_NO_PAD.encode(point.y().unwrap()),
      }]});
      Box::pin(async move { Some(serde_json::to_vec(&jwks).unwrap()) })
    }
  }
}
