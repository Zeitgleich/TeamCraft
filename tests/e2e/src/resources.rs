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
      .fold(Self::default(), |sum, usage| Self {
        pss_bytes: sum.pss_bytes + usage.pss_bytes,
        cpu_time: sum.cpu_time + usage.cpu_time,
        context_switches: sum.context_switches + usage.context_switches,
        process_count: sum.process_count + usage.process_count,
      })
  }

  fn of_process(dir: &Path) -> Self {
    Self {
      pss_bytes: pss_bytes(dir),
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

/// `Pss:` of `smaps_rollup`, in kB.
fn pss_bytes(dir: &Path) -> u64 {
  let Ok(text) = std::fs::read_to_string(dir.join("smaps_rollup")) else {
    return 0;
  };
  text
    .lines()
    .find_map(|line| line.strip_prefix("Pss:"))
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
