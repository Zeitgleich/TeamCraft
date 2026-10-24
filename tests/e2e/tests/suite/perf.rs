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
//! Every run writes its numbers to `target/e2e-artifacts/perf-<scale>.json`.

use std::time::{Duration, Instant};

use chromiumoxide::cdp::browser_protocol::network::{
  EnableParams as NetworkEnableParams, EventRequestWillBeSent, EventWebSocketFrameReceived,
  EventWebSocketFrameSent,
};
use chromiumoxide::cdp::browser_protocol::performance::{EnableParams, GetMetricsParams};
use chromiumoxide::cdp::js_protocol::heap_profiler::CollectGarbageParams;
use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
use chromiumoxide::page::Page;
use futures_util::StreamExt;
use teamcraft_core::client::OutgoingFile;
use teamcraft_e2e::browser::{BrowserSession, WebPortLease, artifact_dir, lease_web_port, web_dir};
use teamcraft_e2e::peer::PASSPHRASE;
use teamcraft_e2e::resources::ProcessUsage;
use teamcraft_e2e::scenario::space_with;
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::{ConversationId, HistoryPolicy, SpaceId, UserId};

use crate::web::{TEST_CSP, click_text, wait_for_element, wait_for_text};

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
/// Channel switches of the leak check (the budget's "memory growth over 100 channel switches").
const LEAK_CHECK_SWITCHES: usize = 100;
const MIB: f64 = 1024.0 * 1024.0;
/// How long the empty browser is watched for its own idle CPU and wakeups.
const BASELINE_IDLE: Duration = Duration::from_secs(30);
/// How long the app may take to come back after a navigation (a large catch-up).
const LONG_WAIT: Duration = Duration::from_secs(900);
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
  /// 1 when the identity provider's session had expired while Carol was away (she signed in again,
  /// which is part of `seconds_return_to_channel_list`).
  signed_in_again: f64,
  return_long_task_count: f64,
  return_long_task_total_ms: f64,
  return_longest_task_ms: f64,
  seconds_reload_to_channel_list: f64,
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
  scroll_frames: f64,
  scroll_frames_over_50ms: f64,
  js_heap_mib_after_first_switches: f64,
  js_heap_mib_after_more_switches: f64,
  pss_mib_after_switches: f64,
  idle_seconds: f64,
  idle_cpu_percent: f64,
  idle_wakeups_per_second: f64,
  idle_task_ms_per_minute: f64,
  /// HTTP requests of the page while idle (the budget: none besides the gateway heartbeat).
  idle_http_requests_per_minute: f64,
  /// Gateway frames in both directions while idle (heartbeats and their answers).
  idle_gateway_frames_per_minute: f64,
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
    let browser = BrowserSession::launch().await?;
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
  wait_for_element(page, "#enroll-passphrase")
    .await
    .click()
    .await
    .unwrap()
    .type_str(PASSPHRASE)
    .await
    .unwrap();
  click_text(page, "Unlock").await;
  wait_for_text(page, "Find or jump to").await;
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

async fn measure(page: &Page, seeded: &Seeded, profile: &str, scale: Scale) -> Report {
  let mut report = Report::default();
  page.execute(EnableParams::default()).await.unwrap();

  // Carol comes back: the client starts, catches up (welcomes, MLS, messages) and shows the list.
  // Navigation is started from the page, without waiting for its load event: the client may keep
  // the page's only thread busy for a long time, which is part of what is measured here.
  page.evaluate_on_new_document(LONG_TASK_RECORDER).await.unwrap();
  let started = Instant::now();
  page
    .evaluate(format!("location.href = '{WEB_ORIGIN}/?backend=server'"))
    .await
    .unwrap();
  let had_to_sign_in = come_back(page, &seeded.first_space_channels[1].0).await;
  report.signed_in_again = if had_to_sign_in { 1.0 } else { 0.0 };
  report.seconds_return_to_channel_list = started.elapsed().as_secs_f64();
  for (name, last) in &seeded.first_space_channels {
    switch_within(page, name, last, Duration::from_secs(280)).await;
  }
  report.seconds_return_until_all_channels_read = started.elapsed().as_secs_f64();
  let long_tasks = evaluate_value(page, "window.__longTasks || [0, 0, 0]".into()).await;
  report.return_long_task_count = long_tasks[0].as_f64().unwrap_or(0.0);
  report.return_long_task_total_ms = long_tasks[1].as_f64().unwrap_or(0.0);
  report.return_longest_task_ms = long_tasks[2].as_f64().unwrap_or(0.0);

  // Cold start of a signed-in client: reload until the channel list is back.
  let started = Instant::now();
  page.evaluate("location.reload()").await.unwrap();
  wait_long_for_text(page, &seeded.first_space_channels[1].0).await;
  report.seconds_reload_to_channel_list = started.elapsed().as_secs_f64();
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
  let (frames, slow_frames) = scroll_back(page).await;
  report.scroll_frames = frames;
  report.scroll_frames_over_50ms = slow_frames;
  report.long_channel_rows_after_scrolling_back = rows_in_dom(page).await;

  // Leak check: 100 more switches, then compare the heap after a full collection.
  for index in 0..LEAK_CHECK_SWITCHES {
    let (name, last) = &short_channels[index % short_channels.len()];
    switch_to(page, name, last).await;
  }
  collect_garbage(page).await;
  report.js_heap_mib_after_more_switches = metric(page, "JSHeapUsedSize").await / MIB;
  report.pss_mib_after_switches = mib(ProcessUsage::of_matching(profile).pss_bytes);

  // Idle in the foreground: nothing happens, so the client should not use the CPU.
  let usage_before = ProcessUsage::of_matching(profile);
  let tasks_before = metric(page, "TaskDuration").await;
  let (http_requests, gateway_frames) = count_traffic(page, scale.idle).await;
  let usage_after = ProcessUsage::of_matching(profile);
  let tasks_after = metric(page, "TaskDuration").await;
  let idle_seconds = scale.idle.as_secs_f64();
  report.idle_seconds = idle_seconds;
  report.idle_cpu_percent =
    (usage_after.cpu_time.saturating_sub(usage_before.cpu_time)).as_secs_f64() / idle_seconds * 100.0;
  report.idle_wakeups_per_second = (usage_after
    .context_switches
    .saturating_sub(usage_before.context_switches)) as f64
    / idle_seconds;
  report.idle_task_ms_per_minute = (tasks_after - tasks_before) * 1000.0 * 60.0 / idle_seconds;
  report.idle_http_requests_per_minute = http_requests * 60.0 / idle_seconds;
  report.idle_gateway_frames_per_minute = gateway_frames * 60.0 / idle_seconds;
  report.wasm_kib = wasm_bytes() as f64 / 1024.0;
  report
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
async fn count_traffic(page: &Page, period: Duration) -> (f64, f64) {
  page.execute(NetworkEnableParams::default()).await.unwrap();
  let mut requests = page.event_listener::<EventRequestWillBeSent>().await.unwrap();
  let mut sent = page.event_listener::<EventWebSocketFrameSent>().await.unwrap();
  let mut received = page
    .event_listener::<EventWebSocketFrameReceived>()
    .await
    .unwrap();
  let (mut http_requests, mut frames) = (0.0, 0.0);
  let deadline = tokio::time::sleep(period);
  tokio::pin!(deadline);
  loop {
    tokio::select! {
      () = &mut deadline => break,
      Some(_) = requests.next() => http_requests += 1.0,
      Some(_) = sent.next() => frames += 1.0,
      Some(_) = received.next() => frames += 1.0,
    }
  }
  (http_requests, frames)
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
    } else if !has_signed_in && text.contains("Sign in") && !text.contains("Find or jump to") {
      click_text(page, "Sign in").await;
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

/// Scrolls the long channel upwards in steps (loading older pages as a user would) and counts
/// frames and frames slower than 50 ms meanwhile. Returns `(frames, slow_frames)`.
async fn scroll_back(page: &Page) -> (f64, f64) {
  let script = r#"new Promise(resolve => {
    const list = document.querySelector('.message-list');
    let frames = 0, slow = 0, last = performance.now(), steps = 0;
    const tick = now => {
      frames += 1;
      if (now - last > 50) slow += 1;
      last = now;
      if (steps < 120) { list.scrollTop = Math.max(0, list.scrollTop - 600); steps += 1; requestAnimationFrame(tick); }
      else resolve([frames, slow]);
    };
    requestAnimationFrame(tick);
  })"#;
  let values = evaluate_value(page, script.into()).await;
  (values[0].as_f64().unwrap(), values[1].as_f64().unwrap())
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
        "seconds_return_until_all_channels_read",
        self.seconds_return_until_all_channels_read,
      ),
      ("signed_in_again", self.signed_in_again),
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
      ("scroll_frames", self.scroll_frames),
      ("scroll_frames_over_50ms", self.scroll_frames_over_50ms),
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
      ("idle_cpu_percent", self.idle_cpu_percent),
      ("idle_wakeups_per_second", self.idle_wakeups_per_second),
      ("idle_task_ms_per_minute", self.idle_task_ms_per_minute),
      (
        "idle_http_requests_per_minute",
        self.idle_http_requests_per_minute,
      ),
      (
        "idle_gateway_frames_per_minute",
        self.idle_gateway_frames_per_minute,
      ),
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
    assert!(self.pss_mib_after_switches < 2000.0, "{self:#?}");
    assert!(
      self.js_heap_mib_after_more_switches < self.js_heap_mib_after_first_switches * 1.5 + 5.0,
      "heap grows with channel switches: {self:#?}"
    );
    assert!(self.idle_cpu_percent < 5.0, "busy while idle: {self:#?}");
  }
}
