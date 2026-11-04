//! An open administration console on a large instance must not use up the address's rate limit
//! (issue #1217): the real web client in headless Chromium, signed in as an owner, with a workspace
//! of 220 channels, on the Retention page. The test counts the API requests of the page when the
//! console opens and over a period afterwards, in which the console's timer fires several times.
//!
//! `ADMIN_REQUESTS_CHANNELS` (default 220) sets the size of the workspace, `ADMIN_REQUESTS_WINDOW_SECS`
//! (default 100) how long the open console is watched. The numbers are printed and written to
//! `target/e2e-artifacts/admin-console-requests.json`. By hand, against another web build (for
//! example one from before a change), set `E2E_WEB_DIR`:
//!
//! ```sh
//! ADMIN_REQUESTS_CHANNELS=250 cargo test -p teamcraft-e2e --features browser --test suite -- --nocapture admin_console_requests::
//! ```

use std::collections::BTreeMap;
use std::time::Duration;

use chromiumoxide::cdp::browser_protocol::network::{
  EnableParams as NetworkEnableParams, EventRequestWillBeSent,
};
use chromiumoxide::page::Page;
use futures_util::StreamExt;
use teamcraft_e2e::browser::artifact_dir;
use teamcraft_e2e::scenario::space_with;

use teamcraft_e2e::browser::{BrowserSession, lease_web_port, web_dir};
use teamcraft_e2e::{Peer, Stack};

use crate::perf::sign_in_and_unlock;
use crate::web::{TEST_CSP, click_text, wait_for_text};

/// The performance test's port: the browser tests' 8082 may be taken, and the dev realm registers both.
const PORT: u16 = 8083;
const WEB_ORIGIN: &str = "http://127.0.0.1:8083";

const DEFAULT_CHANNELS: usize = 220;
const DEFAULT_WINDOW_SECS: u64 = 100;
/// No API request for this long: the console is done opening.
const QUIET: Duration = Duration::from_secs(8);
/// The opening reads the lists, a few pages of them and the audit log: well below one request per
/// channel. Before the fix it was about 2 per channel (up to 100 channels) plus one per user.
const MAX_OPENING_REQUESTS: usize = 80;
/// The console's timer reads the open section's data; the address allows 1200 a minute.
const MAX_REQUESTS_PER_MINUTE: f64 = 30.0;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_open_admin_console_stays_far_below_the_rate_limit_of_the_address() {
  let Some(web_dir) = web_dir() else {
    assert!(
      std::env::var("E2E_REQUIRED").map_or(true, |value| value != "1"),
      "no web build: run tests/e2e/web/build.sh"
    );
    eprintln!("skipping: no web build (tests/e2e/web/build.sh)");
    return;
  };
  // One test at a time owns the port. Seeding the workspace is one address making thousands of
  // requests; what the console adds is counted from the browser's own network events instead.
  let _turn = lease_web_port(PORT).await;
  let Some(stack) = Stack::builder()
    .with_port(PORT)
    .with_web_dir(web_dir)
    .with_env("TEAMCRAFT_WEB__CONTENT_SECURITY_POLICY", TEST_CSP)
    .with_env("TEAMCRAFT_SECURITY__RATE_LIMITS__IS_ENABLED", "false")
    .start()
    .await
  else {
    return;
  };
  let Some(browser) = BrowserSession::launch().await else {
    return;
  };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let channels = env_number("ADMIN_REQUESTS_CHANNELS", DEFAULT_CHANNELS as u64) as usize;
  let window = Duration::from_secs(env_number("ADMIN_REQUESTS_WINDOW_SECS", DEFAULT_WINDOW_SECS));

  let space = space_with(&stack, &alice, "Admin Load Space", &[]).await;
  for number in 0..channels {
    alice
      .client
      .create_channel(space, &format!("load-{number:03}"))
      .await
      .expect("creating a channel");
  }
  // The browser enrolls as a new device and gets the channels' keys from the key bag.
  alice.client.sync_account_data().await.unwrap();

  let report = std::sync::Arc::new(std::sync::Mutex::new(None));
  let slot = report.clone();
  browser
    .screenshot_on_failure("admin-console-requests", |page| async move {
      sign_in_and_unlock(&page, "alice").await;
      page.execute(NetworkEnableParams::default()).await.unwrap();
      let mut requests = page.event_listener::<EventRequestWillBeSent>().await.unwrap();

      // The new device's own catch-up is not the console's: wait until the app is quiet.
      let catch_up = collect_until_quiet(&mut requests).await;
      eprintln!(
        "catch-up before the console opened: {} API requests",
        catch_up.len()
      );

      open_retention_page(&page).await;
      wait_for_text(&page, "# load-0").await;
      let opening = collect_until_quiet(&mut requests).await;
      let watched = collect_for(&mut requests, window).await;

      let per_minute = watched.len() as f64 * 60.0 / window.as_secs_f64();
      eprintln!("channels: {channels}");
      eprintln!("opening the console: {} API requests", opening.len());
      print_kinds("opening", &opening);
      eprintln!(
        "open console over {} s: {} API requests, {per_minute:.1} per minute",
        window.as_secs(),
        watched.len()
      );
      print_kinds("open", &watched);
      *slot.lock().unwrap() = Some(Report {
        channels,
        opening_requests: opening.len(),
        window_secs: window.as_secs(),
        window_requests: watched.len(),
        requests_per_minute: per_minute,
        opening_kinds: kinds(&opening),
        window_kinds: kinds(&watched),
      });

      assert_eq!(
        per_object(&opening) + per_object(&watched),
        0,
        "requests per channel or per user"
      );
      assert!(
        opening.len() <= MAX_OPENING_REQUESTS,
        "{} requests when the console opens",
        opening.len()
      );
      assert!(
        per_minute <= MAX_REQUESTS_PER_MINUTE,
        "{per_minute:.1} requests per minute from an open console"
      );
    })
    .await;
  if let Some(report) = report.lock().unwrap().take() {
    let _ = std::fs::create_dir_all(artifact_dir());
    let _ = std::fs::write(
      artifact_dir().join("admin-console-requests.json"),
      serde_json::to_string_pretty(&report.to_json()).unwrap(),
    );
  }
  browser.close().await;
  stack.stop().await;
}

/// Opens the console from the account menu, then its Retention page.
async fn open_retention_page(page: &Page) {
  page
    .evaluate("document.querySelector('.avatar-btn').click()")
    .await
    .unwrap();
  click_text(page, "Administration").await;
  crate::web::wait_for_element(page, ".admin-nav-item").await;
  page
    .evaluate(
      "[...document.querySelectorAll('.admin-nav-item')].find(e => e.innerText.trim() === 'Retention').click()",
    )
    .await
    .unwrap();
}

/// The API paths the page asks for until it made none for [`QUIET`].
async fn collect_until_quiet(
  requests: &mut (impl futures_util::Stream<Item = std::sync::Arc<EventRequestWillBeSent>> + Unpin),
) -> Vec<String> {
  let mut paths = Vec::new();
  while let Ok(Some(event)) = tokio::time::timeout(QUIET, requests.next()).await {
    if let Some(path) = api_path(&event.request.url) {
      paths.push(path);
    }
  }
  paths
}

/// The API paths the page asks for during `period`.
async fn collect_for(
  requests: &mut (impl futures_util::Stream<Item = std::sync::Arc<EventRequestWillBeSent>> + Unpin),
  period: Duration,
) -> Vec<String> {
  let mut paths = Vec::new();
  let deadline = tokio::time::sleep(period);
  tokio::pin!(deadline);
  loop {
    tokio::select! {
      () = &mut deadline => break,
      Some(event) = requests.next() => {
        if let Some(path) = api_path(&event.request.url) {
          paths.push(path);
        }
      },
    }
  }
  paths
}

fn api_path(url: &str) -> Option<String> {
  url
    .strip_prefix(WEB_ORIGIN)
    .filter(|path| path.starts_with("/api/"))
    .map(str::to_owned)
}

/// Requests for one channel's or one user's details.
fn per_object(paths: &[String]) -> usize {
  paths
    .iter()
    .filter(|path| {
      let path = path.split('?').next().unwrap_or_default();
      (path.starts_with("/api/v1/admin/conversations/") && path.ends_with("/members"))
        || (path.starts_with("/api/v1/conversations/") && path.ends_with("/retention"))
        || (path.starts_with("/api/v1/admin/users/") && path.ends_with("/devices"))
    })
    .count()
}

/// Requests by kind: numbers in the path stand for "N".
fn kinds(paths: &[String]) -> BTreeMap<String, usize> {
  let mut found = BTreeMap::new();
  for path in paths {
    let mut kind = String::new();
    let mut in_number = false;
    for character in path.split('?').next().unwrap_or_default().chars() {
      if character.is_ascii_digit() {
        if !in_number {
          kind.push('N');
        }
        in_number = true;
      } else {
        in_number = false;
        kind.push(character);
      }
    }
    *found.entry(kind).or_insert(0) += 1;
  }
  found
}

fn print_kinds(what: &str, paths: &[String]) {
  for (kind, count) in kinds(paths) {
    eprintln!("  {what}: {count:4} x {kind}");
  }
}

fn env_number(name: &str, default: u64) -> u64 {
  std::env::var(name)
    .ok()
    .and_then(|value| value.parse().ok())
    .unwrap_or(default)
}

struct Report {
  channels: usize,
  opening_requests: usize,
  window_secs: u64,
  window_requests: usize,
  requests_per_minute: f64,
  opening_kinds: BTreeMap<String, usize>,
  window_kinds: BTreeMap<String, usize>,
}

impl Report {
  fn to_json(&self) -> serde_json::Value {
    serde_json::json!({
      "channels": self.channels,
      "opening_requests": self.opening_requests,
      "window_secs": self.window_secs,
      "window_requests": self.window_requests,
      "requests_per_minute": self.requests_per_minute,
      "opening_kinds": self.opening_kinds,
      "window_kinds": self.window_kinds,
    })
  }
}
