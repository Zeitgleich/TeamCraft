//! Custom emoji of the workspace (issue #207, e2ee.md §4.12): an admin's device signs them, the
//! members' devices keep only what an admin of their verified roster signed, and messages and
//! reactions name them inside the ciphertext, so renames and deletions leave earlier ones alone.
#![cfg(feature = "ui")]
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, backend_of, run_commands, settle};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::workspace::RosterRole;
use teamcraft_proto::ConversationId;

/// Alice is the workspace's owner; she and Bob share a channel. Carol is a member whom the
/// (compromised) server treats as an admin, though no roster names her.
struct Team {
  /// Kept alive for the test.
  _server: FakeServer,
  alice: Peer,
  bob: Peer,
  carol: Peer,
  channel: ConversationId,
}

async fn team() -> Team {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let alice_id = alice.client.user_id();
  server.set_admin(alice_id);
  server.set_admin(carol.client.user_id());
  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner)])
    .await
    .unwrap();
  let channel = alice
    .client
    .create_channel(alice.space().await, "team")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  Team {
    _server: server,
    alice,
    bob,
    carol,
    channel,
  }
}

/// The header of a still PNG: what the checks read.
fn png(side: u32) -> Vec<u8> {
  let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
  bytes.extend_from_slice(&side.to_be_bytes());
  bytes.extend_from_slice(&side.to_be_bytes());
  bytes.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
  bytes.extend_from_slice(b"\0\0\0\0IDAT\0\0\0\0");
  bytes
}

#[tokio::test]
async fn an_admins_emoji_reaches_members_messages_and_reactions() {
  let mut t = team().await;
  let picture = png(32);
  t.alice
    .client
    .add_custom_emoji("ship_it".into(), picture.clone())
    .await
    .unwrap();
  settle(&mut [&mut t.alice, &mut t.bob]).await;
  let (alice_ui, bob_ui) = (backend_of(&t.alice), backend_of(&t.bob));
  // Nothing is read until the UI needs the set.
  assert!(bob_ui.custom_emoji().emoji.is_empty());
  bob_ui.want_custom_emoji();
  run_commands(&mut t.bob, &bob_ui).await;

  let set = bob_ui.custom_emoji();
  assert_eq!(set.emoji.len(), 1);
  let ship_it = set.emoji[0].clone();
  assert_eq!(ship_it.shortcode, "ship_it");
  assert_eq!(ship_it.signed_by, t.alice.client.user_id());
  assert_eq!(
    bob_ui.custom_emoji_picture(ship_it.sha256),
    Some(picture.clone()),
    "the picture was downloaded and checked against the signed hash"
  );

  alice_ui.post_message(t.channel, None, "deploy :ship_it: now, not :unknown:");
  run_commands(&mut t.alice, &alice_ui).await;
  settle(&mut [&mut t.alice, &mut t.bob]).await;
  let received = bob_ui
    .messages(t.channel, 0, 20)
    .into_iter()
    .find(|message| message.body.contains("deploy"))
    .unwrap();
  assert_eq!(received.body, "deploy :ship_it: now, not :unknown:");
  assert_eq!(received.custom_emoji, [ship_it.reference()]);

  bob_ui.toggle_reaction(t.channel, received.id, ":ship_it:");
  run_commands(&mut t.bob, &bob_ui).await;
  settle(&mut [&mut t.alice, &mut t.bob]).await;
  let reacted = alice_ui
    .messages(t.channel, 0, 20)
    .into_iter()
    .find(|message| message.id == received.id)
    .unwrap();
  assert_eq!(reacted.reactions.len(), 1);
  assert_eq!(reacted.reactions[0].emoji, ":ship_it:");
  assert_eq!(reacted.reactions[0].maybe_custom, Some(ship_it.reference()));
}

#[tokio::test]
async fn renames_and_deletions_leave_earlier_messages_alone() {
  let mut t = team().await;
  let picture = png(16);
  t.alice
    .client
    .add_custom_emoji("party".into(), picture.clone())
    .await
    .unwrap();
  settle(&mut [&mut t.alice, &mut t.bob]).await;
  let (alice_ui, bob_ui) = (backend_of(&t.alice), backend_of(&t.bob));
  bob_ui.want_custom_emoji();
  run_commands(&mut t.bob, &bob_ui).await;
  let party = bob_ui.custom_emoji().emoji[0].clone();
  alice_ui.post_message(t.channel, None, ":party:");
  run_commands(&mut t.alice, &alice_ui).await;
  settle(&mut [&mut t.alice, &mut t.bob]).await;

  t.alice
    .client
    .rename_custom_emoji(party.id, "celebrate".into())
    .await
    .unwrap();
  settle(&mut [&mut t.alice, &mut t.bob]).await;
  // The gateway said they changed: the next need reads them again, within the minute.
  bob_ui.want_custom_emoji();
  run_commands(&mut t.bob, &bob_ui).await;
  let renamed = bob_ui.custom_emoji();
  assert_eq!(renamed.emoji[0].shortcode, "celebrate");
  assert_eq!(renamed.emoji[0].sha256, party.sha256, "the same picture");

  t.alice.client.delete_custom_emoji(party.id).await.unwrap();
  settle(&mut [&mut t.alice, &mut t.bob]).await;
  bob_ui.want_custom_emoji();
  run_commands(&mut t.bob, &bob_ui).await;
  assert!(bob_ui.custom_emoji().emoji.is_empty());
  let earlier = bob_ui
    .messages(t.channel, 0, 20)
    .into_iter()
    .find(|message| message.body == ":party:")
    .unwrap();
  assert_eq!(
    earlier.custom_emoji,
    [party.reference()],
    "the message names what it named when sent"
  );
  assert_eq!(
    bob_ui.custom_emoji_picture(party.sha256),
    Some(picture),
    "a device that held the picture keeps showing it"
  );
}

#[tokio::test]
async fn emoji_signed_by_somebody_the_roster_does_not_name_are_refused() {
  let mut t = team().await;
  // The server takes it from Carol, as it would from a forged admin.
  t.carol
    .client
    .add_custom_emoji("forged".into(), png(16))
    .await
    .unwrap();
  t.alice
    .client
    .add_custom_emoji("real".into(), png(24))
    .await
    .unwrap();
  settle(&mut [&mut t.alice, &mut t.bob, &mut t.carol]).await;
  let bob_ui = backend_of(&t.bob);
  bob_ui.want_custom_emoji();
  run_commands(&mut t.bob, &bob_ui).await;
  let shown: Vec<String> = bob_ui
    .custom_emoji()
    .emoji
    .into_iter()
    .map(|emoji| emoji.shortcode)
    .collect();
  assert_eq!(shown, ["real"]);
}

#[tokio::test]
async fn names_and_pictures_are_checked_before_anything_is_sent() {
  let mut t = team().await;
  for (shortcode, picture) in [
    ("Upper", png(16)),
    ("smile", png(16)),
    ("too_wide", png(129)),
    ("gif", b"GIF89a\x10\x00\x10\x00".to_vec()),
  ] {
    assert!(
      t.alice
        .client
        .add_custom_emoji(shortcode.into(), picture)
        .await
        .is_err(),
      "{shortcode}"
    );
  }
  t.alice
    .client
    .add_custom_emoji("ours".into(), png(16))
    .await
    .unwrap();
  assert!(
    t.alice
      .client
      .add_custom_emoji("ours".into(), png(17))
      .await
      .is_err(),
    "the name is taken"
  );
}
