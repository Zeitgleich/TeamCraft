//! The demo instance's reviewer sign-in (issue #1743; docs/operations.md, "Demo instance"): the
//! seed of `demo-seed` played on a test server, the database snapshot and restore of
//! `dev/demo/demo-reset.sh`, then a new device of the first reviewer signs in, restores the account
//! from the shared passphrase, as a store reviewer's phone does, and reads a seeded message, a
//! direct message and a file. Nobody else is online then, as on the demo instance after a reset:
//! the keys come from the key backup alone.
//!
//! Then the seed devices come back from their restored stores, as `demo-peers` runs them (issue: a
//! reviewer could read but not write, nobody let the new device in): they admit the reviewer's
//! device, its direct message to `alex` (queued meanwhile) reaches `alex`, `alex` answers it once,
//! and its message in `#general` reaches the owner.
//!
//! The dev realm has four people with a member role, so `bob` plays both `alex` and `robin` and
//! `erin` is the only reviewer. Needs the dev stack's Postgres container for `pg_dump` and
//! `pg_restore` (as `scripts/e2e.sh` starts it).

use std::path::Path;
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use teamcraft_core::clock::{Clock, SystemClock};
use teamcraft_core::records::{self, Draft};
use teamcraft_core::store::{LocalStore, MessageContent, MessageFilter};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_e2e::app::DiskDir;
use teamcraft_e2e::demo::{self, ALEX_TO_REVIEWER, Cast, MEETING_NOTES, SPACE_NAME, WELCOME};
use teamcraft_e2e::demo_peers::AUTO_REPLY;
use teamcraft_e2e::peer::PASSPHRASE;
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::{ClientMessageId, ConversationId};
use tokio::process::{Child, Command};

/// Made after the snapshot: the restore must take it away.
const AFTER_SNAPSHOT_SPACE: &str = "Created after the snapshot";
const TO_ALEX: &str = "Hello from the store review.";
const IN_GENERAL: &str = "Testing the demo workspace.";
/// The admission is staggered by rank (`device_add_stagger`, 10 s), then the current key is handed
/// over the same way: well under a minute with the members online.
const ADMISSION_TIMEOUT: Duration = Duration::from_secs(90);
/// The process must stay light on the demo's small server (target well under 50 MB). Counted without
/// the pages of its binary, which a debug build has many more of than the release build.
const PEERS_HEAP_LIMIT_KIB: u64 = 32 * 1024;

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
  let stores = DiskDir::new("demo-peers");

  // The apps' own key stretching, as on the demo instance: the phone opens the backup with it.
  let seeded = demo::seed(
    &stack,
    &cast,
    PASSPHRASE,
    Argon2Settings::default(),
    stores.path(),
  )
  .await;
  let general = seeded.general;
  let (from_alex, _) = seeded.reviewer_dms[0];
  let seeded_identity = seeded
    .peers
    .iter()
    .find(|peer| peer.name == reviewer)
    .expect("the reviewer's seed device")
    .client
    .identity_fingerprint();
  // The seeding run ends: none of its devices is online until the peers start.
  drop(seeded);

  let dump = demo::snapshot(&mut stack, stores.path()).await;
  let owner = stack.tokens(&cast.owner);
  stack
    .create_space(&owner, AFTER_SNAPSHOT_SPACE, "invite_only")
    .await;
  demo::restore(&mut stack, &dump, stores.path()).await;

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

  // Queued as the app's composer does, while nobody can let the device in (the demo before
  // `demo-peers`): they wait in the outbox.
  let to_alex = queue(&phone, from_alex, TO_ALEX);
  let in_general = queue(&phone, general, IN_GENERAL);
  drop(phone.client.flush_outbox().await);

  let mut peers = start_peers(&stack, stores.path(), &cast.peer_logins());
  wait_sent(&mut phone, &mut peers, from_alex, to_alex).await;
  wait_sent(&mut phone, &mut peers, general, in_general).await;
  // Alex answers only what Alex's device decrypted: the reviewer's message got through.
  wait_reply(&mut phone, &mut peers, from_alex).await;
  let pid = peers.id().expect("the peers' process ID");
  let (total_kib, heap_kib) = (memory_kib(pid, "VmRSS"), memory_kib(pid, "RssAnon"));
  eprintln!(
    "demo-peers: {total_kib} KiB resident with three seed devices, {heap_kib} KiB of it not the binary"
  );
  assert!(
    heap_kib < PEERS_HEAP_LIMIT_KIB,
    "demo-peers holds {heap_kib} KiB besides its binary, more than {PEERS_HEAP_LIMIT_KIB} KiB"
  );
  stop_peers(&mut peers).await;

  // Once: the answer is the newest message, and nothing answers it.
  phone.pump().await;
  let replies = phone
    .texts(from_alex)
    .iter()
    .filter(|text| *text == AUTO_REPLY)
    .count();
  assert_eq!(replies, 1, "alex answered once: {:?}", phone.texts(from_alex));
  // The device's store survived the stop with what it read.
  let alex = demo::seed_store(stores.path(), &cast.alex);
  let alex_read = alex
    .messages(from_alex, MessageFilter::TopLevel)
    .unwrap()
    .into_iter()
    .any(|message| matches!(&message.content, MessageContent::Text { body, .. } if body == TO_ALEX));
  assert!(alex_read, "alex's store holds the reviewer's message");

  stack.stop().await;
}

fn queue(phone: &Peer, conversation: ConversationId, text: &str) -> ClientMessageId {
  let draft = Draft {
    conversation_id: conversation,
    maybe_thread_id: None,
    content: MessageContent::text(text, Vec::new()),
  };
  records::enqueue_message(&phone.store, phone.user_id(), SystemClock.now_ms(), draft)
    .expect("queueing the message")
}

/// The real `demo-peers` binary on the seed devices of `logins` (the dev realm's, not the demo's).
fn start_peers(stack: &Stack, stores: &Path, logins: &[&str]) -> Child {
  Command::new(env!("CARGO_BIN_EXE_demo-peers"))
    .env("E2E_DEPLOYED_URL", &stack.base_url)
    .env("DEMO_PEER_STORES", stores)
    .env("DEMO_PEER_LOGINS", logins.join(","))
    .kill_on_drop(true)
    .spawn()
    .expect("starting demo-peers")
}

/// Pumps the phone, which retries its queued message as the app does, until the server took the
/// message: the device is in the group and has the current key, so a seed device let it in.
async fn wait_sent(phone: &mut Peer, peers: &mut Child, conversation: ConversationId, id: ClientMessageId) {
  wait_phone(phone, peers, &format!("the server takes {id}"), |phone| {
    phone
      .store
      .message(conversation, id)
      .unwrap()
      .is_some_and(|message| message.maybe_server_id.is_some())
  })
  .await;
}

async fn wait_reply(phone: &mut Peer, peers: &mut Child, conversation: ConversationId) {
  wait_phone(phone, peers, "alex answers", |phone| {
    phone.texts(conversation).iter().any(|text| text == AUTO_REPLY)
  })
  .await;
}

/// Pumps the phone until `done` holds; fails when the peers exit or after [`ADMISSION_TIMEOUT`].
async fn wait_phone(
  phone: &mut Peer,
  peers: &mut Child,
  what: &str,
  mut done: impl FnMut(&mut Peer) -> bool,
) {
  let started = Instant::now();
  loop {
    // A wait longer than the gateway's heartbeat window: keep the session, reconnect if it closed.
    phone.heartbeat().await;
    phone.pump_reconnecting().await;
    // A failed flush (no key yet) leaves the message queued; the next turn tries again.
    drop(phone.client.flush_outbox().await);
    if done(phone) {
      return;
    }
    if let Some(status) = peers.try_wait().expect("checking demo-peers") {
      panic!("demo-peers exited ({status}) before {what}");
    }
    assert!(
      started.elapsed() < ADMISSION_TIMEOUT,
      "{what}: not within {ADMISSION_TIMEOUT:?}, the reviewer's device was not let in"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}

/// SIGTERM, as systemd stops the service: it closes its connections and stores and exits cleanly.
async fn stop_peers(peers: &mut Child) {
  let pid = peers.id().expect("the peers' process ID");
  let status = std::process::Command::new("kill")
    .args(["-TERM", &pid.to_string()])
    .status()
    .expect("running kill");
  assert!(status.success(), "sending SIGTERM to demo-peers");
  let exit = tokio::time::timeout(Duration::from_secs(30), peers.wait())
    .await
    .expect("demo-peers stops within 30 s")
    .expect("waiting for demo-peers");
  assert!(exit.success(), "demo-peers exits cleanly: {exit}");
}

/// A memory figure of process `pid` from `/proc/<pid>/status` (`VmRSS`, `RssAnon`).
fn memory_kib(pid: u32, field: &str) -> u64 {
  let status = std::fs::read_to_string(format!("/proc/{pid}/status")).expect("reading the process status");
  status
    .lines()
    .find_map(|line| line.strip_prefix(field)?.strip_prefix(':'))
    .and_then(|value| value.trim().trim_end_matches("kB").trim().parse().ok())
    .unwrap_or_else(|| panic!("{field} in the process status"))
}

/// The message of `#general` that holds the seeded meeting notes, once it is decrypted.
fn meeting_notes(peer: &Peer, general: ConversationId) -> Option<ClientMessageId> {
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
