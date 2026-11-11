//! Moving a browser call on the WebSocket fallback back to QUIC without a rejoin (#1309, #1338),
//! without the browser: when to try, what a try's session brings, and in which order the inputs of
//! the old and the new session reach the engine. The call task opens and closes the sessions and
//! does what this decides; time is passed in, so all of it is tested without a network.
//!
//! While the call is on the fallback and in it, and the browser has WebTransport, a QUIC session is
//! opened about every minute ([`QuicRetrySchedule`]) and asked to take the call's session over
//! (`switch`, with the secret of the session's `joined`). Its inputs carry a generation of their
//! own ([`SessionWatch::candidate`](super::session_watch::SessionWatch::candidate)), which isn't
//! the current one: until the SFU answers, its datagrams are kept for the engine (up to
//! [`MAX_EARLY_DATAGRAMS`]) and its first signal is the answer.
//!
//! After `switched` the engine's output goes to the new session at once, and the new session's
//! datagrams go to the engine. Its signals and its end are held back: the SFU sends what was queued
//! for the old session there first, up to its close with `SFU_SWITCHED_CLOSE_CODE`, and numbers on
//! on the new one, so the engine must see the old session's events first. At that close the new
//! session becomes the current one
//! ([`SessionWatch::moved`](super::session_watch::SessionWatch::moved)), without a rejoin, and what
//! was held back follows. An old session that ends otherwise during the move, or doesn't close
//! within [`SWITCH_DRAIN_TIMEOUT`], is lost as any other.

use std::mem;

use teamcraft_proto::{CallServerMessage, ErrorCode};
use tracing::{debug, info};
use web_time::Instant;

use super::SessionEnd;
use crate::call_resilience::{
  MAX_EARLY_DATAGRAMS, QuicRetrySchedule, ResilienceConfig, SWITCH_DRAIN_TIMEOUT,
};

/// Moving the call back to QUIC; `T` is a session's transport.
pub(super) struct SwitchBack<T> {
  /// When to try next; `None` while the call may not try.
  maybe_schedule: Option<QuicRetrySchedule>,
  phase: Phase<T>,
  /// The SFU doesn't know `switch` (it is from before #1309): no more tries in this call.
  is_unsupported: bool,
}

enum Phase<T> {
  Idle,
  /// A try's session of this generation is connecting.
  Opening {
    generation: u64,
  },
  /// A try's session asked to take the call's session over and waits for the answer.
  Asked(Asked<T>),
  /// The SFU moved the call's session to this one; the old one brings what was queued for it.
  Moving(Moving<T>),
}

struct Asked<T> {
  generation: u64,
  transport: T,
  asked_at: Instant,
  /// No answer in time: counted as a failed try, but kept open until the next one. The SFU may have
  /// moved the session to it all the same, which a late answer or the old session's close tells.
  is_overdue: bool,
  /// Datagrams that came before the answer.
  early: Vec<Vec<u8>>,
}

struct Moving<T> {
  generation: u64,
  transport: T,
  since: Instant,
  held: Vec<Held>,
}

/// An input of the session the call moves to, held back until the old one closed.
#[derive(Debug, PartialEq)]
pub(super) enum Held {
  Signal(CallServerMessage),
  Ended(SessionEnd),
}

/// What a timer tick asks of the call task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Due {
  Nothing,
  /// Time for a QUIC try: open a session with a candidate generation and pass that to
  /// [`SwitchBack::start`].
  Try,
  /// After the move the old session didn't close in time: it is lost.
  DrainTimedOut,
}

/// What became of a signal of a session that isn't the current one.
#[derive(Debug, PartialEq)]
pub(super) enum Answer<T> {
  /// News of a session the call no longer cares about.
  Stale,
  /// The session the call moves to: held back until the old one closed.
  Held,
  /// The SFU moved the call's session to the try's: the engine's output goes there from now on, and
  /// the datagrams that came before the answer go to the engine.
  Switched { early: Vec<Vec<u8>> },
  /// The SFU refused the try: its session is of no use any more.
  Refused(T),
}

/// The session the call goes on on after the old one closed.
#[derive(Debug, PartialEq)]
pub(super) struct Moved<T> {
  pub generation: u64,
  pub transport: T,
  /// Datagrams of the new session that haven't gone to the engine yet.
  pub early: Vec<Vec<u8>>,
  /// What the new session brought while the old one was still read, in order.
  pub held: Vec<Held>,
}

impl<T> SwitchBack<T> {
  pub(super) fn new() -> Self {
    Self {
      maybe_schedule: None,
      phase: Phase::Idle,
      is_unsupported: false,
    }
  }

  /// A timer tick at `now`. `may_try` says whether the call may try QUIC: it is on the fallback and
  /// in the call with a secret to move with, and the browser has WebTransport. Tries are planned
  /// while it holds and dropped while it doesn't.
  pub(super) fn on_tick(&mut self, may_try: bool, now: Instant, resilience: &ResilienceConfig) -> Due {
    match &mut self.phase {
      Phase::Moving(moving) => {
        return if now.saturating_duration_since(moving.since) >= SWITCH_DRAIN_TIMEOUT {
          Due::DrainTimedOut
        } else {
          Due::Nothing
        };
      },
      Phase::Opening { .. } => return Due::Nothing,
      Phase::Asked(asked) if !asked.is_overdue => {
        if now.saturating_duration_since(asked.asked_at) < resilience.switch_answer_timeout {
          return Due::Nothing;
        }
        debug!("no answer to the switch in time");
        asked.is_overdue = true;
        self.failed_try(now, resilience);
      },
      Phase::Asked(_) | Phase::Idle => {},
    }
    if !may_try || self.is_unsupported {
      self.maybe_schedule = None;
      return Due::Nothing;
    }
    let schedule = self.maybe_schedule.get_or_insert_with(|| {
      debug!("on the fallback: QUIC is tried again later");
      QuicRetrySchedule::new(now, resilience)
    });
    if now >= schedule.next_try_at() {
      Due::Try
    } else {
      Due::Nothing
    }
  }

  /// A try whose session has `generation` starts connecting. Returns the last try's session, which
  /// didn't answer in time, to close: a minute or more ago, had the SFU moved the session there, the
  /// old one would have closed.
  pub(super) fn start(&mut self, generation: u64) -> Option<T> {
    match mem::replace(&mut self.phase, Phase::Opening { generation }) {
      Phase::Asked(overdue) => Some(overdue.transport),
      Phase::Idle | Phase::Opening { .. } | Phase::Moving(_) => None,
    }
  }

  /// The try's session of `generation` opened at `now` and asked to take the call's session over.
  /// `Err` gives it back when the try is over: the call lost its session while it connected, or
  /// the call ends.
  pub(super) fn asked(&mut self, generation: u64, transport: T, now: Instant) -> Result<(), T> {
    if !matches!(self.phase, Phase::Opening { generation: opening } if opening == generation) {
      return Err(transport);
    }
    self.phase = Phase::Asked(Asked {
      generation,
      transport,
      asked_at: now,
      is_overdue: false,
      early: Vec::new(),
    });
    Ok(())
  }

  /// The try's session of `generation` didn't connect.
  pub(super) fn failed_to_open(&mut self, generation: u64, now: Instant, resilience: &ResilienceConfig) {
    if matches!(self.phase, Phase::Opening { generation: opening } if opening == generation) {
      self.phase = Phase::Idle;
      self.failed_try(now, resilience);
    }
  }

  /// A signal of the session of `generation`, which isn't the current one.
  pub(super) fn on_signal(
    &mut self,
    generation: u64,
    message: CallServerMessage,
    now: Instant,
    resilience: &ResilienceConfig,
  ) -> Answer<T> {
    match mem::replace(&mut self.phase, Phase::Idle) {
      Phase::Moving(mut moving) if moving.generation == generation => {
        moving.held.push(Held::Signal(message));
        self.phase = Phase::Moving(moving);
        Answer::Held
      },
      Phase::Asked(asked) if asked.generation == generation => {
        self.on_answer(asked, message, now, resilience)
      },
      other => {
        self.phase = other;
        Answer::Stale
      },
    }
  }

  /// A datagram of the session of `generation`, which isn't the current one: `Some` goes to the
  /// engine now.
  pub(super) fn on_datagram(&mut self, generation: u64, bytes: Vec<u8>) -> Option<Vec<u8>> {
    match &mut self.phase {
      Phase::Moving(moving) if moving.generation == generation => Some(bytes),
      Phase::Asked(asked) if asked.generation == generation => {
        if asked.early.len() < MAX_EARLY_DATAGRAMS {
          asked.early.push(bytes);
        }
        None
      },
      _ => None,
    }
  }

  /// The session of `generation`, which isn't the current one, ended at `now`. Returns a try's
  /// session that ended before its answer, to close.
  pub(super) fn on_ended(
    &mut self,
    generation: u64,
    end: SessionEnd,
    now: Instant,
    resilience: &ResilienceConfig,
  ) -> Option<T> {
    match mem::replace(&mut self.phase, Phase::Idle) {
      Phase::Moving(mut moving) if moving.generation == generation => {
        moving.held.push(Held::Ended(end));
        self.phase = Phase::Moving(moving);
        None
      },
      Phase::Asked(asked) if asked.generation == generation => {
        debug!(?end, "the QUIC try ended without an answer");
        if !asked.is_overdue {
          self.failed_try(now, resilience);
        }
        Some(asked.transport)
      },
      other => {
        self.phase = other;
        None
      },
    }
  }

  /// The call's current session closed because the SFU moved it to another one: the session it
  /// moved to, where the call goes on. That is the try's even if its answer hasn't come yet (the old
  /// session's close may overtake it): only this page knows the secret, and it gave it to no other
  /// session that is still open. `None` if no try could have taken it (the SFU moved the session to
  /// one this page gave up on): that is a loss.
  pub(super) fn finish(&mut self) -> Option<Moved<T>> {
    let moved = match mem::replace(&mut self.phase, Phase::Idle) {
      Phase::Moving(moving) => Moved {
        generation: moving.generation,
        transport: moving.transport,
        early: Vec::new(),
        held: moving.held,
      },
      Phase::Asked(asked) => Moved {
        generation: asked.generation,
        transport: asked.transport,
        early: asked.early,
        held: Vec::new(),
      },
      Phase::Idle | Phase::Opening { .. } => return None,
    };
    // On QUIC now: nothing to try any more.
    self.maybe_schedule = None;
    Some(moved)
  }

  /// The call's session is lost, or the call ends: no more tries for it. Returns the sessions the
  /// SFU may have moved the call's session to, which hold its slot as well: they are kept open like
  /// the lost one until the call is back. Whether the SFU can move sessions at all stays known.
  pub(super) fn stop(&mut self) -> Vec<T> {
    self.maybe_schedule = None;
    match mem::replace(&mut self.phase, Phase::Idle) {
      Phase::Asked(Asked { transport, .. }) | Phase::Moving(Moving { transport, .. }) => vec![transport],
      Phase::Idle | Phase::Opening { .. } => Vec::new(),
    }
  }

  /// The session the engine's output goes to while the call moves: the new one, as soon as the SFU
  /// said so.
  pub(super) fn maybe_moving_to(&self) -> Option<&T> {
    match &self.phase {
      Phase::Moving(moving) => Some(&moving.transport),
      Phase::Idle | Phase::Opening { .. } | Phase::Asked(_) => None,
    }
  }

  /// The SFU's answer to the try's `switch`.
  fn on_answer(
    &mut self,
    asked: Asked<T>,
    message: CallServerMessage,
    now: Instant,
    resilience: &ResilienceConfig,
  ) -> Answer<T> {
    match message {
      CallServerMessage::Switched => {
        info!("QUIC gets through again: the call moves to it");
        self.phase = Phase::Moving(Moving {
          generation: asked.generation,
          transport: asked.transport,
          since: now,
          held: Vec::new(),
        });
        Answer::Switched { early: asked.early }
      },
      CallServerMessage::Error { error } => {
        // What an SFU from before says to anything but `join`.
        if error.error_code == ErrorCode::ProtocolViolation {
          info!("the SFU can't move the call back to QUIC: it stays on the fallback");
          self.is_unsupported = true;
          self.maybe_schedule = None;
        } else {
          debug!(code = ?error.error_code, "the SFU refused the switch");
          if !asked.is_overdue {
            self.failed_try(now, resilience);
          }
        }
        Answer::Refused(asked.transport)
      },
      other => {
        debug!(?other, "ignoring a signal before the answer to the switch");
        self.phase = Phase::Asked(asked);
        Answer::Stale
      },
    }
  }

  /// The next try waits longer.
  fn failed_try(&mut self, now: Instant, resilience: &ResilienceConfig) {
    if let Some(schedule) = &mut self.maybe_schedule {
      schedule.on_failed_try(now, resilience);
    }
  }
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use teamcraft_proto::{
    CallEvent, CallId, CallParticipant, DeviceId, ErrorBody, Id, MediaCapabilities, MediaStreamId, Snowflake,
    UserId,
  };

  use super::*;
  use crate::bwe::BweConfig;
  use crate::call_engine::{CallEngine, CallEngineConfig, CallEngineEvent, EngineOutput};
  use crate::call_web::session_watch::SessionWatch;

  /// A transport, as far as these tests care.
  type Session = &'static str;

  mod schedule {
    use super::*;

    #[test]
    fn the_first_try_comes_about_a_minute_after_the_fallback() {
      let start = Instant::now();
      let mut switch = SwitchBack::<Session>::new();
      assert_eq!(switch.on_tick(true, start, &config()), Due::Nothing);
      assert_eq!(switch.on_tick(true, at(start, 47_000), &config()), Due::Nothing);
      assert_eq!(switch.on_tick(true, at(start, 73_000), &config()), Due::Try);
    }

    #[test]
    fn nothing_is_tried_while_the_call_may_not() {
      let start = Instant::now();
      let mut switch = SwitchBack::<Session>::new();
      switch.on_tick(true, start, &config());
      // Not in the call any more (a rejoin), or on QUIC: the plan goes.
      assert_eq!(switch.on_tick(false, at(start, 80_000), &config()), Due::Nothing);
      // Planned afresh once it may again.
      assert_eq!(switch.on_tick(true, at(start, 81_000), &config()), Due::Nothing);
      assert_eq!(switch.on_tick(true, at(start, 120_000), &config()), Due::Nothing);
      assert_eq!(switch.on_tick(true, at(start, 160_000), &config()), Due::Try);
    }

    #[test]
    fn a_try_that_does_not_connect_backs_off() {
      let start = Instant::now();
      let mut switch = SwitchBack::<Session>::new();
      let mut now = first_try(&mut switch, start);
      switch.start(1);
      // Not due again while it runs.
      assert_eq!(switch.on_tick(true, at(now, 90_000), &config()), Due::Nothing);
      now = at(now, 10_000);
      switch.failed_to_open(1, now, &config());
      // About two minutes now.
      assert_eq!(switch.on_tick(true, at(now, 95_000), &config()), Due::Nothing);
      assert_eq!(switch.on_tick(true, at(now, 145_000), &config()), Due::Try);
    }

    #[test]
    fn an_unanswered_try_backs_off_and_is_closed_by_the_next() {
      let start = Instant::now();
      let mut switch = SwitchBack::new();
      let now = first_try(&mut switch, start);
      switch.start(1);
      switch.asked(1, "try 1", now).unwrap();
      assert_eq!(switch.on_tick(true, at(now, 1_900), &config()), Due::Nothing);
      // No answer in two seconds: a failed try, the next one in about two minutes.
      assert_eq!(switch.on_tick(true, at(now, 2_000), &config()), Due::Nothing);
      assert_eq!(switch.on_tick(true, at(now, 90_000), &config()), Due::Nothing);
      assert_eq!(switch.on_tick(true, at(now, 150_000), &config()), Due::Try);
      assert_eq!(switch.start(2), Some("try 1"));
    }

    #[test]
    fn a_refusal_backs_off() {
      let start = Instant::now();
      let mut switch = SwitchBack::new();
      let now = first_try(&mut switch, start);
      switch.start(1);
      switch.asked(1, "try", now).unwrap();
      let answer = switch.on_signal(1, error(ErrorCode::InvalidJoinToken), at(now, 50), &config());
      assert_eq!(answer, Answer::Refused("try"));
      assert_eq!(switch.on_tick(true, at(now, 90_000), &config()), Due::Nothing);
      assert_eq!(switch.on_tick(true, at(now, 150_000), &config()), Due::Try);
    }

    /// An SFU from before #1309 answers "expected join": no more tries in this call.
    #[test]
    fn an_sfu_that_cannot_move_sessions_is_not_asked_again() {
      let start = Instant::now();
      let mut switch = SwitchBack::new();
      let now = first_try(&mut switch, start);
      switch.start(1);
      switch.asked(1, "try", now).unwrap();
      let answer = switch.on_signal(1, error(ErrorCode::ProtocolViolation), at(now, 50), &config());
      assert_eq!(answer, Answer::Refused("try"));
      for minutes in 1..30 {
        assert_eq!(
          switch.on_tick(true, at(now, minutes * 60_000), &config()),
          Due::Nothing
        );
      }
    }

    #[test]
    fn a_try_that_ends_without_an_answer_backs_off() {
      let start = Instant::now();
      let mut switch = SwitchBack::new();
      let now = first_try(&mut switch, start);
      switch.start(1);
      switch.asked(1, "try", now).unwrap();
      assert_eq!(
        switch.on_ended(1, SessionEnd::Lost, at(now, 100), &config()),
        Some("try")
      );
      assert_eq!(switch.on_tick(true, at(now, 90_000), &config()), Due::Nothing);
      assert_eq!(switch.on_tick(true, at(now, 150_000), &config()), Due::Try);
    }

    #[test]
    fn a_move_whose_old_session_does_not_close_is_a_loss() {
      let start = Instant::now();
      let mut switch = SwitchBack::new();
      let now = first_try(&mut switch, start);
      switch.start(1);
      switch.asked(1, "quic", now).unwrap();
      switch.on_signal(1, CallServerMessage::Switched, now, &config());
      assert_eq!(switch.on_tick(true, at(now, 9_000), &config()), Due::Nothing);
      assert_eq!(
        switch.on_tick(true, at(now, 10_000), &config()),
        Due::DrainTimedOut
      );
    }
  }

  /// The new step of the generation counter: the same session over a new transport, its inputs held
  /// back until the old session closed, and no rejoin.
  mod moving {
    use super::*;

    #[test]
    fn the_new_sessions_signals_and_end_wait_for_the_old_ones_close() {
      let start = Instant::now();
      let mut watch = SessionWatch::new(start, config());
      let mut switch = SwitchBack::new();
      let old = 0;
      let now = first_try(&mut switch, start);
      let candidate = watch.candidate();
      switch.start(candidate);
      switch.asked(candidate, "quic", now).unwrap();
      // Media comes at once, before the answer: kept.
      assert_eq!(switch.on_datagram(candidate, vec![1]), None);
      assert_eq!(
        switch.on_signal(candidate, CallServerMessage::Switched, now, &config()),
        Answer::Switched { early: vec![vec![1]] }
      );
      assert_eq!(switch.maybe_moving_to(), Some(&"quic"));
      // From now on its media goes to the engine at once; its signals and its end wait.
      assert_eq!(switch.on_datagram(candidate, vec![2]), Some(vec![2]));
      assert_eq!(
        switch.on_signal(candidate, event(8), now, &config()),
        Answer::Held
      );
      assert_eq!(switch.on_ended(candidate, SessionEnd::Lost, now, &config()), None);
      // The old session is still the current one.
      assert!(watch.is_current(old));
      assert!(!watch.is_current(candidate));

      let moved = switch.finish().expect("moved");
      watch.moved(moved.generation, at(now, 50));
      assert_eq!(moved.transport, "quic");
      assert!(moved.early.is_empty(), "went to the engine with the answer");
      assert_eq!(
        moved.held,
        vec![Held::Signal(event(8)), Held::Ended(SessionEnd::Lost)]
      );
      assert!(watch.is_current(candidate));
      assert!(!watch.is_current(old));
      assert_eq!(switch.maybe_moving_to(), None);
    }

    /// The old session's close can overtake the answer: the SFU moved the session to the try.
    #[test]
    fn the_old_sessions_close_before_the_answer_moves_the_call_to_the_try() {
      let start = Instant::now();
      let mut switch = SwitchBack::new();
      let now = first_try(&mut switch, start);
      switch.start(1);
      switch.asked(1, "quic", now).unwrap();
      switch.on_datagram(1, vec![1]);
      let moved = switch.finish().expect("moved");
      assert_eq!(moved.early, vec![vec![1]]);
      assert_eq!(moved.transport, "quic");
    }

    /// Also when the answer was overdue: it may still come, and the old session's close says it did.
    #[test]
    fn a_late_answer_still_moves_the_call() {
      let start = Instant::now();
      let mut switch = SwitchBack::new();
      let now = first_try(&mut switch, start);
      switch.start(1);
      switch.asked(1, "quic", now).unwrap();
      switch.on_tick(true, at(now, 3_000), &config());
      assert!(matches!(
        switch.on_signal(1, CallServerMessage::Switched, at(now, 3_500), &config()),
        Answer::Switched { .. }
      ));
      assert_eq!(switch.finish().map(|moved| moved.transport), Some("quic"));
    }

    #[test]
    fn a_close_for_a_move_this_page_did_not_ask_for_is_a_loss() {
      let mut switch = SwitchBack::<Session>::new();
      assert_eq!(switch.finish(), None);
      switch.start(1);
      assert_eq!(switch.finish(), None, "still connecting: it never asked");
    }

    /// The call's session is lost while a try runs: a session the SFU may have moved it to is kept
    /// like the lost one, and one still connecting is no longer the try's when it opens.
    #[test]
    fn a_loss_keeps_what_the_session_may_have_moved_to() {
      let start = Instant::now();
      let mut switch = SwitchBack::new();
      let now = first_try(&mut switch, start);
      switch.start(1);
      switch.asked(1, "quic", now).unwrap();
      switch.on_signal(1, CallServerMessage::Switched, now, &config());
      assert_eq!(switch.stop(), vec!["quic"]);
      assert_eq!(switch.on_datagram(1, vec![1]), None, "stale");

      let mut switch = SwitchBack::new();
      let now = first_try(&mut switch, start);
      switch.start(2);
      assert!(switch.stop().is_empty());
      assert_eq!(switch.asked(2, "late", at(now, 500)), Err("late"));
    }

    /// With the engine: the old session's last events and the new session's first ones arrive in
    /// either order, and the engine sees one sequence without a gap and never rejoins.
    #[test]
    fn the_engine_sees_one_sequence_and_no_rejoin() {
      let start = Instant::now();
      let mut engine = joined_engine(start);
      let mut watch = SessionWatch::new(start, config());
      let mut switch = SwitchBack::new();
      let old = 0;
      let now = first_try(&mut switch, start);
      let candidate = watch.candidate();
      switch.start(candidate);
      switch.asked(candidate, "quic", now).unwrap();
      assert!(matches!(
        switch.on_signal(candidate, CallServerMessage::Switched, now, &config()),
        Answer::Switched { .. }
      ));

      // The new session's first event comes before the old one's last.
      let mut delivered = Vec::new();
      for (generation, seq) in [(candidate, 4), (old, 2), (candidate, 5), (old, 3)] {
        if watch.is_current(generation) {
          delivered.push(seq);
          engine.handle_signal(event(seq), now).unwrap();
        } else {
          assert_eq!(
            switch.on_signal(generation, event(seq), now, &config()),
            Answer::Held
          );
        }
      }
      let moved = switch.finish().expect("moved");
      watch.moved(moved.generation, now);
      engine.moved_to_another_path();
      for held in moved.held {
        let Held::Signal(message) = held else {
          panic!("only signals were held");
        };
        engine.handle_signal(message, now).unwrap();
      }
      assert_eq!(delivered, vec![2, 3]);
      assert!(engine.is_in_call());
      let outputs: Vec<EngineOutput> = std::iter::from_fn(|| engine.poll_output()).collect();
      assert!(
        !outputs.iter().any(|output| matches!(
          output,
          EngineOutput::Event(CallEngineEvent::Reconnecting | CallEngineEvent::Reconnected)
        )),
        "the call never rejoined"
      );
    }

    fn joined_engine(now: Instant) -> CallEngine {
      let own = DeviceId::from_snowflake(Snowflake::new(1).unwrap());
      let mut engine = CallEngine::new(
        CallEngineConfig {
          user_id: UserId::from_snowflake(Snowflake::new(1001).unwrap()),
          device_id: own,
          capabilities: MediaCapabilities::default(),
          bwe: BweConfig::default(),
        },
        now,
      );
      engine.join("t".into());
      let joined = CallEvent::Joined {
        call_id: CallId::from_snowflake(Snowflake::new(99).unwrap()),
        participant: CallParticipant {
          user_id: UserId::from_snowflake(Snowflake::new(1001).unwrap()),
          device_id: own,
          audio_stream_id: MediaStreamId(1),
        },
        participants: Vec::new(),
        video_tracks: Vec::new(),
        screen_audio_tracks: Vec::new(),
        maybe_reconnect_grace_ms: Some(30_000),
        maybe_max_simulcast_layers: Some(teamcraft_proto::MAX_SIMULCAST_LAYERS),
        maybe_switch_secret: None,
        maybe_screen_share_taken_over: None,
        maybe_retransmit_window_ms: None,
      };
      engine
        .handle_signal(
          CallServerMessage::Event {
            seq: 1,
            event: joined,
          },
          now,
        )
        .unwrap();
      engine
    }
  }

  /// The first try is due; returns when.
  fn first_try(switch: &mut SwitchBack<Session>, start: Instant) -> Instant {
    switch.on_tick(true, start, &config());
    let due = at(start, 72_000);
    assert_eq!(switch.on_tick(true, due, &config()), Due::Try);
    due
  }

  /// An event that changes nothing but the sequence.
  fn event(seq: u64) -> CallServerMessage {
    CallServerMessage::Event {
      seq,
      event: CallEvent::ActiveSpeakers {
        device_ids: Vec::new(),
      },
    }
  }

  fn error(code: ErrorCode) -> CallServerMessage {
    CallServerMessage::Error {
      error: ErrorBody::new(code, "no"),
    }
  }

  fn config() -> ResilienceConfig {
    ResilienceConfig::browser()
  }

  fn at(start: Instant, millis: u64) -> Instant {
    start + Duration::from_millis(millis)
  }
}
