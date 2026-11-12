//! A native screen share on the share policy (#1777): the desktop's encoders follow the same
//! ladder as the browser's ([`crate::share_policy`]). The encoder thread measures its load, the
//! call task feeds it and the uplink's view of the top layer into a [`ShareGovernor`], and each
//! new step becomes a [`ShareChange`]: rates the running encoders take in place (#1781), or new
//! layers the track is published anew with, as the web client does.

use teamcraft_proto::VideoLayerInfo;
use tracing::info;
// `std::time::Instant` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

use crate::camera_governor::{CameraGovernorConfig, FrameTally};
use crate::share_policy::{ShareGovernor, ShareLimits, SharePlan, UplinkTop};
use crate::video::within_layer_limit;

/// A screen share's governor and the layers its encoders were last told.
#[derive(Debug, Clone)]
pub struct ShareSteps {
  governor: ShareGovernor,
  /// The most layers the SFU takes ([`within_layer_limit`]).
  max_layers: u8,
  layers: Vec<VideoLayerInfo>,
}

/// How a share's encoders follow a new step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareChange {
  /// Only bitrates and frame rates changed: the running encoders take them, in place where they
  /// can, and the track stays published.
  Rates(Vec<VideoLayerInfo>),
  /// A layer came or went or changed size: new encoders, each starting with a keyframe, under a
  /// new publication.
  Layers(Vec<VideoLayerInfo>),
}

impl ShareSteps {
  /// Starts at the full plan of `plan`, within `max_layers`.
  pub fn new(plan: SharePlan, max_layers: u8, now: Instant) -> Self {
    Self::with_config(plan, CameraGovernorConfig::default(), max_layers, now)
  }

  pub fn with_config(plan: SharePlan, config: CameraGovernorConfig, max_layers: u8, now: Instant) -> Self {
    let governor = ShareGovernor::with_config(plan, config, now);
    let layers = within_layer_limit(&governor.layers(), max_layers);
    Self {
      governor,
      max_layers,
      layers,
    }
  }

  /// The layers the encoders were last told.
  pub fn layers(&self) -> &[VideoLayerInfo] {
    &self.layers
  }

  /// The encoders' load since the last call: a change once a load window judged it.
  pub fn on_load(&mut self, tally: FrameTally, now: Instant) -> Option<ShareChange> {
    self.governor.on_frame(tally);
    let level = self.governor.evaluate(now)?;
    let step = self.governor.step();
    info!(
      level,
      top_framerate = step.top_framerate,
      has_middle = step.has_middle,
      top_lines = ?step.maybe_top.map(|top| top.width.min(top.height)),
      reason = ?self.governor.limits().maybe_reason,
      "screen share encoders' load changed the share's step"
    );
    self.follow_step(now)
  }

  /// How the uplink treats the top layer now: where it can't carry it even at its least frame
  /// rate, the top steps to a smaller size, and back once it carried it whole.
  pub fn on_uplink(&mut self, top: UplinkTop, now: Instant) -> Option<ShareChange> {
    if !self.governor.on_uplink(top, now) {
      return None;
    }
    info!(
      top_lines = ?self.governor.step().maybe_top.map(|top| top.width.min(top.height)),
      "the uplink changed the screen share's top layer size"
    );
    self.follow_step(now)
  }

  /// What holds the share below its plan (stats for nerds).
  pub fn limits(&self) -> ShareLimits {
    self.governor.limits()
  }

  /// The change that brings the encoders to the governor's step, if any. New layers are measured
  /// afresh: their encoders start anew.
  fn follow_step(&mut self, now: Instant) -> Option<ShareChange> {
    let layers = within_layer_limit(&self.governor.layers(), self.max_layers);
    if layers == self.layers {
      return None;
    }
    let is_same_shape = layers.len() == self.layers.len()
      && layers.iter().zip(&self.layers).all(|(new, old)| {
        (new.layer, new.width, new.height, &new.maybe_codec)
          == (old.layer, old.width, old.height, &old.maybe_codec)
      });
    self.layers = layers.clone();
    if is_same_shape {
      return Some(ShareChange::Rates(layers));
    }
    self.governor.measure_afresh(now);
    Some(ShareChange::Layers(layers))
  }
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use teamcraft_proto::{ScreenContentMode, ScreenShareResolution, VideoCodec};

  use super::*;
  use crate::share_policy::{EncoderKind, ShareCodecs, ShareEncoders};
  use crate::video::{ScreenProfile, VideoResolution};

  #[test]
  fn a_held_top_layer_steps_its_size_down_and_back_with_new_layers() {
    let start = Instant::now();
    let mut steps = ShareSteps::new(plan(), 3, start);
    let full = steps.layers().to_vec();
    assert!(steps.on_uplink(UplinkTop::Held, start).is_none());
    let Some(ShareChange::Layers(smaller)) = steps.on_uplink(UplinkTop::Held, start + Duration::from_secs(2))
    else {
      panic!("a top held for a window steps down a size");
    };
    let top = |layers: &[VideoLayerInfo]| layers.last().map(|info| info.height);
    assert!(top(&smaller) < top(&full), "{smaller:?}");
    let mut now = start + Duration::from_secs(2);
    let mut maybe_back = None;
    while maybe_back.is_none() && now < start + Duration::from_secs(60) {
      now += Duration::from_millis(500);
      maybe_back = steps.on_uplink(UplinkTop::Whole, now);
    }
    assert_eq!(maybe_back, Some(ShareChange::Layers(full)));
  }

  #[test]
  fn the_layers_stay_within_the_sfus_limit() {
    let steps = ShareSteps::new(plan(), 2, Instant::now());
    assert_eq!(steps.layers().len(), 2);
    assert_eq!(steps.layers()[0].layer, 0);
  }

  fn plan() -> SharePlan {
    SharePlan {
      source: VideoResolution {
        width: 2560,
        height: 1440,
      },
      framerate: 30,
      profile: ScreenProfile {
        mode: ScreenContentMode::Detail,
        size: ScreenShareResolution::Standard,
      },
      codecs: ShareCodecs {
        by_all: vec![VideoCodec::H264],
        by_some: Vec::new(),
      },
      encoders: ShareEncoders {
        av1: EncoderKind::Unavailable,
        vp9: EncoderKind::Unavailable,
      },
    }
  }
}
