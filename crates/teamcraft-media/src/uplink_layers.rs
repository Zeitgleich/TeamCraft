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
//! A camera whose encoders follow the allocation ([`OwnTrack::adapts_bitrate`]) gives up bitrate
//! before resolution (#939): its top sent layer may be encoded at a share of its bitrate
//! ([`UplinkPolicy::camera_bitrate_shares`]), down to half, before it stops and the next lower
//! layer becomes the top one. The frame rate stays. Lower layers keep their whole bitrate (other
//! viewers on weak links take them). Before, the top layer stopped as soon as its whole bitrate
//! didn't fit: a 500 kbit/s uplink sent 320×180 at about 80 kbit/s, a 2 Mbit/s one 640×360.
//! A share changes only after a hold ([`UplinkPolicy::share_downswitch_hold`],
//! [`UplinkPolicy::share_upswitch_hold`]; each change rebuilds the encoder, which starts with a
//! keyframe), and such a camera climbs back one step at a time: an estimate that grew while the
//! uplink carried less is no proof that it carries every layer at once.
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
  /// The shares of its bitrate the top sent layer of a camera that adapts its bitrate may be
  /// encoded at, lowest first, the last one 1.0. Few steps: the encoders are rebuilt (and send a
  /// keyframe) when a layer's bitrate changes.
  pub camera_bitrate_shares: [f64; 3],
  /// How long the estimate has to stay below a camera's top layer at its share before the
  /// share drops while the layer stays (a lower layer is taken at once). A lower share rebuilds
  /// the layer's encoder, and its keyframe is the largest picture there is: sent into a link
  /// that just got congested, it held the picture up for more than a second (#939). The
  /// estimator's first cut is often only a step, and the cut to what the path delivers follows
  /// within this time.
  pub share_downswitch_hold: Duration,
  /// How long the estimate has to allow a higher share of a camera's top layer before it
  /// rises while the layer stays. Each change costs a keyframe of the layer; stepping through
  /// the shares one after another sent three within a second into a nearly full link (#939).
  pub share_upswitch_hold: Duration,
  /// A step up of a camera that adapts its bitrate counts as carried once it held this long; one
  /// undone sooner was a probe the uplink didn't carry, and the next step up waits (4 s, doubling
  /// with every such probe up to [`MAX_PROBE_BACKOFF`], from the start after one that held). The
  /// estimate grows again within seconds after a cut, and every probe costs a keyframe and a
  /// moment of congestion: on a link that stays limited they came every few seconds (#939).
  pub probe_proof: Duration,
}

/// The longest wait before a camera steps up again after probes that failed.
const MAX_PROBE_BACKOFF: Duration = Duration::from_secs(32);
/// The wait after the first probe that failed.
const FIRST_PROBE_BACKOFF: Duration = Duration::from_secs(4);

/// The step-up probes of one adaptive camera.
#[derive(Debug, Default)]
struct Probes {
  /// When it last stepped up, until the step counts as carried.
  maybe_stepped_up_at: Option<Instant>,
  backoff: Duration,
  maybe_blocked_until: Option<Instant>,
}

/// One own video track as the allocation sees it.
#[derive(Debug, Clone, Copy)]
pub struct OwnTrack<'a> {
  pub stream: StreamId,
  pub kind: &'a VideoTrackKind,
  pub layers: &'a [VideoLayerInfo],
  /// Its encoders follow [`UplinkLayers::bitrate_bps`] (a camera the call encodes itself); other
  /// tracks are sent at their layers' whole bitrate or not at all.
  pub adapts_bitrate: bool,
}

/// The current allocation: per track the highest layer that may be sent, `None` for paused.
#[derive(Debug)]
pub struct UplinkLayers {
  /// Set once the estimator cut the rate; until then every layer is sent.
  is_limited: bool,
  last_estimate_bps: u32,
  top_layers: HashMap<StreamId, Option<u8>>,
  /// The share of its bitrate the top layer of a track is encoded at, where less than all of it.
  top_shares: HashMap<StreamId, f64>,
  /// Since when the estimate is below a screen share's current layers.
  downgrade_pending_since: HashMap<StreamId, Instant>,
  /// Since when the estimate asks for another share of a camera's top layer, and whether a
  /// higher one.
  share_change_pending_since: HashMap<StreamId, (Instant, bool)>,
  probes: HashMap<StreamId, Probes>,
}

impl Default for UplinkPolicy {
  fn default() -> Self {
    Self {
      audio_reserve_bps: 48_000,
      screen_audio_reserve_bps: 80_000,
      upgrade_margin: 1.15,
      screen_downswitch_hold: Duration::from_secs(3),
      camera_bitrate_shares: [0.5, 0.7, 1.0],
      share_downswitch_hold: Duration::from_secs(1),
      share_upswitch_hold: Duration::from_secs(2),
      probe_proof: Duration::from_secs(4),
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
      top_shares: HashMap::new(),
      downgrade_pending_since: HashMap::new(),
      share_change_pending_since: HashMap::new(),
      probes: HashMap::new(),
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
    } else if estimate_bps >= full_demand && self.is_fully_allocated(tracks) {
      // A camera that adapts its bitrate climbs back step by step first: an estimate that grew
      // while the uplink carried less is no proof that it carries everything at once (#939).
      self.is_limited = false;
    }
    let before: Vec<(Option<u8>, f64)> = tracks
      .iter()
      .map(|track| (self.top_of(track), self.share_of(track.stream)))
      .collect();
    self.allocate(estimate_bps, tracks, is_sending_screen_audio, policy, now);
    tracks
      .iter()
      .zip(before)
      .any(|(track, before)| (self.top_of(track), self.share_of(track.stream)) != before)
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
    self.top_shares.clear();
    self.downgrade_pending_since.clear();
    self.share_change_pending_since.clear();
    self.probes.clear();
  }

  /// The bitrate `info`'s layer of `stream` is encoded at: its whole bitrate, or a share of it
  /// while it is the top sent layer of a camera that adapts its bitrate on a limited uplink.
  pub fn bitrate_bps(&self, stream: StreamId, info: &VideoLayerInfo) -> u32 {
    let is_top = self
      .top_layers
      .get(&stream)
      .is_some_and(|maybe_top| *maybe_top == Some(info.layer));
    if !is_top {
      return info.max_bitrate_bps;
    }
    // Saturating float-to-int conversion is intended; a share is at most 1.
    (f64::from(info.max_bitrate_bps) * self.share_of(stream)).round() as u32
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
    self.top_shares.remove(&stream);
    self.downgrade_pending_since.remove(&stream);
    self.share_change_pending_since.remove(&stream);
    self.probes.remove(&stream);
  }

  fn share_of(&self, stream: StreamId) -> f64 {
    self.top_shares.get(&stream).copied().unwrap_or(1.0)
  }

  /// Whether every track sends every layer at its whole bitrate.
  fn is_fully_allocated(&self, tracks: &[OwnTrack<'_>]) -> bool {
    tracks
      .iter()
      .all(|track| self.top_of(track) == top_index(track.layers) && self.share_of(track.stream) >= 1.0)
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
      self.top_shares.clear();
      self.downgrade_pending_since.clear();
      self.share_change_pending_since.clear();
      return;
    }
    let reserve = policy.audio_reserve_bps
      + if is_sending_screen_audio {
        policy.screen_audio_reserve_bps
      } else {
        0
      };
    let mut budget = f64::from(estimate_bps.saturating_sub(reserve));
    // An estimate that covers everything needs no margin on top: it is capped there.
    let margin = if estimate_bps >= demand_bps(tracks, is_sending_screen_audio, policy) {
      1.0
    } else {
      policy.upgrade_margin
    };
    let mut ordered: Vec<&OwnTrack<'_>> = tracks.iter().collect();
    // Screen shares first: their content (text) is what people are looking at.
    ordered.sort_by_key(|track| match track.kind {
      VideoTrackKind::Screen => 0,
      _ => 1,
    });
    let mut allocation = HashMap::new();
    let mut shares = HashMap::new();
    for track in ordered {
      let maybe_current = self.top_layers.get(&track.stream).copied().flatten();
      let current_share = self.share_of(track.stream);
      let is_adaptive = track.adapts_bitrate && *track.kind == VideoTrackKind::Camera;
      let track_shares: &[f64] = if is_adaptive {
        &policy.camera_bitrate_shares
      } else {
        &[1.0]
      };
      // Candidates from the least to the most: each layer at each share, the lower layers whole.
      let mut maybe_chosen: Option<(u8, f64)> = None;
      let mut below = 0.0;
      'layers: for info in track.layers {
        for &share in track_shares {
          let cost = below + f64::from(info.max_bitrate_bps) * share;
          // Keeping a layer at a share needs it to fit; more needs the margin on top.
          let is_kept = maybe_current.is_some_and(|current| (info.layer, share) <= (current, current_share));
          let needed = if is_kept { cost } else { cost * margin };
          if needed > budget {
            break 'layers;
          }
          maybe_chosen = Some((info.layer, share));
          if !is_kept && is_adaptive {
            // One step up at a time: the step is the probe of whether the uplink carries more.
            break 'layers;
          }
        }
        below += f64::from(info.max_bitrate_bps);
      }
      let maybe_top = maybe_chosen.map(|(layer, _)| layer);
      let maybe_top = self.after_screen_hold(track, maybe_current, maybe_top, policy, now);
      let share = match maybe_chosen {
        Some((layer, share)) if Some(layer) == maybe_top => share,
        _ => 1.0,
      };
      let share = self.after_share_hold(
        track.stream,
        (maybe_current, current_share),
        (maybe_top, share),
        policy,
        now,
      );
      let (maybe_top, share) = if is_adaptive {
        self.after_probe_backoff(
          track.stream,
          (maybe_current, current_share),
          (maybe_top, share),
          policy,
          now,
        )
      } else {
        (maybe_top, share)
      };
      let used = track
        .layers
        .iter()
        .filter(|info| maybe_top.is_some_and(|top| info.layer <= top))
        .map(|info| {
          let layer_share = if Some(info.layer) == maybe_top { share } else { 1.0 };
          f64::from(info.max_bitrate_bps) * layer_share
        })
        .sum::<f64>();
      budget -= used;
      allocation.insert(track.stream, maybe_top);
      if share < 1.0 {
        shares.insert(track.stream, share);
      }
    }
    self.top_layers = allocation;
    self.top_shares = shares;
    self
      .downgrade_pending_since
      .retain(|stream, _| tracks.iter().any(|track| track.stream == *stream));
    self
      .share_change_pending_since
      .retain(|stream, _| tracks.iter().any(|track| track.stream == *stream));
    self
      .probes
      .retain(|stream, _| tracks.iter().any(|track| track.stream == *stream));
  }

  /// `chosen` for an adaptive camera at `current`, unless it is a step up while an earlier one
  /// that failed still holds them back ([`UplinkPolicy::probe_proof`]): then `current`.
  fn after_probe_backoff(
    &mut self,
    stream: StreamId,
    current: (Option<u8>, f64),
    chosen: (Option<u8>, f64),
    policy: &UplinkPolicy,
    now: Instant,
  ) -> (Option<u8>, f64) {
    let rank = |(maybe_layer, share): (Option<u8>, f64)| (maybe_layer.map_or(-1, i16::from), share);
    let probes = self.probes.entry(stream).or_default();
    if probes
      .maybe_stepped_up_at
      .is_some_and(|at| now.saturating_duration_since(at) >= policy.probe_proof)
    {
      // The last step held: the uplink carries it.
      probes.maybe_stepped_up_at = None;
      probes.backoff = Duration::ZERO;
    }
    if rank(chosen) > rank(current) {
      if probes.maybe_blocked_until.is_some_and(|until| now < until) {
        return current;
      }
      probes.maybe_stepped_up_at = Some(now);
    } else if rank(chosen) < rank(current) && probes.maybe_stepped_up_at.take().is_some() {
      probes.backoff = (probes.backoff * 2).clamp(FIRST_PROBE_BACKOFF, MAX_PROBE_BACKOFF);
      probes.maybe_blocked_until = Some(now + probes.backoff);
    }
    chosen
  }

  /// `share` for the top layer `maybe_top`, unless that is the current top layer at another
  /// share, asked for less long than [`UplinkPolicy::share_downswitch_hold`] (lower) or
  /// [`UplinkPolicy::share_upswitch_hold`] (higher): then the current share.
  fn after_share_hold(
    &mut self,
    stream: StreamId,
    (maybe_current, current_share): (Option<u8>, f64),
    (maybe_top, share): (Option<u8>, f64),
    policy: &UplinkPolicy,
    now: Instant,
  ) -> f64 {
    if maybe_top.is_none() || maybe_top != maybe_current || share == current_share {
      self.share_change_pending_since.remove(&stream);
      return share;
    }
    let is_higher = share > current_share;
    let hold = if is_higher {
      policy.share_upswitch_hold
    } else {
      policy.share_downswitch_hold
    };
    let pending = self
      .share_change_pending_since
      .entry(stream)
      .or_insert((now, is_higher));
    if pending.1 != is_higher {
      *pending = (now, is_higher);
    }
    if now.saturating_duration_since(pending.0) < hold {
      return current_share;
    }
    self.share_change_pending_since.remove(&stream);
    share
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

/// How far above what sending everything takes the estimate may grow ([`probe_demand_bps`]), on
/// top of [`UplinkPolicy::upgrade_margin`] (as the SFU's `full_demand_bps`).
const PROBE_MARGIN: f64 = 1.1;

/// What the estimator may grow to ([`crate::bwe::SendSideEstimator::set_demand_bps`]): what
/// sending everything takes, with the margin a layer needs to come back and a little more. Capped
/// at exactly what everything takes, the estimate fell below it with every step cut (the queue of
/// a keyframe), and a screen share's sharp layer stopped once the estimate took longer than the
/// hold to grow back: a CI runner's viewer was sent back to the blurred half-size picture (#939,
/// screen_sharpness). With the margin a step cut leaves every layer in place.
pub fn probe_demand_bps(
  tracks: &[OwnTrack<'_>],
  is_sending_screen_audio: bool,
  policy: &UplinkPolicy,
) -> u32 {
  let audio = f64::from(audio_reserve_bps(is_sending_screen_audio, policy));
  let video = f64::from(demand_bps(tracks, is_sending_screen_audio, policy)) - audio;
  // Saturating float-to-int conversion is intended.
  ((video * policy.upgrade_margin + audio) * PROBE_MARGIN) as u32
}

fn audio_reserve_bps(is_sending_screen_audio: bool, policy: &UplinkPolicy) -> u32 {
  policy.audio_reserve_bps
    + if is_sending_screen_audio {
      policy.screen_audio_reserve_bps
    } else {
      0
    }
}

/// What sending everything takes: audio plus every layer of every own track.
pub fn demand_bps(tracks: &[OwnTrack<'_>], is_sending_screen_audio: bool, policy: &UplinkPolicy) -> u32 {
  let video: u32 = tracks
    .iter()
    .flat_map(|track| track.layers.iter())
    .map(|info| info.max_bitrate_bps)
    .fold(0, u32::saturating_add);
  video.saturating_add(audio_reserve_bps(is_sending_screen_audio, policy))
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

  /// #978: a 1080p camera's 180p layer keeps a weak uplink in the picture; with the three layers
  /// of before (360p the lowest, at half its bitrate 190 kbit/s) 200 kbit/s sent no video at all.
  #[test]
  fn a_weak_uplink_sends_a_1080p_cameras_180p_layer() {
    let four = full_hd_layers();
    let tracks = [adapting_camera(&four)];
    let mut uplink = UplinkLayers::new(300_000);
    uplink.on_estimate(6_000_000, &tracks, false, &policy(), now());
    assert!(uplink.allows(CAMERA, 3));
    uplink.on_estimate(200_000, &tracks, false, &policy(), now());
    assert!(uplink.allows(CAMERA, 0));
    assert!(!uplink.allows(CAMERA, 1));
    let three = within_three(&four);
    let tracks = [adapting_camera(&three)];
    let mut before = UplinkLayers::new(300_000);
    before.on_estimate(6_000_000, &tracks, false, &policy(), now());
    before.on_estimate(200_000, &tracks, false, &policy(), now());
    assert!(!before.allows(CAMERA, 0));
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
        adapts_bitrate: false,
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
      adapts_bitrate: false,
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

  /// A 4K share (#565) steps down through its three layers as the uplink shrinks: 4K at about 15
  /// Mbit/s, 1080p at a quarter of that, 540p at a sixteenth.
  #[test]
  fn a_4k_screen_share_steps_down_layer_by_layer() {
    let shares = crate::video::screen_layers(
      crate::video::VideoResolution {
        width: 3840,
        height: 2160,
      },
      30,
      teamcraft_proto::ScreenContentMode::Detail,
      &teamcraft_proto::ScreenShareResolution::Uhd,
    );
    assert_eq!(shares.len(), 3);
    let tracks = [OwnTrack {
      stream: SCREEN,
      kind: &VideoTrackKind::Screen,
      layers: &shares,
      adapts_bitrate: false,
    }];
    let start = Instant::now();
    let at = |seconds: u64| start + Duration::from_secs(seconds);
    let mut uplink = UplinkLayers::new(300_000);
    uplink.on_estimate(25_000_000, &tracks, false, &policy(), at(0));
    assert!(uplink.allows(SCREEN, 2));
    // A 10 Mbit/s uplink (the estimator's old ceiling) can't carry 4K for long: down to 1080p.
    uplink.on_estimate(10_000_000, &tracks, false, &policy(), at(1));
    uplink.on_estimate(10_000_000, &tracks, false, &policy(), at(5));
    assert!(!uplink.allows(SCREEN, 2));
    assert!(uplink.allows(SCREEN, 1));
    // A weak link keeps the 540p layer.
    uplink.on_estimate(2_000_000, &tracks, false, &policy(), at(6));
    uplink.on_estimate(2_000_000, &tracks, false, &policy(), at(10));
    assert!(!uplink.allows(SCREEN, 1));
    assert!(uplink.allows(SCREEN, 0));
  }

  /// Issue #939: a step up that the uplink doesn't carry is undone at once; the next one waits,
  /// longer with every failure, and as before once a step held.
  #[test]
  fn steps_up_that_fail_back_off() {
    let layers = camera_layers();
    let tracks = [adapting_camera(&layers)];
    let start = Instant::now();
    let at = |millis: u64| start + Duration::from_millis(millis);
    let mut uplink = UplinkLayers::new(300_000);
    uplink.on_estimate(2_500_000, &tracks, false, &policy(), at(0));
    // Down to the middle layer, whole.
    uplink.on_estimate(800_000, &tracks, false, &policy(), at(100));
    assert!(!uplink.allows(CAMERA, 2));
    // The estimate grows: the top layer comes back at half (the probe), and the cut that
    // follows undoes it.
    uplink.on_estimate(1_700_000, &tracks, false, &policy(), at(1_000));
    assert!(uplink.allows(CAMERA, 2));
    uplink.on_estimate(800_000, &tracks, false, &policy(), at(1_500));
    assert!(!uplink.allows(CAMERA, 2));
    // The next probe waits 4 s, the one after that 8 s.
    uplink.on_estimate(1_700_000, &tracks, false, &policy(), at(3_000));
    assert!(!uplink.allows(CAMERA, 2));
    uplink.on_estimate(1_700_000, &tracks, false, &policy(), at(5_600));
    assert!(uplink.allows(CAMERA, 2));
    uplink.on_estimate(800_000, &tracks, false, &policy(), at(6_000));
    uplink.on_estimate(1_700_000, &tracks, false, &policy(), at(13_000));
    assert!(!uplink.allows(CAMERA, 2));
    uplink.on_estimate(1_700_000, &tracks, false, &policy(), at(14_100));
    assert!(uplink.allows(CAMERA, 2));
    // This one holds: a later failure waits 4 s again.
    uplink.on_estimate(1_700_000, &tracks, false, &policy(), at(18_200));
    uplink.on_estimate(800_000, &tracks, false, &policy(), at(19_000));
    uplink.on_estimate(1_700_000, &tracks, false, &policy(), at(19_100));
    assert!(uplink.allows(CAMERA, 2));
  }

  /// Issue #939: a camera that adapts its bitrate keeps its top layer at a share of its bitrate
  /// where it would stop, so a 500 kbit/s uplink sends the middle layer rather than the smallest.
  #[test]
  fn an_adapting_camera_gives_up_bitrate_before_resolution() {
    let layers = camera_layers();
    let tracks = [adapting_camera(&layers)];
    let start = Instant::now();
    let at = |millis: u64| start + Duration::from_millis(millis);
    let mut uplink = UplinkLayers::new(300_000);
    uplink.on_estimate(2_500_000, &tracks, false, &policy(), at(0));
    assert_eq!(bitrates(&uplink, &layers), [150_000, 500_000, 1_500_000]);
    // 48k audio, 150k and 500k leave 1.05M: the top layer at 70 %, once the estimate stayed
    // there for the hold.
    assert!(!uplink.on_estimate(1_750_000, &tracks, false, &policy(), at(100)));
    assert_eq!(bitrates(&uplink, &layers), [150_000, 500_000, 1_500_000]);
    assert!(uplink.on_estimate(1_750_000, &tracks, false, &policy(), at(1_100)));
    assert!(uplink.allows(CAMERA, 2));
    assert_eq!(bitrates(&uplink, &layers), [150_000, 500_000, 1_050_000]);
    // At half of it, the top layer still fits.
    uplink.on_estimate(1_450_000, &tracks, false, &policy(), at(1_200));
    assert!(uplink.on_estimate(1_450_000, &tracks, false, &policy(), at(2_200)));
    assert_eq!(bitrates(&uplink, &layers), [150_000, 500_000, 750_000]);
    // Below that it stops at once, and the middle layer is the top one, at the share that fits.
    assert!(uplink.on_estimate(560_000, &tracks, false, &policy(), at(2_300)));
    assert!(!uplink.allows(CAMERA, 2));
    assert!(uplink.allows(CAMERA, 1));
    assert_eq!(uplink.bitrate_bps(CAMERA, &layers[1]), 350_000);
    // A higher share comes back only with the margin, and once that held for a while.
    assert!(!uplink.on_estimate(700_000, &tracks, false, &policy(), at(2_400)));
    assert_eq!(uplink.bitrate_bps(CAMERA, &layers[1]), 350_000);
    assert!(!uplink.on_estimate(820_000, &tracks, false, &policy(), at(2_500)));
    assert!(uplink.on_estimate(820_000, &tracks, false, &policy(), at(4_500)));
    assert_eq!(uplink.bitrate_bps(CAMERA, &layers[1]), 500_000);
    // Enough for everything: back one step at a time, the top layer's share after the hold.
    assert!(uplink.on_estimate(3_000_000, &tracks, false, &policy(), at(4_600)));
    assert_eq!(bitrates(&uplink, &layers), [150_000, 500_000, 750_000]);
    assert!(!uplink.on_estimate(3_000_000, &tracks, false, &policy(), at(4_700)));
    assert!(uplink.on_estimate(3_000_000, &tracks, false, &policy(), at(6_700)));
    assert_eq!(bitrates(&uplink, &layers), [150_000, 500_000, 1_050_000]);
    assert!(!uplink.on_estimate(3_000_000, &tracks, false, &policy(), at(6_800)));
    assert!(uplink.on_estimate(3_000_000, &tracks, false, &policy(), at(8_800)));
    assert_eq!(bitrates(&uplink, &layers), [150_000, 500_000, 1_500_000]);
  }

  /// A dip of the estimate shorter than the hold keeps the top layer's share: its encoder isn't
  /// rebuilt for it.
  #[test]
  fn a_short_dip_keeps_the_top_layers_share() {
    let layers = camera_layers();
    let tracks = [adapting_camera(&layers)];
    let start = Instant::now();
    let at = |millis: u64| start + Duration::from_millis(millis);
    let mut uplink = UplinkLayers::new(300_000);
    uplink.on_estimate(2_500_000, &tracks, false, &policy(), at(0));
    uplink.on_estimate(1_750_000, &tracks, false, &policy(), at(100));
    assert!(!uplink.on_estimate(2_300_000, &tracks, false, &policy(), at(400)));
    assert!(!uplink.on_estimate(1_750_000, &tracks, false, &policy(), at(500)));
    assert!(!uplink.on_estimate(1_750_000, &tracks, false, &policy(), at(900)));
    assert_eq!(uplink.bitrate_bps(CAMERA, &layers[2]), 1_500_000);
  }

  /// Cameras the call doesn't encode itself and screen shares keep their whole bitrate per layer.
  #[test]
  fn other_tracks_send_a_layer_whole_or_not_at_all() {
    let layers = camera_layers();
    let tracks = [camera(&layers)];
    let mut uplink = UplinkLayers::new(300_000);
    uplink.on_estimate(2_500_000, &tracks, false, &policy(), now());
    uplink.on_estimate(1_750_000, &tracks, false, &policy(), now());
    assert!(!uplink.allows(CAMERA, 2));
    assert_eq!(bitrates(&uplink, &layers), [150_000, 500_000]);
    let screen_layers = [layer(0, 1_600_000), layer(1, 6_600_000)];
    let tracks = [OwnTrack {
      stream: SCREEN,
      kind: &VideoTrackKind::Screen,
      layers: &screen_layers,
      adapts_bitrate: true,
    }];
    let start = Instant::now();
    let mut uplink = UplinkLayers::new(300_000);
    uplink.on_estimate(9_000_000, &tracks, false, &policy(), start);
    uplink.on_estimate(6_000_000, &tracks, false, &policy(), start);
    uplink.on_estimate(
      6_000_000,
      &tracks,
      false,
      &policy(),
      start + Duration::from_secs(4),
    );
    assert!(!uplink.allows(SCREEN, 1));
    assert_eq!(uplink.bitrate_bps(SCREEN, &screen_layers[0]), 1_600_000);
  }

  /// The estimate may grow far enough above what everything takes that a step cut (to 85 %)
  /// leaves every layer, the screen share's sharp one too, in place without a hold.
  #[test]
  fn a_step_cut_from_the_probe_demand_keeps_every_layer() {
    let screen_layers = [layer(0, 1_660_000), layer(1, 6_635_000)];
    let tracks = [OwnTrack {
      stream: SCREEN,
      kind: &VideoTrackKind::Screen,
      layers: &screen_layers,
      adapts_bitrate: false,
    }];
    let cap = probe_demand_bps(&tracks, false, &policy());
    assert!(cap > demand_bps(&tracks, false, &policy()));
    let start = Instant::now();
    let mut uplink = UplinkLayers::new(300_000);
    uplink.on_estimate(cap, &tracks, false, &policy(), start);
    // Cut a step, and long after the hold still not grown back.
    let cut = (f64::from(cap) * 0.85) as u32;
    uplink.on_estimate(cut, &tracks, false, &policy(), start + Duration::from_millis(100));
    uplink.on_estimate(cut, &tracks, false, &policy(), start + Duration::from_secs(10));
    assert!(uplink.allows(SCREEN, 1));
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
      adapts_bitrate: false,
    }
  }

  fn adapting_camera(layers: &[VideoLayerInfo]) -> OwnTrack<'_> {
    OwnTrack {
      adapts_bitrate: true,
      ..camera(layers)
    }
  }

  /// The bitrate of every layer of [`CAMERA`] that may be sent.
  fn bitrates(uplink: &UplinkLayers, layers: &[VideoLayerInfo]) -> Vec<u32> {
    layers
      .iter()
      .filter(|info| uplink.allows(CAMERA, info.layer))
      .map(|info| uplink.bitrate_bps(CAMERA, info))
      .collect()
  }

  fn camera_layers() -> Vec<VideoLayerInfo> {
    vec![layer(0, 150_000), layer(1, 500_000), layer(2, 1_500_000)]
  }

  /// A 1080p camera's layers (`video::camera_layers`, #978): 180p to 1080p.
  fn full_hd_layers() -> Vec<VideoLayerInfo> {
    vec![
      layer(0, 93_750),
      layer(1, 375_000),
      layer(2, 1_500_000),
      layer(3, 3_000_000),
    ]
  }

  /// `layers` as an SFU that takes three gets them.
  fn within_three(layers: &[VideoLayerInfo]) -> Vec<VideoLayerInfo> {
    crate::video::within_layer_limit(layers, 3)
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
