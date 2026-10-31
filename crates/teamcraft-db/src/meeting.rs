//! Meetings, their invitees and their chat conversation (PLAN.md §9).

use std::collections::HashMap;

use teamcraft_proto::{
  ChannelRole, ConversationId, IdempotencyKey, MeetingAttendeeId, MeetingId, MeetingInviteId, UserId,
};
use time::OffsetDateTime;

use crate::conversation::{NewConversation, insert_conversation, insert_membership};
use crate::database::Database;
use crate::error::DbError;
use crate::idempotency::{Created, IdempotentKind, claim_key};
use crate::ids::{from_db, from_db_opt, to_db};

/// A stored meeting. `kind`, `status` are the wire names (`scheduled`, `instant`; `scheduled`,
/// `cancelled`, `ended`).
#[derive(Debug, Clone)]
pub struct StoredMeeting {
  pub id: MeetingId,
  pub uid: String,
  pub organizer: UserId,
  pub conversation: ConversationId,
  pub title: String,
  pub maybe_description: Option<String>,
  pub starts_at: OffsetDateTime,
  pub ends_at: OffsetDateTime,
  pub maybe_recurrence_rule: Option<String>,
  pub kind: String,
  pub status: String,
  pub has_lobby: bool,
  pub sequence: i32,
  /// When the whole meeting was cancelled; `None` while it is on, and for meetings cancelled before
  /// this was recorded.
  pub maybe_cancelled_at: Option<OffsetDateTime>,
}

/// An invitee as stored. `rsvp` is the wire name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAttendee {
  pub id: MeetingAttendeeId,
  pub meeting: MeetingId,
  pub maybe_user: Option<UserId>,
  pub maybe_email: Option<String>,
  pub maybe_name: Option<String>,
  pub rsvp: String,
}

#[derive(Debug, Clone)]
pub struct NewMeeting<'a> {
  pub id: MeetingId,
  pub uid: &'a str,
  pub organizer: UserId,
  pub conversation: NewConversation<'a>,
  pub title: &'a str,
  pub maybe_description: Option<&'a str>,
  pub starts_at: OffsetDateTime,
  pub ends_at: OffsetDateTime,
  pub maybe_recurrence_rule: Option<&'a str>,
  pub kind: &'a str,
  pub has_lobby: bool,
  pub attendees: &'a [NewAttendee],
  pub maybe_idempotency_key: Option<IdempotencyKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAttendee {
  pub id: MeetingAttendeeId,
  pub maybe_user: Option<UserId>,
  /// Lower-cased.
  pub maybe_email: Option<String>,
  pub maybe_name: Option<String>,
}

/// Fields to change; `None` keeps the stored value. An empty description clears it.
#[derive(Debug, Clone, Default)]
pub struct MeetingChanges<'a> {
  pub maybe_title: Option<&'a str>,
  pub maybe_description: Option<&'a str>,
  pub maybe_starts_at: Option<OffsetDateTime>,
  pub maybe_ends_at: Option<OffsetDateTime>,
  pub maybe_has_lobby: Option<bool>,
  /// A new RRULE value (a series is never turned back into a single meeting).
  pub maybe_recurrence_rule: Option<&'a str>,
}

/// A join link of a meeting: a personal one-time link, or the meeting's shared guest link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMeetingInvite {
  pub id: MeetingInviteId,
  pub meeting: MeetingId,
  pub maybe_email: Option<String>,
  pub maybe_label: Option<String>,
  pub is_redeemed: bool,
  pub maybe_guest_user: Option<UserId>,
  /// The shared link of the meeting: many guests, each gets an invite of their own.
  pub is_shared: bool,
  pub is_revoked: bool,
}

/// Why a guest could not get an invite from a shared link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedInviteClaim {
  Claimed,
  /// The link was revoked (or is not a shared link).
  Revoked,
  /// The link has admitted as many guests as it may.
  Full,
}

/// A guest waiting in (or let out of) the lobby.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LobbyRow {
  pub user: UserId,
  /// `waiting`, `admitted` or `denied`.
  pub state: String,
  pub requested_at: OffsetDateTime,
}

/// How the invitee list changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttendeeDiff {
  pub added: Vec<StoredAttendee>,
  pub removed: Vec<StoredAttendee>,
  /// Guest accounts that had redeemed the personal link of a removed e-mail invitee; their
  /// sessions must end.
  pub revoked_guests: Vec<UserId>,
}

/// Which invitee an answer belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttendeeKey<'a> {
  User(UserId),
  Email(&'a str),
}

impl Database {
  /// Creates the meeting, its chat conversation (the organizer is admin there) and the invitees;
  /// invited users become members of the chat.
  pub async fn create_meeting(&self, new: &NewMeeting<'_>) -> Result<Created<MeetingId>, DbError> {
    let mut tx = self.pool().begin().await?;
    if let Some(key) = &new.maybe_idempotency_key {
      let maybe_existing = claim_key(
        &mut tx,
        new.organizer,
        IdempotentKind::Meeting,
        key,
        to_db(new.id),
      )
      .await?;
      if let Some(existing) = maybe_existing {
        return Ok(Created {
          id: from_db(existing)?,
          is_new: false,
        });
      }
    }
    insert_conversation(&mut tx, &new.conversation).await?;
    insert_membership(&mut tx, new.conversation.id, new.organizer, ChannelRole::Admin).await?;
    sqlx::query!(
      r#"INSERT INTO meetings (id, uid, organizer_user_id, conversation_id, title, description, starts_at,
                               ends_at, recurrence_rule, kind, has_lobby)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"#,
      to_db(new.id),
      new.uid,
      to_db(new.organizer),
      to_db(new.conversation.id),
      new.title,
      new.maybe_description,
      new.starts_at,
      new.ends_at,
      new.maybe_recurrence_rule,
      new.kind,
      new.has_lobby,
    )
    .execute(&mut *tx)
    .await?;
    for attendee in new.attendees {
      insert_attendee(&mut tx, new.id, attendee).await?;
      if let Some(user) = attendee.maybe_user.filter(|user| *user != new.organizer) {
        insert_membership(&mut tx, new.conversation.id, user, ChannelRole::Member).await?;
      }
    }
    tx.commit().await?;
    Ok(Created {
      id: new.id,
      is_new: true,
    })
  }

  pub async fn get_meeting(&self, id: MeetingId) -> Result<Option<StoredMeeting>, DbError> {
    let row = sqlx::query!(
      r#"SELECT id, uid, organizer_user_id, conversation_id, title, description, starts_at, ends_at,
                recurrence_rule, kind, status, has_lobby, sequence, cancelled_at
         FROM meetings WHERE id = $1"#,
      to_db(id),
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|row| {
        Ok(StoredMeeting {
          id: from_db(row.id)?,
          uid: row.uid,
          organizer: from_db(row.organizer_user_id)?,
          conversation: from_db(row.conversation_id)?,
          title: row.title,
          maybe_description: row.description,
          starts_at: row.starts_at,
          ends_at: row.ends_at,
          maybe_recurrence_rule: row.recurrence_rule,
          kind: row.kind,
          status: row.status,
          has_lobby: row.has_lobby,
          sequence: row.sequence,
          maybe_cancelled_at: row.cancelled_at,
        })
      })
      .transpose()
  }

  pub async fn get_meeting_by_uid(&self, uid: &str) -> Result<Option<StoredMeeting>, DbError> {
    let id = sqlx::query_scalar!("SELECT id FROM meetings WHERE uid = $1", uid)
      .fetch_optional(self.pool())
      .await?;
    match id {
      Some(id) => self.get_meeting(from_db(id)?).await,
      None => Ok(None),
    }
  }

  /// Meetings the user organizes or is invited to that end after `from` and start before `to`,
  /// soonest first. Cancelled meetings are included (clients show them struck through) until the
  /// organizer takes them off the lists. A repeating series is included whatever `from` is, as its
  /// stored first occurrence; the caller expands it.
  pub async fn list_meetings_of_user(
    &self,
    user: UserId,
    from: OffsetDateTime,
    maybe_to: Option<OffsetDateTime>,
    limit: i64,
  ) -> Result<Vec<StoredMeeting>, DbError> {
    let ids = sqlx::query_scalar!(
      r#"SELECT m.id FROM meetings m
         WHERE (m.ends_at > $2 OR m.recurrence_rule IS NOT NULL) AND ($3::timestamptz IS NULL OR m.starts_at < $3)
           AND m.hidden_at IS NULL
           AND (m.organizer_user_id = $1
                OR EXISTS (SELECT 1 FROM meeting_attendees a WHERE a.meeting_id = m.id AND a.user_id = $1))
         ORDER BY m.starts_at, m.id LIMIT $4"#,
      to_db(user),
      from,
      maybe_to,
      limit,
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

  /// Users among `users` that are in a meeting at `now` (organizer, or invited and not declined; not
  /// cancelled), with the end of the latest such meeting.
  pub async fn users_in_meeting(
    &self,
    users: &[UserId],
    now: OffsetDateTime,
  ) -> Result<HashMap<UserId, OffsetDateTime>, DbError> {
    let raw: Vec<i64> = users.iter().map(|user| to_db(*user)).collect();
    let rows = sqlx::query!(
      r#"SELECT u.user_id AS "user_id!", max(m.ends_at) AS "ends_at!"
         FROM meetings m
         JOIN LATERAL (
           SELECT m.organizer_user_id AS user_id
           UNION
           SELECT a.user_id FROM meeting_attendees a
            WHERE a.meeting_id = m.id AND a.user_id IS NOT NULL AND a.rsvp <> 'declined'
         ) u ON true
         WHERE m.status = 'scheduled' AND m.starts_at <= $2 AND m.ends_at > $2
           AND u.user_id = ANY($1)
         GROUP BY u.user_id"#,
      &raw,
      now,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| Ok((from_db(row.user_id)?, row.ends_at)))
      .collect()
  }

  pub async fn list_attendees(&self, meeting: MeetingId) -> Result<Vec<StoredAttendee>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT id, meeting_id, user_id, email, display_name, rsvp FROM meeting_attendees
         WHERE meeting_id = $1 ORDER BY id"#,
      to_db(meeting),
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(StoredAttendee {
          id: from_db(row.id)?,
          meeting: from_db(row.meeting_id)?,
          maybe_user: from_db_opt(row.user_id)?,
          maybe_email: row.email,
          maybe_name: row.display_name,
          rsvp: row.rsvp,
        })
      })
      .collect()
  }

  /// Applies the changes and increases the iCalendar sequence. Returns the new meeting.
  pub async fn update_meeting(
    &self,
    id: MeetingId,
    changes: &MeetingChanges<'_>,
  ) -> Result<Option<StoredMeeting>, DbError> {
    sqlx::query!(
      r#"UPDATE meetings SET
           title = COALESCE($2, title),
           description = CASE WHEN $3::text IS NULL THEN description WHEN $3 = '' THEN NULL ELSE $3 END,
           starts_at = COALESCE($4, starts_at),
           ends_at = COALESCE($5, ends_at),
           has_lobby = COALESCE($6, has_lobby),
           recurrence_rule = COALESCE($7, recurrence_rule),
           sequence = sequence + 1
         WHERE id = $1"#,
      to_db(id),
      changes.maybe_title,
      changes.maybe_description,
      changes.maybe_starts_at,
      changes.maybe_ends_at,
      changes.maybe_has_lobby,
      changes.maybe_recurrence_rule,
    )
    .execute(self.pool())
    .await?;
    self.get_meeting(id).await
  }

  /// Replaces the invitee list. People who stay keep their answer; invited users join the chat and
  /// removed users leave it (the organizer always stays).
  pub async fn replace_attendees(
    &self,
    meeting: &StoredMeeting,
    wanted: &[NewAttendee],
  ) -> Result<AttendeeDiff, DbError> {
    let mut tx = self.pool().begin().await?;
    let current = self.list_attendees(meeting.id).await?;
    let mut diff = AttendeeDiff::default();

    for existing in &current {
      let is_kept = wanted.iter().any(|candidate| same_person(existing, candidate));
      if !is_kept {
        sqlx::query!("DELETE FROM meeting_attendees WHERE id = $1", to_db(existing.id))
          .execute(&mut *tx)
          .await?;
        if let Some(user) = existing.maybe_user.filter(|user| *user != meeting.organizer) {
          sqlx::query!(
            "DELETE FROM memberships WHERE conversation_id = $1 AND user_id = $2",
            to_db(meeting.conversation),
            to_db(user),
          )
          .execute(&mut *tx)
          .await?;
        }
        if let Some(email) = &existing.maybe_email {
          // The personal join link of a removed invitee must stop working: an unused invite is
          // deleted (a later re-invitation gets a fresh one), a redeemed one is revoked and its
          // guest account reported so the caller ends the session.
          sqlx::query!(
            "DELETE FROM meeting_invites WHERE meeting_id = $1 AND lower(email) = lower($2) AND redeemed_at IS NULL",
            to_db(meeting.id),
            email,
          )
          .execute(&mut *tx)
          .await?;
          let guests = sqlx::query_scalar!(
            r#"UPDATE meeting_invites SET revoked_at = now()
               WHERE meeting_id = $1 AND lower(email) = lower($2) AND redeemed_at IS NOT NULL AND revoked_at IS NULL
               RETURNING guest_user_id"#,
            to_db(meeting.id),
            email,
          )
          .fetch_all(&mut *tx)
          .await?;
          for guest in guests.into_iter().flatten() {
            diff.revoked_guests.push(from_db(guest)?);
          }
        }
        diff.removed.push(existing.clone());
      }
    }
    for candidate in wanted {
      if current.iter().any(|existing| same_person(existing, candidate)) {
        continue;
      }
      insert_attendee(&mut tx, meeting.id, candidate).await?;
      if let Some(user) = candidate.maybe_user.filter(|user| *user != meeting.organizer) {
        sqlx::query!(
          r#"INSERT INTO memberships (conversation_id, user_id, role) VALUES ($1, $2, 'member')
             ON CONFLICT DO NOTHING"#,
          to_db(meeting.conversation),
          to_db(user),
        )
        .execute(&mut *tx)
        .await?;
      }
      diff.added.push(StoredAttendee {
        id: candidate.id,
        meeting: meeting.id,
        maybe_user: candidate.maybe_user,
        maybe_email: candidate.maybe_email.clone(),
        maybe_name: candidate.maybe_name.clone(),
        rsvp: "needs_action".into(),
      });
    }
    tx.commit().await?;
    Ok(diff)
  }

  /// The meeting whose chat is this conversation.
  pub async fn get_meeting_by_conversation(
    &self,
    conversation: ConversationId,
  ) -> Result<Option<StoredMeeting>, DbError> {
    let id = sqlx::query_scalar!(
      "SELECT id FROM meetings WHERE conversation_id = $1",
      to_db(conversation)
    )
    .fetch_optional(self.pool())
    .await?;
    match id {
      Some(id) => self.get_meeting(from_db(id)?).await,
      None => Ok(None),
    }
  }

  /// The host ended the meeting for everyone. `false` if it was not scheduled.
  pub async fn end_meeting(&self, id: MeetingId) -> Result<bool, DbError> {
    let result = sqlx::query!(
      "UPDATE meetings SET status = 'ended' WHERE id = $1 AND status = 'scheduled'",
      to_db(id)
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() == 1)
  }

  /// Marks the meeting cancelled and archives its chat. Returns `false` if it was not scheduled.
  pub async fn cancel_meeting(&self, meeting: &StoredMeeting) -> Result<bool, DbError> {
    let mut tx = self.pool().begin().await?;
    let result = sqlx::query!(
      r#"UPDATE meetings SET status = 'cancelled', cancelled_at = now(), sequence = sequence + 1
         WHERE id = $1 AND status = 'scheduled'"#,
      to_db(meeting.id),
    )
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
      return Ok(false);
    }
    sqlx::query!(
      "UPDATE conversations SET archived_at = now() WHERE id = $1 AND archived_at IS NULL",
      to_db(meeting.conversation),
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
  }

  /// Takes a cancellation back: the meeting is scheduled again and its chat no longer archived.
  /// `false` if it was not cancelled.
  pub async fn restore_meeting(&self, meeting: &StoredMeeting) -> Result<bool, DbError> {
    let mut tx = self.pool().begin().await?;
    let result = sqlx::query!(
      r#"UPDATE meetings SET status = 'scheduled', cancelled_at = NULL, hidden_at = NULL, sequence = sequence + 1
         WHERE id = $1 AND status = 'cancelled'"#,
      to_db(meeting.id),
    )
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
      return Ok(false);
    }
    sqlx::query!(
      "UPDATE conversations SET archived_at = NULL WHERE id = $1",
      to_db(meeting.conversation),
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
  }

  /// Takes a cancelled meeting off everybody's lists; its chat stays. `false` if it is not cancelled.
  pub async fn hide_meeting(&self, id: MeetingId) -> Result<bool, DbError> {
    let result = sqlx::query!(
      "UPDATE meetings SET hidden_at = now() WHERE id = $1 AND status = 'cancelled' AND hidden_at IS NULL",
      to_db(id),
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() == 1)
  }

  /// Cancels one occurrence of a series (by its start in the series) and increases the iCalendar
  /// sequence. `false` if it was cancelled already.
  pub async fn cancel_occurrence(
    &self,
    meeting: MeetingId,
    starts_at: OffsetDateTime,
  ) -> Result<bool, DbError> {
    let mut tx = self.pool().begin().await?;
    let result = sqlx::query!(
      r#"INSERT INTO meeting_cancelled_occurrences (meeting_id, starts_at) VALUES ($1, $2)
         ON CONFLICT DO NOTHING"#,
      to_db(meeting),
      starts_at,
    )
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
      return Ok(false);
    }
    bump_sequence(&mut *tx, meeting).await?;
    tx.commit().await?;
    Ok(true)
  }

  /// Takes back the cancellation of one occurrence. `false` if it was not cancelled.
  pub async fn restore_occurrence(
    &self,
    meeting: MeetingId,
    starts_at: OffsetDateTime,
  ) -> Result<bool, DbError> {
    let mut tx = self.pool().begin().await?;
    let result = sqlx::query!(
      "DELETE FROM meeting_cancelled_occurrences WHERE meeting_id = $1 AND starts_at = $2",
      to_db(meeting),
      starts_at,
    )
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
      return Ok(false);
    }
    bump_sequence(&mut *tx, meeting).await?;
    tx.commit().await?;
    Ok(true)
  }

  /// The cancelled occurrences of the meetings, soonest first per meeting.
  pub async fn cancelled_occurrences(
    &self,
    meetings: &[MeetingId],
  ) -> Result<HashMap<MeetingId, Vec<OffsetDateTime>>, DbError> {
    let raw: Vec<i64> = meetings.iter().map(|meeting| to_db(*meeting)).collect();
    let rows = sqlx::query!(
      r#"SELECT meeting_id, starts_at FROM meeting_cancelled_occurrences
         WHERE meeting_id = ANY($1) ORDER BY meeting_id, starts_at"#,
      &raw,
    )
    .fetch_all(self.pool())
    .await?;
    let mut found: HashMap<MeetingId, Vec<OffsetDateTime>> = HashMap::new();
    for row in rows {
      found
        .entry(from_db(row.meeting_id)?)
        .or_default()
        .push(row.starts_at);
    }
    Ok(found)
  }

  /// Records an invitee's answer; `false` if nobody matches.
  pub async fn set_rsvp(
    &self,
    meeting: MeetingId,
    key: &AttendeeKey<'_>,
    rsvp: &str,
  ) -> Result<bool, DbError> {
    let (maybe_user, maybe_email) = match key {
      AttendeeKey::User(user) => (Some(to_db(*user)), None),
      AttendeeKey::Email(email) => (None, Some(*email)),
    };
    let result = sqlx::query!(
      r#"UPDATE meeting_attendees SET rsvp = $4
         WHERE meeting_id = $1 AND (user_id = $2 OR lower(email) = lower($3))"#,
      to_db(meeting),
      maybe_user,
      maybe_email,
      rsvp,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() > 0)
  }

  /// Removes a meeting with its chat and invitees (rollback of a failed creation).
  pub async fn delete_meeting(&self, meeting: &StoredMeeting) -> Result<(), DbError> {
    // The key goes with the meeting, so a retry after an undone creation creates it again.
    sqlx::query!(
      "DELETE FROM create_idempotency_keys WHERE kind = 'meeting' AND resource_id = $1",
      to_db(meeting.id)
    )
    .execute(self.pool())
    .await?;
    sqlx::query!(
      "DELETE FROM conversations WHERE id = $1",
      to_db(meeting.conversation)
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// E-mail addresses of the users that have one.
  pub async fn get_user_emails(&self, users: &[UserId]) -> Result<HashMap<UserId, String>, DbError> {
    let raw: Vec<i64> = users.iter().map(|user| to_db(*user)).collect();
    let rows = sqlx::query!(
      "SELECT id, email AS \"email!\" FROM users WHERE id = ANY($1) AND email IS NOT NULL",
      &raw,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| Ok((from_db(row.id)?, row.email)))
      .collect()
  }

  /// The unredeemed invite for this e-mail address, or a new one with `new_id`.
  pub async fn ensure_email_invite(
    &self,
    meeting: MeetingId,
    email: &str,
    new_id: MeetingInviteId,
    created_by: UserId,
  ) -> Result<MeetingInviteId, DbError> {
    let id = sqlx::query_scalar!(
      r#"INSERT INTO meeting_invites (id, meeting_id, email, created_by) VALUES ($1, $2, $3, $4)
         ON CONFLICT (meeting_id, email) WHERE email IS NOT NULL AND redeemed_at IS NULL
         DO UPDATE SET email = EXCLUDED.email
         RETURNING id"#,
      to_db(new_id),
      to_db(meeting),
      email,
      to_db(created_by),
    )
    .fetch_one(self.pool())
    .await?;
    from_db(id)
  }

  /// A link the host hands out by hand.
  pub async fn create_label_invite(
    &self,
    meeting: MeetingId,
    id: MeetingInviteId,
    maybe_label: Option<&str>,
    created_by: UserId,
  ) -> Result<(), DbError> {
    sqlx::query!(
      "INSERT INTO meeting_invites (id, meeting_id, label, created_by) VALUES ($1, $2, $3, $4)",
      to_db(id),
      to_db(meeting),
      maybe_label,
      to_db(created_by),
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  pub async fn get_meeting_invite(
    &self,
    id: MeetingInviteId,
  ) -> Result<Option<StoredMeetingInvite>, DbError> {
    let row = sqlx::query!(
      r#"SELECT id, meeting_id, email, label, redeemed_at IS NOT NULL AS "is_redeemed!", guest_user_id,
                is_shared, revoked_at IS NOT NULL AS "is_revoked!"
         FROM meeting_invites WHERE id = $1"#,
      to_db(id),
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|row| {
        Ok(StoredMeetingInvite {
          id: from_db(row.id)?,
          meeting: from_db(row.meeting_id)?,
          maybe_email: row.email,
          maybe_label: row.label,
          is_redeemed: row.is_redeemed,
          maybe_guest_user: from_db_opt(row.guest_user_id)?,
          is_shared: row.is_shared,
          is_revoked: row.is_revoked,
        })
      })
      .transpose()
  }

  /// The live shared guest link of the meeting, or a new one with `new_id`.
  pub async fn ensure_shared_invite(
    &self,
    meeting: MeetingId,
    new_id: MeetingInviteId,
    created_by: UserId,
  ) -> Result<MeetingInviteId, DbError> {
    let id = sqlx::query_scalar!(
      r#"INSERT INTO meeting_invites (id, meeting_id, created_by, is_shared) VALUES ($1, $2, $3, true)
         ON CONFLICT (meeting_id) WHERE is_shared AND revoked_at IS NULL
         DO UPDATE SET is_shared = EXCLUDED.is_shared
         RETURNING id"#,
      to_db(new_id),
      to_db(meeting),
      to_db(created_by),
    )
    .fetch_one(self.pool())
    .await?;
    from_db(id)
  }

  /// Revokes the meeting's live shared guest link; `false` if it has none.
  pub async fn revoke_shared_invite(&self, meeting: MeetingId) -> Result<bool, DbError> {
    let result = sqlx::query!(
      "UPDATE meeting_invites SET revoked_at = now() WHERE meeting_id = $1 AND is_shared AND revoked_at IS NULL",
      to_db(meeting),
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() > 0)
  }

  /// Makes a one-time invite `id` for one guest of the shared link `shared`, if the link is live and
  /// has handed out fewer than `max_guests` invites. The link's row is locked while counting, so
  /// concurrent guests can't exceed the limit.
  pub async fn claim_shared_invite(
    &self,
    shared: MeetingInviteId,
    id: MeetingInviteId,
    label: &str,
    max_guests: u32,
  ) -> Result<SharedInviteClaim, DbError> {
    let mut tx = self.pool().begin().await?;
    let Some(link) = sqlx::query!(
      r#"SELECT meeting_id, created_by FROM meeting_invites
         WHERE id = $1 AND is_shared AND revoked_at IS NULL FOR UPDATE"#,
      to_db(shared),
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
      return Ok(SharedInviteClaim::Revoked);
    };
    let handed_out = sqlx::query_scalar!(
      r#"SELECT count(*) AS "count!" FROM meeting_invites WHERE shared_invite_id = $1"#,
      to_db(shared),
    )
    .fetch_one(&mut *tx)
    .await?;
    if handed_out >= i64::from(max_guests) {
      return Ok(SharedInviteClaim::Full);
    }
    sqlx::query!(
      r#"INSERT INTO meeting_invites (id, meeting_id, label, created_by, shared_invite_id)
         VALUES ($1, $2, $3, $4, $5)"#,
      to_db(id),
      link.meeting_id,
      label,
      link.created_by,
      to_db(shared),
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(SharedInviteClaim::Claimed)
  }

  /// Marks the invite redeemed by `guest`; `false` if it was redeemed before.
  pub async fn redeem_meeting_invite(&self, id: MeetingInviteId, guest: UserId) -> Result<bool, DbError> {
    let result = sqlx::query!(
      "UPDATE meeting_invites SET redeemed_at = now(), guest_user_id = $2 WHERE id = $1 AND redeemed_at IS NULL",
      to_db(id),
      to_db(guest),
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() > 0)
  }

  /// Puts the guest into the lobby as waiting, or returns the existing state.
  pub async fn enter_lobby(&self, meeting: MeetingId, user: UserId) -> Result<LobbyRow, DbError> {
    let row = sqlx::query!(
      r#"INSERT INTO meeting_lobby (meeting_id, user_id) VALUES ($1, $2)
         ON CONFLICT (meeting_id, user_id) DO UPDATE SET meeting_id = EXCLUDED.meeting_id
         RETURNING state, requested_at"#,
      to_db(meeting),
      to_db(user),
    )
    .fetch_one(self.pool())
    .await?;
    Ok(LobbyRow {
      user,
      state: row.state,
      requested_at: row.requested_at,
    })
  }

  /// A member who is not in the meeting chat knocks: they wait in the lobby. Returns the row and
  /// whether the knock is news for the hosts: a first knock, or a member admitted before who is no
  /// longer in the chat (waiting again). Knocking again while waiting, or after being denied,
  /// changes nothing.
  pub async fn knock_lobby(&self, meeting: MeetingId, user: UserId) -> Result<(LobbyRow, bool), DbError> {
    let maybe_changed = sqlx::query!(
      r#"INSERT INTO meeting_lobby (meeting_id, user_id) VALUES ($1, $2)
         ON CONFLICT (meeting_id, user_id) DO UPDATE
           SET state = 'waiting', requested_at = now(), decided_at = NULL, decided_by = NULL
           WHERE meeting_lobby.state = 'admitted'
         RETURNING state, requested_at"#,
      to_db(meeting),
      to_db(user),
    )
    .fetch_optional(self.pool())
    .await?;
    if let Some(row) = maybe_changed {
      let entry = LobbyRow {
        user,
        state: row.state,
        requested_at: row.requested_at,
      };
      return Ok((entry, true));
    }
    let existing = self
      .get_lobby_entry(meeting, user)
      .await?
      // Only gone if the meeting was deleted in between.
      .ok_or(DbError::Query(sqlx::Error::RowNotFound))?;
    Ok((existing, false))
  }

  /// Gives back the place a shared link handed out when the guest never got in with it (their
  /// account or the redemption failed), so failures don't use up the link.
  pub async fn release_shared_invite_claim(&self, id: MeetingInviteId) -> Result<(), DbError> {
    sqlx::query!(
      "DELETE FROM meeting_invites WHERE id = $1 AND shared_invite_id IS NOT NULL AND redeemed_at IS NULL",
      to_db(id),
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  pub async fn get_lobby_entry(&self, meeting: MeetingId, user: UserId) -> Result<Option<LobbyRow>, DbError> {
    let row = sqlx::query!(
      "SELECT state, requested_at FROM meeting_lobby WHERE meeting_id = $1 AND user_id = $2",
      to_db(meeting),
      to_db(user),
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(row.map(|row| LobbyRow {
      user,
      state: row.state,
      requested_at: row.requested_at,
    }))
  }

  /// Active guests of meetings whose access has ended: the meeting is over for longer than
  /// `grace_secs`, or it was cancelled. At most `limit`, oldest meeting first; the caller
  /// deactivates them and asks again.
  pub async fn list_guests_past_access(
    &self,
    now: OffsetDateTime,
    grace_secs: f64,
    limit: i64,
  ) -> Result<Vec<(MeetingId, UserId)>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT DISTINCT l.user_id, l.meeting_id, m.ends_at
         FROM meeting_lobby l
         JOIN meetings m ON m.id = l.meeting_id
         JOIN users u ON u.id = l.user_id
         WHERE u.kind = 'guest' AND u.deactivated_at IS NULL
           AND (m.status <> 'scheduled' OR m.ends_at + make_interval(secs => $2) < $1)
         ORDER BY m.ends_at, l.user_id
         LIMIT $3"#,
      now,
      grace_secs,
      limit,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| Ok((from_db(row.meeting_id)?, from_db(row.user_id)?)))
      .collect()
  }

  /// Stores a guest's proof of the link secret, once. `false` when there is none to set (the guest
  /// has no lobby place) or one is stored already.
  pub async fn set_guest_proof(
    &self,
    meeting: MeetingId,
    user: UserId,
    proof: &[u8],
  ) -> Result<bool, DbError> {
    let result = sqlx::query!(
      "UPDATE meeting_lobby SET guest_proof = $3 WHERE meeting_id = $1 AND user_id = $2 AND guest_proof IS NULL",
      to_db(meeting),
      to_db(user),
      proof,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() == 1)
  }

  /// The proofs guests of the meeting stored, by guest.
  pub async fn guest_proofs(&self, meeting: MeetingId) -> Result<HashMap<UserId, Vec<u8>>, DbError> {
    let rows = sqlx::query!(
      "SELECT user_id, guest_proof FROM meeting_lobby WHERE meeting_id = $1 AND guest_proof IS NOT NULL",
      to_db(meeting),
    )
    .fetch_all(self.pool())
    .await?;
    let mut proofs = HashMap::new();
    for row in rows {
      if let Some(proof) = row.guest_proof {
        proofs.insert(from_db(row.user_id)?, proof);
      }
    }
    Ok(proofs)
  }

  /// Guests waiting for a decision, longest waiting first.
  pub async fn list_waiting(&self, meeting: MeetingId) -> Result<Vec<LobbyRow>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT user_id, state, requested_at FROM meeting_lobby
         WHERE meeting_id = $1 AND state = 'waiting' ORDER BY requested_at, user_id"#,
      to_db(meeting),
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(LobbyRow {
          user: from_db(row.user_id)?,
          state: row.state,
          requested_at: row.requested_at,
        })
      })
      .collect()
  }

  /// Admits or denies a waiting guest (`state` is `admitted` or `denied`); an admitted guest joins
  /// the meeting chat. `false` if the guest was not waiting.
  pub async fn decide_lobby(
    &self,
    meeting: &StoredMeeting,
    user: UserId,
    state: &str,
    decided_by: UserId,
  ) -> Result<bool, DbError> {
    let mut tx = self.pool().begin().await?;
    let result = sqlx::query!(
      r#"UPDATE meeting_lobby SET state = $3, decided_at = now(), decided_by = $4
         WHERE meeting_id = $1 AND user_id = $2 AND state = 'waiting'"#,
      to_db(meeting.id),
      to_db(user),
      state,
      to_db(decided_by),
    )
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
      return Ok(false);
    }
    if state == "admitted" {
      sqlx::query!(
        "INSERT INTO memberships (conversation_id, user_id, role) VALUES ($1, $2, 'member') ON CONFLICT DO NOTHING",
        to_db(meeting.conversation),
        to_db(user),
      )
      .execute(&mut *tx)
      .await?;
    }
    tx.commit().await?;
    Ok(true)
  }

  /// Members of the meeting chat who may host: the organizer and chat admins.
  pub async fn meeting_host_ids(&self, meeting: &StoredMeeting) -> Result<Vec<UserId>, DbError> {
    let ids = sqlx::query_scalar!(
      "SELECT user_id FROM memberships WHERE conversation_id = $1 AND role = 'admin' ORDER BY user_id",
      to_db(meeting.conversation),
    )
    .fetch_all(self.pool())
    .await?;
    let mut hosts: Vec<UserId> = ids.into_iter().map(from_db).collect::<Result<_, _>>()?;
    if !hosts.contains(&meeting.organizer) {
      hosts.push(meeting.organizer);
    }
    Ok(hosts)
  }

  /// IDs of users the meeting concerns: organizer and invited users.
  pub async fn meeting_user_ids(&self, meeting: &StoredMeeting) -> Result<Vec<UserId>, DbError> {
    let ids = sqlx::query_scalar!(
      "SELECT user_id AS \"user_id!\" FROM meeting_attendees WHERE meeting_id = $1 AND user_id IS NOT NULL",
      to_db(meeting.id),
    )
    .fetch_all(self.pool())
    .await?;
    let mut users: Vec<UserId> = ids.into_iter().map(from_db).collect::<Result<_, _>>()?;
    users.push(meeting.organizer);
    users.sort_unstable();
    users.dedup();
    Ok(users)
  }
}

/// Every change sent to invitees increases the event's iCalendar SEQUENCE.
async fn bump_sequence(tx: &mut sqlx::PgConnection, meeting: MeetingId) -> Result<(), DbError> {
  sqlx::query!(
    "UPDATE meetings SET sequence = sequence + 1 WHERE id = $1",
    to_db(meeting)
  )
  .execute(tx)
  .await?;
  Ok(())
}

fn same_person(existing: &StoredAttendee, candidate: &NewAttendee) -> bool {
  match (existing.maybe_user, candidate.maybe_user) {
    (Some(a), Some(b)) => a == b,
    (None, None) => existing.maybe_email == candidate.maybe_email,
    _ => false,
  }
}

async fn insert_attendee(
  tx: &mut sqlx::PgConnection,
  meeting: MeetingId,
  attendee: &NewAttendee,
) -> Result<(), DbError> {
  sqlx::query!(
    r#"INSERT INTO meeting_attendees (id, meeting_id, user_id, email, display_name)
       VALUES ($1, $2, $3, $4, $5)"#,
    to_db(attendee.id),
    to_db(meeting),
    attendee.maybe_user.map(to_db),
    attendee.maybe_email,
    attendee.maybe_name,
  )
  .execute(tx)
  .await?;
  Ok(())
}
