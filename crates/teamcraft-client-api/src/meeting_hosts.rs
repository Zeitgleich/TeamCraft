//! Co-hosts and handing a meeting over (#1720; docs/meetings.md, "Hosts"): who hosts a meeting as
//! the server lists it, the edits the organizer's menu makes, and when to offer a handover. Desktop
//! and web (`teamcraft-ui`) and the mobile apps (through teamcraft-ffi) decide alike. The signing
//! is the core's (`Client::change_meeting_hosts`); this is what the buttons ask it for.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{MeetingId, MeetingResponse, MeetingStatus};

use crate::model::{ConversationId, UserId};

/// A user's part in a meeting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MeetingRole {
  Organizer,
  CoHost,
  Participant,
}

/// A change of a meeting's hosts the organizer's device signs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingHostsChange {
  pub meeting: MeetingId,
  pub conversation: ConversationId,
  /// Everybody who hosts besides the organizer afterwards.
  pub co_hosts: Vec<UserId>,
  /// Hands the meeting to this member of its chat.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub maybe_new_organizer: Option<UserId>,
}

/// How the last change of a meeting's hosts on this device went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HostsChangeState {
  Saving,
  Saved,
  Failed { reason: String },
}

/// A meeting where the user's part changed, for a notice ("You are now a co-host of …").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostingNews {
  pub meeting: MeetingId,
  pub title: String,
  pub role: MeetingRole,
}

/// `user`'s part in `meeting` as the server lists it.
pub fn role_of(meeting: &MeetingResponse, user: UserId) -> MeetingRole {
  if meeting.organizer_user_id == user {
    MeetingRole::Organizer
  } else if meeting.co_host_user_ids.contains(&user) {
    MeetingRole::CoHost
  } else {
    MeetingRole::Participant
  }
}

/// Whether `user` hosts the meeting: the organizer or a co-host.
pub fn is_host(meeting: &MeetingResponse, user: UserId) -> bool {
  role_of(meeting, user) != MeetingRole::Participant
}

/// The organizer `me` makes `user` a co-host.
pub fn making_co_host(meeting: &MeetingResponse, user: UserId) -> MeetingHostsChange {
  let mut co_hosts = meeting.co_host_user_ids.clone();
  if user != meeting.organizer_user_id && !co_hosts.contains(&user) {
    co_hosts.push(user);
  }
  change(meeting, co_hosts, None)
}

/// The organizer `me` takes `user` off the co-hosts.
pub fn removing_co_host(meeting: &MeetingResponse, user: UserId) -> MeetingHostsChange {
  let co_hosts = meeting
    .co_host_user_ids
    .iter()
    .copied()
    .filter(|co_host| *co_host != user)
    .collect();
  change(meeting, co_hosts, None)
}

/// The organizer hands the meeting to `new_organizer` and stays a co-host; the other co-hosts stay.
pub fn handing_over(meeting: &MeetingResponse, new_organizer: UserId) -> MeetingHostsChange {
  let mut co_hosts: Vec<UserId> = meeting
    .co_host_user_ids
    .iter()
    .copied()
    .filter(|co_host| *co_host != new_organizer)
    .collect();
  if !co_hosts.contains(&meeting.organizer_user_id) {
    co_hosts.push(meeting.organizer_user_id);
  }
  change(meeting, co_hosts, Some(new_organizer))
}

/// The co-hosts the scheduling dialog sets for a saved meeting: the picked people who are invited
/// users (an external address can't host), the organizer left out. `None` when nothing changes.
pub fn co_hosts_from_dialog(meeting: &MeetingResponse, picked: &[UserId]) -> Option<MeetingHostsChange> {
  let invited: Vec<UserId> = meeting
    .attendees
    .iter()
    .filter_map(|attendee| attendee.maybe_user_id)
    .collect();
  let mut co_hosts: Vec<UserId> = Vec::new();
  for user in picked {
    if *user != meeting.organizer_user_id && invited.contains(user) && !co_hosts.contains(user) {
      co_hosts.push(*user);
    }
  }
  let mut before = meeting.co_host_user_ids.clone();
  before.sort_unstable();
  let mut after = co_hosts.clone();
  after.sort_unstable();
  (before != after).then(|| change(meeting, co_hosts, None))
}

/// Whether the organizer leaving the meeting's call should be asked to hand it over: people wait
/// in its lobby, nobody else hosts it, and somebody who could take it over (`candidates`: the other
/// people in the call who aren't guests) is there.
pub fn should_offer_handover(
  meeting: &MeetingResponse,
  me: UserId,
  waiting: usize,
  candidates: &[UserId],
) -> bool {
  meeting.organizer_user_id == me
    && meeting.status == MeetingStatus::Scheduled
    && waiting > 0
    && meeting.co_host_user_ids.is_empty()
    && candidates.iter().any(|user| *user != me)
}

/// The meetings in which `me` became the organizer or a co-host between two lists (the meetings
/// reloaded after a `meeting_update`): a notice for each. Meetings not in `before` are new to the
/// list and say nothing.
pub fn hosting_news(before: &[MeetingResponse], after: &[MeetingResponse], me: UserId) -> Vec<HostingNews> {
  let mut news: Vec<HostingNews> = Vec::new();
  for meeting in after {
    let Some(earlier) = before.iter().find(|earlier| earlier.id == meeting.id) else {
      continue;
    };
    let (was, is) = (role_of(earlier, me), role_of(meeting, me));
    let is_promotion = matches!(
      (was, is),
      (
        MeetingRole::Participant,
        MeetingRole::CoHost | MeetingRole::Organizer
      ) | (MeetingRole::CoHost, MeetingRole::Organizer)
    );
    if is_promotion && !news.iter().any(|known| known.meeting == meeting.id) {
      news.push(HostingNews {
        meeting: meeting.id,
        title: meeting.title.clone(),
        role: is,
      });
    }
  }
  news
}

fn change(
  meeting: &MeetingResponse,
  co_hosts: Vec<UserId>,
  maybe_new_organizer: Option<UserId>,
) -> MeetingHostsChange {
  MeetingHostsChange {
    meeting: meeting.id,
    conversation: meeting.conversation_id,
    co_hosts,
    maybe_new_organizer,
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{MeetingAttendeeResponse, MeetingKind, RsvpStatus, UnixMillis};

  use super::*;
  use crate::model::id_from_raw;

  fn user(raw: u64) -> UserId {
    id_from_raw(raw)
  }

  fn meeting(organizer: u64, co_hosts: &[u64], invited: &[u64]) -> MeetingResponse {
    MeetingResponse {
      id: id_from_raw(7),
      title: "Weekly".into(),
      maybe_description: None,
      starts_at: UnixMillis(0),
      ends_at: UnixMillis(1),
      kind: MeetingKind::Scheduled,
      status: MeetingStatus::Scheduled,
      has_lobby: true,
      organizer_user_id: user(organizer),
      conversation_id: id_from_raw(70),
      maybe_space: None,
      join_url: String::new(),
      maybe_recurrence: None,
      is_occurrence_cancelled: false,
      is_occurrence_holiday: false,
      can_host_lobby: false,
      co_host_user_ids: co_hosts.iter().map(|raw| user(*raw)).collect(),
      attendees: invited
        .iter()
        .map(|raw| MeetingAttendeeResponse {
          maybe_user_id: Some(user(*raw)),
          maybe_email: None,
          maybe_name: None,
          rsvp: RsvpStatus::Accepted,
        })
        .collect(),
      undelivered_invitations: Vec::new(),
    }
  }

  #[test]
  fn roles_follow_the_list() {
    let weekly = meeting(1, &[2], &[2, 3]);
    assert_eq!(role_of(&weekly, user(1)), MeetingRole::Organizer);
    assert_eq!(role_of(&weekly, user(2)), MeetingRole::CoHost);
    assert_eq!(role_of(&weekly, user(3)), MeetingRole::Participant);
    assert!(is_host(&weekly, user(2)));
    assert!(!is_host(&weekly, user(3)));
  }

  #[test]
  fn the_menu_adds_and_takes_off_co_hosts() {
    let weekly = meeting(1, &[2], &[2, 3]);
    assert_eq!(making_co_host(&weekly, user(3)).co_hosts, vec![user(2), user(3)]);
    assert_eq!(making_co_host(&weekly, user(2)).co_hosts, vec![user(2)], "once");
    assert_eq!(
      making_co_host(&weekly, user(1)).co_hosts,
      vec![user(2)],
      "never the organizer"
    );
    assert!(removing_co_host(&weekly, user(2)).co_hosts.is_empty());
  }

  #[test]
  fn a_handover_keeps_the_former_organizer_as_co_host() {
    let weekly = meeting(1, &[2, 3], &[2, 3]);
    let change = handing_over(&weekly, user(3));
    assert_eq!(change.maybe_new_organizer, Some(user(3)));
    assert_eq!(change.co_hosts, vec![user(2), user(1)]);
  }

  #[test]
  fn the_dialog_names_invited_users_only() {
    let weekly = meeting(1, &[], &[2, 3]);
    let change = co_hosts_from_dialog(&weekly, &[2, 9, 1, 2].map(user)).unwrap();
    assert_eq!(change.co_hosts, vec![user(2)]);
    let same = meeting(1, &[2], &[2, 3]);
    assert!(
      co_hosts_from_dialog(&same, &[user(2)]).is_none(),
      "nothing changed"
    );
    assert!(co_hosts_from_dialog(&same, &[]).is_some(), "taken off");
  }

  #[test]
  fn the_organizer_is_asked_only_with_people_waiting_and_nobody_else_hosting() {
    let weekly = meeting(1, &[], &[2]);
    assert!(should_offer_handover(&weekly, user(1), 1, &[user(2)]));
    assert!(
      !should_offer_handover(&weekly, user(1), 0, &[user(2)]),
      "nobody waits"
    );
    assert!(
      !should_offer_handover(&weekly, user(1), 1, &[user(1)]),
      "nobody to take it"
    );
    assert!(
      !should_offer_handover(&weekly, user(2), 1, &[user(1)]),
      "not the organizer"
    );
    let cohosted = meeting(1, &[2], &[2]);
    assert!(
      !should_offer_handover(&cohosted, user(1), 1, &[user(2)]),
      "a co-host stays"
    );
  }

  #[test]
  fn news_tells_of_promotions_only() {
    let before = vec![meeting(1, &[], &[2, 3])];
    let after = vec![meeting(1, &[2], &[2, 3])];
    let news = hosting_news(&before, &after, user(2));
    assert_eq!(news.len(), 1);
    assert_eq!(news[0].role, MeetingRole::CoHost);
    assert!(hosting_news(&before, &after, user(3)).is_empty());
    assert!(
      hosting_news(&after, &before, user(2)).is_empty(),
      "a demotion is no news"
    );
    let handed = vec![meeting(2, &[1], &[1, 3])];
    assert_eq!(
      hosting_news(&after, &handed, user(2))[0].role,
      MeetingRole::Organizer
    );
    assert!(hosting_news(&[], &after, user(2)).is_empty(), "new to the list");
  }
}
