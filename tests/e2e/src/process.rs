//! Child processes of the stack: logs go to a file, the process dies with the test.

use std::collections::HashSet;
use std::net::{TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use tokio::process::{Child, Command};

/// A running binary of the stack. Killed on drop.
pub struct Service {
  pub name: String,
  log_path: PathBuf,
  child: Child,
}

impl Service {
  pub fn spawn(
    name: &str,
    binary: &Path,
    args: &[String],
    envs: &[(String, String)],
    dir: &Path,
  ) -> anyhow::Result<Self> {
    let log_path = dir.join(format!("{name}.log"));
    let log = std::fs::File::create(&log_path).with_context(|| format!("creating {}", log_path.display()))?;
    let mut command = Command::new(binary);
    // The server reads every TEAMCRAFT_* variable as configuration: only what the stack sets counts.
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("TEAMCRAFT_")) {
      command.env_remove(key);
    }
    let child = command
      .args(args)
      .envs(envs.iter().map(|(key, value)| (key, value)))
      .stdin(Stdio::null())
      .stdout(log.try_clone()?)
      .stderr(log)
      .kill_on_drop(true)
      .spawn()
      .with_context(|| {
        format!(
          "starting {} (build it first, see tests/e2e/README.md)",
          binary.display()
        )
      })?;
    Ok(Self {
      name: name.to_owned(),
      log_path,
      child,
    })
  }

  pub fn log(&self) -> String {
    std::fs::read_to_string(&self.log_path).unwrap_or_default()
  }

  pub fn log_path(&self) -> &Path {
    &self.log_path
  }

  /// The log with ANSI colors removed (`tracing` colors even into files unless told not to).
  pub fn plain_log(&self) -> String {
    strip_ansi(&self.log())
  }

  /// Waits until the log contains `needle`; fails early if the process exited.
  pub async fn wait_for_log(&mut self, needle: &str, timeout: Duration) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
      if self.plain_log().contains(needle) {
        return Ok(());
      }
      if let Some(status) = self.child.try_wait()? {
        bail!(
          "{} exited with {status} before logging {needle:?}:\n{}",
          self.name,
          self.tail(40)
        );
      }
      if Instant::now() > deadline {
        bail!(
          "{} did not log {needle:?} in {timeout:?}:\n{}",
          self.name,
          self.tail(40)
        );
      }
      tokio::time::sleep(Duration::from_millis(100)).await;
    }
  }

  /// Waits until `url` answers 200.
  pub async fn wait_for_http(&mut self, url: &str, timeout: Duration) -> anyhow::Result<()> {
    let http = reqwest::Client::new();
    let deadline = Instant::now() + timeout;
    loop {
      if let Ok(response) = http.get(url).send().await
        && response.status().is_success()
      {
        return Ok(());
      }
      if let Some(status) = self.child.try_wait()? {
        bail!(
          "{} exited with {status} before {url} answered:\n{}",
          self.name,
          self.tail(40)
        );
      }
      if Instant::now() > deadline {
        bail!("{url} did not answer in {timeout:?}:\n{}", self.tail(40));
      }
      tokio::time::sleep(Duration::from_millis(100)).await;
    }
  }

  pub fn tail(&self, lines: usize) -> String {
    let log = self.plain_log();
    let all: Vec<&str> = log.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
  }

  pub fn is_running(&mut self) -> bool {
    matches!(self.child.try_wait(), Ok(None))
  }
}

/// A TCP port that was free a moment ago (dual stack: free on IPv4 and IPv6). Never hands out a
/// port twice within the test run: with the same port, the second server would fail to bind while
/// its health check was answered by the first one, and the test would talk to the wrong database.
pub fn free_tcp_port() -> u16 {
  unused_port(|| TcpListener::bind("[::]:0").map(|socket| socket.local_addr().map(|addr| addr.port())))
}

/// A UDP port that was free a moment ago (see [`free_tcp_port`]).
pub fn free_udp_port() -> u16 {
  unused_port(|| UdpSocket::bind("[::]:0").map(|socket| socket.local_addr().map(|addr| addr.port())))
}

fn unused_port(probe: impl Fn() -> std::io::Result<std::io::Result<u16>>) -> u16 {
  static HANDED_OUT: LazyLock<Mutex<HashSet<u16>>> = LazyLock::new(Mutex::default);
  loop {
    let port = probe().and_then(|port| port).expect("probing for a free port");
    if HANDED_OUT.lock().expect("port registry").insert(port) {
      return port;
    }
  }
}

/// Directory with the compiled binaries: the one that holds the running test executable's `deps`.
pub fn binary_dir() -> PathBuf {
  if let Ok(dir) = std::env::var("E2E_BIN_DIR") {
    return PathBuf::from(dir);
  }
  let exe = std::env::current_exe().expect("test executable path");
  // target/<profile>/deps/<test> -> target/<profile>
  exe
    .parent()
    .and_then(Path::parent)
    .expect("target directory")
    .to_path_buf()
}

pub fn binary(name: &str) -> PathBuf {
  binary_dir().join(name)
}

fn strip_ansi(text: &str) -> String {
  let mut out = String::with_capacity(text.len());
  let mut chars = text.chars().peekable();
  while let Some(c) = chars.next() {
    if c == '\u{1b}' && chars.peek() == Some(&'[') {
      for next in chars.by_ref() {
        if next.is_ascii_alphabetic() {
          break;
        }
      }
    } else {
      out.push(c);
    }
  }
  out
}
