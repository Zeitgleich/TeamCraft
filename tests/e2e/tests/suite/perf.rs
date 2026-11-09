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

use chromiumoxide::cdp::browser_protocol::emulation::{
  ClearDeviceMetricsOverrideParams, SetCpuThrottlingRateParams, SetDeviceMetricsOverrideParams,
};
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
use chromiumoxide::page::{Page, ScreenshotParams};
use futures_util::StreamExt;
use teamcraft_core::client::OutgoingFile;
use teamcraft_crypto::local_store::LocalStoreKeys;
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_e2e::browser::{BrowserSession, WebPortLease, artifact_dir, lease_web_port, web_dir};
use teamcraft_e2e::peer::PASSPHRASE;
use teamcraft_e2e::resources::ProcessUsage;
use teamcraft_e2e::scenario::space_with;
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::{ConversationId, HistoryPolicy, SpaceId, UserId};

use crate::web::{TEST_CSP, click_text, type_until_kept, wait_for_element, wait_for_text};

const PORT: u16 = 8083;
/// The other port the dev realm registers. This test holds it as well: what it measures (times,
/// CPU, memory) shouldn't depend on a second Chromium running next to it.
const OTHER_BROWSER_TESTS_PORT: u16 = 8082;
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
/// How much slower than this machine the browser's CPU runs while it catches up many
/// conversations (issue #1575): the reporter's laptop took 200 ms a conversation where this one
/// takes 50. `PERF_CATCH_UP_CPU_SLOWDOWN` sets another factor (1: none).
const CATCH_UP_CPU_SLOWDOWN: f64 = 4.0;
/// Counts tasks that block the page's thread for more than 50 ms (`[count, total ms, longest ms]`).
const LONG_TASK_RECORDER: &str = "window.__longTasks = [0, 0, 0];
  window.__longTaskList = [];
  new PerformanceObserver(list => {
    for (const entry of list.getEntries()) {
      window.__longTasks[0] += 1;
      window.__longTasks[1] += entry.duration;
      window.__longTasks[2] = Math.max(window.__longTasks[2], entry.duration);
      window.__longTaskList.push([Math.round(entry.startTime), Math.round(entry.duration)]);
    }
  }).observe({ type: 'longtask', buffered: true });
  window.__resizeLoops = 0;
  window.addEventListener('error', event => {
    if (String(event.message).includes('ResizeObserver loop')) {
      window.__resizeLoops += 1;
    }
  });";
/// Reads the raw database key the way the page does (docs/e2ee.md §9.1): any script of the origin
/// can. Hex, for the harness to derive the lookup ID of the catalog's state.
const READ_DATABASE_KEY: &str = "(async () => {
  const databases = await indexedDB.databases();
  const { name } = databases.find(database => database.name.startsWith('teamcraft-')
    && !database.name.startsWith('teamcraft-media'));
  const database = await new Promise((resolve, reject) => {
    const request = indexedDB.open(name);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
  const keys = database.transaction('keys').objectStore('keys');
  const read = key => new Promise((resolve, reject) => {
    const request = keys.get(key);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
  const [wrapping, wrapped] = await Promise.all([read('wrapping_key'), read('wrapped_database_key')]);
  database.close();
  const raw = await crypto.subtle.decrypt(
    { name: 'AES-GCM', iv: wrapped.slice(0, 12) }, wrapping, wrapped.slice(12));
  return JSON.stringify([name, Array.from(new Uint8Array(raw), byte => byte.toString(16).padStart(2, '0')).join('')]);
})()";
/// How far the catalog build's progress bar is, or -1 while it doesn't show (or the page is the one
/// a reload is about to replace).
const BUILD_PROGRESS: &str = "(() => {
  const bar = !window.__isBeingReloaded && document.querySelector('.store-build[role=progressbar]');
  return bar ? Number(bar.getAttribute('aria-valuenow')) : -1;
})()";

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
      come_back_to_busy_channels(&page, &mut alice, &seeded, scale, &profile, &mut report).await;
      come_back_to_many_conversations(&page, &mut alice, &mut bob, &seeded, scale, &profile, &mut report)
        .await;
      watch_a_busy_conversation(&page, &mut alice, &seeded, &mut report).await;
      measure_catalog_build(&page, &seeded, &mut report).await;
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
  /// Messages each busy channel gets while Carol is away the last time, beyond what a capped
  /// catch-up reads (issue #1572).
  backlog_messages: usize,
  /// Messages every channel but two gets while Carol is away once more: fewer than a capped
  /// catch-up reads, so all of them are decrypted in the catch-up itself (issue #1575).
  catch_up_messages: usize,
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
  _turn: [WebPortLease; 2],
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
  /// Main-thread tasks over 50 ms from the reload until the channel list shows: reading the store.
  reload_long_task_count: f64,
  reload_longest_task_ms: f64,
  /// What the store's own `opened the local store` line says of the reload; -1 without one.
  store_read_ms: f64,
  store_records: f64,
  store_sealed_mib: f64,
  wasm_memory_mib_after_store_read: f64,
  /// The reload's gateway connection from the socket until caught up, as the client measured it
  /// (`gateway connection caught up`, issue #714); -1 when the client logged none.
  reload_sync_ms: f64,
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
  /// Layouts of the page per channel switch, and "ResizeObserver loop" errors over the switches: a
  /// resize observer whose callback resized what it observes lays out again and again (issue #1575).
  layouts_per_switch: f64,
  switch_resize_observer_loops: f64,
  /// An open conversation at its newest message while messages arrive, the composer grows line by
  /// line and the window changes height (issue #1575): the page's layouts meanwhile, and the
  /// "ResizeObserver loop" errors.
  busy_conversation_layouts: f64,
  busy_conversation_resize_observer_loops: f64,
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
  /// The first catalog build after an update (issue #1127): from loading the page until the channel
  /// list shows, the main-thread tasks over 50 ms meanwhile, and how many steps the bar showed.
  seconds_catalog_build_to_channel_list: f64,
  catalog_build_long_task_count: f64,
  catalog_build_long_task_total_ms: f64,
  catalog_build_longest_task_ms: f64,
  catalog_build_progress_steps: f64,
  /// A reload in the middle of a build: the percentage it had reached, the first it shows after the
  /// reload (no lower, or it started over), and the time from the reload to the channel list. -1
  /// when the build was too short to interrupt, or shows no progress.
  catalog_resume_percent_at_reload: f64,
  catalog_resume_first_percent_after: f64,
  seconds_catalog_resume_to_channel_list: f64,
  /// Coming back to busy channels whose backlogs are read in the background (issue #1572):
  /// switches between quiet channels meanwhile, from the page load until the browser went quiet,
  /// and the page's main-thread tasks over 50 ms in that time.
  backlog_switch_ms_p50: f64,
  backlog_switch_ms_p95: f64,
  backlog_switch_ms_max: f64,
  seconds_backlog_until_settled: f64,
  backlog_long_task_count: f64,
  backlog_long_task_total_ms: f64,
  backlog_longest_task_ms: f64,
  backlog_gateway_connections_lost: f64,
  /// Coming back to many conversations with new messages, all read in the catch-up itself (issue
  /// #1575): switches between quiet channels while it runs, the time until it was done, and the
  /// page's main-thread tasks over 50 ms (and over 100 ms) from the page load until then.
  catch_up_switch_ms_p50: f64,
  catch_up_switch_ms_p95: f64,
  catch_up_switch_ms_max: f64,
  seconds_catch_up_until_done: f64,
  catch_up_long_task_count: f64,
  catch_up_long_tasks_over_100ms: f64,
  catch_up_long_task_total_ms: f64,
  catch_up_longest_task_ms: f64,
  /// The same, only those of the content phase (from the end of the phase before it until its own
  /// end, by the client's log): the conversations' messages decrypted and written.
  catch_up_content_seconds: f64,
  catch_up_content_long_task_count: f64,
  catch_up_content_long_tasks_over_100ms: f64,
  catch_up_content_longest_task_ms: f64,
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
        backlog_messages: 2_000,
        catch_up_messages: 150,
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
        backlog_messages: 250,
        catch_up_messages: 60,
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
        backlog_messages: 600,
        catch_up_messages: 150,
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
    let turn = [
      lease_web_port(PORT).await,
      lease_web_port(OTHER_BROWSER_TESTS_PORT).await,
    ];
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
pub(crate) async fn sign_in_and_unlock(page: &Page, user: &str) {
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

/// Coming back after weeks away from busy channels (issue #1572). Every channel of the first space
/// but the long one and two quiet ones grows by more than a capped catch-up reads (200 messages),
/// so the rest of each is read in the background, a page at a time. Meanwhile Carol switches
/// between the quiet channels, which got nothing new; measured until the browser went quiet.
/// `PERF_BACKLOG_MESSAGES` sets the messages per busy channel.
async fn come_back_to_busy_channels(
  page: &Page,
  alice: &mut Peer,
  seeded: &Seeded,
  scale: Scale,
  profile: &str,
  report: &mut Report,
) {
  page.goto("about:blank").await.unwrap();
  let quiet = &seeded.first_space_channels[1..3];
  let busy = &seeded.channels[3..seeded.first_space_channels.len()];
  // PERF_BACKLOG_MESSAGES=n: a longer absence than the scale's.
  let messages = std::env::var("PERF_BACKLOG_MESSAGES")
    .ok()
    .and_then(|value| value.parse().ok())
    .unwrap_or(scale.backlog_messages);
  for index in 0..messages {
    for channel in busy {
      send(alice, *channel, index).await;
    }
  }
  let started = Instant::now();
  let log = watch_catch_up_log(page, started).await;
  page
    .evaluate(format!("location.href = '{WEB_ORIGIN}/?backend=server'"))
    .await
    .unwrap();
  come_back(page, &quiet[0].0).await;
  let mut times = Vec::with_capacity(BUSY_SWITCHES);
  for index in 0..BUSY_SWITCHES {
    let (name, last) = &quiet[index % quiet.len()];
    times.push(switch_within(page, name, last, Duration::from_secs(280)).await);
  }
  times.sort_by(f64::total_cmp);
  report.backlog_switch_ms_p50 = percentile(&times, 50);
  report.backlog_switch_ms_p95 = percentile(&times, 95);
  report.backlog_switch_ms_max = times.last().copied().unwrap_or(0.0);
  wait_until_done(page).await;
  wait_until_browser_settled(profile).await;
  report.seconds_backlog_until_settled = started.elapsed().as_secs_f64();
  let long_tasks = evaluate_value(page, "window.__longTasks || [0, 0, 0]".into()).await;
  report.backlog_long_task_count = long_tasks[0].as_f64().unwrap_or(0.0);
  report.backlog_long_task_total_ms = long_tasks[1].as_f64().unwrap_or(0.0);
  report.backlog_longest_task_ms = long_tasks[2].as_f64().unwrap_or(0.0);
  report.backlog_gateway_connections_lost = log
    .lock()
    .unwrap()
    .iter()
    .filter(|(_, line)| line.contains("gateway connection lost"))
    .count() as f64;
}

/// The list of an open conversation changes size the ways it does in use (issue #1575): messages
/// arrive while the reader is at the newest one, the composer grows line by line and is cleared,
/// and the window gets shorter and taller. Each of these resizes what the list's observer watches.
async fn watch_a_busy_conversation(page: &Page, alice: &mut Peer, seeded: &Seeded, report: &mut Report) {
  let (name, last) = &seeded.first_space_channels[1];
  switch_to(page, name, last).await;
  settle(page).await;
  let layouts_before = metric(page, "LayoutCount").await;
  let loops_before = resize_loops(page).await;
  for index in 0..15 {
    send(alice, seeded.channels[1], index).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
  for line in 0..8 {
    page
      .evaluate(format!(
        "(() => {{ const el = document.querySelector('[data-composer]'); el.focus(); \
         document.execCommand('insertText', false, 'line {line}'); \
         document.execCommand('insertLineBreak'); }})()"
      ))
      .await
      .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
  }
  page
    .evaluate("(() => { const el = document.querySelector('[data-composer]'); el.focus(); document.execCommand('selectAll'); document.execCommand('delete'); })()")
    .await
    .unwrap();
  for height in [700, 900, 600, 900] {
    page
      .execute(SetDeviceMetricsOverrideParams::new(1280, height, 1.0, false))
      .await
      .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
  }
  page
    .execute(ClearDeviceMetricsOverrideParams::default())
    .await
    .unwrap();
  settle(page).await;
  report.busy_conversation_layouts = metric(page, "LayoutCount").await - layouts_before;
  report.busy_conversation_resize_observer_loops = resize_loops(page).await - loops_before;
}

/// Coming back after an absence in which most conversations got new messages, fewer than a capped
/// catch-up reads (issue #1575): the catch-up decrypts all of them itself, conversation after
/// conversation, on the page's only thread. Meanwhile Carol switches between two quiet channels.
/// `PERF_CATCH_UP_MESSAGES` sets the messages per channel.
async fn come_back_to_many_conversations(
  page: &Page,
  alice: &mut Peer,
  bob: &mut Peer,
  seeded: &Seeded,
  scale: Scale,
  profile: &str,
  report: &mut Report,
) {
  page.goto("about:blank").await.unwrap();
  let quiet = &seeded.first_space_channels[1..3];
  let busy: Vec<ConversationId> = seeded
    .channels
    .iter()
    .enumerate()
    .filter(|(index, _)| !(1..3).contains(index))
    .map(|(_, channel)| *channel)
    .collect();
  let messages = std::env::var("PERF_CATCH_UP_MESSAGES")
    .ok()
    .and_then(|value| value.parse().ok())
    .unwrap_or(scale.catch_up_messages);
  let (alice_half, bob_half) = busy.split_at(busy.len() / 2);
  let alice_writes = async {
    for index in 0..messages {
      for channel in alice_half {
        send(alice, *channel, index).await;
      }
    }
  };
  let bob_writes = async {
    for index in 0..messages {
      for channel in bob_half {
        send(bob, *channel, index).await;
      }
    }
  };
  tokio::join!(alice_writes, bob_writes);
  let slowdown = std::env::var("PERF_CATCH_UP_CPU_SLOWDOWN")
    .ok()
    .and_then(|value| value.parse().ok())
    .unwrap_or(CATCH_UP_CPU_SLOWDOWN);
  page
    .execute(SetCpuThrottlingRateParams::new(slowdown))
    .await
    .unwrap();
  let started = Instant::now();
  let log = watch_catch_up_log(page, started).await;
  page
    .evaluate(format!("location.href = '{WEB_ORIGIN}/?backend=server'"))
    .await
    .unwrap();
  come_back(page, &quiet[0].0).await;
  let mut times = Vec::with_capacity(BUSY_SWITCHES);
  for index in 0..BUSY_SWITCHES {
    let (name, last) = &quiet[index % quiet.len()];
    times.push(switch_within(page, name, last, Duration::from_secs(280)).await);
  }
  times.sort_by(f64::total_cmp);
  report.catch_up_switch_ms_p50 = percentile(&times, 50);
  report.catch_up_switch_ms_p95 = percentile(&times, 95);
  report.catch_up_switch_ms_max = times.last().copied().unwrap_or(0.0);
  report.seconds_catch_up_until_done = wait_until_done(page).await.duration_since(started).as_secs_f64();
  let long_tasks = evaluate_value(page, "window.__longTasks || [0, 0, 0]".into()).await;
  report.catch_up_long_task_count = long_tasks[0].as_f64().unwrap_or(0.0);
  report.catch_up_long_task_total_ms = long_tasks[1].as_f64().unwrap_or(0.0);
  report.catch_up_longest_task_ms = long_tasks[2].as_f64().unwrap_or(0.0);
  report.catch_up_long_tasks_over_100ms = evaluate_value(
    page,
    "(window.__longTaskList || []).filter(([, duration]) => duration > 100).length".into(),
  )
  .await
  .as_f64()
  .unwrap_or(0.0);
  print_long_tasks(page, "catch-up of many conversations").await;
  let lines = log.lock().unwrap().clone();
  for (at, line) in &lines {
    if line.contains("phase done") || line.contains("catch-up done") {
      eprintln!("catch-up log {at:>7.1} s  {line}");
    }
  }
  // The content phase, in the page's time (its origin is close to `started`).
  let phase_end = |phase: &str| {
    lines
      .iter()
      .rev()
      .find(|(_, line)| line.contains(&format!("phase=\\\"{phase}\\\"")))
      .map(|(at, _)| at * 1000.0)
  };
  if let (Some(from), Some(to)) = (phase_end("missing groups"), phase_end("content")) {
    let tasks = evaluate_value(page, "window.__longTaskList || []".into()).await;
    let durations: Vec<f64> = tasks
      .as_array()
      .into_iter()
      .flatten()
      .filter_map(|task| Some((task[0].as_f64()?, task[1].as_f64()?)))
      .filter(|(start, duration)| start + duration > from && *start < to)
      .map(|(_, duration)| duration)
      .collect();
    report.catch_up_content_seconds = (to - from) / 1000.0;
    report.catch_up_content_long_task_count = durations.len() as f64;
    report.catch_up_content_long_tasks_over_100ms =
      durations.iter().filter(|duration| **duration > 100.0).count() as f64;
    report.catch_up_content_longest_task_ms = durations.iter().copied().fold(0.0, f64::max);
  }
  page.execute(SetCpuThrottlingRateParams::new(1.0)).await.unwrap();
  wait_until_browser_settled(profile).await;
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
  let lines_before_reload = catch_up_log.lock().unwrap().len();
  let started = Instant::now();
  page.evaluate("location.reload()").await.unwrap();
  wait_long_for_text(page, &seeded.first_space_channels[1].0).await;
  report.seconds_reload_to_channel_list = started.elapsed().as_secs_f64();
  let long_tasks = evaluate_value(page, "window.__longTasks || [0, 0, 0]".into()).await;
  report.reload_long_task_count = long_tasks[0].as_f64().unwrap_or(0.0);
  report.reload_longest_task_ms = long_tasks[2].as_f64().unwrap_or(0.0);
  print_long_tasks(page, "reload").await;
  wait_until_done(page).await;
  let store_line = catch_up_log
    .lock()
    .unwrap()
    .get(lines_before_reload..)
    .unwrap_or_default()
    .iter()
    .rev()
    .find(|(_, line)| line.contains("opened the local store"))
    .map(|(_, line)| line.clone())
    .unwrap_or_default();
  report.store_read_ms = field_of(&store_line, "read_ms").unwrap_or(-1.0);
  report.store_records = field_of(&store_line, "records").unwrap_or(-1.0);
  report.store_sealed_mib = field_of(&store_line, "sealed_mib").unwrap_or(-1.0);
  report.wasm_memory_mib_after_store_read = field_of(&store_line, "wasm_mib").unwrap_or(-1.0);
  report.reload_sync_ms = catch_up_log
    .lock()
    .unwrap()
    .get(lines_before_reload..)
    .unwrap_or_default()
    .iter()
    .rev()
    .find_map(|(_, line)| caught_up_ms(line))
    .unwrap_or(-1.0);
  report.seconds_reload_until_browser_settled = wait_until_browser_settled(profile).await.as_secs_f64();
  settle(page).await;
  report.pss_mib_after_start = mib(ProcessUsage::of_matching(profile).pss_bytes);
  report.js_heap_mib_after_start = metric(page, "JSHeapUsedSize").await / MIB;
  report.dom_nodes_after_start = metric(page, "Nodes").await;

  // Channel switches between the short channels of the first space.
  let short_channels = &seeded.first_space_channels[1..];
  let mut times = Vec::new();
  let layouts_before = metric(page, "LayoutCount").await;
  let loops_before = resize_loops(page).await;
  for index in 0..SWITCHES {
    let (name, last) = &short_channels[index % short_channels.len()];
    times.push(switch_to(page, name, last).await);
  }
  times.sort_by(f64::total_cmp);
  report.switch_ms_p50 = percentile(&times, 50);
  report.switch_ms_p95 = percentile(&times, 95);
  report.switch_ms_max = times.last().copied().unwrap_or(0.0);
  report.layouts_per_switch = (metric(page, "LayoutCount").await - layouts_before) / SWITCHES as f64;
  report.switch_resize_observer_loops = resize_loops(page).await - loops_before;
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

/// The first catalog build after an update, on the store the run filled: the catalog's state is
/// deleted from outside the app, so the next start builds it again under a new generation, like an
/// update that changes what the catalog holds. Then once more with a reload in the middle.
async fn measure_catalog_build(page: &Page, seeded: &Seeded, report: &mut Report) {
  let channel = &seeded.first_space_channels[1].0;
  forget_catalog_state(page).await;
  let started = Instant::now();
  page
    .evaluate(format!("location.href = '{WEB_ORIGIN}/?backend=server'"))
    .await
    .unwrap();
  let mut percents = std::collections::BTreeSet::new();
  wait_long_while_watching_build(page, channel, &mut percents).await;
  report.seconds_catalog_build_to_channel_list = started.elapsed().as_secs_f64();
  report.catalog_build_progress_steps = percents.len() as f64;
  let long_tasks = evaluate_value(page, "window.__longTasks || [0, 0, 0]".into()).await;
  report.catalog_build_long_task_count = long_tasks[0].as_f64().unwrap_or(0.0);
  report.catalog_build_long_task_total_ms = long_tasks[1].as_f64().unwrap_or(0.0);
  report.catalog_build_longest_task_ms = long_tasks[2].as_f64().unwrap_or(0.0);
  print_long_tasks(page, "catalog build").await;
  report.catalog_resume_percent_at_reload = -1.0;
  report.catalog_resume_first_percent_after = -1.0;
  report.seconds_catalog_resume_to_channel_list = -1.0;
  if percents.is_empty() {
    return;
  }
  wait_until_done(page).await;
  forget_catalog_state(page).await;
  page
    .evaluate(format!("location.href = '{WEB_ORIGIN}/?backend=server'"))
    .await
    .unwrap();
  let Some(at_reload) = wait_for_build_percent(page, 30.0, channel).await else {
    wait_long_for_text(page, channel).await;
    return;
  };
  report.catalog_resume_percent_at_reload = at_reload;
  // The bar is what the user sees, not something behind the loading screen.
  tokio::time::sleep(Duration::from_secs(1)).await;
  let is_on_top = evaluate_value(
    page,
    "(() => {
      const bar = document.querySelector('.store-build');
      if (!bar) return true;
      const box = bar.getBoundingClientRect();
      const top = document.elementFromPoint(box.left + box.width / 2, box.top + box.height / 2);
      return !!(top && top.closest('.store-build'));
    })()"
      .into(),
  )
  .await;
  assert_eq!(is_on_top, serde_json::json!(true), "the build's bar is covered");
  let screenshot = artifact_dir().join("perf-store-build.png");
  let _ = std::fs::create_dir_all(artifact_dir());
  if let Err(error) = page
    .save_screenshot(ScreenshotParams::builder().build(), &screenshot)
    .await
  {
    eprintln!("could not take a screenshot of the build: {error}");
  }
  let started = Instant::now();
  page
    .evaluate("window.__isBeingReloaded = true; location.reload()")
    .await
    .unwrap();
  // A build that ended within the moment before its bar shows has nothing to say.
  if let Some(first) = wait_for_build_percent(page, 0.0, channel).await {
    report.catalog_resume_first_percent_after = first;
    assert!(
      first >= at_reload - 5.0,
      "the build started over after the reload: {report:#?}"
    );
  }
  wait_long_for_text(page, channel).await;
  report.seconds_catalog_resume_to_channel_list = started.elapsed().as_secs_f64();
}

/// The page's tasks over 50 ms so far, as `start+duration` in milliseconds.
async fn print_long_tasks(page: &Page, what: &str) {
  let tasks = evaluate_value(page, "window.__longTaskList || []".into()).await;
  eprintln!("long tasks of the {what}: {tasks}");
}

/// Leaves the app (its core holds the store), deletes the record of the catalog's state.
async fn forget_catalog_state(page: &Page) {
  page.goto(format!("{WEB_ORIGIN}/healthz")).await.unwrap();
  let answer = evaluate_value(page, READ_DATABASE_KEY.to_owned()).await;
  let answer: Vec<String> = serde_json::from_str(answer.as_str().unwrap()).unwrap();
  let raw: Vec<u8> = (0..answer[1].len() / 2)
    .map(|index| u8::from_str_radix(&answer[1][index * 2..index * 2 + 2], 16).unwrap())
    .collect();
  let key = SymmetricKey::from_slice(&raw).unwrap();
  // sealed.rs `entry_id`: the reserved table's name, a zero byte, the key.
  let id = LocalStoreKeys::derive(&key).lookup_id("entry", b"_catalog\0meta");
  let record: String = id.0.iter().map(|byte| format!("{byte:02x}")).collect();
  let database = &answer[0];
  evaluate_value(
    page,
    format!(
      "new Promise((resolve, reject) => {{
        const request = indexedDB.open('{database}');
        request.onerror = () => reject(request.error);
        request.onsuccess = () => {{
          const transaction = request.result.transaction('records', 'readwrite');
          transaction.objectStore('records').delete('{record}');
          transaction.oncomplete = () => {{ request.result.close(); resolve(true); }};
          transaction.onerror = () => reject(transaction.error);
        }};
      }})"
    ),
  )
  .await;
}

/// [`wait_long_for_text`], noting every percentage the build's bar shows on the way.
async fn wait_long_while_watching_build(
  page: &Page,
  needle: &str,
  percents: &mut std::collections::BTreeSet<u64>,
) {
  let deadline = Instant::now() + LONG_WAIT;
  loop {
    let script = format!("JSON.stringify([{BUILD_PROGRESS}, document.body ? document.body.innerText : ''])");
    let answer = page
      .evaluate(script)
      .await
      .ok()
      .and_then(|result| result.into_value::<String>().ok())
      .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    if let Some(answer) = answer {
      if let Some(percent) = answer[0].as_f64().filter(|percent| *percent >= 0.0) {
        percents.insert(percent as u64);
      }
      if answer[1].as_str().is_some_and(|text| text.contains(needle)) {
        return;
      }
    }
    assert!(
      Instant::now() < deadline,
      "{needle:?} did not appear within {LONG_WAIT:?}"
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
}

/// The build's percentage once it has reached `at_least`; `None` when `needle` showed first: the
/// app is there, so the build ended.
async fn wait_for_build_percent(page: &Page, at_least: f64, needle: &str) -> Option<f64> {
  let deadline = Instant::now() + LONG_WAIT;
  while Instant::now() < deadline {
    let script = format!("JSON.stringify([{BUILD_PROGRESS}, document.body ? document.body.innerText : ''])");
    let answer = page
      .evaluate(script)
      .await
      .ok()
      .and_then(|result| result.into_value::<String>().ok())
      .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    if let Some(answer) = answer {
      if let Some(percent) = answer[0].as_f64().filter(|percent| *percent >= at_least.max(0.0)) {
        return Some(percent);
      }
      if answer[0].as_f64().is_none_or(|percent| percent < 0.0)
        && answer[1].as_str().is_some_and(|text| text.contains(needle))
      {
        return None;
      }
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
  None
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
  const WATCHED: [&str; 6] = [
    "opened the local store",
    "catch-up",
    "full sync",
    "gateway connection lost",
    "gateway connected",
    "gateway connection caught up",
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

/// The `total_ms` of a `gateway connection caught up` line.
fn caught_up_ms(line: &str) -> Option<f64> {
  if !line.contains("gateway connection caught up") {
    return None;
  }
  let (_, rest) = line.split_once("total_ms=")?;
  let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
  digits.parse().ok()
}

/// The number after `name=` in a log line.
fn field_of(line: &str, name: &str) -> Option<f64> {
  let (_, rest) = line.split_once(&format!("{name}="))?;
  let digits: String = rest
    .chars()
    .take_while(|digit| digit.is_ascii_digit() || *digit == '.')
    .collect();
  digits.parse().ok()
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
pub(crate) async fn sign_in_at_keycloak(page: &Page, user: &str) {
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

/// "ResizeObserver loop" errors since the page was loaded (`LONG_TASK_RECORDER`).
async fn resize_loops(page: &Page) -> f64 {
  evaluate_value(page, "window.__resizeLoops || 0".into())
    .await
    .as_f64()
    .unwrap_or(0.0)
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
      ("reload_sync_ms", self.reload_sync_ms),
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
      ("layouts_per_switch", self.layouts_per_switch),
      ("switch_resize_observer_loops", self.switch_resize_observer_loops),
      ("busy_conversation_layouts", self.busy_conversation_layouts),
      (
        "busy_conversation_resize_observer_loops",
        self.busy_conversation_resize_observer_loops,
      ),
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
      ("reload_long_task_count", self.reload_long_task_count),
      ("reload_longest_task_ms", self.reload_longest_task_ms),
      ("store_read_ms", self.store_read_ms),
      ("store_records", self.store_records),
      ("store_sealed_mib", self.store_sealed_mib),
      (
        "wasm_memory_mib_after_store_read",
        self.wasm_memory_mib_after_store_read,
      ),
      (
        "seconds_catalog_build_to_channel_list",
        self.seconds_catalog_build_to_channel_list,
      ),
      (
        "catalog_build_long_task_count",
        self.catalog_build_long_task_count,
      ),
      (
        "catalog_build_long_task_total_ms",
        self.catalog_build_long_task_total_ms,
      ),
      (
        "catalog_build_longest_task_ms",
        self.catalog_build_longest_task_ms,
      ),
      ("catalog_build_progress_steps", self.catalog_build_progress_steps),
      (
        "catalog_resume_percent_at_reload",
        self.catalog_resume_percent_at_reload,
      ),
      (
        "catalog_resume_first_percent_after",
        self.catalog_resume_first_percent_after,
      ),
      (
        "seconds_catalog_resume_to_channel_list",
        self.seconds_catalog_resume_to_channel_list,
      ),
      ("backlog_switch_ms_p50", self.backlog_switch_ms_p50),
      ("backlog_switch_ms_p95", self.backlog_switch_ms_p95),
      ("backlog_switch_ms_max", self.backlog_switch_ms_max),
      (
        "seconds_backlog_until_settled",
        self.seconds_backlog_until_settled,
      ),
      ("backlog_long_task_count", self.backlog_long_task_count),
      ("backlog_long_task_total_ms", self.backlog_long_task_total_ms),
      ("backlog_longest_task_ms", self.backlog_longest_task_ms),
      (
        "backlog_gateway_connections_lost",
        self.backlog_gateway_connections_lost,
      ),
      ("catch_up_switch_ms_p50", self.catch_up_switch_ms_p50),
      ("catch_up_switch_ms_p95", self.catch_up_switch_ms_p95),
      ("catch_up_switch_ms_max", self.catch_up_switch_ms_max),
      ("seconds_catch_up_until_done", self.seconds_catch_up_until_done),
      ("catch_up_long_task_count", self.catch_up_long_task_count),
      (
        "catch_up_long_tasks_over_100ms",
        self.catch_up_long_tasks_over_100ms,
      ),
      ("catch_up_long_task_total_ms", self.catch_up_long_task_total_ms),
      ("catch_up_longest_task_ms", self.catch_up_longest_task_ms),
      ("catch_up_content_seconds", self.catch_up_content_seconds),
      (
        "catch_up_content_long_task_count",
        self.catch_up_content_long_task_count,
      ),
      (
        "catch_up_content_long_tasks_over_100ms",
        self.catch_up_content_long_tasks_over_100ms,
      ),
      (
        "catch_up_content_longest_task_ms",
        self.catch_up_content_longest_task_ms,
      ),
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
    // Issue #714: synced within 300 ms of opening; the bound leaves room for a shared machine.
    assert!(
      self.reload_sync_ms >= 0.0,
      "the client logged no caught-up connection after the reload: {self:#?}"
    );
    assert!(self.reload_sync_ms < 3000.0, "{self:#?}");
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
