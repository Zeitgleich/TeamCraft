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
/// `teamcraft_client_api::video_stats::BufferCounts`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BufferCounts {
  pub pictures: u64,
  pub cursor_only: u64,
  pub corrupted: u64,
  pub unmapped: u64,
  pub unreadable: u64,
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
  samples: VecDeque<(Instant, BufferCounts)>,
  is_stalled: bool,
}

/// A change of a capture's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StallChange {
  /// Buffers keep coming, and most of them can't be used: what came over the window.
  Stalled(BufferCounts),
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
    }
  }
}

impl BufferCounts {
  /// Buffers that should have been a picture and weren't.
  pub fn unusable(&self) -> u64 {
    self.corrupted + self.unmapped + self.unreadable
  }

  /// What came since `earlier` (counters only grow; a restarted capture counts from zero).
  pub fn since(&self, earlier: &Self) -> Self {
    Self {
      pictures: self.pictures.saturating_sub(earlier.pictures),
      cursor_only: self.cursor_only.saturating_sub(earlier.cursor_only),
      corrupted: self.corrupted.saturating_sub(earlier.corrupted),
      unmapped: self.unmapped.saturating_sub(earlier.unmapped),
      unreadable: self.unreadable.saturating_sub(earlier.unreadable),
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
    }
  }
}

impl CaptureStallWatch {
  pub fn new() -> Self {
    Self::default()
  }

  /// Takes the counts at `now`; a change, when the state changed.
  pub fn observe(&mut self, stats: BufferCounts, now: Instant) -> Option<StallChange> {
    self.samples.push_back((now, stats));
    while self
      .samples
      .get(1)
      .is_some_and(|(at, _)| now.saturating_duration_since(*at) >= STALL_WINDOW)
    {
      self.samples.pop_front();
    }
    let (first_at, first) = *self.samples.front()?;
    if now.saturating_duration_since(first_at) < STALL_WINDOW {
      return None;
    }
    let window = stats.since(&first);
    let unusable = window.unusable();
    match self.is_stalled {
      false if unusable >= MIN_UNUSABLE && unusable > window.pictures => {
        self.is_stalled = true;
        Some(StallChange::Stalled(window))
      },
      true if window.pictures > unusable => {
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
      if let Some(change) = watch.observe(health.stats(), start + TICK * tick) {
        changes.push((tick, change));
      }
    }
    let [(at, StallChange::Stalled(window))] = changes.as_slice() else {
      panic!("{changes:?}");
    };
    assert_eq!(*at, 50, "after the window");
    assert_eq!(window.unmapped, 150);
    assert_eq!(window.pictures, 0);
    assert_eq!(window.cursor_only, 50);
    assert!(watch.is_stalled());
    let mut recovered_at = None;
    for tick in 80..200u32 {
      for _ in 0..3 {
        health.record(BufferOutcome::Picture);
      }
      if watch.observe(health.stats(), start + TICK * tick) == Some(StallChange::Recovered) {
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
      assert_eq!(watch.observe(health.stats(), start + TICK * tick), None);
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
      assert_eq!(watch.observe(health.stats(), start + TICK * tick), None);
    }
  }
}
