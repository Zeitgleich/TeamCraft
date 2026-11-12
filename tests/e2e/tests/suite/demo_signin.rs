//! The demo instance's reviewer sign-in (issue #1743; docs/operations.md, "Demo instance"): the
//! seed of `demo-seed` played on a test server, the database snapshot and restore of
//! `dev/demo/demo-reset.sh`, then a new device of the first reviewer signs in, restores the account
//! from the shared passphrase, as a store reviewer's phone does, and reads a seeded message, a
//! direct message and a file. Nobody else is online then, as on the demo instance after a reset:
//! the keys come from the key backup alone.
//!
//! The dev realm has four people with a member role, so `bob` plays both `alex` and `robin` and
//! `erin` is the only reviewer. Needs the dev stack's Postgres container for `pg_dump` and
//! `pg_restore` (as `scripts/e2e.sh` starts it).

use reqwest::StatusCode;
use teamcraft_core::store::{LocalStore, MessageContent, MessageFilter};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_e2e::demo::{self, ALEX_TO_REVIEWER, Cast, MEETING_NOTES, SPACE_NAME, WELCOME};
use teamcraft_e2e::peer::PASSPHRASE;
use teamcraft_e2e::{Peer, Stack};

/// Made after the snapshot: the restore must take it away.
const AFTER_SNAPSHOT_SPACE: &str = "Created after the snapshot";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reviewers_new_device_reads_the_seed_after_a_reset() {
  let Some(mut stack) = Stack::builder()
    // Seeding writes far faster than a person; the rate limits would throttle it.
    .with_env("TEAMCRAFT_SECURITY__RATE_LIMITS__IS_ENABLED", "false")
    .start()
    .await
  else {
    return;
  };
  if let Err(reason) = demo::check_postgres_tools().await {
    let is_required = std::env::var("E2E_REQUIRED").is_ok_and(|value| value == "1");
    assert!(
      !is_required,
      "the dev stack is required (E2E_REQUIRED=1): {reason}"
    );
    eprintln!("skipping: {reason}");
    stack.stop().await;
    return;
  }
  let cast = Cast::dev();
  let reviewer = cast.reviewers[0].clone();

  // The apps' own key stretching, as on the demo instance: the phone opens the backup with it.
  let seeded = demo::seed(&stack, &cast, PASSPHRASE, Argon2Settings::default()).await;
  let general = seeded.general;
  let (from_alex, _) = seeded.reviewer_dms[0];
  let seeded_identity = seeded
    .peers
    .iter()
    .find(|peer| peer.name == reviewer)
    .expect("the reviewer's seed device")
    .client
    .identity_fingerprint();
  // The seeding run ends: none of its devices is online after the reset.
  drop(seeded);

  let dump = demo::snapshot(&mut stack).await;
  let owner = stack.tokens(&cast.owner);
  stack
    .create_space(&owner, AFTER_SNAPSHOT_SPACE, "invite_only")
    .await;
  demo::restore(&mut stack, &dump).await;

  let spaces = stack.get(&owner, "/spaces").await.expect(StatusCode::OK);
  let listed = spaces.body.to_string();
  assert!(listed.contains(SPACE_NAME), "the seeded space is back: {listed}");
  assert!(
    !listed.contains(AFTER_SNAPSHOT_SPACE),
    "the restore took the later space away: {listed}"
  );

  let mut phone = demo::enroll_device(
    &stack,
    &reviewer,
    "Reviewer phone",
    PASSPHRASE,
    Argon2Settings::default(),
  )
  .await;
  assert_eq!(
    phone.client.identity_fingerprint(),
    seeded_identity,
    "the new device recovered the seeded identity instead of making a new one"
  );
  phone.wait_for_text(general, WELCOME).await;
  phone.wait_for_text(from_alex, ALEX_TO_REVIEWER).await;
  phone
    .wait_until("the meeting notes arrive in #general", |peer| {
      meeting_notes(peer, general).is_some()
    })
    .await;
  let notes = meeting_notes(&phone, general).expect("the meeting notes");
  let content = phone
    .client
    .fetch_file(general, notes)
    .await
    .unwrap_or_else(|error| panic!("the reviewer downloads {}: {error}", MEETING_NOTES.name));
  assert_eq!(content.expose_secret(), MEETING_NOTES.text.as_bytes());

  stack.stop().await;
}

/// The message of `#general` that holds the seeded meeting notes, once it is decrypted.
fn meeting_notes(
  peer: &Peer,
  general: teamcraft_proto::ConversationId,
) -> Option<teamcraft_proto::ClientMessageId> {
  peer
    .store
    .messages(general, MessageFilter::TopLevel)
    .unwrap()
    .into_iter()
    .find(|message| {
      matches!(&message.content, MessageContent::File { attachment } if attachment.file_name == MEETING_NOTES.name)
    })
    .map(|message| message.client_message_id)
}
