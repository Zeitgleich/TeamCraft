//! The seed of the demo instance for store reviews (docs/operations.md, "Demo instance"; issues
//! #1628, #1743): who writes what where. The `demo-seed` binary fills the real instance with it;
//! the `demo_signin` scenario seeds a test server the same way and checks that a reviewer's new
//! device reads the seed after a snapshot and restore.
//!
//! Real clients do the work, so keys, MLS groups and attachments are what any app would make: every
//! person of the [`Cast`] signs in, enrolls one device with the shared recovery passphrase, the owner
//! signs the workspace policy, and the others fill a space, channels, direct messages and files.
//! Everything is placeholder content: no real people, companies or hosts.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use teamcraft_core::client::OutgoingFile;
use teamcraft_core::store::MemoryStore;
use teamcraft_core::{ClientSettings, EnrollmentOptions, EnrollmentSecret};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::secret::SecretBytes;
use teamcraft_proto::{ConversationId, RetentionSetting, SpaceId};

use crate::peer::{Peer, settle};
use crate::scenario::{channel_with, space_with};
use crate::stack::Stack;

pub const SPACE_NAME: &str = "Project A";
/// Messages kept per channel: the seed stays, the reviewers' own messages can't pile up between
/// resets. Counted, not dated, so the seed survives however old the snapshot gets.
const RETENTION_MAX_MESSAGES: u64 = 500;
const RETENTION_LIMIT_MESSAGES: u64 = 1000;
/// Time for welcomes, keys and messages in flight to reach everybody before the run ends.
const SETTLE: Duration = Duration::from_secs(5);
/// The name of the seeded devices in the users' device lists.
const SEED_DEVICE_NAME: &str = "Demo seed";

/// The first message of `#general`, which a reviewer sees first.
pub const WELCOME: &str = "Welcome to Project A. This workspace is reset every night.";
/// What `alex` writes to each reviewer.
pub const ALEX_TO_REVIEWER: &str = "Hi! Feel free to send me a message to try things out.";
/// What `sam` writes to each reviewer.
pub const SAM_TO_REVIEWER: &str = "Welcome aboard. The release checklist is in #release-planning.";
/// The file `alex` shares in `#general`.
pub const MEETING_NOTES: SeedFile = SeedFile {
  name: "meeting-notes.md",
  media_type: "text/markdown",
  text: "# Weekly sync\n\n- Timeline review\n- Open questions on the onboarding flow\n- Next steps\n",
};
const RELEASE_CHECKLIST: SeedFile = SeedFile {
  name: "release-checklist.txt",
  media_type: "text/plain",
  text: "Release checklist\n\n[x] Changelog written\n[x] Translations updated\n[ ] Store screenshots\n[ ] Final test run\n",
};
const BUDGET: SeedFile = SeedFile {
  name: "budget.csv",
  media_type: "text/csv",
  text: "item,amount\nDesign,1200\nDevelopment,4800\nTesting,900\n",
};

/// A small text file of the seed.
#[derive(Clone, Copy, Debug)]
pub struct SeedFile {
  pub name: &'static str,
  pub media_type: &'static str,
  pub text: &'static str,
}

/// The people of the seed by the part they play.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
  /// Signs the workspace policy and creates the space and its channels.
  Owner,
  Alex,
  Sam,
  Robin,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Channel {
  General,
  Design,
  Planning,
}

enum Post {
  Text(&'static str),
  File(SeedFile),
}

/// The channels' content, in the order it is written.
const CHANNEL_POSTS: [(Role, Channel, Post); 10] = [
  (Role::Owner, Channel::General, Post::Text(WELCOME)),
  (
    Role::Alex,
    Channel::General,
    Post::Text("Morning! The weekly sync moves to Thursday, 10:00."),
  ),
  (
    Role::Sam,
    Channel::General,
    Post::Text("Works for me. I'll bring the updated timeline."),
  ),
  (Role::Alex, Channel::General, Post::File(MEETING_NOTES)),
  (
    Role::Robin,
    Channel::Design,
    Post::Text("First drafts of the new onboarding screens are up for feedback."),
  ),
  (
    Role::Alex,
    Channel::Design,
    Post::Text("Looks clean. Can we try a larger heading on the first screen?"),
  ),
  (
    Role::Robin,
    Channel::Design,
    Post::Text("Sure, I'll post a second version this afternoon."),
  ),
  (
    Role::Sam,
    Channel::Planning,
    Post::Text("Release checklist for the next version is attached."),
  ),
  (Role::Sam, Channel::Planning, Post::File(RELEASE_CHECKLIST)),
  (
    Role::Owner,
    Channel::Planning,
    Post::Text("Thanks. Let's freeze features on Friday."),
  ),
];

/// The logins that play the seed's roles at the identity provider. One login may play several of
/// `alex`, `sam` and `robin` (the dev realm has fewer people than the demo); the owner, the two who
/// write to each other (`alex` and `sam`) and every reviewer are people of their own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cast {
  pub owner: String,
  pub alex: String,
  pub sam: String,
  pub robin: String,
  /// One account per store, so two reviews at the same time don't meet.
  pub reviewers: Vec<String>,
}

impl Cast {
  /// The demo instance's people: `owner`, `alex`, `sam`, `robin`, `reviewer-1` and `reviewer-2`,
  /// each with `suffix` appended (Dex signs in with the e-mail address).
  pub fn demo(suffix: &str) -> Self {
    let login = |user: &str| format!("{user}{suffix}");
    Self {
      owner: login("owner"),
      alex: login("alex"),
      sam: login("sam"),
      robin: login("robin"),
      reviewers: vec![login("reviewer-1"), login("reviewer-2")],
    }
  }

  /// The seed on the dev realm, which has four people with a member role: `alice` is the owner,
  /// `bob` plays `alex` and `robin`, `carol` plays `sam`, and `erin` is the only reviewer.
  pub fn dev() -> Self {
    Self {
      owner: "alice".into(),
      alex: "bob".into(),
      sam: "carol".into(),
      robin: "bob".into(),
      reviewers: vec!["erin".into()],
    }
  }

  /// The login that plays `role`.
  pub fn login(&self, role: Role) -> &str {
    match role {
      Role::Owner => &self.owner,
      Role::Alex => &self.alex,
      Role::Sam => &self.sam,
      Role::Robin => &self.robin,
    }
  }

  /// Every login once, the owner first, then in the order the seed enrolls them.
  pub fn logins(&self) -> Vec<&str> {
    let mut logins: Vec<&str> = Vec::new();
    let all = [&self.owner, &self.alex, &self.sam, &self.robin]
      .into_iter()
      .chain(&self.reviewers);
    for login in all {
      if !logins.contains(&login.as_str()) {
        logins.push(login);
      }
    }
    logins
  }

  /// Why the seed can't be played with this cast, if it can't: the owner creates the channels
  /// others join, `alex` and `sam` write to each other and to every reviewer.
  pub fn check(&self) -> Result<(), String> {
    if self.reviewers.is_empty() {
      return Err("the cast has no reviewer".into());
    }
    let others = [&self.alex, &self.sam, &self.robin]
      .into_iter()
      .chain(&self.reviewers);
    if others.clone().any(|login| *login == self.owner) {
      return Err(format!("the owner {} plays another part too", self.owner));
    }
    if self.alex == self.sam {
      return Err(format!(
        "{} plays both alex and sam, who write to each other",
        self.alex
      ));
    }
    for (index, reviewer) in self.reviewers.iter().enumerate() {
      if [&self.alex, &self.sam, &self.robin].contains(&reviewer)
        || self.reviewers[..index].contains(reviewer)
      {
        return Err(format!("the reviewer {reviewer} plays another part too"));
      }
    }
    Ok(())
  }
}

/// What a seeding run made: its devices (one per login, in [`Cast::logins`]'s order) and the
/// conversations a check reads.
pub struct Seeded {
  pub peers: Vec<Peer>,
  pub space: SpaceId,
  pub general: ConversationId,
  pub design: ConversationId,
  pub planning: ConversationId,
  /// Per reviewer, in the cast's order: the direct messages from `alex` and from `sam`.
  pub reviewer_dms: Vec<(ConversationId, ConversationId)>,
}

/// Seeds an empty instance with `cast`; every user's key backup opens with `passphrase`. The
/// devices use the apps' own key stretching (`argon2`: the default for the real instance), so
/// the reviewers' phones open the backup as they would any other.
pub async fn seed(stack: &Stack, cast: &Cast, passphrase: &str, argon2: Argon2Settings) -> Seeded {
  if let Err(reason) = cast.check() {
    panic!("the demo cast: {reason}");
  }
  let logins = cast.logins();
  let at = |role: Role| index_of(&logins, cast.login(role));
  let reviewer_at: Vec<usize> = cast
    .reviewers
    .iter()
    .map(|reviewer| index_of(&logins, reviewer))
    .collect();
  assert_eq!(at(Role::Owner), 0, "the owner comes first");

  let mut owner = enroll(stack, logins[0], passphrase, argon2).await;
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
  let mut peers = Vec::with_capacity(logins.len());
  peers.push(owner);
  for login in &logins[1..] {
    peers.push(enroll(stack, login, passphrase, argon2).await);
  }
  eprintln!("enrolled every demo user");

  let (owner, others) = peers.split_first_mut().expect("the owner");
  let members: Vec<(&Peer, &str)> = others.iter().map(|peer| (peer, "member")).collect();
  let space = space_with(stack, owner, SPACE_NAME, &members).await;
  let mut everybody: Vec<&mut Peer> = others.iter_mut().collect();
  let general = channel_with(owner, space, "general", &mut everybody).await;
  let design = channel_with(owner, space, "design", &mut everybody).await;
  let planning = channel_with(owner, space, "release-planning", &mut everybody).await;
  eprintln!("created the space and its channels");

  for (role, channel, post) in &CHANNEL_POSTS {
    let conversation = match channel {
      Channel::General => general,
      Channel::Design => design,
      Channel::Planning => planning,
    };
    let peer = &mut peers[at(*role)];
    match post {
      Post::Text(body) => say(peer, conversation, body).await,
      Post::File(file) => share(peer, conversation, *file).await,
    }
  }

  let mut from_alex = Vec::with_capacity(reviewer_at.len());
  for &reviewer in &reviewer_at {
    let (alex, reviewer) = pair(&mut peers, at(Role::Alex), reviewer);
    from_alex.push(dm(alex, reviewer, ALEX_TO_REVIEWER).await);
  }
  let mut from_sam = Vec::with_capacity(reviewer_at.len());
  for &reviewer in &reviewer_at {
    let (sam, reviewer) = pair(&mut peers, at(Role::Sam), reviewer);
    from_sam.push(dm(sam, reviewer, SAM_TO_REVIEWER).await);
  }
  let (alex, sam) = pair(&mut peers, at(Role::Alex), at(Role::Sam));
  let alex_and_sam = dm(alex, sam, "Do you have the budget sheet for Project A?").await;
  share(sam, alex_and_sam, BUDGET).await;
  eprintln!("wrote the messages, direct messages and files");

  let mut all: Vec<&mut Peer> = peers.iter_mut().collect();
  settle(&mut all, SETTLE).await;
  // A reviewer's new device restores the content keys from the key backup, so every backup must
  // hold the keys of every conversation before the snapshot.
  for peer in all.iter_mut() {
    peer
      .client
      .upload_key_bag()
      .await
      .unwrap_or_else(|error| panic!("{} uploads its key backup: {error}", peer.name));
  }
  settle(&mut all, SETTLE).await;
  Seeded {
    peers,
    space,
    general,
    design,
    planning,
    reviewer_dms: from_alex.into_iter().zip(from_sam).collect(),
  }
}

/// How a device of the demo opens the key backup: the shared passphrase with `argon2`.
pub fn enrollment_options(device_name: &str, passphrase: &str, argon2: Argon2Settings) -> EnrollmentOptions {
  EnrollmentOptions {
    device_name: device_name.into(),
    secret: EnrollmentSecret::Passphrase(SecretBytes::new(passphrase.as_bytes().to_vec())),
    argon2,
    maybe_attestation: None,
  }
}

/// Enrolls a device of `login` with the shared recovery passphrase: the user's first (the seed's),
/// or a new one that recovers the identity from the key backup (a reviewer's phone).
pub async fn enroll_device(
  stack: &Stack,
  login: &str,
  device_name: &str,
  passphrase: &str,
  argon2: Argon2Settings,
) -> Peer {
  Peer::enroll_with_options(
    stack,
    login,
    Arc::new(MemoryStore::new()),
    ClientSettings::default(),
    enrollment_options(device_name, passphrase, argon2),
  )
  .await
}

async fn enroll(stack: &Stack, login: &str, passphrase: &str, argon2: Argon2Settings) -> Peer {
  enroll_device(stack, login, SEED_DEVICE_NAME, passphrase, argon2).await
}

/// The dev stack's Postgres tools: those in its container, so `pg_dump` is as new as the server.
fn postgres_tool(tool: &str) -> tokio::process::Command {
  let compose_file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docker-compose.dev.yml");
  let mut command = tokio::process::Command::new("docker");
  command.args(["compose", "-f"]).arg(compose_file).args([
    "exec",
    "-T",
    "postgres",
    tool,
    "-U",
    POSTGRES_USER,
  ]);
  command
}

const POSTGRES_USER: &str = "teamcraft";

/// Why the snapshot and restore can't run here, if they can't: they need the dev stack's Postgres
/// container (`docker compose`), which `scripts/e2e.sh` starts.
pub async fn check_postgres_tools() -> Result<(), String> {
  let output = postgres_tool("pg_isready")
    .output()
    .await
    .map_err(|error| format!("docker compose is not available: {error}"))?;
  if output.status.success() {
    Ok(())
  } else {
    Err(format!(
      "the dev stack's Postgres container does not answer: {}",
      String::from_utf8_lossy(&output.stderr).trim()
    ))
  }
}

/// `demo-reset.sh snapshot` on a test server: stops it, dumps its database in the same format
/// (`pg_dump --format=custom --no-owner --no-privileges`) and starts it again. Returns the dump.
///
/// The bucket is not copied: the test servers share the dev stack's bucket, and `rclone sync` of a
/// copy back would delete other tests' files. Nothing in it changes between snapshot and restore
/// here, so the seed's files are what the restored database points at either way.
pub async fn snapshot(stack: &mut Stack) -> PathBuf {
  stack.stop_server().await;
  let dump = stack.dir.join("database.dump");
  let file = std::fs::File::create(&dump).expect("creating the dump file");
  let database = stack.database_name().to_owned();
  let output = postgres_tool("pg_dump")
    .args([
      "--format=custom",
      "--no-owner",
      "--no-privileges",
      "--dbname",
      &database,
    ])
    .stdout(file)
    .stderr(Stdio::piped())
    .output()
    .await
    .expect("running pg_dump");
  assert!(
    output.status.success(),
    "pg_dump: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  stack
    .respawn_server()
    .await
    .unwrap_or_else(|error| panic!("starting the server after the snapshot: {error:#}"));
  dump
}

/// `demo-reset.sh restore` on a test server: stops it, empties the schema `public`, restores the
/// dump with the script's flags and starts the server again, which migrates the seed forward.
pub async fn restore(stack: &mut Stack, dump: &Path) {
  stack.stop_server().await;
  let database = stack.database_name().to_owned();
  let output = postgres_tool("psql")
    .args(["-X", "-v", "ON_ERROR_STOP=1", "-q", "--dbname", &database])
    .args([
      "-c",
      "SET client_min_messages = warning",
      "-c",
      "DROP SCHEMA IF EXISTS public CASCADE",
      "-c",
      "CREATE SCHEMA public",
    ])
    .output()
    .await
    .expect("running psql");
  assert!(
    output.status.success(),
    "emptying the database: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  let file = std::fs::File::open(dump).expect("opening the dump");
  let output = postgres_tool("pg_restore")
    .args([
      "--no-owner",
      "--no-privileges",
      "--exit-on-error",
      "--single-transaction",
      "--dbname",
      &database,
    ])
    .stdin(file)
    .stderr(Stdio::piped())
    .output()
    .await
    .expect("running pg_restore");
  assert!(
    output.status.success(),
    "pg_restore: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  stack
    .respawn_server()
    .await
    .unwrap_or_else(|error| panic!("starting the server after the restore: {error:#}"));
}

fn index_of(logins: &[&str], login: &str) -> usize {
  logins
    .iter()
    .position(|known| *known == login)
    .unwrap_or_else(|| panic!("{login} is not in the cast"))
}

/// Two different items (peers) of the list at once, in the asked order.
fn pair<T>(items: &mut [T], first: usize, second: usize) -> (&mut T, &mut T) {
  assert_ne!(first, second, "a peer can't write to itself");
  if first < second {
    let (head, tail) = items.split_at_mut(second);
    (&mut head[first], &mut tail[0])
  } else {
    let (head, tail) = items.split_at_mut(first);
    (&mut tail[0], &mut head[second])
  }
}

async fn say(peer: &mut Peer, conversation: ConversationId, body: &str) {
  peer
    .client
    .send_text(conversation, None, body)
    .await
    .unwrap_or_else(|error| panic!("{} writes in {conversation}: {error}", peer.name));
}

async fn share(peer: &mut Peer, conversation: ConversationId, file: SeedFile) {
  peer
    .client
    .send_file(
      conversation,
      None,
      OutgoingFile {
        file_name: file.name.into(),
        media_type: file.media_type.into(),
        content: file.text.as_bytes().to_vec(),
      },
    )
    .await
    .unwrap_or_else(|error| panic!("{} shares {}: {error}", peer.name, file.name));
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_demo_cast_has_six_people_with_the_suffix() {
    let cast = Cast::demo("@example.com");
    assert_eq!(
      cast.logins(),
      [
        "owner@example.com",
        "alex@example.com",
        "sam@example.com",
        "robin@example.com",
        "reviewer-1@example.com",
        "reviewer-2@example.com",
      ]
    );
    assert_eq!(cast.check(), Ok(()));
    assert_eq!(cast.login(Role::Robin), "robin@example.com");
  }

  #[test]
  fn a_login_with_several_parts_is_enrolled_once() {
    let cast = Cast::dev();
    assert_eq!(cast.check(), Ok(()));
    assert_eq!(cast.logins(), ["alice", "bob", "carol", "erin"]);
  }

  #[test]
  fn a_cast_the_seed_cannot_play_is_refused() {
    let owner_writes_too = Cast {
      alex: "alice".into(),
      ..Cast::dev()
    };
    assert!(owner_writes_too.check().is_err());
    let alex_is_sam = Cast {
      sam: "bob".into(),
      ..Cast::dev()
    };
    assert!(alex_is_sam.check().is_err());
    let reviewer_writes = Cast {
      reviewers: vec!["carol".into()],
      ..Cast::dev()
    };
    assert!(reviewer_writes.check().is_err());
    let same_reviewer_twice = Cast {
      reviewers: vec!["erin".into(), "erin".into()],
      ..Cast::dev()
    };
    assert!(same_reviewer_twice.check().is_err());
    let no_reviewer = Cast {
      reviewers: Vec::new(),
      ..Cast::dev()
    };
    assert!(no_reviewer.check().is_err());
  }

  #[test]
  fn every_role_posts_and_the_reviewers_first_file_is_in_general() {
    for role in [Role::Owner, Role::Alex, Role::Sam, Role::Robin] {
      assert!(
        CHANNEL_POSTS.iter().any(|(author, _, _)| *author == role),
        "{role:?} writes nothing"
      );
    }
    assert!(matches!(
      CHANNEL_POSTS[0],
      (Role::Owner, Channel::General, Post::Text(WELCOME))
    ));
    assert!(CHANNEL_POSTS.iter().any(|(_, channel, post)| {
      *channel == Channel::General && matches!(post, Post::File(file) if file.name == MEETING_NOTES.name)
    }));
  }

  #[test]
  fn pair_hands_out_both_in_the_asked_order() {
    let mut numbers = [0, 1, 2, 3];
    let (first, second) = pair(&mut numbers, 3, 1);
    assert_eq!((*first, *second), (3, 1));
    let (first, second) = pair(&mut numbers, 0, 2);
    assert_eq!((*first, *second), (0, 2));
  }
}
