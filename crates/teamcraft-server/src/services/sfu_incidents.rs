//! The SFU's incidents as handled warnings in the crash reports' backend (docs/crash-reports.md,
//! "The SFU's incidents"; issue #1762): DAVE groups that started over, refused joiners, late
//! transitions, signaling queues that overflowed, and a high share of sessions on the WebSocket
//! fallback. The SFU counts them and sends the counts with its roster report; the server reports
//! them through its own path ([`crash_reports::capture_events`]), rate limited per kind.
//!
//! Counts only: the SFU's report names no call, device or address, and neither does an event.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use teamcraft_client_api::crash_reporting::{CrashEvent, CrashLevel, ErrorSource, EventId};
use teamcraft_proto::SfuCallQuality;

use crate::services::crash_reports;
use crate::state::AppState;

/// One kind of incident is reported at most this often; the counts in between are summed into it.
const INTERVAL: Duration = Duration::from_secs(30 * 60);
/// Incident reports per process at most.
const MAX_REPORTS: u32 = 100;
/// The fallback's share is judged once this many sessions joined since the last judgement…
const FALLBACK_MIN_SESSIONS: u32 = 20;
/// …and reported from this share on, in percent.
const FALLBACK_SHARE_PERCENT: u32 = 30;

/// What the server keeps between the SFU's reports: when each kind was last reported, the counts
/// since, and the sessions for the fallback's share.
#[derive(Debug, Default)]
pub struct IncidentLimiter {
  last_reported: HashMap<Kind, Instant>,
  /// Counts not yet reported, per kind.
  pending: HashMap<Kind, u32>,
  /// Extra numbers for the restart's breadcrumb: strikes and refusals summed since.
  strikes: u32,
  quic_sessions: u32,
  fallback_sessions: u32,
  /// The last judgement over the threshold: sessions on the fallback, and in all.
  maybe_last_fallback: Option<(u32, u32)>,
  reported: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
  DaveRestart,
  DaveRefusal,
  DaveLateTransition,
  SignalingOverflow,
  FallbackShare,
}

/// The server got an SFU roster report with this quality window: reports what is due.
pub fn report(state: &AppState, quality: &SfuCallQuality) {
  static LIMITER: OnceLock<Mutex<IncidentLimiter>> = OnceLock::new();
  let events = LIMITER
    .get_or_init(Mutex::default)
    .lock()
    .unwrap_or_else(PoisonError::into_inner)
    .take(quality, Instant::now(), now_ms(), random_event_id);
  if !events.is_empty() {
    crash_reports::capture_events(state, events);
  }
}

impl IncidentLimiter {
  /// Adds `quality`'s counts and returns the events due at `now`: each kind with counts at most
  /// once an [`INTERVAL`], [`MAX_REPORTS`] in all.
  pub fn take(
    &mut self,
    quality: &SfuCallQuality,
    now: Instant,
    now_ms: i64,
    mut event_id: impl FnMut() -> EventId,
  ) -> Vec<CrashEvent> {
    let incidents = quality.incidents;
    self.add(Kind::DaveRestart, incidents.dave_restarts);
    self.add(Kind::DaveRefusal, incidents.dave_refusals);
    self.add(Kind::DaveLateTransition, incidents.dave_late_transitions);
    self.add(Kind::SignalingOverflow, incidents.signaling_overflows);
    self.strikes = self.strikes.saturating_add(incidents.dave_strikes);
    self.judge_fallback(quality);
    let mut events = Vec::new();
    for kind in [
      Kind::DaveRestart,
      Kind::DaveRefusal,
      Kind::DaveLateTransition,
      Kind::SignalingOverflow,
      Kind::FallbackShare,
    ] {
      let count = self.pending.get(&kind).copied().unwrap_or(0);
      let is_due = self
        .last_reported
        .get(&kind)
        .is_none_or(|last| now.saturating_duration_since(*last) >= INTERVAL);
      if count == 0 || !is_due || self.reported >= MAX_REPORTS {
        continue;
      }
      let detail = self.detail(kind, count);
      self.pending.remove(&kind);
      if kind == Kind::DaveRestart {
        self.strikes = 0;
      }
      self.last_reported.insert(kind, now);
      self.reported += 1;
      events.push(CrashEvent {
        event_id: event_id(),
        timestamp_ms: now_ms,
        level: CrashLevel::Warning,
        source: ErrorSource::Rust,
        error_type: kind.error_type().to_owned(),
        message: kind.message().to_owned(),
        frames: Vec::new(),
        breadcrumbs: vec![detail],
        maybe_call: None,
      });
    }
    events
  }

  fn add(&mut self, kind: Kind, count: u32) {
    if count > 0 {
      let pending = self.pending.entry(kind).or_default();
      *pending = pending.saturating_add(count);
    }
  }

  /// Once enough sessions joined, a fallback share over the threshold counts as one incident.
  fn judge_fallback(&mut self, quality: &SfuCallQuality) {
    self.quic_sessions = self.quic_sessions.saturating_add(quality.quic_sessions);
    self.fallback_sessions = self.fallback_sessions.saturating_add(quality.fallback_sessions);
    let total = self.quic_sessions.saturating_add(self.fallback_sessions);
    if total < FALLBACK_MIN_SESSIONS {
      return;
    }
    if u64::from(self.fallback_sessions) * 100 >= u64::from(total) * u64::from(FALLBACK_SHARE_PERCENT) {
      self.add(Kind::FallbackShare, 1);
      self.maybe_last_fallback = Some((self.fallback_sessions, total));
    }
    self.quic_sessions = 0;
    self.fallback_sessions = 0;
  }

  fn detail(&self, kind: Kind, count: u32) -> String {
    match kind {
      Kind::DaveRestart => format!(
        "sfu: {count} DAVE restarts, {} strikes since the last report",
        self.strikes
      ),
      Kind::FallbackShare => {
        let (fallback, total) = self.maybe_last_fallback.unwrap_or_default();
        format!("sfu: {fallback} of {total} sessions joined on the fallback")
      },
      _ => format!("sfu: {count} since the last report"),
    }
  }
}

impl Kind {
  fn error_type(self) -> &'static str {
    match self {
      Self::DaveRestart => "SfuDaveRestart",
      Self::DaveRefusal => "SfuDaveRefusal",
      Self::DaveLateTransition => "SfuDaveLateTransition",
      Self::SignalingOverflow => "SfuSignalingOverflow",
      Self::FallbackShare => "SfuFallbackShare",
    }
  }

  fn message(self) -> &'static str {
    match self {
      Self::DaveRestart => "a call's DAVE group started over: no member committed in time",
      Self::DaveRefusal => "a device was refused by a call's encryption after stalling it",
      Self::DaveLateTransition => "a DAVE transition ran before every member was ready",
      Self::SignalingOverflow => "a participant was dropped: its signaling queue overflowed",
      Self::FallbackShare => "many sessions join over the WebSocket fallback (UDP blocked)",
    }
  }
}

fn random_event_id() -> EventId {
  use rand_core::RngCore;
  let mut bytes = [0; 16];
  rand_core::OsRng.fill_bytes(&mut bytes);
  EventId::from_random_bytes(bytes)
}

fn now_ms() -> i64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0, |elapsed| {
      i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
    })
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::SfuCallIncidents;

  use super::*;

  #[test]
  fn each_kind_is_reported_once_an_interval_with_the_counts_summed() {
    let mut limiter = IncidentLimiter::default();
    let start = Instant::now();
    let restart = quality(
      SfuCallIncidents {
        dave_restarts: 1,
        dave_strikes: 2,
        ..SfuCallIncidents::default()
      },
      0,
      0,
    );
    let first = limiter.take(&restart, start, 0, id);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].error_type, "SfuDaveRestart");
    assert_eq!(first[0].level, CrashLevel::Warning);
    assert_eq!(
      first[0].breadcrumbs,
      ["sfu: 1 DAVE restarts, 2 strikes since the last report"]
    );
    assert!(
      limiter
        .take(&restart, start + Duration::from_secs(60), 0, id)
        .is_empty()
    );
    assert!(
      limiter
        .take(&restart, start + Duration::from_secs(120), 0, id)
        .is_empty()
    );
    let later = limiter.take(&restart, start + INTERVAL, 0, id);
    assert_eq!(
      later[0].breadcrumbs,
      ["sfu: 3 DAVE restarts, 6 strikes since the last report"]
    );
  }

  #[test]
  fn kinds_are_limited_on_their_own() {
    let mut limiter = IncidentLimiter::default();
    let start = Instant::now();
    let all = quality(
      SfuCallIncidents {
        dave_restarts: 1,
        dave_strikes: 1,
        dave_refusals: 1,
        dave_late_transitions: 1,
        signaling_overflows: 1,
      },
      0,
      0,
    );
    let types: Vec<String> = limiter
      .take(&all, start, 0, id)
      .into_iter()
      .map(|event| event.error_type)
      .collect();
    assert_eq!(
      types,
      [
        "SfuDaveRestart",
        "SfuDaveRefusal",
        "SfuDaveLateTransition",
        "SfuSignalingOverflow"
      ]
    );
  }

  #[test]
  fn a_high_fallback_share_is_reported_once_enough_sessions_joined() {
    let mut limiter = IncidentLimiter::default();
    let start = Instant::now();
    let none = SfuCallIncidents::default();
    assert!(limiter.take(&quality(none, 5, 5), start, 0, id).is_empty());
    let events = limiter.take(&quality(none, 7, 3), start, 0, id);
    assert_eq!(events[0].error_type, "SfuFallbackShare");
    assert_eq!(
      events[0].breadcrumbs,
      ["sfu: 8 of 20 sessions joined on the fallback"]
    );
    // A low share isn't.
    let mut quiet = IncidentLimiter::default();
    assert!(quiet.take(&quality(none, 19, 1), start, 0, id).is_empty());
  }

  #[test]
  fn reports_per_process_are_capped() {
    let mut limiter = IncidentLimiter::default();
    let start = Instant::now();
    let restart = quality(
      SfuCallIncidents {
        dave_restarts: 1,
        ..SfuCallIncidents::default()
      },
      0,
      0,
    );
    let sent: usize = (0..300)
      .map(|step| limiter.take(&restart, start + INTERVAL * step, 0, id).len())
      .sum();
    assert_eq!(sent, MAX_REPORTS as usize);
  }

  #[test]
  fn events_name_no_call_device_or_address() {
    let mut limiter = IncidentLimiter::default();
    let events = limiter.take(
      &quality(
        SfuCallIncidents {
          dave_restarts: 1,
          dave_refusals: 1,
          ..SfuCallIncidents::default()
        },
        0,
        30,
      ),
      Instant::now(),
      0,
      id,
    );
    for event in events {
      let text = format!("{} {} {:?}", event.error_type, event.message, event.breadcrumbs);
      assert!(text.chars().filter(char::is_ascii_digit).count() <= 6, "{text}");
      assert!(event.maybe_call.is_none());
    }
  }

  fn quality(incidents: SfuCallIncidents, quic_sessions: u32, fallback_sessions: u32) -> SfuCallQuality {
    SfuCallQuality {
      quic_sessions,
      fallback_sessions,
      incidents,
      ..SfuCallQuality::default()
    }
  }

  fn id() -> EventId {
    EventId::from_random_bytes([0; 16])
  }
}
