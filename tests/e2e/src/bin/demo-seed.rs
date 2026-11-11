//! Seeds the demo instance for store reviews (docs/operations.md, "Demo instance"; issue #1628).
//!
//! Real clients do the work, so keys, MLS groups and attachments are what any app would make: the
//! seeding run signs in every demo user at the instance's identity provider, enrolls one device
//! each, signs the workspace policy, and fills a space, channels, direct messages and files. The
//! operator then takes the snapshot (`dev/demo/demo-reset.sh snapshot`) that the nightly reset
//! restores. Run it once against an empty instance:
//!
//! ```sh
//! E2E_DEPLOYED_URL=https://demo.example.com DEV_TOKEN_ISSUER=https://demo.example.com/dex \
//!   DEV_TOKEN_PASSWORD=... DEV_TOKEN_SCOPES="openid profile email groups" \
//!   DEMO_RECOVERY_PASSPHRASE=... \
//!   cargo run -p teamcraft-e2e --features e2e --bin demo-seed
//! ```
//!
//! Everything here is placeholder content: no real people, companies or hosts. To change the seed,
//! edit the constants and `seed` below, then run it again on an empty instance and take a new
//! snapshot.
// A one-shot operator tool built on the test harness: it reports on stderr and stops at the first
// failure, as the harness does.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use std::sync::Arc;
use std::time::Duration;

use teamcraft_core::client::OutgoingFile;
use teamcraft_core::store::MemoryStore;
use teamcraft_core::{ClientSettings, EnrollmentOptions, EnrollmentSecret};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::secret::SecretBytes;
use teamcraft_e2e::peer::settle;
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::{ConversationId, RetentionSetting};

/// The passphrase that opens every demo user's key backup: reviewers type it on their first
/// sign-in after a reset (operations.md, "Review instructions").
const PASSPHRASE_VAR: &str = "DEMO_RECOVERY_PASSPHRASE";
/// Appended to the user names below to make the logins of the identity provider (Dex signs in
/// with the e-mail address).
const LOGIN_SUFFIX_VAR: &str = "DEMO_LOGIN_SUFFIX";
const DEFAULT_LOGIN_SUFFIX: &str = "@example.com";
/// The owner signs the workspace policy and creates the space; it has the role `owner` at the
/// provider.
const OWNER: &str = "owner";
const MEMBERS: [&str; 3] = ["alex", "sam", "robin"];
/// One account per store, so two reviews at the same time don't meet.
const REVIEWERS: [&str; 2] = ["reviewer-1", "reviewer-2"];
const SPACE_NAME: &str = "Project A";
/// Messages kept per channel: the seed stays, the reviewers' own messages can't pile up between
/// resets. Counted, not dated, so the seed survives however old the snapshot gets.
const RETENTION_MAX_MESSAGES: u64 = 500;
const RETENTION_LIMIT_MESSAGES: u64 = 1000;
/// Time for welcomes, keys and messages in flight to reach everybody before the run ends.
const SETTLE: Duration = Duration::from_secs(5);

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
  let Some(stack) = Stack::deployed() else {
    std::process::exit(2);
  };
  let Ok(passphrase) = std::env::var(PASSPHRASE_VAR) else {
    eprintln!("{PASSPHRASE_VAR} is not set");
    std::process::exit(2);
  };
  let suffix = std::env::var(LOGIN_SUFFIX_VAR).unwrap_or_else(|_| DEFAULT_LOGIN_SUFFIX.to_owned());
  seed(&stack, &passphrase, &suffix).await;
  eprintln!("demo seed done: take the snapshot now (dev/demo/demo-reset.sh snapshot)");
}

async fn seed(stack: &Stack, passphrase: &str, suffix: &str) {
  let enroll_user = |user: &'static str| enroll(stack, user, suffix, passphrase);
  let mut owner = enroll_user(OWNER).await;
  owner
    .set_policy(|policy| {
      policy.retention.default = RetentionSetting {
        maybe_max_age_days: None,
        maybe_max_messages: Some(RETENTION_MAX_MESSAGES),
      };
      policy.retention.limit = RetentionSetting {
        maybe_max_age_days: None,
        maybe_max_messages: Some(RETENTION_LIMIT_MESSAGES),
      };
      policy.retention.keep_pinned = true;
      // Reviewers unlock with the shared passphrase; a recovery kit file is no use to them.
      policy.recovery.is_passphrase_allowed = true;
    })
    .await;
  eprintln!("signed the workspace policy");

  let mut alex = enroll_user(MEMBERS[0]).await;
  let mut sam = enroll_user(MEMBERS[1]).await;
  let mut robin = enroll_user(MEMBERS[2]).await;
  let mut first_reviewer = enroll_user(REVIEWERS[0]).await;
  let mut second_reviewer = enroll_user(REVIEWERS[1]).await;
  eprintln!("enrolled every demo user");

  let members: Vec<(&Peer, &str)> = [&alex, &sam, &robin, &first_reviewer, &second_reviewer]
    .into_iter()
    .map(|peer| (peer, "member"))
    .collect();
  let space = space_with(stack, &owner, SPACE_NAME, &members).await;

  let mut everybody = [
    &mut alex,
    &mut sam,
    &mut robin,
    &mut first_reviewer,
    &mut second_reviewer,
  ];
  let general = channel_with(&mut owner, space, "general", &mut everybody).await;
  let design = channel_with(&mut owner, space, "design", &mut everybody).await;
  let planning = channel_with(&mut owner, space, "release-planning", &mut everybody).await;
  eprintln!("created the space and its channels");

  say(
    &mut owner,
    general,
    "Welcome to Project A. This workspace is reset every night.",
  )
  .await;
  say(
    &mut alex,
    general,
    "Morning! The weekly sync moves to Thursday, 10:00.",
  )
  .await;
  say(
    &mut sam,
    general,
    "Works for me. I'll bring the updated timeline.",
  )
  .await;
  share(
    &mut alex,
    general,
    "meeting-notes.md",
    "text/markdown",
    "# Weekly sync\n\n- Timeline review\n- Open questions on the onboarding flow\n- Next steps\n",
  )
  .await;
  say(
    &mut robin,
    design,
    "First drafts of the new onboarding screens are up for feedback.",
  )
  .await;
  say(
    &mut alex,
    design,
    "Looks clean. Can we try a larger heading on the first screen?",
  )
  .await;
  say(
    &mut robin,
    design,
    "Sure, I'll post a second version this afternoon.",
  )
  .await;
  say(
    &mut sam,
    planning,
    "Release checklist for the next version is attached.",
  )
  .await;
  share(
    &mut sam,
    planning,
    "release-checklist.txt",
    "text/plain",
    "Release checklist\n\n[x] Changelog written\n[x] Translations updated\n[ ] Store screenshots\n[ ] Final test run\n",
  )
  .await;
  say(&mut owner, planning, "Thanks. Let's freeze features on Friday.").await;

  dm(
    &mut alex,
    &mut first_reviewer,
    "Hi! Feel free to send me a message to try things out.",
  )
  .await;
  dm(
    &mut alex,
    &mut second_reviewer,
    "Hi! Feel free to send me a message to try things out.",
  )
  .await;
  dm(
    &mut sam,
    &mut first_reviewer,
    "Welcome aboard. The release checklist is in #release-planning.",
  )
  .await;
  dm(
    &mut sam,
    &mut second_reviewer,
    "Welcome aboard. The release checklist is in #release-planning.",
  )
  .await;
  let alex_and_sam = dm(&mut alex, &mut sam, "Do you have the budget sheet for Project A?").await;
  share(
    &mut sam,
    alex_and_sam,
    "budget.csv",
    "text/csv",
    "item,amount\nDesign,1200\nDevelopment,4800\nTesting,900\n",
  )
  .await;
  eprintln!("wrote the messages, direct messages and files");

  let mut peers = [
    &mut owner,
    &mut alex,
    &mut sam,
    &mut robin,
    &mut first_reviewer,
    &mut second_reviewer,
  ];
  settle(&mut peers, SETTLE).await;
  // A reviewer's new device restores the content keys from the key backup, so every backup must
  // hold the keys of every conversation before the snapshot.
  for peer in peers.iter_mut() {
    peer
      .client
      .upload_key_bag()
      .await
      .unwrap_or_else(|error| panic!("{} uploads its key backup: {error}", peer.name));
  }
  settle(&mut peers, SETTLE).await;
}

/// Enrolls the demo user's only seeded device, with the shared recovery passphrase and the apps'
/// own key stretching (the reviewers' phones open the backup with it).
async fn enroll(stack: &Stack, user: &'static str, suffix: &str, passphrase: &str) -> Peer {
  let login = format!("{user}{suffix}");
  let options = EnrollmentOptions {
    device_name: "Demo seed".into(),
    secret: EnrollmentSecret::Passphrase(SecretBytes::new(passphrase.as_bytes().to_vec())),
    argon2: Argon2Settings::default(),
    maybe_attestation: None,
  };
  let mut peer = Peer::enroll_with_options(
    stack,
    &login,
    Arc::new(MemoryStore::new()),
    ClientSettings::default(),
    options,
  )
  .await;
  peer.name = user.to_owned();
  peer
}

async fn say(peer: &mut Peer, conversation: ConversationId, body: &str) {
  peer
    .client
    .send_text(conversation, None, body)
    .await
    .unwrap_or_else(|error| panic!("{} writes in {conversation}: {error}", peer.name));
}

async fn share(peer: &mut Peer, conversation: ConversationId, file_name: &str, media_type: &str, text: &str) {
  peer
    .client
    .send_file(
      conversation,
      None,
      OutgoingFile {
        file_name: file_name.into(),
        media_type: media_type.into(),
        content: text.as_bytes().to_vec(),
      },
    )
    .await
    .unwrap_or_else(|error| panic!("{} shares {file_name}: {error}", peer.name));
}

/// A direct message from `from` to `to`, who joins it and answers.
async fn dm(from: &mut Peer, to: &mut Peer, body: &str) -> ConversationId {
  let conversation = from
    .client
    .open_dm(to.user_id())
    .await
    .unwrap_or_else(|error| panic!("{} opens a DM with {}: {error}", from.name, to.name));
  say(from, conversation, body).await;
  to.wait_joined(conversation).await;
  to.wait_for_text(conversation, body).await;
  say(to, conversation, "Thanks!").await;
  conversation
}
