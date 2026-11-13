//! Edits of a meeting's invitee list, as the organizer's client sends them: the people added during
//! its call (#1141) and the notetaker (e2ee.md §8.2). Desktop and web (`teamcraft-ui`) and the
//! mobile apps (through teamcraft-ffi) build the same edit.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{MeetingInvitee, MeetingResponse, UpdateMeetingRequest};

use crate::model::UserId;

/// How long after a meeting's end the server keeps a guest's session (`guest_grace_secs`, default
/// two hours): the host's device removes the guest from the encryption group then at the latest,
/// and the organizer's signed admitters end then.
pub const GUEST_ACCESS_GRACE_MS: i64 = 2 * 60 * 60_000;

/// A meeting's invitees as the organizer's device hands them to the core for the meeting chat's
/// group (#1726, e2ee.md §4.6.5, "Meetings (D10)"), by where each user comes from: the server's
/// list adds only users the organizer's signed list names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ChatInvitees {
  /// A decision of the organizer's on this device: `added`, the users chosen just now (a create
  /// request, the users an edit adds); `kept`, users carried over from the server's list.
  Chosen { added: Vec<UserId>, kept: Vec<UserId> },
  /// The meeting's attendees as the server lists them (a Join, an edit finished after a restart).
  Listed { listed: Vec<UserId> },
}

impl ChatInvitees {
  /// The users the organizer invited in a request this device just sent.
  pub fn chosen(added: Vec<UserId>) -> Self {
    Self::Chosen {
      added,
      kept: Vec::new(),
    }
  }

  /// The attendees as the server lists them.
  pub fn listed(listed: Vec<UserId>) -> Self {
    Self::Listed { listed }
  }
}

/// The TeamCraft users a saved meeting lists as attendees (externals have no account). The
/// organizer's own device sets the meeting chat's group up from this as it knows the meeting.
pub fn attending_users(meeting: &MeetingResponse) -> Vec<UserId> {
  meeting
    .attendees
    .iter()
    .filter_map(|attendee| attendee.maybe_user_id)
    .collect()
}

/// The edit that adds `user` to the invitees of the meeting `me` organizes and changes nothing
/// else: the notetaker added during one of its calls (e2ee.md §8.2). The invitees go as a whole
/// list, as the meeting has them now.
pub fn adding_invitee(meeting: &MeetingResponse, me: UserId, user: UserId) -> UpdateMeetingRequest {
  adding_invitees(meeting, me, &[user])
}

/// The edit that adds `users` to the invitees of the meeting `me` organizes, as the people the
/// organizer adds during its call (#1141); the organizer and people invited already are no
/// additions.
pub fn adding_invitees(meeting: &MeetingResponse, me: UserId, users: &[UserId]) -> UpdateMeetingRequest {
  let mut invitees: Vec<MeetingInvitee> = meeting
    .attendees
    .iter()
    .filter_map(|attendee| attendee.maybe_user_id)
    .filter(|member| *member != me && !users.contains(member))
    .map(|user_id| MeetingInvitee::User { user_id })
    .collect();
  let mut added: Vec<UserId> = Vec::new();
  for user in users {
    if *user != me && !added.contains(user) {
      added.push(*user);
    }
  }
  invitees.extend(added.into_iter().map(|user_id| MeetingInvitee::User { user_id }));
  invitees.extend(meeting.attendees.iter().filter_map(|attendee| {
    match (attendee.maybe_user_id, &attendee.maybe_email) {
      (None, Some(email)) => Some(MeetingInvitee::External {
        email: email.clone(),
        maybe_name: attendee.maybe_name.clone().filter(|name| !name.is_empty()),
      }),
      _ => None,
    }
  }));
  UpdateMeetingRequest {
    maybe_invitees: Some(invitees),
    ..UpdateMeetingRequest::default()
  }
}

/// The users an edit adds: in `request`'s invitees and on none of `seen`, the meeting as this
/// device knew it before the save (the list on screen, the one read again just before). A user the
/// server listed before counts as carried over, not as the organizer's choice (#1726).
pub fn added_users(request: &UpdateMeetingRequest, seen: &[&MeetingResponse]) -> Vec<UserId> {
  let Some(invitees) = request.maybe_invitees.as_ref() else {
    return Vec::new();
  };
  let listed_before: Vec<UserId> = seen.iter().flat_map(|meeting| attending_users(meeting)).collect();
  let mut added = Vec::new();
  for invitee in invitees {
    if let MeetingInvitee::User { user_id } = invitee
      && !listed_before.contains(user_id)
      && !added.contains(user_id)
    {
      added.push(*user_id);
    }
  }
  added
}

/// From the server's answer to a save: the invitee list as saved, with the users the organizer
/// `added` as its own choice and the others carried over, and the users who were on `before` and
/// are not on it any more, for the meeting chat's group.
pub fn saved_change(
  before: &MeetingResponse,
  saved: &MeetingResponse,
  added: Vec<UserId>,
) -> (ChatInvitees, Vec<UserId>) {
  let now = attending_users(saved);
  let removed = attending_users(before)
    .into_iter()
    .filter(|user| !now.contains(user) && *user != before.organizer_user_id)
    .collect();
  let added: Vec<UserId> = added.into_iter().filter(|user| now.contains(user)).collect();
  let kept = now.into_iter().filter(|user| !added.contains(user)).collect();
  (ChatInvitees::Chosen { added, kept }, removed)
}
