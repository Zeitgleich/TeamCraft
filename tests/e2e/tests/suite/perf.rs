//! Client performance against a generated workspace (docs/robustness-audit.md, "Performance and
//! resources"; budgets in .claude/rules/robustness.md): the web client in headless Chromium.
//!
//! Carol signs in with the browser and leaves. While she is away, Alice and Bob fill a
//! workspace with spaces, channels, messages and images and add her everywhere. Then she comes
//! back: catch-up, channel switches, a long history, a leak check over repeated switches and a
//! period of idling are measured.
//!
//! `PERF_SCALE=small` (the default, a few minutes, part of the nightly suite) asserts generous
//! budgets as a regression check. `PERF_SCALE=large` (20 spaces, 300 channels, 100k messages) is
//! for measuring by hand, `PERF_SCALE=tiny` for working on the test:
//!
//! ```sh
//! PERF_SCALE=large cargo test -p teamcraft-e2e --features browser --test suite -- --nocapture perf::
//! ```
//!
//! Every run writes its numbers to `target/e2e-artifacts/perf-<scale>.json`, and prints the idle CPU
//! per kind of browser process. `PERF_SECOND_IDLE=1` watches a second idle period of the same length
//! afterwards (after a large catch-up the browser stays busy for a few minutes, docs/performance.md).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chromiumoxide::cdp::browser_protocol::network::{
  EnableParams as NetworkEnableParams, EventRequestWillBeSent, EventWebSocketFrameReceived,
  EventWebSocketFrameSent,
};
use chromiumoxide::cdp::browser_protocol::performance::{EnableParams, GetMetricsParams};
use chromiumoxide::cdp::js_protocol::heap_profiler::CollectGarbageParams;
use chromiumoxide::cdp::js_protocol::profiler::{
  EnableParams as ProfilerEnableParams, SetSamplingIntervalParams, StartParams as ProfilerStartParams,
  StopParams as ProfilerStopParams,
};
use chromiumoxide::cdp::js_protocol::runtime::{EvaluateParams, EventConsoleApiCalled};
use chromiumoxide::page::Page;
use futures_util::StreamExt;
use teamcraft_core::client::OutgoingFile;
use teamcraft_e2e::browser::{BrowserSession, WebPortLease, artifact_dir, lease_web_port, web_dir};
use teamcraft_e2e::peer::PASSPHRASE;
use teamcraft_e2e::resources::ProcessUsage;
use teamcraft_e2e::scenario::space_with;
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::{ConversationId, HistoryPolicy, SpaceId, UserId};

use crate::web::{TEST_CSP, click_text, type_until_kept, wait_for_element, wait_for_text};

/// Not the browser tests' 8082, so both can run at once (the dev realm registers both).
const PORT: u16 = 8083;
const WEB_ORIGIN: &str = "http://127.0.0.1:8083";
const IMAGES: [&[u8]; 3] = [
  include_bytes!("../../../../assets/backgrounds/lake-morning.jpg"),
  include_bytes!("../../../../assets/backgrounds/empty-room.jpg"),
  include_bytes!("../../../../assets/backgrounds/grey-lounge.jpg"),
];
const SENTENCES: [&str; 6] = [
  "Can you have a look at the release notes before lunch?",
  "The build is green again, the flaky test was a timing issue in the fixture.",
  "Meeting moved to 3 pm, same room. I'll bring the numbers for Project A.",
  "Thanks! Merged. Let's keep an eye on the error rate after the deploy tonight.",
  "Short summary: customers like the new onboarding, but the import is too slow for big files.",
  "ok",
];
/// Channel switches measured for latency.
const SWITCHES: usize = 40;
/// Channel switches measured while the background catch-up runs and right after it, while the
/// browser still writes it to IndexedDB (issue #326).
const BUSY_SWITCHES: usize = 20;
/// Channel switches of the leak check (the budget's "memory growth over 100 channel switches").
const LEAK_CHECK_SWITCHES: usize = 100;
const MIB: f64 = 1024.0 * 1024.0;
/// How long the empty browser is watched for its own idle CPU and wakeups.
const BASELINE_IDLE: Duration = Duration::from_secs(30);
/// How long the app may take to come back after a navigation (a large catch-up).
const LONG_WAIT: Duration = Duration::from_secs(900);
/// Connected and without an API request this long, the client counts as done with what it was
/// doing.
const QUIET: Duration = Duration::from_secs(10);
/// The whole browser below this much CPU (percent of one core) over [`SETTLE_WINDOW`] counts as
/// settled: what a catch-up left behind (IndexedDB writing, decoding) is done.
const SETTLED_CPU_PERCENT: f64 = 3.0;
const SETTLE_WINDOW: Duration = Duration::from_secs(10);
/// Counts tasks that block the page's thread for more than 50 ms (`[count, total ms, longest ms]`).
const LONG_TASK_RECORDER: &str = "window.__longTasks = [0, 0, 0];
  new PerformanceObserver(list => {
    for (const entry of list.getEntries()) {
      window.__longTasks[0] += 1;
      window.__longTasks[1] += entry.duration;
      window.__longTasks[2] = Math.max(window.__longTasks[2], entry.duration);
    }
  }).observe({ type: 'longtask', buffered: true });";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_web_client_stays_fast_and_lean_with_a_large_workspace() {
  let scale = Scale::from_env();
  let Some(rig) = Rig::start().await else {
    return;
  };
  let Rig {
    stack,
    browser,
    mut alice,
    mut bob,
    _turn,
  } = rig;
  let profile = browser.profile_dir().to_string_lossy().into_owned();
  let outcome = std::sync::Arc::new(std::sync::Mutex::new(None));
  let slot = outcome.clone();
  let stack_ref = &stack;
  browser
    .screenshot_on_failure("perf-web", |page| async move {
      // The browser alone (an empty page), to tell its own memory from the client's.
      tokio::time::sleep(Duration::from_secs(2)).await;
      let baseline = mib(ProcessUsage::of_matching(&profile).pss_bytes);
      let (baseline_cpu, baseline_wakeups) = idle_usage(&profile, BASELINE_IDLE).await;
      let started = Instant::now();
      sign_in_and_unlock(&page, "carol").await;
      let first_setup = started.elapsed().as_secs_f64();
      // Carol leaves; the page unloads with the client in it.
      page.goto("about:blank").await.unwrap();
      let started = Instant::now();
      let seeded = seed(stack_ref, &mut alice, &mut bob, scale).await;
      eprintln!("seeded the {} workspace in {:?}", scale.name, started.elapsed());
      let mut report = measure(&page, &seeded, &profile, scale).await;
      scroll_through_held_history(&page, &mut alice, &seeded, scale, &mut report).await;
      report.seconds_sign_in_and_unlock = first_setup;
      report.pss_mib_empty_browser = baseline;
      report.empty_browser_idle_cpu_percent = baseline_cpu;
      report.empty_browser_idle_wakeups_per_second = baseline_wakeups;
      report.print_and_save(scale);
      *slot.lock().unwrap() = Some(report);
      if let Ok(user) = std::env::var("PERF_MOBILE_USER") {
        page.goto("about:blank").await.unwrap();
        serve_a_mobile_device(stack_ref, &mut alice, &seeded, &user).await;
      }
    })
    .await;
  let report = outcome.lock().unwrap().take().unwrap();
  if scale.is_regression_check {
    report.assert_budgets();
  }
  browser.close().await;
  stack.stop().await;
}

/// The size of the generated workspace.
#[derive(Clone, Copy, Debug)]
struct Scale {
  name: &'static str,
  spaces: usize,
  channels_per_space: usize,
  messages: usize,
  long_channel_messages: usize,
  images: usize,
  idle: Duration,
  is_regression_check: bool,
}

/// What the seeding produced, for the measurements to navigate.
struct Seeded {
  spaces: Vec<SpaceId>,
  channels: Vec<ConversationId>,
  /// Channel names of the first space (shown in the sidebar), with each channel's newest message.
  first_space_channels: Vec<(String, String)>,
  long_channel: (String, String),
}

struct Rig {
  stack: Stack,
  browser: BrowserSession,
  /// Owns the spaces and channels.
  alice: Peer,
  /// A second writer, so conversations have more than one author.
  bob: Peer,
  _turn: WebPortLease,
}

/// Every number of one run (written as JSON as-is).
#[derive(Debug, Default)]
struct Report {
  seconds_sign_in_and_unlock: f64,
  seconds_return_to_channel_list: f64,
  seconds_return_until_all_channels_read: f64,
  /// Until the first channel opened shows its newest message (issue #247: the open channel is
  /// caught up first).
  seconds_return_first_channel_read: f64,
  /// Until the client stopped asking the server: every conversation caught up.
  seconds_return_until_caught_up: f64,
  /// From then until the whole browser went quiet (IndexedDB has written the catch-up).
  seconds_return_until_browser_settled: f64,
  /// 1 when the identity provider's session had expired while Carol was away (she signed in again,
  /// which is part of `seconds_return_to_channel_list`).
  signed_in_again: f64,
  /// Switches while the background catch-up runs (issue #326), and when they were done.
  switch_ms_p95_during_catch_up: f64,
  switch_ms_max_during_catch_up: f64,
  seconds_return_busy_switches_done: f64,
  /// Switches right after the catch-up, while IndexedDB may still be writing it.
  switch_ms_p95_after_catch_up: f64,
  switch_ms_max_after_catch_up: f64,
  /// Gateway connections lost while coming back (issue #325: a long sync must keep its heartbeats).
  return_gateway_connections_lost: f64,
  return_long_task_count: f64,
  return_long_task_total_ms: f64,
  return_longest_task_ms: f64,
  seconds_reload_to_channel_list: f64,
  /// From the channel list after the reload until the browser went quiet; the switches and the
  /// idle period are measured after that.
  seconds_reload_until_browser_settled: f64,
  pss_mib_empty_browser: f64,
  empty_browser_idle_cpu_percent: f64,
  empty_browser_idle_wakeups_per_second: f64,
  pss_mib_after_start: f64,
  js_heap_mib_after_start: f64,
  dom_nodes_after_start: f64,
  switch_ms_p50: f64,
  switch_ms_p95: f64,
  switch_ms_max: f64,
  long_channel_open_ms: f64,
  long_channel_rows_in_dom: f64,
  long_channel_rows_after_scrolling_back: f64,
  /// Scrolling up at a steady wheel speed into pages not loaded yet (issue #1050): frames that showed
  /// a placeholder row in the viewport (the user caught up with the loading), the longest frame and
  /// the main-thread tasks over 50 ms meanwhile.
  steady_scroll_frames: f64,
  steady_scroll_frames_with_placeholders: f64,
  steady_scroll_longest_frame_ms: f64,
  steady_scroll_frames_over_33ms: f64,
  steady_scroll_long_tasks: f64,
  steady_scroll_longest_task_ms: f64,
  steady_scroll_frames_moving_backwards: f64,
  scroll_frames: f64,
  scroll_frames_over_50ms: f64,
  scroll_frames_moving_backwards: f64,
  js_heap_mib_after_first_switches: f64,
  js_heap_mib_after_more_switches: f64,
  pss_mib_after_switches: f64,
  idle_seconds: f64,
  idle_cpu_percent: f64,
  idle_wakeups_per_second: f64,
  idle_task_ms_per_minute: f64,
  /// HTTP requests of the page while idle (the budget: none besides the gateway heartbeat).
  idle_http_requests_per_minute: f64,
  /// Of those, requests to TeamCraft's API (the rest: token renewal at the identity provider).
  /// Issue #252: none at all.
  idle_api_requests_per_minute: f64,
  /// CSS animations running when the idle period starts (none expected).
  idle_running_animations: f64,
  /// Gateway frames in both directions while idle (heartbeats and their answers).
  idle_gateway_frames_per_minute: f64,
  /// Memory at the start and the end of the idle period, after a full collection (issue #399: an
  /// idle client must not grow).
  pss_mib_idle_start: f64,
  pss_mib_idle_end: f64,
  js_heap_mib_idle_start: f64,
  js_heap_mib_idle_end: f64,
  wasm_kib: f64,
}

impl Scale {
  fn from_env() -> Self {
    match std::env::var("PERF_SCALE").as_deref() {
      Ok("large") => Self {
        name: "large",
        spaces: 20,
        channels_per_space: 15,
        messages: 100_000,
        long_channel_messages: 20_000,
        images: 200,
        idle: Duration::from_secs(300),
        is_regression_check: false,
      },
      Ok("tiny") => Self {
        name: "tiny",
        spaces: 1,
        channels_per_space: 4,
        messages: 60,
        long_channel_messages: 30,
        images: 2,
        idle: Duration::from_secs(10),
        is_regression_check: false,
      },
      _ => Self {
        name: "small",
        spaces: 3,
        channels_per_space: 8,
        messages: 3_000,
        long_channel_messages: 1_500,
        images: 12,
        idle: Duration::from_secs(60),
        is_regression_check: true,
      },
    }
  }
}

impl Rig {
  async fn start() -> Option<Self> {
    let Some(web_dir) = web_dir() else {
      let is_required = std::env::var("E2E_REQUIRED").is_ok_and(|value| value == "1");
      assert!(!is_required, "no web build: run tests/e2e/web/build.sh");
      eprintln!("skipping: no web build (tests/e2e/web/build.sh)");
      return None;
    };
    let builder = Stack::builder()
      .with_port(PORT)
      .with_web_dir(web_dir)
      .with_env("TEAMCRAFT_WEB__CONTENT_SECURITY_POLICY", TEST_CSP)
      // Seeding writes far faster than a person; the rate limits would throttle it.
      .with_env("TEAMCRAFT_SECURITY__RATE_LIMITS__IS_ENABLED", "false")
      // More connections than the harness default: the seeding peers and the browser in parallel.
      .with_env("TEAMCRAFT_DATABASE__MAX_CONNECTIONS", "16");
    let turn = lease_web_port(PORT).await;
    let stack = builder.start().await?;
    let browser = BrowserSession::launch_for_measurements().await?;
    let alice = Peer::enroll(&stack, "alice").await;
    let bob = Peer::enroll(&stack, "bob").await;
    // Carol's keys exist before the browser signs in (it unlocks them); her first device then
    // stays offline. The channels are created later, so the browser gets every welcome itself.
    drop(Peer::enroll(&stack, "carol").await);
    Some(Self {
      stack,
      browser,
      alice,
      bob,
      _turn: turn,
    })
  }
}

/// Signs in through Keycloak and unlocks the account's keys with the recovery passphrase.
async fn sign_in_and_unlock(page: &Page, user: &str) {
  page.goto(format!("{WEB_ORIGIN}/healthz")).await.unwrap();
  page
    .evaluate("localStorage.setItem('teamcraft.locale', 'en')")
    .await
    .unwrap();
  page.goto(format!("{WEB_ORIGIN}/?backend=server")).await.unwrap();
  wait_for_text(page, "Sign in").await;
  click_text(page, "Sign in").await;
  sign_in_at_keycloak(page, user).await;
  // The setup screen may render again while the core settles and lose what was typed (#444).
  type_until_kept(page, "#enroll-secret", PASSPHRASE).await;
  click_text(page, "Unlock").await;
  wait_for_text(page, "Activity").await;
}

/// Alice owns every space; Bob and Carol (the browser) are in all of them. Channels get their
/// share of the messages round-robin, the first channel of the first space a long history, and
/// images are spread over the first space.
async fn seed(stack: &Stack, alice: &mut Peer, bob: &mut Peer, scale: Scale) -> Seeded {
  let carol_tokens = stack.tokens("carol");
  let carol: UserId = stack.get(&carol_tokens, "/me").await.id().parse().unwrap();
  let mut channels: Vec<(ConversationId, String)> = Vec::new();
  let mut spaces = Vec::new();
  for space_index in 0..scale.spaces {
    let space = space_with(
      stack,
      alice,
      &format!("Space {space_index:02}"),
      &[(&*bob, "member")],
    )
    .await;
    stack
      .add_space_member(&alice.tokens, &space.to_string(), &carol.to_string(), "member")
      .await;
    spaces.push(space);
    for channel_index in 0..scale.channels_per_space {
      let name = format!("s{space_index:02}c{channel_index:03}");
      let channel = alice.client.create_channel(space, &name).await.unwrap();
      alice.client.add_member(channel, bob.user_id()).await.unwrap();
      alice.client.add_member(channel, carol).await.unwrap();
      channels.push((channel, name));
    }
  }
  // Bob idled while the browser was set up, so his gateway may have been closed meanwhile.
  let deadline = Instant::now() + Duration::from_secs(300);
  while !channels
    .iter()
    .all(|(channel, _)| bob.client.has_group(*channel).unwrap())
  {
    assert!(Instant::now() < deadline, "Bob did not join every channel");
    bob.pump_reconnecting().await;
    bob.heartbeat().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
  }

  // Two writers in parallel, each its own client: Alice fills the long channel and half of the
  // rest, Bob the other half.
  let (long_channel, _) = channels[0];
  let rest = scale.messages.saturating_sub(scale.long_channel_messages);
  let others: Vec<ConversationId> = channels[1..].iter().map(|(channel, _)| *channel).collect();
  let alice_writes = async {
    for index in 0..scale.long_channel_messages {
      send(alice, long_channel, index).await;
    }
    for index in (0..rest).step_by(2) {
      send(alice, others[index % others.len()], index).await;
    }
  };
  let bob_writes = async {
    for index in (1..rest).step_by(2) {
      send(bob, others[index % others.len()], index).await;
    }
  };
  tokio::join!(alice_writes, bob_writes);
  let first_space = &channels[..scale.channels_per_space];
  for index in 0..scale.images {
    let (channel, _) = first_space[index % first_space.len()];
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
  // The newest message of every channel of the first space marks "this channel is shown".
  let mut first_space_channels = Vec::new();
  for (channel, name) in first_space {
    let last = format!("newest in {name}");
    alice.client.send_text(*channel, None, &last).await.unwrap();
    first_space_channels.push((name.clone(), last));
  }
  Seeded {
    spaces,
    channels: channels.iter().map(|(channel, _)| *channel).collect(),
    long_channel: first_space_channels[0].clone(),
    first_space_channels,
  }
}

/// For measuring the desktop and mobile apps against the same workspace (`PERF_MOBILE_USER`,
/// `PERF_MOBILE_DEVICES`, `PERF_HOLD_SECS`): waits until `user` has set up that many devices, adds
/// them everywhere with the full history, then keeps the server running.
async fn serve_a_mobile_device(stack: &Stack, alice: &mut Peer, seeded: &Seeded, user: &str) {
  let tokens = stack.tokens(user);
  let user_id: UserId = stack.get(&tokens, "/me").await.id().parse().unwrap();
  for space in &seeded.spaces {
    stack
      .add_space_member(&alice.tokens, &space.to_string(), &user_id.to_string(), "member")
      .await;
  }
  eprintln!(
    "PERF: sign in as {user} on the device now (server {})",
    stack.base_url
  );
  let wanted: usize = std::env::var("PERF_MOBILE_DEVICES")
    .ok()
    .and_then(|value| value.parse().ok())
    .unwrap_or(1);
  let deadline = Instant::now() + Duration::from_secs(3600);
  loop {
    let devices = stack
      .get(&alice.tokens, &format!("/users/{user_id}/devices"))
      .await;
    if devices.body["devices"]
      .as_array()
      .is_some_and(|list| list.len() >= wanted)
    {
      break;
    }
    assert!(
      Instant::now() < deadline,
      "{wanted} devices of {user} did not appear"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
  }
  // Its key packages are uploaded right after the device is registered.
  tokio::time::sleep(Duration::from_secs(15)).await;
  for channel in &seeded.channels {
    alice
      .client
      .add_member_with_history(*channel, user_id, Some(HistoryPolicy::Full))
      .await
      .unwrap();
  }
  let hold = std::env::var("PERF_HOLD_SECS")
    .ok()
    .and_then(|value| value.parse().ok())
    .unwrap_or(3600);
  eprintln!(
    "PERF: {user} is in {} channels; the server stays up for {hold} s",
    seeded.channels.len()
  );
  tokio::time::sleep(Duration::from_secs(hold)).await;
}

async fn send(sender: &mut Peer, channel: ConversationId, index: usize) {
  let body = format!("{} ({index})", SENTENCES[index % SENTENCES.len()]);
  sender.client.send_text(channel, None, &body).await.unwrap();
}

/// Scrolling back through history the client holds, as the desktop app does after weeks in a busy
/// channel (issue #1050). The browser only has the newest page of a channel it joined; history it
/// was away for is caught up completely. So Carol leaves, the long channel grows by
/// `long_channel_messages`, and she comes back and scrolls up steadily from the newest message.
async fn scroll_through_held_history(
  page: &Page,
  alice: &mut Peer,
  seeded: &Seeded,
  scale: Scale,
  report: &mut Report,
) {
  page.goto("about:blank").await.unwrap();
  let long_channel = seeded.channels[0];
  for index in 0..scale.long_channel_messages {
    send(alice, long_channel, index).await;
  }
  let last = "newest of the held history";
  alice.client.send_text(long_channel, None, last).await.unwrap();
  page
    .evaluate(format!("location.href = '{WEB_ORIGIN}/?backend=server'"))
    .await
    .unwrap();
  come_back(page, &seeded.first_space_channels[1].0).await;
  wait_until_done(page).await;
  switch_within(page, &seeded.long_channel.0, last, Duration::from_secs(120)).await;
  settle(page).await;
  // PERF_PROFILE_SCROLL=1 records a CPU profile of the scrolling (open it in Chrome's DevTools).
  let is_profiling = std::env::var("PERF_PROFILE_SCROLL").is_ok_and(|value| value == "1");
  if is_profiling {
    page.execute(ProfilerEnableParams::default()).await.unwrap();
    page.execute(SetSamplingIntervalParams::new(200)).await.unwrap();
    page.execute(ProfilerStartParams::default()).await.unwrap();
  }
  let steady = scroll_back_steadily(page).await;
  if is_profiling {
    let profile = page.execute(ProfilerStopParams::default()).await.unwrap();
    let path = artifact_dir().join("perf-scroll.cpuprofile");
    let _ = std::fs::create_dir_all(artifact_dir());
    std::fs::write(&path, serde_json::to_vec(&profile.result.profile).unwrap()).unwrap();
    eprintln!("scroll profile: {}", path.display());
  }
  report.steady_scroll_frames = steady[0];
  report.steady_scroll_frames_with_placeholders = steady[1];
  report.steady_scroll_longest_frame_ms = steady[2];
  report.steady_scroll_frames_over_33ms = steady[3];
  report.steady_scroll_long_tasks = steady[4];
  report.steady_scroll_longest_task_ms = steady[5];
  report.steady_scroll_frames_moving_backwards = steady[6];
}

async fn measure(page: &Page, seeded: &Seeded, profile: &str, scale: Scale) -> Report {
  let mut report = Report::default();
  page.execute(EnableParams::default()).await.unwrap();

  // Carol comes back: the client starts, catches up (welcomes, MLS, messages) and shows the list.
  // Navigation is started from the page, without waiting for its load event: the client may keep
  // the page's only thread busy for a long time, which is part of what is measured here.
  page.evaluate_on_new_document(LONG_TASK_RECORDER).await.unwrap();
  let started = Instant::now();
  let catch_up_log = watch_catch_up_log(page, started).await;
  page
    .evaluate(format!("location.href = '{WEB_ORIGIN}/?backend=server'"))
    .await
    .unwrap();
  let had_to_sign_in = come_back(page, &seeded.first_space_channels[1].0).await;
  report.signed_in_again = if had_to_sign_in { 1.0 } else { 0.0 };
  report.seconds_return_to_channel_list = started.elapsed().as_secs_f64();
  for (index, (name, last)) in seeded.first_space_channels.iter().enumerate() {
    switch_within(page, name, last, Duration::from_secs(280)).await;
    if index == 0 {
      report.seconds_return_first_channel_read = started.elapsed().as_secs_f64();
    }
  }
  report.seconds_return_until_all_channels_read = started.elapsed().as_secs_f64();
  // Switching while the rest of the workspace is caught up in the background (issue #326).
  let mut times = busy_switches(page, &seeded.first_space_channels[1..]).await;
  report.switch_ms_p95_during_catch_up = percentile(&times, 95);
  report.switch_ms_max_during_catch_up = times.last().copied().unwrap_or(0.0);
  report.seconds_return_busy_switches_done = started.elapsed().as_secs_f64();
  // The rest of the workspace is caught up in the background (issue #247): until the sync lets go
  // of the connection and the client stops asking the server.
  report.seconds_return_until_caught_up = wait_until_done(page).await.duration_since(started).as_secs_f64();
  // Right after the catch-up, while the browser may still write it to IndexedDB (issue #326).
  times = busy_switches(page, &seeded.first_space_channels[1..]).await;
  report.switch_ms_p95_after_catch_up = percentile(&times, 95);
  report.switch_ms_max_after_catch_up = times.last().copied().unwrap_or(0.0);
  let lines = catch_up_log.lock().unwrap().clone();
  for (at, line) in &lines {
    eprintln!("catch-up log {at:>7.1} s  {line}");
  }
  report.return_gateway_connections_lost = lines
    .iter()
    .filter(|(_, line)| line.contains("gateway connection lost"))
    .count() as f64;
  let long_tasks = evaluate_value(page, "window.__longTasks || [0, 0, 0]".into()).await;
  report.return_long_task_count = long_tasks[0].as_f64().unwrap_or(0.0);
  report.return_long_task_total_ms = long_tasks[1].as_f64().unwrap_or(0.0);
  report.return_longest_task_ms = long_tasks[2].as_f64().unwrap_or(0.0);
  report.seconds_return_until_browser_settled = wait_until_browser_settled(profile).await.as_secs_f64();

  // Cold start of a signed-in client: reload until the channel list is back.
  let started = Instant::now();
  page.evaluate("location.reload()").await.unwrap();
  wait_long_for_text(page, &seeded.first_space_channels[1].0).await;
  report.seconds_reload_to_channel_list = started.elapsed().as_secs_f64();
  wait_until_done(page).await;
  report.seconds_reload_until_browser_settled = wait_until_browser_settled(profile).await.as_secs_f64();
  settle(page).await;
  report.pss_mib_after_start = mib(ProcessUsage::of_matching(profile).pss_bytes);
  report.js_heap_mib_after_start = metric(page, "JSHeapUsedSize").await / MIB;
  report.dom_nodes_after_start = metric(page, "Nodes").await;

  // Channel switches between the short channels of the first space.
  let short_channels = &seeded.first_space_channels[1..];
  let mut times = Vec::new();
  for index in 0..SWITCHES {
    let (name, last) = &short_channels[index % short_channels.len()];
    times.push(switch_to(page, name, last).await);
  }
  times.sort_by(f64::total_cmp);
  report.switch_ms_p50 = percentile(&times, 50);
  report.switch_ms_p95 = percentile(&times, 95);
  report.switch_ms_max = times.last().copied().unwrap_or(0.0);
  collect_garbage(page).await;
  report.js_heap_mib_after_first_switches = metric(page, "JSHeapUsedSize").await / MIB;

  // The long channel: open, count rendered rows (is the list virtualised?), scroll back.
  let (long_name, long_last) = &seeded.long_channel;
  report.long_channel_open_ms = switch_to(page, long_name, long_last).await;
  report.long_channel_rows_in_dom = rows_in_dom(page).await;
  let (frames, slow_frames, backward_frames) = scroll_back(page).await;
  report.scroll_frames = frames;
  report.scroll_frames_over_50ms = slow_frames;
  report.scroll_frames_moving_backwards = backward_frames;
  report.long_channel_rows_after_scrolling_back = rows_in_dom(page).await;

  // Leak check: 100 more switches, then compare the heap after a full collection.
  for index in 0..LEAK_CHECK_SWITCHES {
    let (name, last) = &short_channels[index % short_channels.len()];
    switch_to(page, name, last).await;
  }
  collect_garbage(page).await;
  report.js_heap_mib_after_more_switches = metric(page, "JSHeapUsedSize").await / MIB;
  report.pss_mib_after_switches = mib(ProcessUsage::of_matching(profile).pss_bytes);

  // Idle in the foreground: nothing happens, so the client should not use the CPU. What the reload
  // and the switches started is finished first.
  wait_until_done(page).await;
  report.idle_running_animations = running_animations(page).await;
  collect_garbage(page).await;
  report.js_heap_mib_idle_start = metric(page, "JSHeapUsedSize").await / MIB;
  report.pss_mib_idle_start = mib(ProcessUsage::of_matching(profile).pss_bytes);
  let kinds_before = ProcessUsage::by_kind(profile);
  let usage_before = ProcessUsage::of_matching(profile);
  let tasks_before = metric(page, "TaskDuration").await;
  let traffic = count_traffic(page, scale.idle).await;
  let usage_after = ProcessUsage::of_matching(profile);
  let tasks_after = metric(page, "TaskDuration").await;
  collect_garbage(page).await;
  report.js_heap_mib_idle_end = metric(page, "JSHeapUsedSize").await / MIB;
  report.pss_mib_idle_end = mib(ProcessUsage::of_matching(profile).pss_bytes);
  for (kind, after) in ProcessUsage::by_kind(profile) {
    let before = kinds_before.get(&kind).copied().unwrap_or_default();
    eprintln!(
      "idle cpu {kind}: {:.1} %, {:.0} wakeups/s",
      after.cpu_time.saturating_sub(before.cpu_time).as_secs_f64() / scale.idle.as_secs_f64() * 100.0,
      after.context_switches.saturating_sub(before.context_switches) as f64 / scale.idle.as_secs_f64()
    );
  }
  let idle_seconds = scale.idle.as_secs_f64();
  report.idle_seconds = idle_seconds;
  report.idle_cpu_percent =
    (usage_after.cpu_time.saturating_sub(usage_before.cpu_time)).as_secs_f64() / idle_seconds * 100.0;
  report.idle_wakeups_per_second = (usage_after
    .context_switches
    .saturating_sub(usage_before.context_switches)) as f64
    / idle_seconds;
  report.idle_task_ms_per_minute = (tasks_after - tasks_before) * 1000.0 * 60.0 / idle_seconds;
  report.idle_http_requests_per_minute = traffic.http_requests * 60.0 / idle_seconds;
  report.idle_api_requests_per_minute = traffic.api_requests * 60.0 / idle_seconds;
  report.idle_gateway_frames_per_minute = traffic.gateway_frames * 60.0 / idle_seconds;
  if std::env::var_os("PERF_SECOND_IDLE").is_some() {
    // By hand: does the idle CPU fall further once the browser has digested the catch-up?
    let before = ProcessUsage::by_kind(profile);
    tokio::time::sleep(scale.idle).await;
    for (kind, after) in ProcessUsage::by_kind(profile) {
      let earlier = before.get(&kind).copied().unwrap_or_default();
      eprintln!(
        "second idle cpu {kind}: {:.1} %",
        after.cpu_time.saturating_sub(earlier.cpu_time).as_secs_f64() / scale.idle.as_secs_f64() * 100.0
      );
    }
  }
  report.wasm_kib = wasm_bytes() as f64 / 1024.0;
  report
}

/// [`BUSY_SWITCHES`] switches between the first space's channels, sorted. Generous timeout: the
/// channel may still be catching up.
async fn busy_switches(page: &Page, channels: &[(String, String)]) -> Vec<f64> {
  let mut times = Vec::with_capacity(BUSY_SWITCHES);
  for index in 0..BUSY_SWITCHES {
    let (name, last) = &channels[index % channels.len()];
    times.push(switch_within(page, name, last, Duration::from_secs(280)).await);
  }
  times.sort_by(f64::total_cmp);
  times
}

/// The client's console lines about the catch-up and the gateway (phase timings, lost
/// connections), with the seconds since `since`.
async fn watch_catch_up_log(page: &Page, since: Instant) -> Arc<Mutex<Vec<(f64, String)>>> {
  const WATCHED: [&str; 4] = [
    "catch-up",
    "full sync",
    "gateway connection lost",
    "gateway connected",
  ];
  let lines = Arc::new(Mutex::new(Vec::new()));
  let sink = lines.clone();
  let mut events = page.event_listener::<EventConsoleApiCalled>().await.unwrap();
  tokio::spawn(async move {
    while let Some(event) = events.next().await {
      let text: Vec<String> = event
        .args
        .iter()
        .map(|arg| {
          arg
            .value
            .as_ref()
            .map_or_else(|| arg.description.clone().unwrap_or_default(), |v| v.to_string())
        })
        .collect();
      let text = text.join(" ");
      if WATCHED.iter().any(|watched| text.contains(watched)) {
        sink.lock().unwrap().push((since.elapsed().as_secs_f64(), text));
      }
    }
  });
  lines
}

fn mib(bytes: u64) -> f64 {
  bytes as f64 / MIB
}

fn percentile(sorted: &[f64], percent: usize) -> f64 {
  if sorted.is_empty() {
    return 0.0;
  }
  let index = (sorted.len() * percent / 100).min(sorted.len() - 1);
  sorted[index]
}

/// Waits `period` while counting the page's HTTP requests and gateway frames (DevTools network
/// events; the service worker's own requests are not the page's and are not counted).
async fn count_traffic(page: &Page, period: Duration) -> Traffic {
  page.execute(NetworkEnableParams::default()).await.unwrap();
  let mut requests = page.event_listener::<EventRequestWillBeSent>().await.unwrap();
  let mut sent = page.event_listener::<EventWebSocketFrameSent>().await.unwrap();
  let mut received = page
    .event_listener::<EventWebSocketFrameReceived>()
    .await
    .unwrap();
  let mut traffic = Traffic::default();
  let deadline = tokio::time::sleep(period);
  tokio::pin!(deadline);
  loop {
    tokio::select! {
      () = &mut deadline => break,
      Some(request) = requests.next() => {
        traffic.http_requests += 1.0;
        if request.request.url.starts_with(&format!("{WEB_ORIGIN}/api/")) {
          eprintln!("request while idle: {}", request.request.url);
          traffic.api_requests += 1.0;
        }
      },
      Some(_) = sent.next() => traffic.gateway_frames += 1.0,
      Some(_) = received.next() => traffic.gateway_frames += 1.0,
    }
  }
  traffic
}

/// What the page sent and received over a period.
#[derive(Debug, Default)]
struct Traffic {
  http_requests: f64,
  api_requests: f64,
  gateway_frames: f64,
}

/// Waits until the client is done with what it was doing: connected (no connection hint or notice
/// for [`QUIET`]: a sync holds the connection, and the hint shows after the grace period) and no
/// API request for [`QUIET`]. Returns when the connection hint went away for good.
async fn wait_until_done(page: &Page) -> Instant {
  let deadline = Instant::now() + LONG_WAIT;
  let mut maybe_connected_since: Option<Instant> = None;
  loop {
    assert!(Instant::now() < deadline, "the client never settled");
    let has_banner = evaluate_value(
      page,
      "!!document.querySelector('.connection-hint, .connection-notice')".into(),
    )
    .await
    .as_bool()
    .unwrap_or(true);
    match (has_banner, maybe_connected_since) {
      (true, _) => maybe_connected_since = None,
      (false, None) => maybe_connected_since = Some(Instant::now()),
      (false, Some(since)) if since.elapsed() >= QUIET => break,
      (false, Some(_)) => {},
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
  }
  let connected_at = maybe_connected_since.unwrap_or_else(Instant::now);
  wait_until_quiet(page).await;
  connected_at
}

/// CSS animations running on the page (a spinner or a loading placeholder left on screen keeps
/// the compositor busy while the page is idle); prints what they animate.
async fn running_animations(page: &Page) -> f64 {
  let found = evaluate_value(
    page,
    "document.getAnimations().filter(a => a.playState === 'running').map(a => { \
       const el = a.effect && a.effect.target; \
       return el ? `${el.tagName.toLowerCase()}.${el.className} in ${el.closest('[data-conversation], .row, section')?.className ?? '-'}` : '?'; \
     })"
      .into(),
  )
  .await;
  let found = found.as_array().cloned().unwrap_or_default();
  for animation in found.iter().take(10) {
    eprintln!("running animation: {animation}");
  }
  found.len() as f64
}

/// Waits until the page made no API request for [`QUIET`] (at most [`LONG_WAIT`]).
async fn wait_until_quiet(page: &Page) {
  page.execute(NetworkEnableParams::default()).await.unwrap();
  let mut requests = page.event_listener::<EventRequestWillBeSent>().await.unwrap();
  let deadline = Instant::now() + LONG_WAIT;
  let api = format!("{WEB_ORIGIN}/api/");
  loop {
    match tokio::time::timeout(QUIET, requests.next()).await {
      Err(_) => return,
      Ok(Some(request)) if request.request.url.starts_with(&api) => {
        assert!(Instant::now() < deadline, "the client never went quiet");
      },
      Ok(Some(_)) => {},
      Ok(None) => return,
    }
  }
}

/// Waits until the browser's processes together stay under [`SETTLED_CPU_PERCENT`] for a
/// [`SETTLE_WINDOW`] (at most [`LONG_WAIT`]); returns how long that took.
async fn wait_until_browser_settled(profile: &str) -> Duration {
  let started = Instant::now();
  loop {
    let (cpu, _) = idle_usage(profile, SETTLE_WINDOW).await;
    if cpu < SETTLED_CPU_PERCENT {
      return started.elapsed().saturating_sub(SETTLE_WINDOW);
    }
    eprintln!("browser still busy: {cpu:.1} % CPU");
    assert!(started.elapsed() < LONG_WAIT, "the browser never settled");
  }
}

/// CPU (percent of one core) and wakeups per second of the browser's processes over `period`.
async fn idle_usage(profile: &str, period: Duration) -> (f64, f64) {
  let before = ProcessUsage::of_matching(profile);
  tokio::time::sleep(period).await;
  let after = ProcessUsage::of_matching(profile);
  let seconds = period.as_secs_f64();
  (
    after.cpu_time.saturating_sub(before.cpu_time).as_secs_f64() / seconds * 100.0,
    after.context_switches.saturating_sub(before.context_switches) as f64 / seconds,
  )
}

/// Waits until the app shows `needle` after Carol came back. If the identity provider's session
/// expired meanwhile (a long seeding run), she signs in again first; returns whether she had to.
async fn come_back(page: &Page, needle: &str) -> bool {
  let deadline = Instant::now() + LONG_WAIT;
  let mut has_signed_in = false;
  loop {
    let text = page
      .evaluate("document.body ? document.body.innerText : ''")
      .await
      .ok()
      .and_then(|result| result.into_value::<String>().ok())
      .unwrap_or_default();
    if text.contains(needle) {
      return has_signed_in;
    }
    if !has_signed_in && page.find_element("input[name=username]").await.is_ok() {
      sign_in_at_keycloak(page, "carol").await;
      has_signed_in = true;
    } else if !has_signed_in && text.contains("Sign in") && !text.contains("Activity") {
      click_text(page, "Sign in").await;
    } else if text.contains("Activity") {
      // The app opens on Chats (#558); the seeded channels are in the first space.
      let _ = page
        .evaluate("document.querySelector('.space-btn[aria-label^=\"Space 00\"]')?.click()")
        .await;
    }
    assert!(
      Instant::now() < deadline,
      "{needle:?} did not appear within {LONG_WAIT:?}"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}

/// Fills in Keycloak's login form.
async fn sign_in_at_keycloak(page: &Page, user: &str) {
  let username = wait_for_element(page, "input[name=username]").await;
  username.click().await.unwrap().type_str(user).await.unwrap();
  let password = page.find_element("input[name=password]").await.unwrap();
  password
    .click()
    .await
    .unwrap()
    .type_str(format!("{user}-dev-pass"))
    .await
    .unwrap()
    .press_key("Enter")
    .await
    .unwrap();
}

/// Like `wait_for_text`, for a page that may stay unresponsive for minutes.
async fn wait_long_for_text(page: &Page, needle: &str) {
  let deadline = Instant::now() + LONG_WAIT;
  loop {
    let text = page
      .evaluate("document.body ? document.body.innerText : ''")
      .await
      .ok()
      .and_then(|result| result.into_value::<String>().ok())
      .unwrap_or_default();
    if text.contains(needle) {
      return;
    }
    assert!(
      Instant::now() < deadline,
      "{needle:?} did not appear within {LONG_WAIT:?}"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}

/// Lets startup work (decryption, image thumbnails) finish before memory is read.
async fn settle(page: &Page) {
  tokio::time::sleep(Duration::from_secs(5)).await;
  collect_garbage(page).await;
}

async fn collect_garbage(page: &Page) {
  page.execute(CollectGarbageParams::default()).await.unwrap();
}

async fn metric(page: &Page, name: &str) -> f64 {
  let metrics = page.execute(GetMetricsParams::default()).await.unwrap();
  metrics
    .result
    .metrics
    .iter()
    .find(|metric| metric.name == name)
    .map_or(0.0, |metric| metric.value)
}

async fn evaluate_value(page: &Page, script: String) -> serde_json::Value {
  let params = EvaluateParams::builder()
    .expression(script)
    .await_promise(true)
    .return_by_value(true)
    .build()
    .unwrap();
  page.evaluate(params).await.unwrap().into_value().unwrap()
}

/// Clicks the sidebar entry named `name` and measures until the message list shows `last`
/// (milliseconds, measured in the page and polled once per frame). In a channel with unread
/// messages that includes the click on "jump to the newest".
async fn switch_to(page: &Page, name: &str, last: &str) -> f64 {
  switch_within(page, name, last, Duration::from_secs(30)).await
}

async fn switch_within(page: &Page, name: &str, last: &str, timeout: Duration) -> f64 {
  let timeout_ms = timeout.as_millis();
  let script = format!(
    r#"new Promise((resolve, reject) => {{
      const entry = [...document.querySelectorAll('.sidebar .sb-item')]
        .find(e => (e.querySelector('.sb-name')?.innerText ?? '').trim() === {name:?});
      if (!entry) {{ reject('no sidebar entry ' + {name:?}); return; }}
      const started = performance.now();
      entry.click();
      const check = () => {{
        const list = document.querySelector('.message-list');
        if (list && list.innerText.includes({last:?})) {{ resolve(performance.now() - started); return; }}
        // An unread channel opens at the first unread message; the newest is one click away.
        const jump = document.querySelector('.jump-latest');
        if (jump) jump.click();
        if (performance.now() - started > {timeout_ms}) {{
          reject('channel did not show ' + {last:?} + '; the list shows: ' + (list ? list.innerText.slice(-800) : 'no list'));
          return;
        }}
        requestAnimationFrame(check);
      }};
      requestAnimationFrame(check);
    }})"#
  );
  evaluate_value(page, script).await.as_f64().unwrap()
}

async fn rows_in_dom(page: &Page) -> f64 {
  evaluate_value(
    page,
    "document.querySelectorAll('.message-list .rows > .row').length".into(),
  )
  .await
  .as_f64()
  .unwrap()
}

/// Scrolls the long channel upwards at a steady, brisk wheel speed (60 px a frame, about 3,600 px
/// a second, for 400 frames: several pages of older messages) and returns `[frames, frames with a
/// placeholder row in the viewport, longest frame ms, frames over 33 ms, long tasks, longest task
/// ms, frames that moved the list backwards]`. A placeholder in sight means the user outran the
/// loading; at this speed the next page has to be there before its rows come into view (#1050).
async fn scroll_back_steadily(page: &Page) -> Vec<f64> {
  let script = r#"new Promise(resolve => {
    const list = document.querySelector('.message-list');
    const tasks = [0, 0];
    const observer = new PerformanceObserver(entries => {
      for (const entry of entries.getEntries()) {
        tasks[0] += 1;
        tasks[1] = Math.max(tasks[1], entry.duration);
      }
    });
    observer.observe({ type: 'longtask' });
    let frames = 0, placeholders = 0, longest = 0, over33 = 0, backwards = 0, last = null, tracked = null;
    const viewport = () => list.getBoundingClientRect();
    const pick = () => {
      const top = viewport().top;
      const row = [...list.querySelectorAll('.row[data-idx]')].find(row => row.getBoundingClientRect().top > top);
      tracked = row ? { idx: row.dataset.idx, top: row.getBoundingClientRect().top } : null;
    };
    const check = () => {
      const row = tracked && list.querySelector(`.row[data-idx="${tracked.idx}"]`);
      if (row && row.getBoundingClientRect().top < tracked.top - 1) backwards += 1;
      const { top, bottom } = viewport();
      const isWaiting = [...list.querySelectorAll('.row .msg-skeleton')].some(skeleton => {
        const rect = skeleton.getBoundingClientRect();
        return rect.bottom > top && rect.top < bottom;
      });
      if (isWaiting) placeholders += 1;
    };
    const tick = now => {
      if (last !== null) {
        const frame = now - last;
        longest = Math.max(longest, frame);
        if (frame > 33) over33 += 1;
      }
      last = now;
      frames += 1;
      if (frames <= 400 && list.scrollTop > 0) {
        pick();
        list.dispatchEvent(new WheelEvent('wheel', { deltaY: -60, bubbles: true }));
        list.scrollTop = Math.max(0, list.scrollTop - 60);
        setTimeout(check, 0);
        requestAnimationFrame(tick);
      } else {
        observer.disconnect();
        resolve([frames, placeholders, longest, over33, tasks[0], tasks[1], backwards]);
      }
    };
    requestAnimationFrame(tick);
  })"#;
  let values = evaluate_value(page, script.into()).await;
  (0..7).map(|index| values[index].as_f64().unwrap()).collect()
}

/// Scrolls the long channel upwards in steps (loading older pages as a user would) and counts
/// frames, frames slower than 50 ms and frames that moved the list backwards meanwhile (#642): the
/// row at the top may only move down while scrolling up, checked after the frame was painted (the
/// list corrects row heights in its own frame callbacks). Returns `(frames, slow, backwards)`.
async fn scroll_back(page: &Page) -> (f64, f64, f64) {
  let script = r#"new Promise(resolve => {
    const list = document.querySelector('.message-list');
    let frames = 0, slow = 0, backwards = 0, last = performance.now(), steps = 0, tracked = null;
    const pick = () => {
      const top = list.getBoundingClientRect().top;
      const row = [...list.querySelectorAll('.row[data-idx]')].find(row => row.getBoundingClientRect().top > top);
      tracked = row ? { idx: row.dataset.idx, top: row.getBoundingClientRect().top } : null;
    };
    const check = () => {
      const row = tracked && list.querySelector(`.row[data-idx="${tracked.idx}"]`);
      if (row && row.getBoundingClientRect().top < tracked.top - 1) backwards += 1;
    };
    const tick = now => {
      frames += 1;
      if (now - last > 50) slow += 1;
      last = now;
      if (steps < 120) {
        pick();
        // A person scrolls with the wheel (or a finger, or keys): only that leaves the bottom
        // (#665). The synthetic event says so; the step itself is set directly.
        list.dispatchEvent(new WheelEvent('wheel', { deltaY: -600, bubbles: true }));
        list.scrollTop = Math.max(0, list.scrollTop - 600);
        steps += 1;
        setTimeout(check, 0);
        requestAnimationFrame(tick);
      } else resolve([frames, slow, backwards]);
    };
    requestAnimationFrame(tick);
  })"#;
  let values = evaluate_value(page, script.into()).await;
  (
    values[0].as_f64().unwrap(),
    values[1].as_f64().unwrap(),
    values[2].as_f64().unwrap(),
  )
}

fn wasm_bytes() -> u64 {
  let Some(dir) = web_dir() else { return 0 };
  std::fs::read_dir(dir)
    .map(|entries| {
      entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "wasm"))
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum()
    })
    .unwrap_or(0)
}

impl Report {
  fn rows(&self) -> Vec<(&'static str, f64)> {
    vec![
      ("seconds_sign_in_and_unlock", self.seconds_sign_in_and_unlock),
      (
        "seconds_return_to_channel_list",
        self.seconds_return_to_channel_list,
      ),
      (
        "seconds_return_first_channel_read",
        self.seconds_return_first_channel_read,
      ),
      (
        "seconds_return_until_all_channels_read",
        self.seconds_return_until_all_channels_read,
      ),
      (
        "seconds_return_until_browser_settled",
        self.seconds_return_until_browser_settled,
      ),
      (
        "seconds_reload_until_browser_settled",
        self.seconds_reload_until_browser_settled,
      ),
      (
        "seconds_return_until_caught_up",
        self.seconds_return_until_caught_up,
      ),
      ("signed_in_again", self.signed_in_again),
      (
        "switch_ms_p95_during_catch_up",
        self.switch_ms_p95_during_catch_up,
      ),
      (
        "switch_ms_max_during_catch_up",
        self.switch_ms_max_during_catch_up,
      ),
      (
        "seconds_return_busy_switches_done",
        self.seconds_return_busy_switches_done,
      ),
      ("switch_ms_p95_after_catch_up", self.switch_ms_p95_after_catch_up),
      ("switch_ms_max_after_catch_up", self.switch_ms_max_after_catch_up),
      (
        "return_gateway_connections_lost",
        self.return_gateway_connections_lost,
      ),
      ("return_long_task_count", self.return_long_task_count),
      ("return_long_task_total_ms", self.return_long_task_total_ms),
      ("return_longest_task_ms", self.return_longest_task_ms),
      (
        "seconds_reload_to_channel_list",
        self.seconds_reload_to_channel_list,
      ),
      ("pss_mib_empty_browser", self.pss_mib_empty_browser),
      (
        "empty_browser_idle_cpu_percent",
        self.empty_browser_idle_cpu_percent,
      ),
      (
        "empty_browser_idle_wakeups_per_second",
        self.empty_browser_idle_wakeups_per_second,
      ),
      ("pss_mib_after_start", self.pss_mib_after_start),
      ("js_heap_mib_after_start", self.js_heap_mib_after_start),
      ("dom_nodes_after_start", self.dom_nodes_after_start),
      ("switch_ms_p50", self.switch_ms_p50),
      ("switch_ms_p95", self.switch_ms_p95),
      ("switch_ms_max", self.switch_ms_max),
      ("long_channel_open_ms", self.long_channel_open_ms),
      ("long_channel_rows_in_dom", self.long_channel_rows_in_dom),
      (
        "long_channel_rows_after_scrolling_back",
        self.long_channel_rows_after_scrolling_back,
      ),
      ("steady_scroll_frames", self.steady_scroll_frames),
      (
        "steady_scroll_frames_with_placeholders",
        self.steady_scroll_frames_with_placeholders,
      ),
      (
        "steady_scroll_longest_frame_ms",
        self.steady_scroll_longest_frame_ms,
      ),
      (
        "steady_scroll_frames_over_33ms",
        self.steady_scroll_frames_over_33ms,
      ),
      ("steady_scroll_long_tasks", self.steady_scroll_long_tasks),
      (
        "steady_scroll_longest_task_ms",
        self.steady_scroll_longest_task_ms,
      ),
      (
        "steady_scroll_frames_moving_backwards",
        self.steady_scroll_frames_moving_backwards,
      ),
      ("scroll_frames", self.scroll_frames),
      ("scroll_frames_over_50ms", self.scroll_frames_over_50ms),
      (
        "scroll_frames_moving_backwards",
        self.scroll_frames_moving_backwards,
      ),
      (
        "js_heap_mib_after_first_switches",
        self.js_heap_mib_after_first_switches,
      ),
      (
        "js_heap_mib_after_more_switches",
        self.js_heap_mib_after_more_switches,
      ),
      ("pss_mib_after_switches", self.pss_mib_after_switches),
      ("idle_seconds", self.idle_seconds),
      ("idle_running_animations", self.idle_running_animations),
      ("idle_cpu_percent", self.idle_cpu_percent),
      ("idle_wakeups_per_second", self.idle_wakeups_per_second),
      ("idle_task_ms_per_minute", self.idle_task_ms_per_minute),
      (
        "idle_http_requests_per_minute",
        self.idle_http_requests_per_minute,
      ),
      ("idle_api_requests_per_minute", self.idle_api_requests_per_minute),
      (
        "idle_gateway_frames_per_minute",
        self.idle_gateway_frames_per_minute,
      ),
      ("pss_mib_idle_start", self.pss_mib_idle_start),
      ("pss_mib_idle_end", self.pss_mib_idle_end),
      ("js_heap_mib_idle_start", self.js_heap_mib_idle_start),
      ("js_heap_mib_idle_end", self.js_heap_mib_idle_end),
      ("wasm_kib", self.wasm_kib),
    ]
  }

  fn print_and_save(&self, scale: Scale) {
    let rows = self.rows();
    for (name, value) in &rows {
      eprintln!("perf {:>6} {name:<40} {value:>10.1}", scale.name);
    }
    let json: serde_json::Map<String, serde_json::Value> = rows
      .into_iter()
      .map(|(name, value)| (name.to_owned(), serde_json::json!(value)))
      .collect();
    let dir = artifact_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("perf-{}.json", scale.name));
    std::fs::write(&path, serde_json::to_vec_pretty(&json).unwrap()).unwrap();
    eprintln!("perf numbers: {}", path.display());
  }

  /// Generous bounds that only a regression breaks (the machine running the nightly suite is
  /// shared); the budgets in robustness.md are the targets.
  fn assert_budgets(&self) {
    assert!(self.seconds_reload_to_channel_list < 15.0, "{self:#?}");
    assert!(self.switch_ms_p95 < 1500.0, "{self:#?}");
    assert!(
      self.long_channel_rows_in_dom < 300.0,
      "message list not virtualised: {self:#?}"
    );
    assert!(self.long_channel_rows_after_scrolling_back < 600.0, "{self:#?}");
    assert_eq!(
      self.scroll_frames_moving_backwards, 0.0,
      "the list jumps while scrolling back (#642): {self:#?}"
    );
    // #1050: the scroll covers several pages of history the browser holds, which arrive ahead of
    // the reader without the list jumping.
    assert!(
      self.steady_scroll_frames > 300.0,
      "the held history is shorter than the scroll: {self:#?}"
    );
    assert!(
      self.steady_scroll_frames_with_placeholders < 10.0,
      "scrolling up waits for older messages (#1050): {self:#?}"
    );
    assert_eq!(
      self.steady_scroll_frames_moving_backwards, 0.0,
      "the list jumps while older pages arrive (#1050): {self:#?}"
    );
    assert!(self.pss_mib_after_switches < 2000.0, "{self:#?}");
    assert!(
      self.js_heap_mib_after_more_switches < self.js_heap_mib_after_first_switches * 1.5 + 5.0,
      "heap grows with channel switches: {self:#?}"
    );
    assert!(self.idle_cpu_percent < 5.0, "busy while idle: {self:#?}");
    // Issue #399: a loop while idle showed as growth of several MiB per second.
    assert!(
      self.pss_mib_idle_end < self.pss_mib_idle_start * 1.1 + 30.0,
      "memory grows while idle: {self:#?}"
    );
    assert!(
      self.js_heap_mib_idle_end < self.js_heap_mib_idle_start * 1.5 + 5.0,
      "heap grows while idle: {self:#?}"
    );
    // Issue #252: nothing is read on a timer while connected.
    assert_eq!(
      self.idle_api_requests_per_minute, 0.0,
      "requests while idle: {self:#?}"
    );
  }
}
