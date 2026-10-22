use serde_json::json;
use teamcraft_db::NewInvite;
use teamcraft_proto::{
  ConversationKind, CreateInviteRequest, InviteId, InviteResponse, ListInvitesResponse, Permission, SpaceId,
  SpaceRole,
};
use time::OffsetDateTime;
use tracing::warn;

use super::access::load_space;
use super::audit::{self, Target as AuditTarget};
use super::convert::{invite_response, offset_date_time};
use super::spaces::require_member_management;
use crate::auth::AuthenticatedUser;
use crate::brand::ResolvedBrand;
use crate::error::ApiError;
use crate::meetings::ical::Method;
use crate::meetings::mailer::OutgoingMail;
use crate::state::AppState;

const MAX_EMAIL_CHARS: usize = 254;
const MAX_INVITE_CHANNELS: usize = 100;

/// Invites a person by email. The invite is applied when someone signs in with that verified
/// email address (accounts::authenticate); delivering an email to the invitee is not part of M1.
pub async fn create_invite(
  state: &AppState,
  caller: &crate::auth::AuthenticatedUser,
  brand: &ResolvedBrand,
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
  if request.role == SpaceRole::Guest {
    check_guest_domain(state, &email)?;
  }
  check_channels(state, caller, space, &request.channel_ids).await?;
  let maybe_expires_at = future_expiry(request.maybe_expires_at)?;

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
  audit::record(
    state,
    Some(caller.user.id),
    "invite_created",
    Some(AuditTarget::new("space", space)),
    &json!({"role": invite.role.as_str()}),
  )
  .await?;
  send_invite_mail(state, caller, brand, &access.view.space.name, &invite).await;
  Ok(invite_response(&invite))
}

/// The channels an invite lists must be channels of the space.
pub async fn check_channels(
  state: &AppState,
  caller: &AuthenticatedUser,
  space: SpaceId,
  channels: &[teamcraft_proto::ConversationId],
) -> Result<(), ApiError> {
  if channels.len() > MAX_INVITE_CHANNELS {
    return Err(ApiError::InvalidRequest("too many channels".into()));
  }
  for channel in channels {
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
  Ok(())
}

/// An optional expiry given by a client; it must lie in the future.
pub fn future_expiry(
  maybe_millis: Option<teamcraft_proto::UnixMillis>,
) -> Result<Option<OffsetDateTime>, ApiError> {
  let Some(millis) = maybe_millis else {
    return Ok(None);
  };
  let at = offset_date_time(millis).ok_or_else(|| ApiError::InvalidRequest("invalid expires_at".into()))?;
  if at <= OffsetDateTime::now_utc() {
    return Err(ApiError::InvalidRequest(
      "expires_at must be in the future".into(),
    ));
  }
  Ok(Some(at))
}

/// Mails the invitee, if the server has a mail server. A failure is logged and does not undo the
/// invite: it is applied at sign-in anyway.
async fn send_invite_mail(
  state: &AppState,
  caller: &AuthenticatedUser,
  request_brand: &ResolvedBrand,
  space_name: &str,
  invite: &teamcraft_db::Invite,
) {
  let Some(mailer) = &state.maybe_mailer else {
    return;
  };
  // Mail goes out as the brand of the recipient's domain, else as the brand the inviter used.
  let brand = state.brands.for_recipient(&invite.email, &request_brand.brand);
  let url = brand.base_url(&state.settings.mail.public_base_url);
  let (subject, body) = brand.invite_texts(&state.settings.mail);
  let fill = |template: &str| {
    template
      .replace("{inviter}", &caller.user.display_name)
      .replace("{space}", space_name)
      .replace("{role}", invite.role.as_str())
      .replace("{url}", &url)
  };
  let mail = OutgoingMail {
    to_address: invite.email.clone(),
    maybe_to_name: None,
    subject: fill(&subject),
    text: fill(&body),
    method: Method::Request,
    ics: String::new(),
    maybe_from_name: None,
    maybe_from_address: None,
  }
  .as_brand(brand);
  if let Err(error) = mailer.send(&mail).await {
    warn!(%error, invite_id = %invite.id, "sending the invitation e-mail failed");
  }
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

/// Guests may only be invited from the configured domains, if there are any.
pub fn check_guest_domain(state: &AppState, email: &str) -> Result<(), ApiError> {
  let allowed = &state.settings.guests.allowed_email_domains;
  if allowed.is_empty() {
    return Ok(());
  }
  let domain = email.rsplit_once('@').map_or("", |(_, domain)| domain);
  let is_allowed = allowed.iter().any(|candidate| {
    let candidate = candidate.trim_start_matches('@').to_lowercase();
    domain == candidate || domain.ends_with(&format!(".{candidate}"))
  });
  if is_allowed {
    Ok(())
  } else {
    Err(ApiError::InvalidRequest(
      "guests from this e-mail domain are not allowed".into(),
    ))
  }
}

/// Lowercases and sanity-checks an email address; real verification is the identity provider's job.
pub fn normalize_email(email: &str) -> Result<String, ApiError> {
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
