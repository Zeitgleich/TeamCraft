//! Guests of meetings: personal one-time join links, the shared guest link, guest sessions and the
//! lobby (ARCHITECTURE.md §6.4; PLAN.md §9, §12; docs/meetings.md).
//!
//! A guest has no account at the identity provider. The link's token is traded for a session token
//! signed by this server; the guest then exists as an ordinary guest user (kind `guest`) and waits
//! in the lobby until a host admits them into the meeting chat. Until then they are a member of
//! nothing and can see nothing. Members who open the meeting's link without being in its chat knock
//! the same way and wait in the same lobby.

use teamcraft_db::{LobbyRow, SharedInviteClaim, StoredMeeting, StoredMeetingInvite};
use teamcraft_proto::{
  CreateGuestLinkRequest, GatewayEvent, GuestLinkResponse, JoinMeetingRequest, JoinMeetingResponse,
  ListLobbyResponse, LobbyEntry, LobbyState, LobbyStateResponse, MeetingId, MeetingInviteId, MeetingStatus,
  UserId, UserKind,
};
use time::OffsetDateTime;

use super::access::is_guest;
use super::accounts;
use super::convert::{unix_millis, user_summary};
use super::validate_name;
use crate::auth::{AuthError, AuthenticatedUser};
use crate::error::ApiError;
use crate::meetings::MeetingService;
use crate::state::AppState;

const MAX_NAME_CHARS: usize = 100;
const MAX_LABEL_CHARS: usize = 100;

/// Trades a join link (personal or shared) for a guest session and puts the guest into the lobby
/// (or straight into the meeting if it has no lobby and the link is personal). Needs no
/// authentication: the token is the credential.
pub async fn join_meeting(
  state: &AppState,
  request: JoinMeetingRequest,
) -> Result<JoinMeetingResponse, ApiError> {
  let service = service_of(state)?;
  let display_name = validate_name(&request.display_name, "display_name", MAX_NAME_CHARS)?;
  let (link_id, meeting_id) = service
    .tokens()
    .verify_invite(&request.invite_token)
    .map_err(|_| AuthError::InvalidToken("invalid join link".into()))?;
  let link = state
    .database
    .get_meeting_invite(link_id)
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
  // A shared link hands every guest a one-time invite of their own; from here on both kinds of
  // link take the same path. Guests of a shared link always wait in the lobby.
  let invite_id = invite_for_guest(state, service, &link, &display_name).await?;
  let has_lobby = meeting.has_lobby || link.is_shared;

  let guest = match redeem_invite(state, invite_id, &display_name, now).await {
    Ok(guest) => guest,
    Err(error) => {
      if link.is_shared {
        release_claim(state, invite_id).await;
      }
      return Err(error);
    },
  };
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
  let lobby_state = if has_lobby {
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

/// The meeting's shared guest link, made on first use (host only). One link for many guests: each
/// guest who opens it still waits in the lobby, and the link ends with the meeting or when a host
/// revokes it. Asking again returns the same link.
pub async fn shared_guest_link(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: MeetingId,
) -> Result<GuestLinkResponse, ApiError> {
  let service = service_of(state)?;
  let meeting = host_meeting(state, caller, id).await?;
  let new_id: MeetingInviteId = state.next_id()?;
  let link = state
    .database
    .ensure_shared_invite(id, new_id, caller.user.id)
    .await?;
  let token = service
    .tokens()
    .sign_invite(link, id)
    .map_err(|error| ApiError::Internal(error.to_string()))?;
  Ok(GuestLinkResponse {
    join_url: service.guest_join_url(id, &token),
    expires_at: unix_millis(access_ends_at(service, &meeting)),
  })
}

/// Revokes the meeting's shared guest link (host only): nobody new gets in with it; guests already
/// waiting or admitted stay. A link asked for afterwards is a new one. Revoking twice is fine.
pub async fn revoke_shared_guest_link(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: MeetingId,
) -> Result<(), ApiError> {
  host_meeting(state, caller, id).await?;
  state.database.revoke_shared_invite(id).await?;
  Ok(())
}

/// A member with the meeting's link who is not in its chat asks to be let in and waits in the
/// lobby like a guest; members already in the chat are in. Members always wait for a host, also in
/// meetings without a lobby: meeting IDs are not secret, guest links are.
pub async fn knock(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: MeetingId,
) -> Result<LobbyStateResponse, ApiError> {
  service_of(state)?;
  if is_guest(caller) {
    return Err(ApiError::Forbidden("guests join with a guest link".into()));
  }
  let meeting = state
    .database
    .get_meeting(id)
    .await?
    .ok_or_else(ApiError::meeting_not_found)?;
  let is_member = state
    .database
    .conversation_member_ids(meeting.conversation)
    .await?
    .contains(&caller.user.id);
  let is_live =
    meeting.status == MeetingStatus::Scheduled.as_str() && OffsetDateTime::now_utc() < meeting.ends_at;
  match (is_member, is_live) {
    (true, true) => Ok(LobbyStateResponse {
      state: LobbyState::Admitted,
    }),
    (true, false) => Err(ApiError::Conflict("the meeting is over or cancelled".into())),
    // Outsiders learn nothing about a meeting that is over: the same answer as for no meeting.
    (false, false) => Err(ApiError::meeting_not_found()),
    (false, true) => {
      // Also a member admitted before and no longer in the chat waits again.
      let (entry, is_news) = state.database.knock_lobby(id, caller.user.id).await?;
      if is_news {
        announce_waiting(state, &meeting, caller).await?;
      }
      Ok(LobbyStateResponse {
        state: LobbyState::from(entry.state),
      })
    },
  }
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
        .map(|user| entry(state, user, row))
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

/// A guest leaves the meeting: their place in the lobby is closed and their account is
/// deactivated, which ends their session and connections, flags their devices removed, takes them
/// out of the chat and the call and tells the others (whose devices commit the removal from the
/// encryption group). Members have an account that stays; they leave through the chat instead.
pub async fn leave(state: &AppState, caller: &AuthenticatedUser, id: MeetingId) -> Result<(), ApiError> {
  service_of(state)?;
  if !is_guest(caller) || caller.user.kind != UserKind::Guest {
    return Err(ApiError::Forbidden("only guests leave a meeting this way".into()));
  }
  let meeting = state
    .database
    .get_meeting(id)
    .await?
    .ok_or_else(ApiError::meeting_not_found)?;
  if state
    .database
    .get_lobby_entry(id, caller.user.id)
    .await?
    .is_none()
  {
    return Err(ApiError::meeting_not_found());
  }
  // A guest who still waits must not stay in the hosts' lobby, where "admit" would let in an
  // account that is gone. Not waiting any more is fine: they were admitted or turned away.
  if state
    .database
    .decide_lobby(
      &meeting,
      caller.user.id,
      LobbyState::Denied.as_str(),
      caller.user.id,
    )
    .await?
  {
    let hosts = state.database.meeting_host_ids(&meeting).await?;
    state.hub.publish(
      hosts,
      &GatewayEvent::LobbyDecision {
        meeting_id: id,
        state: LobbyState::Denied,
      },
    );
  }
  super::admin::deactivate_user(
    state,
    Some(caller.user.id),
    caller.user.id,
    teamcraft_proto::DeactivationReason::Left,
    "left the meeting",
  )
  .await?;
  Ok(())
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
  // A member who knocked keeps their account; they are just not let in.
  let is_member_account = state
    .database
    .get_user(guest)
    .await?
    .is_some_and(|user| user.kind != UserKind::Guest);
  if is_member_account {
    return Ok(());
  }
  // The guest's session token would otherwise stay valid until the meeting ends: the account is
  // deactivated, which also ends their connections.
  super::admin::deactivate_user(
    state,
    Some(caller.user.id),
    guest,
    teamcraft_proto::DeactivationReason::Admin,
    "turned away in the meeting lobby",
  )
  .await?;
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
        user: user_summary(state, &user),
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
      user: user_summary(state, &guest.user),
    },
  );
  Ok(())
}

/// The one-time invite a guest redeems: a personal link is its own invite, a shared link makes a
/// new one for every guest (refused once revoked or full).
async fn invite_for_guest(
  state: &AppState,
  service: &MeetingService,
  link: &StoredMeetingInvite,
  display_name: &str,
) -> Result<MeetingInviteId, ApiError> {
  if !link.is_shared {
    return Ok(link.id);
  }
  let revoked = || ApiError::from(AuthError::InvalidToken("the join link was revoked".into()));
  if link.is_revoked {
    return Err(revoked());
  }
  let invite: MeetingInviteId = state.next_id()?;
  match state
    .database
    .claim_shared_invite(link.id, invite, display_name, service.max_shared_link_guests())
    .await?
  {
    SharedInviteClaim::Claimed => Ok(invite),
    SharedInviteClaim::Revoked => Err(revoked()),
    SharedInviteClaim::Full => Err(ApiError::Conflict("the join link admits no more guests".into())),
  }
}

/// Makes the guest's account and marks their invite redeemed by it. The account exists from the
/// first request on; it is made first so the invite can name it.
async fn redeem_invite(
  state: &AppState,
  invite: MeetingInviteId,
  display_name: &str,
  now: OffsetDateTime,
) -> Result<AuthenticatedUser, ApiError> {
  let guest = accounts::authenticate(
    state,
    accounts::guest_identity(invite.to_string(), display_name.to_owned(), now.unix_timestamp()),
  )
  .await?;
  if !state
    .database
    .redeem_meeting_invite(invite, guest.user.id)
    .await?
  {
    return Err(AuthError::InvalidToken("the join link was already used".into()).into());
  }
  Ok(guest)
}

/// Gives a shared link's place back when its guest did not get in, so failed joins don't use up
/// the link. A place that can't be given back stays used: logged, the join fails either way.
async fn release_claim(state: &AppState, invite: MeetingInviteId) {
  if let Err(error) = state.database.release_shared_invite_claim(invite).await {
    tracing::warn!(%error, "could not give back a shared guest link's place");
  }
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

fn entry(state: &AppState, user: &teamcraft_db::User, row: &LobbyRow) -> LobbyEntry {
  LobbyEntry {
    user: user_summary(state, user),
    requested_at: unix_millis(row.requested_at),
  }
}

fn service_of(state: &AppState) -> Result<&MeetingService, ApiError> {
  state
    .maybe_meetings
    .as_deref()
    .ok_or_else(|| ApiError::Unavailable("meetings are not configured".into()))
}
