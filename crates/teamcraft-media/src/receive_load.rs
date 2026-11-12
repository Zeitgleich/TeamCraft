//! Keeps the video this device receives within what it can decode (#1762).
//!
//! A machine that can't keep up with the video it receives (a loaded laptop, a VM next to a
//! build) let frames pile up in its decoders: the share froze and stuttered while the SFU went on
//! sending the largest layer, because nobody upstream learned of it. Part of what looked like
//! loss came from a starved receive path, not from the link.
//!
//! [`ReceiveLoad`] judges the decoders of all received streams together (they share the machine)
//! with the camera's judge, [`EncoderLoad`] (one implementation of the hysteresis, #1768): frames
//! that wait in a decoder's queue, or that reach the decoder late after their last packet
//! arrived. Overloaded, it asks the SFU for a lower layer of one stream per step, the least
//! important first: small camera tiles before the stage, cameras before a screen share. Calm for
//! [`CameraGovernorConfig::calm_period`], it steps back up in the reverse order; a step up that
//! doesn't hold doubles the wait.
//!
//! A decoder holds a frame or two in its pipeline while it keeps up: Firefox's WebCodecs decoder
//! reports 0.3 to 0.9 frames waiting at 480p on an idle machine, every window, and Chrome now and
//! then two. Only frames beyond [`DECODER_PIPELINE_FRAMES`] count as queued: measured as they
//! were, a Firefox viewer's 480p was never calm, and it stayed there through minutes of room on
//! the link (#1792). A decoder that falls behind piles up more than that within a window; one
//! whose frames come late against the frame interval is the other sign.
//!
//! Bursty arrival is the network's too (#1809): after a loss burst resent packets complete several
//! frames at once, the WebSocket fallback hands over a backlog after head-of-line blocking, a
//! handover ends with a burst. The decoder's queue then holds more than its pipeline for a moment
//! and empties it within a frame interval or two. So a queue counts only once it stayed above the
//! pipeline longer than a decoder that keeps the frame rate needs to drain its peak (then all of
//! it, from when it rose), and frames that came as a burst (closer than a quarter of the frame
//! interval to the one before) don't count toward lateness: they waited behind each other. A
//! decoder slower than the frame rate never drains: its queue counts within a window.
//!
//! Only this device's load counts here, never the network: a frame's lateness is measured from
//! the arrival of the packet that completed it, so a frame that waited for a resent packet isn't
//! late, and the step down limits only what this device asks for. Nothing of it reaches the
//! transport feedback or the senders' estimates, so a slow receiver costs nobody else quality.

use std::collections::BTreeMap;
use std::time::Duration;

use teamcraft_proto::{MediaStreamId, VideoTrackKind};
// `std::time::Instant` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

use crate::camera_governor::{CameraGovernorConfig, EncoderLoad, FrameTally, OverloadReason};
use crate::rtp::RtpTimestamp;
use crate::video::VideoResolution;

/// Frames a decoder that keeps up holds in its pipeline: waiting frames up to this many are no
/// sign of overload.
pub const DECODER_PIPELINE_FRAMES: u32 = 2;
/// The RTP video clock.
const RTP_VIDEO_CLOCK_HZ: u64 = 90_000;
/// A received stream's frame interval until two frames tell (30 fps).
const DEFAULT_FRAME_INTERVAL: Duration = Duration::from_micros(33_333);
/// Frame intervals outside these are gaps, not the stream's rate: a decoder's lateness is weighed
/// against the rate.
const MIN_FRAME_INTERVAL: Duration = Duration::from_millis(5);
const MAX_FRAME_INTERVAL: Duration = Duration::from_millis(500);

/// A frame that came closer than this share of the frame interval to the one before it came in
/// a burst.
const BURST_SPACING_DIVISOR: u32 = 4;

/// The received video's decoders against the frames they get, and the layers that leaves each
/// watched stream.
#[derive(Debug, Clone)]
pub struct ReceiveLoad {
  load: EncoderLoad,
  config: CameraGovernorConfig,
  /// Each decoding stream's recent frames: bursts and how long its queue stood above the pipeline.
  flows: BTreeMap<MediaStreamId, Flow>,
}

/// A received stream a tile shows, as the load's ladder sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedStream {
  pub stream_id: MediaStreamId,
  pub importance: Importance,
  /// The layer its tile and the viewer's quality choice ask for ([`crate::video::chosen_layer`]).
  pub wanted_layer: u8,
  /// The layers its sender offers, in any order; empty while unknown.
  pub offered_layers: Vec<u8>,
}

/// How much a stream matters to the viewer: a screen share more than any camera, a larger tile
/// more than a smaller one. The least important stream is stepped down first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Importance {
  is_screen: bool,
  shown_pixels: u32,
}

/// A frame handed to a decoder.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DecodedFrame {
  /// Frames already waiting in the decoder (WebCodecs `decodeQueueSize`).
  pub queued: u32,
  /// How long after the arrival of the packet that completed it the frame reached the decoder:
  /// the receive path's own delay, without the network's.
  pub late_by: Duration,
  /// The stream's frame interval, which [`Self::late_by`] is measured against.
  pub interval: Duration,
}

/// One stream's frames as they reached its decoder (#1809).
#[derive(Debug, Clone, Copy, Default)]
struct Flow {
  maybe_last_at: Option<Instant>,
  /// Since when its queue stands above the pipeline, and the most frames beyond it meanwhile.
  maybe_backlog: Option<(Instant, u32)>,
  /// Frames beyond the pipeline not counted yet: they count once the queue didn't drain in time,
  /// and are forgotten when it did.
  withheld: u32,
}

/// A received stream's frame interval, from its frames' RTP timestamps: what
/// [`DecodedFrame::interval`] weighs the lateness against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameInterval {
  maybe_last: Option<RtpTimestamp>,
  interval: Duration,
}

/// Why the receiving decoders were last found overloaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiveOverload {
  /// Frames waited in the decoders' queues.
  QueuedFrames,
  /// Frames reached the decoders late: the page's receive path didn't keep up.
  LateFrames,
}

impl ReceiveLoad {
  pub fn new(now: Instant) -> Self {
    Self::with_config(CameraGovernorConfig::default(), now)
  }

  pub fn with_config(config: CameraGovernorConfig, now: Instant) -> Self {
    Self {
      load: EncoderLoad::with_levels(1, now),
      config,
      flows: BTreeMap::new(),
    }
  }

  /// A frame of `stream_id` was handed to its decoder at `now`. Of the frames waiting in it, only
  /// those beyond its pipeline ([`DECODER_PIPELINE_FRAMES`]) count, and only once the queue had
  /// time to drain (#1809); a frame that came in a burst doesn't count toward lateness.
  pub fn on_frame(&mut self, stream_id: MediaStreamId, frame: DecodedFrame, now: Instant) {
    let flow = self.flows.entry(stream_id).or_default();
    let is_burst = flow
      .maybe_last_at
      .replace(now)
      .is_some_and(|last| now.saturating_duration_since(last) < frame.interval / BURST_SPACING_DIVISOR);
    let queued = flow.backlog(frame, now);
    let (encode_time, frame_interval) = if is_burst {
      (Duration::ZERO, Duration::ZERO)
    } else {
      (frame.late_by, frame.interval)
    };
    self.load.on_frame(FrameTally {
      given: 1,
      skipped: 0,
      queued,
      encode_time,
      frame_interval,
    });
  }

  /// Measures afresh from `now`: nothing was decoded for a while (no stream watched, the call
  /// paused), so the window says nothing of the load.
  pub fn measure_afresh(&mut self, now: Instant) {
    self.load.measure_afresh(now);
    self.flows.clear();
  }

  /// Judges the load once a window is whole; whether the layers of `streams` changed.
  pub fn evaluate(&mut self, streams: &[WatchedStream], now: Instant) -> bool {
    // Streams no longer watched decode nothing more: their flows would only grow the map.
    self
      .flows
      .retain(|stream_id, _| streams.iter().any(|stream| stream.stream_id == *stream_id));
    self.load.set_level_count(step_count(streams) + 1);
    self.load.evaluate(now, &self.config).is_some()
  }

  /// Whether this device holds some stream below what its tile asks for.
  pub fn is_limited(&self) -> bool {
    self.load.level() > 0
  }

  /// Why the decoders were last found overloaded, while limited.
  pub fn maybe_reason(&self) -> Option<ReceiveOverload> {
    self.load.maybe_reason().map(|reason| match reason {
      OverloadReason::SkippedFrames | OverloadReason::QueuedFrames => ReceiveOverload::QueuedFrames,
      OverloadReason::SlowEncoding => ReceiveOverload::LateFrames,
    })
  }

  /// The layer to ask for of each of `streams`: what it wants, less the steps taken, the least
  /// important stream first. A stream whose layers are unknown keeps what it wants.
  pub fn layers(&self, streams: &[WatchedStream]) -> BTreeMap<MediaStreamId, u8> {
    let mut ladders: Vec<Ladder<'_>> = streams.iter().map(Ladder::new).collect();
    ladders.sort_by_key(|ladder| (ladder.stream.importance, ladder.stream.stream_id));
    let mut steps = self.load.level();
    for ladder in &mut ladders {
      let taken = steps.min(ladder.position);
      ladder.position -= taken;
      steps -= taken;
    }
    ladders
      .iter()
      .map(|ladder| (ladder.stream.stream_id, ladder.layer()))
      .collect()
  }
}

impl WatchedStream {
  /// The layer it gets while this device keeps up: the highest offered one up to what it
  /// wants.
  pub fn unlimited_layer(&self) -> u8 {
    Ladder::new(self).layer()
  }
}

impl Flow {
  /// The frames beyond the pipeline that count as queued. While the queue is new enough that a
  /// decoder keeping the frame rate could still drain its peak, they are held back; once it
  /// didn't, they count with the frame's own.
  fn backlog(&mut self, frame: DecodedFrame, now: Instant) -> u32 {
    let excess = frame.queued.saturating_sub(DECODER_PIPELINE_FRAMES);
    if excess == 0 {
      self.maybe_backlog = None;
      self.withheld = 0;
      return 0;
    }
    let (since, peak) = self.maybe_backlog.get_or_insert((now, excess));
    *peak = (*peak).max(excess);
    let to_drain = frame.interval.saturating_mul(*peak);
    if now.saturating_duration_since(*since) < to_drain {
      self.withheld = self.withheld.saturating_add(excess);
      0
    } else {
      excess.saturating_add(std::mem::take(&mut self.withheld))
    }
  }
}

impl FrameInterval {
  /// The interval before the frame at `timestamp`, from the one before it: what a decoder has
  /// for it. Gaps (a lost frame, a paused sender) don't count.
  pub fn next(&mut self, timestamp: RtpTimestamp) -> Duration {
    if let Some(last) = self.maybe_last.replace(timestamp) {
      let ticks = u64::from(timestamp.0.wrapping_sub(last.0));
      let interval = Duration::from_micros(ticks * 1_000_000 / RTP_VIDEO_CLOCK_HZ);
      if (MIN_FRAME_INTERVAL..=MAX_FRAME_INTERVAL).contains(&interval) {
        self.interval = interval;
      }
    }
    self.interval
  }
}

impl Default for FrameInterval {
  fn default() -> Self {
    Self {
      maybe_last: None,
      interval: DEFAULT_FRAME_INTERVAL,
    }
  }
}

impl Importance {
  pub fn new(kind: &VideoTrackKind, shown: VideoResolution) -> Self {
    Self {
      is_screen: *kind == VideoTrackKind::Screen,
      shown_pixels: u32::from(shown.width) * u32::from(shown.height),
    }
  }
}

/// A stream's offered layers, lowest first, and where on them it stands.
struct Ladder<'a> {
  stream: &'a WatchedStream,
  layers: Vec<u8>,
  /// Index into [`Self::layers`]: the layer asked for.
  position: usize,
}

impl<'a> Ladder<'a> {
  fn new(stream: &'a WatchedStream) -> Self {
    let mut layers = stream.offered_layers.clone();
    layers.sort_unstable();
    layers.dedup();
    let position = layers
      .iter()
      .rposition(|layer| *layer <= stream.wanted_layer)
      .unwrap_or(0);
    Self {
      stream,
      layers,
      position,
    }
  }

  fn layer(&self) -> u8 {
    self
      .layers
      .get(self.position)
      .copied()
      .unwrap_or(self.stream.wanted_layer)
  }
}

/// How many steps down `streams` have between them: each one's layers below the one it wants.
fn step_count(streams: &[WatchedStream]) -> usize {
  streams.iter().map(|stream| Ladder::new(stream).position).sum()
}

#[cfg(test)]
mod tests {
  use super::*;

  const THUMBNAIL: MediaStreamId = MediaStreamId(1);
  const STAGE: MediaStreamId = MediaStreamId(2);
  const SHARE: MediaStreamId = MediaStreamId(3);
  const FRAME_INTERVAL: Duration = Duration::from_millis(33);

  mod detection {
    use super::*;

    /// Hamburg's share (#1762): frames queue in the decoder for a whole window, so the least
    /// important stream gets a lower layer.
    #[test]
    fn frames_queued_in_the_decoders_step_down() {
      let mut clock = Clock::new();
      let streams = call();
      clock.run(&streams, overloaded());
      assert!(clock.load.is_limited());
      assert_eq!(clock.load.maybe_reason(), Some(ReceiveOverload::QueuedFrames));
    }

    #[test]
    fn frames_that_reach_the_decoder_late_step_down() {
      let mut clock = Clock::new();
      let streams = call();
      clock.run(&streams, starved());
      assert_eq!(clock.load.maybe_reason(), Some(ReceiveOverload::LateFrames));
    }

    #[test]
    fn decoders_that_keep_up_change_nothing() {
      let mut clock = Clock::new();
      let streams = call();
      for _ in 0..30 {
        assert!(!clock.run(&streams, calm()));
      }
      assert_eq!(clock.load.layers(&streams), wanted(&streams));
    }

    /// The network's delay isn't the receive path's: a frame that waited for a resent packet
    /// reaches the decoder right after that packet came, so it isn't late, however long the
    /// loss held it up.
    #[test]
    fn a_frame_held_up_by_the_network_is_not_late() {
      let mut clock = Clock::new();
      let streams = call();
      let after_a_resend = DecodedFrame {
        late_by: Duration::from_millis(2),
        ..calm()
      };
      for _ in 0..10 {
        assert!(!clock.run(&streams, after_a_resend));
      }
      assert!(!clock.load.is_limited());
    }

    /// #1792: Firefox's decoder holds one or two frames all the time on an idle machine. That
    /// is its pipeline, not a queue: a viewer held down by a moment of load steps back up within
    /// two calm windows, and stays.
    #[test]
    fn a_steady_pipeline_of_one_or_two_frames_is_calm() {
      let mut clock = Clock::new();
      let streams = call();
      assert!(clock.run(&streams, overloaded()));
      let pipeline = |queued: u32| DecodedFrame { queued, ..calm() };
      let calm_from = clock.now;
      let mut steps = 0;
      for round in 0..6u32 {
        steps += usize::from(clock.run(&streams, pipeline(1 + round % 2)));
      }
      assert!(
        !clock.load.is_limited(),
        "still limited after {:?}",
        clock.now - calm_from
      );
      assert_eq!(steps, 1);
      assert_eq!(clock.load.layers(&streams), wanted(&streams));
    }

    /// A decoder whose output falls behind its input: the queue grows frame by frame, and the
    /// least important stream steps down after the window.
    #[test]
    fn a_growing_queue_steps_down() {
      let start = Instant::now();
      let mut load = ReceiveLoad::new(start);
      let streams = call();
      let mut now = start;
      for frame in 0..61u32 {
        now += FRAME_INTERVAL;
        load.on_frame(
          STAGE,
          DecodedFrame {
            queued: frame / 6,
            ..calm()
          },
          now,
        );
      }
      assert!(load.evaluate(&streams, now));
      assert_eq!(load.maybe_reason(), Some(ReceiveOverload::QueuedFrames));
    }

    /// #1809: a lossy or fallback link hands frames over in bursts: ten after a 300 ms gap,
    /// every 2 s. A decoder faster than the frame rate empties them within a few intervals, so
    /// neither the queue they leave for a moment nor the wait behind each other is overload.
    #[test]
    fn bursts_that_a_fast_decoder_drains_change_nothing() {
      let start = Instant::now();
      let mut load = ReceiveLoad::new(start);
      let streams = call();
      let mut decoder = Decoder::new(Duration::from_millis(8));
      let mut now = start;
      for _ in 0..10 {
        // A second and a half of steady frames, then a gap and the held frames at once.
        for _ in 0..51 {
          now += FRAME_INTERVAL;
          load.on_frame(STAGE, decoder.hand_over(now, Duration::ZERO), now);
        }
        now += Duration::from_millis(300);
        for index in 0..10u32 {
          let at = now + Duration::from_millis(u64::from(index));
          // Each waited for the ones before it in the receive path.
          let late_by = Duration::from_millis(u64::from(index) * 3);
          load.on_frame(STAGE, decoder.hand_over(at, late_by), at);
        }
        now += Duration::from_millis(10);
        assert!(!load.evaluate(&streams, now), "limited at {:?}", now - start);
      }
      assert!(!load.is_limited());
    }

    /// #1809: a decoder slower than the frame rate never drains its queue, bursts or not: it is
    /// limited within two windows.
    #[test]
    fn a_decoder_slower_than_the_frame_rate_steps_down_within_two_windows() {
      let start = Instant::now();
      let mut load = ReceiveLoad::new(start);
      let streams = call();
      let mut decoder = Decoder::new(Duration::from_millis(40));
      let mut now = start;
      let mut window_end = start + CameraGovernorConfig::default().load_window;
      let mut windows = 0;
      while !load.is_limited() && windows < 2 {
        now += FRAME_INTERVAL;
        load.on_frame(STAGE, decoder.hand_over(now, Duration::ZERO), now);
        if now >= window_end {
          load.evaluate(&streams, now);
          windows += 1;
          window_end = now + CameraGovernorConfig::default().load_window;
        }
      }
      assert!(load.is_limited(), "not limited after {windows} windows");
      assert_eq!(load.maybe_reason(), Some(ReceiveOverload::QueuedFrames));
    }

    #[test]
    fn nothing_is_judged_before_a_whole_window() {
      let start = Instant::now();
      let mut load = ReceiveLoad::new(start);
      let streams = call();
      let mut now = start;
      for _ in 0..30 {
        now += FRAME_INTERVAL;
        load.on_frame(STAGE, overloaded(), now);
      }
      assert!(!load.evaluate(&streams, start + Duration::from_secs(1)));
    }
  }

  mod stepping {
    use super::*;

    /// The thumbnail goes down first, then the stage camera, the share last; one layer per step.
    #[test]
    fn the_least_important_stream_steps_down_first() {
      let mut clock = Clock::new();
      let streams = call();
      let mut seen = Vec::new();
      while clock.run(&streams, overloaded()) {
        let layers = clock.load.layers(&streams);
        seen.push((layers[&THUMBNAIL], layers[&STAGE], layers[&SHARE]));
      }
      assert_eq!(seen, vec![(0, 2, 2), (0, 1, 2), (0, 0, 2), (0, 0, 1), (0, 0, 0)]);
      // All at their lowest: nothing more to give.
      assert!(!clock.run(&streams, overloaded()));
    }

    #[test]
    fn a_stream_without_known_layers_keeps_what_it_wants() {
      let mut clock = Clock::new();
      let streams = vec![stream(STAGE, &VideoTrackKind::Camera, 1280, 720, 2, &[])];
      assert!(!clock.run(&streams, overloaded()));
      assert_eq!(clock.load.layers(&streams)[&STAGE], 2);
    }

    /// A stream that stops being watched takes its steps along: the level never points past
    /// the ladder that is left.
    #[test]
    fn fewer_streams_shorten_the_ladder() {
      let mut clock = Clock::new();
      let streams = call();
      for _ in 0..4 {
        clock.run(&streams, overloaded());
      }
      let left = vec![streams[2].clone()];
      clock.run(&left, overloaded());
      assert_eq!(clock.load.layers(&left)[&SHARE], 0);
      assert!(clock.load.is_limited());
    }
  }

  mod recovery {
    use super::*;

    /// After a calm period it steps back up in the reverse order: the share first.
    #[test]
    fn a_calm_device_steps_back_up_in_reverse_order() {
      let mut clock = Clock::new();
      let streams = call();
      while clock.run(&streams, overloaded()) {}
      let mut seen = Vec::new();
      for _ in 0..60 {
        if clock.run(&streams, calm()) {
          let layers = clock.load.layers(&streams);
          seen.push((layers[&THUMBNAIL], layers[&STAGE], layers[&SHARE]));
        }
      }
      assert_eq!(seen, vec![(0, 0, 1), (0, 0, 2), (0, 1, 2), (0, 2, 2), (1, 2, 2)]);
      assert!(!clock.load.is_limited());
      assert_eq!(clock.load.maybe_reason(), None);
    }

    /// Hysteresis: a step up that overloads again at once is taken back, and the next try
    /// waits twice as long.
    #[test]
    fn a_step_up_that_fails_doubles_the_wait() {
      let config = CameraGovernorConfig::default();
      let mut clock = Clock::new();
      let streams = call();
      clock.run(&streams, overloaded());
      let mut calm_windows = 0;
      while !clock.run(&streams, calm()) {
        calm_windows += 1;
      }
      assert!(!clock.load.is_limited());
      assert!(clock.run(&streams, overloaded()));
      let mut calm_windows_again = 0;
      while !clock.run(&streams, calm()) {
        calm_windows_again += 1;
      }
      let window = config.load_window.as_secs();
      assert!(
        (calm_windows_again + 1) * window >= 2 * config.calm_period.as_secs(),
        "waited {calm_windows_again} windows after a failed probe, {calm_windows} before"
      );
    }
  }

  /// A decoder that takes `decode_time` per frame, one after the other: how many frames wait in
  /// it when the next is handed over.
  struct Decoder {
    decode_time: Duration,
    done_at: std::collections::VecDeque<Instant>,
  }

  impl Decoder {
    fn new(decode_time: Duration) -> Self {
      Self {
        decode_time,
        done_at: std::collections::VecDeque::new(),
      }
    }

    fn hand_over(&mut self, now: Instant, late_by: Duration) -> DecodedFrame {
      while self.done_at.front().is_some_and(|done| *done <= now) {
        self.done_at.pop_front();
      }
      let queued = u32::try_from(self.done_at.len()).unwrap();
      let start = self.done_at.back().copied().unwrap_or(now).max(now);
      self.done_at.push_back(start + self.decode_time);
      DecodedFrame {
        queued,
        late_by,
        interval: FRAME_INTERVAL,
      }
    }
  }

  /// Feeds a window of frames at 30 fps, then judges it.
  struct Clock {
    now: Instant,
    load: ReceiveLoad,
  }

  impl Clock {
    fn new() -> Self {
      let now = Instant::now();
      Self {
        now,
        load: ReceiveLoad::new(now),
      }
    }

    fn run(&mut self, streams: &[WatchedStream], frame: DecodedFrame) -> bool {
      let end = self.now + CameraGovernorConfig::default().load_window;
      while self.now < end {
        self.now += FRAME_INTERVAL;
        self.load.on_frame(STAGE, frame, self.now);
      }
      self.load.evaluate(streams, self.now)
    }
  }

  /// A thumbnail (shown small, wants the middle of three layers), the stage camera (wants its
  /// top) and a screen share (its top).
  #[test]
  fn the_frame_interval_follows_the_timestamps_and_skips_gaps() {
    let mut interval = FrameInterval::default();
    assert_eq!(interval.next(RtpTimestamp(0)), DEFAULT_FRAME_INTERVAL);
    assert_eq!(interval.next(RtpTimestamp(6_000)), Duration::from_micros(66_666));
    // A paused sender's gap of five seconds isn't the rate.
    assert_eq!(
      interval.next(RtpTimestamp(456_000)),
      Duration::from_micros(66_666)
    );
    // The 32-bit clock wraps.
    let mut wrapping = FrameInterval::default();
    wrapping.next(RtpTimestamp(u32::MAX - 999));
    assert_eq!(wrapping.next(RtpTimestamp(2_000)), Duration::from_micros(33_333));
  }

  fn call() -> Vec<WatchedStream> {
    vec![
      stream(THUMBNAIL, &VideoTrackKind::Camera, 320, 180, 1, &[0, 1, 2]),
      stream(STAGE, &VideoTrackKind::Camera, 1280, 720, 2, &[0, 1, 2]),
      stream(SHARE, &VideoTrackKind::Screen, 640, 360, u8::MAX, &[0, 1, 2]),
    ]
  }

  fn stream(
    stream_id: MediaStreamId,
    kind: &VideoTrackKind,
    width: u16,
    height: u16,
    wanted_layer: u8,
    offered_layers: &[u8],
  ) -> WatchedStream {
    WatchedStream {
      stream_id,
      importance: Importance::new(kind, VideoResolution { width, height }),
      wanted_layer,
      offered_layers: offered_layers.to_vec(),
    }
  }

  fn wanted(streams: &[WatchedStream]) -> BTreeMap<MediaStreamId, u8> {
    streams
      .iter()
      .map(|stream| (stream.stream_id, stream.unlimited_layer()))
      .collect()
  }

  /// A decoder that fell behind: frames pile up far beyond its pipeline.
  fn overloaded() -> DecodedFrame {
    DecodedFrame { queued: 5, ..calm() }
  }

  fn starved() -> DecodedFrame {
    DecodedFrame {
      late_by: Duration::from_millis(60),
      ..calm()
    }
  }

  fn calm() -> DecodedFrame {
    DecodedFrame {
      queued: 0,
      late_by: Duration::ZERO,
      interval: FRAME_INTERVAL,
    }
  }
}
