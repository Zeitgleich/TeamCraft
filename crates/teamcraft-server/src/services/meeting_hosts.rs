//! Co-hosts and handing a meeting over (#1720; e2ee.md §4.6, "Meeting hosts"; docs/meetings.md):
//! a host list the organizer's identity key signs (`teamcraft_crypto::meeting_hosts`).
//!
//! The server keeps every handover and the newest list per meeting, hands them to the members of
//! the meeting chat (admitted guests too), and reads from the newest list who hosts with the
//! organizer: co-hosts admit and deny in the lobby, make guest links and use the host controls. It
//! checks the shape (signed by the key it names, for this meeting and chat), that the caller
//! organizes the meeting and signed as themselves, and that everybody named is a person in the
//! meeting chat. Whether the key is the organizer's is for guests and members to check: the
//! server's word adds no host for them. A list naming another organizer hands the meeting over:
//! the organizer changes here too (the calendar event and the invitations follow).

use serde_json::json;
use teamcraft_crypto::meeting_hosts::{MAX_HANDOVERS, SignedMeetingHosts};
use teamcraft_db::{Handover, HostListOutcome, NewHostList, StoredMeeting};
use teamcraft_proto::{
  Base64Bytes, ConversationId, GatewayEvent, Id, MeetingHostsResponse, MeetingKind, MeetingStatus,
  PutMeetingHostsRequest, Snowflake, UserId, UserKind,
};
use tracing::{info, warn};

use super::audit::{self, Target as AuditTarget};
use super::meeting_admitters::member_meeting;
use super::meetings;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Keeps the organizer's signed host list of the meeting whose chat is `conversation` (the
/// organizer only). `409` when a list with the same or a higher sequence is stored (another device
/// of the organizer was first) or the meeting was handed over too often.
pub async fn put_hosts(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  request: PutMeetingHostsRequest,
) -> Result<(), ApiError> {
  let meeting = member_meeting(state, caller, conversation).await?;
  if meeting.organizer != caller.user.id {
    return Err(ApiError::Forbidden("only the organizer names the hosts".into()));
  }
  if meeting.status != MeetingStatus::Scheduled.as_str() {
    return Err(ApiError::Conflict("the meeting is not scheduled".into()));
  }
  let bytes = request.statement.as_slice();
  let list = SignedMeetingHosts::from_bytes(bytes)
    .map_err(|error| ApiError::InvalidRequest(format!("unreadable host list: {error}")))?;
  list
    .check_shape()
    .map_err(|error| ApiError::InvalidRequest(format!("malformed host list: {error}")))?;
  if !is_same_id(list.unverified_meeting_id().get(), meeting.id) {
    return Err(ApiError::InvalidRequest(
      "the host list names another meeting".into(),
    ));
  }
  if !is_same_id(list.unverified_conversation_id().get(), conversation) {
    return Err(ApiError::InvalidRequest(
      "the host list names another conversation".into(),
    ));
  }
  if user_of(list.unverified_signer().get())? != caller.user.id {
    return Err(ApiError::InvalidRequest(
      "the host list is signed for another user".into(),
    ));
  }
  let sequence = i64::try_from(list.unverified_sequence())
    .map_err(|_| ApiError::InvalidRequest("the host list's sequence is too large".into()))?;
  let organizer = user_of(list.unverified_organizer().get())?;
  let co_hosts = list
    .unverified_co_hosts()
    .into_iter()
    .map(|user| user_of(user.get()))
    .collect::<Result<Vec<_>, _>>()?;
  let maybe_new_organizer = (organizer != caller.user.id).then_some(organizer);
  ensure_people_of_the_chat(state, &meeting, maybe_new_organizer.iter().chain(&co_hosts)).await?;
  let maybe_handover = match maybe_new_organizer {
    Some(new_organizer) => Some(Handover {
      new_organizer,
      former_attendee: state.next_id()?,
    }),
    None => None,
  };
  let co_hosts_before = state
    .database
    .meeting_co_hosts(&[meeting.id])
    .await?
    .remove(&meeting.id)
    .unwrap_or_default();
  let outcome = state
    .database
    .put_meeting_hosts(
      meeting.id,
      caller.user.id,
      &NewHostList {
        sequence,
        statement: bytes,
        co_hosts: &co_hosts,
        maybe_handover,
      },
      i64::try_from(MAX_HANDOVERS).unwrap_or(i64::MAX),
    )
    .await?;
  match outcome {
    HostListOutcome::Kept => {},
    HostListOutcome::Stale => {
      return Err(ApiError::Conflict(
        "a host list with the same or a higher sequence is stored".into(),
      ));
    },
    HostListOutcome::NotOrganizer => {
      return Err(ApiError::Forbidden("only the organizer names the hosts".into()));
    },
    HostListOutcome::TooManyHandovers => {
      return Err(ApiError::Conflict("the meeting was handed over too often".into()));
    },
  }
  let added: Vec<UserId> = co_hosts
    .iter()
    .copied()
    .filter(|user| !co_hosts_before.contains(user))
    .collect();
  let removed: Vec<UserId> = co_hosts_before
    .iter()
    .copied()
    .filter(|user| !co_hosts.contains(user))
    .collect();
  audit::record(
    state,
    Some(caller.user.id),
    if maybe_new_organizer.is_some() {
      "meeting_handover"
    } else {
      "meeting_hosts"
    },
    Some(AuditTarget::new("meeting", meeting.id)),
    &json!({
      "sequence": sequence,
      "co_hosts_added": added,
      "co_hosts_removed": removed,
      "new_organizer": maybe_new_organizer,
    }),
  )
  .await?;
  info!(meeting_id = %meeting.id, sequence, is_handover = maybe_new_organizer.is_some(), "meeting hosts published");
  let mut audience = state.database.meeting_user_ids(&meeting).await?;
  if maybe_new_organizer.is_some() {
    // The chat names its organizer: its members read it again (and with it who the founder of the
    // chat's group handed the meeting to, e2ee.md §4.6).
    let members = state
      .database
      .conversation_member_ids(meeting.conversation)
      .await?;
    state.hub.publish(
      members,
      &GatewayEvent::ConversationUpdate {
        conversation_id: meeting.conversation,
      },
    );
    after_handover(state, &meeting).await;
  }
  audience.extend(co_hosts_before);
  audience.extend(&co_hosts);
  audience.sort_unstable();
  audience.dedup();
  state.hub.publish(
    audience,
    &GatewayEvent::MeetingUpdate {
      meeting_id: meeting.id,
    },
  );
  Ok(())
}

/// The host lists of the meeting whose chat is `conversation`: every handover and the newest list,
/// oldest first (members of the chat, admitted guests too). Empty before the first.
pub async fn hosts(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
) -> Result<MeetingHostsResponse, ApiError> {
  let meeting = member_meeting(state, caller, conversation).await?;
  let statements = state
    .database
    .meeting_host_lists(meeting.id)
    .await?
    .into_iter()
    .map(Base64Bytes::new)
    .collect();
  Ok(MeetingHostsResponse { statements })
}

/// A handed-over meeting's calendar event names the new organizer (with a higher `SEQUENCE`, which
/// the database increased) and the invitees get the update. A calendar or mailer that is down
/// doesn't undo the handover: logged, the next change of the meeting writes the event again.
async fn after_handover(state: &AppState, before: &StoredMeeting) {
  let Some(service) = state.maybe_meetings.as_deref() else {
    return;
  };
  let meeting = match state.database.get_meeting(before.id).await {
    Ok(Some(meeting)) => meeting,
    Ok(None) => return,
    Err(error) => {
      warn!(%error, meeting_id = %before.id, "could not read the handed-over meeting");
      return;
    },
  };
  if meeting.kind != MeetingKind::Scheduled.as_str() {
    return;
  }
  if let Err(error) = meetings::sync_calendar(state, service, &meeting).await {
    warn!(%error, meeting_id = %meeting.id, "writing the handed-over meeting's calendar event failed");
  }
  match state.database.list_attendees(meeting.id).await {
    Ok(attendees) => {
      if let Err(error) = meetings::send_update_mails(state, service, &meeting, &attendees).await {
        warn!(%error, meeting_id = %meeting.id, "sending the handed-over meeting's update failed");
      }
    },
    Err(error) => warn!(%error, meeting_id = %meeting.id, "could not read the invitees"),
  }
}

/// Everybody a list names besides the signer must be a person (no guest, no bot, not deactivated)
/// who is in the meeting chat: a host lets people in and is let in by nobody.
async fn ensure_people_of_the_chat(
  state: &AppState,
  meeting: &StoredMeeting,
  named: impl Iterator<Item = &UserId>,
) -> Result<(), ApiError> {
  let named: Vec<UserId> = named.copied().collect();
  if named.is_empty() {
    return Ok(());
  }
  let members = state
    .database
    .conversation_member_ids(meeting.conversation)
    .await?;
  let users = state.database.get_users(&named).await?;
  for user in &named {
    let is_person = users
      .iter()
      .find(|known| known.id == *user)
      .is_some_and(|known| known.kind == UserKind::Member && !known.is_deactivated);
    if !is_person || !members.contains(user) {
      return Err(ApiError::InvalidRequest(format!(
        "{user} is not a person in the meeting chat"
      )));
    }
  }
  Ok(())
}

/// A user ID inside a signed list (unsigned 64 bit) as the server's.
fn user_of(raw: u64) -> Result<UserId, ApiError> {
  i64::try_from(raw)
    .ok()
    .and_then(|raw| Snowflake::new(raw).ok())
    .map(UserId::from_snowflake)
    .ok_or_else(|| ApiError::InvalidRequest("the host list names an invalid user".into()))
}

/// Whether an ID inside a signed list (unsigned 64 bit) is `id`.
fn is_same_id(raw: u64, id: impl Id) -> bool {
  i64::try_from(raw).is_ok_and(|raw| raw == id.snowflake().get())
}
