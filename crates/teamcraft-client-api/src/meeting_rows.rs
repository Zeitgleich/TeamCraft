//! What a meeting's row offers and which meetings the Past view lists (docs/meetings.md), decided
//! alike by desktop and web (`teamcraft-ui`'s meetings page) and the phone apps (teamcraft-ffi).

use teamcraft_proto::{MeetingResponse, MeetingStatus};

use crate::meeting_edits::is_over;
use crate::model::{TimestampMs, UserId};

/// How far back the Past view reaches (#956).
pub const PAST_LOOKBACK_MS: i64 = 30 * 24 * 60 * 60_000;
/// The server's own maximum: a month of daily series still fits.
pub const PAST_LIST_LIMIT: u32 = 500;

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
}

/// What [`MeetingActions`] are decided from: the facts of a row as an app has them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowFacts {
  pub is_organizer: bool,
  pub is_cancelled: bool,
  /// Only this occurrence of a series is cancelled (or left out as a holiday).
  pub is_occurrence_cancelled: bool,
  /// Ended or cancelled ([`is_over`]).
  pub is_over: bool,
  pub ends_at: TimestampMs,
}

impl MeetingActions {
  pub fn of(meeting: &MeetingResponse, me: UserId, now: TimestampMs) -> Self {
    Self::from_facts(
      RowFacts {
        is_organizer: meeting.organizer_user_id == me,
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
      }
    );
    let invited = MeetingActions::of(&upcoming, id(2), NOW);
    assert!(invited.can_copy_link && !invited.can_change);
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
