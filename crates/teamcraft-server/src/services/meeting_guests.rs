//! Guests of meetings: personal one-time join links, guest sessions and the lobby
//! (ARCHITECTURE.md §6.4; PLAN.md §9, §12).
//!
//! A guest has no account at the identity provider. The link's token is traded for a session token
//! signed by this server; the guest then exists as an ordinary guest user (kind `guest`) and waits
//! in the lobby until a host admits them into the meeting chat. Until then they are a member of
//! nothing and can see nothing.

use teamcraft_db::{LobbyRow, StoredMeeting};
use teamcraft_proto::{
  CreateGuestLinkRequest, GatewayEvent, GuestLinkResponse, JoinMeetingRequest, JoinMeetingResponse,
  ListLobbyResponse, LobbyEntry, LobbyState, LobbyStateResponse, MeetingId, MeetingInviteId, MeetingStatus,
  UserId,
};
use time::OffsetDateTime;

use super::accounts;
use super::convert::{unix_millis, user_summary};
use super::validate_name;
use crate::auth::{AuthError, AuthenticatedUser};
use crate::error::ApiError;
use crate::meetings::MeetingService;
use crate::state::AppState;

const MAX_NAME_CHARS: usize = 100;
const MAX_LABEL_CHARS: usize = 100;

/// Trades a personal join link for a guest session and puts the guest into the lobby (or straight
/// into the meeting if it has no lobby). Needs no authentication: the token is the credential.
pub async fn join_meeting(
  state: &AppState,
  request: JoinMeetingRequest,
) -> Result<JoinMeetingResponse, ApiError> {
  let service = service_of(state)?;
  let display_name = validate_name(&request.display_name, "display_name", MAX_NAME_CHARS)?;
  let (invite_id, meeting_id) = service
    .tokens()
    .verify_invite(&request.invite_token)
    .map_err(|_| AuthError::InvalidToken("invalid join link".into()))?;
  let invite = state
    .database
    .get_meeting_invite(invite_id)
    .await?
    .filter(|invite| invite.meeting == meeting_id)
    .ok_or_else(|| AuthError::InvalidToken("invalid join link".into()))?;
  let meeting = state
    .database
    .get_meeting(meeting_id)
    .await?
    .ok_or_else(|| AuthError::InvalidToken("invalid join link".into()))?;
  let now = OffsetDateTime::now_utc();
  if meeting.status != MeetingStatus::Scheduled.as_str() {
    return Err(AuthError::InvalidToken("the meeting was cancelled".into()).into());
  }
  let expires_at = access_ends_at(service, &meeting);
  if now >= expires_at {
    return Err(AuthError::InvalidToken("the meeting is over".into()).into());
  }

  // The account exists from the first request on; make it now so the invite can name it.
  let guest = accounts::authenticate(
    state,
    accounts::guest_identity(invite_id.to_string(), display_name.clone(), now.unix_timestamp()),
  )
  .await?;
  if !state
    .database
    .redeem_meeting_invite(invite.id, guest.user.id)
    .await?
  {
    return Err(AuthError::InvalidToken("the join link was already used".into()).into());
  }
  let token = service
    .tokens()
    .sign_session(
      invite_id,
      meeting_id,
      &display_name,
      now.unix_timestamp(),
      expires_at.unix_timestamp(),
    )
    .map_err(|error| ApiError::Internal(error.to_string()))?;

  let entry = state.database.enter_lobby(meeting_id, guest.user.id).await?;
  let lobby_state = if meeting.has_lobby {
    announce_waiting(state, &meeting, &guest).await?;
    LobbyState::from(entry.state)
  } else {
    // No lobby: the guest is let in right away on behalf of the organizer.
    admit_guest(state, &meeting, guest.user.id, meeting.organizer).await?;
    LobbyState::Admitted
  };
  Ok(JoinMeetingResponse {
    guest_token: token,
    expires_at: unix_millis(expires_at),
    meeting_id,
    conversation_id: meeting.conversation,
    user_id: guest.user.id,
    lobby_state,
  })
}

/// A personal join link for somebody the host invites by hand (host only).
pub async fn create_guest_link(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: MeetingId,
  request: CreateGuestLinkRequest,
) -> Result<GuestLinkResponse, ApiError> {
  let service = service_of(state)?;
  let meeting = host_meeting(state, caller, id).await?;
  let maybe_label = request
    .maybe_label
    .as_deref()
    .map(|label| validate_name(label, "label", MAX_LABEL_CHARS))
    .transpose()?;
  let invite: MeetingInviteId = state.next_id()?;
  state
    .database
    .create_label_invite(id, invite, maybe_label.as_deref(), caller.user.id)
    .await?;
  let token = service
    .tokens()
    .sign_invite(invite, id)
    .map_err(|error| ApiError::Internal(error.to_string()))?;
  Ok(GuestLinkResponse {
    join_url: service.guest_join_url(id, &token),
    expires_at: unix_millis(access_ends_at(service, &meeting)),
  })
}

/// The guests waiting for a decision (hosts only).
pub async fn list_lobby(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: MeetingId,
) -> Result<ListLobbyResponse, ApiError> {
  let meeting = host_meeting(state, caller, id).await?;
  let waiting = state.database.list_waiting(meeting.id).await?;
  let users = state
    .database
    .get_users(&waiting.iter().map(|row| row.user).collect::<Vec<_>>())
    .await?;
  let entries = waiting
    .iter()
    .filter_map(|row| {
      users
        .iter()
        .find(|user| user.id == row.user)
        .map(|user| entry(user, row))
    })
    .collect();
  Ok(ListLobbyResponse { entries })
}

/// The caller's own place in the lobby (for guests that reconnect).
pub async fn my_lobby_state(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: MeetingId,
) -> Result<LobbyStateResponse, ApiError> {
  service_of(state)?;
  let row = state
    .database
    .get_lobby_entry(id, caller.user.id)
    .await?
    .ok_or_else(ApiError::meeting_not_found)?;
  Ok(LobbyStateResponse {
    state: LobbyState::from(row.state),
  })
}

/// Lets a waiting guest into the meeting chat (hosts only).
pub async fn admit(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: MeetingId,
  guest: UserId,
) -> Result<(), ApiError> {
  let meeting = host_meeting(state, caller, id).await?;
  admit_guest(state, &meeting, guest, caller.user.id).await
}

/// Turns a waiting guest away (hosts only); their session ends.
pub async fn deny(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: MeetingId,
  guest: UserId,
) -> Result<(), ApiError> {
  let meeting = host_meeting(state, caller, id).await?;
  if !state
    .database
    .decide_lobby(&meeting, guest, LobbyState::Denied.as_str(), caller.user.id)
    .await?
  {
    return Err(ApiError::Conflict("the guest is not waiting".into()));
  }
  state.hub.publish(
    [guest],
    &GatewayEvent::LobbyDecision {
      meeting_id: id,
      state: LobbyState::Denied,
    },
  );
  state.hub.revoke_user(guest);
  Ok(())
}

async fn admit_guest(
  state: &AppState,
  meeting: &StoredMeeting,
  guest: UserId,
  decided_by: UserId,
) -> Result<(), ApiError> {
  if !state
    .database
    .decide_lobby(meeting, guest, LobbyState::Admitted.as_str(), decided_by)
    .await?
  {
    return Err(ApiError::Conflict("the guest is not waiting".into()));
  }
  if let Some(user) = state.database.get_user(guest).await? {
    let members = state
      .database
      .conversation_member_ids(meeting.conversation)
      .await?;
    state.hub.publish(
      members,
      &GatewayEvent::MembershipAdd {
        conversation_id: meeting.conversation,
        user: user_summary(&user),
      },
    );
  }
  state.hub.publish(
    [guest],
    &GatewayEvent::LobbyDecision {
      meeting_id: meeting.id,
      state: LobbyState::Admitted,
    },
  );
  Ok(())
}

/// Tells the hosts that somebody waits.
async fn announce_waiting(
  state: &AppState,
  meeting: &StoredMeeting,
  guest: &AuthenticatedUser,
) -> Result<(), ApiError> {
  let hosts = state.database.meeting_host_ids(meeting).await?;
  state.hub.publish(
    hosts,
    &GatewayEvent::LobbyJoin {
      meeting_id: meeting.id,
      user: user_summary(&guest.user),
    },
  );
  Ok(())
}

/// The meeting, if the caller hosts it (organizer or chat admin); other members get "forbidden",
/// strangers "not found". Cancelled meetings have no lobby.
async fn host_meeting(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: MeetingId,
) -> Result<StoredMeeting, ApiError> {
  service_of(state)?;
  let meeting = state
    .database
    .get_meeting(id)
    .await?
    .ok_or_else(ApiError::meeting_not_found)?;
  let members = state
    .database
    .conversation_member_ids(meeting.conversation)
    .await?;
  if !members.contains(&caller.user.id) {
    return Err(ApiError::meeting_not_found());
  }
  if !state
    .database
    .meeting_host_ids(&meeting)
    .await?
    .contains(&caller.user.id)
  {
    return Err(ApiError::Forbidden("only hosts can do that".into()));
  }
  if meeting.status != MeetingStatus::Scheduled.as_str() {
    return Err(ApiError::Conflict("the meeting is not scheduled".into()));
  }
  Ok(meeting)
}

/// Guests may stay until the meeting's end plus the grace time.
fn access_ends_at(service: &MeetingService, meeting: &StoredMeeting) -> OffsetDateTime {
  meeting.ends_at + service.guest_grace()
}

fn entry(user: &teamcraft_db::User, row: &LobbyRow) -> LobbyEntry {
  LobbyEntry {
    user: user_summary(user),
    requested_at: unix_millis(row.requested_at),
  }
}

fn service_of(state: &AppState) -> Result<&MeetingService, ApiError> {
  state
    .maybe_meetings
    .as_deref()
    .ok_or_else(|| ApiError::Unavailable("meetings are not configured".into()))
}
