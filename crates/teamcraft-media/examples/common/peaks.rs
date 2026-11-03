//! A layer's short bitrate peaks against its target (issue #1005), for the benches
//! (`screen_bench`, `camera_bitrate`): its largest picture and its busiest 100 ms and 1 s, as a
//! multiple of what the target sends in that time, and what a model link with a little headroom
//! and a router's queue makes of them.

use std::time::Duration;

/// The windows the peaks are measured over.
const PEAK_WINDOWS: [Duration; 2] = [Duration::from_millis(100), Duration::from_secs(1)];
/// The model link of the peaks report: it drains at this multiple of the layer's target and
/// queues [`LINK_BUFFER`] of its own time before it drops (a router's buffer).
const LINK_FACTOR: f64 = 1.25;
const LINK_BUFFER: Duration = Duration::from_millis(250);

/// The largest picture and the busiest windows of [`PEAK_WINDOWS`], each as a multiple of what
/// the target sends in the same time (a picture: in one frame time).
pub fn peaks_report(sizes: &[(u64, usize)], target_bps: u32, framerate: u8) -> String {
  let bytes_per_second = f64::from(target_bps) / 8.0;
  let largest = sizes.iter().map(|(_, bytes)| *bytes).max().unwrap_or(0) as f64;
  let mut report = format!(
    "peak picture {:.1}x",
    largest / (bytes_per_second / f64::from(framerate.max(1)))
  );
  let (longest_wait, dropped) = link_waits(sizes, bytes_per_second * LINK_FACTOR);
  report += &format!(
    "; link {LINK_FACTOR}x: longest wait {} ms, {dropped} of {} pictures dropped",
    longest_wait.as_millis(),
    sizes.len()
  );
  for window in PEAK_WINDOWS {
    report += &format!(
      ", {} ms {:.2}x",
      window.as_millis(),
      busiest_window(sizes, window) / (bytes_per_second * window.as_secs_f64())
    );
  }
  report
}

/// How long the longest-waiting picture queues on a link draining `link_bytes_per_second`, and
/// how many pictures it drops because the queue is full ([`LINK_BUFFER`]).
fn link_waits(sizes: &[(u64, usize)], link_bytes_per_second: f64) -> (Duration, usize) {
  let (mut queue, mut previous_us, mut longest, mut dropped) = (0.0f64, 0u64, 0.0f64, 0usize);
  for (at, bytes) in sizes {
    let elapsed = at.saturating_sub(previous_us) as f64 / 1e6;
    previous_us = *at;
    queue = (queue - link_bytes_per_second * elapsed).max(0.0);
    if queue + *bytes as f64 > link_bytes_per_second * LINK_BUFFER.as_secs_f64() {
      dropped += 1;
      continue;
    }
    queue += *bytes as f64;
    longest = longest.max(queue / link_bytes_per_second);
  }
  (Duration::from_secs_f64(longest), dropped)
}

/// The most bytes pictures captured within one `window` add up to.
fn busiest_window(sizes: &[(u64, usize)], window: Duration) -> f64 {
  let window_us = window.as_micros() as u64;
  let (mut start, mut sum, mut best) = (0usize, 0usize, 0usize);
  for (at, bytes) in sizes {
    sum += bytes;
    while sizes[start].0 + window_us <= *at {
      sum -= sizes[start].1;
      start += 1;
    }
    best = best.max(sum);
  }
  best as f64
}
