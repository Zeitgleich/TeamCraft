//! QUIC's congestion control for the connection that carries the own voice alone (#2141, #1889).
//!
//! When an upload collapses (5 Mbit/s to 120 kbit/s from one second to the next), the video sent
//! for the old rate fills the bottleneck's queue: the voice connection sees its packets lost and
//! its round trip grow to seconds, and Cubic shrinks its window to two packets. Two packets per
//! round trip of two seconds are about 10 kbit/s, less than the thinnest voice: the voice waited
//! in QUIC for seconds after the video had stopped and the queue had drained, and arrived too late
//! to be played (the suite's `voice_collapse` measured 8–9 s of voice lost per collapse).
//!
//! A voice is a few dozen kbit/s that its codec paces, as the RTP voice of any call: it isn't the
//! flow that has to make room, and its encoder steps down on a thin link by itself
//! (`crate::voice_steps`). So its window never falls below [`MIN_VOICE_WINDOW_BYTES`]: enough for
//! the full voice across a round trip of a few seconds. Above it, Cubic decides as on any
//! connection.
//!
//! What waits for the window doesn't wait long either: the connection's datagram buffer is
//! [`VOICE_DATAGRAM_BUFFER_BYTES`], and QUIC drops the oldest datagram for a new one.

use std::any::Any;
use std::sync::Arc;
use std::time::Instant;

use quinn_proto::RttEstimator;
use wtransport::quinn::TransportConfig;
use wtransport::quinn::congestion::{Controller, ControllerFactory, ControllerMetrics, CubicConfig};

/// The voice connection's smallest window: a full voice (32 kbit/s of Opus and its headers, about
/// 60 kbit/s on the wire) across a round trip of three seconds.
pub const MIN_VOICE_WINDOW_BYTES: u64 = 24 * 1024;
/// QUIC's datagram buffer on the voice connection: about 200 ms of a full voice. A voice datagram
/// that would wait longer is no use: it would come too late to be played.
pub const VOICE_DATAGRAM_BUFFER_BYTES: usize = 2 * 1024;

/// Sets a voice connection's congestion control and datagram buffer (see the module docs).
pub fn set_voice_congestion(transport: &mut TransportConfig) {
  transport
    .congestion_controller_factory(Arc::new(VoiceCongestion::default()))
    .datagram_send_buffer_size(VOICE_DATAGRAM_BUFFER_BYTES);
}

/// Builds a [`VoiceWindow`] around Cubic.
#[derive(Debug, Default)]
pub struct VoiceCongestion {
  cubic: Arc<CubicConfig>,
}

/// Cubic, whose window never goes below [`MIN_VOICE_WINDOW_BYTES`].
struct VoiceWindow {
  cubic: Box<dyn Controller>,
}

impl ControllerFactory for VoiceCongestion {
  fn build(self: Arc<Self>, now: Instant, current_mtu: u16) -> Box<dyn Controller> {
    Box::new(VoiceWindow {
      cubic: Arc::clone(&self.cubic).build(now, current_mtu),
    })
  }
}

impl Controller for VoiceWindow {
  fn on_sent(&mut self, now: Instant, bytes: u64, last_packet_number: u64) {
    self.cubic.on_sent(now, bytes, last_packet_number);
  }

  fn on_ack(&mut self, now: Instant, sent: Instant, bytes: u64, app_limited: bool, rtt: &RttEstimator) {
    self.cubic.on_ack(now, sent, bytes, app_limited, rtt);
  }

  fn on_end_acks(
    &mut self,
    now: Instant,
    in_flight: u64,
    app_limited: bool,
    largest_packet_num_acked: Option<u64>,
  ) {
    self
      .cubic
      .on_end_acks(now, in_flight, app_limited, largest_packet_num_acked);
  }

  fn on_congestion_event(
    &mut self,
    now: Instant,
    sent: Instant,
    is_persistent_congestion: bool,
    lost_bytes: u64,
  ) {
    self
      .cubic
      .on_congestion_event(now, sent, is_persistent_congestion, lost_bytes);
  }

  fn on_mtu_update(&mut self, new_mtu: u16) {
    self.cubic.on_mtu_update(new_mtu);
  }

  fn window(&self) -> u64 {
    self.cubic.window().max(MIN_VOICE_WINDOW_BYTES)
  }

  fn metrics(&self) -> ControllerMetrics {
    let mut metrics = self.cubic.metrics();
    metrics.congestion_window = self.window();
    metrics
  }

  fn clone_box(&self) -> Box<dyn Controller> {
    Box::new(Self {
      cubic: self.cubic.clone_box(),
    })
  }

  fn initial_window(&self) -> u64 {
    self.cubic.initial_window().max(MIN_VOICE_WINDOW_BYTES)
  }

  fn into_any(self: Box<Self>) -> Box<dyn Any> {
    self
  }
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use super::*;

  const MTU: u16 = 1200;

  #[test]
  fn the_window_stays_above_the_floor_through_persistent_congestion() {
    let now = Instant::now();
    let mut window = Arc::new(VoiceCongestion::default()).build(now, MTU);
    for round in 0..10 {
      let at = now + Duration::from_secs(round);
      window.on_congestion_event(at, at, true, 10_000);
    }
    assert_eq!(window.window(), MIN_VOICE_WINDOW_BYTES);
    assert_eq!(window.metrics().congestion_window, MIN_VOICE_WINDOW_BYTES);
    // Cubic alone would be down to its minimum of two packets.
    let mut cubic = Arc::new(CubicConfig::default()).build(now, MTU);
    cubic.on_congestion_event(now, now, true, 10_000);
    assert!(cubic.window() < MIN_VOICE_WINDOW_BYTES);
  }

  #[test]
  fn above_the_floor_cubic_decides() {
    let now = Instant::now();
    let cubic_initial = Arc::new(CubicConfig::default()).build(now, MTU).initial_window();
    let window = Arc::new(VoiceCongestion::default()).build(now, MTU);
    assert_eq!(window.initial_window(), cubic_initial.max(MIN_VOICE_WINDOW_BYTES));
    assert_eq!(window.window(), window.initial_window());
    let copy = window.clone_box();
    assert_eq!(copy.window(), window.window());
  }
}
