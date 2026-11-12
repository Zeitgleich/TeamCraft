//! "Stats for nerds" (#938): what each video of the active call does right now, for the overlay
//! on its tile and for bug reports.
//!
//! The desktop shell (`call_video_stats`) and the browser's call module (`video_stats`, see
//! [`crate::page_calls`]) answer with a [`VideoStatsReport`]. Its counters only grow during a
//! track's life; the UI turns two reports a second apart into rates. A value the platform can't
//! tell is `None` (shown as "–"), never made up.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{MediaStreamId, VideoCodec, VideoTrackKind};

/// A [`VideoStatsReport`] as the media layer answered it (JSON), read with
/// [`VideoStatsJson::read`] where the stats are shown: the web client's call screen loads with
/// its own chunk, and the reading goes there with it instead of into every first visit
/// (robustness.md, "Download size").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoStatsJson(pub String);

/// The video of the active call at one moment.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VideoStatsReport {
  /// When the counters were read: milliseconds on a clock of the driver that only goes forward.
  pub at_ms: u64,
  /// Round trip to the SFU, as the connection measures it.
  #[serde(default)]
  pub maybe_rtt_ms: Option<u32>,
  /// What the send side estimates for this device's uplink.
  #[serde(default)]
  pub maybe_uplink: Option<UplinkStats>,
  /// What the probe of this device's uplink at the start of the call showed (#1762); `None`
  /// before it ended, and where the SFU takes no probe.
  #[serde(default)]
  pub maybe_uplink_probe: Option<LinkProbeStats>,
  /// What the SFU's probe of this device's downlink showed.
  #[serde(default)]
  pub maybe_downlink_probe: Option<LinkProbeStats>,
  #[serde(default)]
  pub tracks: Vec<VideoTrackStats>,
}

/// What a probe of a link at the start of a call showed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkProbeStats {
  pub capacity_bps: u32,
  pub loss_percent: u8,
}

/// The uplink as the bandwidth estimator sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UplinkStats {
  pub estimate_bps: u32,
  pub loss_percent: u8,
}

/// One video track: an own one (sent) or another device's (received).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "side", rename_all = "snake_case")]
pub enum VideoTrackStats {
  Sent(SentVideoStats),
  Received(ReceivedVideoStats),
}

/// An own camera or screen share.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentVideoStats {
  pub stream_id: MediaStreamId,
  pub kind: VideoTrackKind,
  pub codec: VideoCodec,
  #[serde(default)]
  pub maybe_encoder: Option<VideoEncoderInfo>,
  /// The size the camera or screen delivers (before scaling to the layers).
  #[serde(default)]
  pub maybe_capture: Option<VideoSize>,
  /// The simulcast layers, lowest first.
  pub layers: Vec<SentLayerStats>,
  pub packets: u64,
  pub bytes: u64,
  #[serde(default)]
  pub maybe_frames_captured: Option<u64>,
  /// Pictures that were encoded but not sent: the track not confirmed, the call not yet secured,
  /// or encryption failed.
  pub pictures_dropped: u64,
  /// Captured frames that showed nothing but black.
  #[serde(default)]
  pub maybe_frames_black: Option<u64>,
  /// Frames handed to the layers' encoders, one per layer and picture (the browser's WebCodecs
  /// encoders; screen-share quality, #1733).
  #[serde(default)]
  pub maybe_frames_given: Option<u64>,
  /// Frames a layer's encoder didn't get because its queue was full: the encoder doesn't keep up.
  #[serde(default)]
  pub maybe_frames_skipped: Option<u64>,
  /// How long the uplink held the top layer back (it carried only lower ones), in milliseconds.
  #[serde(default)]
  pub maybe_held_back_ms: Option<u64>,
  /// What holds an own camera below its plan (#1762); `None` where nothing governs it (screen
  /// shares, the desktop's encoders).
  #[serde(default)]
  pub maybe_camera_limits: Option<CameraLimitsStats>,
}

/// Why an own camera sends less than it was planned with (#1762): its encoders didn't keep up
/// (the frame rate first, then the top layers), or a layer's encoder sent far more than its
/// bitrate (configured lower, then given fewer frames).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraLimitsStats {
  /// The frame rate the camera was started with.
  pub planned_framerate: u8,
  /// The frame rate it is captured at now.
  pub framerate: u8,
  /// Top layers left out because the encoders didn't keep up.
  pub top_layers_off: u8,
  /// Why the encoders were found overloaded, while the camera runs below its plan.
  #[serde(default)]
  pub maybe_overload: Option<EncoderOverload>,
  /// Layers held below their bitrate or frame rate because their encoder overshot.
  #[serde(default)]
  pub layers: Vec<LayerLimitStats>,
}

/// How a camera's encoders were found overloaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncoderOverload {
  /// Frames found an encoder's queue full and were skipped.
  SkippedFrames,
  /// Frames waited in the encoders' queues.
  QueuedFrames,
  /// Encoding took most of the frame interval.
  SlowEncoding,
  #[serde(other)]
  Unknown,
}

/// A layer whose encoder overshot its bitrate, and what it is held to now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerLimitStats {
  pub layer: u8,
  pub bitrate_bps: u32,
  pub framerate: u8,
}

impl CameraLimitsStats {
  /// Whether anything holds the camera below its plan.
  pub fn is_limited(&self) -> bool {
    self.framerate < self.planned_framerate || self.top_layers_off > 0 || !self.layers.is_empty()
  }
}

/// One simulcast layer of an own track.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentLayerStats {
  pub layer: u8,
  pub size: VideoSize,
  /// The layer is wanted: someone receives it and the uplink carries it.
  pub is_target: bool,
  pub pictures: u64,
  pub bytes: u64,
  /// Pictures of the layer that went out as keyframes.
  #[serde(default)]
  pub keyframes: u64,
}

/// Which encoder works for an own track.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoEncoderInfo {
  /// E.g. `vtenc_h264`, `OpenH264`, `WebCodecs`; several joined with ", " when layers differ.
  pub name: String,
  pub implementation: EncoderImplementation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncoderImplementation {
  Hardware,
  Software,
  /// The browser decides (WebCodecs); it doesn't say which.
  Browser,
}

/// Another device's track this device receives.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReceivedVideoStats {
  pub stream_id: MediaStreamId,
  pub kind: VideoTrackKind,
  pub codec: VideoCodec,
  /// The layers the sender offers, lowest first.
  pub layers: Vec<OfferedLayer>,
  /// The layer arriving now.
  #[serde(default)]
  pub maybe_layer: Option<u8>,
  /// The highest layer this device asked for; `None` while not subscribed.
  #[serde(default)]
  pub maybe_max_layer: Option<u8>,
  pub packets: u64,
  pub bytes: u64,
  pub packets_lost: u64,
  /// Packets that went missing, were asked for again and came (#1762). Older reports lack it.
  #[serde(default)]
  pub packets_recovered: u64,
  /// Complete, decrypted frames handed to the decoder.
  pub frames: u64,
  /// Complete frames that couldn't be used: a reference went missing, or they didn't decrypt.
  pub frames_discarded: u64,
  /// Interarrival jitter (RFC 3550) of the track's packets.
  #[serde(default)]
  pub maybe_jitter_ms: Option<f32>,
  /// Keyframes this device asked the SFU for (a lost packet, a frame that didn't decrypt).
  #[serde(default)]
  pub keyframe_requests: u64,
  /// The longest time between two frames handed to the decoder while subscribed, in
  /// milliseconds: the longest the picture stood still. A still screen shared from the desktop
  /// app sends about one picture a second.
  #[serde(default)]
  pub maybe_longest_gap_ms: Option<u64>,
  /// The frame rate the sender announced for the layer arriving now.
  #[serde(default)]
  pub maybe_max_framerate: Option<u8>,
}

/// A layer another device offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferedLayer {
  pub layer: u8,
  pub size: VideoSize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VideoSize {
  pub width: u16,
  pub height: u16,
}

impl VideoStatsJson {
  /// The report; `None` for text that isn't one (`null` outside a call).
  pub fn read(&self) -> Option<VideoStatsReport> {
    serde_json::from_str::<Option<VideoStatsReport>>(&self.0)
      .ok()
      .flatten()
  }
}

impl VideoStatsReport {
  /// The own track of `kind`; the newest while a replaced one is still listed.
  pub fn sent(&self, kind: &VideoTrackKind) -> Option<&SentVideoStats> {
    self.tracks.iter().rev().find_map(|track| match track {
      VideoTrackStats::Sent(sent) if sent.kind == *kind => Some(sent),
      _ => None,
    })
  }

  /// Another device's track.
  pub fn received(&self, stream_id: MediaStreamId) -> Option<&ReceivedVideoStats> {
    self.tracks.iter().find_map(|track| match track {
      VideoTrackStats::Received(received) if received.stream_id == stream_id => Some(received),
      _ => None,
    })
  }
}

impl SentVideoStats {
  /// The highest layer pictures went out on, with the counters of `before` (the report a moment
  /// earlier, if any) telling which layers sent since.
  pub fn top_active_layer(&self, maybe_before: Option<&SentVideoStats>) -> Option<&SentLayerStats> {
    self.active_layers(maybe_before).last().copied()
  }

  /// The layers that sent pictures since `before`, lowest first; without `before`, the layers
  /// that sent any.
  pub fn active_layers(&self, maybe_before: Option<&SentVideoStats>) -> Vec<&SentLayerStats> {
    self
      .layers
      .iter()
      .filter(|layer| {
        let earlier = maybe_before
          .and_then(|before| before.layers.iter().find(|old| old.layer == layer.layer))
          .map_or(0, |old| old.pictures);
        layer.pictures > earlier
      })
      .collect()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_report_reads_back_from_its_wire_form() {
    let report = VideoStatsReport {
      at_ms: 12_000,
      maybe_rtt_ms: Some(34),
      maybe_uplink: Some(UplinkStats {
        estimate_bps: 2_500_000,
        loss_percent: 1,
      }),
      maybe_uplink_probe: Some(LinkProbeStats {
        capacity_bps: 4_000_000,
        loss_percent: 0,
      }),
      maybe_downlink_probe: None,
      tracks: vec![
        VideoTrackStats::Sent(sent(&[(0, 10), (1, 0)])),
        VideoTrackStats::Received(ReceivedVideoStats {
          stream_id: MediaStreamId(7),
          kind: VideoTrackKind::Screen,
          codec: VideoCodec::H264,
          layers: vec![],
          maybe_layer: Some(0),
          maybe_max_layer: Some(2),
          packets: 10,
          bytes: 9_000,
          packets_lost: 1,
          packets_recovered: 0,
          frames: 3,
          frames_discarded: 0,
          maybe_jitter_ms: Some(4.5),
          keyframe_requests: 2,
          maybe_longest_gap_ms: Some(120),
          maybe_max_framerate: Some(30),
        }),
      ],
    };
    let wire = serde_json::to_string(&report).unwrap();
    assert!(wire.contains(r#""side":"sent""#), "{wire}");
    assert!(wire.contains(r#""implementation":"hardware""#), "{wire}");
    assert_eq!(VideoStatsJson(wire).read(), Some(report));
  }

  #[test]
  fn no_report_reads_as_none() {
    assert_eq!(VideoStatsJson("null".to_owned()).read(), None);
    assert_eq!(VideoStatsJson("{".to_owned()).read(), None);
  }

  #[test]
  fn a_report_without_optional_parts_reads() {
    let report: VideoStatsReport = serde_json::from_str(r#"{"at_ms":5}"#).unwrap();
    assert_eq!(
      report,
      VideoStatsReport {
        at_ms: 5,
        ..VideoStatsReport::default()
      }
    );
  }

  #[test]
  fn active_layers_are_those_that_sent_since_the_last_report() {
    let before = sent(&[(0, 10), (1, 5), (2, 7)]);
    let now = sent(&[(0, 40), (1, 35), (2, 7)]);
    let active: Vec<u8> = now
      .active_layers(Some(&before))
      .iter()
      .map(|layer| layer.layer)
      .collect();
    assert_eq!(active, [0, 1]);
    assert_eq!(
      now.top_active_layer(Some(&before)).map(|layer| layer.layer),
      Some(1)
    );
    // Without an earlier report, every layer that ever sent counts.
    assert_eq!(now.top_active_layer(None).map(|layer| layer.layer), Some(2));
    assert_eq!(sent(&[(0, 0)]).top_active_layer(None), None);
  }

  fn sent(layers: &[(u8, u64)]) -> SentVideoStats {
    SentVideoStats {
      stream_id: MediaStreamId(3),
      kind: VideoTrackKind::Camera,
      codec: VideoCodec::H264,
      maybe_encoder: Some(VideoEncoderInfo {
        name: "vtenc_h264".to_owned(),
        implementation: EncoderImplementation::Hardware,
      }),
      maybe_capture: None,
      layers: layers
        .iter()
        .map(|&(layer, pictures)| SentLayerStats {
          layer,
          size: VideoSize {
            width: 320 << layer,
            height: 180 << layer,
          },
          is_target: true,
          pictures,
          bytes: pictures * 1000,
          keyframes: 1,
        })
        .collect(),
      packets: 0,
      bytes: 0,
      maybe_frames_captured: None,
      pictures_dropped: 0,
      maybe_frames_black: None,
      maybe_frames_given: None,
      maybe_frames_skipped: None,
      maybe_held_back_ms: Some(0),
      maybe_camera_limits: None,
    }
  }
}
