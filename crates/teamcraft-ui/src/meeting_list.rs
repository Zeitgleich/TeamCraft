//! What the meetings page shows and in which order (#956): the views, the "now / next" meeting,
//! the invitations still waiting for an answer, the day groups and each meeting's state. Pure
//! functions over the server's list, so they are tested without a browser.

use std::collections::HashSet;

use teamcraft_proto::{MeetingId, MeetingResponse, MeetingStatus, RecurrenceFrequency, RsvpStatus};

use crate::clock::local_day;
use crate::meetings::{is_over, my_rsvp};
use crate::model::{TimestampMs, UserId};

/// Join is the main action from this long before a meeting starts (dialling in early is normal).
pub const JOIN_LEAD_MS: i64 = 15 * 60_000;
/// A meeting this close to its start says "in 12 min" instead of its time.
pub const SOON_MS: i64 = 60 * 60_000;
/// Rows rendered at first, and added by each "Show more" (robustness.md, "Large data").
pub const PAGE_ROWS: usize = 30;

/// The views of the meetings page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MeetingsTab {
  /// Everything still to come, the user's and others'.
  Upcoming,
  /// Meetings the user organizes or co-hosts.
  Hosted,
  /// Meetings somebody else organizes that the user is invited to.
  Invited,
  /// Meetings that ended, newest first.
  Past,
}

/// A meeting's state at a glance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeetingState {
  /// Cancelled, or left out for a public holiday.
  Cancelled {
    is_holiday: bool,
  },
  Ended,
  Running,
  /// Starts within [`SOON_MS`]; minutes until the start, at least 1.
  StartsSoon {
    minutes: i64,
  },
  /// Later than [`SOON_MS`].
  Later,
}

/// The invitations of one meeting the user hasn't answered.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingAnswer {
  /// The first occurrence still to come.
  pub meeting: MeetingResponse,
  /// More occurrences of the same series in the list (one answer covers the meeting).
  pub more_occurrences: usize,
}

/// Meetings of one local day, for the day headings.
#[derive(Clone, Debug, PartialEq)]
pub struct MeetingDay {
  pub day: i64,
  pub first_start: TimestampMs,
  pub meetings: Vec<MeetingResponse>,
}

/// The day heading of a day: relative where it helps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DayLabel {
  Yesterday,
  Today,
  Tomorrow,
  /// Within the coming week: the weekday name is enough.
  Weekday,
  /// Further away: weekday and date.
  Date,
}

/// How a series repeats, for its one-line summary ("Every week", "Every 2 weeks").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeriesSummary {
  pub frequency: RecurrenceFrequency,
  pub interval: u32,
  /// Monday to Friday of a daily series.
  pub is_workdays: bool,
}

impl MeetingsTab {
  pub const ALL: [MeetingsTab; 4] = [Self::Upcoming, Self::Hosted, Self::Invited, Self::Past];

  pub fn label_key(self) -> &'static str {
    match self {
      Self::Upcoming => "meetings-tab-upcoming",
      Self::Hosted => "meetings-tab-hosted",
      Self::Invited => "meetings-tab-invited",
      Self::Past => "meetings-tab-past",
    }
  }

  pub fn empty_title_key(self) -> &'static str {
    match self {
      Self::Upcoming => "meetings-empty-title",
      Self::Hosted => "meetings-hosted-empty-title",
      Self::Invited => "meetings-invited-empty-title",
      Self::Past => "meetings-past-empty-title",
    }
  }

  pub fn empty_body_key(self, is_in_space: bool) -> &'static str {
    match (self, is_in_space) {
      (Self::Upcoming, true) => "meetings-space-empty",
      (Self::Upcoming, false) => "meetings-empty",
      (Self::Hosted, _) => "meetings-hosted-empty",
      (Self::Invited, _) => "meetings-invited-empty",
      (Self::Past, _) => "meetings-past-empty",
    }
  }

  /// The tab's meetings among `meetings` (the upcoming list, or the past one for [`Self::Past`]),
  /// in the order they are shown.
  pub fn select(self, meetings: &[MeetingResponse], me: UserId, now: TimestampMs) -> Vec<MeetingResponse> {
    let is_hosted = |meeting: &MeetingResponse| is_hosted_by(meeting, me);
    let mut selected: Vec<MeetingResponse> = meetings
      .iter()
      .filter(|meeting| match self {
        // A cancelled meeting stays until it would have ended, so it can be restored.
        Self::Upcoming => meeting.ends_at.0 > now,
        Self::Hosted => meeting.ends_at.0 > now && is_hosted(meeting),
        Self::Invited => meeting.ends_at.0 > now && meeting.organizer_user_id != me,
        Self::Past => meeting.ends_at.0 <= now,
      })
      .cloned()
      .collect();
    match self {
      Self::Past => selected.sort_by_key(|meeting| std::cmp::Reverse((meeting.starts_at.0, meeting.id))),
      _ => selected.sort_by_key(|meeting| (meeting.starts_at.0, meeting.id)),
    }
    selected
  }
}

/// Whether `me` organizes or co-hosts the meeting.
pub fn is_hosted_by(meeting: &MeetingResponse, me: UserId) -> bool {
  meeting.organizer_user_id == me || meeting.co_host_user_ids.contains(&me)
}

/// The state of a meeting at `now` (the server's clock, so a wrong local clock doesn't say
/// "in 3 hours" for a meeting that runs).
pub fn meeting_state(meeting: &MeetingResponse, now: TimestampMs) -> MeetingState {
  if meeting.status == MeetingStatus::Cancelled {
    return MeetingState::Cancelled {
      is_holiday: meeting.is_occurrence_holiday,
    };
  }
  if is_over(meeting, now) {
    return MeetingState::Ended;
  }
  let until_start = meeting.starts_at.0 - now;
  match until_start {
    ..=0 => MeetingState::Running,
    1..SOON_MS => MeetingState::StartsSoon {
      minutes: ((until_start + 59_999) / 60_000).max(1),
    },
    _ => MeetingState::Later,
  }
}

/// Whether Join is the meeting's main action at `now`.
pub fn is_joinable(meeting: &MeetingResponse, now: TimestampMs) -> bool {
  !is_over(meeting, now) && meeting.starts_at.0 - JOIN_LEAD_MS <= now
}

/// The meeting for the "now / next" area: a running one (the latest started first), else the next
/// one still to come. Cancelled meetings never.
pub fn now_or_next(meetings: &[MeetingResponse], now: TimestampMs) -> Option<MeetingResponse> {
  let live = |meeting: &&MeetingResponse| !is_over(meeting, now);
  let running = meetings
    .iter()
    .filter(live)
    .filter(|meeting| meeting.starts_at.0 <= now)
    .max_by_key(|meeting| meeting.starts_at.0);
  running
    .or_else(|| {
      meetings
        .iter()
        .filter(live)
        .min_by_key(|meeting| meeting.starts_at.0)
    })
    .cloned()
}

/// The meetings of somebody else whose invitation `me` hasn't answered, once per meeting: a
/// series is one invitation, however many occurrences the list holds.
pub fn pending_answers(meetings: &[MeetingResponse], me: UserId, now: TimestampMs) -> Vec<PendingAnswer> {
  let mut answers: Vec<PendingAnswer> = Vec::new();
  let mut seen: HashSet<MeetingId> = HashSet::new();
  for meeting in meetings.iter().filter(|meeting| {
    !is_over(meeting, now)
      && meeting.organizer_user_id != me
      && my_rsvp(meeting, me) == Some(RsvpStatus::NeedsAction)
  }) {
    if seen.insert(meeting.id) {
      answers.push(PendingAnswer {
        meeting: meeting.clone(),
        more_occurrences: 0,
      });
    } else if let Some(answer) = answers.iter_mut().find(|answer| answer.meeting.id == meeting.id) {
      answer.more_occurrences += 1;
    }
  }
  answers
}

/// The first `rows` meetings grouped by local day, in the order given.
pub fn meeting_days(meetings: &[MeetingResponse], rows: usize) -> Vec<MeetingDay> {
  let mut days: Vec<MeetingDay> = Vec::new();
  for meeting in meetings.iter().take(rows) {
    let day = local_day(meeting.starts_at.0);
    match days.last_mut().filter(|last| last.day == day) {
      Some(last) => last.meetings.push(meeting.clone()),
      None => days.push(MeetingDay {
        day,
        first_start: meeting.starts_at.0,
        meetings: vec![meeting.clone()],
      }),
    }
  }
  days
}

/// The heading of `day` seen from `today` (both local day numbers).
pub fn day_label(day: i64, today: i64) -> DayLabel {
  match day - today {
    -1 => DayLabel::Yesterday,
    0 => DayLabel::Today,
    1 => DayLabel::Tomorrow,
    2..=6 => DayLabel::Weekday,
    _ => DayLabel::Date,
  }
}

/// A meeting's length in whole minutes.
pub fn duration_minutes(meeting: &MeetingResponse) -> i64 {
  ((meeting.ends_at.0 - meeting.starts_at.0) / 60_000).max(0)
}

/// The series summary of a repeating meeting.
pub fn series_summary(meeting: &MeetingResponse) -> Option<SeriesSummary> {
  use teamcraft_proto::Weekday;
  const WORKDAYS: [Weekday; 5] = [
    Weekday::Monday,
    Weekday::Tuesday,
    Weekday::Wednesday,
    Weekday::Thursday,
    Weekday::Friday,
  ];
  meeting.maybe_recurrence.as_ref().map(|recurrence| {
    let days: HashSet<&Weekday> = recurrence.weekdays.iter().collect();
    SeriesSummary {
      frequency: recurrence.frequency.clone(),
      interval: recurrence.interval.max(1),
      is_workdays: days.len() == WORKDAYS.len() && WORKDAYS.iter().all(|day| days.contains(day)),
    }
  })
}

impl SeriesSummary {
  /// The Fluent key; `$count` is [`Self::interval`].
  pub fn key(&self) -> &'static str {
    match (&self.frequency, self.is_workdays) {
      (RecurrenceFrequency::Daily, true) => "meetings-series-workdays",
      (RecurrenceFrequency::Daily, false) => "meetings-series-daily",
      (RecurrenceFrequency::Weekly, _) => "meetings-series-weekly",
      (RecurrenceFrequency::Monthly, _) => "meetings-series-monthly",
      (RecurrenceFrequency::Unknown(_), _) => "meetings-recurring",
    }
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{MeetingAttendeeResponse, MeetingKind, MeetingRecurrence, UnixMillis, Weekday};

  use super::*;
  use crate::model::id_from_raw;

  const NOW: i64 = 1_800_000_000_000;
  const MINUTE: i64 = 60_000;

  mod tabs {
    use super::*;

    #[test]
    fn upcoming_keeps_cancelled_until_they_would_have_ended() {
      let mut cancelled = meeting(1, other(), NOW + 60 * MINUTE);
      cancelled.status = MeetingStatus::Cancelled;
      let ended = meeting(2, other(), NOW - 90 * MINUTE);
      let shown = MeetingsTab::Upcoming.select(&[ended, cancelled], me(), NOW);
      assert_eq!(ids(&shown), raw(&[1]));
    }

    #[test]
    fn hosted_and_invited_split_by_organizer_and_co_host() {
      let mine = meeting(1, me(), NOW + MINUTE);
      let mut co_hosted = meeting(2, other(), NOW + 2 * MINUTE);
      co_hosted.co_host_user_ids = vec![me()];
      let theirs = meeting(3, other(), NOW + 3 * MINUTE);
      let all = [mine, co_hosted, theirs];
      assert_eq!(ids(&MeetingsTab::Hosted.select(&all, me(), NOW)), raw(&[1, 2]));
      assert_eq!(ids(&MeetingsTab::Invited.select(&all, me(), NOW)), raw(&[2, 3]));
    }

    #[test]
    fn past_is_newest_first() {
      let older = meeting(1, me(), NOW - 300 * MINUTE);
      let newer = meeting(2, me(), NOW - 100 * MINUTE);
      let running = meeting(3, me(), NOW - 10 * MINUTE);
      let shown = MeetingsTab::Past.select(&[older, newer, running], me(), NOW);
      assert_eq!(ids(&shown), raw(&[2, 1]));
    }
  }

  mod state {
    use super::*;

    #[test]
    fn follows_the_clock() {
      let meeting = meeting(1, me(), NOW + 12 * MINUTE + 1);
      assert_eq!(
        meeting_state(&meeting, NOW),
        MeetingState::StartsSoon { minutes: 13 }
      );
      assert_eq!(meeting_state(&meeting, NOW - 2 * SOON_MS), MeetingState::Later);
      assert_eq!(meeting_state(&meeting, NOW + 20 * MINUTE), MeetingState::Running);
      assert_eq!(meeting_state(&meeting, NOW + 120 * MINUTE), MeetingState::Ended);
    }

    #[test]
    fn cancelled_says_whether_a_holiday_left_it_out() {
      let mut holiday = meeting(1, me(), NOW + MINUTE);
      holiday.status = MeetingStatus::Cancelled;
      holiday.is_occurrence_holiday = true;
      assert_eq!(
        meeting_state(&holiday, NOW),
        MeetingState::Cancelled { is_holiday: true }
      );
    }

    #[test]
    fn join_leads_a_quarter_hour_early() {
      let meeting = meeting(1, me(), NOW + JOIN_LEAD_MS);
      assert!(is_joinable(&meeting, NOW));
      assert!(!is_joinable(&meeting, NOW - 1));
    }
  }

  mod now_next {
    use super::*;

    #[test]
    fn prefers_the_latest_running_meeting() {
      let early = meeting(1, me(), NOW - 30 * MINUTE);
      let late = meeting(2, me(), NOW - 5 * MINUTE);
      let next = meeting(3, me(), NOW + 5 * MINUTE);
      let picked = now_or_next(&[early, late, next], NOW);
      assert_eq!(
        picked.map(|meeting| meeting.id),
        Some(id_from_raw::<MeetingId>(2))
      );
    }

    #[test]
    fn skips_cancelled_and_ended() {
      let mut cancelled = meeting(1, me(), NOW + MINUTE);
      cancelled.status = MeetingStatus::Cancelled;
      let ended = meeting(2, me(), NOW - 120 * MINUTE);
      let next = meeting(3, me(), NOW + 90 * MINUTE);
      let picked = now_or_next(&[ended, cancelled, next], NOW);
      assert_eq!(
        picked.map(|meeting| meeting.id),
        Some(id_from_raw::<MeetingId>(3))
      );
      assert_eq!(now_or_next(&[], NOW), None);
    }
  }

  mod answers {
    use super::*;

    #[test]
    fn a_series_is_one_invitation() {
      let first = invited(meeting(1, other(), NOW + MINUTE), RsvpStatus::NeedsAction);
      let second = invited(
        meeting(1, other(), NOW + 7 * 24 * 60 * MINUTE),
        RsvpStatus::NeedsAction,
      );
      let answers = pending_answers(&[first, second], me(), NOW);
      assert_eq!(answers.len(), 1);
      assert_eq!(answers[0].more_occurrences, 1);
      assert_eq!(answers[0].meeting.starts_at.0, NOW + MINUTE);
    }

    #[test]
    fn leaves_out_answered_own_and_over() {
      let answered = invited(meeting(1, other(), NOW + MINUTE), RsvpStatus::Accepted);
      let own = invited(meeting(2, me(), NOW + MINUTE), RsvpStatus::NeedsAction);
      let over = invited(meeting(3, other(), NOW - 120 * MINUTE), RsvpStatus::NeedsAction);
      assert!(pending_answers(&[answered, own, over], me(), NOW).is_empty());
    }
  }

  mod days {
    use super::*;

    #[test]
    fn groups_by_day_and_stops_at_the_page() {
      let day = 24 * 60 * MINUTE;
      let list = [
        meeting(1, me(), NOW),
        meeting(2, me(), NOW + 1),
        meeting(3, me(), NOW + 2 * day),
      ];
      let all = meeting_days(&list, PAGE_ROWS);
      assert_eq!(all.len(), 2);
      assert_eq!(all[0].meetings.len(), 2);
      let paged = meeting_days(&list, 1);
      assert_eq!(paged.len(), 1);
      assert_eq!(paged[0].meetings.len(), 1);
    }

    #[test]
    fn labels_are_relative_nearby() {
      assert_eq!(day_label(9, 10), DayLabel::Yesterday);
      assert_eq!(day_label(10, 10), DayLabel::Today);
      assert_eq!(day_label(11, 10), DayLabel::Tomorrow);
      assert_eq!(day_label(16, 10), DayLabel::Weekday);
      assert_eq!(day_label(17, 10), DayLabel::Date);
      assert_eq!(day_label(3, 10), DayLabel::Date);
    }
  }

  mod series {
    use super::*;

    #[test]
    fn workdays_are_named() {
      let mut daily = meeting(1, me(), NOW);
      daily.maybe_recurrence = Some(recurrence(
        RecurrenceFrequency::Daily,
        vec![
          Weekday::Friday,
          Weekday::Monday,
          Weekday::Tuesday,
          Weekday::Wednesday,
          Weekday::Thursday,
        ],
      ));
      assert_eq!(
        series_summary(&daily).as_ref().map(SeriesSummary::key),
        Some("meetings-series-workdays")
      );
      let mut weekly = meeting(2, me(), NOW);
      weekly.maybe_recurrence = Some(recurrence(RecurrenceFrequency::Weekly, Vec::new()));
      assert_eq!(
        series_summary(&weekly).as_ref().map(SeriesSummary::key),
        Some("meetings-series-weekly")
      );
      assert_eq!(series_summary(&meeting(3, me(), NOW)), None);
    }
  }

  fn meeting(id: u64, organizer: UserId, starts_at: i64) -> MeetingResponse {
    MeetingResponse {
      id: id_from_raw(id),
      title: format!("Meeting {id}"),
      maybe_description: None,
      starts_at: UnixMillis(starts_at),
      ends_at: UnixMillis(starts_at + 60 * MINUTE),
      kind: MeetingKind::Scheduled,
      status: MeetingStatus::Scheduled,
      has_lobby: false,
      organizer_user_id: organizer,
      conversation_id: id_from_raw(100 + id),
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

  fn invited(mut meeting: MeetingResponse, rsvp: RsvpStatus) -> MeetingResponse {
    meeting.attendees.push(MeetingAttendeeResponse {
      maybe_user_id: Some(me()),
      maybe_email: None,
      maybe_name: None,
      rsvp,
    });
    meeting
  }

  fn recurrence(frequency: RecurrenceFrequency, weekdays: Vec<Weekday>) -> MeetingRecurrence {
    MeetingRecurrence {
      frequency,
      interval: 1,
      weekdays,
      maybe_count: None,
      maybe_until: None,
      skips_holidays: false,
    }
  }

  fn ids(meetings: &[MeetingResponse]) -> Vec<MeetingId> {
    meetings.iter().map(|meeting| meeting.id).collect()
  }

  fn raw(ids: &[u64]) -> Vec<MeetingId> {
    ids.iter().map(|id| id_from_raw(*id)).collect()
  }

  fn me() -> UserId {
    id_from_raw(1)
  }

  fn other() -> UserId {
    id_from_raw(2)
  }
}
