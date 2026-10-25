//! The custom status (PLAN.md §4) between the view model and the wire: what is sent to
//! `PUT /api/v1/me/status`, what is shown of what the server reported, and what the workspace
//! policy allows. Server-visible by design (low sensitivity), so nothing here is encrypted.

use teamcraft_proto::{CustomStatus as CustomStatusWire, PresencePolicy, SetCustomStatusRequest, UnixMillis};

use crate::model::{CustomStatus, StatusExpiry, TimestampMs};

/// A status whose end passed by the local clock is still shown this long. The server decides when
/// a status ends and announces it; the margin keeps a device whose clock runs ahead from hiding a
/// status early, while one that is offline still stops showing it soon after.
pub const EXPIRY_LEEWAY_MS: TimestampMs = 2 * 60_000;

const MS_PER_MINUTE: TimestampMs = 60_000;
const MS_PER_DAY: TimestampMs = 86_400_000;

impl CustomStatus {
  /// The status the server reported. `None` for one without emoji and text (a derived "in a
  /// meeting" or "in a call", which the UI shows on its own).
  pub fn from_wire(wire: &CustomStatusWire) -> Option<Self> {
    let emoji = wire.maybe_emoji.clone().unwrap_or_default();
    let text = wire.maybe_text.clone().unwrap_or_default();
    if emoji.trim().is_empty() && text.trim().is_empty() {
      return None;
    }
    let maybe_expires_at = wire.maybe_expires_at.map(|at| at.0);
    Some(Self {
      emoji,
      text,
      expiry: if maybe_expires_at.is_some() {
        StatusExpiry::Today
      } else {
        StatusExpiry::Never
      },
      maybe_expires_at,
    })
  }

  /// The status as the server reports it, with the end resolved at `now` on this device's clock
  /// (what the user sees until the server confirms; [`set_request`] turns it into the request).
  /// `None` if there is nothing to set (no emoji and no text): that is a clear.
  pub fn to_wire(&self, now: TimestampMs) -> Option<CustomStatusWire> {
    let maybe_emoji = Some(self.emoji.trim().to_owned()).filter(|emoji| !emoji.is_empty());
    let maybe_text = Some(self.text.trim().to_owned()).filter(|text| !text.is_empty());
    if maybe_emoji.is_none() && maybe_text.is_none() {
      return None;
    }
    Some(CustomStatusWire {
      maybe_emoji,
      maybe_text,
      maybe_expires_at: self.resolved_expires_at(now).map(UnixMillis),
      maybe_activity: None,
    })
  }

  /// When the status ends: the time the setter resolved, or else `expiry` from `now`. Without the
  /// user's time zone "today" and "this week" end at midnight UTC (of the day, of Sunday); clients
  /// that know the zone set [`CustomStatus::maybe_expires_at`] instead.
  pub fn resolved_expires_at(&self, now: TimestampMs) -> Option<TimestampMs> {
    if let Some(at) = self.maybe_expires_at {
      return Some(at);
    }
    match self.expiry {
      StatusExpiry::Never => None,
      StatusExpiry::ThirtyMinutes => Some(now + 30 * MS_PER_MINUTE),
      StatusExpiry::OneHour => Some(now + 60 * MS_PER_MINUTE),
      StatusExpiry::Today => Some(end_of_utc_day(now)),
      StatusExpiry::ThisWeek => {
        // 1970-01-01 was a Thursday; Monday is 0.
        let weekday = (now.div_euclid(MS_PER_DAY) + 3).rem_euclid(7);
        Some(end_of_utc_day(now) + (6 - weekday) * MS_PER_DAY)
      },
    }
  }

  /// Whether the status is still shown at `now` (local clock, with [`EXPIRY_LEEWAY_MS`]).
  pub fn is_shown_at(&self, now: TimestampMs) -> bool {
    self
      .maybe_expires_at
      .is_none_or(|at| now < at.saturating_add(EXPIRY_LEEWAY_MS))
  }
}

/// The body of `PUT /api/v1/me/status` for `status` sent at `now`, both on this device's clock.
/// The end goes as a duration, which the server resolves on its own clock: a device whose clock
/// runs behind would otherwise send an end that looks past to the server and be refused for good.
/// The end itself goes along for servers that don't know durations yet. `None` if the status
/// already ended: then there is nothing left to set.
pub fn set_request(status: &CustomStatusWire, now: TimestampMs) -> Option<SetCustomStatusRequest> {
  SetCustomStatusRequest::for_status(status, UnixMillis(now))
}

/// What of a person's status the viewer sees: nothing while the workspace switched statuses off or
/// after it ended.
pub fn shown_status(
  policy: &PresencePolicy,
  maybe_status: Option<CustomStatus>,
  now: TimestampMs,
) -> Option<CustomStatus> {
  maybe_status.filter(|status| policy.statuses_enabled && status.is_shown_at(now))
}

/// Whether the policy lets the user set `status`: statuses must be on, and with custom ones off it
/// has to be one of the presets (the server compares the same way).
pub fn is_allowed(policy: &PresencePolicy, status: &CustomStatus) -> bool {
  if !policy.statuses_enabled {
    return false;
  }
  let no_empty = |emoji: &str| Some(emoji.trim().to_owned()).filter(|emoji| !emoji.is_empty());
  let emoji = no_empty(&status.emoji);
  policy.custom_status_enabled
    || policy.presets.iter().any(|preset| {
      // An empty emoji is no emoji, on both sides.
      preset.maybe_emoji.as_deref().and_then(no_empty) == emoji && preset.text.trim() == status.text.trim()
    })
}

fn end_of_utc_day(now: TimestampMs) -> TimestampMs {
  (now.div_euclid(MS_PER_DAY) + 1) * MS_PER_DAY
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::StatusPreset;

  use super::*;

  /// 2026-10-21 (a Wednesday) 12:00 UTC.
  const NOW: TimestampMs = 1_792_540_800_000 + 12 * 3_600_000;

  mod wire {
    use super::*;

    #[test]
    fn a_status_becomes_a_request_with_its_end_resolved() {
      let request = status("🌴", " Vacation ", StatusExpiry::OneHour)
        .to_wire(NOW)
        .unwrap();
      assert_eq!(request.maybe_emoji.as_deref(), Some("🌴"));
      assert_eq!(request.maybe_text.as_deref(), Some("Vacation"));
      assert_eq!(request.maybe_expires_at, Some(UnixMillis(NOW + 3_600_000)));
      assert_eq!(request.maybe_activity, None);
    }

    #[test]
    fn the_request_says_how_long_so_a_skewed_clock_does_not_matter() {
      // The device's clock runs an hour behind the server: its own end would look past there.
      let wire = status("🌴", "Vacation", StatusExpiry::ThirtyMinutes)
        .to_wire(NOW)
        .unwrap();
      let request = set_request(&wire, NOW).unwrap();
      assert_eq!(request.maybe_expires_in_seconds, Some(30 * 60));
      assert_eq!(request.maybe_expires_at, Some(UnixMillis(NOW + 30 * 60_000)));
      // Sent later from the queue, it asks for what is left of it, rounded up.
      let later = set_request(&wire, NOW + 10 * 60_000 + 500).unwrap();
      assert_eq!(later.maybe_expires_in_seconds, Some(20 * 60));
      let forever = status("🌴", "Vacation", StatusExpiry::Never)
        .to_wire(NOW)
        .unwrap();
      assert_eq!(set_request(&forever, NOW).unwrap().maybe_expires_in_seconds, None);
    }

    #[test]
    fn a_status_that_ended_before_it_was_sent_is_no_request() {
      let wire = status("🌴", "Vacation", StatusExpiry::ThirtyMinutes)
        .to_wire(NOW)
        .unwrap();
      assert_eq!(set_request(&wire, NOW + 30 * 60_000), None);
    }

    #[test]
    fn the_request_keeps_its_wire_names() {
      let wire = status("🌴", "Vacation", StatusExpiry::OneHour)
        .to_wire(NOW)
        .unwrap();
      let json = serde_json::to_value(set_request(&wire, NOW).unwrap()).unwrap();
      assert_eq!(
        json,
        serde_json::json!({
          "emoji": "🌴",
          "text": "Vacation",
          "expires_at": NOW + 3_600_000,
          "expires_in_seconds": 3600,
        })
      );
    }

    #[test]
    fn a_resolved_end_wins_over_the_choice() {
      let mut chosen = status("🌴", "Vacation", StatusExpiry::Today);
      chosen.maybe_expires_at = Some(NOW + 5);
      assert_eq!(chosen.resolved_expires_at(NOW), Some(NOW + 5));
    }

    #[test]
    fn without_a_time_zone_today_and_this_week_end_at_midnight_utc() {
      let midnight = 1_792_540_800_000 + 86_400_000;
      assert_eq!(
        status("", "x", StatusExpiry::Today).resolved_expires_at(NOW),
        Some(midnight)
      );
      // Wednesday: four more days to Monday 00:00.
      assert_eq!(
        status("", "x", StatusExpiry::ThisWeek).resolved_expires_at(NOW),
        Some(midnight + 4 * 86_400_000)
      );
      assert_eq!(
        status("", "x", StatusExpiry::Never).resolved_expires_at(NOW),
        None
      );
    }

    #[test]
    fn an_empty_status_is_a_clear() {
      assert_eq!(status(" ", "  ", StatusExpiry::Never).to_wire(NOW), None);
    }

    #[test]
    fn the_reported_status_keeps_its_end() {
      let wire = CustomStatusWire {
        maybe_emoji: Some("🤒".into()),
        maybe_text: None,
        maybe_expires_at: Some(UnixMillis(NOW + 10)),
        maybe_activity: None,
      };
      let shown = CustomStatus::from_wire(&wire).unwrap();
      assert_eq!(shown.emoji, "🤒");
      assert_eq!(shown.text, "");
      assert_eq!(shown.maybe_expires_at, Some(NOW + 10));
    }

    #[test]
    fn a_derived_status_is_not_a_custom_one() {
      let wire = CustomStatusWire {
        maybe_emoji: None,
        maybe_text: None,
        maybe_expires_at: None,
        maybe_activity: Some(teamcraft_proto::PresenceActivity::InCall),
      };
      assert_eq!(CustomStatus::from_wire(&wire), None);
    }
  }

  mod shown {
    use super::*;

    #[test]
    fn an_ended_status_goes_after_the_leeway() {
      let mut ending = status("🌴", "Vacation", StatusExpiry::Today);
      ending.maybe_expires_at = Some(NOW);
      assert!(ending.is_shown_at(NOW + EXPIRY_LEEWAY_MS - 1));
      assert!(!ending.is_shown_at(NOW + EXPIRY_LEEWAY_MS));
      let policy = PresencePolicy::default();
      assert_eq!(
        shown_status(&policy, Some(ending.clone()), NOW + EXPIRY_LEEWAY_MS),
        None
      );
      assert_eq!(shown_status(&policy, Some(ending.clone()), NOW), Some(ending));
    }

    #[test]
    fn nothing_shows_while_the_workspace_switched_statuses_off() {
      let policy = PresencePolicy {
        statuses_enabled: false,
        ..PresencePolicy::default()
      };
      assert_eq!(
        shown_status(&policy, Some(status("🌴", "Vacation", StatusExpiry::Never)), NOW),
        None
      );
    }
  }

  mod allowed {
    use super::*;

    #[test]
    fn with_custom_statuses_off_only_presets_are_allowed() {
      let policy = PresencePolicy {
        custom_status_enabled: false,
        presets: vec![StatusPreset {
          maybe_emoji: Some("🤒".into()),
          text: "Sick".into(),
          maybe_duration_minutes: None,
        }],
        ..PresencePolicy::default()
      };
      assert!(is_allowed(&policy, &status("🤒", " Sick", StatusExpiry::Today)));
      let without_emoji = PresencePolicy {
        presets: vec![StatusPreset {
          maybe_emoji: Some(" ".into()),
          text: "Lunch".into(),
          maybe_duration_minutes: None,
        }],
        ..policy.clone()
      };
      assert!(is_allowed(
        &without_emoji,
        &status("", "Lunch", StatusExpiry::Today)
      ));
      assert!(!is_allowed(
        &without_emoji,
        &status("🍕", "Lunch", StatusExpiry::Today)
      ));
      assert!(!is_allowed(
        &policy,
        &status("🌴", "Vacation", StatusExpiry::Today)
      ));
      assert!(is_allowed(
        &PresencePolicy::default(),
        &status("🌴", "Vacation", StatusExpiry::Today)
      ));
    }

    #[test]
    fn nothing_is_allowed_while_statuses_are_off() {
      let policy = PresencePolicy {
        statuses_enabled: false,
        ..PresencePolicy::default()
      };
      assert!(!is_allowed(
        &policy,
        &status("🌴", "Vacation", StatusExpiry::Today)
      ));
    }
  }

  fn status(emoji: &str, text: &str, expiry: StatusExpiry) -> CustomStatus {
    CustomStatus {
      emoji: emoji.to_owned(),
      text: text.to_owned(),
      expiry,
      maybe_expires_at: None,
    }
  }
}
