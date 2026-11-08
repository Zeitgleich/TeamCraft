//! Who is told about a meeting's start, and who was already (docs/meetings.md, "Starting"). What is
//! due is worked out from the meeting as it is now; the only row kept is that a notice went out, so a
//! meeting that moved or was cancelled can't fire and a restart or second instance doesn't send twice.

use teamcraft_proto::{MeetingId, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db, to_db_all};
use crate::meeting::StoredMeeting;

/// One of the two notices of a meeting occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MeetingNoticeKind {
  /// Some minutes before the start.
  Reminder,
  /// At the start.
  Start,
}

impl MeetingNoticeKind {
  fn as_str(self) -> &'static str {
    match self {
      Self::Reminder => "reminder",
      Self::Start => "start",
    }
  }
}

/// Somebody a meeting's notices are for, with their settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeetingNoticeRecipient {
  pub user: UserId,
  /// Minutes before the start, `0`: no reminder. `None`: never chose.
  pub maybe_reminder_minutes: Option<i16>,
  /// `None`: never chose.
  pub maybe_notifies_start: Option<bool>,
}

impl Database {
  /// Planned meetings that are on and may have an occurrence starting between `from` and `to`: those
  /// that start in that range, and every series that began before its end (only its rule can tell).
  pub async fn meetings_for_notices(
    &self,
    from: OffsetDateTime,
    to: OffsetDateTime,
  ) -> Result<Vec<StoredMeeting>, DbError> {
    let ids = sqlx::query_scalar!(
      r#"SELECT id FROM meetings
         WHERE kind = 'scheduled' AND status = 'scheduled' AND starts_at < $2
           AND (starts_at >= $1 OR recurrence_rule IS NOT NULL)"#,
      from,
      to,
    )
    .fetch_all(self.pool())
    .await?;
    let mut meetings = Vec::with_capacity(ids.len());
    for id in ids {
      if let Some(meeting) = self.get_meeting(from_db(id)?).await? {
        meetings.push(meeting);
      }
    }
    Ok(meetings)
  }

  /// Everybody the meeting's notices are for: its organizer and the invited users who haven't
  /// declined. External invitees have no account to notify; deactivated accounts get nothing.
  pub async fn meeting_notice_recipients(
    &self,
    meeting: MeetingId,
  ) -> Result<Vec<MeetingNoticeRecipient>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT u.id AS "user_id!", s.meeting_reminder_minutes, s.notifies_meeting_start
         FROM (SELECT organizer_user_id AS user_id FROM meetings WHERE id = $1
               UNION
               SELECT user_id FROM meeting_attendees
               WHERE meeting_id = $1 AND user_id IS NOT NULL AND rsvp <> 'declined') invited
         JOIN users u ON u.id = invited.user_id AND u.deactivated_at IS NULL
         LEFT JOIN user_notification_settings s ON s.user_id = u.id"#,
      to_db(meeting),
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(MeetingNoticeRecipient {
          user: from_db(row.user_id)?,
          maybe_reminder_minutes: row.meeting_reminder_minutes,
          maybe_notifies_start: row.notifies_meeting_start,
        })
      })
      .collect()
  }

  /// Takes the notice of this occurrence for `users` and returns those it was still open for. One
  /// statement, so of several server instances exactly one gets each user.
  pub async fn claim_meeting_notices(
    &self,
    meeting: MeetingId,
    starts_at: OffsetDateTime,
    kind: MeetingNoticeKind,
    users: &[UserId],
  ) -> Result<Vec<UserId>, DbError> {
    let claimed = sqlx::query_scalar!(
      r#"INSERT INTO meeting_notices (meeting_id, starts_at, kind, user_id)
         SELECT $1, $2, $3, user_id FROM UNNEST($4::bigint[]) AS user_id
         ON CONFLICT DO NOTHING
         RETURNING user_id"#,
      to_db(meeting),
      starts_at,
      kind.as_str(),
      &to_db_all(users),
    )
    .fetch_all(self.pool())
    .await?;
    claimed.into_iter().map(from_db).collect()
  }

  /// Forgets the notices of occurrences that started before `before`. Returns how many.
  pub async fn delete_old_meeting_notices(&self, before: OffsetDateTime) -> Result<u64, DbError> {
    let result = sqlx::query!("DELETE FROM meeting_notices WHERE starts_at < $1", before)
      .execute(self.pool())
      .await?;
    Ok(result.rows_affected())
  }
}
