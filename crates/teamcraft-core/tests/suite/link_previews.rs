//! Link previews made by the sender under the signed policy's domain list, and dropped by
//! receivers once the policy disallows the domain (e2ee.md §12).
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

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
    maybe_client_message_id: None,
    conversation: channel,
    maybe_thread: None,
    body: "and https://elsewhere.example.org/x".into(),
    maybe_reply_to: None,
    maybe_reply_attachment: None,
    priority: Default::default(),
    wants_acknowledgement: false,
    is_silent: false,
    also_send_to_channel: false,
    files: Vec::new(),
    link_preview: Default::default(),
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
    maybe_client_message_id: None,
    conversation: channel,
    maybe_thread: None,
    body: "read https://docs.example.com/guide please".into(),
    maybe_reply_to: None,
    maybe_reply_attachment: None,
    priority: Default::default(),
    wants_acknowledgement: false,
    is_silent: false,
    also_send_to_channel: false,
    files: Vec::new(),
    link_preview: Default::default(),
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

  // The Files & links tab (issue #609) reads them from the store's index, newest first, with the
  // sender's preview where the policy allows it.
  let page = bob_ui.conversation_links(channel, 2).unwrap();
  let urls: Vec<_> = page.links.iter().map(|link| link.url.as_str()).collect();
  assert_eq!(
    urls,
    [
      "https://docs.example.com/guide",
      "https://elsewhere.example.org/x"
    ]
  );
  assert!(page.has_more);
  assert_eq!(page.links[0].message, last.id);
  assert_eq!(page.links[0].sender, alice.client.user_id());
  assert!(page.links[0].maybe_preview.is_some());
  assert!(page.links[1].maybe_preview.is_none());
  // A reply only in its thread isn't listed, as the channel doesn't show it.
  alice_ui.post_message(
    channel,
    Some(teamcraft_client_api::model::thread_id_of(last.id)),
    "in the thread: https://docs.example.com/thread",
  );
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  let all = bob_ui.conversation_links(channel, 10).unwrap();
  assert_eq!(all.links.len(), 3);
  assert!(!all.has_more);

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
  assert!(
    bob_ui.conversation_links(channel, 1).unwrap().links[0]
      .maybe_preview
      .is_none(),
    "the links tab follows the policy too"
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

/// The composer's link preview (#2073): made while typing, sent exactly as shown.
mod composer {
  use std::rc::Rc;
  use std::sync::Arc;

  use teamcraft_client_api::actions::{ComposerLinkPreview, OutgoingLinkPreview, OutgoingMessage};
  use teamcraft_core::clock::{Clock, ManualClock};
  use teamcraft_core::composer_preview::{ComposerFetches, RETRY_FAILED_AFTER};
  use teamcraft_core::store::MemoryStore;
  use teamcraft_core::ui_backend::{CoreChatBackend, CoreCommand};

  use super::*;

  const GUIDE: &str = "https://docs.example.com/guide";

  #[tokio::test]
  async fn the_message_carries_the_preview_the_composer_showed() {
    let (_server, mut alice, mut bob, channel) = workspace_with_policy(LinkPreviewMode::AllowList).await;
    let (ui, fetches, _clock) = composer_of(&alice, Box::new(web()));

    ui.update_composer_link(channel, None, "read https://docs.example.com/guide please");
    assert_eq!(
      ui.composer_link_preview(channel, None),
      ComposerLinkPreview::Loading { url: GUIDE.into() }
    );
    assert!(run_fetches(&ui, &fetches).await, "the preview came in");
    let ComposerLinkPreview::Ready { preview } = ui.composer_link_preview(channel, None) else {
      panic!("the composer shows the preview");
    };
    assert_eq!(preview.maybe_title.as_deref(), Some("The Guide"));
    // Typing on with the same link fetches nothing again.
    ui.update_composer_link(channel, None, "read https://docs.example.com/guide please, now");
    assert!(!run_fetches(&ui, &fetches).await);

    ui.send_message(composed(
      channel,
      "read https://docs.example.com/guide please, now",
    ));
    run_commands(&mut alice, &ui).await;
    settle(&mut [&mut alice, &mut bob]).await;
    let bob_ui = backend_of(&bob);
    let sent = user_messages(&bob_ui, channel).remove(0);
    let received = bob_ui.message_extras(channel, &[sent.id])[0]
      .maybe_link_preview
      .clone()
      .unwrap();
    assert_eq!(received, preview, "exactly what the composer showed");
    assert_eq!(ui.composer_link_preview(channel, None), ComposerLinkPreview::None);
  }

  #[tokio::test]
  async fn a_removed_preview_is_not_sent_and_stays_removed_for_that_link() {
    let (_server, mut alice, mut bob, channel) = workspace_with_policy(LinkPreviewMode::AllowList).await;
    let (ui, fetches, _clock) = composer_of(&alice, Box::new(web()));
    ui.update_composer_link(channel, None, GUIDE);
    run_fetches(&ui, &fetches).await;

    ui.remove_composer_link_preview(channel, None);
    assert_eq!(ui.composer_link_preview(channel, None), ComposerLinkPreview::None);
    // Kept in the store: another backend (another thread, or after a restart) sees it removed.
    let other = backend_of(&alice);
    other.update_composer_link(channel, None, &format!("{GUIDE} again"));
    assert!(!run_fetches(&other, &fetches).await, "not fetched again");
    assert_eq!(
      other.composer_link_preview(channel, None),
      ComposerLinkPreview::None
    );

    ui.send_message(composed(channel, GUIDE));
    run_commands(&mut alice, &ui).await;
    settle(&mut [&mut alice, &mut bob]).await;
    let bob_ui = backend_of(&bob);
    let sent = user_messages(&bob_ui, channel).remove(0);
    assert!(
      bob_ui.message_extras(channel, &[sent.id])[0]
        .maybe_link_preview
        .is_none()
    );
  }

  #[tokio::test]
  async fn sending_while_the_page_loads_goes_at_once_without_a_preview() {
    let (_server, mut alice, mut bob, channel) = workspace_with_policy(LinkPreviewMode::AllowList).await;
    let (ui, fetches, _clock) = composer_of(&alice, Box::new(Stalled));
    ui.update_composer_link(channel, None, GUIDE);
    let fetch = take_fetches(&ui, &fetches).pop().unwrap();

    ui.send_message(composed(channel, GUIDE));
    // The host hears that the draft moved on; the message is in the outbox already.
    assert!(take_fetches(&ui, &fetches).is_empty());
    run_commands(&mut alice, &ui).await;
    settle(&mut [&mut alice, &mut bob]).await;
    let bob_ui = backend_of(&bob);
    let sent = user_messages(&bob_ui, channel).remove(0);
    assert!(
      bob_ui.message_extras(channel, &[sent.id])[0]
        .maybe_link_preview
        .is_none()
    );
    // The send cancelled the fetch: it ends without writing.
    assert!(!fetch.await);
  }

  #[tokio::test]
  async fn editing_the_link_away_cancels_its_fetch() {
    let (_server, alice, _bob, channel) = workspace_with_policy(LinkPreviewMode::AllowList).await;
    let (ui, fetches, _clock) = composer_of(&alice, Box::new(Stalled));
    ui.update_composer_link(channel, None, GUIDE);
    let fetch = take_fetches(&ui, &fetches).pop().unwrap();

    ui.update_composer_link(channel, None, "no link any more");
    assert!(take_fetches(&ui, &fetches).is_empty());
    assert!(!fetch.await, "cancelled");
    assert_eq!(ui.composer_link_preview(channel, None), ComposerLinkPreview::None);
  }

  #[tokio::test]
  async fn a_failed_fetch_is_tried_again_after_a_while_and_disallowed_links_get_none() {
    let (_server, alice, _bob, channel) = workspace_with_policy(LinkPreviewMode::AllowList).await;
    let (ui, fetches, clock) = composer_of(&alice, Box::new(FakeWeb(HashMap::new())));
    ui.update_composer_link(channel, None, GUIDE);
    assert!(run_fetches(&ui, &fetches).await);
    assert_eq!(ui.composer_link_preview(channel, None), ComposerLinkPreview::None);
    ui.update_composer_link(channel, None, &format!("{GUIDE} ok"));
    assert!(take_fetches(&ui, &fetches).is_empty(), "not on every keystroke");
    clock.advance(i64::try_from(RETRY_FAILED_AFTER.as_millis()).unwrap());
    ui.update_composer_link(channel, None, &format!("{GUIDE} ok?"));
    assert_eq!(take_fetches(&ui, &fetches).len(), 1, "tried again");

    ui.update_composer_link(channel, None, "https://elsewhere.example.org/x");
    assert!(take_fetches(&ui, &fetches).is_empty());
    assert_eq!(ui.composer_link_preview(channel, None), ComposerLinkPreview::None);
  }

  /// A page that never answers.
  struct Stalled;

  impl PreviewFetcher for Stalled {
    fn fetch(&self, _url: Url, _max_bytes: usize) -> Pin<Box<dyn Future<Output = Option<Fetched>> + '_>> {
      Box::pin(std::future::pending())
    }
  }

  type Ui = CoreChatBackend<Arc<MemoryStore>>;

  fn composer_of(
    peer: &Peer,
    fetcher: Box<dyn PreviewFetcher>,
  ) -> (Ui, ComposerFetches<Arc<MemoryStore>>, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let ui = CoreChatBackend::new(peer.store.clone(), peer.client.user_id(), Box::new(clock.clone()));
    let fetches = ComposerFetches::new(peer.store.clone(), Default::default())
      .with_fetcher(fetcher, Rc::new(clock.clone()) as Rc<dyn Clock>);
    (ui, fetches, clock)
  }

  /// The fetches the backend asked the host for; other commands are dropped.
  fn take_fetches(
    ui: &Ui,
    fetches: &ComposerFetches<Arc<MemoryStore>>,
  ) -> Vec<futures_util::future::LocalBoxFuture<'static, bool>> {
    ui.drain_commands()
      .iter()
      .filter(|command| matches!(command, CoreCommand::ComposerLinkChanged { .. }))
      .filter_map(|command| fetches.on_command(command))
      .collect()
  }

  /// Runs the fetches the backend asked for; whether any changed the preview.
  async fn run_fetches(ui: &Ui, fetches: &ComposerFetches<Arc<MemoryStore>>) -> bool {
    let mut is_changed = false;
    for fetch in take_fetches(ui, fetches) {
      is_changed |= fetch.await;
    }
    is_changed
  }

  fn composed(conversation: ConversationId, body: &str) -> OutgoingMessage {
    OutgoingMessage {
      maybe_client_message_id: None,
      conversation,
      maybe_thread: None,
      body: body.into(),
      maybe_reply_to: None,
      maybe_reply_attachment: None,
      priority: Default::default(),
      wants_acknowledgement: false,
      is_silent: false,
      also_send_to_channel: false,
      files: Vec::new(),
      link_preview: OutgoingLinkPreview::AsShown,
    }
  }
}
