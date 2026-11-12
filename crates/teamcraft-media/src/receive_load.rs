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
use crate::video::VideoResolution;

/// The received video's decoders against the frames they get, and the layers that leaves each
/// watched stream.
#[derive(Debug, Clone)]
pub struct ReceiveLoad {
  load: EncoderLoad,
  config: CameraGovernorConfig,
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
    }
  }

  /// A frame was handed to a decoder.
  pub fn on_frame(&mut self, frame: DecodedFrame) {
    self.load.on_frame(FrameTally {
      given: 1,
      skipped: 0,
      queued: frame.queued,
      encode_time: frame.late_by,
      frame_interval: frame.interval,
    });
  }

  /// Measures afresh from `now`: nothing was decoded for a while (no stream watched, the call
  /// paused), so the window says nothing of the load.
  pub fn measure_afresh(&mut self, now: Instant) {
    self.load.measure_afresh(now);
  }

  /// Judges the load once a window is whole; whether the layers of `streams` changed.
  pub fn evaluate(&mut self, streams: &[WatchedStream], now: Instant) -> bool {
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

    #[test]
    fn nothing_is_judged_before_a_whole_window() {
      let start = Instant::now();
      let mut load = ReceiveLoad::new(start);
      let streams = call();
      for _ in 0..30 {
        load.on_frame(overloaded());
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
        self.load.on_frame(frame);
      }
      self.load.evaluate(streams, self.now)
    }
  }

  /// A thumbnail (shown small, wants the middle of three layers), the stage camera (wants its
  /// top) and a screen share (its top).
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

  fn overloaded() -> DecodedFrame {
    DecodedFrame { queued: 3, ..calm() }
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
