//! Simulcast layer allocation for one receiver (ARCHITECTURE.md §6.3, SPIKE.md M6 notes).
//!
//! The receiver's downlink estimate, minus a reserve for audio, is the budget. Every subscribed
//! track first gets its lowest layer in priority order (or is paused when even that doesn't fit),
//! except tracks that may not be paused (screen shares by default: without simulcast there is no
//! lower layer, and their traffic is what lets the estimate grow), which keep their lowest layer
//! even over budget; then tracks are upgraded one layer at a time in priority order up to the layer the receiver asked
//! for. Downgrades apply at once (a screen share's after `screen_downswitch_hold`); an upgrade only
//! when the budget covers the new layer with `upswitch_headroom` for `upswitch_hold`, so a noisy
//! estimate doesn't make layers flap. A paused track resumes its lowest layer as a probe, first
//! `resume_probe_interval` after the pause and then less and less often while the probes don't
//! keep it (up to `max_resume_probe_interval`): without traffic there is no feedback, and the
//! estimate would never recover.
//!
//! An upgrade can also wait for proof that the link carries it ([`UpgradeProof`]): the estimate
//! grows while the link isn't full, and switching to the next layer to find out stalled the
//! picture of a viewer whose link couldn't carry it (#994). The allocation then asks for a probe
//! of the rate the upgrade needs ([`Allocation::maybe_probe`]) and switches once one showed it.

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
  /// ... or this long when a probe showed the link carries the new total ([`UpgradeProof`]): the
  /// hold rides out a noisy estimate, a probe measured the link.
  pub shown_upswitch_hold: Duration,
  /// A track paused for lack of bandwidth is tried again at its lowest layer this long after the
  /// pause. A link that just lost a burst (a limit setting in, cross traffic) often carries the
  /// lowest layer again within a second or two, while the estimate, cut to what got through of the
  /// burst, needs far longer to grow back: at a steady 5 s, a viewer behind a limited downlink saw
  /// no picture for 5.3 s on a CI runner.
  pub resume_probe_interval: Duration,
  /// Every probe that doesn't keep the track doubles the wait before the next one, up to this: a
  /// link that really can't carry the lowest layer isn't probed every few seconds.
  pub max_resume_probe_interval: Duration,
  /// How long the budget has to stay below a track's lowest layer before it is paused (it drops
  /// to its lowest layer at once). A link that carries the lowest layer with little to spare
  /// (180p on 250 kbit/s) saw its estimate cut below it by the delay of a keyframe, and the
  /// picture stood still for seconds until the track resumed (#994); the estimate is back above
  /// within a second or so.
  pub pause_hold: Duration,
  /// Whether screen shares may be paused for lack of bandwidth like cameras.
  pub can_pause_screen_shares: bool,
  /// How long the budget has to stay below a screen share's layer before it drops. A still
  /// screen sends almost nothing but keyframes of hundreds of kilobytes, and the estimate
  /// reacts to such a burst with a cut it recovers from within a second or two; dropping the
  /// layer at once sent a viewer back to the blurred half-size picture right after the sharp one
  /// arrived, and every switch costs another such keyframe (issue #941). A link that really
  /// can't carry the share stays below for longer.
  pub screen_downswitch_hold: Duration,
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
  /// How long the budget has to stay below the current layer before it drops (zero: at once).
  pub downswitch_hold: Duration,
}

/// What an upgrade needs besides an estimate that covers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradeProof {
  /// Nothing (a receiver that sends no feedback can't be probed).
  NotRequired,
  /// A probe has to have shown that the link carries the new total; it showed this much lately
  /// (0: nothing).
  Shown { bps: u32 },
}

/// The layers of one receiver and the probe they wait for.
#[derive(Debug, Clone, PartialEq)]
pub struct Allocation {
  /// The layer to forward per track (`None` = paused).
  pub layers: HashMap<StreamId, Option<u8>>,
  /// An upgrade is due but not shown yet ([`UpgradeProof::Shown`]): the probe it waits for.
  pub maybe_probe: Option<ProbeRequest>,
}

/// A probe an upgrade waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeRequest {
  /// The rate the link has to carry for the upgrade (audio reserve and headroom included).
  pub target_bps: u32,
  /// The rate that keeps the current layers with headroom: a probe that falls short limits the
  /// estimate no lower, it is no reason to give up what gets through.
  pub floor_bps: u32,
}

/// Remembers the current allocation of one receiver for hysteresis.
#[derive(Debug, Default)]
pub struct LayerAllocator {
  current: HashMap<StreamId, Option<u8>>,
  upgrade_pending_since: HashMap<StreamId, Instant>,
  /// Since when the budget is below the layer of a track with a [`TrackDemand::downswitch_hold`].
  downgrade_pending_since: HashMap<StreamId, Instant>,
  /// Since when the budget is below the lowest layer of a track that is still sent.
  pause_pending_since: HashMap<StreamId, Instant>,
  paused_since: HashMap<StreamId, Instant>,
  /// Resume probes in progress: the lowest layer is kept until then, whatever the estimate says,
  /// so its feedback can lift the estimate.
  probing_until: HashMap<StreamId, Instant>,
  /// The wait before the next resume probe, after probes that didn't keep the track.
  resume_wait: HashMap<StreamId, Duration>,
}

impl Default for LayerAllocationConfig {
  fn default() -> Self {
    Self {
      audio_reserve_bps: 3 * 40_000,
      upswitch_headroom: 1.15,
      upswitch_hold: Duration::from_secs(2),
      shown_upswitch_hold: Duration::from_millis(500),
      resume_probe_interval: Duration::from_secs(2),
      max_resume_probe_interval: Duration::from_secs(8),
      pause_hold: Duration::from_secs(2),
      can_pause_screen_shares: false,
      screen_downswitch_hold: Duration::from_secs(3),
    }
  }
}

impl LayerAllocator {
  pub fn new() -> Self {
    Self::default()
  }

  /// The layer to forward per track (`None` = paused), given the receiver's current estimate,
  /// without proof for upgrades.
  pub fn allocate(
    &mut self,
    config: &LayerAllocationConfig,
    estimate_bps: u32,
    demands: &[TrackDemand<'_>],
    now: Instant,
  ) -> HashMap<StreamId, Option<u8>> {
    self
      .allocate_with_proof(config, estimate_bps, UpgradeProof::NotRequired, demands, now)
      .layers
  }

  /// The layers per track, given the receiver's current estimate and what upgrades need besides.
  pub fn allocate_with_proof(
    &mut self,
    config: &LayerAllocationConfig,
    estimate_bps: u32,
    proof: UpgradeProof,
    demands: &[TrackDemand<'_>],
    now: Instant,
  ) -> Allocation {
    let budget = f64::from(estimate_bps.saturating_sub(config.audio_reserve_bps));
    let ideal = ideal_allocation(budget, demands, 1.0);
    let with_headroom = ideal_allocation(budget, demands, config.upswitch_headroom);
    let shown_bps = match proof {
      UpgradeProof::NotRequired => estimate_bps,
      UpgradeProof::Shown { bps } => bps,
    };
    // What a probe showed the link carries, regardless of the estimate (whether to probe again)
    // and within it (whether to switch up).
    let shown_alone = ideal_allocation(
      f64::from(shown_bps.saturating_sub(config.audio_reserve_bps)),
      demands,
      config.upswitch_headroom,
    );
    let shown_with_headroom = ideal_allocation(
      f64::from(
        shown_bps
          .min(estimate_bps)
          .saturating_sub(config.audio_reserve_bps),
      ),
      demands,
      config.upswitch_headroom,
    );

    let mut result = HashMap::new();
    for demand in demands {
      let stream_id = demand.stream_id;
      let current = self.current.get(&stream_id).copied().flatten();
      let wanted = ideal.get(&stream_id).copied().flatten();
      let affordable_with_headroom = with_headroom.get(&stream_id).copied().flatten();
      if layer_rank(wanted) >= layer_rank(current) {
        // The budget covers the current layer (again).
        self.downgrade_pending_since.remove(&stream_id);
      }
      if wanted.is_some() {
        self.pause_pending_since.remove(&stream_id);
      }
      let chosen = if current.is_none() && !self.current.contains_key(&stream_id) {
        // New subscription: start where the budget allows.
        wanted
      } else if layer_rank(wanted) <= layer_rank(current) {
        // Same or lower: apply immediately, unless the track holds its layer through a dip.
        self.upgrade_pending_since.remove(&stream_id);
        let lowest = demand
          .layers
          .first()
          .map(|info| info.layer)
          .filter(|layer| *layer <= demand.max_layer);
        // A resume probe that ends has had its time.
        let is_probing = self.probing_until.contains_key(&stream_id);
        if wanted.is_none()
          && current.is_some()
          && lowest.is_some()
          && !is_probing
          && !config.pause_hold.is_zero()
        {
          // Down to the lowest layer at once (after a screen share's hold); paused only once
          // the budget stays below that.
          let since = *self.pause_pending_since.entry(stream_id).or_insert(now);
          if now.saturating_duration_since(since) >= config.pause_hold {
            self.pause_pending_since.remove(&stream_id);
            self.downgrade_after_hold(demand, current, None, now)
          } else {
            self.downgrade_after_hold(demand, current, lowest.min(current), now)
          }
        } else {
          self.downgrade_after_hold(demand, current, wanted, now)
        }
      } else if layer_rank(affordable_with_headroom) > layer_rank(current) {
        let since = *self.upgrade_pending_since.entry(stream_id).or_insert(now);
        // Step up one layer at a time; a paused track resumes at its lowest layer.
        let step = match current {
          Some(layer) => Some(layer + 1).min(affordable_with_headroom),
          None => demand.layers.first().map(|info| info.layer),
        };
        let is_shown = match proof {
          UpgradeProof::NotRequired => false,
          UpgradeProof::Shown { .. } => {
            layer_rank(shown_with_headroom.get(&stream_id).copied().flatten()) >= layer_rank(step)
          },
        };
        let hold = if is_shown {
          config.shown_upswitch_hold.min(config.upswitch_hold)
        } else {
          config.upswitch_hold
        };
        if now.saturating_duration_since(since) < hold {
          current
        } else if !is_shown && proof != UpgradeProof::NotRequired {
          // Due, but the link hasn't shown it carries the step: the hold stays over, so the
          // step follows as soon as a probe shows it.
          current
        } else {
          self.upgrade_pending_since.remove(&stream_id);
          step
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
    self
      .downgrade_pending_since
      .retain(|stream_id, _| result.contains_key(stream_id));
    self
      .resume_wait
      .retain(|stream_id, _| result.contains_key(stream_id));
    self
      .pause_pending_since
      .retain(|stream_id, _| result.contains_key(stream_id));
    let maybe_probe = match proof {
      UpgradeProof::NotRequired => None,
      UpgradeProof::Shown { .. } => {
        // Every track below the layer it asked for has a next step; those the link hasn't shown
        // it carries are worth a probe, whether the estimate covers them yet or not: a link that
        // got better is found by the probe, not by an estimate that grows slowly after
        // congestion.
        let unshown: Vec<(StreamId, u8)> = demands
          .iter()
          .filter_map(|demand| {
            let layer = result.get(&demand.stream_id).copied().flatten();
            let step = next_layer(demand, layer)?;
            let shown = shown_alone.get(&demand.stream_id).copied().flatten();
            (layer_rank(shown) < i16::from(step)).then_some((demand.stream_id, step))
          })
          .collect();
        probe_request(config, demands, &result, &unshown)
      },
    };
    Allocation {
      layers: result,
      maybe_probe,
    }
  }

  /// `wanted` (at most `current`) once the budget has been below `current` for the track's
  /// [`TrackDemand::downswitch_hold`]; `current` until then.
  fn downgrade_after_hold(
    &mut self,
    demand: &TrackDemand<'_>,
    current: Option<u8>,
    wanted: Option<u8>,
    now: Instant,
  ) -> Option<u8> {
    // The receiver asks for less, or the layer isn't there to have any more (the publisher's
    // uplink dropped it, its frames are too large for the receiver): no reason to wait.
    let is_still_there = current
      .is_some_and(|layer| layer <= demand.max_layer && demand.layers.iter().any(|info| info.layer == layer));
    if wanted == current || demand.downswitch_hold.is_zero() || !is_still_there {
      return wanted;
    }
    let since = *self
      .downgrade_pending_since
      .entry(demand.stream_id)
      .or_insert(now);
    if now.saturating_duration_since(since) < demand.downswitch_hold {
      return current;
    }
    self.downgrade_pending_since.remove(&demand.stream_id);
    wanted
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
      self.resume_wait.remove(&demand.stream_id);
      return chosen;
    }
    if self
      .probing_until
      .get(&demand.stream_id)
      .is_some_and(|until| now < *until)
    {
      return lowest;
    }
    if self.probing_until.remove(&demand.stream_id).is_some() {
      // The probe ended without the estimate covering the layer: wait longer for the next.
      let wait = self
        .resume_wait
        .entry(demand.stream_id)
        .or_insert(config.resume_probe_interval);
      *wait = (*wait * 2).min(config.max_resume_probe_interval);
    }
    let wait = self
      .resume_wait
      .get(&demand.stream_id)
      .copied()
      .unwrap_or(config.resume_probe_interval);
    let since = *self.paused_since.entry(demand.stream_id).or_insert(now);
    if now.saturating_duration_since(since) < wait {
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

/// The layer above `maybe_layer` (the lowest for a paused track) that `demand` has and asked for.
fn next_layer(demand: &TrackDemand<'_>, maybe_layer: Option<u8>) -> Option<u8> {
  demand
    .layers
    .iter()
    .map(|info| info.layer)
    .find(|layer| i16::from(*layer) > layer_rank(maybe_layer) && *layer <= demand.max_layer)
}

/// The cheapest of the `unshown` steps (track, layer) as a probe, or `None` without any.
fn probe_request(
  config: &LayerAllocationConfig,
  demands: &[TrackDemand<'_>],
  layers: &HashMap<StreamId, Option<u8>>,
  unshown: &[(StreamId, u8)],
) -> Option<ProbeRequest> {
  let bitrate = |demand: &TrackDemand<'_>, maybe_layer: Option<u8>| -> f64 {
    maybe_layer
      .and_then(|layer| demand.layers.iter().find(|info| info.layer == layer))
      .map_or(0.0, |info| f64::from(info.max_bitrate_bps))
  };
  let total_with = |maybe_step: Option<(StreamId, u8)>| -> f64 {
    let video: f64 = demands
      .iter()
      .map(|demand| {
        let layer = match maybe_step {
          Some((stream_id, step)) if stream_id == demand.stream_id => Some(step),
          _ => layers.get(&demand.stream_id).copied().flatten(),
        };
        bitrate(demand, layer)
      })
      .sum();
    f64::from(config.audio_reserve_bps) + video * config.upswitch_headroom
  };
  let target = unshown
    .iter()
    .map(|step| total_with(Some(*step)))
    .min_by(f64::total_cmp)?;
  // Saturating float-to-int conversions are intended.
  Some(ProbeRequest {
    target_bps: target.ceil() as u32,
    floor_bps: total_with(None) as u32,
  })
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

    /// Issue #941: a screen share's keyframe makes the estimate dip; the sharp layer stays
    /// through the dip, and drops only when the budget stays below it.
    #[test]
    fn a_screen_share_keeps_its_layer_through_a_dip() {
      let layers = three_layers();
      let screen = TrackDemand {
        can_pause: false,
        downswitch_hold: Duration::from_secs(3),
        ..demand(1, 0, 2, &layers)
      };
      let demands = [screen];
      let mut allocator = LayerAllocator::new();
      let start = Instant::now();
      assert_eq!(
        allocator.allocate(&config(), 10_000_000, &demands, start)[&StreamId(1)],
        Some(2)
      );
      let layer_at = |allocator: &mut LayerAllocator, millis: u64, estimate: u32| {
        allocator.allocate(
          &config(),
          estimate,
          &demands,
          start + Duration::from_millis(millis),
        )[&StreamId(1)]
      };
      // A dip of two seconds, then the estimate covers the layer again.
      assert_eq!(layer_at(&mut allocator, 100, 1_000_000), Some(2));
      assert_eq!(layer_at(&mut allocator, 2_100, 1_000_000), Some(2));
      assert_eq!(layer_at(&mut allocator, 2_200, 2_000_000), Some(2));
      // A dip that lasts: down after the hold, which starts afresh.
      assert_eq!(layer_at(&mut allocator, 3_000, 1_000_000), Some(2));
      assert_eq!(layer_at(&mut allocator, 5_900, 1_000_000), Some(2));
      assert_eq!(layer_at(&mut allocator, 6_000, 1_000_000), Some(1));
      // Asking for less applies at once.
      let lower = [TrackDemand {
        max_layer: 0,
        ..demands[0].clone()
      }];
      assert_eq!(
        allocator.allocate(
          &config(),
          10_000_000,
          &lower,
          start + Duration::from_millis(6_100)
        )[&StreamId(1)],
        Some(0)
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
      let before = start + Duration::from_millis(1_900);
      assert_eq!(
        allocator.allocate(&config(), 200_000, &demands, before)[&StreamId(1)],
        None
      );
      let after = start + Duration::from_secs(2);
      assert_eq!(
        allocator.allocate(&config(), 200_000, &demands, after)[&StreamId(1)],
        Some(0)
      );
    }

    /// Probes that don't keep the track come less and less often (2, 4, 8, 8 s apart); once the
    /// track runs again, the next pause is probed soon again.
    #[test]
    fn resume_probes_back_off_while_they_fail() {
      let layers = three_layers();
      let mut allocator = LayerAllocator::new();
      let start = Instant::now();
      let demands = [demand(1, 0, 2, &layers)];
      let mut probes_at = Vec::new();
      let mut was_sent = false;
      for tick in 0..=400 {
        let at = start + Duration::from_millis(100 * tick);
        let is_sent = allocator.allocate(&config(), 200_000, &demands, at)[&StreamId(1)].is_some();
        if is_sent && !was_sent {
          probes_at.push(tick / 10);
        }
        was_sent = is_sent;
      }
      // Paused at 0; probes of 2 s at 2, then 2 + 2 + 4, 8 + 2 + 8, 18 + 2 + 8, 28 + 2 + 8.
      assert_eq!(probes_at, vec![2, 8, 18, 28, 38]);

      // The estimate covers the layer (an upgrade from paused waits for the hold): the next pause
      // is probed after 2 s again.
      let covered = start + Duration::from_secs(41);
      allocator.allocate(&config(), 400_000, &demands, covered);
      let resumed = covered + Duration::from_secs(2);
      assert_eq!(
        allocator.allocate(&config(), 400_000, &demands, resumed)[&StreamId(1)],
        Some(0)
      );
      let below = resumed + Duration::from_millis(100);
      assert_eq!(
        allocator.allocate(&config(), 200_000, &demands, below)[&StreamId(1)],
        Some(0),
        "the pause hold"
      );
      let paused = below + Duration::from_secs(2);
      assert_eq!(
        allocator.allocate(&config(), 200_000, &demands, paused)[&StreamId(1)],
        None
      );
      assert_eq!(
        allocator.allocate(&config(), 200_000, &demands, paused + Duration::from_secs(2))[&StreamId(1)],
        Some(0)
      );
    }
  }

  mod proof {
    use super::*;

    /// #994: an upgrade the estimate covers waits until a probe showed the link carries it; the
    /// allocation asks for that probe, and for one of the next step even before the estimate
    /// covers it.
    #[test]
    fn an_upgrade_waits_for_a_probe_that_shows_the_link_carries_it() {
      let layers = three_layers();
      let mut allocator = LayerAllocator::new();
      let start = Instant::now();
      let demands = [demand(1, 0, 2, &layers)];
      let at = |seconds: f64| start + Duration::from_secs_f64(seconds);
      let nothing_shown = UpgradeProof::Shown { bps: 0 };
      let first = allocator.allocate_with_proof(&config(), 400_000, nothing_shown, &demands, at(0.0));
      assert_eq!(first.layers[&StreamId(1)], Some(0));
      // 120k audio + 1.15 x 500k for layer 1; 120k + 1.15 x 150k keep layer 0.
      let request = ProbeRequest {
        target_bps: 695_000,
        floor_bps: 292_500,
      };
      assert_eq!(
        first.maybe_probe,
        Some(request),
        "a probe before the estimate covers it"
      );
      let due = allocator.allocate_with_proof(&config(), 3_000_000, nothing_shown, &demands, at(0.1));
      assert_eq!(due.layers[&StreamId(1)], Some(0));
      let held = allocator.allocate_with_proof(&config(), 3_000_000, nothing_shown, &demands, at(2.5));
      assert_eq!(held.layers[&StreamId(1)], Some(0), "past the hold, but not shown");
      assert_eq!(held.maybe_probe, Some(request));
      let shown = UpgradeProof::Shown { bps: 700_000 };
      let up = allocator.allocate_with_proof(&config(), 3_000_000, shown, &demands, at(2.6));
      assert_eq!(up.layers[&StreamId(1)], Some(1), "shown: up at once");
      // The next step, 120k + 1.15 x 1.5M, isn't shown by 700k.
      assert_eq!(up.maybe_probe.map(|probe| probe.target_bps), Some(1_845_000));
      // Shown, the next step needs only the short hold.
      let more = UpgradeProof::Shown { bps: 2_000_000 };
      assert_eq!(
        allocator
          .allocate_with_proof(&config(), 3_000_000, more, &demands, at(2.7))
          .layers[&StreamId(1)],
        Some(1)
      );
      let top = allocator.allocate_with_proof(&config(), 3_000_000, more, &demands, at(3.2));
      assert_eq!(top.layers[&StreamId(1)], Some(2));
      assert_eq!(top.maybe_probe, None, "at the top: nothing to probe");
    }

    /// A probe that showed the step stands until it expires, also while the estimate doesn't
    /// cover the step yet (its fast start): no burst after burst meanwhile.
    #[test]
    fn a_shown_step_is_not_probed_again() {
      let layers = three_layers();
      let mut allocator = LayerAllocator::new();
      let demands = [demand(1, 0, 2, &layers)];
      let shown = UpgradeProof::Shown { bps: 700_000 };
      let allocation = allocator.allocate_with_proof(&config(), 400_000, shown, &demands, Instant::now());
      assert_eq!(
        allocation.layers[&StreamId(1)],
        Some(0),
        "the estimate doesn't cover it"
      );
      assert_eq!(allocation.maybe_probe, None);
    }
  }

  mod pausing {
    use super::*;

    /// #994: a dip of the estimate below the lowest layer doesn't pause the track at once; one
    /// that lasts does. A higher layer drops to the lowest at once.
    #[test]
    fn a_track_is_paused_only_when_the_budget_stays_below_its_lowest_layer() {
      let layers = three_layers();
      let mut allocator = LayerAllocator::new();
      let start = Instant::now();
      let demands = [demand(1, 0, 2, &layers)];
      let layer_at = |allocator: &mut LayerAllocator, millis: u64, estimate: u32| {
        allocator.allocate(
          &config(),
          estimate,
          &demands,
          start + Duration::from_millis(millis),
        )[&StreamId(1)]
      };
      assert_eq!(layer_at(&mut allocator, 0, 1_000_000), Some(1));
      assert_eq!(layer_at(&mut allocator, 100, 200_000), Some(0));
      assert_eq!(layer_at(&mut allocator, 1_500, 200_000), Some(0));
      assert_eq!(layer_at(&mut allocator, 1_600, 300_000), Some(0), "back above");
      assert_eq!(layer_at(&mut allocator, 1_700, 200_000), Some(0));
      assert_eq!(layer_at(&mut allocator, 3_600, 200_000), Some(0));
      assert_eq!(layer_at(&mut allocator, 3_700, 200_000), None);
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
      downswitch_hold: Duration::ZERO,
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
