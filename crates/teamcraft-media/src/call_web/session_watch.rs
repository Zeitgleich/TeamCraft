//! The browser call task's decisions about its sessions, without the browser: which session's news
//! still counts, when a session's path counts as lost, when to read its packet count, and when a
//! network change may hurry a rejoin. Time is passed in, so all of it is tested without a network.

use web_time::Instant;

use crate::call_resilience::{PathMonitor, PathState, ReconnectSchedule, ResilienceConfig};

/// Generations, packet counts and loss of the call's current session.
#[derive(Debug, Clone)]
pub(super) struct SessionWatch {
  /// Counts sessions: news of an older one (signals, datagrams, packet counts, its end) is stale.
  generation: u64,
  is_connected: bool,
  monitor: PathMonitor,
  path_watch: PathWatch,
  /// The packet count reading on its way, if any.
  maybe_pending_reading: Option<u64>,
  next_reading: u64,
  /// Readings up to this one were asked for before (or right at) the last network change: they
  /// may include packets from before it, so they are a baseline, not a sign of life.
  maybe_baseline_until: Option<u64>,
  resilience: ResilienceConfig,
}

/// Whether the browser reports packet counts, which prove the session's path alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathWatch {
  Untested,
  Watching,
  Unavailable,
}

/// A session's received packets, from `getStats()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PacketCount {
  Received(u64),
  /// The browser reports no packet counts: lost sessions show only when they close.
  Unsupported,
  /// No reading this time (the session is closing).
  Unknown,
}

impl PacketCount {
  /// The count of `getStats().packetsReceived`. An open session has received its handshake at
  /// least, so zero means the browser doesn't count: Safari 26 reports all its stats as zero.
  pub(super) fn from_stats(maybe_packets_received: Option<f64>) -> Self {
    match maybe_packets_received {
      // Counts are whole numbers far below 2^53.
      Some(count) if count >= 1.0 => Self::Received(count as u64),
      _ => Self::Unsupported,
    }
  }
}

/// A packet count reading to ask the browser for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Reading {
  pub generation: u64,
  pub id: u64,
}

/// What a packet count means for the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PathVerdict {
  /// Alive, still migrating, stale news or no reading: nothing to do.
  Unchanged,
  /// The browser can't report counts (log it once).
  Unwatchable,
  /// Silent for too long, or after a network change: rejoin.
  Lost,
}

/// What to do about a network change the page reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NetworkVerdict {
  /// The session has to show it still works: read its count now (unless a reading is on its way).
  Check(Option<Reading>),
  /// No session: a rejoin is under way, it may try again early.
  HurryRejoin,
  /// The browser can't tell whether the session still works; it shows when it closes.
  Nothing,
}

impl SessionWatch {
  /// The first session is up at `now`.
  pub(super) fn new(now: Instant, resilience: ResilienceConfig) -> Self {
    Self {
      generation: 0,
      is_connected: true,
      monitor: PathMonitor::new(now),
      path_watch: PathWatch::Untested,
      maybe_pending_reading: None,
      next_reading: 0,
      maybe_baseline_until: None,
      resilience,
    }
  }

  /// Whether news from the session of `generation` is about the current one.
  pub(super) fn is_current(&self, generation: u64) -> bool {
    self.is_connected && generation == self.generation
  }

  #[cfg(test)]
  fn generation(&self) -> u64 {
    self.generation
  }

  /// The current session is lost; returns the generation of the session that replaces it. From
  /// now on, news of the lost one is stale.
  pub(super) fn lose(&mut self) -> u64 {
    self.is_connected = false;
    self.generation += 1;
    self.maybe_pending_reading = None;
    self.maybe_baseline_until = None;
    self.generation
  }

  /// A new session of `generation` is up at `now`.
  pub(super) fn relinked(&mut self, generation: u64, now: Instant) {
    self.generation = generation;
    self.is_connected = true;
    self.monitor = PathMonitor::new(now);
    self.maybe_pending_reading = None;
    self.maybe_baseline_until = None;
  }

  /// The next packet count reading to ask for: none without a session, while one is on its way, or
  /// when the browser can't report counts.
  pub(super) fn next_reading(&mut self) -> Option<Reading> {
    if !self.is_connected || self.path_watch == PathWatch::Unavailable || self.maybe_pending_reading.is_some()
    {
      return None;
    }
    let id = self.next_reading;
    self.next_reading += 1;
    self.maybe_pending_reading = Some(id);
    Some(Reading {
      generation: self.generation,
      id,
    })
  }

  /// A reading arrived at `now`.
  pub(super) fn on_reading(&mut self, reading: Reading, count: PacketCount, now: Instant) -> PathVerdict {
    if self.maybe_pending_reading == Some(reading.id) {
      self.maybe_pending_reading = None;
    }
    if !self.is_current(reading.generation) {
      return PathVerdict::Unchanged;
    }
    match count {
      PacketCount::Received(count) => {
        self.path_watch = PathWatch::Watching;
        if self
          .maybe_baseline_until
          .is_some_and(|baseline_until| reading.id <= baseline_until)
        {
          self.monitor.on_baseline_packets(count);
        } else {
          self.maybe_baseline_until = None;
          self.monitor.on_received_packets(count, now);
        }
      },
      PacketCount::Unsupported => {
        let was_untested = self.path_watch == PathWatch::Untested;
        self.path_watch = PathWatch::Unavailable;
        return if was_untested {
          PathVerdict::Unwatchable
        } else {
          PathVerdict::Unchanged
        };
      },
      PacketCount::Unknown => return PathVerdict::Unchanged,
    }
    match self.monitor.check(now, &self.resilience) {
      PathState::Dead => PathVerdict::Lost,
      PathState::Alive | PathState::Migrating => PathVerdict::Unchanged,
    }
  }

  /// The page reported a network change at `now`.
  pub(super) fn on_network_changed(&mut self, now: Instant) -> NetworkVerdict {
    if !self.is_connected {
      return NetworkVerdict::HurryRejoin;
    }
    if self.path_watch == PathWatch::Unavailable {
      return NetworkVerdict::Nothing;
    }
    self.monitor.on_network_changed(now, &self.resilience);
    // A reading on its way, and the next one, may still count packets from before the change.
    let maybe_reading = self.next_reading();
    self.maybe_baseline_until = Some(match maybe_reading {
      Some(reading) => reading.id,
      // The one on its way is followed by the next one, asked for at the next tick.
      None => self.next_reading,
    });
    NetworkVerdict::Check(maybe_reading)
  }
}

/// Whether a network change at `now` may make the next rejoin attempt due at once: only after an
/// attempt failed, at least `first_retry_interval` ago. Before the first attempt there is nothing
/// to hurry (a lost session's is due at once; after the SFU went away its spread stands), and
/// hurrying again and again would skip the back-off and hammer the server with ticket requests.
pub(super) fn may_hurry(
  schedule: &ReconnectSchedule,
  maybe_last_failure_at: Option<Instant>,
  now: Instant,
  resilience: &ResilienceConfig,
) -> bool {
  schedule.attempts() > 0
    && maybe_last_failure_at
      .is_some_and(|failed_at| now.saturating_duration_since(failed_at) >= resilience.first_retry_interval)
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use super::*;
  use crate::call_resilience::Jitter;

  const GRACE: Duration = Duration::from_secs(30);

  mod path {
    use super::*;

    #[test]
    fn packet_counts_come_from_stats_that_count() {
      assert_eq!(PacketCount::from_stats(Some(42.0)), PacketCount::Received(42));
      assert_eq!(PacketCount::from_stats(Some(1.0)), PacketCount::Received(1));
      // Safari's stats are all zero, also while datagrams arrive.
      assert_eq!(PacketCount::from_stats(Some(0.0)), PacketCount::Unsupported);
      assert_eq!(PacketCount::from_stats(Some(-1.0)), PacketCount::Unsupported);
      assert_eq!(PacketCount::from_stats(Some(f64::NAN)), PacketCount::Unsupported);
      assert_eq!(PacketCount::from_stats(None), PacketCount::Unsupported);
    }

    #[test]
    fn steady_readings_keep_the_session() {
      let start = Instant::now();
      let mut watch = SessionWatch::new(start, config());
      for step in 1..=20u64 {
        let reading = watch.next_reading().unwrap();
        let verdict = watch.on_reading(reading, PacketCount::Received(step * 3), at(start, step * 500));
        assert_eq!(verdict, PathVerdict::Unchanged, "step {step}");
      }
    }

    #[test]
    fn silence_beyond_the_stall_timeout_loses_the_session() {
      let start = Instant::now();
      let mut watch = SessionWatch::new(start, config());
      read(&mut watch, 10, at(start, 500));
      assert_eq!(read(&mut watch, 10, at(start, 5_000)), PathVerdict::Unchanged);
      assert_eq!(read(&mut watch, 10, at(start, 5_500)), PathVerdict::Lost);
    }

    #[test]
    fn only_one_reading_is_on_its_way() {
      let mut watch = SessionWatch::new(Instant::now(), config());
      let reading = watch.next_reading().unwrap();
      assert_eq!(watch.next_reading(), None);
      watch.on_reading(reading, PacketCount::Unknown, Instant::now());
      assert!(watch.next_reading().is_some());
    }

    #[test]
    fn a_browser_without_counts_is_not_watched() {
      let start = Instant::now();
      let mut watch = SessionWatch::new(start, config());
      let reading = watch.next_reading().unwrap();
      assert_eq!(
        watch.on_reading(reading, PacketCount::Unsupported, start),
        PathVerdict::Unwatchable
      );
      assert_eq!(watch.next_reading(), None);
      // Nothing to check after a network change, and never lost for silence.
      assert_eq!(
        watch.on_network_changed(at(start, 9_000)),
        NetworkVerdict::Nothing
      );
    }
  }

  mod network_change {
    use super::*;

    #[test]
    fn the_first_reading_after_a_change_is_only_a_baseline() {
      let start = Instant::now();
      let mut watch = SessionWatch::new(start, config());
      read(&mut watch, 10, at(start, 500));
      let NetworkVerdict::Check(Some(reading)) = watch.on_network_changed(at(start, 1_000)) else {
        panic!("no reading asked for");
      };
      // Packets that arrived just before the change are no proof the new network works.
      watch.on_reading(reading, PacketCount::Received(12), at(start, 1_050));
      assert_eq!(read(&mut watch, 12, at(start, 3_500)), PathVerdict::Unchanged);
      assert_eq!(read(&mut watch, 12, at(start, 4_000)), PathVerdict::Lost);
    }

    #[test]
    fn packets_after_the_baseline_prove_the_session() {
      let start = Instant::now();
      let mut watch = SessionWatch::new(start, config());
      read(&mut watch, 10, at(start, 500));
      let NetworkVerdict::Check(Some(reading)) = watch.on_network_changed(at(start, 1_000)) else {
        panic!("no reading asked for");
      };
      watch.on_reading(reading, PacketCount::Received(12), at(start, 1_050));
      // An SFU keep-alive on the new path.
      assert_eq!(read(&mut watch, 13, at(start, 2_500)), PathVerdict::Unchanged);
      assert_eq!(read(&mut watch, 15, at(start, 4_500)), PathVerdict::Unchanged);
    }

    #[test]
    fn a_reading_on_its_way_at_the_change_is_a_baseline_and_so_is_the_next() {
      let start = Instant::now();
      let mut watch = SessionWatch::new(start, config());
      let before = watch.next_reading().unwrap();
      assert_eq!(
        watch.on_network_changed(at(start, 100)),
        NetworkVerdict::Check(None)
      );
      watch.on_reading(before, PacketCount::Received(5), at(start, 150));
      read(&mut watch, 6, at(start, 600));
      assert_eq!(read(&mut watch, 6, at(start, 3_100)), PathVerdict::Lost);
    }

    #[test]
    fn while_rejoining_a_change_hurries_the_rejoin() {
      let start = Instant::now();
      let mut watch = SessionWatch::new(start, config());
      watch.lose();
      assert_eq!(watch.on_network_changed(start), NetworkVerdict::HurryRejoin);
    }
  }

  mod generations {
    use super::*;

    #[test]
    fn news_of_a_lost_session_is_stale() {
      let start = Instant::now();
      let mut watch = SessionWatch::new(start, config());
      let old = watch.generation();
      let next = watch.lose();
      assert!(!watch.is_current(old), "while rejoining");
      watch.relinked(next, at(start, 1_000));
      // The old session lives on (falsely declared lost): its signals must not reach the engine.
      assert!(!watch.is_current(old));
      assert!(watch.is_current(next));
    }

    #[test]
    fn a_reading_pending_across_a_relink_neither_blocks_nor_counts() {
      let start = Instant::now();
      let mut watch = SessionWatch::new(start, config());
      let old = watch.next_reading().unwrap();
      let next = watch.lose();
      watch.relinked(next, at(start, 1_000));
      let fresh = watch.next_reading().expect("the old reading must not block");
      assert_eq!(fresh.generation, next);
      // The old session's count arrives late: it is no news of the new one.
      assert_eq!(
        watch.on_reading(old, PacketCount::Received(1_000), at(start, 7_000)),
        PathVerdict::Unchanged
      );
      assert!(
        watch.next_reading().is_none(),
        "the fresh reading is still on its way"
      );
    }
  }

  mod hurry {
    use super::*;

    #[test]
    fn nothing_is_hurried_before_the_first_attempt() {
      let start = Instant::now();
      let mut schedule = ReconnectSchedule::with_jitter(start, GRACE, Jitter::seeded(1));
      // After the SFU went away the first attempt waits for its spread.
      schedule.spread_first_attempt(start, &config());
      assert!(!may_hurry(&schedule, None, at(start, 500), &config()));
    }

    #[test]
    fn a_failed_attempt_is_hurried_only_after_the_first_retry_interval() {
      let start = Instant::now();
      let mut schedule = ReconnectSchedule::with_jitter(start, GRACE, Jitter::seeded(1));
      schedule.on_failed_attempt(start, &config());
      let interval = config().first_retry_interval;
      assert!(!may_hurry(
        &schedule,
        Some(start),
        start + interval / 2,
        &config()
      ));
      assert!(may_hurry(&schedule, Some(start), start + interval, &config()));
    }
  }

  fn read(watch: &mut SessionWatch, count: u64, now: Instant) -> PathVerdict {
    let reading = watch.next_reading().expect("a reading");
    watch.on_reading(reading, PacketCount::Received(count), now)
  }

  fn config() -> ResilienceConfig {
    ResilienceConfig::browser()
  }

  fn at(start: Instant, millis: u64) -> Instant {
    start + Duration::from_millis(millis)
  }
}
