//! "Report a problem" through the fake server (docs/bug-reports.md): a report goes as an encrypted
//! group DM to the recipients of the verified policy (the owners and admins by default), waits on
//! the device while offline, and arrives once however often delivery runs.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, backend_of, settle};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::bug_report::{BugReport, ReportScreenshot};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::BugReportError;
use teamcraft_core::client::bug_reports::{MAX_QUEUED_REPORTS, queued_reports};
use teamcraft_core::error::CoreError;
use teamcraft_core::pending_work::PendingWork;
use teamcraft_core::upkeep;
use teamcraft_core::workspace::RosterRole;
use teamcraft_proto::{
  Base64Bytes, BugReportPolicy, ClientMessageId, ConversationId, UserId, WorkspacePolicy,
};

const DESCRIPTION: &str = "The meeting drops my audio after a minute";
const REPORT_TEXT: &str = "TeamCraft problem report\n\nLog (1 lines, oldest first)\nWARN call: lost\n";

#[tokio::test]
async fn a_report_reaches_the_owners_encrypted_with_its_files() {
  let (server, mut alice, mut bob, _carol) = workspace(false).await;
  let report = report(1, true);
  bob.client.queue_bug_report(report.clone()).await.unwrap();
  upkeep::flush(&mut bob.client).await;
  settle(&mut [&mut alice, &mut bob]).await;

  let dm = only_dm(&alice, bob.client.user_id());
  assert_eq!(alice.texts(dm), [DESCRIPTION]);
  let alice_ui = backend_of(&alice);
  let received = alice_ui.messages(dm, 0, 1).remove(0);
  let files = alice_ui.message_extras(dm, &[received.id])[0].files.clone();
  assert_eq!(files.len(), 2);
  assert_eq!(files[0].file_name, report.file_name);
  let reference = alice_ui
    .attachment_ref(dm, received.id, &files[0].file_id)
    .unwrap();
  let opened = alice.client.open_attachment(&reference).await.unwrap();
  assert_eq!(opened.expose_secret(), REPORT_TEXT.as_bytes());
  // Named by its type, encoded again without metadata: the picked file's name doesn't travel.
  assert_eq!(files[1].file_name, "screenshot.png");

  // The server holds ciphertext only: neither the description nor the report text.
  for stored in server
    .stored_ciphertexts()
    .into_iter()
    .chain(server.stored_blobs())
  {
    assert!(!contains(&stored, DESCRIPTION.as_bytes()));
    assert!(!contains(&stored, b"TeamCraft problem report"));
  }
  assert!(queued_reports(bob.store.as_ref()).unwrap().is_empty());
}

#[tokio::test]
async fn the_policys_recipients_receive_it_instead_of_the_owners() {
  let (_server, mut alice, mut bob, mut carol) = workspace(true).await;
  bob.client.queue_bug_report(report(2, false)).await.unwrap();
  upkeep::flush(&mut bob.client).await;
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;

  let dm = only_dm(&carol, bob.client.user_id());
  assert_eq!(carol.texts(dm), [DESCRIPTION]);
  assert!(
    backend_of(&alice).direct_conversations().is_empty(),
    "the owner isn't a recipient"
  );
}

#[tokio::test]
async fn a_report_made_offline_waits_and_arrives_once() {
  let (server, mut alice, mut bob, _carol) = workspace(false).await;
  let bob_id = bob.client.user_id();
  server.set_offline(bob_id, true);
  let report = report(3, false);
  bob.client.queue_bug_report(report.clone()).await.unwrap();
  // Retried by the app after a lost answer: still one report.
  bob.client.queue_bug_report(report).await.unwrap();
  upkeep::flush(&mut bob.client).await;
  assert_eq!(PendingWork::read(&bob.store).unwrap().reports, 1);

  server.set_offline(bob_id, false);
  upkeep::flush(&mut bob.client).await;
  upkeep::flush(&mut bob.client).await;
  settle(&mut [&mut alice, &mut bob]).await;
  assert_eq!(PendingWork::read(&bob.store).unwrap().reports, 0);
  let dm = only_dm(&alice, bob_id);
  assert_eq!(alice.texts(dm), [DESCRIPTION], "delivered once");
}

/// A hostile server answers "the recipients' group DM" with a conversation of its choosing: a
/// channel, or a group DM with someone who isn't a recipient. Nothing goes there; the report waits
/// and the reporter is told.
mod hostile_server {
  use super::*;

  #[tokio::test]
  async fn a_channel_named_for_the_dm_gets_nothing() {
    let (server, mut alice, mut bob, mut carol) = workspace(false).await;
    let channel = alice
      .client
      .create_channel(alice.space().await, "general")
      .await
      .unwrap();
    for member in [bob.client.user_id(), carol.client.user_id()] {
      alice.client.add_member(channel, member).await.unwrap();
    }
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    server.redirect_group_dms_to(channel);

    bob.client.queue_bug_report(report(20, false)).await.unwrap();
    upkeep::flush(&mut bob.client).await;
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    assert_nothing_sent_to(&carol, channel);
    assert_waits_and_tells(&mut bob);
  }

  #[tokio::test]
  async fn a_group_dm_with_someone_else_gets_nothing() {
    let (server, mut alice, mut bob, mut carol) = workspace(false).await;
    // Bob's own group DM with Carol, who receives no reports (Alice, the owner, does).
    let other_dm = bob.client.open_group_dm(&[carol.client.user_id()]).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    server.redirect_group_dms_to(other_dm);

    bob.client.queue_bug_report(report(21, false)).await.unwrap();
    upkeep::flush(&mut bob.client).await;
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    assert_nothing_sent_to(&carol, other_dm);
    assert_waits_and_tells(&mut bob);
  }

  fn assert_nothing_sent_to(peer: &Peer, conversation: ConversationId) {
    assert!(
      !peer
        .texts(conversation)
        .iter()
        .any(|text| text.contains(DESCRIPTION)),
      "the report reached a conversation the server named"
    );
  }

  fn assert_waits_and_tells(reporter: &mut Peer) {
    assert_eq!(
      queued_reports(reporter.store.as_ref()).unwrap().len(),
      1,
      "it waits"
    );
    let failures = reporter.client.take_background_failures();
    assert!(
      failures
        .iter()
        .any(|failure| matches!(failure, CoreError::BugReport(BugReportError::WrongConversation))),
      "{failures:?}"
    );
  }
}

#[tokio::test]
async fn nobody_to_receive_it_refuses_the_report() {
  let (_server, mut alice, _bob, _carol) = workspace(false).await;
  // Alice is the only owner: a report of hers would go to nobody.
  let error = alice.client.queue_bug_report(report(4, false)).await.unwrap_err();
  assert!(
    matches!(error, CoreError::BugReport(BugReportError::NoRecipients)),
    "{error:?}"
  );
  assert!(queued_reports(alice.store.as_ref()).unwrap().is_empty());
}

/// The report crossed IPC or FFI: the core checks it again and refuses a malformed one.
#[tokio::test]
async fn a_malformed_report_is_refused() {
  let (_server, _alice, mut bob, _carol) = workspace(false).await;
  let mut empty = report(30, false);
  empty.description = "  ".to_owned();
  let mut not_a_picture = report(31, true);
  if let Some(screenshot) = not_a_picture.maybe_screenshot.as_mut() {
    screenshot.bytes = Base64Bytes::new(b"%PDF-1.7".to_vec());
  }
  for bad in [empty, not_a_picture] {
    let error = bob.client.queue_bug_report(bad).await.unwrap_err();
    assert!(
      matches!(error, CoreError::BugReport(BugReportError::Invalid(_))),
      "{error:?}"
    );
  }
  assert!(queued_reports(bob.store.as_ref()).unwrap().is_empty());
}

#[tokio::test]
async fn the_queue_is_bounded() {
  let (server, _alice, mut bob, _carol) = workspace(false).await;
  server.set_offline(bob.client.user_id(), true);
  for seed in 0..MAX_QUEUED_REPORTS {
    bob
      .client
      .queue_bug_report(report(10 + seed as u8, false))
      .await
      .unwrap();
  }
  let error = bob.client.queue_bug_report(report(99, false)).await.unwrap_err();
  assert!(
    matches!(error, CoreError::BugReport(BugReportError::QueueFull)),
    "{error:?}"
  );
}

/// Alice owns the workspace, Bob and Carol are members; with `carol_receives` the policy names
/// Carol as the one recipient.
async fn workspace(carol_receives: bool) -> (FakeServer, Peer, Peer, Peer) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let alice_id = alice.client.user_id();
  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner)])
    .await
    .unwrap();
  let recipients = if carol_receives {
    vec![carol.client.user_id()]
  } else {
    Vec::new()
  };
  alice
    .client
    .publish_policy(WorkspacePolicy {
      bug_reports: BugReportPolicy { recipients },
      ..WorkspacePolicy::default()
    })
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  for peer in [&mut alice, &mut bob, &mut carol] {
    peer.client.sync_workspace().await.unwrap();
  }
  (server, alice, bob, carol)
}

fn report(seed: u8, has_screenshot: bool) -> BugReport {
  BugReport {
    id: ClientMessageId::from_bytes([seed; 16]),
    description: format!("  {DESCRIPTION}\n"),
    report_text: REPORT_TEXT.to_owned(),
    file_name: "teamcraft-report-2026-11-07T12-00-00Z.txt".to_owned(),
    maybe_screenshot: has_screenshot.then(|| ReportScreenshot {
      file_name: "holiday at home.png".to_owned(),
      media_type: "image/png".to_owned(),
      bytes: Base64Bytes::new(png()),
    }),
  }
}

/// The one DM `peer` has with `with`.
fn only_dm(peer: &Peer, with: UserId) -> ConversationId {
  let dms: Vec<_> = backend_of(peer)
    .direct_conversations()
    .into_iter()
    .filter(|conversation| conversation.member_ids.contains(&with))
    .collect();
  assert_eq!(dms.len(), 1, "{dms:?}");
  dms[0].id
}

fn png() -> Vec<u8> {
  let picture = image::RgbImage::from_fn(8, 4, |x, _| image::Rgb([(x * 30) as u8, 10, 10]));
  let mut out = std::io::Cursor::new(Vec::new());
  picture.write_to(&mut out, image::ImageFormat::Png).unwrap();
  out.into_inner()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
  haystack.windows(needle.len()).any(|window| window == needle)
}
