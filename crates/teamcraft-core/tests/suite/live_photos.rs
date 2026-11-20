//! Live Photos and Motion Photos (issue #2181): a picture sent with its motion part, through the UI
//! adapter and the fake server. The server sees two attachments; receivers see one picture that
//! plays.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::io::Cursor;

use common::{Peer, backend_of, run_commands, settle};
use image::{ImageBuffer, ImageFormat, Rgb};
use teamcraft_client_api::actions::{
  FileFailure, FileInfo, FileSource, FileTransfer, MessagePriority, OutgoingFile, OutgoingMessage,
  OutgoingMotion,
};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::{Message, MessageKind};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_proto::{ConversationId, MotionLimits};

/// Stands in for the clip: the core never looks inside it.
const CLIP: &[u8] = b"\x00\x00\x00\x10ftypisom\x00\x00\x02\x00 a short clip";

#[tokio::test]
async fn a_live_photo_arrives_as_one_picture_that_plays() {
  let (server, mut alice, mut bob, channel) = two_in_a_channel(None).await;
  let (alice_ui, bob_ui) = (backend_of(&alice), backend_of(&bob));

  alice_ui.send_message(outgoing(
    channel,
    vec![live_photo("IMG_0001.HEIC", CLIP.to_vec())],
  ));
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  assert_eq!(server.attachment_ids().len(), 2, "the server sees two files");
  let received = last(&bob_ui, channel);
  let files = files(&bob_ui, channel, &received);
  assert_eq!(files.len(), 1, "the motion is no file of its own");
  let motion = files[0].maybe_motion.clone().expect("a live photo");
  assert_eq!(motion.media_type, "video/mp4");
  assert_eq!(motion.maybe_duration_ms, Some(2900));
  assert_eq!(motion.size_bytes, CLIP.len() as u64);
  assert_eq!(
    motion.transfer,
    FileTransfer::Remote,
    "not downloaded with the message"
  );
  assert_ne!(motion.file_id, files[0].file_id);

  let reference = bob_ui
    .attachment_ref(channel, received.id, &motion.file_id)
    .expect("the motion opens like a file of the message");
  assert_eq!(reference.file_name, "IMG_0001.mp4");
  let opened = bob.client.open_attachment(&reference).await.unwrap();
  assert_eq!(opened.expose_secret(), CLIP);

  // The viewer reads a motion part only up to the motion limit, whatever its sender claims.
  let (_, motion_cap) = bob_ui.viewed_file(channel, received.id, &motion.file_id).unwrap();
  assert_eq!(motion_cap, teamcraft_proto::MAX_MOTION_BYTES_LIMIT);
  let (_, picture_cap) = bob_ui
    .viewed_file(channel, received.id, &files[0].file_id)
    .unwrap();
  assert_eq!(picture_cap, teamcraft_core::MAX_VIEWER_BYTES);
  let mut claims_more = reference.clone();
  claims_more.size_bytes = motion_cap + 1;
  assert!(
    bob
      .client
      .start_capped_viewer_download(&claims_more, motion_cap)
      .await
      .is_err(),
    "a motion claiming more than the limit is not fetched"
  );

  let own = files_of_last(&alice_ui, channel);
  assert_eq!(own.len(), 1);
  assert!(own[0].is_live(), "the sender sees it as sent");
}

#[tokio::test]
async fn a_motion_over_the_limit_sends_the_picture_alone_and_says_so() {
  let limits = MotionLimits {
    is_allowed: true,
    max_bytes: teamcraft_proto::MIN_MOTION_BYTES_LIMIT,
  };
  let (server, mut alice, mut bob, channel) = two_in_a_channel(Some(limits)).await;
  let (alice_ui, bob_ui) = (backend_of(&alice), backend_of(&bob));
  let large = vec![7; teamcraft_proto::MIN_MOTION_BYTES_LIMIT as usize + 1];

  alice_ui.send_message(outgoing(channel, vec![live_photo("IMG_0002.jpg", large)]));
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  assert_eq!(server.attachment_ids().len(), 1, "only the picture went up");
  let received = files_of_last(&bob_ui, channel);
  assert_eq!(received.len(), 1);
  assert!(!received[0].is_live());
  let own = files_of_last(&alice_ui, channel);
  let failed: Vec<&FileInfo> = own
    .iter()
    .filter(|file| matches!(file.transfer, FileTransfer::UploadFailed { .. }))
    .collect();
  assert_eq!(failed.len(), 1, "the sender sees the motion as not sent");
  assert_eq!(failed[0].file_name, "IMG_0002.mp4");
  assert_eq!(
    failed[0].transfer,
    FileTransfer::UploadFailed {
      reason: FileFailure::TooLarge
    }
  );
}

#[tokio::test]
async fn with_motion_off_the_picture_goes_alone() {
  let limits = MotionLimits {
    is_allowed: false,
    ..MotionLimits::default()
  };
  let (server, mut alice, mut bob, channel) = two_in_a_channel(Some(limits)).await;
  let (alice_ui, bob_ui) = (backend_of(&alice), backend_of(&bob));
  assert_eq!(
    alice_ui.motion_limits(),
    limits,
    "the app asks before it converts"
  );

  alice_ui.send_message(outgoing(channel, vec![live_photo("IMG_0003.jpg", CLIP.to_vec())]));
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  assert_eq!(server.attachment_ids().len(), 1);
  assert!(!files_of_last(&bob_ui, channel)[0].is_live());
  assert_eq!(
    files_of_last(&alice_ui, channel).len(),
    1,
    "nothing failed: nothing was offered"
  );
}

#[tokio::test]
async fn a_motion_on_anything_but_a_picture_is_dropped() {
  let (server, mut alice, mut bob, channel) = two_in_a_channel(None).await;
  let (alice_ui, bob_ui) = (backend_of(&alice), backend_of(&bob));
  let mut document = live_photo("notes.txt", CLIP.to_vec());
  document.media_type = "text/plain".into();
  let mut not_a_video = live_photo("IMG_0004.jpg", CLIP.to_vec());
  if let Some(motion) = not_a_video.maybe_motion.as_mut() {
    motion.media_type = "text/html".into();
  }

  alice_ui.send_message(outgoing(channel, vec![document, not_a_video]));
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  assert_eq!(server.attachment_ids().len(), 2, "the two files, no motion");
  let received = files_of_last(&bob_ui, channel);
  assert_eq!(received.len(), 2);
  assert!(received.iter().all(|file| !file.is_live()));
}

#[tokio::test]
async fn cancelling_a_live_photo_cancels_its_motion_too() {
  let (_server, mut alice, mut bob, channel) = two_in_a_channel(None).await;
  let alice_ui = backend_of(&alice);
  alice_ui.send_message(OutgoingMessage {
    body: "the site".into(),
    ..outgoing(channel, vec![live_photo("IMG_0005.jpg", CLIP.to_vec())])
  });
  let pending = last(&alice_ui, channel);
  let listed = files(&alice_ui, channel, &pending);
  assert_eq!(listed.len(), 1, "the pending message lists the picture only");
  alice_ui.cancel_upload(channel, pending.id, &listed[0].file_id);
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  let sent = last(&backend_of(&bob), channel);
  assert_eq!(sent.body, "the site");
  assert!(
    files(&backend_of(&bob), channel, &sent).is_empty(),
    "neither the picture nor its motion"
  );
}

async fn two_in_a_channel(maybe_limits: Option<MotionLimits>) -> (FakeServer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  if let Some(limits) = maybe_limits {
    server.set_motion_limits(limits);
  }
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "pictures")
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

fn live_photo(name: &str, clip: Vec<u8>) -> OutgoingFile {
  let picture = ImageBuffer::from_fn(64, 48, |x, y| Rgb([x as u8 * 4, y as u8 * 5, 9]));
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
    maybe_waveform: None,
    maybe_draft_id: None,
    maybe_motion: Some(OutgoingMotion {
      media_type: "video/mp4".into(),
      source: FileSource::Bytes { bytes: clip },
      maybe_duration_ms: Some(2900),
    }),
  }
}

fn outgoing(conversation: ConversationId, files: Vec<OutgoingFile>) -> OutgoingMessage {
  OutgoingMessage {
    maybe_client_message_id: None,
    conversation,
    maybe_thread: None,
    body: String::new(),
    maybe_reply_to: None,
    maybe_reply_attachment: None,
    priority: MessagePriority::Normal,
    wants_acknowledgement: false,
    is_silent: false,
    also_send_to_channel: false,
    files,
    link_preview: Default::default(),
  }
}

fn files(backend: &impl ChatBackend, conversation: ConversationId, message: &Message) -> Vec<FileInfo> {
  backend
    .message_extras(conversation, &[message.id])
    .remove(0)
    .files
}

fn files_of_last(backend: &impl ChatBackend, conversation: ConversationId) -> Vec<FileInfo> {
  files(backend, conversation, &last(backend, conversation))
}

fn last(backend: &impl ChatBackend, conversation: ConversationId) -> Message {
  backend
    .messages(conversation, 0, 100)
    .into_iter()
    .rfind(|message| message.kind == MessageKind::User)
    .unwrap()
}
