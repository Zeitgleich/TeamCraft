//! How smooth a call's screen shares were, as performance data (#1733, docs/crash-reports.md
//! "Screen shares"): while this device shares its screen or receives another's, the media layer's
//! [`VideoStatsReport`] is read every [`POLL_MS`] (the same report "stats for nerds" shows). When a
//! share ends, the sharer and each viewer send a `call.screen_share` transaction with what it did,
//! and a sampled warning `ScreenShareChoppy` when it was choppy ([`Choppy`]).
//!
//! Only numbers and names from the code leave the page: the codec, the kind of encoder, a size
//! class, counts and rates. The stream IDs only find a share again between two reports.

use std::cell::{Cell, RefCell};

use teamcraft_client_api::crash_reporting::PerfUnit;
use teamcraft_client_api::video_stats::{
  EncoderImplementation, ReceivedVideoStats, SentVideoStats, VideoSize, VideoStatsReport, VideoTrackStats,
};
use teamcraft_proto::{MediaStreamId, VideoCodec, VideoTrackKind};

/// How often the media layer is asked while a screen share runs.
pub const POLL_MS: u64 = 2_000;
/// A share looked at for less than this says nothing about how smooth it was: no rates, no
/// warning (its transaction still goes).
pub const MIN_SECONDS: f64 = 10.0;
/// A sharer is choppy when its encoders had no room for more than this share of the frames
/// (`skipped_queue_full`): the encoder doesn't keep up with the frame rate.
pub const CHOPPY_SKIPPED_SHARE: f64 = 0.1;
/// A viewer is choppy when it discarded more than this share of the complete frames it got (a
/// picture they build on went missing): its decoded rate was far below the rate that came.
pub const CHOPPY_DISCARDED_SHARE: f64 = 0.1;
/// ... or when its picture stood still this long at least. A still screen from the desktop app
/// sends a picture a second; a lost keyframe waits for the next (every 4 s, or on request).
pub const CHOPPY_FREEZE_MS: u64 = 2_000;
/// `call.screen_share` transactions per page life at most.
const MAX_REPORTS: u32 = 20;
/// `ScreenShareChoppy` warnings per page life at most.
const MAX_CHOPPY: u32 = 3;
/// Shares measured at once at most (a call with many screens): more are not measured.
const MAX_SHARES: usize = 8;

/// One share, from this device's side: its own (sent) or another device's (received).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShareKey {
  Sent(MediaStreamId),
  Received(MediaStreamId),
}

/// The screen shares seen in the reports so far, until they end.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShareMeter {
  shares: Vec<Share>,
}

#[derive(Clone, Debug, PartialEq)]
struct Share {
  key: ShareKey,
  first_at_ms: u64,
  last_at_ms: u64,
  side: Side,
}

#[derive(Clone, Debug, PartialEq)]
enum Side {
  Sent {
    first: Box<SentVideoStats>,
    last: Box<SentVideoStats>,
    uplink: Uplink,
  },
  Received {
    first: Box<ReceivedVideoStats>,
    last: Box<ReceivedVideoStats>,
    /// Looks at which a layer was asked for, and those at which a lower one arrived.
    layer_looks: u32,
    below_asked: u32,
    max_jitter_ms: f32,
  },
}

/// The uplink as the reports saw it during a share.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Uplink {
  looks: u32,
  min_bps: u32,
  sum_bps: u64,
  max_loss_percent: u8,
  max_rtt_ms: u32,
}

/// What a share did, once it ended.
#[derive(Clone, Debug, PartialEq)]
pub enum ShareSummary {
  Sent(SenderSummary),
  Received(ViewerSummary),
}

/// The sharer's side.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SenderSummary {
  /// Between the first and the last report: what the rates cover.
  pub seconds: f64,
  pub codec: &'static str,
  pub encoder: &'static str,
  pub maybe_capture: Option<VideoSize>,
  /// The highest layer that sent pictures meanwhile (else the highest offered).
  pub maybe_top: Option<VideoSize>,
  pub maybe_captured_fps: Option<f64>,
  pub top_fps: f64,
  /// Of the frames meant for the encoders, the share they had no room for.
  pub maybe_skipped_share: Option<f64>,
  pub bitrate_kbps: f64,
  /// The counts below are since the share started.
  pub maybe_frames_captured: Option<u64>,
  pub top_pictures: u64,
  pub top_keyframes: u64,
  pub maybe_frames_given: Option<u64>,
  pub maybe_frames_skipped: Option<u64>,
  pub pictures_dropped: u64,
  pub maybe_held_back_ms: Option<u64>,
  pub maybe_uplink_min_kbps: Option<f64>,
  pub maybe_uplink_avg_kbps: Option<f64>,
  pub maybe_uplink_max_loss_percent: Option<u8>,
  pub maybe_max_rtt_ms: Option<u32>,
}

/// A viewer's side.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewerSummary {
  pub seconds: f64,
  pub codec: &'static str,
  /// Frames handed to the decoder per second.
  pub decoded_fps: f64,
  /// The rate the sharer announced for the layer arriving at the end.
  pub maybe_max_framerate: Option<u8>,
  /// Of the complete frames that came, the share that couldn't be used.
  pub discarded_share: f64,
  /// Of the packets meant for this device, the share that never came.
  pub lost_share: f64,
  pub bitrate_kbps: f64,
  /// The counts below are since this device received the share.
  pub frames: u64,
  pub frames_discarded: u64,
  pub packets_lost: u64,
  pub keyframe_requests: u64,
  pub maybe_longest_gap_ms: Option<u64>,
  pub max_jitter_ms: f64,
  /// Of the looks at which a layer was asked for, the share at which a lower one arrived.
  pub maybe_below_asked_share: Option<f64>,
  pub maybe_layer: Option<u8>,
  pub maybe_asked_layer: Option<u8>,
  pub maybe_top_offered: Option<u8>,
  pub maybe_size: Option<VideoSize>,
}

/// Why a share was choppy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choppy {
  /// The sharer's encoders skipped more than [`CHOPPY_SKIPPED_SHARE`] of the frames.
  EncoderSkipped,
  /// A viewer discarded more than [`CHOPPY_DISCARDED_SHARE`] of the frames that came.
  FramesDiscarded,
  /// A viewer's picture stood still for [`CHOPPY_FREEZE_MS`] or longer.
  Froze,
}

/// A summary as a transaction's tags, measurements and its step's data: names from the code,
/// numbers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Described {
  pub tags: Vec<(&'static str, &'static str)>,
  pub measurements: Vec<(&'static str, f64, PerfUnit)>,
  pub span_description: &'static str,
  pub data: Vec<(&'static str, f64)>,
}

impl ShareMeter {
  /// Takes a report: shares listed for the first time start, those no longer listed (or whose
  /// clock went back: another call) end, with their summary.
  pub fn observe(&mut self, report: &VideoStatsReport) -> (Vec<ShareKey>, Vec<(ShareKey, ShareSummary)>) {
    let at_ms = report.at_ms;
    let mut ended = Vec::new();
    let mut kept = Vec::with_capacity(self.shares.len());
    for share in self.shares.drain(..) {
      if at_ms >= share.last_at_ms && is_listed(report, share.key) {
        kept.push(share);
      } else {
        ended.push((share.key, share.summary()));
      }
    }
    self.shares = kept;
    let mut started = Vec::new();
    for track in &report.tracks {
      let key = match track {
        VideoTrackStats::Sent(sent) if sent.kind == VideoTrackKind::Screen => ShareKey::Sent(sent.stream_id),
        VideoTrackStats::Received(received) if received.kind == VideoTrackKind::Screen => {
          ShareKey::Received(received.stream_id)
        },
        _ => continue,
      };
      if let Some(share) = self.shares.iter_mut().find(|share| share.key == key) {
        share.observe(at_ms, track, report);
      } else if self.shares.len() < MAX_SHARES {
        self.shares.push(Share::new(key, at_ms, track, report));
        started.push(key);
      }
    }
    (started, ended)
  }

  /// Ends every share (the call ended, or nothing is shared any more).
  pub fn finish_all(&mut self) -> Vec<(ShareKey, ShareSummary)> {
    self
      .shares
      .drain(..)
      .map(|share| (share.key, share.summary()))
      .collect()
  }

  pub fn is_empty(&self) -> bool {
    self.shares.is_empty()
  }
}

fn is_listed(report: &VideoStatsReport, key: ShareKey) -> bool {
  report.tracks.iter().any(|track| match (track, key) {
    (VideoTrackStats::Sent(sent), ShareKey::Sent(stream_id)) => sent.stream_id == stream_id,
    (VideoTrackStats::Received(received), ShareKey::Received(stream_id)) => received.stream_id == stream_id,
    _ => false,
  })
}

impl Share {
  fn new(key: ShareKey, at_ms: u64, track: &VideoTrackStats, report: &VideoStatsReport) -> Self {
    let side = match track {
      VideoTrackStats::Sent(sent) => Side::Sent {
        first: Box::new(sent.clone()),
        last: Box::new(sent.clone()),
        uplink: Uplink::default(),
      },
      VideoTrackStats::Received(received) => Side::Received {
        first: Box::new(received.clone()),
        last: Box::new(received.clone()),
        layer_looks: 0,
        below_asked: 0,
        max_jitter_ms: 0.0,
      },
    };
    let mut share = Self {
      key,
      first_at_ms: at_ms,
      last_at_ms: at_ms,
      side,
    };
    share.look(report, track);
    share
  }

  fn observe(&mut self, at_ms: u64, track: &VideoTrackStats, report: &VideoStatsReport) {
    self.last_at_ms = at_ms;
    match (&mut self.side, track) {
      (Side::Sent { last, .. }, VideoTrackStats::Sent(sent)) => **last = sent.clone(),
      (Side::Received { last, .. }, VideoTrackStats::Received(received)) => **last = received.clone(),
      _ => return,
    }
    self.look(report, track);
  }

  /// What only the moment tells: the uplink, the layer arriving, the jitter.
  fn look(&mut self, report: &VideoStatsReport, track: &VideoTrackStats) {
    match (&mut self.side, track) {
      (Side::Sent { uplink, .. }, VideoTrackStats::Sent(_)) => {
        if let Some(now) = report.maybe_uplink {
          uplink.min_bps = if uplink.looks == 0 {
            now.estimate_bps
          } else {
            uplink.min_bps.min(now.estimate_bps)
          };
          uplink.looks += 1;
          uplink.sum_bps += u64::from(now.estimate_bps);
          uplink.max_loss_percent = uplink.max_loss_percent.max(now.loss_percent);
        }
        if let Some(rtt_ms) = report.maybe_rtt_ms {
          uplink.max_rtt_ms = uplink.max_rtt_ms.max(rtt_ms);
        }
      },
      (
        Side::Received {
          layer_looks,
          below_asked,
          max_jitter_ms,
          ..
        },
        VideoTrackStats::Received(received),
      ) => {
        if let (Some(layer), Some(asked)) = (received.maybe_layer, received.maybe_max_layer) {
          // Asked for more than is offered: the top offered is all there is.
          let top_offered = received.layers.iter().map(|offered| offered.layer).max();
          let reachable = top_offered.map_or(asked, |top| asked.min(top));
          *layer_looks += 1;
          *below_asked += u32::from(layer < reachable);
        }
        if let Some(jitter) = received.maybe_jitter_ms {
          *max_jitter_ms = max_jitter_ms.max(jitter);
        }
      },
      _ => {},
    }
  }

  fn summary(&self) -> ShareSummary {
    let seconds = self.last_at_ms.saturating_sub(self.first_at_ms) as f64 / 1000.0;
    let per_second = |count: u64| {
      if seconds > 0.0 {
        count as f64 / seconds
      } else {
        0.0
      }
    };
    match &self.side {
      Side::Sent { first, last, uplink } => {
        let maybe_top = last
          .top_active_layer(Some(first))
          .or_else(|| last.layers.last())
          .copied();
        let maybe_top_before = maybe_top
          .and_then(|top| first.layers.iter().find(|layer| layer.layer == top.layer))
          .copied();
        let top_pictures = maybe_top.map_or(0, |top| top.pictures);
        let top_pictures_before = maybe_top_before.map_or(0, |top| top.pictures);
        let given = delta(last.maybe_frames_given, first.maybe_frames_given);
        let skipped = delta(last.maybe_frames_skipped, first.maybe_frames_skipped);
        let maybe_skipped_share = match (given, skipped) {
          (Some(given), Some(skipped)) if given + skipped > 0 => {
            Some(skipped as f64 / (given + skipped) as f64)
          },
          _ => None,
        };
        ShareSummary::Sent(SenderSummary {
          seconds,
          codec: codec_name(&last.codec),
          encoder: encoder_name(last.maybe_encoder.as_ref().map(|encoder| encoder.implementation)),
          maybe_capture: last.maybe_capture,
          maybe_top: maybe_top.map(|top| top.size),
          maybe_captured_fps: delta(last.maybe_frames_captured, first.maybe_frames_captured).map(per_second),
          top_fps: per_second(top_pictures.saturating_sub(top_pictures_before)),
          maybe_skipped_share,
          bitrate_kbps: per_second(last.bytes.saturating_sub(first.bytes)) * 8.0 / 1000.0,
          maybe_frames_captured: last.maybe_frames_captured,
          top_pictures,
          top_keyframes: maybe_top.map_or(0, |top| top.keyframes),
          maybe_frames_given: last.maybe_frames_given,
          maybe_frames_skipped: last.maybe_frames_skipped,
          pictures_dropped: last.pictures_dropped,
          maybe_held_back_ms: last.maybe_held_back_ms,
          maybe_uplink_min_kbps: (uplink.looks > 0).then(|| f64::from(uplink.min_bps) / 1000.0),
          maybe_uplink_avg_kbps: (uplink.looks > 0)
            .then(|| uplink.sum_bps as f64 / f64::from(uplink.looks) / 1000.0),
          maybe_uplink_max_loss_percent: (uplink.looks > 0).then_some(uplink.max_loss_percent),
          maybe_max_rtt_ms: (uplink.max_rtt_ms > 0).then_some(uplink.max_rtt_ms),
        })
      },
      Side::Received {
        first,
        last,
        layer_looks,
        below_asked,
        max_jitter_ms,
      } => {
        let frames = last.frames.saturating_sub(first.frames);
        let discarded = last.frames_discarded.saturating_sub(first.frames_discarded);
        let packets = last.packets.saturating_sub(first.packets);
        let lost = last.packets_lost.saturating_sub(first.packets_lost);
        let maybe_size = last.maybe_layer.and_then(|layer| {
          last
            .layers
            .iter()
            .find(|offered| offered.layer == layer)
            .map(|offered| offered.size)
        });
        ShareSummary::Received(ViewerSummary {
          seconds,
          codec: codec_name(&last.codec),
          decoded_fps: per_second(frames),
          maybe_max_framerate: last.maybe_max_framerate,
          discarded_share: share_of(discarded, frames + discarded),
          lost_share: share_of(lost, packets + lost),
          bitrate_kbps: per_second(last.bytes.saturating_sub(first.bytes)) * 8.0 / 1000.0,
          frames: last.frames,
          frames_discarded: last.frames_discarded,
          packets_lost: last.packets_lost,
          keyframe_requests: last.keyframe_requests,
          maybe_longest_gap_ms: last.maybe_longest_gap_ms,
          max_jitter_ms: f64::from(*max_jitter_ms),
          maybe_below_asked_share: (*layer_looks > 0)
            .then(|| f64::from(*below_asked) / f64::from(*layer_looks)),
          maybe_layer: last.maybe_layer,
          maybe_asked_layer: last.maybe_max_layer,
          maybe_top_offered: last.layers.iter().map(|offered| offered.layer).max(),
          maybe_size,
        })
      },
    }
  }
}

impl ShareSummary {
  /// Why the share was choppy, if it was; a share looked at for less than [`MIN_SECONDS`] never
  /// is.
  pub fn choppy(&self) -> Option<Choppy> {
    match self {
      Self::Sent(sent) => (sent.seconds >= MIN_SECONDS
        && sent
          .maybe_skipped_share
          .is_some_and(|share| share > CHOPPY_SKIPPED_SHARE))
      .then_some(Choppy::EncoderSkipped),
      Self::Received(viewer) if viewer.seconds >= MIN_SECONDS => {
        if viewer.discarded_share > CHOPPY_DISCARDED_SHARE {
          Some(Choppy::FramesDiscarded)
        } else if viewer
          .maybe_longest_gap_ms
          .is_some_and(|gap| gap >= CHOPPY_FREEZE_MS)
        {
          Some(Choppy::Froze)
        } else {
          None
        }
      },
      Self::Received(_) => None,
    }
  }

  /// The warning's message: names from the code only, so one cause in one codec is one issue
  /// (the numbers are in the log line before it and in the transaction).
  pub fn choppy_message(&self, choppy: Choppy) -> String {
    match self {
      Self::Sent(sent) => format!(
        "sender: {} ({}, {} encoder, {})",
        choppy.cause(),
        sent.codec,
        sent.encoder,
        size_class(sent.maybe_top)
      ),
      Self::Received(viewer) => format!(
        "viewer: {} ({}, {})",
        choppy.cause(),
        viewer.codec,
        size_class(viewer.maybe_size)
      ),
    }
  }

  /// Tags (at most 8 with the platform's), measurements (at most 8) and the step's data.
  pub fn describe(&self) -> Described {
    let choppy = if self.choppy().is_some() { "yes" } else { "no" };
    let ms = PerfUnit::Millisecond;
    let count = PerfUnit::None;
    match self {
      Self::Sent(sent) => {
        let mut measurements = vec![
          ("sent_fps", sent.top_fps, count),
          ("bitrate_kbps", sent.bitrate_kbps, count),
          ("keyframes", sent.top_keyframes as f64, count),
        ];
        push_some(&mut measurements, "captured_fps", sent.maybe_captured_fps, count);
        push_some(
          &mut measurements,
          "skipped_percent",
          sent.maybe_skipped_share.map(|share| share * 100.0),
          count,
        );
        push_some(
          &mut measurements,
          "uplink_min_kbps",
          sent.maybe_uplink_min_kbps,
          count,
        );
        push_some(
          &mut measurements,
          "uplink_loss_max_percent",
          sent.maybe_uplink_max_loss_percent.map(f64::from),
          count,
        );
        push_some(
          &mut measurements,
          "held_back_ms",
          sent.maybe_held_back_ms.map(|ms| ms as f64),
          ms,
        );
        let mut data = vec![
          ("seconds", sent.seconds),
          ("top_pictures", sent.top_pictures as f64),
          ("top_keyframes", sent.top_keyframes as f64),
          ("pictures_dropped", sent.pictures_dropped as f64),
        ];
        push_size(&mut data, ("capture_width", "capture_height"), sent.maybe_capture);
        push_size(&mut data, ("top_width", "top_height"), sent.maybe_top);
        let numbers = [
          ("frames_captured", sent.maybe_frames_captured.map(|n| n as f64)),
          ("frames_given", sent.maybe_frames_given.map(|n| n as f64)),
          ("frames_skipped", sent.maybe_frames_skipped.map(|n| n as f64)),
          ("held_back_ms", sent.maybe_held_back_ms.map(|n| n as f64)),
          ("uplink_min_kbps", sent.maybe_uplink_min_kbps),
          ("uplink_avg_kbps", sent.maybe_uplink_avg_kbps),
          (
            "uplink_loss_max_percent",
            sent.maybe_uplink_max_loss_percent.map(f64::from),
          ),
          ("rtt_max_ms", sent.maybe_max_rtt_ms.map(f64::from)),
        ];
        data.extend(
          numbers
            .into_iter()
            .filter_map(|(name, value)| Some((name, value?))),
        );
        Described {
          tags: vec![
            ("role", "sender"),
            ("codec", sent.codec),
            ("encoder", sent.encoder),
            ("top_layer", size_class(sent.maybe_top)),
            (
              "held_back",
              if sent.maybe_held_back_ms.unwrap_or(0) > 0 {
                "yes"
              } else {
                "no"
              },
            ),
            ("choppy", choppy),
          ],
          measurements,
          span_description: "screen_share_sent",
          data,
        }
      },
      Self::Received(viewer) => {
        let mut measurements = vec![
          ("decoded_fps", viewer.decoded_fps, count),
          ("discarded_percent", viewer.discarded_share * 100.0, count),
          ("lost_percent", viewer.lost_share * 100.0, count),
          ("jitter_max_ms", viewer.max_jitter_ms, ms),
          ("keyframe_requests", viewer.keyframe_requests as f64, count),
        ];
        push_some(
          &mut measurements,
          "longest_freeze_ms",
          viewer.maybe_longest_gap_ms.map(|ms| ms as f64),
          ms,
        );
        push_some(
          &mut measurements,
          "below_asked_percent",
          viewer.maybe_below_asked_share.map(|share| share * 100.0),
          count,
        );
        measurements.push(("bitrate_kbps", viewer.bitrate_kbps, count));
        let mut data = vec![
          ("seconds", viewer.seconds),
          ("frames", viewer.frames as f64),
          ("frames_discarded", viewer.frames_discarded as f64),
          ("packets_lost", viewer.packets_lost as f64),
        ];
        push_size(&mut data, ("width", "height"), viewer.maybe_size);
        let numbers = [
          ("max_framerate", viewer.maybe_max_framerate.map(f64::from)),
          ("layer", viewer.maybe_layer.map(f64::from)),
          ("asked_layer", viewer.maybe_asked_layer.map(f64::from)),
          ("top_offered_layer", viewer.maybe_top_offered.map(f64::from)),
        ];
        data.extend(
          numbers
            .into_iter()
            .filter_map(|(name, value)| Some((name, value?))),
        );
        let layer = match (viewer.maybe_layer, viewer.maybe_top_offered) {
          (Some(layer), Some(top)) if layer >= top => "top",
          (Some(_), Some(_)) => "lower",
          _ => "unknown",
        };
        Described {
          tags: vec![
            ("role", "viewer"),
            ("codec", viewer.codec),
            ("layer", layer),
            ("size", size_class(viewer.maybe_size)),
            ("choppy", choppy),
          ],
          measurements,
          span_description: "screen_share_received",
          data,
        }
      },
    }
  }
}

impl Choppy {
  fn cause(self) -> &'static str {
    match self {
      Self::EncoderSkipped => "encoder skipped frames",
      Self::FramesDiscarded => "frames discarded",
      Self::Froze => "picture froze",
    }
  }
}

fn delta(last: Option<u64>, first: Option<u64>) -> Option<u64> {
  Some(last?.saturating_sub(first.unwrap_or(0)))
}

fn share_of(part: u64, whole: u64) -> f64 {
  if whole == 0 {
    0.0
  } else {
    part as f64 / whole as f64
  }
}

fn push_some(
  measurements: &mut Vec<(&'static str, f64, PerfUnit)>,
  name: &'static str,
  maybe_value: Option<f64>,
  unit: PerfUnit,
) {
  if let Some(value) = maybe_value {
    measurements.push((name, value, unit));
  }
}

fn push_size(
  data: &mut Vec<(&'static str, f64)>,
  names: (&'static str, &'static str),
  maybe_size: Option<VideoSize>,
) {
  if let Some(size) = maybe_size {
    data.push((names.0, f64::from(size.width)));
    data.push((names.1, f64::from(size.height)));
  }
}

/// The codec's name for a tag.
pub fn codec_name(codec: &VideoCodec) -> &'static str {
  match codec {
    VideoCodec::Av1 => "av1",
    VideoCodec::H264 => "h264",
    VideoCodec::Vp9 => "vp9",
    VideoCodec::Unknown(_) => "other",
  }
}

fn encoder_name(maybe_implementation: Option<EncoderImplementation>) -> &'static str {
  match maybe_implementation {
    Some(EncoderImplementation::Hardware) => "hardware",
    Some(EncoderImplementation::Software) => "software",
    Some(EncoderImplementation::Browser) => "browser",
    None => "unknown",
  }
}

/// A picture's lines as a class from the code (at least that many lines): sizes themselves vary
/// with every screen and window.
pub fn size_class(maybe_size: Option<VideoSize>) -> &'static str {
  let Some(size) = maybe_size else {
    return "unknown";
  };
  match size.height {
    2160.. => "2160p",
    1440.. => "1440p",
    1080.. => "1080p",
    720.. => "720p",
    540.. => "540p",
    360.. => "360p",
    _ => "small",
  }
}

thread_local! {
  static METER: RefCell<ShareMeter> = RefCell::new(ShareMeter::default());
  /// The running shares' measures (only those sampled in), by share.
  static MEASURES: RefCell<Vec<(ShareKey, crate::perf::Measure)>> = const { RefCell::new(Vec::new()) };
  static REPORTS: Cell<u32> = const { Cell::new(0) };
  static CHOPPY_REPORTS: Cell<u32> = const { Cell::new(0) };
}

/// Watches the screen shares of the active call: started once by the app; it ends with the app's
/// scope. Nothing is asked of the media layer while no screen is shared.
pub fn run(state: crate::state::AppState) {
  use leptos::prelude::*;

  let call = state.call;
  let is_sharing = Memo::new(move |_| {
    call.phase.get() == crate::calls::CallPhase::Active
      && state.read(|backend| {
        backend
          .active_call()
          .is_some_and(|active| active.is_sharing_screen)
          || backend
            .call_video_tracks()
            .iter()
            .any(|track| track.kind == VideoTrackKind::Screen)
      })
  });
  crate::reactive::use_interval_while(POLL_MS, move || is_sharing.get(), move || poll(state));
  // The last report of a share that just ended is at most one poll old.
  Effect::new(move |_| {
    if !is_sharing.get() {
      finish(METER.with(|meter| meter.borrow_mut().finish_all()));
    }
  });
}

fn poll(state: crate::state::AppState) {
  state.read(|backend| {
    backend.fetch_call_video_stats(Box::new(|maybe_json| {
      match maybe_json.and_then(|json| json.read()) {
        Some(report) => {
          let (started, ended) = METER.with(|meter| meter.borrow_mut().observe(&report));
          finish(ended);
          start(started);
        },
        // Outside a call: whatever ran ended.
        None => finish(METER.with(|meter| meter.borrow_mut().finish_all())),
      }
    }));
  });
}

fn start(started: Vec<ShareKey>) {
  for key in started {
    let measure = crate::perf::begin(crate::perf::PerfOperation::ScreenShare);
    if measure.is_on() {
      MEASURES.with(|measures| measures.borrow_mut().push((key, measure)));
    }
  }
}

fn finish(ended: Vec<(ShareKey, ShareSummary)>) {
  for (key, summary) in ended {
    let maybe_measure = MEASURES.with(|measures| {
      let mut measures = measures.borrow_mut();
      let position = measures.iter().position(|(running, _)| *running == key)?;
      Some(measures.remove(position).1)
    });
    if let Some(measure) = maybe_measure
      && REPORTS.with(|count| count.replace(count.get() + 1)) < MAX_REPORTS
    {
      send(measure, &summary);
    }
    if let Some(choppy) = summary.choppy()
      && CHOPPY_REPORTS.with(|count| count.replace(count.get() + 1)) < MAX_CHOPPY
    {
      report_choppy(&summary, choppy);
    }
  }
}

fn send(mut measure: crate::perf::Measure, summary: &ShareSummary) {
  let described = summary.describe();
  for (key, value) in described.tags {
    measure.tag(key, value);
  }
  for (name, value, unit) in described.measurements {
    measure.measure(name, value, unit);
  }
  let seconds = match summary {
    ShareSummary::Sent(sent) => sent.seconds,
    ShareSummary::Received(viewer) => viewer.seconds,
  };
  let end_ms = crate::perf::now_ms();
  measure.span_with_data(
    "media",
    described.span_description,
    (end_ms - seconds * 1000.0, end_ms),
    described.data,
  );
  measure.finish_at(end_ms, crate::perf::PerfStatus::Ok);
}

/// A warning with the cause in its message; the numbers go to the log first, so the report's
/// breadcrumbs carry them.
fn report_choppy(summary: &ShareSummary, choppy: Choppy) {
  match summary {
    ShareSummary::Sent(sent) => tracing::warn!(
      ?choppy,
      seconds = sent.seconds,
      codec = sent.codec,
      encoder = sent.encoder,
      top_fps = sent.top_fps,
      captured_fps = ?sent.maybe_captured_fps,
      skipped_share = ?sent.maybe_skipped_share,
      bitrate_kbps = sent.bitrate_kbps,
      keyframes = sent.top_keyframes,
      held_back_ms = ?sent.maybe_held_back_ms,
      uplink_min_kbps = ?sent.maybe_uplink_min_kbps,
      "the screen share was choppy"
    ),
    ShareSummary::Received(viewer) => tracing::warn!(
      ?choppy,
      seconds = viewer.seconds,
      codec = viewer.codec,
      decoded_fps = viewer.decoded_fps,
      max_framerate = ?viewer.maybe_max_framerate,
      discarded_share = viewer.discarded_share,
      lost_share = viewer.lost_share,
      longest_freeze_ms = ?viewer.maybe_longest_gap_ms,
      keyframe_requests = viewer.keyframe_requests,
      layer = ?viewer.maybe_layer,
      asked_layer = ?viewer.maybe_asked_layer,
      "the received screen share was choppy"
    ),
  }
  #[cfg(target_arch = "wasm32")]
  crate::crash_reports::capture_handled("ScreenShareChoppy", summary.choppy_message(choppy));
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::video_stats::{OfferedLayer, SentLayerStats, UplinkStats, VideoEncoderInfo};

  use super::*;

  const OWN: MediaStreamId = MediaStreamId(3);
  const THEIRS: MediaStreamId = MediaStreamId(9);

  #[test]
  fn a_sent_share_ends_when_it_is_no_longer_listed() {
    let mut meter = ShareMeter::default();
    let (started, ended) = meter.observe(&report(0, vec![sent(0, 0, 0, 0)], Some(4_000_000)));
    assert_eq!(started, [ShareKey::Sent(OWN)]);
    assert!(ended.is_empty());
    // 20 s at 30 fps captured; the encoders had no room for 120 of 600 frames (one layer).
    let (started, ended) = meter.observe(&report(10_000, vec![sent(300, 270, 30, 5)], Some(2_000_000)));
    assert!(started.is_empty() && ended.is_empty());
    meter.observe(&report(20_000, vec![sent(600, 480, 120, 9)], Some(3_000_000)));
    let (_, ended) = meter.observe(&report(22_000, Vec::new(), None));
    let [(key, ShareSummary::Sent(summary))] = ended.as_slice() else {
      panic!("{ended:?}");
    };
    assert_eq!(*key, ShareKey::Sent(OWN));
    assert_eq!(summary.seconds, 20.0);
    assert_eq!(summary.codec, "av1");
    assert_eq!(summary.encoder, "browser");
    assert_eq!(summary.maybe_captured_fps, Some(30.0));
    assert_eq!(summary.top_fps, 24.0);
    assert_eq!(summary.maybe_skipped_share, Some(0.2));
    assert_eq!(summary.top_keyframes, 9);
    assert_eq!(summary.maybe_top.map(|size| size.height), Some(1440));
    assert_eq!(summary.maybe_uplink_min_kbps, Some(2_000.0));
    assert_eq!(summary.maybe_uplink_avg_kbps, Some(3_000.0));
    assert_eq!(summary.maybe_held_back_ms, Some(1_500));
    assert!(meter.is_empty());
    assert_eq!(ended[0].1.choppy(), Some(Choppy::EncoderSkipped));
    assert_eq!(
      ended[0].1.choppy_message(Choppy::EncoderSkipped),
      "sender: encoder skipped frames (av1, browser encoder, 1440p)"
    );
  }

  #[test]
  fn a_sender_that_keeps_up_or_was_seen_briefly_is_not_choppy() {
    let summary = |seconds: u64, skipped: u64| {
      let mut meter = ShareMeter::default();
      meter.observe(&report(0, vec![sent(0, 0, 0, 0)], None));
      meter.observe(&report(
        seconds * 1000,
        vec![sent(30 * seconds, 30 * seconds, skipped, 1)],
        None,
      ));
      meter.finish_all().remove(0).1
    };
    // 66 of 666 frames is just under 10 %, 67 of 667 just over.
    assert_eq!(summary(20, 66).choppy(), None);
    assert_eq!(summary(20, 67).choppy(), Some(Choppy::EncoderSkipped));
    // Four seconds say nothing.
    assert_eq!(summary(4, 100).choppy(), None);
  }

  #[test]
  fn a_viewer_counts_discards_freezes_and_lower_layers() {
    let mut meter = ShareMeter::default();
    let (started, _) = meter.observe(&report(0, vec![received(0, 0, 0, Some(1), 0)], None));
    assert_eq!(started, [ShareKey::Received(THEIRS)]);
    meter.observe(&report(10_000, vec![received(280, 20, 5, Some(0), 400)], None));
    meter.observe(&report(20_000, vec![received(560, 40, 12, Some(1), 900)], None));
    let ended = meter.finish_all();
    let [(_, ShareSummary::Received(viewer))] = ended.as_slice() else {
      panic!("{ended:?}");
    };
    assert_eq!(viewer.decoded_fps, 28.0);
    assert!((viewer.discarded_share - 40.0 / 600.0).abs() < 1e-9);
    assert_eq!(viewer.maybe_longest_gap_ms, Some(900));
    // Asked for layer 1 three times, layer 0 arrived once.
    assert_eq!(viewer.maybe_below_asked_share, Some(1.0 / 3.0));
    assert_eq!(viewer.max_jitter_ms, 6.0);
    assert_eq!(viewer.maybe_size.map(|size| size.height), Some(1440));
    assert_eq!(ended[0].1.choppy(), None);
    let described = ended[0].1.describe();
    assert!(described.measurements.len() <= teamcraft_client_api::crash_reporting::MAX_MEASUREMENTS);
    assert!(described.tags.len() < teamcraft_client_api::crash_reporting::MAX_TAGS);
    assert!(described.tags.contains(&("layer", "top")));
  }

  #[test]
  fn a_viewer_is_choppy_when_it_discards_or_freezes() {
    let choppy = |discarded: u64, gap_ms: u64| {
      let mut meter = ShareMeter::default();
      meter.observe(&report(0, vec![received(0, 0, 0, Some(1), 0)], None));
      meter.observe(&report(
        20_000,
        vec![received(540, discarded, 0, Some(1), gap_ms)],
        None,
      ));
      meter.finish_all().remove(0).1.choppy()
    };
    assert_eq!(choppy(0, 1_999), None);
    assert_eq!(choppy(0, 2_000), Some(Choppy::Froze));
    // 60 of 600 is 10 %: not over the limit; 61 of 601 is.
    assert_eq!(choppy(60, 0), None);
    assert_eq!(choppy(61, 0), Some(Choppy::FramesDiscarded));
  }

  #[test]
  fn cameras_are_not_shares_and_a_new_call_ends_the_old_shares() {
    let mut meter = ShareMeter::default();
    let mut camera = sent(0, 0, 0, 0);
    if let VideoTrackStats::Sent(sent) = &mut camera {
      sent.kind = VideoTrackKind::Camera;
    }
    let (started, _) = meter.observe(&report(5_000, vec![camera], None));
    assert!(started.is_empty());
    meter.observe(&report(5_000, vec![sent(0, 0, 0, 0)], None));
    // The clock went back: another call, whose share has the same stream by chance.
    let (started, ended) = meter.observe(&report(1_000, vec![sent(0, 0, 0, 0)], None));
    assert_eq!(ended.len(), 1);
    assert_eq!(started, [ShareKey::Sent(OWN)]);
  }

  #[test]
  fn descriptions_stay_within_the_transaction_limits() {
    let sender = ShareSummary::Sent(SenderSummary {
      maybe_captured_fps: Some(30.0),
      maybe_skipped_share: Some(0.0),
      maybe_uplink_min_kbps: Some(1.0),
      maybe_uplink_max_loss_percent: Some(0),
      maybe_held_back_ms: Some(0),
      ..SenderSummary::default()
    });
    for summary in [sender, ShareSummary::Received(ViewerSummary::default())] {
      let described = summary.describe();
      // The platform tag comes on top.
      assert!(described.tags.len() < teamcraft_client_api::crash_reporting::MAX_TAGS);
      assert!(described.measurements.len() <= teamcraft_client_api::crash_reporting::MAX_MEASUREMENTS);
    }
  }

  #[test]
  fn sizes_go_into_classes() {
    let size = |width, height| Some(VideoSize { width, height });
    assert_eq!(size_class(size(2560, 1600)), "1440p");
    assert_eq!(size_class(size(3840, 2160)), "2160p");
    assert_eq!(size_class(size(1280, 720)), "720p");
    assert_eq!(size_class(size(320, 180)), "small");
    assert_eq!(size_class(None), "unknown");
  }

  fn report(at_ms: u64, tracks: Vec<VideoTrackStats>, maybe_uplink_bps: Option<u32>) -> VideoStatsReport {
    VideoStatsReport {
      at_ms,
      maybe_rtt_ms: Some(40),
      maybe_uplink: maybe_uplink_bps.map(|estimate_bps| UplinkStats {
        estimate_bps,
        loss_percent: 1,
      }),
      maybe_uplink_probe: None,
      maybe_downlink_probe: None,
      tracks,
    }
  }

  /// The own screen in AV1, two layers, the top one sending every picture given to it.
  fn sent(captured: u64, given: u64, skipped: u64, keyframes: u64) -> VideoTrackStats {
    let layer = |layer: u8, height: u16, pictures: u64, keyframes: u64| SentLayerStats {
      layer,
      size: VideoSize {
        width: height / 9 * 16,
        height,
      },
      is_target: true,
      pictures,
      bytes: pictures * 10_000,
      keyframes,
    };
    VideoTrackStats::Sent(SentVideoStats {
      stream_id: OWN,
      kind: VideoTrackKind::Screen,
      codec: VideoCodec::Av1,
      maybe_encoder: Some(VideoEncoderInfo {
        name: "WebCodecs".to_owned(),
        implementation: EncoderImplementation::Browser,
      }),
      maybe_capture: Some(VideoSize {
        width: 2560,
        height: 1440,
      }),
      layers: vec![layer(0, 720, 0, 0), layer(1, 1440, given, keyframes)],
      packets: given * 9,
      bytes: given * 10_000,
      maybe_frames_captured: Some(captured),
      pictures_dropped: 0,
      maybe_frames_black: None,
      maybe_frames_given: Some(given),
      maybe_frames_skipped: Some(skipped),
      maybe_held_back_ms: Some(if captured > 0 { 1_500 } else { 0 }),
      maybe_camera_limits: None,
      maybe_share_limits: None,
      maybe_capture_buffers: None,
    })
  }

  /// Another device's screen: two layers offered, layer 1 asked for.
  fn received(
    frames: u64,
    discarded: u64,
    lost: u64,
    maybe_layer: Option<u8>,
    gap_ms: u64,
  ) -> VideoTrackStats {
    VideoTrackStats::Received(ReceivedVideoStats {
      stream_id: THEIRS,
      kind: VideoTrackKind::Screen,
      codec: VideoCodec::H264,
      layers: [(0, 720), (1, 1440)]
        .into_iter()
        .map(|(layer, height)| OfferedLayer {
          layer,
          size: VideoSize {
            width: height / 9 * 16,
            height,
          },
        })
        .collect(),
      maybe_layer,
      maybe_max_layer: Some(1),
      packets: frames * 10,
      bytes: frames * 10_000,
      packets_lost: lost,
      packets_recovered: 0,
      frames,
      frames_discarded: discarded,
      maybe_jitter_ms: Some(if frames > 300 { 6.0 } else { 3.0 }),
      keyframe_requests: discarded / 10,
      maybe_longest_gap_ms: Some(gap_ms),
      maybe_max_framerate: Some(30),
      maybe_decode_queue: None,
      maybe_device_limit: None,
    })
  }
}
