//! A large workspace for measuring the mobile apps on a real device (docs/performance.md, "Android
//! on a phone"). Not a test: it starts a server, waits until the user signs in on the device, fills
//! the workspace while the device is a member of everything, and keeps the server up. Messages
//! reach the device as they would in daily use (MLS, the gateway), not as a history share.
//!
//! ```sh
//! adb reverse tcp:8080 tcp:8080 && adb reverse tcp:8091 tcp:8091
//! MOBILE_WORKSPACE_USER=carol cargo test -p teamcraft-e2e --features e2e --test suite -- \
//!   --ignored --nocapture mobile_workspace::
//! ```
//!
//! The app signs in at `http://127.0.0.1:8091` as `carol` (password `carol-dev-pass`; a member, not a
//! guest like `dave`). Sizes:
//! `MOBILE_WORKSPACE_SPACES`, `_CHANNELS_PER_SPACE`, `_MESSAGES`, `_LONG_CHANNEL_MESSAGES`, `_IMAGES`;
//! `_HOLD_SECS` keeps the server up (default 12 hours). Writing a number to the file the log names
//! sends that many more messages, spread over the channels and the direct messages, for measuring
//! a return after new messages arrived.
//!
//! With `MOBILE_WORKSPACE_CALLS=1` the server runs an SFU with its WebSocket fallback (QUIC doesn't
//! pass `adb reverse`; `adb reverse` the fallback's port, `ss -ltnp` shows it), and `call dm 60` or
//! `call channel 60` in the control file starts a call with a test tone in the direct message or
//! the first channel.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use teamcraft_core::client::OutgoingFile;
use teamcraft_e2e::call::{hold_call, join_call};
use teamcraft_e2e::scenario::space_with;
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::{ConversationId, UserId};

const PORT: u16 = 8091;
const IMAGES: [&[u8]; 3] = [
  include_bytes!("../../../../assets/backgrounds/lake-morning.jpg"),
  include_bytes!("../../../../assets/backgrounds/empty-room.jpg"),
  include_bytes!("../../../../assets/backgrounds/grey-lounge.jpg"),
];
const SENTENCES: [&str; 8] = [
  "Can you have a look at the release notes before lunch?",
  "The build is green again, the flaky test was a timing issue in the fixture.",
  "Meeting moved to 3 pm, same room. I'll bring the numbers for Project A.",
  "Thanks! Merged. Let's keep an eye on the error rate after the deploy tonight.",
  "Short summary: customers like the new onboarding, but the import is too slow for big files.",
  "ok",
  "**Status for this week**\n\n- Billing export: done, waiting for review\n- Search filters: \
   in progress, the date picker still needs work\n- `retry_after` header: merged\n\nNext week \
   I'm out on Thursday and Friday, ping me before Wednesday if something needs a decision.",
  "Here's the link to the draft: https://example.com/docs/draft-42 — comments welcome, \
   especially on the second section, I'm not sure the numbers add up.",
];
const REACTIONS: [&str; 4] = ["👍", "🎉", "👀", "❤️"];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "serves a workspace for measuring the mobile apps by hand"]
async fn serve_a_large_workspace_to_a_device() {
  let user = std::env::var("MOBILE_WORKSPACE_USER").unwrap_or_else(|_| "carol".into());
  let size = Size::from_env();
  let mut builder = Stack::builder()
    .with_port(PORT)
    .with_env("TEAMCRAFT_SECURITY__RATE_LIMITS__IS_ENABLED", "false")
    .with_env("TEAMCRAFT_DATABASE__MAX_CONNECTIONS", "16");
  if size.has_calls {
    // QUIC doesn't pass `adb reverse`; the SFU's WebSocket fallback does (reverse its port too).
    builder = builder.with_call_fallback();
  }
  let Some(stack) = builder.start().await else {
    return;
  };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut bob = Peer::enroll(&stack, "bob").await;
  let device_user = wait_for_device(&stack, &alice, &user).await;
  let channels = seed(&stack, &mut alice, &mut bob, device_user, size).await;
  let control = burst_file();
  eprintln!(
    "WORKSPACE: ready, {} conversations; write a number to {} to send that many more, or \
     `call dm <seconds>` / `call channel <seconds>` for a call with a test tone",
    channels.len(),
    control.display()
  );
  let deadline = Instant::now() + size.hold;
  let mut sent = size.messages;
  while Instant::now() < deadline {
    alice.heartbeat().await;
    bob.heartbeat().await;
    match take_command(&control) {
      Some(Command::Send(count)) => {
        let started = Instant::now();
        for index in 0..count {
          let writer = if index % 2 == 0 { &mut alice } else { &mut bob };
          send(writer, channels[index % channels.len()], sent + index).await;
        }
        sent += count;
        eprintln!("WORKSPACE: sent {count} more in {:?}", started.elapsed());
      },
      Some(Command::Call { is_direct, duration }) => {
        let Some(sfu) = stack.maybe_sfu.as_ref() else {
          eprintln!("WORKSPACE: no calls: start with MOBILE_WORKSPACE_CALLS=1");
          continue;
        };
        let hash = sfu.certificate_hash;
        // The direct message is Alice's with the device's user, the last conversation.
        let conversation = if is_direct {
          channels[channels.len() - 1]
        } else {
          channels[0]
        };
        eprintln!("WORKSPACE: calling in {conversation} for {duration:?}");
        let alice_call = join_call(&mut alice, conversation, hash, 330.0).await;
        if is_direct {
          let stats = hold_call(alice_call, duration).await;
          eprintln!("WORKSPACE: call ended: {stats:?}");
        } else {
          let bob_call = join_call(&mut bob, conversation, hash, 550.0).await;
          let (alice_stats, bob_stats) =
            tokio::join!(hold_call(alice_call, duration), hold_call(bob_call, duration));
          eprintln!("WORKSPACE: call ended: {alice_stats:?} {bob_stats:?}");
        }
      },
      None => {},
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
  }
  stack.stop().await;
}

/// How much the workspace holds.
#[derive(Clone, Copy, Debug)]
struct Size {
  spaces: usize,
  channels_per_space: usize,
  messages: usize,
  long_channel_messages: usize,
  images: usize,
  /// An SFU with its fallback, for calls with the device (`MOBILE_WORKSPACE_CALLS=1`).
  has_calls: bool,
  hold: Duration,
}

impl Size {
  fn from_env() -> Self {
    Self {
      spaces: number("SPACES", 12),
      channels_per_space: number("CHANNELS_PER_SPACE", 25),
      messages: number("MESSAGES", 40_000),
      long_channel_messages: number("LONG_CHANNEL_MESSAGES", 10_000),
      images: number("IMAGES", 60),
      has_calls: number("CALLS", 0) == 1,
      hold: Duration::from_secs(number("HOLD_SECS", 12 * 3600) as u64),
    }
  }
}

fn number(name: &str, default: usize) -> usize {
  std::env::var(format!("MOBILE_WORKSPACE_{name}"))
    .ok()
    .and_then(|value| value.parse().ok())
    .unwrap_or(default)
}

/// Waits until `user` has set up a device (signed in on the phone and created or unlocked their
/// keys), so it is a member of everything created afterwards.
async fn wait_for_device(stack: &Stack, admin: &Peer, user: &str) -> UserId {
  let tokens = stack.tokens(user);
  let user_id: UserId = stack.get(&tokens, "/me").await.id().parse().unwrap();
  eprintln!(
    "WORKSPACE: sign in as {user} on the device now (server {})",
    stack.base_url
  );
  loop {
    let devices = stack
      .get(&admin.tokens, &format!("/users/{user_id}/devices"))
      .await;
    if devices.body["devices"]
      .as_array()
      .is_some_and(|list| !list.is_empty())
    {
      break;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
  }
  // Its key packages are uploaded right after the device is registered.
  tokio::time::sleep(Duration::from_secs(15)).await;
  user_id
}

/// Alice owns every space; Bob and the device's user are in all of them and in every channel. The
/// first channel gets a long history, the rest share the other messages round-robin, two direct
/// messages and a group get their share too, and every tenth message gets a reaction.
async fn seed(
  stack: &Stack,
  alice: &mut Peer,
  bob: &mut Peer,
  device_user: UserId,
  size: Size,
) -> Vec<ConversationId> {
  let started = Instant::now();
  let mut channels = Vec::new();
  for space_index in 0..size.spaces {
    let space = space_with(
      stack,
      alice,
      &format!("Team {space_index:02}"),
      &[(&*bob, "member")],
    )
    .await;
    stack
      .add_space_member(
        &alice.tokens,
        &space.to_string(),
        &device_user.to_string(),
        "member",
      )
      .await;
    for channel_index in 0..size.channels_per_space {
      let name = format!("project-{space_index:02}-{channel_index:02}");
      let channel = alice.client.create_channel(space, &name).await.unwrap();
      alice.client.add_member(channel, bob.user_id()).await.unwrap();
      alice.client.add_member(channel, device_user).await.unwrap();
      channels.push(channel);
    }
    alice.heartbeat().await;
    bob.heartbeat().await;
  }
  let alice_dm = alice.client.open_dm(device_user).await.unwrap();
  let bob_dm = bob.client.open_dm(device_user).await.unwrap();
  let deadline = Instant::now() + Duration::from_secs(600);
  while !channels
    .iter()
    .all(|channel| bob.client.has_group(*channel).unwrap())
  {
    assert!(Instant::now() < deadline, "Bob did not join every channel");
    bob.pump_reconnecting().await;
    bob.heartbeat().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
  eprintln!(
    "WORKSPACE: {} channels in {:?}",
    channels.len(),
    started.elapsed()
  );

  let long_channel = channels[0];
  let rest = size.messages.saturating_sub(size.long_channel_messages);
  let mut others = channels[1..].to_vec();
  // The direct messages come up more often than any one channel, as they do in real use.
  for _ in 0..8 {
    others.push(alice_dm);
  }
  let alice_writes = async {
    for index in 0..size.long_channel_messages {
      send(alice, long_channel, index).await;
      if index % 2000 == 0 {
        alice.heartbeat().await;
        eprintln!("WORKSPACE: long channel {index}/{}", size.long_channel_messages);
      }
    }
    for index in (0..rest).step_by(2) {
      send(alice, others[index % others.len()], index).await;
      if index % 2000 == 0 {
        alice.heartbeat().await;
      }
    }
  };
  let bob_others: Vec<ConversationId> = others
    .iter()
    .map(|channel| if *channel == alice_dm { bob_dm } else { *channel })
    .collect();
  let bob_writes = async {
    for index in (1..rest).step_by(2) {
      send(bob, bob_others[index % bob_others.len()], index).await;
      if index % 2001 == 0 {
        bob.heartbeat().await;
        eprintln!("WORKSPACE: other channels {index}/{rest}");
      }
    }
  };
  tokio::join!(alice_writes, bob_writes);
  for index in 0..size.images {
    let channel = channels[index % size.channels_per_space.max(1)];
    alice
      .client
      .send_file(
        channel,
        None,
        OutgoingFile {
          file_name: format!("photo-{index}.jpg"),
          media_type: "image/jpeg".into(),
          content: IMAGES[index % IMAGES.len()].to_vec(),
        },
      )
      .await
      .unwrap();
  }
  for channel in &channels[..size.channels_per_space] {
    alice
      .client
      .send_text(*channel, None, "newest message")
      .await
      .unwrap();
  }
  eprintln!("WORKSPACE: seeded in {:?}", started.elapsed());
  let mut conversations = channels;
  conversations.push(alice_dm);
  conversations
}

async fn send(writer: &mut Peer, conversation: ConversationId, index: usize) {
  let body = format!("{} ({index})", SENTENCES[index % SENTENCES.len()]);
  let id = writer.client.send_text(conversation, None, &body).await.unwrap();
  if index % 10 == 3 {
    let emoji = REACTIONS[index % REACTIONS.len()];
    writer.client.react(conversation, id, emoji, true).await.unwrap();
  }
}

fn burst_file() -> PathBuf {
  std::env::temp_dir().join(format!("teamcraft-mobile-workspace-{PORT}.burst"))
}

/// What the control file asks for.
enum Command {
  Send(usize),
  Call { is_direct: bool, duration: Duration },
}

/// The command written to the control file, which is then removed.
fn take_command(path: &PathBuf) -> Option<Command> {
  let text = std::fs::read_to_string(path).ok()?;
  let _ = std::fs::remove_file(path);
  let words: Vec<&str> = text.split_whitespace().collect();
  match words.as_slice() {
    [count] => count.parse().ok().map(Command::Send),
    ["call", kind, seconds] => Some(Command::Call {
      is_direct: *kind == "dm",
      duration: Duration::from_secs(seconds.parse().ok()?),
    }),
    _ => None,
  }
}
