//! Turns what a call does into problem reports for the crash reporter (docs/crash-reports.md,
//! "Call problems"; issue #1762): the drivers feed it the engine's events, a video stats sample every
//! [`STATS_SAMPLE_INTERVAL`] and the call's end; it keeps the call's state (transport, reconnects,
//! DAVE epoch, devices, codecs, the link) and turns problems into [`CallReport`]s with that state,
//! which the drivers hand to the page with the call snapshot.
//!
//! Nothing here runs per frame or per packet: events are the engine's slow path (joins, rekeys,
//! reconnects), and the stats are the counters the stats overlay reads. Each kind of problem is
//! reported a bounded number of times per call ([`CallTelemetry::take_reports`] drains them); the
//! page caps and samples again.
//!
//! Privacy: a report names the call by a random [`CallRef`] and holds counts, enums and code words
//! only; no device, user, call or conversation ID, no name, key, key binding or privacy code
//! (`crash_reporting::call`). The trail says what happened, never who.

use std::collections::BTreeSet;
use std::time::Duration;

use teamcraft_client_api::crash_reporting::{
  CallContext, CallProblem, CallQualitySummary, CallRef, CallReport, CallTransportKind, MAX_CALL_TRAIL,
  MAX_RECEIVED_TRACKS, ReceivedVideoContext, ResumeFailure, SentVideoContext, Unverified, identifier,
};
use teamcraft_client_api::video_stats::{VideoStatsReport, VideoTrackStats};
use teamcraft_proto::{DeviceId, MediaStreamId, VideoCodec, VideoTrackKind};

use crate::call_engine::{CallEngineError, CallEngineEvent, DaveResumeFailure, OwnVideoEnd};
use crate::call_resilience::CallTransport;
use crate::capture_health::STALL_WINDOW;

/// How often the drivers sample the video stats for the telemetry while joined.
pub const STATS_SAMPLE_INTERVAL: Duration = Duration::from_secs(3);
/// Reports of one kind per call; the page caps the call's total again.
const MAX_PER_KIND: u8 = 1;
/// Freezes reported per call (one per track and episode).
const MAX_FREEZES: u8 = 3;
/// Refusals of the SFU reported per call, one per error code.
const MAX_SFU_REJECTIONS: u8 = 3;
/// Reports waiting for the driver; beyond it the oldest go (a driver that never takes them).
const MAX_PENDING: usize = 16;
/// A downlink sample counts as lossy from this loss on.
const LOSSY_PERCENT: u8 = 5;

/// When a call counts as having a problem. Defaults from the field test of 2026-11-11.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelemetryThresholds {
  /// More reconnects than this within [`Self::churn_window`] are churn.
  pub churn_reconnects: u32,
  pub churn_window: Duration,
  /// A subscribed track without a new picture for this long is frozen.
  pub freeze: Duration,
  /// A call's end is summed up as poor from this much frozen time over all tracks…
  pub poor_frozen: Duration,
  /// …or this many reconnects…
  pub poor_reconnects: u32,
  /// …or this many lossy stats samples.
  pub poor_lossy_samples: u32,
  /// Calls shorter than this are not summed up.
  pub poor_min_duration: Duration,
}

/// One call's telemetry, from the join (or the join attempt) to its end.
#[derive(Debug)]
pub struct CallTelemetry {
  call_ref: CallRef,
  thresholds: TelemetryThresholds,
  /// The driver's monotonic clock when the call started.
  started_ms: u64,
  /// The device's wall clock at the same moment, for the reports' `at_ms`.
  started_unix_ms: i64,
  is_joined: bool,
  is_reconnecting: bool,
  next_seq: u32,
  maybe_transport: Option<CallTransportKind>,
  was_fallback: bool,
  reconnects: u32,
  reconnect_times: Vec<u64>,
  maybe_dave_epoch: Option<u64>,
  devices: BTreeSet<DeviceId>,
  maybe_uplink: Option<(u32, u8)>,
  link: LinkSample,
  sent: Vec<SentVideoContext>,
  received: Vec<ReceivedVideoContext>,
  // Small lists rather than maps: a handful of entries, and less code in the browser's call module
  // (its download budget).
  streams: Vec<(MediaStreamId, StreamWatch)>,
  reported: Vec<(&'static str, u8)>,
  sfu_codes: Vec<String>,
  trail: Vec<String>,
  summary: SummaryParts,
  pending: Vec<CallReport>,
}

/// The link as the last stats sample saw it.
#[derive(Debug, Clone, Copy, Default)]
struct LinkSample {
  maybe_rtt_ms: Option<u32>,
  maybe_jitter_ms: Option<u32>,
  maybe_downlink_loss_percent: Option<u8>,
}

/// A received track's counters between samples.
#[derive(Debug, Clone, Copy)]
struct StreamWatch {
  frames: u64,
  packets: u64,
  packets_lost: u64,
  /// When a new picture last arrived (or the watch started).
  changed_ms: u64,
  /// Packets at that moment: did any arrive since?
  packets_at_change: u64,
  is_frozen: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct SummaryParts {
  frozen_ms: u64,
  freezes: u32,
  lossy_samples: u32,
  max_loss_percent: u8,
  max_rtt_ms: u32,
  maybe_min_uplink_kbps: Option<u32>,
}

impl Default for TelemetryThresholds {
  fn default() -> Self {
    Self {
      churn_reconnects: 3,
      churn_window: Duration::from_secs(5 * 60),
      freeze: Duration::from_secs(6),
      poor_frozen: Duration::from_secs(15),
      poor_reconnects: 3,
      poor_lossy_samples: 10,
      poor_min_duration: Duration::from_secs(30),
    }
  }
}

impl CallTelemetry {
  /// A call starting now: `now_ms` on the driver's monotonic clock, `unix_ms` the wall clock.
  pub fn new(call_ref: CallRef, now_ms: u64, unix_ms: i64, thresholds: TelemetryThresholds) -> Self {
    Self {
      call_ref,
      thresholds,
      started_ms: now_ms,
      started_unix_ms: unix_ms,
      is_joined: false,
      is_reconnecting: false,
      next_seq: 1,
      maybe_transport: None,
      was_fallback: false,
      reconnects: 0,
      reconnect_times: Vec::new(),
      maybe_dave_epoch: None,
      devices: BTreeSet::new(),
      maybe_uplink: None,
      link: LinkSample::default(),
      sent: Vec::new(),
      received: Vec::new(),
      streams: Vec::new(),
      reported: Vec::new(),
      sfu_codes: Vec::new(),
      trail: Vec::new(),
      summary: SummaryParts::default(),
      pending: Vec::new(),
    }
  }

  /// One engine event (or a driver's, like the transport). Frames are not for this and ignored.
  pub fn on_event(&mut self, event: &CallEngineEvent, now_ms: u64) {
    match event {
      CallEngineEvent::Joined { participants, .. } => {
        self.is_joined = true;
        self.devices = participants
          .iter()
          .map(|participant| participant.device_id)
          .collect();
        self.note(now_ms, format!("joined ({} devices)", self.devices.len()));
      },
      CallEngineEvent::ParticipantJoined(participant) => {
        self.devices.insert(participant.device_id);
        self.note(now_ms, format!("device joined ({} in call)", self.devices.len()));
      },
      CallEngineEvent::ParticipantLeft(device) => {
        self.devices.remove(device);
        self.note(now_ms, format!("device left ({} in call)", self.devices.len()));
      },
      CallEngineEvent::ParticipantReconnecting(_) => {
        self.note(now_ms, "another device reconnecting".to_owned())
      },
      CallEngineEvent::Reconnecting => self.on_reconnecting(now_ms),
      CallEngineEvent::Reconnected => {
        self.is_reconnecting = false;
        self.note(now_ms, "reconnected".to_owned());
        // A freeze across the reconnect is the reconnect's, not the track's.
        self.restart_stream_watches(now_ms);
      },
      CallEngineEvent::TransportChanged(transport) => self.on_transport(*transport, now_ms),
      CallEngineEvent::Secured { epoch, .. } => {
        self.maybe_dave_epoch = Some(*epoch);
        self.note(now_ms, format!("secured, epoch {epoch}"));
      },
      CallEngineEvent::EncryptionFailed { retry_after } => {
        let is_refused = !retry_after.is_zero();
        self.note(
          now_ms,
          if is_refused {
            "encryption refused"
          } else {
            "encryption not secured in time"
          }
          .to_owned(),
        );
        self.report(
          now_ms,
          CallProblem::EncryptionFailed {
            is_refused,
            retry_after_s: u32::try_from(retry_after.as_secs()).unwrap_or(u32::MAX),
          },
        );
      },
      CallEngineEvent::UntrustedRoster => self.unverified(Unverified::UntrustedRoster, now_ms),
      CallEngineEvent::UnauthenticatedKeys => self.unverified(Unverified::UnauthenticatedKeys, now_ms),
      CallEngineEvent::UplinkEstimate { bps, loss_percent } => {
        self.maybe_uplink = Some((*bps, *loss_percent));
        if self.is_joined {
          let kbps = bps / 1000;
          self.summary.maybe_min_uplink_kbps = Some(
            self
              .summary
              .maybe_min_uplink_kbps
              .map_or(kbps, |min| min.min(kbps)),
          );
        }
      },
      CallEngineEvent::Error(error) => {
        let code = error.error_code.as_str().to_owned();
        self.note(now_ms, format!("sfu refused a request: {code}"));
        if self.sfu_codes.len() < usize::from(MAX_SFU_REJECTIONS) && !self.sfu_codes.contains(&code) {
          self.sfu_codes.push(code.clone());
          self.push(now_ms, CallProblem::SfuRejected { code });
        }
      },
      CallEngineEvent::OwnVideoEnded { kind, end } => {
        // Words only: a takeover names the device that took over.
        let how = match end {
          OwnVideoEnd::BeforeFirstPicture => "before its first picture",
          OwnVideoEnd::WhileSending => "while sending",
          OwnVideoEnd::TakenOver { .. } => "taken over",
          OwnVideoEnd::EncodingFailed => "its encoder gave up",
        };
        self.note(now_ms, format!("own {} ended by itself ({how})", kind.as_str()));
        if *end == OwnVideoEnd::EncodingFailed {
          self.video_encoding_failed(kind, now_ms);
        }
      },
      CallEngineEvent::MemberRefused(_) => {
        self.note(now_ms, "another member refused by the encryption".to_owned());
        self.report(now_ms, CallProblem::MemberRefused);
      },
      CallEngineEvent::DaveResumeFailed(failure) => {
        let cause = match failure {
          DaveResumeFailure::NotResumed => ResumeFailure::NotResumed,
          DaveResumeFailure::MemberOffRoster => ResumeFailure::MemberOffRoster,
        };
        self.note(now_ms, format!("encryption not resumed: {}", cause.as_str()));
        self.report(now_ms, CallProblem::ResumeFailed { cause });
      },
      CallEngineEvent::ScreenShareTakenOver(_) => self.note(now_ms, "screen share taken over".to_owned()),
      CallEngineEvent::ConnectionReplaced => self.note(now_ms, "joined on another connection".to_owned()),
      CallEngineEvent::CaptureStalled { kind, window } => {
        self.note(now_ms, format!("own {} capture stalled", kind.as_str()));
        self.report(
          now_ms,
          CallProblem::CaptureStalled {
            video: kind.clone(),
            window: (*window).into(),
            window_s: seconds(millis(STALL_WINDOW)),
          },
        );
      },
      CallEngineEvent::RemoteCaptureStalled { kind, is_stalled, .. } => {
        let how = if *is_stalled {
          "stalled"
        } else {
          "gives pictures again"
        };
        // The kind comes from another device: an unknown one is its free text, kept to code words.
        self.note(
          now_ms,
          format!("another device's {} capture {how}", identifier(kind.as_str())),
        );
      },
      CallEngineEvent::CaptureRecovered(kind) => {
        self.note(
          now_ms,
          format!("own {} capture gives pictures again", kind.as_str()),
        );
      },
      CallEngineEvent::DecodableVideoCodecs(codecs) => {
        let names: Vec<&str> = codecs.iter().map(VideoCodec::as_str).collect();
        self.note(now_ms, format!("decodable codecs: {}", names.join(",")));
      },
      _ => {},
    }
  }

  /// A video stats sample (every [`STATS_SAMPLE_INTERVAL`] while joined): the codecs and layers
  /// for the context, the link, and freezes of subscribed tracks.
  pub fn observe_stats(&mut self, report: &VideoStatsReport, now_ms: u64) {
    self.sent = sent_context(report);
    self.received = received_context(report);
    self.link.maybe_rtt_ms = report.maybe_rtt_ms;
    if let Some(rtt) = report.maybe_rtt_ms {
      self.summary.max_rtt_ms = self.summary.max_rtt_ms.max(rtt);
    }
    if let Some(uplink) = report.maybe_uplink {
      self.maybe_uplink = Some((uplink.estimate_bps, uplink.loss_percent));
    }
    self.link.maybe_jitter_ms = report
      .tracks
      .iter()
      .filter_map(|track| match track {
        VideoTrackStats::Received(received) => received.maybe_jitter_ms,
        VideoTrackStats::Sent(_) => None,
      })
      .fold(None, |max: Option<f32>, jitter| Some(max.map_or(jitter, |max| max.max(jitter))))
      // Rounding: jitter is a few to a few hundred milliseconds.
      .map(|jitter| jitter.round() as u32);
    self.watch_streams(report, now_ms);
  }

  /// The call ended with an error, or a join failed (before [`CallEngineEvent::Joined`]): `code` is
  /// the failure's stable wire name.
  pub fn call_failed(&mut self, code: &str, now_ms: u64) {
    let is_joined = self.is_joined;
    self.note(now_ms, format!("failed: {code}"));
    self.report(
      now_ms,
      CallProblem::CallFailed {
        code: code.to_owned(),
        is_joined,
      },
    );
  }

  /// The engine stopped the call with this error.
  pub fn engine_failed(&mut self, error: &CallEngineError, now_ms: u64) {
    match error {
      CallEngineError::Protocol(what) => {
        self.note(now_ms, format!("protocol violation: {what}"));
        self.report(
          now_ms,
          CallProblem::ProtocolError {
            what: (*what).to_owned(),
          },
        );
      },
      CallEngineError::SequenceGap { .. } => {
        self.note(now_ms, "event sequence gap".to_owned());
        self.report(
          now_ms,
          CallProblem::ProtocolError {
            what: "sequence_gap".to_owned(),
          },
        );
      },
      CallEngineError::UnverifiedMember(_) => self.unverified(Unverified::UnverifiedMember, now_ms),
      CallEngineError::RetiredKey(_) => self.unverified(Unverified::RetiredKey, now_ms),
      CallEngineError::Dave(_) => self.call_failed("dave", now_ms),
      CallEngineError::CallKey(_) => self.call_failed("call_key", now_ms),
    }
  }

  /// An own track's encoder gave up (`OwnVideoEnd::EncodingFailed`, #1766): the codec, encoder,
  /// lines and top layer it last sent, as the last stats sample saw them.
  fn video_encoding_failed(&mut self, video: &VideoTrackKind, now_ms: u64) {
    let maybe_sent = self.sent.iter().find(|sent| sent.video == *video);
    let codec = maybe_sent.map_or(VideoCodec::Unknown("unknown".to_owned()), |sent| {
      sent.codec.clone()
    });
    let maybe_encoder = maybe_sent.and_then(|sent| sent.maybe_encoder);
    let lines = maybe_sent.map_or(0, |sent| sent.top_lines);
    let layer = maybe_sent.map_or(0, |sent| sent.layers.saturating_sub(1));
    let video = video.clone();
    self.report(
      now_ms,
      CallProblem::VideoEncodingFailed {
        video,
        codec,
        maybe_encoder,
        lines,
        layer,
      },
    );
  }

  /// The call ended: a summary of a call that went badly, over the thresholds only.
  pub fn finish(&mut self, now_ms: u64) {
    for (_, watch) in &mut self.streams {
      if watch.is_frozen {
        self.summary.frozen_ms += now_ms.saturating_sub(watch.changed_ms);
        watch.is_frozen = false;
      }
    }
    let duration_ms = now_ms.saturating_sub(self.started_ms);
    let thresholds = self.thresholds;
    let is_poor = self.summary.frozen_ms >= millis(thresholds.poor_frozen)
      || self.reconnects >= thresholds.poor_reconnects
      || self.summary.lossy_samples >= thresholds.poor_lossy_samples;
    if !self.is_joined || duration_ms < millis(thresholds.poor_min_duration) || !is_poor {
      return;
    }
    let summary = CallQualitySummary {
      duration_s: seconds(duration_ms),
      frozen_s: seconds(self.summary.frozen_ms),
      freezes: self.summary.freezes,
      reconnects: self.reconnects,
      lossy_samples: self.summary.lossy_samples,
      max_loss_percent: self.summary.max_loss_percent,
      max_rtt_ms: self.summary.max_rtt_ms,
      was_fallback: self.was_fallback,
      maybe_min_uplink_kbps: self.summary.maybe_min_uplink_kbps,
    };
    self.note(now_ms, "left".to_owned());
    self.report(now_ms, CallProblem::PoorCall(summary));
  }

  /// The reports since the last call, oldest first.
  pub fn take_reports(&mut self) -> Vec<CallReport> {
    std::mem::take(&mut self.pending)
  }

  fn on_reconnecting(&mut self, now_ms: u64) {
    self.is_reconnecting = true;
    self.reconnects += 1;
    self.note(now_ms, "reconnecting".to_owned());
    let window = millis(self.thresholds.churn_window);
    self.reconnect_times.push(now_ms);
    self
      .reconnect_times
      .retain(|at| now_ms.saturating_sub(*at) <= window);
    let in_window = u32::try_from(self.reconnect_times.len()).unwrap_or(u32::MAX);
    if in_window > self.thresholds.churn_reconnects {
      self.report(
        now_ms,
        CallProblem::ReconnectChurn {
          reconnects: in_window,
          window_s: seconds(window),
        },
      );
    }
  }

  fn on_transport(&mut self, transport: CallTransport, now_ms: u64) {
    let kind = match transport {
      CallTransport::Quic => CallTransportKind::Quic,
      CallTransport::Fallback => CallTransportKind::Fallback,
    };
    if self.maybe_transport == Some(kind) {
      return;
    }
    self.maybe_transport = Some(kind);
    self.note(now_ms, format!("transport {}", kind.as_str()));
    if kind == CallTransportKind::Fallback {
      self.was_fallback = true;
      self.report(now_ms, CallProblem::TransportFallback);
    }
  }

  fn unverified(&mut self, cause: Unverified, now_ms: u64) {
    self.note(now_ms, format!("unverified: {}", cause.as_str()));
    self.report(now_ms, CallProblem::Unverified { cause });
  }

  /// Compares each subscribed track's counters with the last sample: a picture that stood still
  /// past the threshold is a freeze (one report per episode, up to [`MAX_FREEZES`] a call).
  fn watch_streams(&mut self, report: &VideoStatsReport, now_ms: u64) {
    let mut seen = Vec::new();
    let (mut packets, mut lost) = (0_u64, 0_u64);
    let mut freezes = Vec::new();
    for track in &report.tracks {
      let VideoTrackStats::Received(received) = track else {
        continue;
      };
      if received.maybe_max_layer.is_none() {
        continue;
      }
      seen.push(received.stream_id);
      let index = match self
        .streams
        .iter()
        .position(|(stream, _)| *stream == received.stream_id)
      {
        Some(index) => index,
        None => {
          self.streams.push((
            received.stream_id,
            StreamWatch {
              frames: received.frames,
              packets: received.packets,
              packets_lost: received.packets_lost,
              changed_ms: now_ms,
              packets_at_change: received.packets,
              is_frozen: false,
            },
          ));
          self.streams.len() - 1
        },
      };
      let watch = &mut self.streams[index].1;
      packets += received.packets.saturating_sub(watch.packets);
      lost += received.packets_lost.saturating_sub(watch.packets_lost);
      watch.packets = received.packets;
      watch.packets_lost = received.packets_lost;
      // Frames that repeat the picture of a stalled capture (#1805) are no new picture.
      if received.frames != watch.frames && !received.is_sender_capture_stalled {
        watch.frames = received.frames;
        if watch.is_frozen {
          self.summary.frozen_ms += now_ms.saturating_sub(watch.changed_ms);
          watch.is_frozen = false;
        }
        watch.changed_ms = now_ms;
        watch.packets_at_change = received.packets;
        continue;
      }
      let still_ms = now_ms.saturating_sub(watch.changed_ms);
      if !watch.is_frozen && !self.is_reconnecting && still_ms >= millis(self.thresholds.freeze) {
        watch.is_frozen = true;
        freezes.push(CallProblem::VideoFreeze {
          video: received.kind.clone(),
          codec: received.codec.clone(),
          frozen_s: seconds(still_ms),
          is_media_arriving: received.packets > watch.packets_at_change,
        });
      }
    }
    for (stream, watch) in &self.streams {
      if !seen.contains(stream) && watch.is_frozen {
        self.summary.frozen_ms += now_ms.saturating_sub(watch.changed_ms);
      }
    }
    self.streams.retain(|(stream, _)| seen.contains(stream));
    self.note_downlink_loss(packets, lost);
    for freeze in freezes {
      self.summary.freezes += 1;
      if let CallProblem::VideoFreeze { video, .. } = &freeze {
        self.note(now_ms, format!("{} picture froze", video.as_str()));
      }
      self.push_capped(now_ms, freeze, MAX_FREEZES);
    }
  }

  fn note_downlink_loss(&mut self, packets: u64, lost: u64) {
    let expected = packets + lost;
    if expected < 50 {
      self.link.maybe_downlink_loss_percent = None;
      return;
    }
    // Rounding: a share in percent, 0 to 100.
    let percent = u8::try_from(lost * 100 / expected).unwrap_or(100);
    self.link.maybe_downlink_loss_percent = Some(percent);
    self.summary.max_loss_percent = self.summary.max_loss_percent.max(percent);
    if percent >= LOSSY_PERCENT {
      self.summary.lossy_samples += 1;
    }
  }

  fn restart_stream_watches(&mut self, now_ms: u64) {
    for (_, watch) in &mut self.streams {
      if watch.is_frozen {
        self.summary.frozen_ms += now_ms.saturating_sub(watch.changed_ms);
        watch.is_frozen = false;
      }
      watch.changed_ms = now_ms;
      watch.packets_at_change = watch.packets;
    }
  }

  /// A problem of a kind reported at most once a call.
  fn report(&mut self, now_ms: u64, problem: CallProblem) {
    self.push_capped(now_ms, problem, MAX_PER_KIND);
  }

  fn push_capped(&mut self, now_ms: u64, problem: CallProblem, cap: u8) {
    let kind = problem.kind();
    let index = match self.reported.iter().position(|(reported, _)| *reported == kind) {
      Some(index) => index,
      None => {
        self.reported.push((kind, 0));
        self.reported.len() - 1
      },
    };
    let count = &mut self.reported[index].1;
    if *count >= cap {
      return;
    }
    *count += 1;
    self.push(now_ms, problem);
  }

  fn push(&mut self, now_ms: u64, problem: CallProblem) {
    let report = CallReport {
      call_ref: self.call_ref,
      seq: self.next_seq,
      at_ms: self
        .started_unix_ms
        .saturating_add(i64::try_from(now_ms.saturating_sub(self.started_ms)).unwrap_or(i64::MAX)),
      problem,
      context: self.context(now_ms),
      trail: self.trail.clone(),
    };
    self.next_seq += 1;
    if self.pending.len() >= MAX_PENDING {
      self.pending.remove(0);
    }
    self.pending.push(report);
  }

  fn context(&self, now_ms: u64) -> CallContext {
    CallContext {
      maybe_transport: self.maybe_transport,
      since_join_s: seconds(now_ms.saturating_sub(self.started_ms)),
      reconnects: self.reconnects,
      maybe_dave_epoch: self.maybe_dave_epoch,
      devices: u16::try_from(self.devices.len()).unwrap_or(u16::MAX),
      sent: self.sent.clone(),
      received: self.received.clone(),
      maybe_uplink_kbps: self.maybe_uplink.map(|(bps, _)| bps / 1000),
      maybe_uplink_loss_percent: self.maybe_uplink.map(|(_, loss)| loss),
      maybe_downlink_loss_percent: self.link.maybe_downlink_loss_percent,
      maybe_rtt_ms: self.link.maybe_rtt_ms,
      maybe_jitter_ms: self.link.maybe_jitter_ms,
    }
  }

  /// One line of the trail: the time since the start and what happened, no IDs.
  fn note(&mut self, now_ms: u64, what: String) {
    let since_ms = now_ms.saturating_sub(self.started_ms);
    if self.trail.len() >= MAX_CALL_TRAIL {
      self.trail.remove(0);
    }
    self
      .trail
      .push(format!("+{}.{} s {what}", since_ms / 1000, since_ms % 1000 / 100));
  }
}

/// The own tracks of a sample, for the context.
fn sent_context(report: &VideoStatsReport) -> Vec<SentVideoContext> {
  report
    .tracks
    .iter()
    .filter_map(|track| match track {
      VideoTrackStats::Sent(sent) => Some(SentVideoContext {
        video: sent.kind.clone(),
        codec: sent.codec.clone(),
        maybe_encoder: sent.maybe_encoder.as_ref().map(|encoder| encoder.implementation),
        top_lines: sent
          .layers
          .iter()
          .filter(|layer| layer.pictures > 0)
          .map(|layer| layer.size.height)
          .max()
          .unwrap_or(0),
        layers: u8::try_from(sent.layers.len()).unwrap_or(u8::MAX),
        maybe_planned_fps: sent
          .maybe_camera_limits
          .as_ref()
          .map(|limits| limits.planned_framerate),
        maybe_fps: sent.maybe_camera_limits.as_ref().map(|limits| limits.framerate),
        top_layers_off: sent
          .maybe_camera_limits
          .as_ref()
          .map_or(0, |limits| limits.top_layers_off),
      }),
      VideoTrackStats::Received(_) => None,
    })
    .take(2)
    .collect()
}

/// The subscribed tracks of a sample, for the context.
fn received_context(report: &VideoStatsReport) -> Vec<ReceivedVideoContext> {
  report
    .tracks
    .iter()
    .filter_map(|track| match track {
      VideoTrackStats::Received(received) if received.maybe_max_layer.is_some() => {
        Some(ReceivedVideoContext {
          video: received.kind.clone(),
          codec: received.codec.clone(),
          lines: received
            .maybe_layer
            .and_then(|layer| received.layers.iter().find(|offered| offered.layer == layer))
            .map_or(0, |offered| offered.size.height),
          maybe_layer: received.maybe_layer,
          maybe_max_layer: received.maybe_max_layer,
        })
      },
      _ => None,
    })
    .take(MAX_RECEIVED_TRACKS)
    .collect()
}

fn millis(duration: Duration) -> u64 {
  u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn seconds(ms: u64) -> u32 {
  u32::try_from(ms / 1000).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::video_stats::{
    EncoderImplementation, OfferedLayer, ReceivedVideoStats, SentLayerStats, SentVideoStats, UplinkStats,
    VideoEncoderInfo, VideoSize,
  };
  use teamcraft_proto::{CallId, CallParticipant, ErrorBody, ErrorCode, Id, Snowflake, UserId};

  use super::*;

  const START_UNIX_MS: i64 = 1_700_000_000_000;

  mod reports {
    use super::*;

    #[test]
    fn a_join_then_an_encryption_failure_carries_the_calls_state() {
      let mut telemetry = joined(3);
      telemetry.on_event(&CallEngineEvent::TransportChanged(CallTransport::Quic), 1_000);
      telemetry.on_event(
        &CallEngineEvent::Secured {
          epoch: 4,
          privacy_code: "12345 67890".to_owned(),
        },
        2_000,
      );
      telemetry.observe_stats(&stats(10, 100, 0), 3_000);
      telemetry.on_event(
        &CallEngineEvent::EncryptionFailed {
          retry_after: Duration::from_secs(60),
        },
        42_500,
      );
      let reports = telemetry.take_reports();
      assert_eq!(reports.len(), 1);
      let report = &reports[0];
      assert_eq!(report.seq, 1);
      assert_eq!(report.at_ms, START_UNIX_MS + 42_500);
      assert_eq!(
        report.problem,
        CallProblem::EncryptionFailed {
          is_refused: true,
          retry_after_s: 60
        }
      );
      let context = &report.context;
      assert_eq!(context.maybe_transport, Some(CallTransportKind::Quic));
      assert_eq!(context.since_join_s, 42);
      assert_eq!(context.maybe_dave_epoch, Some(4));
      assert_eq!(context.devices, 3);
      assert_eq!(context.maybe_rtt_ms, Some(80));
      assert_eq!(context.maybe_uplink_kbps, Some(1_500));
      assert_eq!(context.sent[0].codec, VideoCodec::Av1);
      assert_eq!(context.sent[0].top_lines, 720);
      assert_eq!(
        context.sent[0].maybe_encoder,
        Some(EncoderImplementation::Browser)
      );
      assert_eq!(context.received[0].lines, 360);
      assert_eq!(report.trail.last().unwrap(), "+42.5 s encryption refused");
      // The privacy code and the devices' IDs appear nowhere.
      let json = serde_json::to_string(report).unwrap();
      assert!(!json.contains("12345"), "{json}");
      assert!(!json.contains(&device(2).to_string()), "{json}");
      assert!(telemetry.take_reports().is_empty());
    }

    #[test]
    fn each_kind_is_reported_once_a_call() {
      let mut telemetry = joined(2);
      for at in [1_000, 2_000] {
        telemetry.on_event(
          &CallEngineEvent::EncryptionFailed {
            retry_after: Duration::ZERO,
          },
          at,
        );
        telemetry.on_event(&CallEngineEvent::UntrustedRoster, at);
      }
      let kinds: Vec<&str> = telemetry
        .take_reports()
        .iter()
        .map(|report| report.problem.kind())
        .collect();
      assert_eq!(kinds, ["encryption_failed", "unverified"]);
    }

    #[test]
    fn the_fallback_is_reported_once_and_marks_the_call() {
      let mut telemetry = joined(2);
      telemetry.on_event(&CallEngineEvent::TransportChanged(CallTransport::Fallback), 100);
      telemetry.on_event(&CallEngineEvent::TransportChanged(CallTransport::Quic), 200);
      telemetry.on_event(&CallEngineEvent::TransportChanged(CallTransport::Fallback), 300);
      let reports = telemetry.take_reports();
      assert_eq!(reports.len(), 1);
      assert_eq!(reports[0].problem, CallProblem::TransportFallback);
      assert_eq!(
        reports[0].context.maybe_transport,
        Some(CallTransportKind::Fallback)
      );
    }

    #[test]
    fn sfu_refusals_go_once_per_code_with_the_code_only() {
      let mut telemetry = joined(2);
      for code in [
        ErrorCode::CallLocked,
        ErrorCode::CallLocked,
        ErrorCode::RemovedFromCall,
      ] {
        telemetry.on_event(
          &CallEngineEvent::Error(ErrorBody::new(code, "unknown video track 123456789")),
          1_000,
        );
      }
      let reports = telemetry.take_reports();
      assert_eq!(reports.len(), 2);
      assert!(!serde_json::to_string(&reports).unwrap().contains("123456789"));
    }

    #[test]
    fn engine_errors_map_to_their_problems() {
      let cases = [
        (
          CallEngineError::Protocol("joined twice"),
          CallProblem::ProtocolError {
            what: "joined twice".to_owned(),
          },
        ),
        (
          CallEngineError::UnverifiedMember(device(9)),
          CallProblem::Unverified {
            cause: Unverified::UnverifiedMember,
          },
        ),
        (
          CallEngineError::RetiredKey(device(9)),
          CallProblem::Unverified {
            cause: Unverified::RetiredKey,
          },
        ),
        (
          CallEngineError::SequenceGap { last: 1, received: 5 },
          CallProblem::ProtocolError {
            what: "sequence_gap".to_owned(),
          },
        ),
      ];
      for (error, problem) in cases {
        let mut telemetry = joined(2);
        telemetry.engine_failed(&error, 5_000);
        let reports = telemetry.take_reports();
        assert_eq!(reports[0].problem, problem);
        assert!(
          !serde_json::to_string(&reports)
            .unwrap()
            .contains(&device(9).to_string())
        );
      }
    }

    #[test]
    fn a_join_that_failed_says_so() {
      let mut telemetry = telemetry();
      telemetry.call_failed("call_server_unreachable", 8_000);
      assert_eq!(
        telemetry.take_reports()[0].problem,
        CallProblem::CallFailed {
          code: "call_server_unreachable".to_owned(),
          is_joined: false
        }
      );
    }

    #[test]
    fn refusals_and_failed_resumes_are_reported_by_cause() {
      let mut telemetry = joined(2);
      telemetry.on_event(&CallEngineEvent::MemberRefused(device(2)), 1_000);
      telemetry.on_event(
        &CallEngineEvent::DaveResumeFailed(DaveResumeFailure::MemberOffRoster),
        2_000,
      );
      let reports = telemetry.take_reports();
      assert_eq!(reports[0].problem, CallProblem::MemberRefused);
      assert_eq!(
        reports[1].problem,
        CallProblem::ResumeFailed {
          cause: ResumeFailure::MemberOffRoster
        }
      );
      assert!(
        !serde_json::to_string(&reports)
          .unwrap()
          .contains(&device(2).to_string())
      );
    }

    #[test]
    fn an_encoder_that_gave_up_names_what_it_last_sent() {
      let mut telemetry = joined(2);
      telemetry.observe_stats(&stats(10, 100, 0), 0);
      telemetry.on_event(
        &CallEngineEvent::OwnVideoEnded {
          kind: VideoTrackKind::Camera,
          end: OwnVideoEnd::EncodingFailed,
        },
        3_000,
      );
      assert_eq!(
        telemetry.take_reports()[0].problem,
        CallProblem::VideoEncodingFailed {
          video: VideoTrackKind::Camera,
          codec: VideoCodec::Av1,
          maybe_encoder: Some(EncoderImplementation::Browser),
          lines: 720,
          layer: 1,
        }
      );
    }

    /// #1805: an own capture that gives the others a still picture is reported once, with what
    /// became of its buffers over the window.
    #[test]
    fn a_stalled_capture_is_reported_with_its_buffer_counts() {
      let mut telemetry = joined(2);
      let window = crate::capture_health::BufferCounts {
        cursor_only: 50,
        unmapped: 150,
        ..Default::default()
      };
      let stalled = CallEngineEvent::CaptureStalled {
        kind: VideoTrackKind::Screen,
        window,
      };
      telemetry.on_event(&stalled, 3_000);
      telemetry.on_event(&CallEngineEvent::CaptureRecovered(VideoTrackKind::Screen), 9_000);
      telemetry.on_event(&stalled, 12_000);
      let reports = telemetry.take_reports();
      assert_eq!(reports.len(), 1, "once a call");
      let CallProblem::CaptureStalled {
        video,
        window: counts,
        window_s,
      } = &reports[0].problem
      else {
        panic!("{:?}", reports[0].problem);
      };
      assert_eq!(*video, VideoTrackKind::Screen);
      assert_eq!(
        (counts.unmapped, counts.cursor_only, counts.pictures),
        (150, 50, 0)
      );
      assert_eq!(*window_s, 5);
    }

    #[test]
    fn pending_reports_are_bounded() {
      let mut telemetry = joined(2);
      for code in 0..40 {
        telemetry.push(code, CallProblem::TransportFallback);
      }
      let reports = telemetry.take_reports();
      assert_eq!(reports.len(), MAX_PENDING);
      assert_eq!(reports.last().unwrap().seq, 40);
    }

    #[test]
    fn a_taken_over_share_names_nobody_in_the_trail() {
      let mut telemetry = joined(2);
      telemetry.on_event(
        &CallEngineEvent::OwnVideoEnded {
          kind: VideoTrackKind::Screen,
          end: OwnVideoEnd::TakenOver {
            by_user_id: UserId::from_snowflake(Snowflake::new(4_242_424_242).unwrap()),
            by_device_id: device(5),
          },
        },
        1_000,
      );
      telemetry.on_event(&CallEngineEvent::UntrustedRoster, 2_000);
      let json = serde_json::to_string(&telemetry.take_reports()).unwrap();
      assert!(json.contains("own screen ended by itself (taken over)"), "{json}");
      assert!(
        !json.contains("4242424242") && !json.contains(&device(5).to_string()),
        "{json}"
      );
    }

    #[test]
    fn the_trail_is_bounded_and_keeps_the_newest() {
      let mut telemetry = joined(2);
      for at in 0..100 {
        telemetry.on_event(&CallEngineEvent::ParticipantReconnecting(device(5)), at * 1_000);
      }
      telemetry.on_event(&CallEngineEvent::UntrustedRoster, 200_000);
      let report = &telemetry.take_reports()[0];
      assert_eq!(report.trail.len(), MAX_CALL_TRAIL);
      assert_eq!(
        report.trail.last().unwrap(),
        "+200.0 s unverified: untrusted_roster"
      );
    }
  }

  mod churn {
    use super::*;

    #[test]
    fn reconnects_beyond_the_threshold_in_the_window_are_churn() {
      let mut telemetry = joined(2);
      for at in [10_000, 70_000, 130_000] {
        reconnect(&mut telemetry, at);
      }
      assert!(telemetry.take_reports().is_empty());
      reconnect(&mut telemetry, 190_000);
      let reports = telemetry.take_reports();
      assert_eq!(
        reports[0].problem,
        CallProblem::ReconnectChurn {
          reconnects: 4,
          window_s: 300
        }
      );
      assert_eq!(reports[0].context.reconnects, 4);
    }

    #[test]
    fn reconnects_spread_over_a_long_call_are_not() {
      let mut telemetry = joined(2);
      for minute in 0..10 {
        reconnect(&mut telemetry, minute * 10 * 60_000);
      }
      assert!(telemetry.take_reports().is_empty());
    }
  }

  mod freezes {
    use super::*;

    #[test]
    fn a_picture_that_stands_still_while_packets_arrive_is_a_freeze() {
      let mut telemetry = joined(2);
      telemetry.observe_stats(&stats(100, 1_000, 0), 0);
      telemetry.observe_stats(&stats(100, 1_200, 0), 3_000);
      assert!(telemetry.take_reports().is_empty());
      telemetry.observe_stats(&stats(100, 1_400, 0), 6_000);
      let reports = telemetry.take_reports();
      assert_eq!(
        reports[0].problem,
        CallProblem::VideoFreeze {
          video: VideoTrackKind::Camera,
          codec: VideoCodec::Vp9,
          frozen_s: 6,
          is_media_arriving: true,
        }
      );
      // One report per episode: it stays frozen.
      telemetry.observe_stats(&stats(100, 1_600, 0), 9_000);
      assert!(telemetry.take_reports().is_empty());
    }

    /// #1805: a sharer whose capture stalled still sends a picture a second (the last one
    /// again), and says so in its frames: the viewer counts the time as frozen.
    #[test]
    fn frames_of_a_stalled_capture_are_no_new_pictures() {
      let mut telemetry = joined(2);
      let stalled = |frames, packets| {
        let mut report = stats(frames, packets, 0);
        for track in &mut report.tracks {
          if let VideoTrackStats::Received(received) = track {
            received.is_sender_capture_stalled = true;
          }
        }
        report
      };
      telemetry.observe_stats(&stats(100, 1_000, 0), 0);
      telemetry.observe_stats(&stalled(103, 1_100), 3_000);
      assert!(telemetry.take_reports().is_empty());
      telemetry.observe_stats(&stalled(106, 1_200), 6_000);
      assert!(matches!(
        telemetry.take_reports()[0].problem,
        CallProblem::VideoFreeze {
          frozen_s: 6,
          is_media_arriving: true,
          ..
        }
      ));
    }

    /// A still slide sends a picture a second without the flag: no freeze, however long.
    #[test]
    fn a_still_slide_is_no_freeze() {
      let mut telemetry = joined(2);
      for second in 0..30u64 {
        telemetry.observe_stats(&stats(100 + second, 1_000 + second * 10, 0), second * 1_000);
      }
      assert!(telemetry.take_reports().is_empty());
    }

    #[test]
    fn a_silent_track_is_told_from_one_whose_decoder_stalls() {
      let mut telemetry = joined(2);
      telemetry.observe_stats(&stats(100, 1_000, 0), 0);
      telemetry.observe_stats(&stats(100, 1_000, 0), 7_000);
      assert!(matches!(
        telemetry.take_reports()[0].problem,
        CallProblem::VideoFreeze {
          is_media_arriving: false,
          ..
        }
      ));
    }

    #[test]
    fn a_track_not_subscribed_never_freezes() {
      let mut telemetry = joined(2);
      let mut report = stats(100, 1_000, 0);
      if let VideoTrackStats::Received(received) = &mut report.tracks[1] {
        received.maybe_max_layer = None;
      }
      telemetry.observe_stats(&report, 0);
      telemetry.observe_stats(&report, 60_000);
      assert!(telemetry.take_reports().is_empty());
    }

    #[test]
    fn no_freeze_is_reported_while_reconnecting_and_the_watch_restarts_after() {
      let mut telemetry = joined(2);
      telemetry.observe_stats(&stats(100, 1_000, 0), 0);
      telemetry.on_event(&CallEngineEvent::Reconnecting, 1_000);
      telemetry.observe_stats(&stats(100, 1_000, 0), 9_000);
      telemetry.on_event(&CallEngineEvent::Reconnected, 10_000);
      telemetry.observe_stats(&stats(100, 1_000, 0), 12_000);
      assert!(telemetry.take_reports().is_empty());
      telemetry.observe_stats(&stats(100, 1_000, 0), 16_000);
      assert!(matches!(
        telemetry.take_reports()[0].problem,
        CallProblem::VideoFreeze { frozen_s: 6, .. }
      ));
    }

    #[test]
    fn freezes_are_capped_per_call() {
      let mut telemetry = joined(2);
      let mut at = 0;
      for frames in 0..10 {
        telemetry.observe_stats(&stats(frames, 1_000, 0), at);
        telemetry.observe_stats(&stats(frames, 1_000, 0), at + 7_000);
        at += 10_000;
      }
      assert_eq!(telemetry.take_reports().len(), usize::from(MAX_FREEZES));
      assert_eq!(telemetry.summary.freezes, 10);
    }
  }

  mod summary {
    use super::*;

    #[test]
    fn a_bad_call_is_summed_up_at_its_end() {
      let mut telemetry = joined(2);
      telemetry.on_event(&CallEngineEvent::TransportChanged(CallTransport::Fallback), 0);
      telemetry.take_reports();
      telemetry.observe_stats(&stats(100, 1_000, 0), 0);
      telemetry.observe_stats(&stats(100, 1_100, 0), 3_000);
      telemetry.observe_stats(&stats(100, 1_100, 0), 9_000);
      telemetry.observe_stats(&stats(200, 1_200, 100), 20_000);
      telemetry.take_reports();
      telemetry.finish(60_000);
      let reports = telemetry.take_reports();
      let CallProblem::PoorCall(summary) = &reports[0].problem else {
        panic!("{reports:?}");
      };
      assert_eq!(summary.duration_s, 60);
      assert_eq!(summary.frozen_s, 20);
      assert_eq!(summary.freezes, 1);
      assert_eq!(summary.max_loss_percent, 50);
      assert!(summary.was_fallback);
    }

    #[test]
    fn a_good_or_short_call_sends_nothing() {
      let mut good = joined(2);
      good.observe_stats(&stats(100, 1_000, 0), 0);
      good.observe_stats(&stats(200, 2_000, 1), 3_000);
      good.finish(600_000);
      assert!(good.take_reports().is_empty());
      let mut short = joined(2);
      for at in [1_000, 2_000, 3_000] {
        reconnect(&mut short, at);
      }
      short.take_reports();
      short.finish(10_000);
      assert!(short.take_reports().is_empty());
    }
  }

  fn telemetry() -> CallTelemetry {
    CallTelemetry::new(
      CallRef::from_random_bytes([7; 8]),
      0,
      START_UNIX_MS,
      TelemetryThresholds::default(),
    )
  }

  fn joined(devices: i64) -> CallTelemetry {
    let mut telemetry = telemetry();
    telemetry.on_event(
      &CallEngineEvent::Joined {
        call_id: CallId::from_snowflake(Snowflake::new(77).unwrap()),
        participants: (1..=devices)
          .map(|index| CallParticipant {
            user_id: UserId::from_snowflake(Snowflake::new(index).unwrap()),
            device_id: device(index),
            audio_stream_id: MediaStreamId(u32::try_from(index).unwrap() + 100),
          })
          .collect(),
      },
      0,
    );
    telemetry
  }

  fn reconnect(telemetry: &mut CallTelemetry, at: u64) {
    telemetry.on_event(&CallEngineEvent::Reconnecting, at);
    telemetry.on_event(&CallEngineEvent::Reconnected, at + 500);
  }

  fn device(raw: i64) -> DeviceId {
    DeviceId::from_snowflake(Snowflake::new(raw + 1_000_000_000).unwrap())
  }

  /// One own camera (AV1, 720p top layer) and one received camera (VP9, 360p arriving).
  fn stats(frames: u64, packets: u64, packets_lost: u64) -> VideoStatsReport {
    let size = |width, height| VideoSize { width, height };
    VideoStatsReport {
      at_ms: 0,
      maybe_rtt_ms: Some(80),
      maybe_uplink: Some(UplinkStats {
        estimate_bps: 1_500_000,
        loss_percent: 1,
      }),
      tracks: vec![
        VideoTrackStats::Sent(SentVideoStats {
          stream_id: MediaStreamId(10),
          kind: VideoTrackKind::Camera,
          codec: VideoCodec::Av1,
          maybe_encoder: Some(VideoEncoderInfo {
            name: "WebCodecs".to_owned(),
            implementation: EncoderImplementation::Browser,
          }),
          maybe_capture: Some(size(1280, 720)),
          layers: vec![
            SentLayerStats {
              layer: 0,
              size: size(640, 360),
              is_target: true,
              pictures: 10,
              bytes: 1_000,
              keyframes: 1,
            },
            SentLayerStats {
              layer: 1,
              size: size(1280, 720),
              is_target: true,
              pictures: 10,
              bytes: 4_000,
              keyframes: 1,
            },
          ],
          packets: 100,
          bytes: 5_000,
          maybe_frames_captured: None,
          pictures_dropped: 0,
          maybe_frames_black: None,
          maybe_frames_given: None,
          maybe_frames_skipped: None,
          maybe_held_back_ms: None,
          maybe_camera_limits: None,
          maybe_share_limits: None,
          maybe_capture_buffers: None,
        }),
        VideoTrackStats::Received(ReceivedVideoStats {
          stream_id: MediaStreamId(20),
          kind: VideoTrackKind::Camera,
          codec: VideoCodec::Vp9,
          layers: vec![
            OfferedLayer {
              layer: 0,
              size: size(640, 360),
            },
            OfferedLayer {
              layer: 1,
              size: size(1280, 720),
            },
          ],
          maybe_layer: Some(0),
          maybe_max_layer: Some(1),
          packets,
          bytes: packets * 1_000,
          packets_lost,
          packets_recovered: 0,
          frames,
          frames_discarded: 0,
          maybe_jitter_ms: Some(12.4),
          keyframe_requests: 0,
          maybe_longest_gap_ms: None,
          maybe_max_framerate: None,
          is_sender_capture_stalled: false,
          maybe_decode_queue: None,
          maybe_device_limit: None,
        }),
      ],
      maybe_uplink_probe: None,
      maybe_downlink_probe: None,
    }
  }
}
