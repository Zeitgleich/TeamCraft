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

  /// The operating system's process ID; `None` once the process was reaped.
  pub fn pid(&self) -> Option<u32> {
    self.child.id()
  }

  /// Sends SIGTERM (what systemd and `docker stop` send) and waits up to `timeout` for the exit.
  /// Returns how long the process took, or an error if it was still running at the deadline.
  pub async fn terminate(&mut self, timeout: Duration) -> anyhow::Result<Duration> {
    let pid = self.pid().context("the process already exited")?;
    let started = Instant::now();
    // `kill` instead of a signal crate: the workspace forbids unsafe code and this is test-only.
    let status = std::process::Command::new("kill")
      .args(["-TERM", &pid.to_string()])
      .status()
      .context("running kill")?;
    if !status.success() {
      bail!("kill -TERM {pid} failed: {status}");
    }
    match tokio::time::timeout(timeout, self.child.wait()).await {
      Ok(result) => {
        result?;
        Ok(started.elapsed())
      },
      Err(_) => bail!(
        "{} still runs {timeout:?} after SIGTERM:\n{}",
        self.name,
        self.tail(20)
      ),
    }
  }

  /// Waits up to `timeout` for the process to exit on its own; `None` while it still runs.
  pub async fn wait_exit(&mut self, timeout: Duration) -> Option<std::process::ExitStatus> {
    tokio::time::timeout(timeout, self.child.wait())
      .await
      .ok()
      .and_then(Result::ok)
  }

  /// Resident memory and CPU time used so far, from `/proc` (Linux only; `None` elsewhere or after
  /// the exit).
  pub fn stats(&self) -> Option<ProcessStats> {
    ProcessStats::read(self.pid()?)
  }
}

/// Whether a TCP connection from `client_port` to the local `server_port` is still established, as
/// the kernel sees it (`/proc/net/tcp` and `tcp6`; Linux only, `false` elsewhere). Tells a
/// connection the server still holds from one it closed, whatever the client has read.
pub fn is_tcp_established(server_port: u16, client_port: u16) -> bool {
  const ESTABLISHED: &str = "01";
  let port_of = |address: &str| {
    address
      .rsplit_once(':')
      .and_then(|(_, port)| u16::from_str_radix(port, 16).ok())
  };
  ["/proc/net/tcp", "/proc/net/tcp6"]
    .iter()
    .filter_map(|path| std::fs::read_to_string(path).ok())
    .flat_map(|table| table.lines().skip(1).map(str::to_owned).collect::<Vec<_>>())
    .any(|line| {
      let fields: Vec<&str> = line.split_whitespace().collect();
      fields.len() > 3
        && port_of(fields[1]) == Some(server_port)
        && port_of(fields[2]) == Some(client_port)
        && fields[3] == ESTABLISHED
    })
}

/// A snapshot of a process' resource use.
#[derive(Debug, Clone, Copy)]
pub struct ProcessStats {
  pub rss_bytes: u64,
  /// Highest resident memory since the start (`VmHWM`).
  pub peak_rss_bytes: u64,
  /// User plus system CPU time.
  pub cpu_time: Duration,
  pub threads: u64,
  pub taken_at: Instant,
}

impl ProcessStats {
  pub fn read(pid: u32) -> Option<Self> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let kib = |key: &str| -> Option<u64> {
      status
        .lines()
        .find_map(|line| line.strip_prefix(key))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
    };
    // Fields after the command name (which may contain spaces) start after the last ')'.
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    // utime and stime are fields 14 and 15 of the full line, i.e. 11 and 12 after the name.
    let ticks: u64 = fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?;
    Some(Self {
      rss_bytes: kib("VmRSS:")? * 1024,
      peak_rss_bytes: kib("VmHWM:")? * 1024,
      // USER_HZ is 100 on every Linux the tests run on.
      cpu_time: Duration::from_millis(ticks * 10),
      threads: kib("Threads:")?,
      taken_at: Instant::now(),
    })
  }

  /// Average CPU use between `earlier` and this snapshot, in percent of one core.
  pub fn cpu_percent_since(&self, earlier: &ProcessStats) -> f64 {
    let wall = self.taken_at.duration_since(earlier.taken_at).as_secs_f64();
    if wall <= 0.0 {
      return 0.0;
    }
    self.cpu_time.saturating_sub(earlier.cpu_time).as_secs_f64() / wall * 100.0
  }

  pub fn rss_mib(&self) -> f64 {
    self.rss_bytes as f64 / (1024.0 * 1024.0)
  }
}

/// A TCP port that was free a moment ago (dual stack: free on IPv4 and IPv6). Never hands out a
/// port twice within the test run: with the same port, the second server would fail to bind while
/// its health check was answered by the first one, and the test would talk to the wrong database.
pub fn free_tcp_port() -> u16 {
  let any = format!("{}:0", any_address());
  unused_port(|| TcpListener::bind(&any).map(|socket| socket.local_addr().map(|addr| addr.port())))
}

/// A UDP port that was free a moment ago (see [`free_tcp_port`]).
pub fn free_udp_port() -> u16 {
  let any = format!("{}:0", any_address());
  unused_port(|| UdpSocket::bind(&any).map(|socket| socket.local_addr().map(|addr| addr.port())))
}

/// The address the processes under test listen on: `[::]` (dual stack), or IPv4 loopback on hosts
/// whose kernel has no IPv6 at all (some CI sandboxes), where `[::]` cannot be bound.
pub fn any_address() -> &'static str {
  static HAS_IPV6: LazyLock<bool> = LazyLock::new(|| TcpListener::bind("[::1]:0").is_ok());
  if *HAS_IPV6 { "[::]" } else { "127.0.0.1" }
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
