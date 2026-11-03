//! Bad networks for one TCP port, made by the kernel: packet loss, delay, jitter, a narrow link
//! (`tc netem`), a link that swallows everything or answers with resets, and UDP that doesn't get
//! through (`nft`). Only traffic to and from the port is touched, so a client reaches the server
//! through a [`crate::relay::TcpRelay`] whose port is shaped while the other peers and the dev
//! stack talk normally.
//!
//! Changing the kernel's network needs a network namespace of its own: `scripts/e2e-network.sh`
//! runs the suite inside one (rootless, `pasta` forwards the dev stack's ports) and sets
//! [`NETNS_ENV`]. Outside of it [`PortConditions::take`] answers `None` and the tests skip.

use std::process::Command;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

/// Set by `scripts/e2e-network.sh` inside the namespace it runs the suite in.
pub const NETNS_ENV: &str = "E2E_NETNS";
/// The nft table the rules live in.
const TABLE: &str = "e2e_network";
/// The `tc` class the shaped port's packets go to; everything else takes band 1, untouched.
const SHAPED_CLASS: &str = "1:4";
/// One user of the namespace's loopback at a time: the shaping is set on the device as a whole.
static LOOPBACK: Mutex<()> = Mutex::new(());

/// The conditions one port sees; back to normal when dropped.
pub struct PortConditions {
  port: u16,
  _loopback: MutexGuard<'static, ()>,
}

/// What the link between the client and the server does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Condition {
  /// As the machine's loopback is: no loss, no delay.
  Clear,
  /// Packets in both directions disappear without a trace: a laptop that slept, a dead Wi-Fi, a
  /// NAT entry that expired. Open connections only find out by their own timeouts.
  Blackhole,
  /// Every packet to the server is answered with a TCP reset: connections break at once and new
  /// ones are refused (a firewall that rejects, a server port that closed).
  Reset,
  /// UDP to anywhere is dropped (a network that only lets TCP out); TCP is as it was.
  NoUdp,
  /// A poor link: each direction delays by `delay` ± `jitter` and loses `loss_percent` of the
  /// packets (correlated, so losses come in bursts), at most `maybe_rate_kbit` kbit/s.
  Lossy {
    delay: Duration,
    jitter: Duration,
    loss_percent: f32,
    maybe_rate_kbit: Option<u32>,
  },
}

impl PortConditions {
  /// Takes over shaping for `port`; `None` outside of the network namespace.
  pub fn take(port: u16) -> Option<Self> {
    if std::env::var_os(NETNS_ENV).is_none() {
      eprintln!("network conditions: skipped, run through scripts/e2e-network.sh");
      return None;
    }
    let loopback = LOOPBACK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let conditions = Self {
      port,
      _loopback: loopback,
    };
    conditions.install();
    Some(conditions)
  }

  /// Changes what the link does from now on; connections that are open stay open (unless the
  /// condition breaks them).
  pub fn set(&self, condition: Condition) {
    run("nft", &["flush", "table", "inet", TABLE]);
    shape(Duration::ZERO, Duration::ZERO, 0.0, None);
    let port = self.port.to_string();
    match condition {
      Condition::Clear => {},
      Condition::Blackhole => {
        nft_rule(&format!("tcp dport {port} drop"));
        nft_rule(&format!("tcp sport {port} drop"));
      },
      Condition::Reset => nft_rule(&format!("tcp dport {port} reject with tcp reset")),
      Condition::NoUdp => nft_rule("meta l4proto udp drop"),
      Condition::Lossy {
        delay,
        jitter,
        loss_percent,
        maybe_rate_kbit,
      } => shape(delay, jitter, loss_percent, maybe_rate_kbit),
    }
  }

  fn install(&self) {
    let port = self.port.to_string();
    run("tc", &["qdisc", "del", "dev", "lo", "root"]);
    // Four bands, every priority to the first: only the filters below send packets to the fourth.
    let mut prio = vec![
      "qdisc", "add", "dev", "lo", "root", "handle", "1:", "prio", "bands", "4", "priomap",
    ];
    prio.extend(["0"; 16]);
    must("tc", &prio);
    must(
      "tc",
      &[
        "qdisc",
        "add",
        "dev",
        "lo",
        "parent",
        SHAPED_CLASS,
        "handle",
        "40:",
        "netem",
        "delay",
        "0ms",
      ],
    );
    for direction in ["dport", "sport"] {
      must(
        "tc",
        &[
          "filter",
          "add",
          "dev",
          "lo",
          "parent",
          "1:0",
          "protocol",
          "ip",
          "prio",
          "1",
          "u32",
          "match",
          "ip",
          direction,
          &port,
          "0xffff",
          "flowid",
          SHAPED_CLASS,
        ],
      );
    }
    run("nft", &["delete", "table", "inet", TABLE]);
    must("nft", &["add", "table", "inet", TABLE]);
    must(
      "nft",
      &[
        "add", "chain", "inet", TABLE, "output", "{", "type", "filter", "hook", "output", "priority", "0",
        ";", "}",
      ],
    );
  }
}

impl Drop for PortConditions {
  fn drop(&mut self) {
    run("nft", &["delete", "table", "inet", TABLE]);
    run("tc", &["qdisc", "del", "dev", "lo", "root"]);
  }
}

fn shape(delay: Duration, jitter: Duration, loss_percent: f32, maybe_rate_kbit: Option<u32>) {
  let delay = format!("{}ms", delay.as_millis());
  let jitter = format!("{}ms", jitter.as_millis());
  let loss = format!("{loss_percent}%");
  let mut args = vec![
    "qdisc",
    "change",
    "dev",
    "lo",
    "parent",
    SHAPED_CLASS,
    "handle",
    "40:",
    "netem",
    "delay",
    &delay,
    &jitter,
    "25%",
    "loss",
    &loss,
    "25%",
  ];
  let rate;
  if let Some(kbit) = maybe_rate_kbit {
    rate = format!("{kbit}kbit");
    args.extend(["rate", &rate]);
  }
  must("tc", &args);
}

fn nft_rule(rule: &str) {
  let mut args = vec!["add", "rule", "inet", TABLE, "output"];
  args.extend(rule.split(' '));
  must("nft", &args);
}

/// Runs a command that must succeed.
fn must(program: &str, args: &[&str]) {
  let output = Command::new(program)
    .args(args)
    .output()
    .unwrap_or_else(|error| panic!("{program}: {error}"));
  assert!(
    output.status.success(),
    "{program} {}: {}",
    args.join(" "),
    String::from_utf8_lossy(&output.stderr)
  );
}

/// Runs a command whose failure means there was nothing to undo.
fn run(program: &str, args: &[&str]) {
  // Deliberately ignored: removing what isn't there fails, which is fine here.
  drop(Command::new(program).args(args).output());
}
