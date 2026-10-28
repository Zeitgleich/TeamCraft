//! Which simulcast layers of this device's own video tracks the uplink can carry.
//!
//! Audio comes first: when the uplink estimate drops (a congested or lossy mobile network), the
//! highest video layers stop before audio suffers, down to no video at all. Screen shares go before
//! cameras. The estimate's start value is a guess, so layers are only limited once the estimator
//! has actually cut the rate; layers come back with a margin so the allocation doesn't flap. A new
//! own track starts the same way ([`UplinkLayers::measure_afresh`]): a cut while the call sent
//! less (only sound) doesn't hold its layers back (#935). A screen share keeps its layers through
//! a short dip ([`UplinkPolicy::screen_downswitch_hold`]), as at the SFU (#941).
//!
//! Pure policy: the call engine feeds estimates and its tracks, and asks which layers may be sent.

use std::collections::HashMap;
use std::time::Duration;

use teamcraft_proto::{VideoLayerInfo, VideoTrackKind};
// `std::time::Instant` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

use crate::datagram::StreamId;

/// Limits of the uplink allocation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UplinkPolicy {
  /// Kept free for the microphone: Opus at 32 kbit/s with RTP, DAVE and QUIC overhead at 50
  /// packets per second.
  pub audio_reserve_bps: u32,
  /// Kept free for a shared screen's sound while it is sent (64 kbit/s Opus plus overhead).
  pub screen_audio_reserve_bps: u32,
  /// A layer comes back only when the estimate exceeds what it needs by this factor.
  pub upgrade_margin: f64,
  /// How long the estimate has to stay below a screen share's layers before one stops. A still
  /// screen sends almost nothing but keyframes of hundreds of kilobytes, and the estimate reacts
  /// to such a burst with a cut it recovers from within seconds: stopping the sharp layer at
  /// once sent its viewers back to the blurred half-size picture right after the sharp one
  /// arrived (issue #941). An uplink that really can't carry the share stays below for longer.
  pub screen_downswitch_hold: Duration,
}

/// One own video track as the allocation sees it.
#[derive(Debug, Clone, Copy)]
pub struct OwnTrack<'a> {
  pub stream: StreamId,
  pub kind: &'a VideoTrackKind,
  pub layers: &'a [VideoLayerInfo],
}

/// The current allocation: per track the highest layer that may be sent, `None` for paused.
#[derive(Debug)]
pub struct UplinkLayers {
  /// Set once the estimator cut the rate; until then every layer is sent.
  is_limited: bool,
  last_estimate_bps: u32,
  top_layers: HashMap<StreamId, Option<u8>>,
  /// Since when the estimate is below a screen share's current layers.
  downgrade_pending_since: HashMap<StreamId, Instant>,
}

impl Default for UplinkPolicy {
  fn default() -> Self {
    Self {
      audio_reserve_bps: 48_000,
      screen_audio_reserve_bps: 80_000,
      upgrade_margin: 1.15,
      screen_downswitch_hold: Duration::from_secs(3),
    }
  }
}

impl UplinkLayers {
  /// `start_bps`: the estimator's start value; falling below it is the first cut.
  pub fn new(start_bps: u32) -> Self {
    Self {
      is_limited: false,
      last_estimate_bps: start_bps,
      top_layers: HashMap::new(),
      downgrade_pending_since: HashMap::new(),
    }
  }

  /// Applies a new estimate at `now`. Returns whether the allocation changed.
  pub fn on_estimate(
    &mut self,
    estimate_bps: u32,
    tracks: &[OwnTrack<'_>],
    is_sending_screen_audio: bool,
    policy: &UplinkPolicy,
    now: Instant,
  ) -> bool {
    let has_dropped = estimate_bps < self.last_estimate_bps;
    self.last_estimate_bps = estimate_bps;
    let full_demand = demand_bps(tracks, is_sending_screen_audio, policy);
    if has_dropped && estimate_bps < full_demand {
      self.is_limited = true;
    } else if estimate_bps >= full_demand {
      self.is_limited = false;
    }
    let before: Vec<Option<u8>> = tracks.iter().map(|track| self.top_of(track)).collect();
    self.allocate(estimate_bps, tracks, is_sending_screen_audio, policy, now);
    tracks
      .iter()
      .zip(before)
      .any(|(track, top)| self.top_of(track) != top)
  }

  /// The estimator measures afresh from `estimate_bps` (a new own track,
  /// [`crate::bwe::SendSideEstimator::measure_afresh`]): as at the start, every layer is sent
  /// until it cuts the rate again. A limit found while the call sent less said nothing about the
  /// new track: a camera turned on after a cut while only sound went out had every layer held
  /// back, and nothing was sent until it was given up on (#935).
  pub fn measure_afresh(&mut self, estimate_bps: u32) {
    self.is_limited = false;
    self.last_estimate_bps = estimate_bps;
    self.top_layers.clear();
    self.downgrade_pending_since.clear();
  }

  /// Whether `layer` of `stream` may be sent. Tracks the allocation hasn't seen are not limited.
  pub fn allows(&self, stream: StreamId, layer: u8) -> bool {
    match self.top_layers.get(&stream) {
      None => true,
      Some(maybe_top) => maybe_top.is_some_and(|top| layer <= top),
    }
  }

  /// Forgets a track that is no longer published.
  pub fn remove(&mut self, stream: StreamId) {
    self.top_layers.remove(&stream);
    self.downgrade_pending_since.remove(&stream);
  }

  /// The highest layer of `track` that may be sent; tracks not allocated yet send everything.
  fn top_of(&self, track: &OwnTrack<'_>) -> Option<u8> {
    match self.top_layers.get(&track.stream) {
      Some(maybe_top) => *maybe_top,
      None => top_index(track.layers),
    }
  }

  fn allocate(
    &mut self,
    estimate_bps: u32,
    tracks: &[OwnTrack<'_>],
    is_sending_screen_audio: bool,
    policy: &UplinkPolicy,
    now: Instant,
  ) {
    if !self.is_limited {
      self.top_layers = tracks
        .iter()
        .map(|track| (track.stream, top_index(track.layers)))
        .collect();
      self.downgrade_pending_since.clear();
      return;
    }
    let reserve = policy.audio_reserve_bps
      + if is_sending_screen_audio {
        policy.screen_audio_reserve_bps
      } else {
        0
      };
    let mut budget = f64::from(estimate_bps.saturating_sub(reserve));
    let mut ordered: Vec<&OwnTrack<'_>> = tracks.iter().collect();
    // Screen shares first: their content (text) is what people are looking at.
    ordered.sort_by_key(|track| match track.kind {
      VideoTrackKind::Screen => 0,
      _ => 1,
    });
    let mut allocation = HashMap::new();
    for track in ordered {
      let maybe_current = self.top_layers.get(&track.stream).copied().flatten();
      let mut maybe_top = None;
      let mut cumulative = 0.0;
      for info in track.layers {
        cumulative += f64::from(info.max_bitrate_bps);
        // Keeping a layer needs it to fit; getting it back needs the margin on top.
        let is_kept = maybe_current.is_some_and(|current| info.layer <= current);
        let needed = if is_kept {
          cumulative
        } else {
          cumulative * policy.upgrade_margin
        };
        if needed > budget {
          break;
        }
        maybe_top = Some(info.layer);
      }
      let maybe_top = self.after_screen_hold(track, maybe_current, maybe_top, policy, now);
      let used = track
        .layers
        .iter()
        .filter(|info| maybe_top.is_some_and(|top| info.layer <= top))
        .map(|info| f64::from(info.max_bitrate_bps))
        .sum::<f64>();
      budget -= used;
      allocation.insert(track.stream, maybe_top);
    }
    self.top_layers = allocation;
    self
      .downgrade_pending_since
      .retain(|stream, _| tracks.iter().any(|track| track.stream == *stream));
  }

  /// `maybe_top` for `track`, unless it is a screen share whose layers drop below
  /// `maybe_current` for less than [`UplinkPolicy::screen_downswitch_hold`]: then
  /// `maybe_current`.
  fn after_screen_hold(
    &mut self,
    track: &OwnTrack<'_>,
    maybe_current: Option<u8>,
    maybe_top: Option<u8>,
    policy: &UplinkPolicy,
    now: Instant,
  ) -> Option<u8> {
    let rank = |maybe_layer: Option<u8>| maybe_layer.map_or(-1, i16::from);
    let is_screen = *track.kind == VideoTrackKind::Screen;
    let is_still_there =
      maybe_current.is_some_and(|current| track.layers.iter().any(|info| info.layer == current));
    if !is_screen || !is_still_there || rank(maybe_top) >= rank(maybe_current) {
      self.downgrade_pending_since.remove(&track.stream);
      return maybe_top;
    }
    let since = *self.downgrade_pending_since.entry(track.stream).or_insert(now);
    if now.saturating_duration_since(since) < policy.screen_downswitch_hold {
      return maybe_current;
    }
    self.downgrade_pending_since.remove(&track.stream);
    maybe_top
  }
}

/// What sending everything takes: audio plus every layer of every own track. Lets the estimator
/// probe up to it while layers are held back.
pub fn demand_bps(tracks: &[OwnTrack<'_>], is_sending_screen_audio: bool, policy: &UplinkPolicy) -> u32 {
  let video: u32 = tracks
    .iter()
    .flat_map(|track| track.layers.iter())
    .map(|info| info.max_bitrate_bps)
    .fold(0, u32::saturating_add);
  let audio = policy.audio_reserve_bps
    + if is_sending_screen_audio {
      policy.screen_audio_reserve_bps
    } else {
      0
    };
  video.saturating_add(audio)
}

fn top_index(layers: &[VideoLayerInfo]) -> Option<u8> {
  layers.iter().map(|info| info.layer).max()
}

#[cfg(test)]
mod tests {
  use super::*;

  const CAMERA: StreamId = StreamId(10);
  const SCREEN: StreamId = StreamId(11);

  #[test]
  fn everything_is_sent_until_the_estimate_drops() {
    let layers = camera_layers();
    let tracks = [camera(&layers)];
    let mut uplink = UplinkLayers::new(300_000);
    // The start estimate is below the full demand, but it is only a guess.
    assert!(!uplink.on_estimate(300_000, &tracks, false, &policy(), now()));
    assert!(uplink.allows(CAMERA, 2));
    uplink.on_estimate(400_000, &tracks, false, &policy(), now());
    assert!(uplink.allows(CAMERA, 2));
  }

  #[test]
  fn a_drop_keeps_audio_and_the_layers_that_fit() {
    let layers = camera_layers();
    let tracks = [camera(&layers)];
    let mut uplink = UplinkLayers::new(300_000);
    uplink.on_estimate(2_000_000, &tracks, false, &policy(), now());
    // 48k audio + 150k + 500k fit into 800k, the 1.5M layer doesn't.
    assert!(uplink.on_estimate(800_000, &tracks, false, &policy(), now()));
    assert!(uplink.allows(CAMERA, 1));
    assert!(!uplink.allows(CAMERA, 2));
    // Too little for even the lowest layer: video pauses, audio goes on.
    uplink.on_estimate(150_000, &tracks, false, &policy(), now());
    assert!(!uplink.allows(CAMERA, 0));
  }

  /// Issue #935: a cut while only sound went out held every layer of a camera turned on later
  /// back; measured afresh for the new track, it starts with every layer, and only a cut with the
  /// camera sending limits it again.
  #[test]
  fn a_new_track_measured_afresh_is_not_held_back_by_a_cut_while_sending_less() {
    let layers = camera_layers();
    let mut uplink = UplinkLayers::new(300_000);
    // Sound only: a lossy moment cuts the estimate below what the sound is given.
    uplink.on_estimate(300_000, &[], false, &policy(), now());
    uplink.on_estimate(40_000, &[], false, &policy(), now());
    let tracks = [camera(&layers)];
    uplink.on_estimate(42_000, &tracks, false, &policy(), now());
    assert!(!uplink.allows(CAMERA, 0), "the old cut holds the new camera back");
    uplink.measure_afresh(300_000);
    assert!(uplink.allows(CAMERA, 2));
    assert!(!uplink.on_estimate(310_000, &tracks, false, &policy(), now()));
    assert!(uplink.allows(CAMERA, 2));
    // A cut with the camera sending: the layers that fit.
    assert!(uplink.on_estimate(250_000, &tracks, false, &policy(), now()));
    assert!(uplink.allows(CAMERA, 0));
    assert!(!uplink.allows(CAMERA, 1));
  }

  #[test]
  fn layers_come_back_with_a_margin() {
    let layers = camera_layers();
    let tracks = [camera(&layers)];
    let mut uplink = UplinkLayers::new(300_000);
    uplink.on_estimate(2_000_000, &tracks, false, &policy(), now());
    uplink.on_estimate(100_000, &tracks, false, &policy(), now());
    assert!(!uplink.allows(CAMERA, 0));
    // Exactly enough for layer 0 is not enough to bring it back...
    uplink.on_estimate(198_000, &tracks, false, &policy(), now());
    assert!(!uplink.allows(CAMERA, 0));
    // ...with the margin it is, and once on it stays at the same estimate.
    uplink.on_estimate(230_000, &tracks, false, &policy(), now());
    assert!(uplink.allows(CAMERA, 0));
    uplink.on_estimate(200_000, &tracks, false, &policy(), now());
    assert!(uplink.allows(CAMERA, 0));
    // Enough for everything ends the limit.
    uplink.on_estimate(3_000_000, &tracks, false, &policy(), now());
    assert!(uplink.allows(CAMERA, 2));
  }

  #[test]
  fn screen_share_goes_before_the_camera() {
    let camera_layers = camera_layers();
    let screen_layers = [layer(0, 400_000)];
    let tracks = [
      camera(&camera_layers),
      OwnTrack {
        stream: SCREEN,
        kind: &VideoTrackKind::Screen,
        layers: &screen_layers,
      },
    ];
    let mut uplink = UplinkLayers::new(300_000);
    uplink.on_estimate(5_000_000, &tracks, true, &policy(), now());
    // Audio (48k + 80k screen sound) and the screen (400k) fit, the camera doesn't.
    uplink.on_estimate(600_000, &tracks, true, &policy(), now());
    assert!(uplink.allows(SCREEN, 0));
    assert!(!uplink.allows(CAMERA, 0));
  }

  /// Issue #941: the keyframe of a still screen's sharp layer makes the estimate dip; the layer
  /// stays through the dip, and stops only when the estimate stays below it.
  #[test]
  fn a_screen_share_keeps_its_layers_through_a_dip() {
    let screen_layers = [layer(0, 1_600_000), layer(1, 6_600_000)];
    let tracks = [OwnTrack {
      stream: SCREEN,
      kind: &VideoTrackKind::Screen,
      layers: &screen_layers,
    }];
    let start = Instant::now();
    let at = |millis: u64| start + Duration::from_millis(millis);
    let mut uplink = UplinkLayers::new(300_000);
    uplink.on_estimate(8_300_000, &tracks, false, &policy(), at(0));
    // A cut below the full demand: held for now.
    assert!(!uplink.on_estimate(7_000_000, &tracks, false, &policy(), at(100)));
    assert!(uplink.allows(SCREEN, 1));
    assert!(!uplink.on_estimate(7_500_000, &tracks, false, &policy(), at(2_000)));
    // Back above what the layers need: the hold starts afresh with the next dip.
    assert!(!uplink.on_estimate(8_300_000, &tracks, false, &policy(), at(2_500)));
    assert!(!uplink.on_estimate(7_000_000, &tracks, false, &policy(), at(3_000)));
    assert!(!uplink.on_estimate(7_000_000, &tracks, false, &policy(), at(5_900)));
    assert!(uplink.allows(SCREEN, 1));
    // Below for the whole hold: the sharp layer stops.
    assert!(uplink.on_estimate(7_000_000, &tracks, false, &policy(), at(6_000)));
    assert!(!uplink.allows(SCREEN, 1));
    assert!(uplink.allows(SCREEN, 0));
  }

  #[test]
  fn demand_counts_audio_and_every_layer() {
    let layers = camera_layers();
    let tracks = [camera(&layers)];
    assert_eq!(demand_bps(&tracks, false, &policy()), 48_000 + 2_150_000);
    assert_eq!(demand_bps(&[], true, &policy()), 128_000);
  }

  fn policy() -> UplinkPolicy {
    UplinkPolicy::default()
  }

  /// Any time: the hold matters only for screen shares, which the tests above don't hold.
  fn now() -> Instant {
    Instant::now()
  }

  fn camera(layers: &[VideoLayerInfo]) -> OwnTrack<'_> {
    OwnTrack {
      stream: CAMERA,
      kind: &VideoTrackKind::Camera,
      layers,
    }
  }

  fn camera_layers() -> Vec<VideoLayerInfo> {
    vec![layer(0, 150_000), layer(1, 500_000), layer(2, 1_500_000)]
  }

  fn layer(layer: u8, max_bitrate_bps: u32) -> VideoLayerInfo {
    VideoLayerInfo {
      layer,
      width: 320,
      height: 180,
      max_framerate: 30,
      max_bitrate_bps,
    }
  }
}
