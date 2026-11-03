//! Coming back after a long absence (issue #972): a desktop slept for hours, its gateway session is
//! gone, and in a dozen of its conversations others wrote messages and changed the encryption group
//! meanwhile. The app (the runner the apps use, the desktop's store on a disk, [`App`]) comes back
//! through a relay that adds [`FAR_RTT`]; measured: how long until the connection is up, until a
//! message sent to the conversation the user looks at right after the network came back shows, and
//! until the whole backlog is there. Real server, real Keycloak tokens, real MLS.

use std::rc::Rc;
use std::time::{Duration, Instant};

use teamcraft_core::store::LocalStore;
use teamcraft_e2e::app::{App, DiskDir, disk_store, mobile_store};
use teamcraft_e2e::netem::{Condition, PortConditions};
use teamcraft_e2e::peer::settle;
use teamcraft_e2e::relay::TcpRelay;
use teamcraft_e2e::scenario::space_with;
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::{ConversationId, SpaceId, UserId};
use tokio::task::LocalSet;

/// Conversations the returning user is in, as in the issue's log.
const CONVERSATIONS: usize = 38;
/// Conversations that changed while the user was away.
const CHANGED: usize = 12;
/// Messages per changed conversation while the user was away.
const MESSAGES: usize = 40;
/// Times per changed conversation somebody was added and removed again: each is a commit, an epoch
/// the returning device applies and stores.
const MEMBER_CHANGES: usize = 3;
/// The round trip the relay adds: a server in a data centre, the laptop at home.
const FAR_RTT: Duration = Duration::from_millis(30);
/// Shorter than the absence: the server forgets the session (the resume window passed).
const RESUME_WINDOW_SECS: u64 = 5;
const ARRIVAL_TIMEOUT: Duration = Duration::from_secs(180);
/// How long the network is still away when the app opens (only inside the network namespace).
const OFFLINE_AT_START: Duration = Duration::from_secs(15);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "benchmark of a few minutes: scripts/e2e-network.sh -- long_absence"]
async fn coming_back_after_a_long_absence() {
  let is_mobile = std::env::var("E2E_STORE").is_ok_and(|store| store == "mobile");
  let local = LocalSet::new();
  if is_mobile {
    local.run_until(come_back("mobile", mobile_store)).await;
  } else {
    local.run_until(come_back("desktop", disk_store)).await;
  }
}

async fn come_back<S: LocalStore + Clone + 'static>(store_name: &str, open_store: fn(&std::path::Path) -> S) {
  // Seeding writes faster than a person; the limits are not what is measured here.
  let Some(stack) = Stack::builder()
    .with_env("TEAMCRAFT_SECURITY__RATE_LIMITS__IS_ENABLED", "false")
    .with_env("TEAMCRAFT_LOGGING__FILTER", "info,tower_http=debug")
    .with_env(
      "TEAMCRAFT_GATEWAY__RESUME_WINDOW_SECS",
      &RESUME_WINDOW_SECS.to_string(),
    )
    .start()
    .await
  else {
    return;
  };
  let directory = DiskDir::new("long-absence");
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut bob = Peer::enroll(&stack, "bob").await;
  let mut dave = Peer::enroll(&stack, "dave").await;
  let mut carol =
    Peer::enroll_with_store(&stack, "carol", "carol desktop", open_store(directory.path())).await;
  let space = space_with(
    &stack,
    &alice,
    "Long absence",
    &[(&bob, "member"), (&dave, "guest")],
  )
  .await;
  stack
    .add_space_member(
      &alice.tokens,
      &space.to_string(),
      &carol.user_id().to_string(),
      "member",
    )
    .await;

  let seeding = Instant::now();
  let conversations = seed(&mut alice, &mut bob, carol.user_id(), space).await;
  carol
    .wait_for_text(
      conversations[CONVERSATIONS - 1],
      &format!("welcome to {}", CONVERSATIONS - 1),
    )
    .await;
  eprintln!(
    "long_absence: seeded {} conversations in {:.1} s",
    conversations.len(),
    seeding.elapsed().as_secs_f64()
  );
  // Carol's device goes to sleep: its connection ends, the server's session for it expires.
  let me = carol.user_id();
  let tokens = carol.tokens.clone();
  let store = carol.store.clone();
  drop(carol);

  let away = Instant::now();
  let changed = &conversations[..CHANGED];
  let mut expected = Vec::new();
  for (index, conversation) in changed.iter().enumerate() {
    expected.extend(write_while_away(&mut alice, &mut bob, &mut dave, *conversation, index).await);
  }
  // Bob was online all along: he reads everything, or the writing itself went wrong.
  for (channel, text) in expected.iter().rev().step_by(MESSAGES) {
    bob.wait_for_text(*channel, text).await;
  }
  // Erin, who shares no space with carol, opens a direct message with her meanwhile: a
  // conversation and a person new to her device, which she opens first when she comes back (a tap
  // on its notification).
  let mut erin = Peer::enroll(&stack, "erin").await;
  let dm = erin.client.open_dm(me).await.expect("opening a DM");
  let dm_text = "a direct message while carol was away";
  erin.client.send_text(dm, None, dm_text).await.unwrap();
  expected.push((dm, dm_text.to_owned()));
  tokio::time::sleep(Duration::from_secs(RESUME_WINDOW_SECS + 1)).await;
  eprintln!(
    "long_absence: {CHANGED} conversations changed ({MESSAGES} messages, {MEMBER_CHANGES} member changes each) in {:.1} s",
    away.elapsed().as_secs_f64()
  );

  let server_address = stack.base_url.trim_start_matches("http://").to_owned();
  let relay = TcpRelay::start_with_latency(&server_address, FAR_RTT)
    .await
    .unwrap();
  // The new direct message is what the notification opens.
  let focused = dm;
  // The phone opens the app before its network is back ("the server can't be reached").
  let maybe_link = PortConditions::take(relay.port);
  if let Some(link) = &maybe_link {
    link.set(Condition::Blackhole);
  }
  let started = Instant::now();
  let app = Rc::new(App::start_focused(
    &format!("http://127.0.0.1:{}", relay.port),
    tokens,
    store,
    // E2E_NO_FOCUS: as the mobile apps were before they told the core what is open (issue #972).
    std::env::var_os("E2E_NO_FOCUS").is_none().then_some(focused),
  ));
  let reads = app.read_like_mobile(me);
  if let Some(link) = &maybe_link {
    tokio::time::sleep(OFFLINE_AT_START).await;
    link.set(Condition::Clear);
    app.network.notify();
  }
  let live_text = "sent right after carol came back";
  erin.client.send_text(focused, None, live_text).await.unwrap();
  app.wait_for(&focused, live_text, ARRIVAL_TIMEOUT).await;
  let live_at = started.elapsed();
  expected.push((focused, live_text.to_owned()));
  for (channel, text) in &expected {
    app.wait_for(channel, text, ARRIVAL_TIMEOUT).await;
  }
  let all_at = started.elapsed();
  app.wait_connected(ARRIVAL_TIMEOUT).await;
  let mut all_conversations = conversations.clone();
  all_conversations.push(dm);
  app.assert_each_once(&all_conversations, &expected);
  let reads = reads.borrow().clone();
  eprintln!(
    "long_absence ({store_name}): after coming back: the live message showed after {} ms, \
     the whole backlog ({} messages) after {} ms; UI reads meanwhile: {}, waited up to {} ms, took up to {} ms, \
     up to {} conversations and {} people without a name",
    live_at.as_millis(),
    expected.len(),
    all_at.as_millis(),
    reads.reads,
    reads.max_wait.as_millis(),
    reads.max_read.as_millis(),
    reads.max_unnamed_conversations,
    reads.max_unnamed_people
  );
  if let Ok(app) = Rc::try_unwrap(app) {
    app.stop().await;
  }
  drop(relay);
  stack.stop().await;
}

/// `CONVERSATIONS` channels with alice, bob and carol, each with a first message.
async fn seed(alice: &mut Peer, bob: &mut Peer, carol: UserId, space: SpaceId) -> Vec<ConversationId> {
  let mut conversations = Vec::new();
  for index in 0..CONVERSATIONS {
    let channel = alice
      .client
      .create_channel(space, &format!("channel-{index}"))
      .await
      .expect("creating a channel");
    for member in [bob.user_id(), carol] {
      alice
        .client
        .add_member(channel, member)
        .await
        .expect("adding a member");
    }
    alice
      .client
      .send_text(channel, None, &format!("welcome to {index}"))
      .await
      .unwrap();
    conversations.push(channel);
  }
  bob.wait_joined(*conversations.last().unwrap()).await;
  conversations
}

/// Messages and member changes in one conversation while carol is away: dave is added and removed
/// again (commits, new epochs), alice and bob write in between. Returns what was written.
async fn write_while_away(
  alice: &mut Peer,
  bob: &mut Peer,
  dave: &mut Peer,
  conversation: ConversationId,
  index: usize,
) -> Vec<(ConversationId, String)> {
  let member_changes = member_changes();
  let per_round = MESSAGES.div_ceil(member_changes + 1);
  let mut written = Vec::new();
  for round in 0..=member_changes {
    if round > 0 {
      alice
        .client
        .add_member(conversation, dave.user_id())
        .await
        .expect("adding dave");
      dave.wait_joined(conversation).await;
      alice
        .client
        .remove_member(conversation, dave.user_id())
        .await
        .expect("removing dave");
      // The writers are apps that are online: they apply what their gateway brings.
      settle(
        &mut [&mut *alice, &mut *bob, &mut *dave],
        Duration::from_millis(300),
      )
      .await;
    }
    while written.len() < MESSAGES.min(per_round * (round + 1)) {
      let text = format!("while away in {index} #{}", written.len());
      let writer = if written.len() % 3 == 2 {
        &mut *bob
      } else {
        &mut *alice
      };
      writer.client.send_text(conversation, None, &text).await.unwrap();
      written.push((conversation, text));
    }
  }
  written
}

/// [`MEMBER_CHANGES`], or `E2E_MEMBER_CHANGES` to narrow a failure down.
fn member_changes() -> usize {
  std::env::var("E2E_MEMBER_CHANGES")
    .ok()
    .and_then(|value| value.parse().ok())
    .unwrap_or(MEMBER_CHANGES)
}
