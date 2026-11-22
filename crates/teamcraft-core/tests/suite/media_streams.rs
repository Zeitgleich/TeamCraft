//! Videos that play while they stream (#200): the web and desktop players read a file in ranges,
//! and the core answers each from the encrypted blob without downloading the whole file.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::cell::RefCell;
use std::rc::Rc;

use common::{Peer, backend_of, run_commands, settle};
use teamcraft_client_api::actions::{FileSource, MessagePriority, OutgoingFile, OutgoingMessage};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::downloads::{Downloads, Finished};
use teamcraft_core::media_stream::{
  MAX_OPEN_STREAMS, MAX_READ_BYTES, MediaStreamToken, OpenedStream, StreamPart,
};
use teamcraft_core::store::AttachmentRef;
use teamcraft_proto::ConversationId;

/// Three reads' worth and a bit, not a multiple of the chunk size.
const VIDEO_BYTES: usize = 5 * 1024 * 1024 + 12_345;

#[tokio::test]
async fn a_video_streams_in_ranges_and_seeks() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let video = sample(VIDEO_BYTES);
  let reference = sent_file(&mut alice, &mut bob, channel, "clip.mp4", "video/mp4", &video).await;
  let mut downloads = Downloads::new(2);

  let opened = open(&mut downloads, &mut bob, reference).await.unwrap();
  assert_eq!(opened.size_bytes, VIDEO_BYTES as u64);
  assert_eq!(opened.media_type, "video/mp4");

  let first = read(&mut downloads, &mut bob, &opened.token, 0).await.unwrap();
  assert_eq!(first.bytes.expose_secret(), &video[..MAX_READ_BYTES as usize]);
  assert_eq!(first.total_bytes, VIDEO_BYTES as u64);
  // A seek near the end, off a chunk boundary: only the end of the file is read.
  let at = VIDEO_BYTES as u64 - 1000;
  let end = read(&mut downloads, &mut bob, &opened.token, at).await.unwrap();
  assert_eq!(end.start, at);
  assert_eq!(end.bytes.expose_secret(), &video[at as usize..]);
  let past = read(&mut downloads, &mut bob, &opened.token, VIDEO_BYTES as u64)
    .await
    .unwrap();
  assert!(past.bytes.is_empty(), "nothing past the end");

  let ranges = server.download_ranges();
  assert_eq!(ranges.len(), 3, "the header and two reads: {ranges:?}");
  let read_bytes: u64 = ranges.iter().map(|(start, end)| end - start).sum();
  assert!(
    read_bytes < VIDEO_BYTES as u64 / 2,
    "not the whole file: {read_bytes}"
  );
}

#[tokio::test]
async fn a_changed_blob_fails_to_read() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let reference = sent_file(
    &mut alice,
    &mut bob,
    channel,
    "clip.webm",
    "video/webm",
    &sample(3_000_000),
  )
  .await;
  let mut downloads = Downloads::new(2);
  let opened = open(&mut downloads, &mut bob, reference).await.unwrap();
  // The last byte of the blob changes: the read that covers the last chunk fails.
  server.tamper_blobs();
  assert!(
    read(&mut downloads, &mut bob, &opened.token, 0).await.is_ok(),
    "the start is unchanged"
  );
  assert!(
    read(&mut downloads, &mut bob, &opened.token, 2_999_000)
      .await
      .is_err()
  );
}

#[tokio::test]
async fn only_videos_stream() {
  let (_server, mut alice, mut bob, channel) = channel_with_two().await;
  let reference = sent_file(
    &mut alice,
    &mut bob,
    channel,
    "page.html",
    "text/html",
    b"<p>hi</p>",
  )
  .await;
  let mut downloads = Downloads::new(2);
  assert!(open(&mut downloads, &mut bob, reference).await.is_err());
}

#[tokio::test]
async fn closed_and_oldest_streams_no_longer_read() {
  let (_server, mut alice, mut bob, channel) = channel_with_two().await;
  let reference = sent_file(
    &mut alice,
    &mut bob,
    channel,
    "clip.mp4",
    "video/mp4",
    &sample(10_000),
  )
  .await;
  let mut downloads = Downloads::new(2);
  let first = open(&mut downloads, &mut bob, reference.clone()).await.unwrap();
  let mut later = Vec::new();
  for _ in 0..MAX_OPEN_STREAMS {
    later.push(open(&mut downloads, &mut bob, reference.clone()).await.unwrap());
  }
  assert!(
    read(&mut downloads, &mut bob, &first.token, 0).await.is_err(),
    "the oldest was closed for a new one"
  );
  let newest = later.pop().unwrap();
  assert!(read(&mut downloads, &mut bob, &newest.token, 0).await.is_ok());
  downloads.close_stream(&newest.token);
  assert!(read(&mut downloads, &mut bob, &newest.token, 0).await.is_err());
  assert!(MediaStreamToken::parse(newest.token.as_str()).is_some());
  assert!(MediaStreamToken::parse("../etc").is_none());
}

async fn open(
  downloads: &mut Downloads,
  bob: &mut Peer,
  reference: AttachmentRef,
) -> Result<OpenedStream, String> {
  let slot = Rc::new(RefCell::new(None));
  let sink = slot.clone();
  downloads.open_stream(
    reference,
    Box::new(move |opened| *sink.borrow_mut() = Some(opened)),
  );
  drive(downloads, bob).await;
  slot.borrow_mut().take().unwrap()
}

async fn read(
  downloads: &mut Downloads,
  bob: &mut Peer,
  token: &MediaStreamToken,
  start: u64,
) -> Result<StreamPart, String> {
  let slot = Rc::new(RefCell::new(None));
  let sink = slot.clone();
  downloads.read_stream(
    token.clone(),
    start,
    Box::new(move |part| *sink.borrow_mut() = Some(part)),
  );
  drive(downloads, bob).await;
  slot.borrow_mut().take().unwrap()
}

/// Starts what waits and runs it to the end, as a host's loop does.
async fn drive(downloads: &mut Downloads, bob: &mut Peer) {
  downloads.start_due(&mut bob.client).await;
  while downloads.is_running() {
    let finished = downloads.next_finished().await;
    assert!(matches!(finished, Finished::Viewed));
  }
}

async fn sent_file(
  alice: &mut Peer,
  bob: &mut Peer,
  channel: ConversationId,
  name: &str,
  media_type: &str,
  bytes: &[u8],
) -> AttachmentRef {
  let alice_ui = backend_of(alice);
  alice_ui.send_message(OutgoingMessage {
    maybe_client_message_id: None,
    conversation: channel,
    maybe_thread: None,
    body: String::new(),
    maybe_reply_to: None,
    maybe_reply_attachment: None,
    priority: MessagePriority::Normal,
    wants_acknowledgement: false,
    is_silent: false,
    also_send_to_channel: false,
    files: vec![OutgoingFile {
      file_name: name.into(),
      media_type: media_type.into(),
      source: FileSource::Bytes {
        bytes: bytes.to_vec(),
      },
      maybe_thumbnail: None,
      maybe_duration_ms: None,
      maybe_waveform: None,
      maybe_draft_id: None,
      maybe_motion: None,
    }],
    link_preview: Default::default(),
  });
  run_commands(alice, &alice_ui).await;
  settle(&mut [&mut *alice, &mut *bob]).await;
  let bob_ui = backend_of(bob);
  let count = bob_ui.message_count(channel);
  let received = bob_ui.messages(channel, count - 1, 1).remove(0);
  let files = bob_ui.message_extras(channel, &[received.id])[0].files.clone();
  bob_ui
    .attachment_ref(channel, received.id, &files[0].file_id)
    .unwrap()
}

async fn channel_with_two() -> (FakeServer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "videos")
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

fn sample(len: usize) -> Vec<u8> {
  (0..len).map(|index| (index % 251) as u8).collect()
}
