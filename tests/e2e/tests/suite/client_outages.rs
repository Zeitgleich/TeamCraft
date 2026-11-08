//! Fault injection seen from a client (issue #743): a real core client against the real server while
//! the object storage is cut off behind a relay. The server side of the same outages is in
//! `dependency_outages.rs`; here the assertions are about what the user sees: the app stays usable,
//! a file does not retry endlessly but stops with a cause the user can act on, and the retry the
//! user asks for goes through once the storage is back.

use std::sync::Arc;
use std::time::{Duration, Instant};

use teamcraft_core::ClientSettings;
use teamcraft_core::client::OutgoingFile;
use teamcraft_core::client::files::{
  UploadRetrySettings, attachments_of, pending_upload, retry_stalled_upload,
};
use teamcraft_core::retry::JitterMode;
use teamcraft_core::store::{LocalStore, MemoryStore, MessageContent, MessageFilter};
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack};

/// Breakers probe again after a second, so recovery is quick to observe.
const OPEN_SECS: &str = "1";
/// Failed attempts after which a file waits for the user (the client's default is 6).
const ATTEMPTS: u32 = 3;
/// How long recovery may take once the dependency is back.
const RECOVERY: Duration = Duration::from_secs(30);
/// The longest the client may take to stop retrying: far more than the waits add up to, far less
/// than "endless".
const STALL_WITHIN: Duration = Duration::from_secs(60);

mod storage_outage {
  use super::*;

  /// The storage goes away while a file is on its way: other conversations keep working, the file
  /// stops retrying after its attempts (it names the cause and waits for the user), the pending
  /// message is not lost, and "Retry" sends it once the storage is back.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn a_file_stops_retrying_with_a_cause_and_goes_out_on_retry_while_chat_goes_on() {
    let Some(mut stack) = Stack::builder()
      .with_storage_relay()
      .with_env("TEAMCRAFT_DEPENDENCIES__OPEN_SECS", OPEN_SECS)
      .start()
      .await
    else {
      return;
    };
    let settings = ClientSettings {
      upload_retry: UploadRetrySettings {
        retry_after: Duration::from_millis(100),
        retry_max: Duration::from_millis(500),
        max_attempts: ATTEMPTS,
        stalled_retry_every: Duration::from_secs(300),
        jitter: JitterMode::Equal,
      },
      ..ClientSettings::default()
    };
    let mut alice = Peer::enroll_with_settings(
      &stack,
      "alice",
      "alice e2e device",
      Arc::new(MemoryStore::new()),
      settings,
    )
    .await;
    let mut carol = Peer::enroll(&stack, "carol").await;
    let space = space_with(&stack, &alice, "Project A", &[(&carol, "member")]).await;
    let files = channel_with(&mut alice, space, "files", &mut [&mut carol]).await;
    let general = channel_with(&mut alice, space, "general", &mut [&mut carol]).await;

    stack.storage_relay().cut();
    let content = b"quarterly numbers".to_vec();
    let started = Instant::now();
    // The message is stored at once; the upload failing is a state of the file, not an error the
    // user has to deal with right now.
    let message = alice
      .client
      .send_files(
        files,
        None,
        "report",
        vec![OutgoingFile {
          file_name: "numbers.txt".into(),
          media_type: "text/plain".into(),
          content: content.clone(),
        }],
      )
      .await;
    eprintln!(
      "PERF storage down: send_files answered after {:?}",
      started.elapsed()
    );
    assert!(
      started.elapsed() < Duration::from_secs(30),
      "the first attempt hung for {:?}",
      started.elapsed()
    );
    // The call reports the 503 (a host shows the file as waiting); the message itself is kept in
    // the outbox with its pending upload.
    assert!(message.is_err(), "the storage is down: {message:?}");
    let message = alice
      .store
      .messages(files, MessageFilter::All)
      .unwrap()
      .into_iter()
      .find(|stored| stored.maybe_server_id.is_none())
      .expect("the message waits in the outbox")
      .client_message_id;

    // Chat elsewhere is unaffected by the storage.
    alice
      .client
      .send_text(general, None, "still here")
      .await
      .expect("text does not depend on the storage");
    carol.wait_for_text(general, "still here").await;

    // The client retries on its backoff and then gives the file to the user.
    let cause = loop {
      let _ = alice.client.upload_pending().await;
      let upload = pending_upload(&alice.store, files, message)
        .unwrap()
        .expect("the pending upload is kept");
      if let Some(cause) = upload.files[0].retry.stall_cause() {
        break cause;
      }
      assert!(
        started.elapsed() < STALL_WITHIN,
        "still retrying after {:?}: {:?}",
        started.elapsed(),
        upload.files[0].retry
      );
      tokio::time::sleep(Duration::from_millis(50)).await;
    };
    eprintln!(
      "PERF storage down: file stalled ({cause:?}) after {:?}",
      started.elapsed()
    );

    // Stalled means quiet: no further attempt and nothing counted until the user asks.
    let stalled = pending_upload(&alice.store, files, message)
      .unwrap()
      .unwrap()
      .files[0]
      .retry
      .clone();
    assert_eq!(alice.client.upload_pending().await.unwrap(), 0);
    assert_eq!(
      pending_upload(&alice.store, files, message)
        .unwrap()
        .unwrap()
        .files[0]
        .retry,
      stalled
    );
    assert!(stack.server().is_running());

    // The storage is back: the user's Retry sends the message with its file.
    stack.storage_relay().restore();
    assert!(retry_stalled_upload(&alice.store, files, message).unwrap());
    let deadline = Instant::now() + STALL_WITHIN;
    loop {
      let _ = alice.client.upload_pending().await;
      if alice.client.flush_outbox().await.is_ok()
        && pending_upload(&alice.store, files, message).unwrap().is_none()
      {
        break;
      }
      assert!(
        Instant::now() < deadline,
        "the file did not go out after the storage came back"
      );
      tokio::time::sleep(Duration::from_millis(250)).await;
    }
    carol
      .wait_until("the file message arrives", |peer| {
        peer.store.message(files, message).unwrap().is_some()
      })
      .await;
    let received = carol.store.message(files, message).unwrap().unwrap();
    let attachment = attachments_of(&received.content)[0].clone();
    assert_eq!(
      carol
        .client
        .open_attachment(&attachment)
        .await
        .unwrap()
        .expose_secret(),
      content.as_slice()
    );
    eprintln!(
      "PERF storage down: file delivered {:?} after the cut",
      started.elapsed()
    );
    stack.stop().await;
  }
}

mod database_outage {
  use super::*;

  /// Postgres goes away: a message sent meanwhile is kept in the outbox (not lost, not shown as
  /// sent), the failure comes back at once instead of hanging, and once Postgres is back the same
  /// message is delivered exactly once.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn a_message_sent_while_postgres_is_down_waits_in_the_outbox_and_arrives_once() {
    let Some(mut stack) = Stack::builder().with_database_relay().start().await else {
      return;
    };
    let mut alice = Peer::enroll(&stack, "alice").await;
    let mut carol = Peer::enroll(&stack, "carol").await;
    let space = space_with(&stack, &alice, "Project A", &[(&carol, "member")]).await;
    let channel = channel_with(&mut alice, space, "general", &mut [&mut carol]).await;

    stack.database_relay().cut();
    let started = Instant::now();
    let sent = alice
      .client
      .send_text(channel, None, "written during the outage")
      .await;
    eprintln!("PERF postgres down: send answered after {:?}", started.elapsed());
    assert!(
      started.elapsed() < Duration::from_secs(10),
      "the send hung for {:?}",
      started.elapsed()
    );
    let stored = alice
      .store
      .messages(channel, MessageFilter::All)
      .unwrap()
      .into_iter()
      .find(|message| {
        matches!(&message.content, MessageContent::Text { body, .. } if body == "written during the outage")
      })
      .unwrap_or_else(|| panic!("the message is kept locally ({sent:?})"));
    assert!(stored.maybe_server_id.is_none(), "not shown as sent");

    stack.database_relay().restore();
    let deadline = Instant::now() + RECOVERY;
    loop {
      if alice.client.flush_outbox().await.is_ok() {
        break;
      }
      assert!(
        Instant::now() < deadline,
        "the outbox did not flush after Postgres came back"
      );
      tokio::time::sleep(Duration::from_millis(250)).await;
    }
    carol.wait_for_text(channel, "written during the outage").await;
    let copies = carol
      .texts(channel)
      .iter()
      .filter(|text| text.as_str() == "written during the outage")
      .count();
    assert_eq!(copies, 1, "a retried send must not duplicate");
    eprintln!(
      "PERF postgres down: delivered {:?} after the cut",
      started.elapsed()
    );
    stack.stop().await;
  }
}
