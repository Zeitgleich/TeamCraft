//! The Linux desktop app (Tauri on WebKitGTK) against a generated workspace, hidden in the tray
//! (issue #1022; budgets in .claude/rules/robustness.md, "Performance and resources"; numbers in
//! docs/performance.md, "Desktop on Linux: hidden in the tray").
//!
//! Carol signs in with the app and quits it. While she is away, Alice and Bob fill the workspace of
//! `perf.rs` (`PERF_SCALE`, small by default). Then she starts the app again: it catches up, she
//! opens channels, closes the window to the tray, and the app is watched while nobody touches it:
//! CPU and wakeups of the app's process group (the shell with the client core, and WebKit's web and
//! network processes), the server's HTTP requests, and memory split by process.
//!
//! Needs a debug build of the app with the web client embedded (tests/e2e/README.md, "Desktop"):
//!
//! ```sh
//! cargo build -p teamcraft-desktop --features custom-protocol   # with crates/teamcraft-ui/dist
//! E2E_DESKTOP_BIN=$PWD/target/debug/teamcraft-desktop xvfb-run -a \
//!   cargo test -p teamcraft-e2e --features desktop --test suite -- --nocapture desktop_perf::
//! ```
//!
//! Every run writes its numbers to `target/e2e-artifacts/desktop-perf-<scale>.json`.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::json;
use teamcraft_e2e::browser::artifact_dir;
use teamcraft_e2e::desktop::{DesktopApp, DesktopSession};
use teamcraft_e2e::peer::PASSPHRASE;
use teamcraft_e2e::resources::{ProcessUsage, thread_wakeups, wakeups_between};
use teamcraft_e2e::{Peer, Stack};

use crate::perf::{Scale, Seeded, mib, percentile, seed};

/// How long the app may take to catch up after its start with the whole workspace waiting: the
/// large one, and the smaller ones.
const CATCH_UP_WAIT_LARGE: Duration = Duration::from_secs(900);
const CATCH_UP_WAIT: Duration = Duration::from_secs(180);
/// The app's process group below this much CPU (percent of one core) over [`SETTLE_WINDOW`] counts
/// as settled: the catch-up and what it left behind are done.
const SETTLED_CPU_PERCENT: f64 = 3.0;
const SETTLE_WINDOW: Duration = Duration::from_secs(10);
/// Channel switches before memory is read: a user's day, not a fresh start.
const SWITCHES: usize = 40;
/// The budget hidden in the tray is the foreground's: 0.5 % of one core for the whole app. The
/// check allows four times that for a debug build on a shared CI runner; the regression it is for
/// (#1000) was 4 % and more.
const HIDDEN_CPU_PERCENT_LIMIT: f64 = 2.0;
/// Wakeups (voluntary context switches of all threads) per second of the whole app while hidden.
/// Measured 50 to 55, most of them WebKit's own (its IPC and memory pressure threads); a timer
/// that keeps the page or the core busy while nobody can see it adds tens to hundreds.
const HIDDEN_WAKEUPS_PER_SECOND_LIMIT: f64 = 100.0;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_desktop_app_hidden_in_the_tray_stays_idle() {
  let scale = Scale::from_env();
  let Some(session) = DesktopSession::start("perf").await else {
    return;
  };
  let Some(stack) = Stack::builder()
    // Seeding writes far faster than a person; the rate limits would throttle it.
    .with_env("TEAMCRAFT_SECURITY__RATE_LIMITS__IS_ENABLED", "false")
    .with_env("TEAMCRAFT_DATABASE__MAX_CONNECTIONS", "16")
    // Every request of the server, to count the app's while it is hidden.
    .with_env("RUST_LOG", "info,tower_http=debug,teamcraft_server::router=debug")
    .start()
    .await
  else {
    return;
  };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut bob = Peer::enroll(&stack, "bob").await;
  // Carol's keys exist before the app signs in (it unlocks them); her first device stays offline.
  drop(Peer::enroll(&stack, "carol").await);

  let mut report = Report::default();
  let started = Instant::now();
  let mut app = session.launch(&stack.base_url).await.unwrap();
  sign_in_and_unlock(&session, &mut app).await;
  report.seconds_sign_in_and_unlock = started.elapsed().as_secs_f64();
  app.quit().await.unwrap();

  let started = Instant::now();
  let seeded = seed(&stack, &mut alice, &mut bob, scale).await;
  eprintln!("seeded the {} workspace in {:?}", scale.name, started.elapsed());

  let started = Instant::now();
  let mut app = session.launch(&stack.base_url).await.unwrap();
  let (first_name, first_last) = seeded.first_space_channels[0].clone();
  let catch_up_wait = match scale.name {
    "large" => CATCH_UP_WAIT_LARGE,
    _ => CATCH_UP_WAIT,
  };
  open_first_space(&mut app, catch_up_wait).await;
  switch_to(&mut app, &first_name, &first_last, catch_up_wait).await;
  report.seconds_start_to_first_channel = started.elapsed().as_secs_f64();
  report.seconds_start_until_settled =
    wait_until_settled(&mut app).await.as_secs_f64() + report.seconds_start_to_first_channel;

  eprintln!("desktop perf: switching channels");
  switch_through_channels(&mut app, &seeded, &mut report).await;
  wait_until_settled(&mut app).await;
  hold_for_a_look(&app).await;
  eprintln!("desktop perf: watching the visible window");
  report.visible = measure_idle(&stack, &mut app, scale.idle).await;

  // Closed to the tray, as with the window's close button.
  app
    .invoke("window_action", json!({ "action": "close" }))
    .await
    .unwrap();
  wait_until_hidden(&mut app).await;
  wait_until_settled(&mut app).await;
  eprintln!("desktop perf: watching the app hidden in the tray");
  report.hidden = measure_idle(&stack, &mut app, scale.idle).await;

  report.print_and_save(scale);
  if scale.is_regression_check {
    report.assert_budgets();
  }
  app.quit().await.unwrap();
}

/// Signs in through Keycloak in the "system browser" and unlocks the account's keys with the
/// recovery passphrase.
async fn sign_in_and_unlock(session: &DesktopSession, app: &mut DesktopApp) {
  // The server the environment names, confirmed once (desktop.rs).
  app
    .wait_for_text("Continue to sign-in", Duration::from_secs(60))
    .await
    .unwrap();
  app.click("Continue to sign-in").await.unwrap();
  session.sign_in_at_keycloak("carol").await.unwrap();
  // Her keys exist: the recovery passphrase brings them to this device. A setup that asks how first
  // gets the recovery kit picked.
  let deadline = Instant::now() + Duration::from_secs(60);
  loop {
    let text = app.text().await;
    if text.contains("Use my recovery kit") {
      app.click("Use my recovery kit").await.unwrap();
      break;
    }
    if text.contains("Recovery key") {
      break;
    }
    assert!(
      Instant::now() < deadline,
      "no device setup; the page shows {text:?}"
    );
    tokio::time::sleep(Duration::from_millis(250)).await;
  }
  app.fill("#enroll-secret", PASSPHRASE).await.unwrap();
  app.click("Unlock").await.unwrap();
  app
    .wait_for_text("Activity", Duration::from_secs(120))
    .await
    .unwrap();
}

/// Opens channels the way a user does through a day, spread over the workspace.
async fn switch_through_channels(app: &mut DesktopApp, seeded: &Seeded, report: &mut Report) {
  let channels = &seeded.first_space_channels;
  let mut times = Vec::with_capacity(SWITCHES);
  for index in 0..SWITCHES {
    let (name, last) = &channels[index % channels.len()];
    times.push(switch_to(app, name, last, Duration::from_secs(30)).await);
  }
  times.sort_by(f64::total_cmp);
  report.switch_ms_p50 = percentile(&times, 50);
  report.switch_ms_p95 = percentile(&times, 95);
}

/// Clicks the channel in the sidebar and waits until its newest message shows; milliseconds from
/// the click, measured in the page.
async fn switch_to(app: &mut DesktopApp, name: &str, last: &str, timeout: Duration) -> f64 {
  let step = format!(
    r#"(() => {{
      const entry = [...document.querySelectorAll('.sidebar .sb-item')]
        .find(e => (e.querySelector('.sb-name')?.innerText ?? '').trim() === {name:?});
      if (!entry) return 'no entry';
      if (window.__e2eSwitch !== {name:?}) {{
        window.__e2eSwitch = {name:?};
        window.__e2eSwitchStarted = performance.now();
        entry.click();
      }}
      const list = document.querySelector('.message-list');
      if (list && list.innerText.includes({last:?})) {{
        window.__e2eSwitch = null;
        return performance.now() - window.__e2eSwitchStarted;
      }}
      // An unread channel opens at the first unread message; the newest is one click away.
      document.querySelector('.jump-latest')?.click();
      return 'waiting';
    }})()"#
  );
  let deadline = Instant::now() + timeout;
  loop {
    let value = app.evaluate(&step).await.unwrap();
    if let Some(milliseconds) = value.as_f64() {
      return milliseconds;
    }
    if Instant::now() > deadline {
      let sidebar = app
        .evaluate("JSON.stringify([...document.querySelectorAll('.sidebar .sb-item .sb-name')].map(e => e.innerText.trim()))")
        .await
        .unwrap();
      panic!(
        "channel {name} did not show {last:?} within {timeout:?} ({value}); sidebar {sidebar}; the page shows {:?}\nlog:\n{}",
        app.text().await,
        app.log()
      );
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
}

/// `PERF_DESKTOP_HOLD_SECS`: time to look at the app by hand (its inspector port is in the log).
async fn hold_for_a_look(app: &DesktopApp) {
  let Some(seconds) = std::env::var("PERF_DESKTOP_HOLD_SECS")
    .ok()
    .and_then(|value| value.parse().ok())
  else {
    return;
  };
  eprintln!("desktop perf: holding for {seconds} s, pid {}", app.pid());
  tokio::time::sleep(Duration::from_secs(seconds)).await;
}

/// The app starts in Chats; the seeded channels are in the spaces of the rail.
async fn open_first_space(app: &mut DesktopApp, timeout: Duration) {
  let script = "(() => {
    const button = [...document.querySelectorAll('.space-btn')]
      .find(b => (b.getAttribute('aria-label') || '').startsWith('Space 00'));
    if (!button) return false;
    button.click();
    return true;
  })()";
  let deadline = Instant::now() + timeout;
  while app.evaluate(script).await.unwrap() != json!(true) {
    assert!(
      Instant::now() < deadline,
      "no Space 00 in the rail: {:?}",
      app.text().await
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
}

async fn wait_until_hidden(app: &mut DesktopApp) {
  let deadline = Instant::now() + Duration::from_secs(30);
  while app.evaluate("document.visibilityState").await.unwrap() != json!("hidden") {
    assert!(Instant::now() < deadline, "the page did not become hidden");
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}

/// Waits until the app's process group stays below [`SETTLED_CPU_PERCENT`] for [`SETTLE_WINDOW`].
async fn wait_until_settled(app: &mut DesktopApp) -> Duration {
  app.detach_inspector();
  let pid = app.pid();
  let started = Instant::now();
  loop {
    let before = ProcessUsage::total(ProcessUsage::of_tree(pid).values());
    tokio::time::sleep(SETTLE_WINDOW).await;
    let after = ProcessUsage::total(ProcessUsage::of_tree(pid).values());
    let percent = cpu_percent(after.cpu_time.saturating_sub(before.cpu_time), SETTLE_WINDOW);
    eprintln!("desktop perf: {percent:.1} % CPU over {SETTLE_WINDOW:?}");
    if percent < SETTLED_CPU_PERCENT {
      return started.elapsed();
    }
    assert!(
      started.elapsed() < CATCH_UP_WAIT_LARGE,
      "the app stayed busy ({percent:.1} % CPU)"
    );
  }
}

/// CPU, wakeups and memory of each of the app's processes, and the server's requests, over
/// `period` without touching the app.
async fn measure_idle(stack: &Stack, app: &mut DesktopApp, period: Duration) -> Idle {
  app.detach_inspector();
  let log_before = stack.server_log().len();
  let before = ProcessUsage::of_tree(app.pid());
  let wakeups_before = thread_wakeups(app.pid());
  tokio::time::sleep(period).await;
  let after = ProcessUsage::of_tree(app.pid());
  let wakeups = wakeups_between(&wakeups_before, &thread_wakeups(app.pid()));
  let log = stack.server_log();
  let requests: Vec<String> = log
    .get(log_before..)
    .unwrap_or_default()
    .lines()
    .filter(|line| line.contains("finished processing request"))
    .map(|line| {
      line
        .split("request{")
        .nth(1)
        .unwrap_or(line)
        .chars()
        .take(120)
        .collect()
    })
    .collect();
  let seconds = period.as_secs_f64();
  let mut processes = BTreeMap::new();
  for (name, end) in &after {
    let start = before.get(name).copied().unwrap_or_default();
    processes.insert(
      name.clone(),
      ProcessIdle {
        cpu_percent: cpu_percent(end.cpu_time.saturating_sub(start.cpu_time), period),
        wakeups_per_second: wakeups.get(name).copied().unwrap_or(0) as f64 / seconds,
        pss_mib: mib(end.pss_bytes),
        pss_anon_mib: mib(end.pss_anon_bytes),
        processes: end.process_count,
      },
    );
  }
  Idle {
    seconds,
    cpu_percent: processes.values().map(|process| process.cpu_percent).sum(),
    wakeups_per_second: processes.values().map(|process| process.wakeups_per_second).sum(),
    pss_mib: processes.values().map(|process| process.pss_mib).sum(),
    server_requests: requests.len(),
    requests,
    processes,
  }
}

fn cpu_percent(time: Duration, period: Duration) -> f64 {
  time.as_secs_f64() / period.as_secs_f64() * 100.0
}

#[derive(Debug, Default)]
struct Report {
  seconds_sign_in_and_unlock: f64,
  seconds_start_to_first_channel: f64,
  seconds_start_until_settled: f64,
  switch_ms_p50: f64,
  switch_ms_p95: f64,
  /// Window shown, untouched after the channel switches.
  visible: Idle,
  /// Window closed to the tray.
  hidden: Idle,
}

/// The app over an idle period.
#[derive(Debug, Default)]
struct Idle {
  seconds: f64,
  /// The whole process group, percent of one core.
  cpu_percent: f64,
  wakeups_per_second: f64,
  /// At the end of the period.
  pss_mib: f64,
  /// Requests the server answered meanwhile (the app is the only client doing anything).
  server_requests: usize,
  requests: Vec<String>,
  /// By process name: the shell with the core (`teamcraft-deskt`), `WebKitWebProces`,
  /// `WebKitNetworkPr`.
  processes: BTreeMap<String, ProcessIdle>,
}

impl Idle {
  fn json(&self) -> serde_json::Value {
    let processes: serde_json::Map<String, serde_json::Value> = self
      .processes
      .iter()
      .map(|(name, process)| {
        let value = json!({
          "cpu_percent": process.cpu_percent,
          "wakeups_per_second": process.wakeups_per_second,
          "pss_mib": process.pss_mib,
          "pss_anon_mib": process.pss_anon_mib,
          "processes": process.processes,
        });
        (name.clone(), value)
      })
      .collect();
    json!({
      "seconds": self.seconds,
      "cpu_percent": self.cpu_percent,
      "wakeups_per_second": self.wakeups_per_second,
      "pss_mib": self.pss_mib,
      "server_requests": self.server_requests,
      "requests": self.requests,
      "processes": processes,
    })
  }
}

#[derive(Debug, Default)]
struct ProcessIdle {
  cpu_percent: f64,
  wakeups_per_second: f64,
  pss_mib: f64,
  pss_anon_mib: f64,
  processes: usize,
}

impl Report {
  fn print_and_save(&self, scale: Scale) {
    let text = serde_json::to_string_pretty(&self.json()).unwrap();
    eprintln!("desktop perf ({}):\n{text}", scale.name);
    let dir = artifact_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("desktop-perf-{}.json", scale.name)), text).unwrap();
  }

  fn json(&self) -> serde_json::Value {
    json!({
      "seconds_sign_in_and_unlock": self.seconds_sign_in_and_unlock,
      "seconds_start_to_first_channel": self.seconds_start_to_first_channel,
      "seconds_start_until_settled": self.seconds_start_until_settled,
      "switch_ms_p50": self.switch_ms_p50,
      "switch_ms_p95": self.switch_ms_p95,
      "visible": self.visible.json(),
      "hidden": self.hidden.json(),
    })
  }

  fn assert_budgets(&self) {
    let hidden = &self.hidden;
    assert!(
      hidden.cpu_percent < HIDDEN_CPU_PERCENT_LIMIT,
      "busy while hidden in the tray: {self:#?}"
    );
    assert!(
      hidden.wakeups_per_second < HIDDEN_WAKEUPS_PER_SECOND_LIMIT,
      "wakes up while hidden in the tray: {self:#?}"
    );
    // No polling: changes come over the gateway, whose heartbeat is no HTTP request.
    assert_eq!(
      hidden.server_requests, 0,
      "requests while hidden in the tray: {self:#?}"
    );
  }
}
