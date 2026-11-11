//! Firefox over WebDriver BiDi (#1721): the Firefox people have, started with
//! `--remote-debugging-port`, driven over its own WebSocket. No geckodriver, no Node, and no new
//! crate: the protocol is JSON over the `tokio-tungstenite` the suite already uses. chromiumoxide
//! can't drive Firefox (it speaks the DevTools protocol, which Firefox dropped).
//!
//! The page's console is collected as it arrives ([`FirefoxSession::console`]); a failing scenario
//! leaves a screenshot in `target/e2e-artifacts/` like the Chromium tests.

use std::future::Future;
use std::panic::{AssertUnwindSafe, resume_unwind};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use base64::Engine;
use futures_util::{FutureExt, SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::browser::artifact_dir;
use crate::process::free_tcp_port;

/// How long Firefox may take to start and open its remote agent.
const START_TIMEOUT: Duration = Duration::from_secs(60);
/// How long one command may take (a page script that waits, a navigation).
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
/// What Firefox prints once its remote agent listens.
const LISTENING: &str = "WebDriver BiDi listening on ";
/// Preferences of the test profile: a fake camera and microphone without a prompt (as
/// Chromium's `--use-fake-device-for-media-stream`), English pages, and none of the first-run
/// pages and background traffic of a new profile.
const PREFERENCES: &[(&str, &str)] = &[
  ("media.navigator.streams.fake", "true"),
  ("media.navigator.permission.disabled", "true"),
  ("intl.accept_languages", "\"en-US, en\""),
  ("browser.shell.checkDefaultBrowser", "false"),
  ("browser.startup.homepage_override.mstone", "\"ignore\""),
  ("browser.aboutwelcome.enabled", "false"),
  ("datareporting.policy.dataSubmissionEnabled", "false"),
  ("toolkit.telemetry.reportingpolicy.firstRun", "false"),
  ("app.update.disabledForTesting", "true"),
  // JSON (`/healthz`) as a plain page of its origin, whose storage the tests can set; the viewer
  // is a page of its own.
  ("devtools.jsonview.enabled", "false"),
];

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A headless Firefox with one tab, killed on drop.
pub struct FirefoxSession {
  child: Child,
  socket: Socket,
  context: String,
  next_id: u64,
  profile: PathBuf,
  console: Arc<Mutex<Vec<String>>>,
}

impl FirefoxSession {
  /// Starts headless Firefox (`$FIREFOX`, else `firefox` on `PATH` or the macOS app). `None`
  /// (with a skip message) when there is none, unless `E2E_REQUIRED=1` demands one.
  pub async fn launch() -> Option<Self> {
    Self::launch_with(&[]).await
  }

  /// [`Self::launch`] with more preferences (name, value as JavaScript) in the profile.
  pub async fn launch_with(preferences: &[(&str, &str)]) -> Option<Self> {
    let is_required = std::env::var("E2E_REQUIRED").is_ok_and(|value| value == "1");
    let Some(executable) = find_firefox() else {
      assert!(!is_required, "no Firefox found (set FIREFOX=/path/to/firefox)");
      eprintln!("skipping: no Firefox found (set FIREFOX=/path/to/firefox)");
      return None;
    };
    Some(
      Self::start(&executable, preferences)
        .await
        .unwrap_or_else(|error| panic!("starting {}: {error:#}", executable.display())),
    )
  }

  async fn start(executable: &Path, preferences: &[(&str, &str)]) -> anyhow::Result<Self> {
    let profile = std::env::temp_dir().join(format!("teamcraft-e2e-firefox-{:016x}", random_u64()));
    std::fs::create_dir_all(&profile)?;
    let user_js: String = PREFERENCES
      .iter()
      .chain(preferences)
      .map(|(name, value)| format!("user_pref(\"{name}\", {value});\n"))
      .collect();
    std::fs::write(profile.join("user.js"), user_js)?;
    let port = free_tcp_port();
    let mut child = Command::new(executable)
      .args(["--headless", "--no-remote", "--new-instance", "--profile"])
      .arg(&profile)
      .args(["--remote-debugging-port", &port.to_string(), "about:blank"])
      .env("MOZ_CRASHREPORTER_DISABLE", "1")
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::piped())
      .kill_on_drop(true)
      .spawn()
      .context("starting Firefox")?;
    let stderr = child.stderr.take().context("Firefox's stderr")?;
    let address = tokio::time::timeout(START_TIMEOUT, listening_address(stderr))
      .await
      .context("Firefox's remote agent never listened")??;
    let (socket, _) = tokio_tungstenite::connect_async(format!("{address}/session"))
      .await
      .context("connecting to Firefox's remote agent")?;
    let mut session = Self {
      child,
      socket,
      context: String::new(),
      next_id: 0,
      profile,
      console: Arc::default(),
    };
    session.call("session.new", json!({ "capabilities": {} })).await?;
    session
      .call("session.subscribe", json!({ "events": ["log.entryAdded"] }))
      .await?;
    let tree = session.call("browsingContext.getTree", json!({})).await?;
    session.context = tree["contexts"][0]["context"]
      .as_str()
      .context("Firefox has no tab")?
      .to_owned();
    session
      .call(
        "browsingContext.setViewport",
        json!({ "context": session.context, "viewport": { "width": 1360, "height": 840 } }),
      )
      .await?;
    Ok(session)
  }

  /// Everything the page logged so far (`console.*` and uncaught errors), oldest first.
  pub fn console(&self) -> Arc<Mutex<Vec<String>>> {
    Arc::clone(&self.console)
  }

  /// Opens `url` and waits for its load event.
  pub async fn goto(&mut self, url: &str) {
    let params = json!({ "context": self.context, "url": url, "wait": "complete" });
    self
      .call("browsingContext.navigate", params)
      .await
      .unwrap_or_else(|error| panic!("opening {url}: {error:#}"));
  }

  /// Runs `expression` in the page (a promise is awaited) as if the user had just clicked, so
  /// it may start audio; its value as JSON.
  pub async fn evaluate(&mut self, expression: &str) -> Value {
    self
      .try_evaluate(expression)
      .await
      .unwrap_or_else(|error| panic!("{error:#}"))
  }

  async fn try_evaluate(&mut self, expression: &str) -> anyhow::Result<Value> {
    let params = json!({
      "expression": expression,
      "target": { "context": self.context },
      "awaitPromise": true,
      "userActivation": true,
      "resultOwnership": "none",
    });
    let answer = self.call("script.evaluate", params).await?;
    if answer["type"] == "exception" {
      bail!(
        "{expression}: {}",
        answer["exceptionDetails"]["text"].as_str().unwrap_or("threw")
      );
    }
    Ok(plain_value(&answer["result"]))
  }

  /// Waits until `expression` is true in the page, polling; panics after `timeout`.
  pub async fn wait_until(&mut self, what: &str, expression: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
      if self.try_evaluate(expression).await.ok() == Some(Value::Bool(true)) {
        return;
      }
      assert!(Instant::now() < deadline, "timed out waiting for {what}");
      tokio::time::sleep(Duration::from_millis(250)).await;
    }
  }

  /// Waits until the page shows `text`.
  pub async fn wait_for_text(&mut self, text: &str, timeout: Duration) {
    let expression = format!(
      "!!document.body && document.body.innerText.includes({})",
      json!(text)
    );
    self
      .wait_until(&format!("the text {text:?}"), &expression, timeout)
      .await;
  }

  /// Waits until an element matches `selector`.
  pub async fn wait_for_element(&mut self, selector: &str, timeout: Duration) {
    let expression = format!("!!document.querySelector({})", json!(selector));
    self
      .wait_until(&format!("an element {selector:?}"), &expression, timeout)
      .await;
  }

  /// Clicks the first button or link whose text is `text` (or contains it).
  pub async fn click_text(&mut self, text: &str) {
    let expression = format!(
      "(() => {{ const text = {}; \
         const all = [...document.querySelectorAll('button, a, [role=button]')]; \
         const target = all.find(e => e.innerText.trim() === text) ?? all.find(e => e.innerText.includes(text)); \
         if (!target) return false; target.click(); return true; }})()",
      json!(text)
    );
    assert_eq!(
      self.evaluate(&expression).await,
      Value::Bool(true),
      "nothing to click named {text:?}"
    );
  }

  /// Clicks the element `selector` matches.
  pub async fn click_selector(&mut self, selector: &str) {
    let expression = format!(
      "(() => {{ const target = document.querySelector({}); if (!target) return false; target.click(); return true; }})()",
      json!(selector)
    );
    assert_eq!(
      self.evaluate(&expression).await,
      Value::Bool(true),
      "no element {selector:?}"
    );
  }

  /// Types `text` into the input `selector` matches, as the app's `input` handlers see typing.
  pub async fn fill(&mut self, selector: &str, text: &str) {
    let expression = format!(
      "(() => {{ const input = document.querySelector({}); if (!input) return false; input.focus(); \
         const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set; \
         set.call(input, {}); input.dispatchEvent(new Event('input', {{ bubbles: true }})); \
         input.dispatchEvent(new Event('change', {{ bubbles: true }})); return true; }})()",
      json!(selector),
      json!(text)
    );
    assert_eq!(
      self.evaluate(&expression).await,
      Value::Bool(true),
      "no input {selector:?}"
    );
  }

  /// Runs `scenario` (which may use this session) and returns its outcome, a panic included, for
  /// [`Self::screenshot_if_failed`].
  pub async fn outcome(scenario: impl Future<Output = ()>) -> std::thread::Result<()> {
    AssertUnwindSafe(scenario).catch_unwind().await
  }

  /// After a scenario that panicked ([`Self::outcome`]): saves a screenshot named after
  /// `test_name`, prints the page's console, and panics again.
  pub async fn screenshot_if_failed(&mut self, test_name: &str, outcome: std::thread::Result<()>) {
    if let Err(panic) = outcome {
      let path = artifact_dir().join(format!("{test_name}.png"));
      let _ = std::fs::create_dir_all(artifact_dir());
      match self.screenshot(&path).await {
        Ok(()) => eprintln!("screenshot of the failure: {}", path.display()),
        Err(error) => eprintln!("could not take a screenshot: {error:#}"),
      }
      for line in self.console.lock().unwrap().iter() {
        eprintln!("console: {line}");
      }
      resume_unwind(panic);
    }
  }

  async fn screenshot(&mut self, path: &Path) -> anyhow::Result<()> {
    let answer = self
      .call(
        "browsingContext.captureScreenshot",
        json!({ "context": self.context }),
      )
      .await?;
    let data = answer["data"].as_str().context("no screenshot data")?;
    std::fs::write(path, base64::engine::general_purpose::STANDARD.decode(data)?)?;
    Ok(())
  }

  pub async fn close(mut self) {
    let _ = self.call("browser.close", json!({})).await;
    let _ = tokio::time::timeout(Duration::from_secs(10), self.child.wait()).await;
    let _ = self.child.kill().await;
    let _ = std::fs::remove_dir_all(&self.profile);
  }

  /// Sends one command and reads until its answer, keeping the console entries that come first.
  async fn call(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
    self.next_id += 1;
    let id = self.next_id;
    let command = json!({ "id": id, "method": method, "params": params });
    self.socket.send(Message::text(command.to_string())).await?;
    let deadline = tokio::time::Instant::now() + COMMAND_TIMEOUT;
    loop {
      let message = tokio::time::timeout_at(deadline, self.socket.next())
        .await
        .with_context(|| format!("{method} got no answer"))?
        .context("Firefox closed the connection")??;
      let Message::Text(text) = message else {
        continue;
      };
      let value: Value = serde_json::from_str(&text)?;
      if value["type"] == "event" {
        self.on_event(&value);
        continue;
      }
      if value["id"] != json!(id) {
        continue;
      }
      if value["type"] == "error" {
        bail!("{method}: {} ({})", value["error"], value["message"]);
      }
      return Ok(value["result"].clone());
    }
  }

  fn on_event(&self, event: &Value) {
    if event["method"] != "log.entryAdded" {
      return;
    }
    let entry = &event["params"];
    let level = entry["level"].as_str().unwrap_or("log");
    let text = entry["text"].as_str().unwrap_or_default();
    let line = format!("{level}: {text}");
    if std::env::var_os("E2E_CONSOLE").is_some() {
      eprintln!("firefox console {line}");
    }
    self.console.lock().unwrap().push(line);
  }
}

/// The remote agent's address from Firefox's stderr (`ws://127.0.0.1:<port>`).
async fn listening_address(stderr: tokio::process::ChildStderr) -> anyhow::Result<String> {
  let mut lines = BufReader::new(stderr).lines();
  while let Some(line) = lines.next_line().await? {
    if let Some(address) = line.split(LISTENING).nth(1) {
      let address = address.trim().to_owned();
      // Firefox keeps writing to stderr; a full pipe would stall it.
      tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
      return Ok(address);
    }
  }
  bail!("Firefox exited before its remote agent listened")
}

/// A BiDi remote value as plain JSON (strings, numbers, booleans, null, arrays and objects of them).
fn plain_value(remote: &Value) -> Value {
  match remote["type"].as_str() {
    Some("string" | "number" | "boolean") => remote["value"].clone(),
    Some("array") => Value::Array(
      remote["value"]
        .as_array()
        .map(|items| items.iter().map(plain_value).collect())
        .unwrap_or_default(),
    ),
    Some("object") => Value::Object(
      remote["value"]
        .as_array()
        .map(|entries| {
          entries
            .iter()
            .filter_map(|entry| Some((entry[0].as_str()?.to_owned(), plain_value(&entry[1]))))
            .collect()
        })
        .unwrap_or_default(),
    ),
    _ => Value::Null,
  }
}

fn find_firefox() -> Option<PathBuf> {
  if let Some(path) = std::env::var_os("FIREFOX") {
    return Some(PathBuf::from(path));
  }
  let on_path = std::env::var_os("PATH").and_then(|path| {
    std::env::split_paths(&path)
      .map(|dir| dir.join("firefox"))
      .find(|candidate| candidate.is_file())
  });
  on_path.or_else(|| {
    let app = PathBuf::from("/Applications/Firefox.app/Contents/MacOS/firefox");
    app.is_file().then_some(app)
  })
}

fn random_u64() -> u64 {
  use rand_core::RngCore;
  rand_core::OsRng.next_u64()
}
