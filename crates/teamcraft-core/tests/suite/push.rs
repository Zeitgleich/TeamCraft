//! Push previews (e2ee.md §7): what a notification process shows, from the sealed wake-up and the
//! local store, fetching the message when it may.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, settings, settle};
use teamcraft_core::ClientSettings;
use teamcraft_core::api::fake::{FakeServer, FakeServerApi};
use teamcraft_core::push_preview::{
  PreviewContent, PreviewGap, device_push_content, push_preview, save_device_push_content,
};
use teamcraft_core::store::{LocalStore, MessageFilter};
use teamcraft_crypto::push::seal_wakeup;
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{ConversationId, MessageId, PushContentMode, PushKind, PushProvider, PushWakeUp};

#[tokio::test]
async fn preview_fetches_and_decrypts_a_message_the_app_has_not_seen() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let message = alice_says(&mut alice, channel, "lunch at noon?").await;
  let sealed = wake_up_for(&server, &bob, &alice, channel, message);

  // Bob's app hasn't processed its gateway, so only the fetch can show the text.
  let stored_before = bob.store.messages(channel, MessageFilter::All).unwrap().len();
  let preview = push_preview(bob.store.as_ref(), Some(&bob.api), &sealed)
    .await
    .unwrap();
  assert_eq!(preview.content, PreviewContent::Text("lunch at noon?".into()));
  assert_eq!(preview.maybe_gap, None);
  assert_eq!(preview.maybe_conversation_name.as_deref(), Some("planning"));
  assert_eq!(preview.maybe_sender_name.as_deref(), Some("Alice"));
  assert!(!preview.is_direct);
  assert!(preview.is_conversation_verified);
  // A preview only reads: the store didn't change.
  assert_eq!(
    bob.store.messages(channel, MessageFilter::All).unwrap().len(),
    stored_before
  );
  settle(&mut [&mut bob]).await;
}

/// A browser rotates its Web Push subscription (`pushsubscriptionchange`, e2ee.md §7): the new one
/// is registered under the same push key, so wake-ups sealed before still open; an unchanged one
/// costs no request.
#[tokio::test]
async fn a_rotated_subscription_is_registered_under_the_same_push_key() {
  let (server, _alice, mut bob, _channel) = channel_with_two().await;
  let device = bob.client.device_id();
  let before = server.push_registration(device).unwrap();
  assert_eq!(
    bob.client.registered_push_provider().unwrap(),
    Some(PushProvider::WebPush)
  );

  assert!(
    !bob
      .client
      .renew_push_token(PushProvider::WebPush, "{}")
      .await
      .unwrap()
  );
  assert!(
    !bob
      .client
      .renew_push_token(PushProvider::Fcm, "other")
      .await
      .unwrap()
  );
  let rotated = r#"{"endpoint":"https://push.example.com/rotated"}"#;
  assert!(
    bob
      .client
      .renew_push_token(PushProvider::WebPush, rotated)
      .await
      .unwrap()
  );

  let after = server.push_registration(device).unwrap();
  assert_eq!(after.token, rotated);
  assert_eq!(after.push_key.as_slice(), before.push_key.as_slice());
  assert!(
    !bob
      .client
      .renew_push_token(PushProvider::WebPush, rotated)
      .await
      .unwrap()
  );
}

#[tokio::test]
async fn without_a_session_only_names_are_shown_unless_the_store_has_the_message() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let message = alice_says(&mut alice, channel, "see you").await;
  let sealed = wake_up_for(&server, &bob, &alice, channel, message);

  let preview = push_preview::<FakeServerApi>(bob.store.as_ref(), None, &sealed)
    .await
    .unwrap();
  assert_eq!(preview.content, PreviewContent::Unavailable);
  assert_eq!(preview.maybe_sender_name.as_deref(), Some("Alice"));
  assert!(!preview.is_conversation_verified);

  settle(&mut [&mut alice, &mut bob]).await;
  let preview = push_preview::<FakeServerApi>(bob.store.as_ref(), None, &sealed)
    .await
    .unwrap();
  assert_eq!(preview.content, PreviewContent::Text("see you".into()));
  assert!(preview.is_conversation_verified);
}

#[tokio::test]
async fn a_wake_up_naming_another_conversation_is_not_verified() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let message = alice_says(&mut alice, channel, "for the planning channel").await;
  let other = alice
    .client
    .create_channel(alice.space().await, "other")
    .await
    .unwrap();
  // The server claims the planning message belongs to another conversation.
  let sealed = wake_up_for(&server, &bob, &alice, other, message);
  let preview = push_preview(bob.store.as_ref(), Some(&bob.api), &sealed)
    .await
    .unwrap();
  assert_eq!(preview.content, PreviewContent::Unavailable);
  assert!(!preview.is_conversation_verified);
  settle(&mut [&mut alice, &mut bob]).await;
}

#[tokio::test]
async fn a_payload_sealed_with_another_key_is_rejected() {
  let (_server, mut alice, mut bob, channel) = channel_with_two().await;
  alice_says(&mut alice, channel, "hi").await;
  let foreign = seal_wakeup(
    &SymmetricKey::generate(&mut rand_core::OsRng),
    b"{}",
    &mut rand_core::OsRng,
  )
  .unwrap();
  assert!(
    push_preview(bob.store.as_ref(), Some(&bob.api), &foreign)
      .await
      .is_err()
  );
  settle(&mut [&mut bob]).await;
}

#[tokio::test]
async fn the_signed_policy_decides_what_a_notification_shows() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let (alice_id, _bob_id) = (alice.client.user_id(), bob.client.user_id());
  alice
    .client
    .publish_roster(&[(alice_id, teamcraft_core::workspace::RosterRole::Owner)])
    .await
    .unwrap();
  let mut policy = teamcraft_proto::WorkspacePolicy::default();
  policy.notifications.push_content = PushContentMode::Sender;
  alice.client.publish_policy(policy).await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  let message = alice_says(&mut alice, channel, "salary talk").await;
  let sealed = wake_up_for(&server, &bob, &alice, channel, message);
  let preview = push_preview(bob.store.as_ref(), Some(&bob.api), &sealed)
    .await
    .unwrap();
  // Names stay, the text is withheld.
  assert_eq!(preview.content, PreviewContent::Unavailable);
  assert_eq!(preview.maybe_sender_name.as_deref(), Some("Alice"));
  assert_eq!(preview.maybe_gap, Some(PreviewGap::WorkspacePolicy));
  settle(&mut [&mut bob]).await;
}

/// A first direct message from somebody Bob never shared a conversation with, while Bob's app was
/// closed: neither the conversation, the sender's name nor the keys are on the device. The
/// preview falls back to what it knows and says why; the app's catch-up then shows it all.
#[tokio::test]
async fn a_first_direct_message_shows_after_the_app_caught_the_conversation_up() {
  let (server, _alice, mut bob, _channel) = channel_with_two().await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let dm = carol.client.open_dm(bob.client.user_id()).await.unwrap();
  let message = carol_says(&mut carol, dm, "are you around?").await;
  let sealed = wake_up_for(&server, &bob, &carol, dm, message);

  let preview = push_preview(bob.store.as_ref(), Some(&bob.api), &sealed)
    .await
    .unwrap();
  // Never dropped: a notification still comes, it just can't say more yet.
  assert_eq!(preview.content, PreviewContent::Unavailable);
  assert_eq!(preview.maybe_gap, Some(PreviewGap::NotDecrypted));
  assert!(!preview.is_conversation_verified);
  assert!(preview.needs_catch_up());

  bob.client.catch_up_for_push(dm).await.unwrap();
  let preview = push_preview::<FakeServerApi>(bob.store.as_ref(), None, &sealed)
    .await
    .unwrap();
  assert_eq!(preview.content, PreviewContent::Text("are you around?".into()));
  assert_eq!(preview.maybe_sender_name.as_deref(), Some("Carol"));
  assert!(preview.is_direct);
  assert!(preview.is_conversation_verified);
  assert_eq!(preview.maybe_gap, None);
  assert!(!preview.needs_catch_up());
  settle(&mut [&mut carol, &mut bob]).await;
}

#[tokio::test]
async fn without_a_session_the_preview_says_so() {
  let (server, mut alice, bob, channel) = channel_with_two().await;
  let message = alice_says(&mut alice, channel, "later").await;
  let sealed = wake_up_for(&server, &bob, &alice, channel, message);
  let preview = push_preview::<FakeServerApi>(bob.store.as_ref(), None, &sealed)
    .await
    .unwrap();
  assert_eq!(preview.maybe_gap, Some(PreviewGap::NoSession));
  assert_eq!(preview.maybe_sender_name.as_deref(), Some("Alice"));
}

#[tokio::test]
async fn the_device_setting_shows_less_than_the_policy_allows() {
  let (server, mut alice, bob, channel) = channel_with_two().await;
  let message = alice_says(&mut alice, channel, "private").await;
  let sealed = wake_up_for(&server, &bob, &alice, channel, message);
  assert_eq!(device_push_content(bob.store.as_ref()), PushContentMode::Full);

  save_device_push_content(bob.store.as_ref(), &PushContentMode::Sender).unwrap();
  let preview = push_preview(bob.store.as_ref(), Some(&bob.api), &sealed)
    .await
    .unwrap();
  assert_eq!(preview.content, PreviewContent::Unavailable);
  assert_eq!(preview.maybe_sender_name.as_deref(), Some("Alice"));
  assert_eq!(preview.maybe_gap, Some(PreviewGap::DeviceSetting));
  // Withheld on purpose: nothing to catch up.
  assert!(!preview.needs_catch_up());

  save_device_push_content(bob.store.as_ref(), &PushContentMode::Generic).unwrap();
  let preview = push_preview(bob.store.as_ref(), Some(&bob.api), &sealed)
    .await
    .unwrap();
  assert!(preview.maybe_sender_name.is_none() && preview.maybe_conversation_name.is_none());
}

async fn channel_with_two() -> (FakeServer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "planning")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  bob
    .client
    .register_push(PushProvider::WebPush, "{}")
    .await
    .unwrap();
  (server, alice, bob, channel)
}

async fn alice_says(alice: &mut Peer, channel: ConversationId, text: &str) -> MessageId {
  carol_says(alice, channel, text).await
}

async fn carol_says(sender: &mut Peer, channel: ConversationId, text: &str) -> MessageId {
  sender.client.send_text(channel, None, text).await.unwrap();
  sender
    .store
    .messages(channel, MessageFilter::TopLevel)
    .unwrap()
    .last()
    .and_then(|message| message.maybe_server_id)
    .unwrap()
}

/// Seals what the server would push for Alice's message.
fn wake_up_for(
  server: &FakeServer,
  bob: &Peer,
  alice: &Peer,
  channel: ConversationId,
  message: MessageId,
) -> Vec<u8> {
  let registration = server.push_registration(bob.client.device_id()).unwrap();
  let key = SymmetricKey::from_slice(registration.push_key.as_slice()).unwrap();
  let wake_up = PushWakeUp {
    kind: PushKind::Message,
    conversation_id: channel,
    maybe_message_id: Some(message),
    maybe_sender_id: Some(alice.client.user_id()),
    is_urgent: false,
    maybe_thread_id: None,
    maybe_call_id: None,
    maybe_badge: None,
    maybe_sent_at: None,
  };
  seal_wakeup(
    &key,
    &serde_json::to_vec(&wake_up).unwrap(),
    &mut rand_core::OsRng,
  )
  .unwrap()
}

/// Device tokens sealed for the push gateway (docs/push-gateway.md): the server only ever holds a
/// blob that the gateway opens for this one server.
mod sealed_tokens {
  use super::*;

  use base64::Engine as _;
  use base64::engine::general_purpose::URL_SAFE_NO_PAD;
  use teamcraft_core::api::fake::FAKE_GATEWAY_TOKEN_SEED;
  use teamcraft_crypto::push_gateway::{BoundToken, open_bound_token};
  use teamcraft_proto::{BOUND_TOKEN_PREFIX, PushGatewayBinding};

  const SERVER_ID: &str = "0123456789abcdef0123456789abcdef";
  const APNS_TOKEN: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";

  fn through_gateway(server: &FakeServer, providers: Vec<PushProvider>) {
    server.set_push_gateway(Some(PushGatewayBinding {
      url: "https://gateway.example.com".into(),
      server_id: SERVER_ID.into(),
      providers,
    }));
  }

  /// What the gateway would learn from the registration.
  fn opened(server: &FakeServer, peer: &Peer) -> BoundToken {
    let registration = server.push_registration(peer.client.device_id()).unwrap();
    let blob = URL_SAFE_NO_PAD
      .decode(registration.token.strip_prefix(BOUND_TOKEN_PREFIX).unwrap())
      .unwrap();
    open_bound_token(&FAKE_GATEWAY_TOKEN_SEED, &blob).unwrap()
  }

  #[tokio::test]
  async fn the_server_gets_a_sealed_token_bound_to_itself_never_the_raw_one() {
    let server = FakeServer::new();
    let mut bob = Peer::enroll(&server, "Bob").await;
    through_gateway(&server, vec![PushProvider::Apns, PushProvider::Fcm]);

    bob
      .client
      .register_push(PushProvider::Apns, APNS_TOKEN)
      .await
      .unwrap();

    let registration = server.push_registration(bob.client.device_id()).unwrap();
    assert!(registration.token.starts_with(BOUND_TOKEN_PREFIX));
    assert!(!registration.token.contains(APNS_TOKEN));
    let bound = opened(&server, &bob);
    assert_eq!(bound.token, APNS_TOKEN);
    assert_eq!(bound.provider, "apns");
    assert_eq!(bound.server_id, SERVER_ID);
    let now = i64::try_from(bound.expires_at_ms).unwrap();
    assert!(now > 0);
    settle(&mut [&mut bob]).await;
  }

  #[tokio::test]
  async fn self_built_apps_give_servers_without_a_gateway_the_raw_token() {
    let server = FakeServer::new();
    let mut bob = Peer::enroll(&server, "Bob").await;

    bob
      .client
      .register_push(PushProvider::Fcm, "fcm-token")
      .await
      .unwrap();
    assert_eq!(
      server.push_registration(bob.client.device_id()).unwrap().token,
      "fcm-token"
    );

    // A gateway for FCM only: APNs goes to Apple directly, and Web Push never uses it.
    through_gateway(&server, vec![PushProvider::Fcm]);
    bob
      .client
      .register_push(PushProvider::Apns, APNS_TOKEN)
      .await
      .unwrap();
    assert_eq!(
      server.push_registration(bob.client.device_id()).unwrap().token,
      APNS_TOKEN
    );
    bob
      .client
      .register_push(PushProvider::WebPush, "{}")
      .await
      .unwrap();
    assert_eq!(
      server.push_registration(bob.client.device_id()).unwrap().token,
      "{}"
    );
    settle(&mut [&mut bob]).await;
  }

  #[tokio::test]
  async fn the_token_is_sealed_again_only_when_due_keeping_the_push_key() {
    let server = FakeServer::new();
    let mut bob = Peer::enroll(&server, "Bob").await;
    through_gateway(&server, vec![PushProvider::Apns]);
    bob
      .client
      .register_push(PushProvider::Apns, APNS_TOKEN)
      .await
      .unwrap();
    let first = server.push_registration(bob.client.device_id()).unwrap();

    // Nothing is due right after registering.
    assert!(!bob.client.refresh_push().await.unwrap());
    assert_eq!(
      server.push_registration(bob.client.device_id()).unwrap().token,
      first.token
    );

    // Sealed longer ago than the setting allows: a new blob, the same push key, the old blob revoked.
    bob.client.set_push_reseal_after(std::time::Duration::ZERO);
    assert!(bob.client.refresh_push().await.unwrap());
    let second = server.push_registration(bob.client.device_id()).unwrap();
    assert_ne!(second.token, first.token);
    assert_eq!(second.push_key, first.push_key);
    assert_eq!(opened(&server, &bob).token, APNS_TOKEN);
    let old_blob = URL_SAFE_NO_PAD
      .decode(first.token.strip_prefix(BOUND_TOKEN_PREFIX).unwrap())
      .unwrap();
    assert_eq!(server.revoked_gateway_tokens(), vec![old_blob]);
    settle(&mut [&mut bob]).await;
  }

  #[tokio::test]
  async fn a_server_that_changes_how_it_sends_has_the_registration_redone() {
    let server = FakeServer::new();
    let mut bob = Peer::enroll(&server, "Bob").await;
    bob
      .client
      .register_push(PushProvider::Apns, APNS_TOKEN)
      .await
      .unwrap();
    assert!(!bob.client.refresh_push().await.unwrap());

    // The server moves to a gateway: the raw token is replaced by a sealed one.
    through_gateway(&server, vec![PushProvider::Apns]);
    assert!(bob.client.refresh_push().await.unwrap());
    assert_eq!(opened(&server, &bob).token, APNS_TOKEN);

    // Another server ID (the server's key changed): sealed again for the new one.
    server.set_push_gateway(Some(PushGatewayBinding {
      url: "https://gateway.example.com".into(),
      server_id: "ffffffffffffffffffffffffffffffff".into(),
      providers: vec![PushProvider::Apns],
    }));
    assert!(bob.client.refresh_push().await.unwrap());
    assert_eq!(
      opened(&server, &bob).server_id,
      "ffffffffffffffffffffffffffffffff"
    );
    settle(&mut [&mut bob]).await;
  }

  #[tokio::test]
  async fn leaving_the_server_ends_the_sealed_token_at_the_gateway() {
    let server = FakeServer::new();
    let mut bob = Peer::enroll(&server, "Bob").await;
    through_gateway(&server, vec![PushProvider::Apns]);
    bob
      .client
      .register_push(PushProvider::Apns, APNS_TOKEN)
      .await
      .unwrap();
    let blob = URL_SAFE_NO_PAD
      .decode(
        server
          .push_registration(bob.client.device_id())
          .unwrap()
          .token
          .strip_prefix(BOUND_TOKEN_PREFIX)
          .unwrap(),
      )
      .unwrap();

    bob.client.unregister_push().await.unwrap();

    assert!(server.push_registration(bob.client.device_id()).is_none());
    assert_eq!(server.revoked_gateway_tokens(), vec![blob]);
    // Nothing is left to renew.
    assert!(!bob.client.refresh_push().await.unwrap());
    settle(&mut [&mut bob]).await;
  }

  #[tokio::test]
  async fn an_app_built_for_a_gateway_never_hands_out_its_raw_token() {
    let server = FakeServer::new();
    let mut bob = Peer::enroll_with_settings(
      &server,
      "Bob",
      ClientSettings {
        maybe_push_gateway_url: Some("https://store-app-gateway.example.com".into()),
        ..settings()
      },
    )
    .await;
    // The server names no gateway (or none for this provider): no registration at all.
    for providers in [None, Some(vec![PushProvider::Fcm])] {
      server.set_push_gateway(providers.map(|providers| PushGatewayBinding {
        url: "https://gateway.example.com".into(),
        server_id: SERVER_ID.into(),
        providers,
      }));
      assert!(
        bob
          .client
          .register_push(PushProvider::Apns, APNS_TOKEN)
          .await
          .is_err()
      );
      assert!(server.push_registration(bob.client.device_id()).is_none());
    }

    // Registered sealed, then the server stops naming a gateway: the sealed registration stays,
    // it is not replaced by the raw token.
    through_gateway(&server, vec![PushProvider::Apns]);
    bob
      .client
      .register_push(PushProvider::Apns, APNS_TOKEN)
      .await
      .unwrap();
    server.set_push_gateway(None);
    assert!(bob.client.refresh_push().await.is_err());
    let registered = server.push_registration(bob.client.device_id()).unwrap().token;
    assert!(registered.starts_with(BOUND_TOKEN_PREFIX), "{registered}");
    assert!(server.revoked_gateway_tokens().is_empty());
    settle(&mut [&mut bob]).await;
  }

  #[tokio::test]
  async fn the_gateway_an_app_was_built_for_wins_over_the_one_the_server_names() {
    let server = FakeServer::new();
    // The fake gateway answers for any URL; the server's own URL would be gateway.example.com.
    let mut bob = Peer::enroll_with_settings(
      &server,
      "Bob",
      ClientSettings {
        maybe_push_gateway_url: Some("https://store-app-gateway.example.com".into()),
        ..settings()
      },
    )
    .await;
    through_gateway(&server, vec![PushProvider::Apns]);
    bob
      .client
      .register_push(PushProvider::Apns, APNS_TOKEN)
      .await
      .unwrap();
    bob.client.unregister_push().await.unwrap();

    assert_eq!(
      server.revoked_gateway_urls(),
      vec!["https://store-app-gateway.example.com".to_owned()]
    );
    settle(&mut [&mut bob]).await;
  }
}
