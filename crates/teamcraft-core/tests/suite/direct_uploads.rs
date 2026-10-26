//! Files uploaded at once, outside the outbox (the integration bridge's webhooks with files): they
//! go up like the outbox's, the message follows only once all are up, and a failed one is given up
//! at the server.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, settle};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::direct_uploads::{DirectUpload, DirectUploadError};
use teamcraft_core::client::files::UploadSource;
use teamcraft_core::client::upload_parts::PART_CHUNKS;
use teamcraft_core::store::{AttachmentRef, LocalStore, MessageContent, MessageFilter};
use teamcraft_crypto::attachment::DEFAULT_CHUNK_SIZE;
use teamcraft_proto::{Base64Bytes, ConversationId};

/// Plaintext bytes per part.
const PART: usize = PART_CHUNKS as usize * DEFAULT_CHUNK_SIZE as usize;

#[tokio::test]
async fn files_go_up_first_and_arrive_with_their_message() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let small = b"build log".to_vec();
  let large = sample(PART + 1234);
  let path = scratch_file("artifact.tar", &large);

  let mut uploads = vec![
    direct(
      channel,
      bytes_source(&small),
      "build.log",
      "text/plain",
      small.len(),
    ),
    direct(
      channel,
      path_source(&path),
      "artifact.tar",
      "application/x-tar",
      large.len(),
    ),
  ];
  let mut attachments = Vec::new();
  for upload in &mut uploads {
    attachments.push(upload_whole(&mut alice, upload).await);
  }
  assert_eq!(server.part_upload_count(), 2, "the large file went up in parts");
  assert!(
    alice.texts(channel).is_empty(),
    "nothing is sent while files go up"
  );

  alice
    .client
    .queue_text_with_files(channel, "**ci**\nBuild 42", Vec::new(), attachments)
    .unwrap();
  alice.client.flush_outbox().await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  let received = files_of(&bob, channel);
  assert_eq!(bob.texts(channel), ["**ci**\nBuild 42"]);
  assert_eq!(
    received
      .iter()
      .map(|file| file.file_name.as_str())
      .collect::<Vec<_>>(),
    ["build.log", "artifact.tar"]
  );
  assert_eq!(
    bob
      .client
      .open_attachment(&received[0])
      .await
      .unwrap()
      .expose_secret(),
    small
  );
  assert_eq!(
    bob
      .client
      .open_attachment(&received[1])
      .await
      .unwrap()
      .expose_secret(),
    large
  );
  std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[tokio::test]
async fn a_picture_gets_its_thumbnail_and_size() {
  let (_server, mut alice, _bob, channel) = channel_with_two().await;
  let png = png(40, 20);
  let mut upload = direct(channel, bytes_source(&png), "graph.png", "image/png", png.len());
  let attachment = upload_whole(&mut alice, &mut upload).await;
  assert!(attachment.maybe_thumbnail.is_some());
  let dimensions = attachment.maybe_dimensions.unwrap();
  assert_eq!((dimensions.width, dimensions.height), (40, 20));
}

#[tokio::test]
async fn a_file_over_the_server_limit_is_refused_before_anything_is_stored() {
  let (server, mut alice, _bob, channel) = channel_with_two().await;
  server.set_max_attachment_bytes(1024);
  let content = sample(4096);
  let mut upload = direct(
    channel,
    bytes_source(&content),
    "big.bin",
    "application/octet-stream",
    content.len(),
  );
  assert!(matches!(
    alice.client.start_direct_upload(&mut upload).await,
    Err(DirectUploadError::TooLarge)
  ));
  alice.client.abandon_direct_upload(&upload).await;
  assert_eq!(server.attachment_count(), 0);
}

#[tokio::test]
async fn an_upload_that_fails_half_way_is_given_up_at_the_server() {
  let (server, mut alice, _bob, channel) = channel_with_two().await;
  let content = sample(2 * PART + 10);
  let mut upload = direct(
    channel,
    bytes_source(&content),
    "video.mp4",
    "video/mp4",
    content.len(),
  );
  let mut step = alice.client.start_direct_upload(&mut upload).await.unwrap();
  let sent = step.sent().await;
  assert!(
    alice
      .client
      .finish_direct_upload(&mut upload, step, sent)
      .await
      .unwrap()
      .is_none(),
    "the first part is stored"
  );

  server.refuse_blob_uploads(alice.client.user_id(), true);
  let mut step = alice.client.start_direct_upload(&mut upload).await.unwrap();
  let sent = step.sent().await;
  assert!(matches!(
    alice.client.finish_direct_upload(&mut upload, step, sent).await,
    Err(DirectUploadError::Failed(_))
  ));
  alice.client.abandon_direct_upload(&upload).await;
  assert_eq!(server.abandoned_attachments().len(), 1);
  assert_eq!(server.attachment_count(), 0, "its parts are freed");
  assert!(alice.texts(channel).is_empty());
}

/// Drives the upload's steps until the file is up.
async fn upload_whole(peer: &mut Peer, upload: &mut DirectUpload) -> AttachmentRef {
  loop {
    let mut step = peer.client.start_direct_upload(upload).await.unwrap();
    let sent = step.sent().await;
    if let Some(attachment) = peer
      .client
      .finish_direct_upload(upload, step, sent)
      .await
      .unwrap()
    {
      return attachment;
    }
  }
}

async fn channel_with_two() -> (FakeServer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "ci")
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

fn direct(
  conversation: ConversationId,
  source: UploadSource,
  file_name: &str,
  media_type: &str,
  size: usize,
) -> DirectUpload {
  DirectUpload::new(
    conversation,
    source,
    file_name.into(),
    media_type.into(),
    size as u64,
  )
}

fn files_of(peer: &Peer, conversation: ConversationId) -> Vec<AttachmentRef> {
  peer
    .store
    .messages(conversation, MessageFilter::TopLevel)
    .unwrap()
    .into_iter()
    .find_map(|message| match message.content {
      MessageContent::Text { meta, .. } if !meta.attachments.is_empty() => Some(meta.attachments),
      _ => None,
    })
    .unwrap()
}

fn bytes_source(content: &[u8]) -> UploadSource {
  UploadSource::Bytes {
    bytes: Base64Bytes::new(content.to_vec()),
  }
}

fn path_source(path: &std::path::Path) -> UploadSource {
  UploadSource::Path {
    path: path.to_string_lossy().into_owned(),
  }
}

fn sample(len: usize) -> Vec<u8> {
  (0..len).map(|index| (index % 251) as u8).collect()
}

fn scratch_file(name: &str, content: &[u8]) -> std::path::PathBuf {
  let directory =
    std::env::temp_dir().join(format!("teamcraft-direct-uploads-{}-{name}", std::process::id()));
  std::fs::create_dir_all(&directory).unwrap();
  let path = directory.join(name);
  std::fs::write(&path, content).unwrap();
  path
}

fn png(width: u32, height: u32) -> Vec<u8> {
  let image = image::RgbImage::from_pixel(width, height, image::Rgb([200, 40, 40]));
  let mut bytes = std::io::Cursor::new(Vec::new());
  image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
  bytes.into_inner()
}
