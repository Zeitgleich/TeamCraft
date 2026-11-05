//! Replying to and forwarding one picture of an album (issue #241), through the UI adapter and the
//! fake server.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::io::Cursor;

use common::{Peer, backend_of, run_commands, settle};
use image::{ImageBuffer, ImageFormat, Rgb};
use teamcraft_client_api::actions::{FileInfo, FileSource, MessagePriority, OutgoingFile, OutgoingMessage};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::{Message, MessageKind};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_proto::{AttachmentId, ConversationId};

#[tokio::test]
async fn a_reply_to_the_third_of_four_pictures_quotes_that_picture() {
  let (_server, mut alice, mut bob, channel) = two_in_a_channel().await;
  let (alice_ui, bob_ui) = (backend_of(&alice), backend_of(&bob));
  let (album, files) = send_album(&mut alice, &mut bob, channel, "the site").await;

  bob_ui.send_message(OutgoingMessage {
    maybe_reply_to: Some(album.id),
    maybe_reply_attachment: Some(attachment_id(&files[2])),
    ..outgoing(channel, "this one", Vec::new())
  });
  run_commands(&mut bob, &bob_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  let reply = last(&alice_ui, channel);
  let extras = alice_ui.message_extras(channel, &[reply.id]).remove(0);
  let quote = extras.maybe_reply.unwrap();
  assert_eq!(quote.maybe_message, Some(album.id));
  assert_eq!(quote.excerpt, "the site");
  let image = quote.maybe_attachment.unwrap();
  assert_eq!(image.file_id, files[2].file_id);
  assert_eq!(image.file_name, "c.jpg");
  assert_eq!(image.maybe_thumbnail_png, files[2].maybe_thumbnail_png);
  assert!(image.maybe_thumbnail_png.is_some());
  assert!(image.can_open);
}

#[tokio::test]
async fn the_quote_keeps_its_picture_after_the_album_is_deleted() {
  let (_server, mut alice, mut bob, channel) = two_in_a_channel().await;
  let (alice_ui, bob_ui) = (backend_of(&alice), backend_of(&bob));
  let (album, files) = send_album(&mut alice, &mut bob, channel, "").await;
  bob_ui.send_message(OutgoingMessage {
    maybe_reply_to: Some(album.id),
    maybe_reply_attachment: Some(attachment_id(&files[2])),
    ..outgoing(channel, "this one", Vec::new())
  });
  run_commands(&mut bob, &bob_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  let reply = last(&bob_ui, channel);

  alice_ui.delete_message(channel, album.id);
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  let quote = bob_ui
    .message_extras(channel, &[reply.id])
    .remove(0)
    .maybe_reply
    .unwrap();
  let image = quote.maybe_attachment.unwrap();
  assert_eq!(image.maybe_thumbnail_png, files[2].maybe_thumbnail_png);
  assert_eq!(image.file_name, "c.jpg");
  assert!(!image.can_open, "nothing to open any more");
}

#[tokio::test]
async fn a_forward_of_one_picture_shows_nothing_of_the_rest_of_the_album() {
  let (_server, mut alice, mut bob, channel) = two_in_a_channel().await;
  let (alice_ui, bob_ui) = (backend_of(&alice), backend_of(&bob));
  let other = other_channel(&mut alice, &mut bob).await;
  let (album, files) = send_album(&mut alice, &mut bob, channel, "the secret site").await;

  alice_ui.forward_attachment(channel, album.id, attachment_id(&files[2]), other, "look");
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  let forwarded = last(&bob_ui, other);
  assert_eq!(forwarded.body, "look");
  let extras = bob_ui.message_extras(other, &[forwarded.id]).remove(0);
  assert_eq!(extras.files.len(), 1);
  assert_eq!(extras.files[0].file_name, "c.jpg");
  assert_eq!(extras.maybe_forwarded.unwrap().author_name, "Alice");
}

#[tokio::test]
async fn a_forward_of_a_picture_that_is_gone_sends_nothing() {
  let (_server, mut alice, mut bob, channel) = two_in_a_channel().await;
  let alice_ui = backend_of(&alice);
  let other = other_channel(&mut alice, &mut bob).await;
  let (album, files) = send_album(&mut alice, &mut bob, channel, "the site").await;
  let before = alice_ui.messages(other, 0, 100).len();

  let gone = attachment_id(&files[0]);
  alice_ui.delete_message(channel, album.id);
  alice_ui.forward_attachment(channel, album.id, gone, other, "");
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  assert_eq!(alice_ui.messages(other, 0, 100).len(), before);
}

#[tokio::test]
async fn a_message_with_one_picture_is_replied_to_and_forwarded_as_before() {
  let (_server, mut alice, mut bob, channel) = two_in_a_channel().await;
  let (alice_ui, bob_ui) = (backend_of(&alice), backend_of(&bob));
  let other = other_channel(&mut alice, &mut bob).await;
  alice_ui.send_message(outgoing(channel, "the one", vec![picture("a.jpg")]));
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  let single = last(&bob_ui, channel);
  let file = bob_ui
    .message_extras(channel, &[single.id])
    .remove(0)
    .files
    .remove(0);

  bob_ui.send_message(OutgoingMessage {
    maybe_reply_to: Some(single.id),
    maybe_reply_attachment: Some(attachment_id(&file)),
    ..outgoing(channel, "nice", Vec::new())
  });
  bob_ui.forward_attachment(channel, single.id, attachment_id(&file), other, "");
  run_commands(&mut bob, &bob_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  let reply = last(&alice_ui, channel);
  let quote = alice_ui
    .message_extras(channel, &[reply.id])
    .remove(0)
    .maybe_reply
    .unwrap();
  assert_eq!(quote.maybe_attachment, None);
  assert_eq!(quote.excerpt, "the one");
  let forwarded = last(&alice_ui, other);
  assert_eq!(alice_ui.message_extras(other, &[forwarded.id])[0].files.len(), 1);
}

/// Alice sends four pictures; what Bob sees of them.
async fn send_album(
  alice: &mut Peer,
  bob: &mut Peer,
  channel: ConversationId,
  body: &str,
) -> (Message, Vec<FileInfo>) {
  let (alice_ui, bob_ui) = (backend_of(alice), backend_of(bob));
  let pictures = ["a.jpg", "b.jpg", "c.jpg", "d.jpg"].map(picture).to_vec();
  alice_ui.send_message(outgoing(channel, body, pictures));
  run_commands(alice, &alice_ui).await;
  settle(&mut [&mut *alice, &mut *bob]).await;
  let album = last(&bob_ui, channel);
  let files = bob_ui.message_extras(channel, &[album.id]).remove(0).files;
  assert_eq!(files.len(), 4);
  (album, files)
}

async fn two_in_a_channel() -> (FakeServer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = shared_channel(&mut alice, &mut bob, "pictures").await;
  (server, alice, bob, channel)
}

async fn other_channel(alice: &mut Peer, bob: &mut Peer) -> ConversationId {
  shared_channel(alice, bob, "elsewhere").await
}

async fn shared_channel(alice: &mut Peer, bob: &mut Peer, name: &str) -> ConversationId {
  let channel = alice
    .client
    .create_channel(alice.space().await, name)
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut *alice, &mut *bob]).await;
  channel
}

fn outgoing(conversation: ConversationId, body: &str, files: Vec<OutgoingFile>) -> OutgoingMessage {
  OutgoingMessage {
    maybe_client_message_id: None,
    conversation,
    maybe_thread: None,
    body: body.into(),
    maybe_reply_to: None,
    maybe_reply_attachment: None,
    priority: MessagePriority::Normal,
    wants_acknowledgement: false,
    is_silent: false,
    also_send_to_channel: false,
    files,
  }
}

fn picture(name: &str) -> OutgoingFile {
  let picture = ImageBuffer::from_fn(64, 48, |x, y| Rgb([x as u8 * 4, y as u8 * 5, name.len() as u8]));
  let mut bytes = Cursor::new(Vec::new());
  picture.write_to(&mut bytes, ImageFormat::Jpeg).unwrap();
  OutgoingFile {
    file_name: name.into(),
    media_type: "image/jpeg".into(),
    source: FileSource::Bytes {
      bytes: bytes.into_inner(),
    },
    maybe_thumbnail: None,
    maybe_duration_ms: None,
  }
}

fn attachment_id(file: &FileInfo) -> AttachmentId {
  file.file_id.parse().unwrap()
}

fn last(backend: &impl ChatBackend, conversation: ConversationId) -> Message {
  backend
    .messages(conversation, 0, 100)
    .into_iter()
    .rfind(|message| message.kind == MessageKind::User)
    .unwrap()
}
