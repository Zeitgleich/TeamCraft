//! Headless Chromium over the DevTools protocol (chromiumoxide): no driver binary and no Node.
//! Failing tests leave a screenshot in `target/e2e-artifacts/`.

use std::future::Future;
use std::panic::{AssertUnwindSafe, resume_unwind};
use std::path::PathBuf;

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::page::{Page, ScreenshotParams};
use futures_util::{FutureExt, StreamExt};

/// A browser process with one page; closed on drop of the handler task.
pub struct BrowserSession {
  browser: Browser,
  pub page: Page,
  handler: tokio::task::JoinHandle<()>,
  profile: PathBuf,
}

impl BrowserSession {
  /// Launches headless Chromium. `None` (with a skip message) when no browser is installed, unless
  /// `E2E_REQUIRED=1` demands one.
  pub async fn launch() -> Option<Self> {
    Self::launch_with(&[]).await
  }

  /// [`Self::launch`] with more command line switches (fake camera and microphone for calls).
  pub async fn launch_with(extra_args: &[&str]) -> Option<Self> {
    let is_required = std::env::var("E2E_REQUIRED").is_ok_and(|value| value == "1");
    let Some(executable) = find_chrome() else {
      assert!(
        !is_required,
        "no Chrome or Chromium found (set CHROME=/path/to/chrome)"
      );
      eprintln!("skipping: no Chrome or Chromium found (set CHROME=/path/to/chrome)");
      return None;
    };
    // A fresh profile per launch: the default one is shared and would keep Keycloak's session cookie.
    let profile = std::env::temp_dir().join(format!("teamcraft-e2e-chrome-{:016x}", random_u64()));
    let mut builder = BrowserConfig::builder();
    // chromiumoxide adds the leading `--` itself.
    for arg in extra_args {
      builder = builder.arg(arg.trim_start_matches('-'));
    }
    let config = builder
      .chrome_executable(executable)
      .user_data_dir(profile.clone())
      // CI containers run as root without user namespaces.
      .no_sandbox()
      // The UI follows the browser language; the tests look for English texts.
      .arg("lang=en-US")
      .arg("accept-lang=en-US")
      .window_size(1360, 840)
      // Page scripts that wait for the app (perf.rs) may take longer than the 30 s default.
      .request_timeout(std::time::Duration::from_secs(300))
      .build()
      .expect("browser config");
    let (browser, mut handler) = Browser::launch(config).await.expect("launching the browser");
    let handler = tokio::spawn(async move { while handler.next().await.is_some() {} });
    let page = browser.new_page("about:blank").await.expect("opening a page");
    let_workers_start(&page).await;
    Some(Self {
      browser,
      page,
      handler,
      profile,
    })
  }

  /// Runs `scenario`; if it panics, saves a screenshot named after `test_name`, then re-panics.
  pub async fn screenshot_on_failure<F, Fut>(&self, test_name: &str, scenario: F)
  where
    F: FnOnce(Page) -> Fut,
    Fut: Future<Output = ()>,
  {
    let outcome = AssertUnwindSafe(scenario(self.page.clone())).catch_unwind().await;
    if let Err(panic) = outcome {
      let path = artifact_dir().join(format!("{test_name}.png"));
      let _ = std::fs::create_dir_all(artifact_dir());
      match self
        .page
        .save_screenshot(ScreenshotParams::builder().full_page(true).build(), &path)
        .await
      {
        Ok(_) => eprintln!("screenshot of the failure: {}", path.display()),
        Err(error) => eprintln!("could not take a screenshot: {error}"),
      }
      let url = self.page.url().await.ok().flatten().unwrap_or_default();
      eprintln!("page at the failure: {url}");
      resume_unwind(panic);
    }
  }

  /// Opens another tab of the same browser (same profile and origin storage) at `url`.
  pub async fn new_page(&self, url: &str) -> Page {
    let page = self.browser.new_page(url).await.expect("opening a page");
    let_workers_start(&page).await;
    prefer_mouse(&page).await;
    page
  }

  /// The browser's own profile directory: every process of this browser has it on its command line
  /// (see [`crate::resources::ProcessUsage::of_matching`]).
  pub fn profile_dir(&self) -> &std::path::Path {
    &self.profile
  }

  pub async fn close(mut self) {
    let _ = self.browser.close().await;
    let _ = self.browser.wait().await;
    self.handler.abort();
    let _ = std::fs::remove_dir_all(&self.profile);
  }
}

/// Exclusive use of a web client port (one the dev realm registers as redirect URI: 8082, 8083)
/// for as long as it is held, across every test process on the machine: other checkouts run the
/// suite too, and a second server on the port fails to start.
pub struct WebPortLease {
  _turn: tokio::sync::MutexGuard<'static, ()>,
  _file: std::fs::File,
}

static WEB_PORT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Waits until no other test (in this process or another) uses `port`.
pub async fn lease_web_port(port: u16) -> WebPortLease {
  let turn = WEB_PORT.lock().await;
  let path = std::env::temp_dir().join(format!("teamcraft-e2e-web-port-{port}.lock"));
  let file = std::fs::File::options()
    .create(true)
    .truncate(false)
    .write(true)
    .open(&path)
    .expect("opening the port lock file");
  // An advisory lock, released when the file is closed (also when the test process dies).
  let file = tokio::task::spawn_blocking(move || {
    file.lock().expect("locking the port lock file");
    file
  })
  .await
  .expect("waiting for the port lock");
  WebPortLease {
    _turn: turn,
    _file: file,
  }
}

/// `$E2E_ARTIFACTS`, else `target/e2e-artifacts` of the workspace.
pub fn artifact_dir() -> PathBuf {
  std::env::var_os("E2E_ARTIFACTS")
    .map(PathBuf::from)
    .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/e2e-artifacts"))
}

/// The web client built by `tests/e2e/web/build.sh` (or `$E2E_WEB_DIR`).
pub fn web_dir() -> Option<PathBuf> {
  let dir = std::env::var_os("E2E_WEB_DIR")
    .map(PathBuf::from)
    .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/e2e-web"));
  dir
    .join("index.html")
    .exists()
    .then(|| dir.canonicalize().unwrap_or(dir))
}

fn random_u64() -> u64 {
  use rand_core::RngCore;
  rand_core::OsRng.next_u64()
}

fn find_chrome() -> Option<PathBuf> {
  if let Some(path) = std::env::var_os("CHROME") {
    return Some(PathBuf::from(path));
  }
  let path = std::env::var_os("PATH")?;
  [
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
    "chrome",
  ]
  .iter()
  .find_map(|name| {
    std::env::split_paths(&path)
      .map(|dir| dir.join(name))
      .find(|candidate| candidate.is_file())
  })
}

/// chromiumoxide attaches to every target a page starts and pauses it until it is told to run
/// (`waitForDebuggerOnStart`); the page's service worker then stays in "starting" for good, and
/// `navigator.serviceWorker.ready` never settles (#460). The page's own auto-attach is set again
/// without the pause.
async fn let_workers_start(page: &Page) {
  use chromiumoxide::cdp::browser_protocol::target::SetAutoAttachParams;
  let params = SetAutoAttachParams::builder()
    .auto_attach(true)
    .wait_for_debugger_on_start(false)
    .flatten(true)
    .build()
    .expect("auto-attach parameters");
  page
    .execute(params)
    .await
    .expect("letting the page's workers start");
}

/// A headless browser has no mouse and, on a machine with a touch screen, reports a coarse primary
/// pointer: Enter would then insert a newline instead of sending (issue #444). Turns touch off.
pub async fn prefer_mouse(page: &Page) {
  use chromiumoxide::cdp::browser_protocol::emulation::SetTouchEmulationEnabledParams;
  page
    .execute(SetTouchEmulationEnabledParams::new(false))
    .await
    .expect("turning touch emulation off");
}
