//! Whether a capture still turns what its source hands over into pictures (#1805).
//!
//! A screen share that showed one still picture for minutes while the screen moved reached
//! nobody: a viewer's freeze detection counted the share's idle refresh (the last picture again
//! every second) as new pictures, the buffers the capture threw away were warnings in a local log,
//! and the sharer saw nothing wrong. A still screen delivers no buffers at all, so "no new picture
//! for a while" says nothing on its own; buffers that arrive and can't be used do.
//!
//! [`CaptureHealth`] counts each buffer by what became of it ([`BufferOutcome`]), shared between the
//! capture's thread and whoever reads it. [`CaptureStallWatch`] judges the counts over a window:
//! stalled when buffers kept coming and most of them were unusable, recovered once pictures come
//! again. The call driver tells the app (a hint for the sharer) and the call's crash reports.
//!
//! The browser (`call_web`) hands over no buffers: it takes a picture from a playing `<video>` at
//! the track's frame rate, which works as long as the element has one, new or not. What it counts
//! instead (#1805): pictures it took ([`BufferCounts::taken`]), takes with nothing to take
//! ([`BufferCounts::not_ready`]) or that failed ([`BufferCounts::unreadable`]), and where the
//! browser tells, the frames its capture delivered (`pictures`) and dropped before they reached
//! the page (`discarded`). A still tab or screen delivers no frames and drops none, so it never
//! stalls; frames that keep coming and all get dropped do. A capture the browser stops feeding
//! looks the same as a still one and isn't told apart (a measurement in a real browser first).

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

// `std::time::Instant` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

/// The window the counts are judged over: long enough for a moving screen to hand over dozens of
/// buffers, short enough that the sharer learns within seconds.
pub const STALL_WINDOW: Duration = Duration::from_secs(5);
/// Unusable buffers in a window, at least, for a stall: one damaged buffer is no stalled capture.
pub const MIN_UNUSABLE: u64 = 5;

/// What became of one buffer of the capture's source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferOutcome {
  Picture,
  /// No picture by design: only the cursor moved.
  CursorOnly,
  Corrupted,
  /// A buffer the capture couldn't read (a DMA-BUF it can't map).
  Unmapped,
  /// Its picture lies outside its memory, or is in a format the capture can't convert.
  Unreadable,
}

/// What became of a capture's buffers, counted since it started (the call's stats carry them as
/// `teamcraft_client_api::video_stats::CaptureBufferStats`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BufferCounts {
  /// New pictures from the source: buffers that became one, or in the browser the frames its
  /// capture delivered (where it tells).
  pub pictures: u64,
  pub cursor_only: u64,
  pub corrupted: u64,
  pub unmapped: u64,
  pub unreadable: u64,
  /// The browser: pictures taken from the playing capture, new or not (0 on the desktop).
  pub taken: u64,
  /// The browser: takes while the capture had no picture to give (0 on the desktop).
  pub not_ready: u64,
  /// The browser: frames its capture dropped before they reached the page (0 on the desktop and
  /// where the browser doesn't tell).
  pub discarded: u64,
}

/// One reading of a capture: its counts, and the frames its encoders produced so far (the
/// desktop: all layers; the browser: the top layer).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptureSample {
  pub buffers: BufferCounts,
  pub frames_encoded: u64,
}

/// What a stalled capture did over the judged window (#1805), for the sharer's crash report:
/// counters only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StallWindow {
  pub buffers: BufferCounts,
  /// Frames the encoders produced over the window.
  pub frames_encoded: u64,
  /// How long the window was.
  pub length: Duration,
  /// Since the last new picture; `None` when none came since the capture was first judged.
  pub maybe_since_picture: Option<Duration>,
}

/// The buffer counts of one capture, shared between its thread and readers; cloning shares them.
#[derive(Debug, Clone, Default)]
pub struct CaptureHealth {
  counts: Arc<Counts>,
}

#[derive(Debug, Default)]
struct Counts {
  pictures: AtomicU64,
  cursor_only: AtomicU64,
  corrupted: AtomicU64,
  unmapped: AtomicU64,
  unreadable: AtomicU64,
}

/// Judges a capture's counts over [`STALL_WINDOW`].
#[derive(Debug, Default)]
pub struct CaptureStallWatch {
  samples: VecDeque<(Instant, CaptureSample)>,
  is_stalled: bool,
  /// When the count of new pictures last grew.
  maybe_last_picture: Option<Instant>,
}

/// A change of a capture's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StallChange {
  /// Buffers keep coming, and most of them can't be used: what came over the window.
  Stalled(StallWindow),
  /// Pictures come again.
  Recovered,
}

impl CaptureHealth {
  pub fn new() -> Self {
    Self::default()
  }

  /// Counts one buffer.
  pub fn record(&self, outcome: BufferOutcome) {
    let counter = match outcome {
      BufferOutcome::Picture => &self.counts.pictures,
      BufferOutcome::CursorOnly => &self.counts.cursor_only,
      BufferOutcome::Corrupted => &self.counts.corrupted,
      BufferOutcome::Unmapped => &self.counts.unmapped,
      BufferOutcome::Unreadable => &self.counts.unreadable,
    };
    counter.fetch_add(1, Ordering::Relaxed);
  }

  /// The counts so far.
  pub fn stats(&self) -> BufferCounts {
    let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
    BufferCounts {
      pictures: read(&self.counts.pictures),
      cursor_only: read(&self.counts.cursor_only),
      corrupted: read(&self.counts.corrupted),
      unmapped: read(&self.counts.unmapped),
      unreadable: read(&self.counts.unreadable),
      ..BufferCounts::default()
    }
  }
}

impl BufferCounts {
  /// Buffers (or takes) that should have been a picture and weren't.
  pub fn unusable(&self) -> u64 {
    self.corrupted + self.unmapped + self.unreadable + self.not_ready
  }

  /// Whether these counts (over a window) are a stalled capture: unusable buffers keep coming and
  /// outnumber the pictures, or the browser's capture delivered frames to nobody and dropped them
  /// all. A still source hands over nothing, or only cursor moves, and is never stalled.
  pub fn is_stalled(&self) -> bool {
    let unusable = self.unusable();
    let is_unusable = unusable >= MIN_UNUSABLE && unusable > self.pictures.max(self.taken);
    let is_all_dropped = self.discarded >= MIN_UNUSABLE && self.pictures == 0;
    is_unusable || is_all_dropped
  }

  /// What came since `earlier` (counters only grow; a restarted capture counts from zero).
  pub fn since(&self, earlier: &Self) -> Self {
    Self {
      pictures: self.pictures.saturating_sub(earlier.pictures),
      cursor_only: self.cursor_only.saturating_sub(earlier.cursor_only),
      corrupted: self.corrupted.saturating_sub(earlier.corrupted),
      unmapped: self.unmapped.saturating_sub(earlier.unmapped),
      unreadable: self.unreadable.saturating_sub(earlier.unreadable),
      taken: self.taken.saturating_sub(earlier.taken),
      not_ready: self.not_ready.saturating_sub(earlier.not_ready),
      discarded: self.discarded.saturating_sub(earlier.discarded),
    }
  }
}

#[cfg(feature = "call-view")]
impl From<BufferCounts> for teamcraft_client_api::video_stats::CaptureBufferStats {
  fn from(counts: BufferCounts) -> Self {
    Self {
      pictures: counts.pictures,
      cursor_only: counts.cursor_only,
      corrupted: counts.corrupted,
      unmapped: counts.unmapped,
      unreadable: counts.unreadable,
      taken: counts.taken,
      not_ready: counts.not_ready,
      discarded: counts.discarded,
    }
  }
}

impl CaptureStallWatch {
  pub fn new() -> Self {
    Self::default()
  }

  /// Takes a reading at `now`; a change, when the state changed.
  pub fn observe(&mut self, sample: CaptureSample, now: Instant) -> Option<StallChange> {
    let is_new_picture = self
      .samples
      .back()
      .map_or(sample.buffers.pictures > 0, |(_, last)| {
        sample.buffers.pictures > last.buffers.pictures
      });
    if is_new_picture {
      self.maybe_last_picture = Some(now);
    }
    self.samples.push_back((now, sample));
    while self
      .samples
      .get(1)
      .is_some_and(|(at, _)| now.saturating_duration_since(*at) >= STALL_WINDOW)
    {
      self.samples.pop_front();
    }
    let (first_at, first) = *self.samples.front()?;
    let length = now.saturating_duration_since(first_at);
    if length < STALL_WINDOW {
      return None;
    }
    let window = sample.buffers.since(&first.buffers);
    match self.is_stalled {
      false if window.is_stalled() => {
        self.is_stalled = true;
        Some(StallChange::Stalled(StallWindow {
          buffers: window,
          frames_encoded: sample.frames_encoded.saturating_sub(first.frames_encoded),
          length,
          maybe_since_picture: self
            .maybe_last_picture
            .map(|at| now.saturating_duration_since(at)),
        }))
      },
      true if !window.is_stalled() && window.pictures.max(window.taken) > window.unusable() => {
        self.is_stalled = false;
        Some(StallChange::Recovered)
      },
      _ => None,
    }
  }

  pub fn is_stalled(&self) -> bool {
    self.is_stalled
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const TICK: Duration = Duration::from_millis(100);

  fn sample(health: &CaptureHealth) -> CaptureSample {
    CaptureSample {
      buffers: health.stats(),
      frames_encoded: 0,
    }
  }

  /// The browser's capture as `call_web` reads it: per tick of 100 ms, `taken` pictures from the
  /// `<video>` and what the browser says its capture `delivered` and `discarded`.
  #[derive(Default)]
  struct Browser {
    counts: BufferCounts,
    frames_encoded: u64,
  }

  impl Browser {
    fn tick(&mut self, taken: u64, delivered: u64, discarded: u64) -> CaptureSample {
      self.counts.taken += taken;
      self.counts.pictures += delivered;
      self.counts.discarded += discarded;
      self.frames_encoded += taken;
      CaptureSample {
        buffers: self.counts,
        frames_encoded: self.frames_encoded,
      }
    }
  }

  fn changes(samples: impl Iterator<Item = CaptureSample>) -> Vec<(u32, StallChange)> {
    let mut watch = CaptureStallWatch::new();
    let start = Instant::now();
    samples
      .zip(0u32..)
      .filter_map(|(sample, tick)| {
        watch
          .observe(sample, start + TICK * tick)
          .map(|change| (tick, change))
      })
      .collect()
  }

  /// #1805, the browser: a still slide. The capture delivers a frame now and then or none at
  /// all, the page keeps taking the same picture at 15 a second: never a stall.
  #[test]
  fn a_still_slide_in_the_browser_is_no_stall() {
    let mut browser = Browser::default();
    let ticks = (0..600u32).map(|tick| {
      let taken = if tick % 2 == 0 { 2 } else { 1 };
      let delivered = u64::from(tick % 50 == 0);
      browser.tick(taken, delivered, 0)
    });
    assert_eq!(changes(ticks), Vec::new());
  }

  /// The browser: a moving screen at 60 Hz shared at 15 frames a second. The browser may count
  /// the frames it leaves out for the rate as discarded; frames still arrive: no stall.
  #[test]
  fn frames_left_out_for_the_frame_rate_are_no_stall() {
    let mut browser = Browser::default();
    let ticks = (0..600u32).map(|tick| {
      let taken = if tick % 2 == 0 { 2 } else { 1 };
      browser.tick(taken, taken, 6 - taken)
    });
    assert_eq!(changes(ticks), Vec::new());
  }

  /// The browser: its capture keeps producing frames and drops every one before the page gets
  /// it, while the page keeps encoding its last picture. Stalled after the window, with the
  /// counts, the frames encoded and how long ago the last new picture came; recovered once
  /// frames arrive again.
  #[test]
  fn frames_the_browser_drops_all_are_a_stall() {
    let mut browser = Browser::default();
    let moving = (0..30u32).map(|_| (2, 2, 0));
    let dropped = (30..130u32).map(|_| (2, 0, 3));
    let again = (130..300u32).map(|_| (2, 2, 0));
    let ticks = moving
      .chain(dropped)
      .chain(again)
      .map(|(taken, delivered, discarded)| browser.tick(taken, delivered, discarded));
    let found = changes(ticks);
    let [
      (stalled_at, StallChange::Stalled(window)),
      (recovered_at, StallChange::Recovered),
    ] = found.as_slice()
    else {
      panic!("{found:?}");
    };
    // The window holds the last moving frames until it has seen 5 s of drops only.
    assert_eq!(*stalled_at, 79);
    assert_eq!(window.buffers.pictures, 0);
    assert_eq!(window.buffers.discarded, 150);
    assert_eq!(window.buffers.taken, 100);
    assert_eq!(window.frames_encoded, 100);
    assert_eq!(window.length, TICK * 50);
    assert_eq!(window.maybe_since_picture, Some(TICK * 50));
    assert_eq!(*recovered_at, 130, "with the first frames again");
  }

  /// The browser: the page's takes fail or find no picture while the share runs: a stall.
  #[test]
  fn takes_that_fail_in_the_browser_are_a_stall() {
    let mut counts = BufferCounts::default();
    let ticks = (0..80u32).map(|tick| {
      if tick % 2 == 0 {
        counts.unreadable += 1;
      } else {
        counts.not_ready += 1;
      }
      CaptureSample {
        buffers: counts,
        frames_encoded: 0,
      }
    });
    let found = changes(ticks);
    let [(50, StallChange::Stalled(window))] = found.as_slice() else {
      panic!("{found:?}");
    };
    assert_eq!((window.buffers.unreadable, window.buffers.not_ready), (25, 25));
    assert_eq!(window.maybe_since_picture, None, "no picture ever came");
  }

  /// GNOME on an Intel laptop (#1805): the screen moves, buffers come at 30 a second, none can be
  /// read. Stalled after the window, with what came; pictures again: recovered.
  #[test]
  fn buffers_that_keep_coming_unusable_are_a_stall() {
    let health = CaptureHealth::new();
    let mut watch = CaptureStallWatch::new();
    let start = Instant::now();
    let mut changes = Vec::new();
    for tick in 0..80u32 {
      for _ in 0..3 {
        health.record(BufferOutcome::Unmapped);
      }
      health.record(BufferOutcome::CursorOnly);
      if let Some(change) = watch.observe(sample(&health), start + TICK * tick) {
        changes.push((tick, change));
      }
    }
    let [(at, StallChange::Stalled(window))] = changes.as_slice() else {
      panic!("{changes:?}");
    };
    assert_eq!(*at, 50, "after the window");
    assert_eq!(window.buffers.unmapped, 150);
    assert_eq!(window.buffers.pictures, 0);
    assert_eq!(window.buffers.cursor_only, 50);
    assert_eq!(window.maybe_since_picture, None);
    assert!(watch.is_stalled());
    let mut recovered_at = None;
    for tick in 80..200u32 {
      for _ in 0..3 {
        health.record(BufferOutcome::Picture);
      }
      if watch.observe(sample(&health), start + TICK * tick) == Some(StallChange::Recovered) {
        recovered_at = Some(tick);
        break;
      }
    }
    assert!(recovered_at.is_some_and(|tick| tick <= 110), "{recovered_at:?}");
    assert!(!watch.is_stalled());
  }

  /// A still screen hands over nothing, or only cursor moves: no stall, however long.
  #[test]
  fn a_still_screen_is_no_stall() {
    let health = CaptureHealth::new();
    let mut watch = CaptureStallWatch::new();
    let start = Instant::now();
    for tick in 0..300u32 {
      if tick % 7 == 0 {
        health.record(BufferOutcome::CursorOnly);
      }
      assert_eq!(watch.observe(sample(&health), start + TICK * tick), None);
    }
  }

  /// A damaged buffer now and then among pictures is no stall.
  #[test]
  fn a_few_damaged_buffers_among_pictures_are_no_stall() {
    let health = CaptureHealth::new();
    let mut watch = CaptureStallWatch::new();
    let start = Instant::now();
    for tick in 0..300u32 {
      health.record(BufferOutcome::Picture);
      if tick % 10 == 0 {
        health.record(BufferOutcome::Corrupted);
      }
      assert_eq!(watch.observe(sample(&health), start + TICK * tick), None);
    }
  }
}
