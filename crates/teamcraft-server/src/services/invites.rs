use teamcraft_db::NewInvite;
use teamcraft_proto::{
  ConversationKind, CreateInviteRequest, InviteId, InviteResponse, ListInvitesResponse, Permission, SpaceId,
  SpaceRole,
};
use time::OffsetDateTime;

use super::access::load_space;
use super::convert::{invite_response, offset_date_time};
use super::spaces::require_member_management;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

const MAX_EMAIL_CHARS: usize = 254;
const MAX_INVITE_CHANNELS: usize = 100;

/// Invites a person by email. The invite is applied when someone signs in with that verified
/// email address (accounts::authenticate); delivering an email to the invitee is not part of M1.
pub async fn create_invite(
  state: &AppState,
  caller: &crate::auth::AuthenticatedUser,
  space: SpaceId,
  request: CreateInviteRequest,
) -> Result<InviteResponse, ApiError> {
  let access = load_space(state, caller, space).await?;
  if matches!(request.role, SpaceRole::Unknown(_)) {
    return Err(ApiError::InvalidRequest("unknown space role".into()));
  }
  require_member_management(state, caller, &request.role, &access.target())?;
  if access.view.space.is_archived {
    return Err(ApiError::Conflict("the space is archived".into()));
  }

  let email = normalize_email(&request.email)?;
  if request.channel_ids.len() > MAX_INVITE_CHANNELS {
    return Err(ApiError::InvalidRequest("too many channels".into()));
  }
  for channel in &request.channel_ids {
    let view = state.database.get_conversation(*channel, caller.user.id).await?;
    let belongs = view.is_some_and(|v| {
      v.conversation.maybe_space_id == Some(space) && v.conversation.kind == ConversationKind::Channel
    });
    if !belongs {
      return Err(ApiError::InvalidRequest(
        "channel_ids must be channels of this space".into(),
      ));
    }
  }
  let maybe_expires_at = match request.maybe_expires_at {
    Some(millis) => {
      let at =
        offset_date_time(millis).ok_or_else(|| ApiError::InvalidRequest("invalid expires_at".into()))?;
      if at <= OffsetDateTime::now_utc() {
        return Err(ApiError::InvalidRequest(
          "expires_at must be in the future".into(),
        ));
      }
      Some(at)
    },
    None => None,
  };

  let invite = state
    .database
    .create_invite(&NewInvite {
      id: state.next_id()?,
      space_id: space,
      email: &email,
      role: request.role,
      channel_ids: &request.channel_ids,
      sponsor: caller.user.id,
      maybe_expires_at,
    })
    .await?;
  Ok(invite_response(&invite))
}

pub async fn list_invites(
  state: &AppState,
  caller: &AuthenticatedUser,
  space: SpaceId,
) -> Result<ListInvitesResponse, ApiError> {
  let access = load_space(state, caller, space).await?;
  super::access::require(state, caller, Permission::ManageSpaceMembers, &access.target())?;
  let invites = state.database.list_pending_invites(space).await?;
  Ok(ListInvitesResponse {
    invites: invites.iter().map(invite_response).collect(),
  })
}

pub async fn delete_invite(
  state: &AppState,
  caller: &AuthenticatedUser,
  space: SpaceId,
  invite: InviteId,
) -> Result<(), ApiError> {
  let access = load_space(state, caller, space).await?;
  super::access::require(state, caller, Permission::ManageSpaceMembers, &access.target())?;
  let existing = state
    .database
    .get_invite(invite)
    .await?
    .filter(|i| i.space_id == space);
  if existing.is_none() {
    return Err(ApiError::invite_not_found());
  }
  state.database.delete_invite(invite).await?;
  Ok(())
}

/// Lowercases and sanity-checks an email address; real verification is the identity provider's job.
fn normalize_email(email: &str) -> Result<String, ApiError> {
  let email = email.trim().to_lowercase();
  let valid = email.len() <= MAX_EMAIL_CHARS
    && !email.chars().any(|c| c.is_whitespace() || c.is_control())
    && email.split_once('@').is_some_and(|(local, domain)| {
      !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains('@')
    });
  if valid {
    Ok(email)
  } else {
    Err(ApiError::InvalidRequest("invalid email address".into()))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn emails_are_normalized_and_checked() {
    assert_eq!(
      normalize_email("  Guest@Example.COM ").unwrap(),
      "guest@example.com"
    );
    for bad in [
      "",
      "no-at.example.com",
      "@example.com",
      "a@b",
      "a@@b.com",
      "a b@example.com",
      "a@.com",
      "a@b.com.",
    ] {
      assert!(normalize_email(bad).is_err(), "{bad:?}");
    }
  }
}
