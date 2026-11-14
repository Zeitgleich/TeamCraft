//! Scheduling and changing a meeting as its organizer (docs/meetings.md), and the upkeep the
//! organizer's device does around it: the edits whose chat change is still owed, and the signed
//! admitters of meetings that run or start soon (#1103). Desktop and web (`teamcraft-ui`) and the
//! mobile apps (through teamcraft-ffi) decide alike; the app keeps the records where it can.
//!
//! [`MeetingDraft`] is the mobile apps' schedule form, in plain times: desktop and web keep their
//! own form of text inputs (`teamcraft_ui::meetings::ScheduleForm`), with repeats and e-mail
//! invitees, and check the same rules.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{
  CreateMeetingRequest, IdempotencyKey, MeetingId, MeetingInvitee, MeetingResponse, MeetingStatus,
  UnixMillis, UpdateMeetingRequest,
};

use crate::meeting_invitees::attending_users;
use crate::meeting_schedule::MeetingTimes;
use crate::model::{ConversationId, TimestampMs, UserId};

pub const MS_PER_MINUTE: i64 = 60_000;
const MS_PER_HOUR: i64 = 3_600_000;
/// How long a meeting may be scheduled for in one go (the server's limit).
pub const MAX_DURATION_MINUTES: u32 = 24 * 60;
/// A new meeting starts at least this far ahead of now, at a full hour.
const MIN_LEAD_MS: i64 = 10 * 60_000;
/// The organizer's device keeps the signed admitters current for meetings that run or start within
/// this time (#1103); later ones keep the list signed when they were scheduled or edited.
pub const ADMITTERS_LEAD_MS: i64 = 24 * 60 * 60_000;
/// How often per meeting the organizer's device has the core compare its signed admitters.
pub const ADMITTERS_SYNC_INTERVAL_MS: i64 = 10 * 60_000;
/// A pending edit younger than this may still be in flight in this session; only older ones are
/// reconciled.
const PENDING_EDIT_GRACE_MS: i64 = 2 * 60_000;
/// A pending edit older than this is dropped: the meeting is long gone from the list.
const PENDING_EDIT_MAX_AGE_MS: i64 = 7 * 24 * 3_600_000;

/// A meeting the organizer schedules or changes, as the mobile apps' form collects it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingDraft {
  pub title: String,
  pub description: String,
  pub starts_at: TimestampMs,
  pub duration_minutes: u32,
  /// The invited TeamCraft users, the organizer left out.
  pub members: Vec<UserId>,
  pub has_lobby: bool,
  /// Made when the form opens and kept for every submit of it, so a retry after a lost answer
  /// schedules the meeting only once. `None` for an edit.
  pub maybe_idempotency_key: Option<IdempotencyKey>,
}

/// Why a draft can't be saved; the form shows each at its field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DraftProblem {
  MissingTitle,
  StartInThePast,
  InvalidDuration,
}

impl MeetingDraft {
  /// A new meeting: 30 minutes at the next full hour with some lead, with a lobby.
  pub fn new(now: TimestampMs) -> Self {
    Self {
      title: String::new(),
      description: String::new(),
      starts_at: suggested_start(now),
      duration_minutes: 30,
      members: Vec::new(),
      has_lobby: true,
      maybe_idempotency_key: Some(IdempotencyKey::random()),
    }
  }

  /// The form of an edit: what the saved meeting says now. The organizer `me` is no invitee.
  pub fn from_meeting(meeting: &MeetingResponse, me: UserId) -> Self {
    let minutes = (meeting.ends_at.0 - meeting.starts_at.0) / MS_PER_MINUTE;
    let mut members = Vec::new();
    for user in attending_users(meeting) {
      if user != me && user != meeting.organizer_user_id && !members.contains(&user) {
        members.push(user);
      }
    }
    Self {
      title: meeting.title.clone(),
      description: meeting.maybe_description.clone().unwrap_or_default(),
      starts_at: meeting.starts_at.0,
      duration_minutes: u32::try_from(minutes).unwrap_or(0),
      members,
      has_lobby: meeting.has_lobby,
      maybe_idempotency_key: None,
    }
  }

  /// What keeps the draft from being saved, in the order of the fields. `maybe_saved_start` is the
  /// start of the meeting being edited: unchanged, it may lie in the past (a meeting under way can
  /// still get a new invitee).
  pub fn problems(&self, now: TimestampMs, maybe_saved_start: Option<TimestampMs>) -> Vec<DraftProblem> {
    let mut problems = Vec::new();
    if self.title.trim().is_empty() {
      problems.push(DraftProblem::MissingTitle);
    }
    let is_unchanged = maybe_saved_start.is_some_and(|saved| (self.starts_at - saved).abs() < MS_PER_MINUTE);
    if self.starts_at < now - MS_PER_MINUTE && !is_unchanged {
      problems.push(DraftProblem::StartInThePast);
    }
    if self.duration_minutes == 0 || self.duration_minutes > MAX_DURATION_MINUTES {
      problems.push(DraftProblem::InvalidDuration);
    }
    problems
  }

  /// The request that schedules the meeting, or every reason it can't be sent.
  pub fn to_request(&self, now: TimestampMs) -> Result<CreateMeetingRequest, Vec<DraftProblem>> {
    let problems = self.problems(now, None);
    if !problems.is_empty() {
      return Err(problems);
    }
    Ok(CreateMeetingRequest {
      title: self.title.trim().to_owned(),
      maybe_description: Some(self.description.trim().to_owned()).filter(|text| !text.is_empty()),
      starts_at: UnixMillis(self.starts_at),
      ends_at: UnixMillis(self.ends_at()),
      invitees: self.user_invitees(),
      maybe_recurrence: None,
      has_lobby: self.has_lobby,
      maybe_space: None,
      maybe_idempotency_key: self.maybe_idempotency_key,
    })
  }

  /// The change to save to `before`: the time only when it moved, the invitees as the whole list
  /// with the e-mail invitees `before` has (the form offers none), the repeat rule untouched.
  pub fn to_update_request(
    &self,
    now: TimestampMs,
    before: &MeetingResponse,
  ) -> Result<UpdateMeetingRequest, Vec<DraftProblem>> {
    let problems = self.problems(now, Some(before.starts_at.0));
    if !problems.is_empty() {
      return Err(problems);
    }
    // Whole minutes: seconds of the saved times are not a change.
    let has_moved = (self.starts_at - before.starts_at.0).abs() >= MS_PER_MINUTE
      || (self.ends_at() - before.ends_at.0).abs() >= MS_PER_MINUTE;
    let mut invitees = self.user_invitees();
    invitees.extend(before.attendees.iter().filter_map(|attendee| {
      match (attendee.maybe_user_id, &attendee.maybe_email) {
        (None, Some(email)) => Some(MeetingInvitee::External {
          email: email.clone(),
          maybe_name: attendee.maybe_name.clone().filter(|name| !name.is_empty()),
        }),
        _ => None,
      }
    }));
    Ok(UpdateMeetingRequest {
      maybe_title: Some(self.title.trim().to_owned()),
      maybe_description: Some(self.description.trim().to_owned()),
      maybe_starts_at: has_moved.then_some(UnixMillis(self.starts_at)),
      maybe_ends_at: has_moved.then_some(UnixMillis(self.ends_at())),
      maybe_invitees: Some(invitees),
      maybe_has_lobby: Some(self.has_lobby),
      maybe_recurrence: None,
    })
  }

  fn ends_at(&self) -> TimestampMs {
    self.starts_at + i64::from(self.duration_minutes) * MS_PER_MINUTE
  }

  fn user_invitees(&self) -> Vec<MeetingInvitee> {
    let mut users: Vec<UserId> = Vec::new();
    for user in &self.members {
      if !users.contains(user) {
        users.push(*user);
      }
    }
    users
      .into_iter()
      .map(|user_id| MeetingInvitee::User { user_id })
      .collect()
  }
}

/// The next full hour that is at least ten minutes away.
pub fn suggested_start(now: TimestampMs) -> TimestampMs {
  let earliest = now + MIN_LEAD_MS;
  earliest.div_euclid(MS_PER_HOUR) * MS_PER_HOUR + MS_PER_HOUR
}

/// The TeamCraft users a meeting request invites (externals have no account to add).
pub fn invited_users(request: &CreateMeetingRequest) -> Vec<UserId> {
  request
    .invitees
    .iter()
    .filter_map(|invitee| match invitee {
      MeetingInvitee::User { user_id } => Some(*user_id),
      MeetingInvitee::External { .. } => None,
    })
    .collect()
}

/// Who joined and who left a meeting's invitee list when the organizer saved a change: the whole
/// list now (for the chat's encryption group) and the users taken off it. Both come from the
/// organizer's own saved request and the meeting as it was, never from the server's member list
/// (e2ee.md §4.6, "Meeting chats").
pub fn invitee_change(
  before: &MeetingResponse,
  request: &UpdateMeetingRequest,
) -> Option<(Vec<UserId>, Vec<UserId>)> {
  let invitees = request.maybe_invitees.as_ref()?;
  let now: Vec<UserId> = invitees
    .iter()
    .filter_map(|invitee| match invitee {
      MeetingInvitee::User { user_id } => Some(*user_id),
      MeetingInvitee::External { .. } => None,
    })
    .collect();
  let removed = attending_users(before)
    .into_iter()
    .filter(|user| !now.contains(user) && *user != before.organizer_user_id)
    .collect();
  Some((now, removed))
}

/// Whether the meeting has ended or was cancelled (shown dimmed, no join).
pub fn is_over(meeting: &MeetingResponse, now: TimestampMs) -> bool {
  meeting.status != MeetingStatus::Scheduled || meeting.ends_at.0 <= now
}

/// A meeting whose signed admitters the organizer's device compares now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmittersDue {
  pub meeting: MeetingId,
  pub conversation: ConversationId,
  /// The meeting's (occurrence's) times as the server lists them; the core caps the signed end
  /// by its own record (`meeting_schedule::admitters_until`).
  pub server_times: MeetingTimes,
}

/// The meetings `me` organizes that run or start within [`ADMITTERS_LEAD_MS`] and aren't over:
/// who is in the meeting chat's group changes, and a series' next meeting needs a list that lasts
/// until its end (#1103). The core signs anew only when that differs; the app asks at most every
/// [`ADMITTERS_SYNC_INTERVAL_MS`] per meeting.
pub fn admitters_due(meetings: &[MeetingResponse], me: UserId, now: TimestampMs) -> Vec<AdmittersDue> {
  let mut due: Vec<AdmittersDue> = Vec::new();
  for meeting in meetings {
    if meeting.organizer_user_id != me
      || is_over(meeting, now)
      || meeting.starts_at.0 > now + ADMITTERS_LEAD_MS
      || due.iter().any(|known| known.meeting == meeting.id)
    {
      continue;
    }
    due.push(AdmittersDue {
      meeting: meeting.id,
      conversation: meeting.conversation_id,
      server_times: MeetingTimes::of(meeting),
    });
  }
  due
}

// ---- Edits whose chat change is still owed ----------------------------------------------------

/// Users the organizer is about to take off a meeting, written down before the save is sent. If the
/// app dies between the save and the commit that removes them from the chat's group, the next start
/// finishes it ([`plan_reconcile`]); the core's own record of the removal only exists once
/// `update_meeting_chat_members` ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingEdit {
  pub meeting: MeetingId,
  pub removed: Vec<UserId>,
  pub noted_at: TimestampMs,
}

/// The chat change a pending edit still owes: the meeting's chat, the attendees as the server lists
/// them now, and the users to take out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwedChatChange {
  pub meeting: MeetingId,
  pub conversation: ConversationId,
  pub attending: Vec<UserId>,
  pub removed: Vec<UserId>,
}

/// `edits` with the edit of `meeting` that removes `removed` noted (replacing an older one of it).
/// An edit that removes nobody owes nothing.
pub fn noting_pending_edit(
  mut edits: Vec<PendingEdit>,
  meeting: MeetingId,
  removed: Vec<UserId>,
  now: TimestampMs,
) -> Vec<PendingEdit> {
  if removed.is_empty() {
    return edits;
  }
  edits.retain(|edit| edit.meeting != meeting);
  edits.push(PendingEdit {
    meeting,
    removed,
    noted_at: now,
  });
  edits
}

/// Finishes edits that were saved but not applied to the chat's group (the app was closed in
/// between), judging by the meeting list: users no longer attending are removed, an edit that never
/// reached the server is forgotten. The records to keep, and the chat changes to make.
pub fn plan_reconcile(
  edits: &[PendingEdit],
  now: TimestampMs,
  meetings: &[MeetingResponse],
) -> (Vec<PendingEdit>, Vec<OwedChatChange>) {
  let mut kept = Vec::new();
  let mut owed = Vec::new();
  for edit in edits {
    let age = now - edit.noted_at;
    if age > PENDING_EDIT_MAX_AGE_MS {
      continue;
    }
    if age < PENDING_EDIT_GRACE_MS {
      kept.push(edit.clone());
      continue;
    }
    // Not in the list (not loaded, or over): decided on a later list, until it ages out.
    let Some(meeting) = meetings.iter().find(|meeting| meeting.id == edit.meeting) else {
      kept.push(edit.clone());
      continue;
    };
    let attending = attending_users(meeting);
    let removed: Vec<UserId> = edit
      .removed
      .iter()
      .copied()
      .filter(|user| !attending.contains(user))
      .collect();
    if !removed.is_empty() {
      owed.push(OwedChatChange {
        meeting: meeting.id,
        conversation: meeting.conversation_id,
        attending,
        removed,
      });
    }
  }
  (kept, owed)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, MeetingAttendeeResponse, MeetingKind, RsvpStatus, Snowflake};

  use super::*;

  const NOW: TimestampMs = 1_800_000_000_000;

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn attendee(maybe_user_id: Option<UserId>, maybe_email: Option<&str>) -> MeetingAttendeeResponse {
    MeetingAttendeeResponse {
      maybe_user_id,
      maybe_email: maybe_email.map(str::to_owned),
      maybe_name: None,
      rsvp: RsvpStatus::NeedsAction,
    }
  }

  fn meeting(organizer: UserId) -> MeetingResponse {
    MeetingResponse {
      id: id(7),
      title: "Planning".into(),
      maybe_description: None,
      starts_at: UnixMillis(NOW + 2 * MS_PER_HOUR),
      ends_at: UnixMillis(NOW + 2 * MS_PER_HOUR + 30 * MS_PER_MINUTE),
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

  #[test]
  fn a_new_draft_starts_at_a_full_hour_with_some_lead() {
    let draft = MeetingDraft::new(NOW);
    assert_eq!(draft.starts_at % MS_PER_HOUR, 0);
    assert!(draft.starts_at >= NOW + MIN_LEAD_MS);
    assert!(draft.maybe_idempotency_key.is_some());
    assert_eq!(draft.problems(NOW, None), vec![DraftProblem::MissingTitle]);
  }

  #[test]
  fn a_draft_is_refused_for_what_the_server_would_refuse() {
    let mut draft = MeetingDraft::new(NOW);
    draft.starts_at = NOW - 5 * MS_PER_MINUTE;
    draft.duration_minutes = 0;
    assert_eq!(
      draft.to_request(NOW),
      Err(vec![
        DraftProblem::MissingTitle,
        DraftProblem::StartInThePast,
        DraftProblem::InvalidDuration
      ])
    );
    draft.duration_minutes = MAX_DURATION_MINUTES + 1;
    assert!(draft.problems(NOW, None).contains(&DraftProblem::InvalidDuration));
  }

  #[test]
  fn a_valid_draft_invites_each_member_once() {
    let mut draft = MeetingDraft::new(NOW);
    draft.title = "  Planning ".into();
    draft.members = vec![id(2), id(3), id(2)];
    draft.duration_minutes = 45;
    let request = draft.to_request(NOW).expect("valid");
    assert_eq!(request.title, "Planning");
    assert_eq!(request.maybe_description, None);
    assert_eq!(request.ends_at.0 - request.starts_at.0, 45 * MS_PER_MINUTE);
    assert_eq!(invited_users(&request), vec![id(2), id(3)]);
    assert_eq!(request.maybe_idempotency_key, draft.maybe_idempotency_key);
  }

  #[test]
  fn an_edit_keeps_the_email_invitees_and_sends_the_time_only_when_it_moved() {
    let organizer = id(1);
    let mut before = meeting(organizer);
    before.attendees = vec![
      attendee(Some(organizer), None),
      attendee(Some(id(2)), None),
      attendee(None, Some("guest@example.com")),
    ];
    let mut draft = MeetingDraft::from_meeting(&before, organizer);
    assert_eq!(draft.members, vec![id(2)]);
    assert_eq!(draft.duration_minutes, 30);
    draft.members = vec![id(3)];
    let request = draft.to_update_request(NOW, &before).expect("valid");
    assert_eq!(request.maybe_starts_at, None);
    assert_eq!(request.maybe_recurrence, None);
    assert_eq!(
      request.maybe_invitees,
      Some(vec![
        MeetingInvitee::User { user_id: id(3) },
        MeetingInvitee::External {
          email: "guest@example.com".into(),
          maybe_name: None
        },
      ])
    );
    let (invitees, removed) = invitee_change(&before, &request).expect("invitees sent");
    assert_eq!((invitees, removed), (vec![id(3)], vec![id(2)]));
    draft.duration_minutes = 60;
    assert!(
      draft
        .to_update_request(NOW, &before)
        .unwrap()
        .maybe_ends_at
        .is_some()
    );
  }

  #[test]
  fn a_meeting_under_way_can_be_edited_but_not_moved_into_the_past() {
    let mut before = meeting(id(1));
    before.starts_at = UnixMillis(NOW - 10 * MS_PER_MINUTE);
    before.ends_at = UnixMillis(NOW + 20 * MS_PER_MINUTE);
    let mut draft = MeetingDraft::from_meeting(&before, id(1));
    draft.title = "Renamed".into();
    assert!(draft.to_update_request(NOW, &before).is_ok());
    draft.starts_at = NOW - 60 * MS_PER_MINUTE;
    assert_eq!(
      draft.to_update_request(NOW, &before),
      Err(vec![DraftProblem::StartInThePast])
    );
  }

  #[test]
  fn admitters_are_kept_for_own_meetings_that_run_or_start_within_a_day() {
    let me = id(1);
    let soon = meeting(me);
    let mut later = meeting(me);
    later.id = id(8);
    later.starts_at = UnixMillis(NOW + ADMITTERS_LEAD_MS + MS_PER_HOUR);
    let mut cancelled = meeting(me);
    cancelled.id = id(10);
    cancelled.status = MeetingStatus::Cancelled;
    let mut others = meeting(id(2));
    others.id = id(11);
    // A series is listed once per occurrence: asked for once.
    let due = admitters_due(&[soon.clone(), soon.clone(), later, cancelled, others], me, NOW);
    assert_eq!(
      due,
      vec![AdmittersDue {
        meeting: soon.id,
        conversation: soon.conversation_id,
        server_times: MeetingTimes::of(&soon),
      }]
    );
  }

  #[test]
  fn a_saved_edit_whose_removal_is_owed_is_finished_and_an_unsaved_one_forgotten() {
    let organizer = id(1);
    let mut listed = meeting(organizer);
    listed.attendees = vec![attendee(Some(organizer), None), attendee(Some(id(3)), None)];
    let edit = |removed: Vec<UserId>, noted_at| PendingEdit {
      meeting: listed.id,
      removed,
      noted_at,
    };
    let old = NOW - 10 * MS_PER_MINUTE;
    // User 2 is off the saved list: owed. User 3 is still on it: the save never happened.
    let (kept, owed) = plan_reconcile(&[edit(vec![id(2), id(3)], old)], NOW, &[listed.clone()]);
    assert!(kept.is_empty());
    assert_eq!(
      owed,
      vec![OwedChatChange {
        meeting: listed.id,
        conversation: listed.conversation_id,
        attending: vec![organizer, id(3)],
        removed: vec![id(2)],
      }]
    );
    let (kept, owed) = plan_reconcile(&[edit(vec![id(3)], old)], NOW, &[listed.clone()]);
    assert!(kept.is_empty() && owed.is_empty());
    // In flight in this session, or the meeting not listed yet: kept for later.
    let fresh = edit(vec![id(2)], NOW - 1_000);
    assert_eq!(
      plan_reconcile(std::slice::from_ref(&fresh), NOW, &[listed.clone()]).0,
      vec![fresh]
    );
    let unlisted = edit(vec![id(2)], old);
    assert_eq!(
      plan_reconcile(std::slice::from_ref(&unlisted), NOW, &[]).0,
      vec![unlisted]
    );
    // Too old: dropped.
    let ancient = edit(vec![id(2)], NOW - PENDING_EDIT_MAX_AGE_MS - 1);
    assert_eq!(plan_reconcile(&[ancient], NOW, &[]).0, Vec::new());
  }

  #[test]
  fn a_save_with_an_unknown_outcome_keeps_its_record_until_the_list_shows_the_result() {
    let organizer = id(1);
    let mut listed = meeting(organizer);
    listed.attendees = vec![attendee(Some(organizer), None), attendee(Some(id(2)), None)];
    let record = PendingEdit {
      meeting: listed.id,
      removed: vec![id(2)],
      noted_at: NOW - 30_000,
    };
    let (kept, owed) = plan_reconcile(std::slice::from_ref(&record), NOW, std::slice::from_ref(&listed));
    assert_eq!((kept.len(), owed.len()), (1, 0));
    listed.attendees.pop();
    let (kept, owed) = plan_reconcile(std::slice::from_ref(&record), NOW + 10 * MS_PER_MINUTE, &[listed]);
    assert!(kept.is_empty());
    assert_eq!(owed.len(), 1);
  }

  #[test]
  fn noting_an_edit_replaces_the_older_one_of_the_meeting_and_ignores_empty_ones() {
    let edits = noting_pending_edit(Vec::new(), id(7), vec![id(2)], NOW);
    let edits = noting_pending_edit(edits, id(7), vec![id(3)], NOW + 1);
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].removed, vec![id(3)]);
    assert_eq!(noting_pending_edit(edits.clone(), id(8), Vec::new(), NOW), edits);
  }
}
