//! Simulcast layer allocation for one receiver (ARCHITECTURE.md §6.3, SPIKE.md M6 notes).
//!
//! The receiver's downlink estimate, minus a reserve for audio, is the budget. Every subscribed
//! track first gets its lowest layer in priority order (or is paused when even that doesn't fit),
//! except tracks that may not be paused (screen shares by default: without simulcast there is no
//! lower layer, and their traffic is what lets the estimate grow), which keep their lowest layer
//! even over budget; then tracks are upgraded one layer at a time in priority order up to the layer the receiver asked
//! for. Downgrades apply at once; an upgrade only when the budget covers the new layer with
//! `upswitch_headroom` for `upswitch_hold`, so a noisy estimate doesn't make layers flap. A paused
//! track resumes its lowest layer every `resume_probe_interval` as a probe: without traffic there is
//! no feedback, and the estimate would never recover.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use teamcraft_media::datagram::StreamId;
use teamcraft_proto::VideoLayerInfo;

/// Settings of the allocation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerAllocationConfig {
  /// Bitrate kept free for forwarded audio.
  pub audio_reserve_bps: u32,
  /// An upgrade needs the budget to exceed the new total by this factor.
  pub upswitch_headroom: f64,
  /// ... continuously for this long.
  pub upswitch_hold: Duration,
  /// A track paused for lack of bandwidth is tried again at its lowest layer this often.
  pub resume_probe_interval: Duration,
  /// Whether screen shares may be paused for lack of bandwidth like cameras.
  pub can_pause_screen_shares: bool,
}

/// One subscribed track as the allocator sees it.
#[derive(Debug, Clone)]
pub struct TrackDemand<'a> {
  pub stream_id: StreamId,
  /// Lower is more important (screen share, then the active speaker's camera, then the rest).
  pub priority: u8,
  /// Highest layer the receiver wants (its viewport).
  pub max_layer: u8,
  /// Published layers, lowest first.
  pub layers: &'a [VideoLayerInfo],
  /// `false`: the lowest layer is forwarded even over budget.
  pub can_pause: bool,
}

/// Remembers the current allocation of one receiver for hysteresis.
#[derive(Debug, Default)]
pub struct LayerAllocator {
  current: HashMap<StreamId, Option<u8>>,
  upgrade_pending_since: HashMap<StreamId, Instant>,
  paused_since: HashMap<StreamId, Instant>,
  /// Resume probes in progress: the lowest layer is kept until then, whatever the estimate says,
  /// so its feedback can lift the estimate.
  probing_until: HashMap<StreamId, Instant>,
}

impl Default for LayerAllocationConfig {
  fn default() -> Self {
    Self {
      audio_reserve_bps: 3 * 40_000,
      upswitch_headroom: 1.15,
      upswitch_hold: Duration::from_secs(2),
      resume_probe_interval: Duration::from_secs(5),
      can_pause_screen_shares: false,
    }
  }
}

impl LayerAllocator {
  pub fn new() -> Self {
    Self::default()
  }

  /// The layer to forward per track (`None` = paused), given the receiver's current estimate.
  pub fn allocate(
    &mut self,
    config: &LayerAllocationConfig,
    estimate_bps: u32,
    demands: &[TrackDemand<'_>],
    now: Instant,
  ) -> HashMap<StreamId, Option<u8>> {
    let budget = f64::from(estimate_bps.saturating_sub(config.audio_reserve_bps));
    let ideal = ideal_allocation(budget, demands, 1.0);
    let with_headroom = ideal_allocation(budget, demands, config.upswitch_headroom);

    let mut result = HashMap::new();
    for demand in demands {
      let stream_id = demand.stream_id;
      let current = self.current.get(&stream_id).copied().flatten();
      let wanted = ideal.get(&stream_id).copied().flatten();
      let affordable_with_headroom = with_headroom.get(&stream_id).copied().flatten();
      let chosen = if current.is_none() && !self.current.contains_key(&stream_id) {
        // New subscription: start where the budget allows.
        wanted
      } else if layer_rank(wanted) <= layer_rank(current) {
        // Same or lower: apply immediately.
        self.upgrade_pending_since.remove(&stream_id);
        wanted
      } else if layer_rank(affordable_with_headroom) > layer_rank(current) {
        let since = *self.upgrade_pending_since.entry(stream_id).or_insert(now);
        if now.saturating_duration_since(since) >= config.upswitch_hold {
          self.upgrade_pending_since.remove(&stream_id);
          // Step up one layer at a time; a paused track resumes at its lowest layer.
          match current {
            Some(layer) => Some(layer + 1).min(affordable_with_headroom),
            None => demand.layers.first().map(|info| info.layer),
          }
        } else {
          current
        }
      } else {
        self.upgrade_pending_since.remove(&stream_id);
        current
      };
      let chosen = self.probe_if_paused_too_long(config, demand, chosen, now);
      result.insert(stream_id, chosen);
    }
    self.current = result.clone();
    self
      .upgrade_pending_since
      .retain(|stream_id, _| result.contains_key(stream_id));
    self
      .paused_since
      .retain(|stream_id, _| result.contains_key(stream_id));
    result
  }

  fn probe_if_paused_too_long(
    &mut self,
    config: &LayerAllocationConfig,
    demand: &TrackDemand<'_>,
    chosen: Option<u8>,
    now: Instant,
  ) -> Option<u8> {
    let lowest = demand.layers.first().map(|info| info.layer);
    if chosen.is_some() {
      self.paused_since.remove(&demand.stream_id);
      self.probing_until.remove(&demand.stream_id);
      return chosen;
    }
    if self
      .probing_until
      .get(&demand.stream_id)
      .is_some_and(|until| now < *until)
    {
      return lowest;
    }
    self.probing_until.remove(&demand.stream_id);
    let since = *self.paused_since.entry(demand.stream_id).or_insert(now);
    if now.saturating_duration_since(since) < config.resume_probe_interval {
      return None;
    }
    self.paused_since.remove(&demand.stream_id);
    self
      .probing_until
      .insert(demand.stream_id, now + RESUME_PROBE_DURATION);
    lowest
  }
}

/// How long a resume probe keeps the lowest layer before the estimate decides again.
const RESUME_PROBE_DURATION: Duration = Duration::from_secs(2);

/// Bitrate a receiver needs to get every subscribed track at its requested layer, including the
/// audio reserve and the upswitch headroom: the demand that lets the estimator probe upwards.
pub fn full_demand_bps(config: &LayerAllocationConfig, demands: &[TrackDemand<'_>]) -> u32 {
  let video: f64 = demands
    .iter()
    .filter_map(|demand| {
      demand
        .layers
        .iter()
        .filter(|info| info.layer <= demand.max_layer)
        .map(|info| info.max_bitrate_bps)
        .max()
    })
    .map(f64::from)
    .sum();
  // A little above the exact need, so the estimate can actually reach the upgrade threshold.
  const PROBE_MARGIN: f64 = 1.1;
  // Saturating float-to-int conversion is intended.
  ((f64::from(config.audio_reserve_bps) + video * config.upswitch_headroom) * PROBE_MARGIN) as u32
}

/// Greedy allocation that fits `budget` with every layer's bitrate scaled by `headroom`.
fn ideal_allocation(
  budget: f64,
  demands: &[TrackDemand<'_>],
  headroom: f64,
) -> HashMap<StreamId, Option<u8>> {
  let mut ordered: Vec<&TrackDemand<'_>> = demands.iter().collect();
  ordered.sort_by_key(|demand| (demand.priority, demand.stream_id));
  let cost = |demand: &TrackDemand<'_>, layer: u8| -> f64 {
    demand
      .layers
      .iter()
      .find(|info| info.layer == layer)
      .map_or(f64::INFINITY, |info| f64::from(info.max_bitrate_bps) * headroom)
  };

  let mut allocation: HashMap<StreamId, Option<u8>> = HashMap::new();
  let mut spent = 0.0;
  for demand in &ordered {
    let lowest = demand.layers.first().map(|info| info.layer);
    let chosen = lowest.filter(|&layer| {
      layer <= demand.max_layer && (spent + cost(demand, layer) <= budget || !demand.can_pause)
    });
    if let Some(layer) = chosen {
      spent += cost(demand, layer);
    }
    allocation.insert(demand.stream_id, chosen);
  }
  loop {
    let mut upgraded = false;
    for demand in &ordered {
      let Some(Some(layer)) = allocation.get(&demand.stream_id).copied() else {
        continue;
      };
      let next = layer + 1;
      if next > demand.max_layer || !demand.layers.iter().any(|info| info.layer == next) {
        continue;
      }
      let extra = cost(demand, next) - cost(demand, layer);
      if spent + extra <= budget {
        spent += extra;
        allocation.insert(demand.stream_id, Some(next));
        upgraded = true;
      }
    }
    if !upgraded {
      return allocation;
    }
  }
}

/// Orders `None` (paused) below every layer.
fn layer_rank(maybe_layer: Option<u8>) -> i16 {
  maybe_layer.map_or(-1, i16::from)
}

#[cfg(test)]
mod tests {
  use super::*;

  mod allocation {
    use super::*;

    #[test]
    fn budget_picks_the_best_fitting_layer() {
      let layers = three_layers();
      let mut allocator = LayerAllocator::new();
      let now = Instant::now();
      // 120k audio reserve + 500k for layer 1.
      let result = allocator.allocate(&config(), 700_000, &[demand(1, 0, 2, &layers)], now);
      assert_eq!(result[&StreamId(1)], Some(1));
    }

    #[test]
    fn requested_maximum_is_respected() {
      let layers = three_layers();
      let mut allocator = LayerAllocator::new();
      let result = allocator.allocate(&config(), 10_000_000, &[demand(1, 0, 0, &layers)], Instant::now());
      assert_eq!(result[&StreamId(1)], Some(0));
    }

    #[test]
    fn priority_decides_who_gets_upgraded() {
      let layers = three_layers();
      let mut allocator = LayerAllocator::new();
      // Room for two base layers (2 x 150k) plus one upgrade to layer 1 (+350k).
      let demands = [demand(1, 2, 2, &layers), demand(2, 0, 2, &layers)];
      let result = allocator.allocate(&config(), 120_000 + 650_000, &demands, Instant::now());
      assert_eq!((result[&StreamId(1)], result[&StreamId(2)]), (Some(0), Some(1)));
    }

    #[test]
    fn too_little_bandwidth_pauses_video() {
      let layers = three_layers();
      let mut allocator = LayerAllocator::new();
      let result = allocator.allocate(&config(), 200_000, &[demand(1, 0, 2, &layers)], Instant::now());
      assert_eq!(result[&StreamId(1)], None);
    }

    #[test]
    fn unpausable_track_keeps_its_lowest_layer_over_budget() {
      let layers = three_layers();
      let screen = TrackDemand {
        can_pause: false,
        ..demand(1, 0, 2, &layers)
      };
      let camera = demand(2, 2, 2, &layers);
      let mut allocator = LayerAllocator::new();
      let result = allocator.allocate(&config(), 200_000, &[screen, camera], Instant::now());
      assert_eq!((result[&StreamId(1)], result[&StreamId(2)]), (Some(0), None));
    }
  }

  mod hysteresis {
    use super::*;

    #[test]
    fn downgrade_is_immediate_upgrade_waits() {
      let layers = three_layers();
      let mut allocator = LayerAllocator::new();
      let mut now = Instant::now();
      let demands = [demand(1, 0, 2, &layers)];
      assert_eq!(
        allocator.allocate(&config(), 400_000, &demands, now)[&StreamId(1)],
        Some(0)
      );
      // Plenty of budget now; allocation runs with every feedback (100 ms).
      let mut layer_after = |seconds: u64, estimate: u32, now: &mut Instant| {
        let mut layer = None;
        for _ in 0..seconds * 10 {
          *now += Duration::from_millis(100);
          layer = allocator.allocate(&config(), estimate, &demands, *now)[&StreamId(1)];
        }
        layer
      };
      assert_eq!(layer_after(1, 3_000_000, &mut now), Some(0), "still holding");
      assert_eq!(
        layer_after(2, 3_000_000, &mut now),
        Some(1),
        "one layer up after the hold"
      );
      assert_eq!(
        layer_after(2, 3_000_000, &mut now),
        Some(2),
        "the next one after another hold"
      );
      now += Duration::from_millis(100);
      assert_eq!(
        allocator.allocate(&config(), 400_000, &demands, now)[&StreamId(1)],
        Some(0),
        "the estimate collapses: straight down"
      );
    }

    #[test]
    fn upgrade_needs_headroom() {
      let layers = three_layers();
      let mut allocator = LayerAllocator::new();
      let start = Instant::now();
      let demands = [demand(1, 0, 2, &layers)];
      allocator.allocate(&config(), 400_000, &demands, start);
      // 120k + 500k fits exactly, but not with 15 % headroom: stays on layer 0.
      for second in 1..10 {
        let at = start + Duration::from_secs(second);
        assert_eq!(
          allocator.allocate(&config(), 620_000, &demands, at)[&StreamId(1)],
          Some(0)
        );
      }
    }
  }

  mod probing {
    use super::*;

    #[test]
    fn paused_track_is_retried_periodically() {
      let layers = three_layers();
      let mut allocator = LayerAllocator::new();
      let start = Instant::now();
      let demands = [demand(1, 0, 2, &layers)];
      assert_eq!(
        allocator.allocate(&config(), 200_000, &demands, start)[&StreamId(1)],
        None
      );
      let before = start + Duration::from_secs(4);
      assert_eq!(
        allocator.allocate(&config(), 200_000, &demands, before)[&StreamId(1)],
        None
      );
      let after = start + Duration::from_secs(5);
      assert_eq!(
        allocator.allocate(&config(), 200_000, &demands, after)[&StreamId(1)],
        Some(0)
      );
    }
  }

  fn config() -> LayerAllocationConfig {
    LayerAllocationConfig::default()
  }

  fn demand(stream: u32, priority: u8, max_layer: u8, layers: &[VideoLayerInfo]) -> TrackDemand<'_> {
    TrackDemand {
      stream_id: StreamId(stream),
      priority,
      max_layer,
      layers,
      can_pause: true,
    }
  }

  fn three_layers() -> Vec<VideoLayerInfo> {
    [
      (0, 320, 180, 150_000),
      (1, 640, 360, 500_000),
      (2, 1280, 720, 1_500_000),
    ]
    .into_iter()
    .map(|(layer, width, height, max_bitrate_bps)| VideoLayerInfo {
      layer,
      width,
      height,
      max_framerate: 30,
      max_bitrate_bps,
    })
    .collect()
  }
}
