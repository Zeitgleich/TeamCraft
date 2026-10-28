//! The browser call task's decisions about its sessions, without the browser: which session's news
//! still counts, when a session's path counts as lost, when to read its packet count or ping the
//! SFU, and when a network change may hurry a rejoin. Time is passed in, so all of it is tested
//! without a network.
//!
//! Two kinds of evidence prove a session's path alive, either is enough:
//!
//! - the browser's packet count (`getStats().packetsReceived`), which includes the SFU's QUIC
//!   keep-alives. Safari reports every count as zero, so there it proves nothing;
//! - the page's own traffic: each datagram and signaling frame the read loops deliver, and the
//!   SFU's answers to the pings (`CallClientMessage::Ping`) sent when nothing else arrived for
//!   the echo interval. Only an SFU that answered once is watched this way: an older one ignores
//!   pings, and a quiet call would look dead (#841).
//!
//! Media alone never decides: a muted microphone, a camera that is off or a still screen share
//! send nothing for long stretches, and the answered pings keep such a session alive.

use std::time::Duration;

use web_time::Instant;

use crate::call_resilience::{PathMonitor, PathState, ReconnectSchedule, ResilienceConfig};

/// Pings a session may leave unanswered, each given the echo interval, before its SFU counts as
/// one that doesn't answer (an older one). The signaling stream is reliable and ordered, so an SFU
/// that answers has answered by then unless the path is already gone.
const MAX_UNANSWERED_PINGS: u32 = 5;

/// A gap this long between two timer ticks means the page was suspended (a background tab, a
/// locked phone): the silence it didn't watch proves nothing about the path.
const SUSPENDED_TICK_GAP: Duration = Duration::from_secs(2);

/// Generations, packet counts, pings and loss of the call's current session.
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
  /// What the page itself hears of the current session.
  echo: Echo,
  /// Ping ids grow across sessions, so an answer always names the ping it answers.
  next_ping: u64,
  maybe_last_tick_at: Option<Instant>,
  resilience: ResilienceConfig,
}

/// The page's own view of the current session: its traffic and the SFU's answers to pings.
#[derive(Debug, Clone)]
struct Echo {
  watch: EchoWatch,
  /// When a datagram, a signaling frame or an answer last arrived.
  last_heard_at: Instant,
  maybe_last_ping_at: Option<Instant>,
  /// After a network change only the answer to this ping, or to a later one, proves the path:
  /// anything else may have been on its way before the change.
  maybe_handover_ping: Option<u64>,
}

/// Whether the current session's SFU answers pings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EchoWatch {
  /// No answer yet: pinging to find out.
  Probing { unanswered: u32 },
  /// It answered: its answers and the page's own traffic prove the path.
  Answering,
  /// No answer to `MAX_UNANSWERED_PINGS` pings: an older SFU, stop asking (a late answer still
  /// counts).
  Unanswered,
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
  /// The SFU doesn't answer pings (log it once).
  Unanswered,
  /// Silent for too long, or after a network change: rejoin.
  Lost,
}

/// What a timer tick asks of the call task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Tick {
  /// Send a ping with this id.
  pub maybe_ping: Option<u64>,
  pub verdict: PathVerdict,
}

/// What to do about a network change the page reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NetworkVerdict {
  /// The session has to show it still works: read its count now (unless a reading is on its way
  /// or the browser doesn't count) and ping the SFU (unless it doesn't answer).
  Check {
    maybe_reading: Option<Reading>,
    maybe_ping: Option<u64>,
  },
  /// No session: a rejoin is under way, it may try again early.
  HurryRejoin,
  /// Neither the browser's counts nor the SFU's answers can tell whether the session still works;
  /// it shows when it closes.
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
      echo: Echo::new(now),
      next_ping: 0,
      maybe_last_tick_at: None,
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
    // Maybe another SFU (a restarted one): whether it answers pings is found out again.
    self.echo = Echo::new(now);
  }

  /// A datagram or a signaling frame of the current session arrived at `now` (the caller checked
  /// [`Self::is_current`]). It proves the path, except while a network change waits for proof:
  /// then it may have been on its way before the change.
  pub(super) fn on_heard(&mut self, now: Instant) {
    self.echo.last_heard_at = now;
    if !self.monitor.is_migrating() {
      self.monitor.on_activity(now);
    }
  }

  /// The SFU answered the current session's ping `id` at `now` (the caller checked
  /// [`Self::is_current`]).
  pub(super) fn on_pong(&mut self, id: u64, now: Instant) {
    self.echo.watch = EchoWatch::Answering;
    self.echo.last_heard_at = now;
    let is_proof = !self.monitor.is_migrating()
      || self
        .echo
        .maybe_handover_ping
        .is_some_and(|handover_ping| id >= handover_ping);
    if is_proof {
      self.echo.maybe_handover_ping = None;
      self.monitor.on_activity(now);
    }
  }

  /// A timer tick at `now`: whether to ping the SFU, and whether the session is lost. Only a
  /// session whose SFU answers pings is judged here; packet counts are judged as they arrive.
  pub(super) fn on_tick(&mut self, now: Instant) -> Tick {
    let maybe_last_tick_at = self.maybe_last_tick_at.replace(now);
    let mut tick = Tick {
      maybe_ping: None,
      verdict: PathVerdict::Unchanged,
    };
    if !self.is_connected {
      return tick;
    }
    if maybe_last_tick_at.is_some_and(|last| now.saturating_duration_since(last) >= SUSPENDED_TICK_GAP) {
      self.monitor.restart_silence(now, &self.resilience);
      // Ask at once whether the session survived.
      self.echo.maybe_last_ping_at = None;
    }
    let is_ping_due = self
      .echo
      .maybe_last_ping_at
      .is_none_or(|pinged_at| now.saturating_duration_since(pinged_at) >= self.resilience.echo_interval);
    match self.echo.watch {
      EchoWatch::Probing { unanswered } if is_ping_due && unanswered >= MAX_UNANSWERED_PINGS => {
        self.echo.watch = EchoWatch::Unanswered;
        tick.verdict = PathVerdict::Unanswered;
      },
      EchoWatch::Probing { .. } => {
        if is_ping_due {
          tick.maybe_ping = Some(self.ping(now));
        }
      },
      EchoWatch::Answering => {
        let is_quiet =
          now.saturating_duration_since(self.echo.last_heard_at) >= self.resilience.echo_interval;
        // While a network change waits for proof, only answers count: keep asking.
        if is_ping_due && (is_quiet || self.monitor.is_migrating()) {
          tick.maybe_ping = Some(self.ping(now));
        }
        if self.monitor.check(now, &self.resilience) == PathState::Dead {
          tick.verdict = PathVerdict::Lost;
        }
      },
      EchoWatch::Unanswered => {},
    }
    tick
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
    let is_counted = self.path_watch != PathWatch::Unavailable;
    if !is_counted && self.echo.watch != EchoWatch::Answering {
      return NetworkVerdict::Nothing;
    }
    self.monitor.on_network_changed(now, &self.resilience);
    let maybe_ping = (self.echo.watch != EchoWatch::Unanswered).then(|| self.ping(now));
    self.echo.maybe_handover_ping = maybe_ping;
    let maybe_reading = if is_counted {
      // A reading on its way, and the next one, may still count packets from before the change.
      let maybe_reading = self.next_reading();
      self.maybe_baseline_until = Some(match maybe_reading {
        Some(reading) => reading.id,
        // The one on its way is followed by the next one, asked for at the next tick.
        None => self.next_reading,
      });
      maybe_reading
    } else {
      None
    };
    NetworkVerdict::Check {
      maybe_reading,
      maybe_ping,
    }
  }

  /// The next ping's id, sent at `now`.
  fn ping(&mut self, now: Instant) -> u64 {
    let id = self.next_ping;
    self.next_ping += 1;
    self.echo.maybe_last_ping_at = Some(now);
    if let EchoWatch::Probing { unanswered } = &mut self.echo.watch {
      *unanswered += 1;
    }
    id
  }
}

impl Echo {
  fn new(now: Instant) -> Self {
    Self {
      watch: EchoWatch::Probing { unanswered: 0 },
      last_heard_at: now,
      maybe_last_ping_at: None,
      maybe_handover_ping: None,
    }
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
    fn a_browser_without_counts_is_not_asked_again() {
      let start = Instant::now();
      let mut watch = SessionWatch::new(start, config());
      let reading = watch.next_reading().unwrap();
      assert_eq!(
        watch.on_reading(reading, PacketCount::Unsupported, start),
        PathVerdict::Unwatchable
      );
      assert_eq!(watch.next_reading(), None);
      // Before the SFU answered a ping (`echo`) nothing proves the session after a network change.
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
      let NetworkVerdict::Check {
        maybe_reading: Some(reading),
        ..
      } = watch.on_network_changed(at(start, 1_000))
      else {
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
      let NetworkVerdict::Check {
        maybe_reading: Some(reading),
        ..
      } = watch.on_network_changed(at(start, 1_000))
      else {
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
        NetworkVerdict::Check {
          maybe_reading: None,
          // Whether the SFU answers is not known yet: it is asked.
          maybe_ping: Some(0),
        }
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

  /// Safari: no packet counts, so the page's own traffic and the SFU's answers decide (#841).
  mod echo {
    use super::*;

    #[test]
    fn answered_pings_keep_a_silent_call_alive() {
      let start = Instant::now();
      let mut watch = safari(start);
      // Microphone muted, cameras off, nobody sharing: no media for a minute.
      let run = run(&mut watch, start, 100..=60_000, |_| true);
      assert!(run.verdicts.is_empty(), "{:?}", run.verdicts);
      // About one per echo interval, no more.
      assert!((50..=60).contains(&run.pings.len()), "{} pings", run.pings.len());
    }

    #[test]
    fn a_session_whose_answers_stop_is_lost_after_the_stall_timeout() {
      let start = Instant::now();
      let mut watch = safari(start);
      let run = run(&mut watch, start, 100..=30_000, |millis| millis < 10_000);
      // The last answer came at 8.94 s (pings go out every 1.1 s: answered after 40 ms, then quiet
      // for the echo interval).
      assert_eq!(run.verdicts, vec![(14_000, PathVerdict::Lost)]);
    }

    #[test]
    fn media_spares_the_pings() {
      let start = Instant::now();
      let mut watch = safari(start);
      let mut pings = Vec::new();
      for millis in (100..=10_000).step_by(100) {
        // Someone talks: their audio arrives all the time.
        watch.on_heard(at(start, millis - 20));
        let tick = watch.on_tick(at(start, millis));
        assert_eq!(tick.verdict, PathVerdict::Unchanged, "at {millis} ms");
        if let Some(id) = tick.maybe_ping {
          pings.push(id);
          watch.on_pong(id, at(start, millis + 40));
        }
      }
      // Only the first, which finds out that the SFU answers.
      assert_eq!(pings, vec![0]);
    }

    #[test]
    fn media_proves_the_path_without_answers() {
      let start = Instant::now();
      let mut watch = safari(start);
      run(&mut watch, start, 100..=1_000, |_| true);
      for millis in (1_100..=20_000).step_by(100) {
        // A screen share's frames arrive, the answers don't (the SFU's queue was full).
        watch.on_heard(at(start, millis - 50));
        let tick = watch.on_tick(at(start, millis));
        assert_eq!(tick.verdict, PathVerdict::Unchanged, "at {millis} ms");
      }
    }

    #[test]
    fn an_sfu_that_never_answers_is_asked_a_few_times_and_never_judged() {
      let start = Instant::now();
      let mut watch = safari(start);
      let run = run(&mut watch, start, 100..=60_000, |_| false);
      assert_eq!(run.pings.len(), MAX_UNANSWERED_PINGS as usize);
      // Once, after the last ping had its echo interval.
      assert_eq!(run.verdicts, vec![(5_100, PathVerdict::Unanswered)]);
      assert_eq!(
        watch.on_network_changed(at(start, 61_000)),
        NetworkVerdict::Nothing
      );
    }

    #[test]
    fn a_late_answer_still_counts() {
      let start = Instant::now();
      let mut watch = safari(start);
      run(&mut watch, start, 100..=6_000, |_| false);
      watch.on_pong(4, at(start, 6_050));
      let run = run(&mut watch, start, 6_100..=20_000, |_| false);
      assert!(!run.pings.is_empty(), "asking again");
      assert_eq!(run.verdicts, vec![(11_100, PathVerdict::Lost)]);
    }

    #[test]
    fn after_a_network_change_only_a_fresh_answer_proves_the_path() {
      let start = Instant::now();
      let mut watch = safari(start);
      run(&mut watch, start, 100..=2_000, |_| true);
      let NetworkVerdict::Check {
        maybe_reading: None,
        maybe_ping: Some(handover_ping),
      } = watch.on_network_changed(at(start, 2_050))
      else {
        panic!("no ping asked for");
      };
      // What was on its way before the change: media, and the answer to an older ping.
      watch.on_heard(at(start, 2_060));
      watch.on_pong(handover_ping - 1, at(start, 2_070));
      let run = run(&mut watch, start, 2_100..=10_000, |_| false);
      // Asked again while waiting; lost at the handover timeout, not the stall timeout.
      assert!(run.pings.len() >= 2, "{:?}", run.pings);
      assert_eq!(run.verdicts, vec![(5_100, PathVerdict::Lost)]);
    }

    #[test]
    fn a_fresh_answer_after_a_network_change_keeps_the_session() {
      let start = Instant::now();
      let mut watch = safari(start);
      run(&mut watch, start, 100..=2_000, |_| true);
      let NetworkVerdict::Check {
        maybe_ping: Some(handover_ping),
        ..
      } = watch.on_network_changed(at(start, 2_050))
      else {
        panic!("no ping asked for");
      };
      watch.on_pong(handover_ping, at(start, 2_200));
      let run = run(&mut watch, start, 2_100..=20_000, |_| true);
      assert!(run.verdicts.is_empty(), "{:?}", run.verdicts);
    }

    #[test]
    fn a_suspended_page_is_not_lost_for_the_silence_it_slept_through() {
      let start = Instant::now();
      let mut watch = safari(start);
      run(&mut watch, start, 100..=2_000, |_| true);
      // The phone was locked for a minute; the timer stood still.
      let tick = watch.on_tick(at(start, 62_000));
      assert_eq!(tick.verdict, PathVerdict::Unchanged);
      let id = tick.maybe_ping.expect("asks at once");
      watch.on_pong(id, at(start, 62_050));
      let run = run(&mut watch, start, 62_100..=80_000, |_| true);
      assert!(run.verdicts.is_empty(), "{:?}", run.verdicts);
    }

    #[test]
    fn a_suspended_page_whose_session_died_finds_out_within_the_stall_timeout() {
      let start = Instant::now();
      let mut watch = safari(start);
      run(&mut watch, start, 100..=2_000, |_| true);
      assert!(watch.on_tick(at(start, 62_000)).maybe_ping.is_some());
      let run = run(&mut watch, start, 62_100..=80_000, |_| false);
      assert_eq!(run.verdicts, vec![(67_000, PathVerdict::Lost)]);
    }

    #[test]
    fn a_new_session_finds_out_again_whether_its_sfu_answers() {
      let start = Instant::now();
      let mut watch = safari(start);
      run(&mut watch, start, 100..=2_000, |_| true);
      let next = watch.lose();
      assert_eq!(
        watch.on_tick(at(start, 2_100)).maybe_ping,
        None,
        "no session to ask"
      );
      watch.relinked(next, at(start, 2_150));
      // An older SFU took over: never lost for its silence.
      let run = run(&mut watch, start, 2_200..=30_000, |_| false);
      assert_eq!(run.pings.len(), MAX_UNANSWERED_PINGS as usize);
      assert_eq!(run.verdicts, vec![(7_200, PathVerdict::Unanswered)]);
    }

    #[test]
    fn own_traffic_counts_where_the_browser_counts_too() {
      let start = Instant::now();
      let mut watch = SessionWatch::new(start, config());
      read(&mut watch, 10, at(start, 500));
      // The count stands still, but media arrives.
      watch.on_heard(at(start, 5_000));
      assert_eq!(read(&mut watch, 10, at(start, 5_500)), PathVerdict::Unchanged);
      assert_eq!(read(&mut watch, 10, at(start, 10_000)), PathVerdict::Lost);
    }

    #[test]
    fn counting_browsers_are_judged_by_their_readings_until_the_sfu_answers() {
      let start = Instant::now();
      let mut watch = SessionWatch::new(start, config());
      read(&mut watch, 10, at(start, 500));
      // No answers, no readings: the ticks don't judge (the readings do, as before #841).
      let run = run(&mut watch, start, 600..=20_000, |_| false);
      assert_eq!(run.verdicts, vec![(5_600, PathVerdict::Unanswered)]);
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

  /// What [`run`] saw: the pings sent, and every verdict but `Unchanged` with its time.
  struct Run {
    pings: Vec<u64>,
    verdicts: Vec<(u64, PathVerdict)>,
  }

  /// Ticks every 100 ms over `millis` (after `start`), like the call task's timer; the SFU
  /// answers a ping 40 ms after it went out while `answers(millis)` holds. Stops when the session
  /// is lost.
  fn run(
    watch: &mut SessionWatch,
    start: Instant,
    millis: std::ops::RangeInclusive<u64>,
    answers: impl Fn(u64) -> bool,
  ) -> Run {
    let mut run = Run {
      pings: Vec::new(),
      verdicts: Vec::new(),
    };
    for millis in millis.step_by(100) {
      let tick = watch.on_tick(at(start, millis));
      if tick.verdict != PathVerdict::Unchanged {
        run.verdicts.push((millis, tick.verdict));
      }
      if tick.verdict == PathVerdict::Lost {
        break;
      }
      if let Some(id) = tick.maybe_ping {
        run.pings.push(id);
        if answers(millis) {
          watch.on_pong(id, at(start, millis + 40));
        }
      }
    }
    run
  }

  /// A session in a browser whose `getStats()` counts nothing.
  fn safari(start: Instant) -> SessionWatch {
    let mut watch = SessionWatch::new(start, config());
    let reading = watch.next_reading().expect("a reading");
    assert_eq!(
      watch.on_reading(reading, PacketCount::Unsupported, start),
      PathVerdict::Unwatchable
    );
    watch
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
