//! Call problems as handled reports (docs/crash-reports.md, "Call problems"; issue #1762): what
//! the media layer saw go wrong in a call (encryption that failed, a freeze, reconnect churn, the
//! encoder giving up), with the call's state at that moment, so a report from the field can be read
//! without asking the user for logs.
//!
//! The media drivers build [`CallReport`]s on their slow paths (engine events, a stats sample every
//! few seconds, the call's end; `teamcraft_media::call_telemetry`) and hand them to the page with
//! the call snapshot. The page lets them through a [`CallReportGate`] (a cap per call and per page
//! life) and captures them like any handled error: sampled, scrubbed, queued.
//!
//! Privacy by construction: a report has no field for a user, device, conversation or call ID, a
//! name, a key, a key binding, a privacy code or media. The call is named by a [`CallRef`], random
//! per call and device, which only groups the reports of one call on one device. Every value is a
//! number, an enum or an identifier from the code (an error code, a protocol violation's name),
//! which [`CallReport::scrubbed`] cuts to identifier characters.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use teamcraft_proto::{VideoCodec, VideoTrackKind};

use super::event::{CrashEvent, CrashLevel, ErrorSource, EventId, hex};
use super::scrub::MAX_BREADCRUMBS;
use crate::video_stats::EncoderImplementation;

/// Lines of the call's own trail a report carries at most (its breadcrumbs, beside the log's).
pub const MAX_CALL_TRAIL: usize = 20;
/// Received tracks a report describes at most; a big meeting's others are left out.
pub const MAX_RECEIVED_TRACKS: usize = 8;
/// Reports a page sends per life at most, whatever the per-call cap: a page in call after call.
const MAX_REPORTS_PER_PAGE: u32 = 30;
/// Calls the gate remembers; the oldest is forgotten beyond it.
const MAX_CALLS_REMEMBERED: usize = 16;
/// A report older than this when the page sees it is not sent: it waited in a page that was
/// frozen in the background, and what it says is stale.
const MAX_REPORT_AGE_MS: i64 = 5 * 60 * 1000;
/// Longest identifier kept (an error code, a protocol violation's name).
const MAX_IDENTIFIER_CHARS: usize = 64;

/// One problem of one call, with the call's state when it happened.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallReport {
  pub call_ref: CallRef,
  /// Counts the reports of the call from 1, so the page takes each once.
  pub seq: u32,
  /// When it happened (Unix milliseconds, the device's clock; only for display and the age check).
  pub at_ms: i64,
  pub problem: CallProblem,
  pub context: CallContext,
  /// What the call did before, oldest first: `+12.3 s reconnecting`, times since the join.
  #[serde(default)]
  pub trail: Vec<String>,
}

/// Names one call on one device in its reports: 8 random bytes, made when the call starts, never
/// derived from the call, the conversation or the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CallRef([u8; 8]);

/// What went wrong. Each kind is one issue in the backend ([`CallProblem::fingerprint`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CallProblem {
  /// The call's end-to-end encryption couldn't take this device in: the SFU refused it after its
  /// joins stalled the group, or nothing secured the media in time (#1555).
  EncryptionFailed { is_refused: bool, retry_after_s: u32 },
  /// Another device was refused by the encryption (#1763).
  MemberRefused,
  /// A resumed DAVE group failed its checks after a reconnect and was dropped (#1763).
  ResumeFailed,
  /// A member's identity didn't check out: the call stopped its media (e2ee.md §8).
  Unverified { cause: Unverified },
  /// An own track's encoder gave up (#1766).
  VideoEncodingFailed {
    video: VideoTrackKind,
    codec: VideoCodec,
    maybe_encoder: Option<EncoderImplementation>,
    lines: u16,
    layer: u8,
  },
  /// More reconnects than the threshold within its window.
  ReconnectChurn { reconnects: u32, window_s: u32 },
  /// A subscribed track showed no new picture for longer than the threshold.
  VideoFreeze {
    video: VideoTrackKind,
    codec: VideoCodec,
    frozen_s: u32,
    /// Packets of the track still arrived: the decoder or a missing keyframe, not the network.
    is_media_arriving: bool,
  },
  /// The join failed (`is_joined` false) or the call ended with an error; `code` is the failure's
  /// stable wire name (`call_server_unreachable`, `call_encryption_failed`, `dave`).
  CallFailed { code: String, is_joined: bool },
  /// The call runs on the WebSocket fallback (UDP blocked on the way).
  TransportFallback,
  /// The SFU refused a request of this device, by its stable error code.
  SfuRejected { code: String },
  /// The SFU broke the protocol as the engine reads it (`CallEngineError::Protocol`).
  ProtocolError { what: String },
  /// A call that went badly, summed up at its end; only over a threshold.
  PoorCall(CallQualitySummary),
  #[serde(other)]
  Unknown,
}

/// Why a member's identity didn't check out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unverified {
  /// A device in the DAVE group without a verified call key.
  UnverifiedMember,
  /// A device came back with the call key it had before it left.
  RetiredKey,
  /// The SFU proposed members without a trusted roster to check them against.
  UntrustedRoster,
  /// No authenticator: DAVE members are not bound to device identities.
  UnauthenticatedKeys,
}

/// A call's bad parts, over its whole life.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallQualitySummary {
  pub duration_s: u32,
  /// Time subscribed tracks stood still, summed over the tracks.
  pub frozen_s: u32,
  pub freezes: u32,
  pub reconnects: u32,
  /// Stats samples with a downlink loss of 5 % or more.
  pub lossy_samples: u32,
  pub max_loss_percent: u8,
  pub max_rtt_ms: u32,
  pub was_fallback: bool,
  #[serde(default)]
  pub maybe_min_uplink_kbps: Option<u32>,
}

/// The call when the problem happened. Numbers and enums only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallContext {
  #[serde(default)]
  pub maybe_transport: Option<CallTransportKind>,
  pub since_join_s: u32,
  pub reconnects: u32,
  #[serde(default)]
  pub maybe_dave_epoch: Option<u64>,
  /// Devices in the call, this one included.
  pub devices: u16,
  #[serde(default)]
  pub sent: Vec<SentVideoContext>,
  #[serde(default)]
  pub received: Vec<ReceivedVideoContext>,
  #[serde(default)]
  pub maybe_uplink_kbps: Option<u32>,
  #[serde(default)]
  pub maybe_uplink_loss_percent: Option<u8>,
  #[serde(default)]
  pub maybe_downlink_loss_percent: Option<u8>,
  #[serde(default)]
  pub maybe_rtt_ms: Option<u32>,
  /// The highest jitter of the received tracks.
  #[serde(default)]
  pub maybe_jitter_ms: Option<u32>,
}

/// What carries the call to the SFU.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallTransportKind {
  Quic,
  Fallback,
}

/// An own track.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentVideoContext {
  pub video: VideoTrackKind,
  pub codec: VideoCodec,
  #[serde(default)]
  pub maybe_encoder: Option<EncoderImplementation>,
  /// Lines of the highest layer that sends.
  pub top_lines: u16,
  pub layers: u8,
}

/// Another device's track this device receives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceivedVideoContext {
  pub video: VideoTrackKind,
  pub codec: VideoCodec,
  /// Lines of the layer arriving; 0 when none arrives.
  pub lines: u16,
  #[serde(default)]
  pub maybe_layer: Option<u8>,
  #[serde(default)]
  pub maybe_max_layer: Option<u8>,
}

/// The browser a page runs in: its family and major version, nothing finer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrowserInfo {
  pub family: BrowserFamily,
  pub major_version: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserFamily {
  Chrome,
  Edge,
  Firefox,
  Safari,
  Other,
}

/// A call report on its way into a [`CrashEvent`], with what the page adds.
#[derive(Clone, Debug, PartialEq)]
pub struct CallEventDetail {
  pub report: CallReport,
  pub maybe_browser: Option<BrowserInfo>,
}

/// The page's limits: each report once, at most the instance's cap per call, and
/// [`MAX_REPORTS_PER_PAGE`] per page life.
#[derive(Debug)]
pub struct CallReportGate {
  per_call: u8,
  /// Per call: the highest sequence number seen and how many were let through. Oldest first.
  calls: VecDeque<(CallRef, u32, u8)>,
  total: u32,
}

impl CallReport {
  /// The handled event for this report: the level and type of its problem, a message that names
  /// the problem's kind and stable parts only (one cause, one issue), the trail after the log.
  pub fn into_event(
    self,
    event_id: EventId,
    maybe_browser: Option<BrowserInfo>,
    mut breadcrumbs: Vec<String>,
  ) -> CrashEvent {
    let trail: Vec<String> = self
      .trail
      .iter()
      .rev()
      .take(MAX_CALL_TRAIL)
      .rev()
      .map(|line| format!("call {line}"))
      .collect();
    let keep = MAX_BREADCRUMBS.saturating_sub(trail.len());
    let skip = breadcrumbs.len().saturating_sub(keep);
    breadcrumbs.drain(..skip);
    breadcrumbs.extend(trail);
    CrashEvent {
      event_id,
      timestamp_ms: self.at_ms,
      level: self.problem.level(),
      source: ErrorSource::Rust,
      error_type: self.problem.error_type().to_owned(),
      message: self.problem.message(),
      frames: Vec::new(),
      breadcrumbs,
      maybe_call: Some(Box::new(CallEventDetail {
        report: self,
        maybe_browser,
      })),
    }
  }

  /// The report with every identifier cut to identifier characters and the lists to their bounds.
  pub fn scrubbed(self) -> Self {
    Self {
      problem: self.problem.scrubbed(),
      context: self.context.scrubbed(),
      trail: self.trail.into_iter().rev().take(MAX_CALL_TRAIL).rev().collect(),
      ..self
    }
  }
}

impl CallProblem {
  /// The problem's kind as tags and fingerprints name it.
  pub fn kind(&self) -> &'static str {
    match self {
      Self::EncryptionFailed { .. } => "encryption_failed",
      Self::MemberRefused => "member_refused",
      Self::ResumeFailed => "resume_failed",
      Self::Unverified { .. } => "unverified",
      Self::VideoEncodingFailed { .. } => "video_encoding_failed",
      Self::ReconnectChurn { .. } => "reconnect_churn",
      Self::VideoFreeze { .. } => "video_freeze",
      Self::CallFailed { .. } => "call_failed",
      Self::TransportFallback => "transport_fallback",
      Self::SfuRejected { .. } => "sfu_rejected",
      Self::ProtocolError { .. } => "protocol_error",
      Self::PoorCall(_) => "poor_call",
      Self::Unknown => "unknown",
    }
  }

  /// Errors break the call or its security; warnings degrade it. Both are sampled.
  pub fn level(&self) -> CrashLevel {
    match self {
      Self::EncryptionFailed { .. }
      | Self::MemberRefused
      | Self::ResumeFailed
      | Self::Unverified { .. }
      | Self::VideoEncodingFailed { .. }
      | Self::CallFailed { .. }
      | Self::ProtocolError { .. } => CrashLevel::Error,
      Self::ReconnectChurn { .. }
      | Self::VideoFreeze { .. }
      | Self::TransportFallback
      | Self::SfuRejected { .. }
      | Self::PoorCall(_)
      | Self::Unknown => CrashLevel::Warning,
    }
  }

  /// The exception type the backend shows.
  pub fn error_type(&self) -> &'static str {
    match self {
      Self::EncryptionFailed { .. } => "CallEncryptionFailed",
      Self::MemberRefused => "CallMemberRefused",
      Self::ResumeFailed => "CallResumeFailed",
      Self::Unverified { .. } => "CallUnverifiedMember",
      Self::VideoEncodingFailed { .. } => "VideoEncodingFailed",
      Self::ReconnectChurn { .. } => "CallReconnectChurn",
      Self::VideoFreeze { .. } => "CallVideoFreeze",
      Self::CallFailed { .. } => "CallFailed",
      Self::TransportFallback => "CallTransportFallback",
      Self::SfuRejected { .. } => "CallSfuRejected",
      Self::ProtocolError { .. } => "CallProtocolError",
      Self::PoorCall(_) => "PoorCall",
      Self::Unknown => "CallProblem",
    }
  }

  /// Groups reports into issues: the kind and its stable discriminator (a cause, a codec, a code),
  /// never a number that varies from call to call.
  pub fn fingerprint(&self) -> Vec<String> {
    let mut parts = vec!["call".to_owned(), self.kind().to_owned()];
    match self {
      Self::EncryptionFailed { is_refused, .. } => {
        parts.push(if *is_refused { "refused" } else { "setup_timeout" }.to_owned());
      },
      Self::Unverified { cause } => parts.push(cause.as_str().to_owned()),
      Self::VideoEncodingFailed {
        video,
        codec,
        maybe_encoder,
        ..
      } => {
        parts.push(video.as_str().to_owned());
        parts.push(codec.as_str().to_owned());
        parts.push(encoder_name(*maybe_encoder).to_owned());
      },
      Self::VideoFreeze {
        video,
        is_media_arriving,
        ..
      } => {
        parts.push(video.as_str().to_owned());
        parts.push(if *is_media_arriving { "arriving" } else { "silent" }.to_owned());
      },
      Self::CallFailed { code, is_joined } => {
        parts.push(code.clone());
        parts.push(if *is_joined { "in_call" } else { "join" }.to_owned());
      },
      Self::SfuRejected { code } => parts.push(code.clone()),
      Self::ProtocolError { what } => parts.push(what.clone()),
      _ => {},
    }
    parts
  }

  /// The event's message: the kind and its stable parts, so the issue's title says what happened.
  /// The numbers are in the `call` context.
  fn message(&self) -> String {
    match self {
      Self::EncryptionFailed { is_refused: true, .. } => "encryption: the SFU refused this device".to_owned(),
      Self::EncryptionFailed {
        is_refused: false, ..
      } => "encryption: not secured in time".to_owned(),
      Self::MemberRefused => "encryption: another member was refused".to_owned(),
      Self::ResumeFailed => "encryption: the resumed group failed its checks".to_owned(),
      Self::Unverified { cause } => format!("unverified member: {}", cause.as_str()),
      Self::VideoEncodingFailed {
        video,
        codec,
        maybe_encoder,
        ..
      } => format!(
        "{} encoder gave up ({}, {})",
        video.as_str(),
        codec.as_str(),
        encoder_name(*maybe_encoder)
      ),
      Self::ReconnectChurn { .. } => "the call reconnected again and again".to_owned(),
      Self::VideoFreeze {
        video,
        codec,
        is_media_arriving,
        ..
      } => format!(
        "{} picture froze ({}, {})",
        video.as_str(),
        codec.as_str(),
        if *is_media_arriving {
          "packets still arriving"
        } else {
          "no packets"
        }
      ),
      Self::CallFailed {
        code,
        is_joined: false,
      } => format!("join failed: {code}"),
      Self::CallFailed {
        code,
        is_joined: true,
      } => format!("call ended with an error: {code}"),
      Self::TransportFallback => "the call runs on the WebSocket fallback".to_owned(),
      Self::SfuRejected { code } => format!("the SFU refused a request: {code}"),
      Self::ProtocolError { what } => format!("protocol violation: {what}"),
      Self::PoorCall(_) => "a call went badly".to_owned(),
      Self::Unknown => "call problem".to_owned(),
    }
  }

  /// The problem's own numbers, for the `call` context.
  fn details(&self) -> Value {
    match self {
      Self::EncryptionFailed { retry_after_s, .. } => json!({ "retry_after_s": retry_after_s }),
      Self::VideoEncodingFailed { lines, layer, .. } => json!({ "lines": lines, "layer": layer }),
      Self::ReconnectChurn { reconnects, window_s } => {
        json!({ "reconnects_in_window": reconnects, "window_s": window_s })
      },
      Self::VideoFreeze { frozen_s, .. } => json!({ "frozen_s": frozen_s }),
      Self::PoorCall(summary) => serde_json::to_value(summary).unwrap_or(Value::Null),
      _ => Value::Null,
    }
  }

  fn scrubbed(self) -> Self {
    match self {
      Self::CallFailed { code, is_joined } => Self::CallFailed {
        code: identifier(&code),
        is_joined,
      },
      Self::SfuRejected { code } => Self::SfuRejected {
        code: identifier(&code),
      },
      Self::ProtocolError { what } => Self::ProtocolError {
        what: identifier(&what),
      },
      Self::VideoEncodingFailed {
        video,
        codec,
        maybe_encoder,
        lines,
        layer,
      } => Self::VideoEncodingFailed {
        video: scrub_kind(video),
        codec: scrub_codec(codec),
        maybe_encoder,
        lines,
        layer,
      },
      Self::VideoFreeze {
        video,
        codec,
        frozen_s,
        is_media_arriving,
      } => Self::VideoFreeze {
        video: scrub_kind(video),
        codec: scrub_codec(codec),
        frozen_s,
        is_media_arriving,
      },
      other => other,
    }
  }
}

impl Unverified {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::UnverifiedMember => "unverified_member",
      Self::RetiredKey => "retired_key",
      Self::UntrustedRoster => "untrusted_roster",
      Self::UnauthenticatedKeys => "unauthenticated_keys",
    }
  }
}

impl CallContext {
  fn scrubbed(self) -> Self {
    Self {
      sent: self
        .sent
        .into_iter()
        .take(2)
        .map(|sent| SentVideoContext {
          video: scrub_kind(sent.video),
          codec: scrub_codec(sent.codec),
          ..sent
        })
        .collect(),
      received: self
        .received
        .into_iter()
        .take(MAX_RECEIVED_TRACKS)
        .map(|received| ReceivedVideoContext {
          video: scrub_kind(received.video),
          codec: scrub_codec(received.codec),
          ..received
        })
        .collect(),
      ..self
    }
  }

  /// Tags the backend can search by: few values each.
  fn tags(&self) -> BTreeMap<&'static str, String> {
    let mut tags = BTreeMap::new();
    if let Some(transport) = self.maybe_transport {
      tags.insert("call.transport", transport.as_str().to_owned());
    }
    tags.insert("call.devices", devices_bucket(self.devices).to_owned());
    if let Some(codecs) = codec_list(self.sent.iter().map(|sent| &sent.codec)) {
      tags.insert("call.codec_sent", codecs);
    }
    if let Some(codecs) = codec_list(self.received.iter().map(|received| &received.codec)) {
      tags.insert("call.codec_received", codecs);
    }
    if let Some(encoder) = self.sent.iter().find_map(|sent| sent.maybe_encoder) {
      tags.insert("call.encoder", encoder_name(Some(encoder)).to_owned());
    }
    tags.insert(
      "call.reconnected",
      if self.reconnects > 0 { "yes" } else { "no" }.to_owned(),
    );
    tags
  }
}

impl CallTransportKind {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Quic => "quic",
      Self::Fallback => "fallback",
    }
  }
}

impl BrowserInfo {
  /// From a user agent string: the family and major version only.
  pub fn from_user_agent(agent: &str) -> Self {
    // Order matters: Edge and Chrome name Safari too, Edge names Chrome.
    let checks: [(&str, BrowserFamily); 5] = [
      ("Firefox/", BrowserFamily::Firefox),
      ("Edg/", BrowserFamily::Edge),
      ("Chrome/", BrowserFamily::Chrome),
      ("CriOS/", BrowserFamily::Chrome),
      ("Version/", BrowserFamily::Safari),
    ];
    for (marker, family) in checks {
      if let Some(at) = agent.find(marker) {
        if family == BrowserFamily::Safari && !agent.contains("Safari/") {
          continue;
        }
        let digits: String = agent[at + marker.len()..]
          .chars()
          .take_while(char::is_ascii_digit)
          .take(4)
          .collect();
        return Self {
          family,
          major_version: digits.parse().unwrap_or(0),
        };
      }
    }
    Self {
      family: BrowserFamily::Other,
      major_version: 0,
    }
  }

  fn name(self) -> &'static str {
    match self.family {
      BrowserFamily::Chrome => "Chrome",
      BrowserFamily::Edge => "Edge",
      BrowserFamily::Firefox => "Firefox",
      BrowserFamily::Safari => "Safari",
      BrowserFamily::Other => "Other",
    }
  }
}

impl CallEventDetail {
  /// What the envelope adds for a call event: its fingerprint, tags and contexts.
  pub(super) fn apply(&self, payload: &mut Value) {
    let report = &self.report;
    payload["fingerprint"] = json!(report.problem.fingerprint());
    if let Some(tags) = payload["tags"].as_object_mut() {
      tags.insert("call.problem".to_owned(), json!(report.problem.kind()));
      for (name, value) in report.context.tags() {
        tags.insert(name.to_owned(), json!(value));
      }
      if let Some(browser) = self.maybe_browser {
        tags.insert(
          "browser".to_owned(),
          json!(format!("{} {}", browser.name(), browser.major_version)),
        );
        tags.insert("browser.name".to_owned(), json!(browser.name()));
      }
    }
    let mut context = serde_json::to_value(&report.context).unwrap_or(Value::Null);
    context["call_ref"] = json!(report.call_ref.to_string());
    context["seq"] = json!(report.seq);
    context["problem"] = json!(report.problem.kind());
    let details = report.problem.details();
    if !details.is_null() {
      context["problem_details"] = details;
    }
    payload["contexts"]["call"] = context;
    if let Some(browser) = self.maybe_browser {
      payload["contexts"]["browser"] = json!({
        "name": browser.name(),
        "version": browser.major_version.to_string(),
      });
    }
  }
}

impl CallReportGate {
  /// `per_call`: the instance's cap (0: no call reports at all).
  pub fn new(per_call: u8) -> Self {
    Self {
      per_call,
      calls: VecDeque::new(),
      total: 0,
    }
  }

  pub fn set_per_call(&mut self, per_call: u8) {
    self.per_call = per_call;
  }

  /// Whether `report` goes on to the reporter: new, recent, and within the caps. Counts it.
  pub fn admit(&mut self, report: &CallReport, now_ms: i64) -> bool {
    if self.per_call == 0 || self.total >= MAX_REPORTS_PER_PAGE {
      return false;
    }
    let position = match self.calls.iter().position(|(call, ..)| *call == report.call_ref) {
      Some(position) => position,
      None => {
        if self.calls.len() >= MAX_CALLS_REMEMBERED {
          self.calls.pop_front();
        }
        self.calls.push_back((report.call_ref, 0, 0));
        self.calls.len() - 1
      },
    };
    let (_, last_seq, taken) = &mut self.calls[position];
    if report.seq <= *last_seq {
      return false;
    }
    *last_seq = report.seq;
    if *taken >= self.per_call || now_ms.saturating_sub(report.at_ms) > MAX_REPORT_AGE_MS {
      return false;
    }
    *taken += 1;
    self.total += 1;
    true
  }
}

impl CallRef {
  /// From 8 bytes of a random source.
  pub fn from_random_bytes(bytes: [u8; 8]) -> Self {
    Self(bytes)
  }
}

impl fmt::Display for CallRef {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&hex(&self.0))
  }
}

impl TryFrom<String> for CallRef {
  type Error = &'static str;

  fn try_from(text: String) -> Result<Self, Self::Error> {
    if text.len() != 16 || !text.is_ascii() {
      return Err("expected 16 hex digits");
    }
    let mut bytes = [0; 8];
    for (index, byte) in bytes.iter_mut().enumerate() {
      *byte =
        u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).map_err(|_| "expected 16 hex digits")?;
    }
    Ok(Self(bytes))
  }
}

impl From<CallRef> for String {
  fn from(call_ref: CallRef) -> Self {
    call_ref.to_string()
  }
}

/// An identifier from the code (an error code, a violation's name): lowercase ASCII letters,
/// digits and `_ . : -`, spaces as `_`, at most [`MAX_IDENTIFIER_CHARS`]; anything else makes it
/// `other`, so a value that isn't what the code wrote can't carry text.
pub fn identifier(text: &str) -> String {
  let cleaned: String = text
    .trim()
    .chars()
    .take(MAX_IDENTIFIER_CHARS)
    .map(|character| if character == ' ' { '_' } else { character })
    .collect();
  let is_identifier = !cleaned.is_empty()
    && cleaned.chars().all(|character| {
      character.is_ascii_lowercase()
        || character.is_ascii_digit()
        || matches!(character, '_' | '.' | ':' | '-')
    });
  if is_identifier {
    cleaned
  } else {
    "other".to_owned()
  }
}

fn scrub_codec(codec: VideoCodec) -> VideoCodec {
  match codec {
    VideoCodec::Unknown(name) => VideoCodec::Unknown(identifier(&name)),
    known => known,
  }
}

fn scrub_kind(kind: VideoTrackKind) -> VideoTrackKind {
  match kind {
    VideoTrackKind::Unknown(name) => VideoTrackKind::Unknown(identifier(&name)),
    known => known,
  }
}

fn encoder_name(maybe_encoder: Option<EncoderImplementation>) -> &'static str {
  match maybe_encoder {
    Some(EncoderImplementation::Browser) => "webcodecs",
    Some(EncoderImplementation::Hardware) => "hardware",
    Some(EncoderImplementation::Software) => "software",
    None => "unknown",
  }
}

/// The call's size as a tag with few values.
fn devices_bucket(devices: u16) -> &'static str {
  match devices {
    0..=1 => "1",
    2 => "2",
    3..=5 => "3-5",
    6..=10 => "6-10",
    _ => "11+",
  }
}

/// The codecs, sorted and without repeats (`av1,vp9`); `None` for none.
fn codec_list<'a>(codecs: impl Iterator<Item = &'a VideoCodec>) -> Option<String> {
  let mut names: Vec<String> = codecs.map(|codec| identifier(codec.as_str())).collect();
  names.sort();
  names.dedup();
  (!names.is_empty()).then(|| names.join(","))
}

#[cfg(test)]
mod tests {
  use super::*;

  const NOW_MS: i64 = 1_700_000_000_000;

  mod events {
    use super::*;

    #[test]
    fn a_freeze_becomes_a_warning_with_its_fingerprint_tags_and_context() {
      let event = report(1, freeze()).into_event(
        EventId::from_random_bytes([0; 16]),
        Some(BrowserInfo {
          family: BrowserFamily::Firefox,
          major_version: 133,
        }),
        vec!["2026-11-11T10:00:00Z INFO teamcraft_media: joined".to_owned()],
      );
      assert_eq!(event.level, CrashLevel::Warning);
      assert_eq!(event.error_type, "CallVideoFreeze");
      assert_eq!(
        event.message,
        "camera picture froze (vp9, packets still arriving)"
      );
      assert_eq!(event.breadcrumbs.last().unwrap(), "call +12.0 s reconnecting");
      let payload = payload(event);
      assert_eq!(
        payload["fingerprint"],
        json!(["call", "video_freeze", "camera", "arriving"])
      );
      assert_eq!(payload["tags"]["call.problem"], json!("video_freeze"));
      assert_eq!(payload["tags"]["call.transport"], json!("fallback"));
      assert_eq!(payload["tags"]["call.codec_received"], json!("vp9"));
      assert_eq!(payload["tags"]["call.codec_sent"], json!("av1"));
      assert_eq!(payload["tags"]["call.encoder"], json!("webcodecs"));
      assert_eq!(payload["tags"]["call.devices"], json!("3-5"));
      assert_eq!(payload["tags"]["browser"], json!("Firefox 133"));
      let call = &payload["contexts"]["call"];
      assert_eq!(call["call_ref"], json!("0102030405060708"));
      assert_eq!(call["since_join_s"], json!(42));
      assert_eq!(call["maybe_dave_epoch"], json!(5));
      assert_eq!(call["maybe_rtt_ms"], json!(180));
      assert_eq!(call["problem_details"]["frozen_s"], json!(9));
      assert_eq!(payload["contexts"]["browser"]["name"], json!("Firefox"));
    }

    #[test]
    fn breaking_problems_are_errors_and_degrading_ones_warnings() {
      for problem in [
        CallProblem::EncryptionFailed {
          is_refused: true,
          retry_after_s: 60,
        },
        CallProblem::MemberRefused,
        CallProblem::ResumeFailed,
        CallProblem::Unverified {
          cause: Unverified::RetiredKey,
        },
        CallProblem::ProtocolError {
          what: "joined twice".to_owned(),
        },
        CallProblem::CallFailed {
          code: "dave".to_owned(),
          is_joined: true,
        },
      ] {
        assert_eq!(problem.level(), CrashLevel::Error, "{problem:?}");
      }
      for problem in [
        CallProblem::TransportFallback,
        CallProblem::ReconnectChurn {
          reconnects: 4,
          window_s: 300,
        },
        CallProblem::PoorCall(CallQualitySummary::default()),
        freeze(),
      ] {
        assert_eq!(problem.level(), CrashLevel::Warning, "{problem:?}");
      }
    }

    #[test]
    fn the_trail_and_the_log_share_the_breadcrumb_budget() {
      let mut report = report(1, CallProblem::TransportFallback);
      report.trail = (0..40).map(|index| format!("+{index}.0 s event")).collect();
      let log: Vec<String> = (0..50).map(|index| format!("log {index}")).collect();
      let event = report.into_event(EventId::from_random_bytes([0; 16]), None, log);
      assert_eq!(event.breadcrumbs.len(), MAX_BREADCRUMBS);
      assert_eq!(event.breadcrumbs[0], "log 20");
      assert_eq!(event.breadcrumbs.last().unwrap(), "call +39.0 s event");
    }

    #[test]
    fn reports_round_trip_through_the_snapshot_json() {
      let report = report(3, freeze());
      let json = serde_json::to_string(&report).unwrap();
      assert_eq!(serde_json::from_str::<CallReport>(&json).unwrap(), report);
      let newer: CallReport = serde_json::from_value(json!({
        "call_ref": "0102030405060708",
        "seq": 1,
        "at_ms": NOW_MS,
        "problem": { "kind": "something_new" },
        "context": { "since_join_s": 1, "reconnects": 0, "devices": 2 },
      }))
      .unwrap();
      assert_eq!(newer.problem, CallProblem::Unknown);
    }
  }

  mod fingerprints {
    use super::*;

    #[test]
    fn they_hold_the_kind_and_stable_parts_never_numbers() {
      assert_eq!(
        CallProblem::EncryptionFailed {
          is_refused: false,
          retry_after_s: 0
        }
        .fingerprint(),
        ["call", "encryption_failed", "setup_timeout"]
      );
      assert_eq!(
        CallProblem::VideoEncodingFailed {
          video: VideoTrackKind::Screen,
          codec: VideoCodec::Av1,
          maybe_encoder: Some(EncoderImplementation::Hardware),
          lines: 1440,
          layer: 2,
        }
        .fingerprint(),
        ["call", "video_encoding_failed", "screen", "av1", "hardware"]
      );
      assert_eq!(
        CallProblem::CallFailed {
          code: "call_server_unreachable".to_owned(),
          is_joined: false
        }
        .fingerprint(),
        ["call", "call_failed", "call_server_unreachable", "join"]
      );
      let churn = |reconnects| CallProblem::ReconnectChurn {
        reconnects,
        window_s: 300,
      };
      assert_eq!(churn(4).fingerprint(), churn(9).fingerprint());
      let poor = |frozen_s| {
        CallProblem::PoorCall(CallQualitySummary {
          frozen_s,
          ..CallQualitySummary::default()
        })
      };
      assert_eq!(poor(10).fingerprint(), poor(300).fingerprint());
    }
  }

  mod scrubbing {
    use super::*;

    #[test]
    fn identifiers_keep_code_words_and_nothing_else() {
      assert_eq!(identifier("unknown_video_track"), "unknown_video_track");
      assert_eq!(identifier("joined twice"), "joined_twice");
      assert_eq!(identifier("call_encryption_failed"), "call_encryption_failed");
      assert_eq!(identifier("Alice Example"), "other");
      assert_eq!(identifier("alice@example.com"), "other");
      assert_eq!(identifier("\"secret\""), "other");
      assert_eq!(identifier(""), "other");
      assert_eq!(identifier(&"a".repeat(200)).len(), MAX_IDENTIFIER_CHARS);
    }

    #[test]
    fn a_report_carrying_secrets_in_its_strings_loses_them() {
      let secrets = [
        "Alice Example",
        "alice@example.com",
        "eyJhbGciOi.secret.jwt",
        "/home/alice",
      ];
      let mut report = report(
        1,
        CallProblem::ProtocolError {
          what: secrets[0].to_owned(),
        },
      );
      report.context.received[0].codec = VideoCodec::Unknown(secrets[1].to_owned());
      report.context.sent[0].video = VideoTrackKind::Unknown(secrets[3].to_owned());
      let rejected = report.clone().scrubbed();
      let sfu = CallReport {
        problem: CallProblem::SfuRejected {
          code: secrets[2].to_owned(),
        },
        ..report.clone()
      }
      .scrubbed();
      let failed = CallReport {
        problem: CallProblem::CallFailed {
          code: secrets[1].to_owned(),
          is_joined: true,
        },
        ..report
      }
      .scrubbed();
      for scrubbed in [rejected, sfu, failed] {
        let envelope =
          payload(scrubbed.into_event(EventId::from_random_bytes([0; 16]), None, Vec::new())).to_string();
        for secret in secrets {
          assert!(!envelope.contains(secret), "{secret} in {envelope}");
        }
      }
    }

    #[test]
    fn the_call_ref_is_random_bytes_not_an_id() {
      let call_ref = CallRef::from_random_bytes([0xab; 8]);
      assert_eq!(call_ref.to_string(), "abababababababab");
      assert!(CallRef::try_from("123".to_owned()).is_err());
      assert!(CallRef::try_from("zzzzzzzzzzzzzzzz".to_owned()).is_err());
    }
  }

  mod gate {
    use super::*;

    #[test]
    fn each_report_passes_once() {
      let mut gate = CallReportGate::new(8);
      let first = report(1, CallProblem::TransportFallback);
      assert!(gate.admit(&first, NOW_MS));
      assert!(!gate.admit(&first, NOW_MS));
      assert!(gate.admit(&report(2, freeze()), NOW_MS));
    }

    #[test]
    fn a_call_sends_at_most_its_cap() {
      let mut gate = CallReportGate::new(2);
      let admitted = (1..=5)
        .filter(|seq| gate.admit(&report(*seq, freeze()), NOW_MS))
        .count();
      assert_eq!(admitted, 2);
      let mut other = report(1, freeze());
      other.call_ref = CallRef::from_random_bytes([9; 8]);
      assert!(gate.admit(&other, NOW_MS));
    }

    #[test]
    fn a_page_sends_at_most_its_cap_over_all_calls() {
      let mut gate = CallReportGate::new(u8::MAX);
      let admitted = (0..100_u8)
        .filter(|call| {
          let mut report = report(1, freeze());
          report.call_ref = CallRef::from_random_bytes([*call; 8]);
          gate.admit(&report, NOW_MS)
        })
        .count();
      assert_eq!(admitted, MAX_REPORTS_PER_PAGE as usize);
    }

    #[test]
    fn zero_turns_call_reports_off() {
      let mut gate = CallReportGate::new(0);
      assert!(!gate.admit(&report(1, freeze()), NOW_MS));
      gate.set_per_call(1);
      assert!(gate.admit(&report(2, freeze()), NOW_MS));
    }

    #[test]
    fn an_old_report_read_again_after_a_reload_is_not_sent() {
      let mut gate = CallReportGate::new(8);
      assert!(!gate.admit(&report(1, freeze()), NOW_MS + MAX_REPORT_AGE_MS + 1));
    }

    #[test]
    fn the_calls_remembered_are_bounded() {
      let mut gate = CallReportGate::new(8);
      for call in 0..(MAX_CALLS_REMEMBERED as u8 + 4) {
        let mut report = report(1, freeze());
        report.call_ref = CallRef::from_random_bytes([call; 8]);
        gate.admit(&report, NOW_MS);
      }
      assert_eq!(gate.calls.len(), MAX_CALLS_REMEMBERED);
    }
  }

  mod browsers {
    use super::*;

    #[test]
    fn the_family_and_major_version_and_nothing_finer() {
      let cases = [
        (
          "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:133.0) Gecko/20100101 Firefox/133.0",
          BrowserFamily::Firefox,
          133,
        ),
        (
          "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
          BrowserFamily::Chrome,
          131,
        ),
        (
          "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.2903.86",
          BrowserFamily::Edge,
          131,
        ),
        (
          "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Safari/605.1.15",
          BrowserFamily::Safari,
          18,
        ),
        ("curl/8.0", BrowserFamily::Other, 0),
      ];
      for (agent, family, major_version) in cases {
        assert_eq!(
          BrowserInfo::from_user_agent(agent),
          BrowserInfo {
            family,
            major_version
          },
          "{agent}"
        );
      }
    }
  }

  fn freeze() -> CallProblem {
    CallProblem::VideoFreeze {
      video: VideoTrackKind::Camera,
      codec: VideoCodec::Vp9,
      frozen_s: 9,
      is_media_arriving: true,
    }
  }

  fn report(seq: u32, problem: CallProblem) -> CallReport {
    CallReport {
      call_ref: CallRef::from_random_bytes([1, 2, 3, 4, 5, 6, 7, 8]),
      seq,
      at_ms: NOW_MS,
      problem,
      context: CallContext {
        maybe_transport: Some(CallTransportKind::Fallback),
        since_join_s: 42,
        reconnects: 1,
        maybe_dave_epoch: Some(5),
        devices: 3,
        sent: vec![SentVideoContext {
          video: VideoTrackKind::Camera,
          codec: VideoCodec::Av1,
          maybe_encoder: Some(EncoderImplementation::Browser),
          top_lines: 720,
          layers: 3,
        }],
        received: vec![ReceivedVideoContext {
          video: VideoTrackKind::Camera,
          codec: VideoCodec::Vp9,
          lines: 360,
          maybe_layer: Some(0),
          maybe_max_layer: Some(2),
        }],
        maybe_uplink_kbps: Some(900),
        maybe_uplink_loss_percent: Some(2),
        maybe_downlink_loss_percent: Some(7),
        maybe_rtt_ms: Some(180),
        maybe_jitter_ms: Some(30),
      },
      trail: vec!["+3.1 s joined".to_owned(), "+12.0 s reconnecting".to_owned()],
    }
  }

  fn payload(event: CrashEvent) -> Value {
    let envelope = event.to_envelope(
      &super::super::AppInfo {
        version: "1.0.9".to_owned(),
        platform: super::super::AppPlatform::Web,
        os_name: "Windows".to_owned(),
        maybe_os_version: None,
      },
      &super::super::Environment::default(),
      super::super::InstallationId::from_random_bytes([1; 16]),
    );
    serde_json::from_str(envelope.lines().nth(2).unwrap()).unwrap()
  }
}
