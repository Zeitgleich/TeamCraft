//! Co-hosts and handing a meeting over in the apps (#1720), as desktop and web do it
//! (`teamcraft-ui`'s host controls, docs/meetings.md "Hosts"): who hosts the meeting of a call, for
//! the badge and the participant menu, and the organizer's "Make co-host", "Remove as co-host" and
//! "Hand over host role", which the core signs (e2ee.md §4.6, "Meeting hosts"). The edits are the
//! ones desktop and web make (`teamcraft_client_api::meeting_hosts`).

use teamcraft_client_api::backend::ChatBackend as _;
use teamcraft_client_api::meeting_hosts::{
  MeetingHostsChange, MeetingRole, handing_over, making_co_host, removing_co_host, role_of,
  should_offer_handover,
};
use teamcraft_core::clock::{Clock as _, SystemClock};
use teamcraft_proto::{ConversationId, ListMeetingsResponse, MeetingResponse, UserId};

use crate::bridge::from_json;
use crate::engine::{Engine, parse_id};
use crate::error::TeamCraftError;

/// How far back the meeting list reaches for the meeting of a call (it may run past its end).
const MEETING_LOOKBACK_MS: i64 = 24 * 60 * 60_000;
const MEETING_LIST_LIMIT: u32 = 100;

/// A person's part in a meeting, for a badge ("Host", "Co-host").
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MeetingPart {
  Organizer,
  CoHost,
  Participant,
}

/// What the organizer's participant menu does to somebody.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MeetingHostAction {
  MakeCoHost,
  RemoveCoHost,
  /// Hand the meeting to them; the organizer stays co-host.
  HandOver,
}

/// Who hosts the meeting of a conversation, as the server lists it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MeetingHosts {
  pub meeting_id: String,
  pub organizer_user_id: String,
  pub co_host_user_ids: Vec<String>,
  /// The user's own part.
  pub own_part: MeetingPart,
}

impl Engine {
  /// The hosts of the meeting whose chat is `conversation_id`; `None` for a conversation of no
  /// meeting (or one the server no longer lists).
  pub(crate) async fn meeting_hosts(
    &self,
    conversation_id: &str,
  ) -> Result<Option<MeetingHosts>, TeamCraftError> {
    let conversation: ConversationId = parse_id(conversation_id)?;
    let me = self.backend()?.me().id;
    Ok(self.meeting_of(conversation).await?.map(|meeting| MeetingHosts {
      meeting_id: meeting.id.to_string(),
      organizer_user_id: meeting.organizer_user_id.to_string(),
      co_host_user_ids: meeting.co_host_user_ids.iter().map(ToString::to_string).collect(),
      own_part: part_of(role_of(&meeting, me)),
    }))
  }

  /// The organizer's menu on `user_id` in the meeting whose chat is `conversation_id`: the change
  /// of the hosts the core then signs and publishes (the organizer's devices only).
  pub(crate) async fn meeting_host_change(
    &self,
    conversation_id: &str,
    user_id: &str,
    action: MeetingHostAction,
  ) -> Result<MeetingHostsChange, TeamCraftError> {
    let conversation: ConversationId = parse_id(conversation_id)?;
    let user: UserId = parse_id(user_id)?;
    let meeting = self
      .meeting_of(conversation)
      .await?
      .ok_or_else(|| TeamCraftError::InvalidInput("the conversation is no meeting's".into()))?;
    Ok(change_for(&meeting, user, action))
  }

  /// Whether leaving the call of `conversation_id` should first offer to hand the meeting over:
  /// the user organizes it, `waiting` people wait in its lobby, nobody else hosts it and one of
  /// `candidate_ids` (the others in the call who aren't guests) could take it.
  pub(crate) async fn offers_handover(
    &self,
    conversation_id: &str,
    waiting: u32,
    candidate_ids: &[String],
  ) -> Result<bool, TeamCraftError> {
    let conversation: ConversationId = parse_id(conversation_id)?;
    let me = self.backend()?.me().id;
    let candidates = candidate_ids
      .iter()
      .map(|id| parse_id(id))
      .collect::<Result<Vec<UserId>, _>>()?;
    Ok(
      self
        .meeting_of(conversation)
        .await?
        .is_some_and(|meeting| should_offer_handover(&meeting, me, waiting as usize, &candidates)),
    )
  }

  pub(crate) async fn meeting_of(
    &self,
    conversation: ConversationId,
  ) -> Result<Option<MeetingResponse>, TeamCraftError> {
    let from = SystemClock.now_ms() - MEETING_LOOKBACK_MS;
    let path = format!("/meetings?from={from}&limit={MEETING_LIST_LIMIT}");
    let list: ListMeetingsResponse = from_json(&self.api_request("GET", &path, None).await?)?;
    Ok(
      list
        .meetings
        .into_iter()
        .find(|meeting| meeting.conversation_id == conversation),
    )
  }
}

fn change_for(meeting: &MeetingResponse, user: UserId, action: MeetingHostAction) -> MeetingHostsChange {
  match action {
    MeetingHostAction::MakeCoHost => making_co_host(meeting, user),
    MeetingHostAction::RemoveCoHost => removing_co_host(meeting, user),
    MeetingHostAction::HandOver => handing_over(meeting, user),
  }
}

fn part_of(role: MeetingRole) -> MeetingPart {
  match role {
    MeetingRole::Organizer => MeetingPart::Organizer,
    MeetingRole::CoHost => MeetingPart::CoHost,
    MeetingRole::Participant => MeetingPart::Participant,
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, MeetingKind, MeetingStatus, Snowflake, UnixMillis};

  use super::*;

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }

  #[test]
  fn the_menu_makes_the_edits_desktop_makes() {
    let meeting = MeetingResponse {
      id: id(7),
      title: "Planning".into(),
      maybe_description: None,
      starts_at: UnixMillis(0),
      ends_at: UnixMillis(1),
      kind: MeetingKind::Scheduled,
      status: MeetingStatus::Scheduled,
      has_lobby: true,
      organizer_user_id: id(1),
      conversation_id: id(70),
      maybe_space: None,
      join_url: String::new(),
      maybe_recurrence: None,
      is_occurrence_cancelled: false,
      is_occurrence_holiday: false,
      can_host_lobby: true,
      co_host_user_ids: vec![id(2)],
      attendees: Vec::new(),
      undelivered_invitations: Vec::new(),
    };
    let bob: UserId = id(2);
    let carol: UserId = id(3);
    assert_eq!(
      change_for(&meeting, carol, MeetingHostAction::MakeCoHost).co_hosts,
      vec![bob, carol]
    );
    assert!(
      change_for(&meeting, bob, MeetingHostAction::RemoveCoHost)
        .co_hosts
        .is_empty()
    );
    let handover = change_for(&meeting, carol, MeetingHostAction::HandOver);
    assert_eq!(handover.maybe_new_organizer, Some(carol));
    assert_eq!(handover.co_hosts, vec![bob, id(1)]);
    assert_eq!(part_of(role_of(&meeting, bob)), MeetingPart::CoHost);
  }
}
