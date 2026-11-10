//! The desktop app as it ships on Linux (Tauri on WebKitGTK), started from its binary and driven
//! over WebKit's remote inspector, for measuring it (perf of the minimized app, #1022).
//!
//! Nothing of the user's session is used: every run has its own session bus with its own keyring
//! (`gnome-keyring-daemon`, where the app keeps the key of its secrets file), its own home and XDG
//! directories, and a tray watcher on that bus, so closing the window hides it to the tray as on a
//! desktop with a tray icon. The system browser the sign-in opens is a script that writes the
//! address down; [`DesktopSession::sign_in_at_keycloak`] fills in Keycloak's form from there.
//!
//! Needs a debug build of `teamcraft-desktop` with the web client embedded (`E2E_DESKTOP_BIN`, see
//! README.md), a display (`DISPLAY`, e.g. `xvfb-run`), `dbus-daemon` and `gnome-keyring-daemon`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::process::{Service, free_tcp_port};

/// The desktop binary under test.
pub const BINARY_VARIABLE: &str = "E2E_DESKTOP_BIN";
/// The app's identifier: its config directory below `XDG_CONFIG_HOME`.
const APP_IDENTIFIER: &str = "de.zeitgleich.teamcraft";
const KEYRING_PASSWORD: &str = "e2e";
/// How long the inspector may take to answer, and the page to show what a step waits for.
const WAIT: Duration = Duration::from_secs(60);
/// Answers of the page target come wrapped; their IDs are offset from the wrapper's.
const WRAPPER_ID_OFFSET: u64 = 1_000_000;

/// A private desktop session: session bus, keyring, tray watcher, home and the fake browser. Lives
/// across restarts of the app, like a user's login session.
pub struct DesktopSession {
  dir: PathBuf,
  bus_address: String,
  /// The session bus and the keyring, killed on drop.
  _daemons: Vec<Child>,
  /// Owns the tray watcher's name on the bus.
  _tray_watcher: zbus::Connection,
  binary: PathBuf,
}

/// A running desktop app. Killed on drop (its WebKit processes follow it).
pub struct DesktopApp {
  service: Service,
  inspector_port: u16,
  maybe_inspector: Option<Inspector>,
}

/// One connection to WebKit's remote inspector, to the app's page.
struct Inspector {
  socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
  page_target: String,
  next_id: u64,
}

/// An `org.kde.StatusNotifierWatcher` with a host, so the app shows its tray icon (to nobody) and
/// closing the window hides it to the tray.
struct TrayWatcher;

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl TrayWatcher {
  fn register_status_notifier_item(&self, _service: &str) {}

  fn register_status_notifier_host(&self, _service: &str) {}

  #[zbus(property)]
  fn is_status_notifier_host_registered(&self) -> bool {
    true
  }

  #[zbus(property)]
  fn registered_status_notifier_items(&self) -> Vec<String> {
    Vec::new()
  }

  #[zbus(property)]
  fn protocol_version(&self) -> i32 {
    0
  }
}

impl DesktopSession {
  /// `None` (a skip message) without [`BINARY_VARIABLE`], unless `E2E_DESKTOP_REQUIRED=1`.
  pub async fn start(name: &str) -> Option<Self> {
    let is_required = std::env::var("E2E_DESKTOP_REQUIRED").is_ok_and(|value| value == "1");
    let Some(binary) = std::env::var_os(BINARY_VARIABLE).map(PathBuf::from) else {
      assert!(!is_required, "{BINARY_VARIABLE} names no desktop binary");
      eprintln!("skipping: no desktop binary ({BINARY_VARIABLE}, tests/e2e/README.md)");
      return None;
    };
    match Self::start_with(name, binary).await {
      Ok(session) => Some(session),
      Err(error) if !is_required => {
        eprintln!("skipping: no desktop session: {error:#}");
        None
      },
      Err(error) => panic!("desktop session: {error:#}"),
    }
  }

  async fn start_with(name: &str, binary: PathBuf) -> anyhow::Result<Self> {
    let dir = std::env::temp_dir().join(format!("teamcraft-e2e-desktop-{name}-{}", std::process::id()));
    drop(std::fs::remove_dir_all(&dir));
    for sub in ["home", "config", "data", "cache", "state", "run", "bin"] {
      std::fs::create_dir_all(dir.join(sub))?;
    }
    set_private(&dir.join("run"))?;
    write_fake_browser(&dir)?;
    let bus_path = dir.join("run").join("bus");
    let bus_address = format!("unix:path={}", bus_path.display());
    let bus = Command::new("dbus-daemon")
      .args(["--session", "--nofork", "--nopidfile"])
      .arg(format!("--address={bus_address}"))
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .kill_on_drop(true)
      .spawn()
      .context("starting dbus-daemon")?;
    wait_for_path(&bus_path, Duration::from_secs(10)).await?;
    let mut keyring = Command::new("gnome-keyring-daemon")
      .args(["--foreground", "--unlock", "--components=secrets"])
      .envs(session_env(&dir, &bus_address))
      .stdin(Stdio::piped())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .kill_on_drop(true)
      .spawn()
      .context("starting gnome-keyring-daemon")?;
    if let Some(mut input) = keyring.stdin.take() {
      tokio::io::AsyncWriteExt::write_all(&mut input, KEYRING_PASSWORD.as_bytes()).await?;
    }
    let tray_watcher = zbus::connection::Builder::address(bus_address.as_str())?
      .name("org.kde.StatusNotifierWatcher")?
      .serve_at("/StatusNotifierWatcher", TrayWatcher)?
      .build()
      .await
      .context("the tray watcher")?;
    wait_for_secret_service(&tray_watcher).await?;
    // Closing the window keeps the app in the tray (off by default on Linux).
    let config = dir.join("config").join(APP_IDENTIFIER);
    std::fs::create_dir_all(&config)?;
    std::fs::write(config.join("shell.json"), r#"{"is_close_to_tray":true}"#)?;
    Ok(Self {
      dir,
      bus_address,
      _daemons: vec![bus, keyring],
      _tray_watcher: tray_watcher,
      binary,
    })
  }

  /// Starts the app against `server_url`.
  pub async fn launch(&self, server_url: &str) -> anyhow::Result<DesktopApp> {
    let inspector_port = free_tcp_port();
    let mut envs: Vec<(String, String)> = session_env(&self.dir, &self.bus_address);
    envs.extend(
      [
        ("TEAMCRAFT_SERVER_URL", server_url),
        ("TEAMCRAFT_DISABLE_UPDATES", "1"),
        (
          "WEBKIT_INSPECTOR_HTTP_SERVER",
          &format!("127.0.0.1:{inspector_port}"),
        ),
        // X11 through `DISPLAY` (a virtual one in CI), never the user's own Wayland session.
        ("GDK_BACKEND", "x11"),
        ("WAYLAND_DISPLAY", ""),
        ("XDG_CURRENT_DESKTOP", ""),
        ("XDG_SESSION_TYPE", "x11"),
        ("LANG", "en_US.UTF-8"),
        ("LANGUAGE", "en"),
        // No accessibility bus in the session.
        ("NO_AT_BRIDGE", "1"),
        ("NO_COLOR", "1"),
        ("RUST_LOG", "info"),
      ]
      .map(|(key, value)| (key.to_owned(), value.to_owned())),
    );
    let path = std::env::var("PATH").unwrap_or_default();
    envs.push((
      "PATH".to_owned(),
      format!("{}:{path}", self.dir.join("bin").display()),
    ));
    let service = Service::spawn("teamcraft-desktop", &self.binary, &[], &envs, &self.dir)?;
    Ok(DesktopApp {
      service,
      inspector_port,
      maybe_inspector: None,
    })
  }

  /// Waits for the sign-in the app opened in the "system browser" and signs in there as `user`
  /// (the dev realm's `<user>-dev-pass`); Keycloak then redirects to the app's loopback listener.
  pub async fn sign_in_at_keycloak(&self, user: &str) -> anyhow::Result<()> {
    let opened = self.dir.join("opened-urls");
    let deadline = Instant::now() + WAIT;
    let url = loop {
      let text = std::fs::read_to_string(&opened).unwrap_or_default();
      if let Some(url) = text
        .lines()
        .rev()
        .find(|line| line.contains("/protocol/openid-connect/auth"))
      {
        break url.to_owned();
      }
      if Instant::now() > deadline {
        bail!("the app opened no sign-in page; opened: {text:?}");
      }
      tokio::time::sleep(Duration::from_millis(200)).await;
    };
    std::fs::write(&opened, "")?;
    fill_in_keycloak_form(&url, user).await
  }
}

/// The directories stay after a failure, for the app's log (CI uploads them).
impl Drop for DesktopSession {
  fn drop(&mut self) {
    if !std::thread::panicking() {
      drop(std::fs::remove_dir_all(&self.dir));
    }
  }
}

impl DesktopApp {
  pub fn pid(&self) -> u32 {
    self.service.pid().unwrap_or_default()
  }

  pub fn log(&self) -> String {
    self.service.plain_log()
  }

  /// The value of a JavaScript expression in the app's page (JSON).
  pub async fn evaluate(&mut self, expression: &str) -> anyhow::Result<Value> {
    // A reconnect covers a page the app made anew (webview_recovery.rs).
    for attempt in 0..2 {
      if self.maybe_inspector.is_none() {
        self.maybe_inspector = Some(Inspector::connect(self.inspector_port).await?);
      }
      let Some(inspector) = self.maybe_inspector.as_mut() else {
        continue;
      };
      match inspector.evaluate(expression).await {
        Ok(value) => return Ok(value),
        Err(error) if attempt == 0 => {
          eprintln!("inspector: {error:#}; connecting again");
          self.maybe_inspector = None;
        },
        Err(error) => return Err(error),
      }
    }
    bail!("the inspector did not answer")
  }

  /// Like [`Self::evaluate`], for an expression whose value is a promise that settles within
  /// `timeout`.
  pub async fn evaluate_async(&mut self, expression: &str, timeout: Duration) -> anyhow::Result<Value> {
    let started = format!(
      "window.__e2eResult = undefined; Promise.resolve({expression}).then(\
        value => window.__e2eResult = JSON.stringify({{ ok: value === undefined ? null : value }}),\
        error => window.__e2eResult = JSON.stringify({{ error: String(error && error.message || error) }})); 1"
    );
    self.evaluate(&started).await?;
    let deadline = Instant::now() + timeout;
    loop {
      if let Value::String(text) = self.evaluate("window.__e2eResult").await? {
        let result: Value = serde_json::from_str(&text)?;
        if let Some(error) = result.get("error") {
          bail!("{expression}: {error}");
        }
        return Ok(result["ok"].clone());
      }
      if Instant::now() > deadline {
        bail!("{expression} did not settle within {timeout:?}");
      }
      tokio::time::sleep(Duration::from_millis(100)).await;
    }
  }

  /// Calls a command of the shell the way the page does.
  pub async fn invoke(&mut self, command: &str, arguments: Value) -> anyhow::Result<Value> {
    self
      .evaluate_async(
        &format!(
          "window.__TAURI_INTERNALS__.invoke({}, {arguments})",
          json!(command)
        ),
        WAIT,
      )
      .await
  }

  pub async fn text(&mut self) -> String {
    self
      .evaluate("document.body ? document.body.innerText : ''")
      .await
      .ok()
      .and_then(|value| value.as_str().map(str::to_owned))
      .unwrap_or_default()
  }

  /// Waits until the page shows `needle`.
  pub async fn wait_for_text(&mut self, needle: &str, timeout: Duration) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
      let text = self.text().await;
      if text.contains(needle) {
        return Ok(());
      }
      if Instant::now() > deadline {
        bail!(
          "{needle:?} did not show within {timeout:?}; the page shows {:?}\nlog:\n{}",
          tail(&text, 600),
          tail(&self.log(), 3000)
        );
      }
      tokio::time::sleep(Duration::from_millis(250)).await;
    }
  }

  /// Clicks the first visible button or link whose text or label contains `label`.
  pub async fn click(&mut self, label: &str) -> anyhow::Result<()> {
    let script = format!(
      "(label => {{
        const target = [...document.querySelectorAll('button,a,[role=button],[role=menuitem],[role=tab]')]
          .filter(element => element.offsetParent)
          .find(element => (element.innerText || element.getAttribute('aria-label') || '').trim().includes(label));
        if (!target) return false;
        target.click();
        return true;
      }})({})",
      json!(label)
    );
    let deadline = Instant::now() + WAIT;
    while self.evaluate(&script).await? != Value::Bool(true) {
      if Instant::now() > deadline {
        bail!(
          "no button {label:?}; the page shows {:?}",
          tail(&self.text().await, 600)
        );
      }
      tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Ok(())
  }

  /// Types into the input `selector` the way a user does (an `input` event per change).
  pub async fn fill(&mut self, selector: &str, value: &str) -> anyhow::Result<()> {
    let script = format!(
      "((selector, value) => {{
        const input = document.querySelector(selector);
        if (!input) return false;
        const prototype = input.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
        Object.getOwnPropertyDescriptor(prototype, 'value').set.call(input, value);
        input.dispatchEvent(new Event('input', {{ bubbles: true }}));
        input.dispatchEvent(new Event('change', {{ bubbles: true }}));
        return input.value === value;
      }})({}, {})",
      json!(selector),
      json!(value)
    );
    let deadline = Instant::now() + WAIT;
    while self.evaluate(&script).await? != Value::Bool(true) {
      if Instant::now() > deadline {
        bail!(
          "no input {selector:?}; the page shows {:?}",
          tail(&self.text().await, 600)
        );
      }
      tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Ok(())
  }

  /// Closes the inspector connection: an attached inspector instruments the page, which costs CPU
  /// of its own, so nothing is measured while one is attached. The next call connects again.
  pub fn detach_inspector(&mut self) {
    self.maybe_inspector = None;
  }

  /// Ends the app as a logout or shutdown does (SIGTERM).
  pub async fn quit(mut self) -> anyhow::Result<()> {
    self.maybe_inspector = None;
    self.service.terminate(Duration::from_secs(20)).await?;
    Ok(())
  }
}

impl Inspector {
  /// The inspector's start page lists one socket per page target; the app has one page.
  async fn connect(port: u16) -> anyhow::Result<Self> {
    let base = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + WAIT;
    let path = loop {
      let page = match reqwest::get(format!("http://{base}/")).await {
        Ok(response) => response.text().await.unwrap_or_default(),
        Err(_) => String::new(),
      };
      if let Some(path) = page_socket(&page) {
        break path;
      }
      if Instant::now() > deadline {
        bail!("the inspector on {base} lists no page");
      }
      tokio::time::sleep(Duration::from_millis(250)).await;
    };
    let mut request = format!("ws://{base}{path}").into_client_request()?;
    request
      .headers_mut()
      .insert("Origin", format!("http://{base}").parse()?);
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await?;
    // The page's inspector is a target of its own, announced right after the connection.
    let page_target = loop {
      let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await
        .context("no target announced")?
        .context("inspector closed")??;
      let Message::Text(text) = message else {
        continue;
      };
      let value: Value = serde_json::from_str(&text)?;
      if value["method"] == "Target.targetCreated"
        && let Some(id) = value["params"]["targetInfo"]["targetId"].as_str()
      {
        break id.to_owned();
      }
    };
    Ok(Self {
      socket,
      page_target,
      next_id: 0,
    })
  }

  async fn evaluate(&mut self, expression: &str) -> anyhow::Result<Value> {
    let result = self
      .call(
        "Runtime.evaluate",
        json!({ "expression": expression, "returnByValue": true }),
      )
      .await?;
    if result["wasThrown"] == Value::Bool(true) {
      bail!("{expression}: {}", result["result"]["description"]);
    }
    Ok(result["result"]["value"].clone())
  }

  async fn call(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
    self.next_id += 1;
    let id = self.next_id;
    let inner = json!({ "id": id, "method": method, "params": params }).to_string();
    let wrapper = json!({
      "id": WRAPPER_ID_OFFSET + id,
      "method": "Target.sendMessageToTarget",
      "params": { "targetId": self.page_target, "message": inner },
    });
    self.socket.send(Message::text(wrapper.to_string())).await?;
    let deadline = Instant::now() + WAIT;
    loop {
      let left = deadline.saturating_duration_since(Instant::now());
      let message = tokio::time::timeout(left, self.socket.next())
        .await
        .with_context(|| format!("{method} got no answer"))?
        .context("inspector closed")??;
      let Message::Text(text) = message else {
        continue;
      };
      let value: Value = serde_json::from_str(&text)?;
      if value["method"] != "Target.dispatchMessageFromTarget" {
        continue;
      }
      let Some(inner) = value["params"]["message"].as_str() else {
        continue;
      };
      let answer: Value = serde_json::from_str(inner)?;
      if answer["id"] != json!(id) {
        continue;
      }
      if let Some(error) = answer.get("error") {
        bail!("{method}: {error}");
      }
      return Ok(answer["result"].clone());
    }
  }
}

/// `/socket/<connection>/<target>/WebPage` from the inspector's start page.
fn page_socket(page: &str) -> Option<String> {
  let start = page.find("/socket/")?;
  let rest = &page[start..];
  let end = rest.find("/WebPage")? + "/WebPage".len();
  Some(rest[..end].to_owned())
}

/// Logs into Keycloak's form like a browser: its cookies, the form, and the redirect to the app.
async fn fill_in_keycloak_form(url: &str, user: &str) -> anyhow::Result<()> {
  let client = reqwest::Client::builder()
    .redirect(reqwest::redirect::Policy::none())
    .build()?;
  let page = client.get(url).send().await?;
  let cookies: Vec<String> = page
    .headers()
    .get_all("set-cookie")
    .iter()
    .filter_map(|value| value.to_str().ok()?.split(';').next().map(str::to_owned))
    .collect();
  let text = page.text().await?;
  let action = text
    .split("action=\"")
    .nth(1)
    .and_then(|rest| rest.split('"').next())
    .context("no login form")?
    .replace("&amp;", "&");
  let password = format!("{user}-dev-pass");
  let form = [
    ("username", user),
    ("password", password.as_str()),
    ("credentialId", ""),
  ];
  let answer = client
    .post(action)
    .header("cookie", cookies.join("; "))
    .form(&form)
    .send()
    .await?;
  let location = answer
    .headers()
    .get("location")
    .and_then(|value| value.to_str().ok())
    .context("Keycloak did not redirect after the login")?
    .to_owned();
  client.get(location).send().await?;
  Ok(())
}

/// The environment of everything in the session: its own home, XDG directories and bus.
fn session_env(dir: &Path, bus_address: &str) -> Vec<(String, String)> {
  let path = |sub: &str| dir.join(sub).display().to_string();
  vec![
    ("HOME".to_owned(), path("home")),
    ("XDG_CONFIG_HOME".to_owned(), path("config")),
    ("XDG_DATA_HOME".to_owned(), path("data")),
    ("XDG_CACHE_HOME".to_owned(), path("cache")),
    ("XDG_STATE_HOME".to_owned(), path("state")),
    ("XDG_RUNTIME_DIR".to_owned(), path("run")),
    ("DBUS_SESSION_BUS_ADDRESS".to_owned(), bus_address.to_owned()),
  ]
}

/// `xdg-open` that only writes the address down: the "system browser" of the sign-in.
fn write_fake_browser(dir: &Path) -> anyhow::Result<()> {
  let script = dir.join("bin").join("xdg-open");
  let opened = dir.join("opened-urls");
  std::fs::write(
    &script,
    format!("#!/bin/sh\nprintf '%s\\n' \"$1\" >> '{}'\n", opened.display()),
  )?;
  set_mode(&script, 0o755)
}

fn set_private(dir: &Path) -> anyhow::Result<()> {
  set_mode(dir, 0o700)
}

fn set_mode(path: &Path, mode: u32) -> anyhow::Result<()> {
  use std::os::unix::fs::PermissionsExt;
  std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
  Ok(())
}

async fn wait_for_path(path: &Path, timeout: Duration) -> anyhow::Result<()> {
  let deadline = Instant::now() + timeout;
  while !path.exists() {
    if Instant::now() > deadline {
      bail!("{} did not appear", path.display());
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
  Ok(())
}

/// The keyring takes the Secret Service name once it has unlocked the login keyring.
async fn wait_for_secret_service(connection: &zbus::Connection) -> anyhow::Result<()> {
  let bus = zbus::fdo::DBusProxy::new(connection).await?;
  let name = zbus::names::BusName::try_from("org.freedesktop.secrets")?;
  let deadline = Instant::now() + Duration::from_secs(10);
  while !bus.name_has_owner(name.clone()).await? {
    if Instant::now() > deadline {
      bail!("gnome-keyring-daemon did not take org.freedesktop.secrets");
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
  Ok(())
}

fn tail(text: &str, bytes: usize) -> &str {
  let mut start = text.len().saturating_sub(bytes);
  while !text.is_char_boundary(start) {
    start += 1;
  }
  &text[start..]
}
