//! `feed.scroll_back` (#1595): the reader scrolling back in a conversation, from the first scroll up
//! until scrolling rests, as a performance measure (docs/crash-reports.md). It says whether
//! scrolling back was smooth (`longest_frame_ms`, `frames_over_50ms`) and how often the list made up
//! for rows above the reader: in the top spacer, which the reader doesn't feel (`spacer_corrections`),
//! or by moving the scroll position under the reader, which ends a wheel's smooth scrolling
//! (`scroll_corrections`). A scroll back shorter than a screen is not sent.
//!
//! Its frames are counted only while a measured scroll back runs: an animation frame callback per
//! frame, for as long as the reader scrolls.

use std::cell::RefCell;
use std::rc::Rc;

use crate::perf::{self, Measure, PerfOperation, PerfStatus, PerfUnit};

/// A frame longer than this is a stall the reader sees.
const SLOW_FRAME_MS: f64 = 50.0;

/// The scroll back being measured in one feed, if any.
#[derive(Clone, Default)]
pub struct ScrollBackWatch {
  maybe_session: Rc<RefCell<Option<Session>>>,
}

struct Session {
  measure: Measure,
  start_top: f64,
  min_top: f64,
  last_event_ms: f64,
  maybe_last_frame_ms: Option<f64>,
  frames: u32,
  slow_frames: u32,
  longest_frame_ms: f64,
  spacer_corrections: u32,
  scroll_corrections: u32,
}

impl ScrollBackWatch {
  /// The reader's scroll event at `top`: starts a measure when they scroll up and none runs.
  pub fn scrolled(&self, top: f64, is_up: bool) {
    let now_ms = perf::now_ms();
    let is_new = {
      let mut maybe_session = self.maybe_session.borrow_mut();
      match maybe_session.as_mut() {
        Some(session) => {
          session.min_top = session.min_top.min(top);
          session.last_event_ms = now_ms;
          false
        },
        None if is_up => {
          let measure = perf::begin(PerfOperation::ScrollBack);
          if !measure.is_on() {
            return;
          }
          *maybe_session = Some(Session {
            measure,
            start_top: top,
            min_top: top,
            last_event_ms: now_ms,
            maybe_last_frame_ms: None,
            frames: 0,
            slow_frames: 0,
            longest_frame_ms: 0.0,
            spacer_corrections: 0,
            scroll_corrections: 0,
          });
          true
        },
        None => false,
      }
    };
    if is_new {
      self.watch_frames();
    }
  }

  /// The list made up for rows above the reader: in the spacer, or by moving the scroll position.
  pub fn corrected(&self, is_scroll_moved: bool) {
    if let Some(session) = self.maybe_session.borrow_mut().as_mut() {
      if is_scroll_moved {
        session.scroll_corrections += 1;
      } else {
        session.spacer_corrections += 1;
      }
    }
  }

  /// Scrolling rested: the measure ends at the last scroll event, sent if the reader went back at
  /// least `viewport` pixels.
  pub fn rested(&self, viewport: f64) {
    let Some(session) = self.maybe_session.borrow_mut().take() else {
      return;
    };
    let Session {
      mut measure,
      start_top,
      min_top,
      last_event_ms,
      frames,
      slow_frames,
      longest_frame_ms,
      spacer_corrections,
      scroll_corrections,
      ..
    } = session;
    if start_top - min_top < viewport {
      // Too short to say anything; dropped unsent.
      return;
    }
    measure.measure("frames", f64::from(frames), PerfUnit::None);
    measure.measure("frames_over_50ms", f64::from(slow_frames), PerfUnit::None);
    measure.measure("longest_frame_ms", longest_frame_ms, PerfUnit::Millisecond);
    measure.measure(
      "spacer_corrections",
      f64::from(spacer_corrections),
      PerfUnit::None,
    );
    measure.measure(
      "scroll_corrections",
      f64::from(scroll_corrections),
      PerfUnit::None,
    );
    measure.finish_at(last_event_ms, PerfStatus::Ok);
  }

  /// Counts the frames while the session lasts.
  fn watch_frames(&self) {
    let watch = self.clone();
    leptos::prelude::request_animation_frame(move || {
      let now_ms = perf::now_ms();
      let is_running = {
        let mut maybe_session = watch.maybe_session.borrow_mut();
        match maybe_session.as_mut() {
          Some(session) => {
            if let Some(last_ms) = session.maybe_last_frame_ms {
              let frame_ms = now_ms - last_ms;
              session.frames += 1;
              session.longest_frame_ms = session.longest_frame_ms.max(frame_ms);
              if frame_ms > SLOW_FRAME_MS {
                session.slow_frames += 1;
              }
            }
            session.maybe_last_frame_ms = Some(now_ms);
            true
          },
          None => false,
        }
      };
      if is_running {
        watch.watch_frames();
      }
    });
  }
}
