//! Link previews made by the sender under the signed policy's domain list, and dropped by
//! receivers once the policy disallows the domain (e2ee.md §12).
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

mod common;

use std::collections::HashMap;
use std::future::Future;
use std::io::Cursor;
use std::pin::Pin;

use common::{Peer, backend_of, run_commands, settle};
use image::{ImageBuffer, ImageFormat, Rgb};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::link_preview::{Fetched, PreviewFetcher};
use teamcraft_core::workspace::RosterRole;
use teamcraft_proto::{ConversationId, LinkPreviewMode, LinkPreviewPolicy, WorkspacePolicy};
use url::Url;

/// A tiny web: URL → response.
struct FakeWeb(HashMap<String, Fetched>);

impl PreviewFetcher for FakeWeb {
  fn fetch(&self, url: Url, max_bytes: usize) -> Pin<Box<dyn Future<Output = Option<Fetched>> + '_>> {
    let maybe = self
      .0
      .get(url.as_str())
      .cloned()
      .filter(|fetched| fetched.body.len() <= max_bytes);
    Box::pin(async move { maybe })
  }
}

#[tokio::test]
async fn allowed_links_get_previews_and_receivers_follow_the_policy() {
  let (_server, mut alice, mut bob, channel) = workspace_with_policy(LinkPreviewMode::AllowList).await;
  alice.client.set_preview_fetcher(Box::new(web()));
  let (alice_ui, bob_ui) = (backend_of(&alice), backend_of(&bob));

  alice_ui.post_message(channel, None, "read https://docs.example.com/guide please");
  alice_ui.send_message(teamcraft_client_api::actions::OutgoingMessage {
    conversation: channel,
    maybe_thread: None,
    body: "and https://elsewhere.example.org/x".into(),
    maybe_reply_to: None,
    priority: Default::default(),
    wants_acknowledgement: false,
    is_silent: false,
    also_send_to_channel: false,
    files: Vec::new(),
  });
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  let messages = user_messages(&bob_ui, channel);
  let ids: Vec<_> = messages.iter().map(|message| message.id).collect();
  let extras = bob_ui.message_extras(channel, &ids);
  // `post_message` sends plain text without previews; `send_message` makes them.
  assert!(extras[0].maybe_link_preview.is_none());
  assert!(
    extras[1].maybe_link_preview.is_none(),
    "elsewhere.example.org isn't allowed"
  );

  alice_ui.send_message(teamcraft_client_api::actions::OutgoingMessage {
    conversation: channel,
    maybe_thread: None,
    body: "read https://docs.example.com/guide please".into(),
    maybe_reply_to: None,
    priority: Default::default(),
    wants_acknowledgement: false,
    is_silent: false,
    also_send_to_channel: false,
    files: Vec::new(),
  });
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  let last = user_messages(&bob_ui, channel).remove(2);
  let preview = bob_ui.message_extras(channel, &[last.id])[0]
    .maybe_link_preview
    .clone()
    .unwrap();
  assert_eq!(preview.maybe_title.as_deref(), Some("The Guide"));
  assert_eq!(preview.maybe_site_name.as_deref(), Some("Docs"));
  assert!(preview.maybe_image.is_some(), "the og:image became a thumbnail");

  // The owner switches to a block list naming the domain: Bob's client drops the preview.
  alice
    .client
    .publish_policy(policy(LinkPreviewMode::BlockList))
    .await
    .unwrap();
  bob.client.sync_workspace().await.unwrap();
  assert!(
    bob_ui.message_extras(channel, &[last.id])[0]
      .maybe_link_preview
      .is_none()
  );
}

async fn workspace_with_policy(mode: LinkPreviewMode) -> (FakeServer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let alice_id = alice.client.user_id();
  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner)])
    .await
    .unwrap();
  alice.client.publish_policy(policy(mode)).await.unwrap();
  bob.client.sync_workspace().await.unwrap();
  let channel = alice
    .client
    .create_channel(alice.space().await, "links")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  (server, alice, bob, channel)
}

/// Without system messages ("Bob joined").
fn user_messages(
  backend: &impl ChatBackend,
  conversation: ConversationId,
) -> Vec<teamcraft_client_api::model::Message> {
  backend
    .messages(conversation, 0, 100)
    .into_iter()
    .filter(|message| message.kind == teamcraft_client_api::model::MessageKind::User)
    .collect()
}

fn policy(mode: LinkPreviewMode) -> WorkspacePolicy {
  WorkspacePolicy {
    link_previews: LinkPreviewPolicy {
      mode,
      domains: vec!["docs.example.com".into()],
    },
    ..WorkspacePolicy::default()
  }
}

fn web() -> FakeWeb {
  let page = r#"<html><head><meta property="og:title" content="The Guide">
    <meta property="og:site_name" content="Docs"><meta property="og:image" content="/cover.png">
    </head><body>text</body></html>"#;
  let mut image = Cursor::new(Vec::new());
  ImageBuffer::from_fn(40, 20, |_, _| Rgb([1u8, 2, 3]))
    .write_to(&mut image, ImageFormat::Png)
    .unwrap();
  let entry = |url: &str, content_type: &str, body: Vec<u8>| {
    (
      url.to_string(),
      Fetched {
        final_url: Url::parse(url).unwrap(),
        content_type: content_type.into(),
        body,
      },
    )
  };
  FakeWeb(HashMap::from([
    entry(
      "https://docs.example.com/guide",
      "text/html; charset=utf-8",
      page.as_bytes().to_vec(),
    ),
    entry(
      "https://docs.example.com/cover.png",
      "image/png",
      image.into_inner(),
    ),
    entry(
      "https://elsewhere.example.org/x",
      "text/html",
      b"<title>Elsewhere</title>".to_vec(),
    ),
  ]))
}
