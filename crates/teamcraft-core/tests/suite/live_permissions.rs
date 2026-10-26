//! Changed rights reach the UI without a reload (issue #758): the server's `conversation_update`
//! makes the client fetch the conversation, and its `my_permissions` with it, so desktop, web and
//! the native apps offer the new set of actions at once.
#![cfg(feature = "ui")]

use crate::common;

use common::{Peer, backend_of, run_commands, settle};
use teamcraft_client_api::actions::NewChannel;
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_proto::{GatewayEvent, Permission};

#[tokio::test]
async fn a_conversation_update_brings_the_new_permissions_to_the_ui() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let space = alice.space().await;
  let ui = backend_of(&alice);
  ui.create_channel_with(NewChannel {
    maybe_idempotency_key: None,
    space,
    name: "news".into(),
    is_private: false,
    maybe_topic: None,
    maybe_category: None,
    maybe_history: None,
    is_integration: false,
  });
  run_commands(&mut alice, &ui).await;
  let channel = ui
    .space_conversations(space)
    .into_iter()
    .find(|conversation| conversation.name == "news")
    .unwrap()
    .id;
  let before = ui.conversation(channel).unwrap();
  assert_eq!(
    before.maybe_permissions, None,
    "the fake answers like an older server"
  );
  assert!(before.can_react() && before.can_manage_bookmarks(false));

  // An admin turned the channel into an announcement channel: reading only.
  server.set_my_permissions(channel, Some(vec![Permission::AddReaction]));
  server.publish_event(
    &[alice.client.user_id()],
    &GatewayEvent::ConversationUpdate {
      conversation_id: channel,
    },
  );
  settle(&mut [&mut alice]).await;

  let after = ui.conversation(channel).unwrap();
  assert_eq!(after.maybe_permissions, Some(vec![Permission::AddReaction]));
  assert!(!after.can_post);
  assert!(after.can_react());
  assert!(!after.can_manage_bookmarks(false));
  assert!(!after.can_manage_channel());
}
