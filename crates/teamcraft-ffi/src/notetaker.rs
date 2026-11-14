//! The notetaker before and during a call in the apps, as desktop and web have it
//! (`teamcraft-ui`'s `call_notetaker`, e2ee.md §8.2): who is in a running call before joining it,
//! for the pre-join notice, and "Add notetaker" where it is no member of the conversation yet. In
//! a channel whose members the user manages and in a meeting the user organizes, it is added there
//! and the call stays where it is; anywhere else the call moves into a room with it. The route is
//! the one desktop and web choose (`teamcraft_client_api::notetaker::add_route`).

use teamcraft_client_api::backend::ChatBackend as _;
use teamcraft_client_api::meeting_invitees::{adding_invitee, saved_change};
use teamcraft_client_api::meeting_schedule::MeetingTimes;
use teamcraft_client_api::model::ConversationKind;
use teamcraft_client_api::notetaker::{
  AddRoute, ConversationRights, add_route, conversation_rights, may_ask_in, moved_call_hint,
};
use teamcraft_core::CoreError;
use teamcraft_core::api::ServerApi as _;
use teamcraft_core::clock::{Clock as _, SystemClock};
use teamcraft_core::meeting_invitees::InviteeList;
use teamcraft_proto::{
  CallStateResponse, ConversationId, HistoryPolicy, ListMeetingsResponse, MeetingResponse, UserId,
};

use crate::bridge::from_json;
use crate::engine::{Engine, parse_id};
use crate::error::TeamCraftError;

/// How far back the meeting list reaches for the meeting of a running call: meetings that ended
/// after this (a call may run past its meeting's end).
const MEETING_LOOKBACK_MS: i64 = 24 * 60 * 60_000;
/// The most meetings read for it.
const MEETING_LIST_LIMIT: u32 = 100;

/// What "Add notetaker" did where the call is ([`crate::TeamCraft::bring_in_notetaker`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum NotetakerAddition {
  /// The notetaker is a member of the conversation (it was, or the user just added it to the
  /// channel or as an invitee of the meeting) and was asked into its call, which stays where it is.
  AskedIn,
  /// The user can't add it to the conversation (a DM or group DM, a channel whose members somebody
  /// else manages, a meeting somebody else organizes): nothing happened yet. The call moves into a
  /// room with it next (`add_people_to_call` with the notetaker as the added one).
  MoveToRoom,
}

/// Where the call of a conversation went, for somebody who comes to it late
/// ([`crate::TeamCraft::moved_call_hint`]).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MovedCallHint {
  /// The meeting room the call moved into, which the user is a member of.
  pub room_id: String,
  /// The room's name as the chat list shows it.
  pub room_name: String,
  /// Who moved the call.
  pub by_user_id: String,
}

impl Engine {
  /// The room the call of `conversation_id` moved into, if it did, the user is in the room and the
  /// room's call runs with others in it now (issue #1321); `None` otherwise. Never starts a call.
  pub(crate) async fn moved_call_hint(
    &self,
    conversation_id: &str,
  ) -> Result<Option<MovedCallHint>, TeamCraftError> {
    let conversation: ConversationId = parse_id(conversation_id)?;
    let backend = self.backend()?;
    let me = backend.me().id;
    let Some(moved) = backend
      .moved_call(conversation)
      .filter(|moved| !moved.room_name.is_empty())
    else {
      return Ok(None);
    };
    let maybe_call = self
      .api()
      .running_call(moved.room)
      .await
      .map_err(CoreError::from)?;
    let room_has_others = !others_in_call(maybe_call.as_ref(), me).is_empty();
    Ok(
      moved_call_hint(Some(moved), room_has_others, SystemClock.now_ms()).map(|moved| MovedCallHint {
        room_id: moved.room.to_string(),
        room_name: moved.room_name,
        by_user_id: moved.by.to_string(),
      }),
    )
  }

  /// The others in the call running in `conversation_id`, as the server's roster lists them; empty
  /// when none runs. Never starts a call.
  pub(crate) async fn call_participants(&self, conversation_id: &str) -> Result<Vec<String>, TeamCraftError> {
    let conversation: ConversationId = parse_id(conversation_id)?;
    let me = self.backend()?.me().id;
    let maybe_call = self
      .api()
      .running_call(conversation)
      .await
      .map_err(CoreError::from)?;
    Ok(
      others_in_call(maybe_call.as_ref(), me)
        .iter()
        .map(ToString::to_string)
        .collect(),
    )
  }

  /// Adds the notetaker to the conversation of `conversation_id` where the user can (desktop's
  /// route): `true` once it is a member, `false` where the call has to move instead. Its membership
  /// commit is through when this returns, so asking it in reaches a group it is in.
  pub(crate) async fn add_notetaker(&mut self, conversation_id: &str) -> Result<bool, TeamCraftError> {
    let conversation: ConversationId = parse_id(conversation_id)?;
    let (bot, me) = self.notetaker_bot()?;
    if self.is_member(conversation, bot)? {
      return Ok(true);
    }
    let maybe_meeting = if self.may_be_meeting_chat(conversation)? {
      self.organized_meeting(conversation, me).await?
    } else {
      None
    };
    match self.route(conversation, maybe_meeting.as_ref())? {
      Some(AddRoute::Channel { joins_space }) => {
        // A bot never reads what was written before it was added.
        self
          .with_client(move |client| {
            Box::pin(async move {
              client
                .add_people_to_channel(conversation, &[(bot, true)], HistoryPolicy::FromJoin, joins_space)
                .await
            })
          })
          .await?;
        Ok(true)
      },
      Some(AddRoute::Meeting(_)) => {
        let before = maybe_meeting.ok_or_else(|| TeamCraftError::InvalidInput("no such meeting".into()))?;
        self.invite_to_meeting(before, me, bot).await?;
        Ok(true)
      },
      Some(AddRoute::MoveToRoom) | None => Ok(false),
    }
  }

  /// The notetaker of the verified policy, if the policy lets the user ask it in, and the user.
  fn notetaker_bot(&self) -> Result<(UserId, UserId), TeamCraftError> {
    let backend = self.backend()?;
    let me = backend.me();
    let policy = backend
      .notetaker_policy()
      .ok_or_else(|| TeamCraftError::InvalidInput("there is no notetaker".into()))?;
    let Some(bot) = policy.active_bot() else {
      return Err(TeamCraftError::InvalidInput("the notetaker is off".into()));
    };
    if !may_ask_in(
      &policy,
      !me.is_guest && !me.is_bot,
      backend.can_manage_integration_sources(),
    ) {
      return Err(TeamCraftError::InvalidInput(
        "the policy doesn't let you ask the notetaker in".into(),
      ));
    }
    Ok((bot, me.id))
  }

  /// Whether the conversation can be a meeting chat, which the meeting list then tells: not a
  /// one-to-one DM, not a channel of a space.
  fn may_be_meeting_chat(&self, conversation: ConversationId) -> Result<bool, TeamCraftError> {
    Ok(
      self
        .backend()?
        .conversation(conversation)
        .is_none_or(|found| found.kind != ConversationKind::Direct && !found.has_channel_settings()),
    )
  }

  fn is_member(&self, conversation: ConversationId, bot: UserId) -> Result<bool, TeamCraftError> {
    Ok(
      self
        .backend()?
        .conversation(conversation)
        .is_some_and(|found| found.member_ids.contains(&bot)),
    )
  }

  /// Where the user brings the notetaker into the conversation, where it is no member:
  /// `maybe_meeting`, the meeting the user organizes whose chat it is.
  fn route(
    &self,
    conversation: ConversationId,
    maybe_meeting: Option<&MeetingResponse>,
  ) -> Result<Option<AddRoute>, TeamCraftError> {
    let backend = self.backend()?;
    let maybe_organized = maybe_meeting.map(|meeting| meeting.id);
    let rights = match backend.conversation(conversation) {
      Some(found) => conversation_rights(
        &found,
        maybe_organized,
        found.can_add_members(),
        &backend.spaces(),
      ),
      None => maybe_organized.map_or(
        ConversationRights::CannotAdd,
        ConversationRights::OrganizesMeeting,
      ),
    };
    // The apps move calls of every kind.
    Ok(add_route(rights, true))
  }

  /// The meeting `me` organizes whose chat is `conversation`, from the server's meeting list.
  async fn organized_meeting(
    &self,
    conversation: ConversationId,
    me: UserId,
  ) -> Result<Option<MeetingResponse>, TeamCraftError> {
    let from = SystemClock.now_ms() - MEETING_LOOKBACK_MS;
    let path = format!("/meetings?from={from}&limit={MEETING_LIST_LIMIT}");
    let list: ListMeetingsResponse = from_json(&self.api_request("GET", &path, None).await?)?;
    Ok(organized_meeting_of(list.meetings, conversation, me))
  }

  /// Adds `bot` to the invitees of the meeting `me` organizes and to the meeting chat's group, as
  /// desktop saves an edit of the meeting (`save_meeting_edit`).
  async fn invite_to_meeting(
    &mut self,
    before: MeetingResponse,
    me: UserId,
    bot: UserId,
  ) -> Result<(), TeamCraftError> {
    let path = format!("/meetings/{}", before.id);
    // Diffed against the meeting as the server has it now, not the possibly stale list.
    let before: MeetingResponse = from_json(&self.api_request("GET", &path, None).await?)?;
    let request = adding_invitee(&before, me, bot);
    let body =
      serde_json::to_string(&request).map_err(|error| TeamCraftError::InvalidInput(error.to_string()))?;
    let saved: MeetingResponse = from_json(&self.api_request("PATCH", &path, Some(body)).await?)?;
    // The bot is the organizer's own addition; the rest is carried over from the server's list.
    let (invitees, removed) = saved_change(&before, &saved, vec![bot]);
    let (meeting, chat) = (saved.id, saved.conversation_id);
    let times = MeetingTimes::of(&saved);
    self
      .with_client(move |client| {
        Box::pin(async move {
          client
            .update_meeting_chat_members_from(chat, Some(meeting), &InviteeList::from(invitees), &removed)
            .await?;
          // Who may let guests in changed with the group; a failure here leaves the old statement,
          // which the next edit or start renews.
          if let Err(error) = client
            .sync_scheduled_meeting_admitters(meeting, chat, times)
            .await
          {
            tracing::warn!(%error, "the meeting's admitters could not be renewed");
          }
          Ok(())
        })
      })
      .await
  }
}

/// The others in `maybe_call`, each once (a person may be in it on several devices).
fn others_in_call(maybe_call: Option<&CallStateResponse>, me: UserId) -> Vec<UserId> {
  let mut others: Vec<UserId> = maybe_call
    .map(|call| call.participants.iter().map(|entry| entry.user_id).collect())
    .unwrap_or_default();
  others.retain(|user| *user != me);
  others.sort_unstable();
  others.dedup();
  others
}

/// The meeting of `meetings` whose chat is `conversation` and which `me` organizes.
fn organized_meeting_of(
  meetings: Vec<MeetingResponse>,
  conversation: ConversationId,
  me: UserId,
) -> Option<MeetingResponse> {
  meetings
    .into_iter()
    .find(|meeting| meeting.conversation_id == conversation && meeting.organizer_user_id == me)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{CallRosterEntry, Id, Snowflake};

  use super::*;

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }

  #[test]
  fn the_pre_join_roster_names_each_other_person_once() {
    let me: UserId = id(1);
    let call = CallStateResponse {
      call_id: id(50),
      conversation_id: id(7),
      started_by: id(2),
      started_at: teamcraft_proto::UnixMillis(0),
      is_ringing: false,
      participants: [(2, 20), (1, 10), (9, 90), (2, 21)]
        .into_iter()
        .map(|(user, device)| CallRosterEntry {
          user_id: id(user),
          device_id: id(device),
        })
        .collect(),
    };
    assert_eq!(others_in_call(Some(&call), me), vec![id(2), id(9)]);
    assert!(others_in_call(None, me).is_empty(), "no call runs");
  }
}
