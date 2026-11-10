//! Memory and CPU of local processes from Linux `/proc`, for the performance benchmarks (perf.rs).
//! Elsewhere every reading is zero.

use std::path::Path;
use std::time::Duration;

/// Linux reports CPU time in clock ticks of `USER_HZ`, which is 100 on every platform Linux runs on.
const TICKS_PER_SECOND: u64 = 100;

/// Memory and CPU time of a group of processes (a browser's renderer, GPU and utility processes).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessUsage {
  /// Proportional set size: shared pages count once across the group, unlike RSS.
  pub pss_bytes: u64,
  /// The anonymous part of it (heaps, JS and wasm memory), without mapped files such as libraries.
  pub pss_anon_bytes: u64,
  /// User plus system CPU time since each process started.
  pub cpu_time: Duration,
  /// Context switches of all threads: each one is the process waking up (or being preempted), a
  /// proxy for wakeups that keep a laptop's CPU out of its deep idle states.
  pub context_switches: u64,
  pub process_count: usize,
}

impl ProcessUsage {
  /// Every process whose command line contains `needle` (e.g. a unique browser profile directory).
  pub fn of_matching(needle: &str) -> Self {
    let Ok(entries) = std::fs::read_dir("/proc") else {
      return Self::default();
    };
    entries
      .filter_map(Result::ok)
      .filter(|entry| {
        entry
          .file_name()
          .to_string_lossy()
          .chars()
          .all(|c| c.is_ascii_digit())
      })
      .filter(|entry| command_line(&entry.path()).contains(needle))
      .map(|entry| Self::of_process(&entry.path()))
      .fold(Self::default(), Self::plus)
  }

  /// A process and everything it started (the desktop app's WebKit web and network processes), per
  /// process name (`comm`, e.g. `teamcraft-deskt`, `WebKitWebProces`).
  pub fn of_tree(pid: u32) -> std::collections::BTreeMap<String, Self> {
    let mut kinds = std::collections::BTreeMap::<String, Self>::new();
    let mut parents = std::collections::BTreeMap::<u32, u32>::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
      for entry in entries.filter_map(Result::ok) {
        let Some(child) = entry
          .file_name()
          .to_str()
          .and_then(|name| name.parse::<u32>().ok())
        else {
          continue;
        };
        if let Some(parent) = parent_pid(&entry.path()) {
          parents.insert(child, parent);
        }
      }
    }
    let is_in_tree = |mut candidate: u32| loop {
      if candidate == pid {
        return true;
      }
      match parents.get(&candidate) {
        Some(&parent) if parent > 1 => candidate = parent,
        _ => return false,
      }
    };
    for &member in parents.keys().filter(|&&candidate| is_in_tree(candidate)) {
      let dir = Path::new("/proc").join(member.to_string());
      let name = std::fs::read_to_string(dir.join("comm"))
        .map(|name| name.trim().to_owned())
        .unwrap_or_default();
      let sum = kinds.entry(name).or_default();
      *sum = sum.plus(Self::of_process(&dir));
    }
    kinds
  }

  /// The sum over a group, e.g. of [`Self::of_tree`].
  pub fn total<'a>(usages: impl IntoIterator<Item = &'a Self>) -> Self {
    usages.into_iter().copied().fold(Self::default(), Self::plus)
  }

  fn plus(self, other: Self) -> Self {
    Self {
      pss_bytes: self.pss_bytes + other.pss_bytes,
      pss_anon_bytes: self.pss_anon_bytes + other.pss_anon_bytes,
      cpu_time: self.cpu_time + other.cpu_time,
      context_switches: self.context_switches + other.context_switches,
      process_count: self.process_count + other.process_count,
    }
  }

  /// Like [`Self::of_matching`], per kind of process: Chrome's `--type` (renderer, gpu-process,
  /// utility) with its `--utility-sub-type` (e.g. the storage service that holds IndexedDB); the
  /// browser process has neither.
  pub fn by_kind(needle: &str) -> std::collections::BTreeMap<String, Self> {
    let mut kinds = std::collections::BTreeMap::<String, Self>::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
      return kinds;
    };
    for entry in entries.filter_map(Result::ok) {
      let line = command_line(&entry.path());
      if !line.contains(needle) {
        continue;
      }
      let flag = |name: &str| {
        line
          .split_whitespace()
          .find_map(|part| part.strip_prefix(name))
          .map(str::to_owned)
      };
      let kind = match (flag("--type="), flag("--utility-sub-type=")) {
        (Some(kind), Some(sub)) => format!("{kind}:{sub}"),
        (Some(kind), None) => kind,
        (None, _) => "browser".to_owned(),
      };
      let usage = Self::of_process(&entry.path());
      let sum = kinds.entry(kind).or_default();
      *sum = sum.plus(usage);
    }
    kinds
  }

  fn of_process(dir: &Path) -> Self {
    Self {
      pss_bytes: rollup_bytes(dir, "Pss:"),
      pss_anon_bytes: rollup_bytes(dir, "Pss_Anon:"),
      cpu_time: cpu_time(dir),
      context_switches: context_switches(dir),
      process_count: 1,
    }
  }
}

fn command_line(dir: &Path) -> String {
  std::fs::read(dir.join("cmdline"))
    .map(|bytes| String::from_utf8_lossy(&bytes).replace('\0', " "))
    .unwrap_or_default()
}

/// The parent's PID, field 4 of `stat`.
fn parent_pid(dir: &Path) -> Option<u32> {
  let text = std::fs::read_to_string(dir.join("stat")).ok()?;
  let (_, rest) = text.rsplit_once(')')?;
  rest.split_whitespace().nth(1)?.parse().ok()
}

/// A field of `smaps_rollup` (`Pss:`, `Pss_Anon:`), given in kB.
fn rollup_bytes(dir: &Path, field: &str) -> u64 {
  let Ok(text) = std::fs::read_to_string(dir.join("smaps_rollup")) else {
    return 0;
  };
  text
    .lines()
    .find_map(|line| line.strip_prefix(field))
    .and_then(|rest| rest.split_whitespace().next())
    .and_then(|kb| kb.parse::<u64>().ok())
    .map_or(0, |kb| kb * 1024)
}

/// Fields 14 and 15 (utime, stime) of `stat`; the command name in field 2 may contain spaces, so
/// counting starts after its closing parenthesis.
fn cpu_time(dir: &Path) -> Duration {
  let Ok(text) = std::fs::read_to_string(dir.join("stat")) else {
    return Duration::ZERO;
  };
  let Some((_, rest)) = text.rsplit_once(')') else {
    return Duration::ZERO;
  };
  let fields: Vec<&str> = rest.split_whitespace().collect();
  let ticks = |index: usize| {
    fields
      .get(index)
      .and_then(|value| value.parse::<u64>().ok())
      .unwrap_or(0)
  };
  // `rest` starts at field 3 (state), so utime (14) and stime (15) are at 11 and 12.
  let total = ticks(11) + ticks(12);
  Duration::from_millis(total * 1000 / TICKS_PER_SECOND)
}

/// Voluntary and involuntary context switches of every thread (`task/*/status`).
fn context_switches(dir: &Path) -> u64 {
  let Ok(tasks) = std::fs::read_dir(dir.join("task")) else {
    return 0;
  };
  tasks
    .filter_map(Result::ok)
    .filter_map(|task| std::fs::read_to_string(task.path().join("status")).ok())
    .flat_map(|status| {
      status
        .lines()
        .filter(|line| line.contains("ctxt_switches:"))
        .filter_map(|line| line.split_whitespace().nth(1)?.parse::<u64>().ok())
        .collect::<Vec<_>>()
    })
    .sum()
}
