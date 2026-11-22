//! Sticker packs of the workspace (issue #2132, e2ee.md §4.14): an admin's device signs each
//! version of a pack, the members' devices keep only what an admin of their verified roster signed,
//! and a sent sticker names its picture inside the ciphertext with a text fallback, so changes to
//! the pack leave earlier messages alone.
#![cfg(feature = "ui")]
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, backend_of, run_commands, settle};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::workspace::RosterRole;
use teamcraft_proto::{ConversationId, StickerPackChange};

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
async fn an_admins_sticker_reaches_members_with_a_fallback() {
  let mut t = team().await;
  let picture = png(256);
  let pack = t.alice.client.create_sticker_pack("Team".into()).await.unwrap();
  t.alice
    .client
    .add_sticker(pack, "wave".into(), "Waving hand".into(), picture.clone())
    .await
    .unwrap();
  settle(&mut [&mut t.alice, &mut t.bob]).await;
  let (alice_ui, bob_ui) = (backend_of(&t.alice), backend_of(&t.bob));
  // Nothing is read until the UI needs the packs.
  assert!(bob_ui.sticker_packs().packs.is_empty());
  bob_ui.want_sticker_packs();
  run_commands(&mut t.bob, &bob_ui).await;

  let set = bob_ui.sticker_packs();
  assert_eq!(set.packs.len(), 1);
  let team_pack = set.packs[0].clone();
  assert_eq!((team_pack.name.as_str(), team_pack.version), ("Team", 2));
  assert_eq!(team_pack.signed_by, t.alice.client.user_id());
  let wave = team_pack.stickers[0].clone();
  assert_eq!(
    bob_ui.sticker_picture(wave.sha256),
    Some(picture.clone()),
    "the picture was downloaded and checked against the signed hash"
  );

  alice_ui.post_sticker(
    t.channel,
    None,
    wave.reference(pack),
    "[Sticker: Waving hand]".into(),
  );
  run_commands(&mut t.alice, &alice_ui).await;
  settle(&mut [&mut t.alice, &mut t.bob]).await;
  let received = bob_ui
    .messages(t.channel, 0, 20)
    .into_iter()
    .find(|message| message.maybe_sticker.is_some())
    .unwrap();
  assert_eq!(
    received.body, "[Sticker: Waving hand]",
    "older apps show the text"
  );
  assert_eq!(received.maybe_sticker.as_deref(), Some(&wave.reference(pack)));
}

#[tokio::test]
async fn changes_are_new_versions_and_leave_earlier_messages_alone() {
  let mut t = team().await;
  let pack = t.alice.client.create_sticker_pack("Team".into()).await.unwrap();
  for (name, side) in [("wave", 64), ("ok", 65), ("party", 66)] {
    t.alice
      .client
      .add_sticker(pack, name.into(), format!("{name} sticker"), png(side))
      .await
      .unwrap();
  }
  let alice_ui = backend_of(&t.alice);
  let wave = alice_ui.sticker_packs().packs[0].stickers[0].clone();
  alice_ui.post_sticker(
    t.channel,
    None,
    wave.reference(pack),
    "[Sticker: wave sticker]".into(),
  );
  run_commands(&mut t.alice, &alice_ui).await;

  for change in [
    StickerPackChange::Move {
      name: "party".into(),
      to: 0,
    },
    StickerPackChange::Edit {
      name: "ok".into(),
      new_name: "thumbs_up".into(),
      alt: "Thumbs up".into(),
    },
    StickerPackChange::Remove { name: "wave".into() },
    StickerPackChange::Rename {
      name: "Our team".into(),
    },
  ] {
    t.alice.client.change_sticker_pack(pack, change).await.unwrap();
  }
  settle(&mut [&mut t.alice, &mut t.bob]).await;
  let bob_ui = backend_of(&t.bob);
  bob_ui.want_sticker_packs();
  run_commands(&mut t.bob, &bob_ui).await;
  let changed = bob_ui.sticker_packs().packs[0].clone();
  assert_eq!(changed.name, "Our team");
  assert_eq!(changed.version, 8);
  let names: Vec<&str> = changed
    .stickers
    .iter()
    .map(|sticker| sticker.name.as_str())
    .collect();
  assert_eq!(names, ["party", "thumbs_up"]);
  assert_eq!(changed.stickers[1].alt, "Thumbs up");

  let earlier = bob_ui
    .messages(t.channel, 0, 20)
    .into_iter()
    .find(|message| message.maybe_sticker.is_some())
    .unwrap();
  assert_eq!(
    earlier.maybe_sticker.as_deref(),
    Some(&wave.reference(pack)),
    "the message names what it named when sent"
  );

  t.alice.client.delete_sticker_pack(pack).await.unwrap();
  settle(&mut [&mut t.alice, &mut t.bob]).await;
  bob_ui.want_sticker_packs();
  run_commands(&mut t.bob, &bob_ui).await;
  assert!(bob_ui.sticker_packs().packs.is_empty());
}

#[tokio::test]
async fn packs_signed_by_somebody_the_roster_does_not_name_are_refused() {
  let mut t = team().await;
  // The server takes it from Carol, as it would from a forged admin.
  t.carol.client.create_sticker_pack("Forged".into()).await.unwrap();
  t.alice.client.create_sticker_pack("Real".into()).await.unwrap();
  settle(&mut [&mut t.alice, &mut t.bob, &mut t.carol]).await;
  let bob_ui = backend_of(&t.bob);
  bob_ui.want_sticker_packs();
  run_commands(&mut t.bob, &bob_ui).await;
  let shown: Vec<String> = bob_ui
    .sticker_packs()
    .packs
    .into_iter()
    .map(|pack| pack.name)
    .collect();
  assert_eq!(shown, ["Real"]);
}

#[tokio::test]
async fn names_alt_texts_and_pictures_are_checked_before_anything_is_sent() {
  let mut t = team().await;
  let pack = t.alice.client.create_sticker_pack("Team".into()).await.unwrap();
  for (name, alt, picture) in [
    ("Upper", "alt", png(16)),
    ("wave", " ", png(16)),
    ("too_wide", "alt", png(513)),
    ("gif", "alt", b"GIF89a\x10\x00\x10\x00".to_vec()),
  ] {
    assert!(
      t.alice
        .client
        .add_sticker(pack, name.into(), alt.into(), picture)
        .await
        .is_err(),
      "{name}"
    );
  }
  t.alice
    .client
    .add_sticker(pack, "wave".into(), "Waving".into(), png(16))
    .await
    .unwrap();
  assert!(
    t.alice
      .client
      .add_sticker(pack, "wave".into(), "Again".into(), png(17))
      .await
      .is_err(),
    "two stickers of one name"
  );
  assert!(t.alice.client.create_sticker_pack(" ".into()).await.is_err());
}
