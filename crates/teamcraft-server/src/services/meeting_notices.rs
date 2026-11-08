//! Reminders before a planned meeting and the notice at its start (docs/meetings.md, "Starting"), for
//! phones and browsers without the app in front. A time-based push: it comes whether or not anybody
//! started the call, and rings nothing.
//!
//! Nothing is planned ahead. Every pass works out what is due from the meetings as they are now, so
//! a meeting that moved or was cancelled can't fire, and takes each notice in the database
//! (`meeting_notices`), so a restart or a second instance doesn't send one twice. A notice that was
//! taken and then failed is left to the push retries; a process that dies in between loses that one
//! notice rather than risking a double.

use std::collections::HashSet;

use teamcraft_db::{MeetingNoticeKind, MeetingNoticeRecipient, StoredMeeting};
use teamcraft_proto::{
  DEFAULT_MEETING_REMINDER_MINUTES, MAX_MEETING_REMINDER_MINUTES, MeetingStatus, PushKind, PushMeeting,
  UnixMillis, UserId,
};
use time::OffsetDateTime;
use tracing::warn;

use super::convert::{offset_date_time, unix_millis};
use super::meetings::occurrence_starts;
use crate::error::ApiError;
use crate::push::routing::{self, MeetingNotice};
use crate::state::AppState;

const MS_PER_MINUTE: i64 = 60_000;
/// How far ahead a pass looks: the longest lead time a reminder can have.
const LOOK_AHEAD_MS: i64 = MAX_MEETING_REMINDER_MINUTES as i64 * MS_PER_MINUTE;
/// A start notice still goes out this long after the start (the server was down at the start, as
/// the desktop's own reminders do it for a device that slept), never later.
const START_GRACE_MS: i64 = 10 * MS_PER_MINUTE;
/// Notices are remembered this long after their occurrence started; a repeat needs the same start.
const KEEP_NOTICES_MS: i64 = 7 * 24 * 60 * MS_PER_MINUTE;

/// Sends every notice that is due at `now` and returns how many went out. One meeting failing
/// doesn't hold up the others.
pub async fn run_due(state: &AppState, now: OffsetDateTime) -> Result<usize, ApiError> {
  if state.maybe_meetings.is_none() {
    return Ok(0);
  }
  let now_ms = unix_millis(now).0;
  let (from_ms, to_ms) = (now_ms - START_GRACE_MS, now_ms + LOOK_AHEAD_MS);
  let meetings = state
    .database
    .meetings_for_notices(at(from_ms)?, at(to_ms)?)
    .await?;
  let mut sent = 0;
  for meeting in &meetings {
    match notify_meeting(state, meeting, from_ms, to_ms, now_ms).await {
      Ok(count) => sent += count,
      Err(error) => warn!(%error, meeting_id = %meeting.id, "sending the meeting's notices failed"),
    }
  }
  state
    .database
    .delete_old_meeting_notices(at(now_ms - KEEP_NOTICES_MS)?)
    .await?;
  Ok(sent)
}

/// Whether the occurrence a queued notice tells about still is on: the meeting isn't cancelled and
/// still has an occurrence at that start (it wasn't moved, and that one wasn't cancelled).
pub async fn is_occurrence_on(state: &AppState, notice: PushMeeting) -> Result<bool, ApiError> {
  let Some(meeting) = state.database.get_meeting(notice.meeting_id).await? else {
    return Ok(false);
  };
  if meeting.status != MeetingStatus::Scheduled.as_str() {
    return Ok(false);
  }
  let start = notice.starts_at.0;
  let starts = occurrence_starts(state, &meeting, start, start.saturating_add(1)).await?;
  Ok(starts.contains(&start))
}

async fn notify_meeting(
  state: &AppState,
  meeting: &StoredMeeting,
  from_ms: i64,
  to_ms: i64,
  now_ms: i64,
) -> Result<usize, ApiError> {
  let starts = occurrence_starts(state, meeting, from_ms, to_ms).await?;
  if starts.is_empty() {
    return Ok(0);
  }
  let recipients = state.database.meeting_notice_recipients(meeting.id).await?;
  let duration_ms = unix_millis(meeting.ends_at).0 - unix_millis(meeting.starts_at).0;
  let mut sent = 0;
  for starts_at_ms in starts {
    let occurrence = Occurrence {
      starts_at_ms,
      ends_at_ms: starts_at_ms.saturating_add(duration_ms),
    };
    for kind in [MeetingNoticeKind::Reminder, MeetingNoticeKind::Start] {
      let due: Vec<UserId> = recipients
        .iter()
        .filter(|recipient| due_notice(occurrence, now_ms, recipient) == Some(kind))
        .map(|recipient| recipient.user)
        .collect();
      if due.is_empty() {
        continue;
      }
      let claimed = state
        .database
        .claim_meeting_notices(meeting.id, at(starts_at_ms)?, kind, &due)
        .await?;
      if claimed.is_empty() {
        continue;
      }
      sent += claimed.len();
      let notice = MeetingNotice {
        conversation: meeting.conversation,
        organizer: meeting.organizer,
        kind: match kind {
          MeetingNoticeKind::Reminder => PushKind::MeetingReminder,
          MeetingNoticeKind::Start => PushKind::MeetingStart,
        },
        meeting: PushMeeting {
          meeting_id: meeting.id,
          starts_at: UnixMillis(starts_at_ms),
        },
        users: claimed.into_iter().collect::<HashSet<_>>(),
      };
      routing::notify_meeting(state, &notice, OffsetDateTime::now_utc()).await?;
    }
  }
  Ok(sent)
}

/// `ms` as a time for the database. Only the far future or past is out of its range.
fn at(ms: i64) -> Result<OffsetDateTime, ApiError> {
  offset_date_time(UnixMillis(ms)).ok_or_else(|| ApiError::Internal("time out of range".into()))
}

/// One occurrence of a meeting, in Unix milliseconds.
#[derive(Debug, Clone, Copy)]
struct Occurrence {
  starts_at_ms: i64,
  ends_at_ms: i64,
}

/// Which notice is due for `recipient` at `now_ms`, if one is. A device that was offline when the
/// time came still gets the start notice for a while, but only one of the two: the reminder ends
/// when the meeting starts.
fn due_notice(
  occurrence: Occurrence,
  now_ms: i64,
  recipient: &MeetingNoticeRecipient,
) -> Option<MeetingNoticeKind> {
  let Occurrence {
    starts_at_ms,
    ends_at_ms,
  } = occurrence;
  if now_ms >= starts_at_ms {
    let is_running = now_ms < starts_at_ms + START_GRACE_MS && now_ms < ends_at_ms;
    return (is_running && recipient.maybe_notifies_start.unwrap_or(true))
      .then_some(MeetingNoticeKind::Start);
  }
  let minutes = recipient
    .maybe_reminder_minutes
    .map_or(i64::from(DEFAULT_MEETING_REMINDER_MINUTES), i64::from);
  let reminder_at = starts_at_ms - minutes * MS_PER_MINUTE;
  (minutes > 0 && now_ms >= reminder_at).then_some(MeetingNoticeKind::Reminder)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  const START: i64 = 1_800_000_000_000;
  const OCCURRENCE: Occurrence = Occurrence {
    starts_at_ms: START,
    ends_at_ms: START + 60 * MS_PER_MINUTE,
  };

  fn recipient(maybe_minutes: Option<i16>, maybe_start: Option<bool>) -> MeetingNoticeRecipient {
    MeetingNoticeRecipient {
      user: UserId::from_snowflake(Snowflake::ZERO),
      maybe_reminder_minutes: maybe_minutes,
      maybe_notifies_start: maybe_start,
    }
  }

  mod reminder {
    use super::*;

    #[test]
    fn comes_at_the_users_lead_time_and_is_five_minutes_by_default() {
      let default = recipient(None, None);
      assert_eq!(
        due_notice(OCCURRENCE, START - 5 * MS_PER_MINUTE - 1, &default),
        None
      );
      assert_eq!(
        due_notice(OCCURRENCE, START - 5 * MS_PER_MINUTE, &default),
        Some(MeetingNoticeKind::Reminder)
      );
      let early = recipient(Some(15), None);
      assert_eq!(
        due_notice(OCCURRENCE, START - 15 * MS_PER_MINUTE, &early),
        Some(MeetingNoticeKind::Reminder)
      );
    }

    #[test]
    fn off_means_no_reminder_but_the_start_notice_stays() {
      let off = recipient(Some(0), None);
      assert_eq!(due_notice(OCCURRENCE, START - MS_PER_MINUTE, &off), None);
      assert_eq!(
        due_notice(OCCURRENCE, START, &off),
        Some(MeetingNoticeKind::Start)
      );
    }
  }

  mod start_notice {
    use super::*;

    #[test]
    fn comes_at_the_start_and_for_ten_minutes_after_it() {
      let default = recipient(None, None);
      assert_eq!(
        due_notice(OCCURRENCE, START, &default),
        Some(MeetingNoticeKind::Start)
      );
      assert_eq!(
        due_notice(OCCURRENCE, START + 10 * MS_PER_MINUTE - 1, &default),
        Some(MeetingNoticeKind::Start)
      );
      assert_eq!(due_notice(OCCURRENCE, START + 10 * MS_PER_MINUTE, &default), None);
    }

    #[test]
    fn stops_with_the_end_of_a_short_meeting() {
      let short = Occurrence {
        starts_at_ms: START,
        ends_at_ms: START + MS_PER_MINUTE,
      };
      assert_eq!(
        due_notice(short, START + MS_PER_MINUTE, &recipient(None, None)),
        None
      );
    }

    #[test]
    fn can_be_turned_off() {
      assert_eq!(due_notice(OCCURRENCE, START, &recipient(None, Some(false))), None);
    }
  }
}
