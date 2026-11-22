//! What a meeting's row offers and which meetings the Past view lists (docs/meetings.md), decided
//! alike by desktop and web (`teamcraft-ui`'s meetings page) and the phone apps (teamcraft-ffi).

use teamcraft_proto::{MeetingResponse, MeetingStatus};

use crate::meeting_edits::is_over;
use crate::model::{TimestampMs, UserId};

/// How far back the Past view reaches (#956).
pub const PAST_LOOKBACK_MS: i64 = 30 * 24 * 60 * 60_000;
/// The server's own maximum: a month of daily series still fits.
pub const PAST_LIST_LIMIT: u32 = 500;
/// Join is the main action from this long before a meeting starts (dialling in early is normal).
/// From then on its hosts see its lobby on the meeting list too.
pub const JOIN_LEAD_MS: i64 = 15 * 60_000;

/// The actions of a meeting's row for one user at one time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MeetingActions {
  /// "Copy link": while the meeting is still to come or runs.
  pub can_copy_link: bool,
  /// "Edit" and "Cancel": its organizer, while it is still to come or runs.
  pub can_change: bool,
  /// Take a cancellation back (of the meeting, or of this occurrence; a holiday's too): its
  /// organizer, while what was cancelled is still to come.
  pub can_restore: bool,
  /// "Remove" a cancelled meeting from the lists: its organizer, for a whole cancelled meeting.
  pub can_hide: bool,
  /// "Invite guests" (a one-time guest link, the organizer's code): its organizer and co-hosts,
  /// while it is still to come or runs.
  pub can_invite_guests: bool,
  /// "New meeting link" (the old one stops working): its organizer, while it is still to come or
  /// runs. The server refuses everybody else.
  pub can_renew_link: bool,
}

/// What [`MeetingActions`] are decided from: the facts of a row as an app has them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowFacts {
  pub is_organizer: bool,
  /// The organizer made the user a co-host (#1720).
  pub is_co_host: bool,
  pub is_cancelled: bool,
  /// Only this occurrence of a series is cancelled (or left out as a holiday).
  pub is_occurrence_cancelled: bool,
  /// Ended or cancelled ([`is_over`]).
  pub is_over: bool,
  pub ends_at: TimestampMs,
}

/// Whether Join is the meeting's main action at `now`: from [`JOIN_LEAD_MS`] before its start
/// until it is over.
pub fn is_joinable(meeting: &MeetingResponse, now: TimestampMs) -> bool {
  !is_over(meeting, now) && meeting.starts_at.0 - JOIN_LEAD_MS <= now
}

impl MeetingActions {
  pub fn of(meeting: &MeetingResponse, me: UserId, now: TimestampMs) -> Self {
    Self::from_facts(
      RowFacts {
        is_organizer: meeting.organizer_user_id == me,
        is_co_host: meeting.co_host_user_ids.contains(&me),
        is_cancelled: meeting.status == MeetingStatus::Cancelled,
        is_occurrence_cancelled: meeting.is_occurrence_cancelled,
        is_over: is_over(meeting, now),
        ends_at: meeting.ends_at.0,
      },
      now,
    )
  }

  pub fn from_facts(facts: RowFacts, now: TimestampMs) -> Self {
    Self {
      can_copy_link: !facts.is_over,
      can_change: facts.is_organizer && !facts.is_over,
      can_restore: facts.is_organizer && facts.is_cancelled && facts.ends_at > now,
      can_hide: facts.is_organizer && facts.is_cancelled && !facts.is_occurrence_cancelled,
      can_invite_guests: (facts.is_organizer || facts.is_co_host) && !facts.is_over,
      can_renew_link: facts.is_organizer && !facts.is_over,
    }
  }
}

/// The meetings of the Past view among `meetings` (the past list asks up to now, so those still
/// running are in it too): those that ended, latest first.
pub fn past_meetings(meetings: &[MeetingResponse], now: TimestampMs) -> Vec<MeetingResponse> {
  let mut past: Vec<MeetingResponse> = meetings
    .iter()
    .filter(|meeting| meeting.ends_at.0 <= now)
    .cloned()
    .collect();
  past.sort_by_key(|meeting| std::cmp::Reverse((meeting.starts_at.0, meeting.id)));
  past
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, MeetingKind, Snowflake, UnixMillis};

  use super::*;

  const NOW: TimestampMs = 1_800_000_000_000;
  const HOUR: i64 = 3_600_000;

  #[test]
  fn only_the_organizer_changes_restores_and_removes() {
    let organizer = id(1);
    let upcoming = meeting(organizer, NOW + HOUR);
    assert_eq!(
      MeetingActions::of(&upcoming, organizer, NOW),
      MeetingActions {
        can_copy_link: true,
        can_change: true,
        can_restore: false,
        can_hide: false,
        can_invite_guests: true,
        can_renew_link: true,
      }
    );
    let invited = MeetingActions::of(&upcoming, id(2), NOW);
    assert!(invited.can_copy_link && !invited.can_change);
    assert!(!invited.can_invite_guests && !invited.can_renew_link);
    let mut cancelled = upcoming.clone();
    cancelled.status = MeetingStatus::Cancelled;
    let actions = MeetingActions::of(&cancelled, organizer, NOW);
    assert!(!actions.can_copy_link && !actions.can_change);
    assert!(actions.can_restore && actions.can_hide);
    assert_eq!(
      MeetingActions::of(&cancelled, id(2), NOW),
      MeetingActions::default()
    );
  }

  #[test]
  fn a_cancelled_occurrence_is_restored_but_not_removed_and_nothing_past_comes_back() {
    let organizer = id(1);
    let mut occurrence = meeting(organizer, NOW + HOUR);
    occurrence.status = MeetingStatus::Cancelled;
    occurrence.is_occurrence_cancelled = true;
    let actions = MeetingActions::of(&occurrence, organizer, NOW);
    assert!(actions.can_restore && !actions.can_hide);
    let mut gone = meeting(organizer, NOW - 2 * HOUR);
    gone.status = MeetingStatus::Cancelled;
    assert!(!MeetingActions::of(&gone, organizer, NOW).can_restore);
  }

  #[test]
  fn the_past_view_lists_what_ended_latest_first() {
    let organizer = id(1);
    let running = meeting(organizer, NOW - HOUR / 2);
    let earlier = meeting(organizer, NOW - 5 * HOUR);
    let later = meeting(organizer, NOW - 2 * HOUR);
    let past = past_meetings(&[earlier.clone(), running, later.clone()], NOW);
    assert_eq!(past, vec![later, earlier]);
  }

  #[test]
  fn co_hosts_invite_guests_but_only_the_organizer_renews_the_link() {
    let mut upcoming = meeting(id(1), NOW + HOUR);
    upcoming.co_host_user_ids = vec![id(2)];
    let co_host = MeetingActions::of(&upcoming, id(2), NOW);
    assert!(co_host.can_invite_guests);
    assert!(!co_host.can_renew_link && !co_host.can_change);
    let over = MeetingActions::of(&upcoming, id(2), NOW + 2 * HOUR);
    assert!(
      !over.can_invite_guests,
      "nobody invites to a meeting that is over"
    );
    let mut cancelled = upcoming;
    cancelled.status = MeetingStatus::Cancelled;
    let organizer = MeetingActions::of(&cancelled, id(1), NOW);
    assert!(!organizer.can_invite_guests && !organizer.can_renew_link);
  }

  #[test]
  fn join_leads_a_quarter_hour_early_until_the_end() {
    let upcoming = meeting(id(1), NOW + JOIN_LEAD_MS);
    assert!(is_joinable(&upcoming, NOW));
    assert!(!is_joinable(&upcoming, NOW - 1));
    assert!(!is_joinable(&upcoming, NOW + JOIN_LEAD_MS + HOUR), "over");
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }

  /// A meeting of `organizer` that starts at `starts_at` and lasts an hour.
  fn meeting(organizer: UserId, starts_at: TimestampMs) -> MeetingResponse {
    MeetingResponse {
      id: id(7),
      title: "Planning".into(),
      maybe_description: None,
      starts_at: UnixMillis(starts_at),
      ends_at: UnixMillis(starts_at + HOUR),
      kind: MeetingKind::Scheduled,
      status: MeetingStatus::Scheduled,
      has_lobby: true,
      organizer_user_id: organizer,
      conversation_id: id(9),
      maybe_space: None,
      join_url: String::new(),
      maybe_recurrence: None,
      is_occurrence_cancelled: false,
      is_occurrence_holiday: false,
      can_host_lobby: false,
      co_host_user_ids: Vec::new(),
      attendees: Vec::new(),
      undelivered_invitations: Vec::new(),
    }
  }
}
