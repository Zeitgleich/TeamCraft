use teamcraft_db::{AppliedInvite, BOT_ISSUER, OidcIdentity};
use teamcraft_proto::{GatewayEvent, Role, UserKind};
use tracing::info;

use super::bots;
use crate::auth::{AuthError, AuthenticatedUser, TokenIdentity};
use crate::error::ApiError;
use crate::meetings::guest;
use crate::state::AppState;

/// Validates a bearer token: meeting guests carry tokens signed by this server, everybody else a
/// token of the identity provider.
pub async fn validate_token(state: &AppState, token: &str) -> Result<TokenIdentity, AuthError> {
  if token.starts_with(bots::TOKEN_PREFIX) {
    let bot = bots::find_by_token(state, token)
      .await
      .map_err(|error| AuthError::KeysUnavailable(error.to_string()))?
      .ok_or_else(|| AuthError::InvalidToken("unknown or revoked bot token".into()))?;
    return Ok(bot_identity(&bot));
  }
  if let Some(identity) = super::local_accounts::validate_session_token(state, token).await? {
    return Ok(identity);
  }
  if let Some(meetings) = &state.maybe_meetings
    && guest::is_guest_token_header(token)
  {
    let claims = meetings
      .tokens()
      .verify_session(token)
      .map_err(|_| AuthError::InvalidToken("invalid guest session".into()))?;
    return Ok(guest_identity(claims.sub, claims.name, claims.iat));
  }
  state.validator.validate(token).await
}

/// The identity of a bot: a guest-level account under the server's own bot issuer.
fn bot_identity(bot: &teamcraft_db::StoredBot) -> TokenIdentity {
  TokenIdentity {
    issuer: BOT_ISSUER.to_owned(),
    subject: bot.user.to_string(),
    maybe_username: None,
    display_name: bot.name.clone(),
    maybe_email: None,
    is_email_verified: false,
    role: Role::Guest,
    issued_at: 0,
    authenticated_at: 0,
    maybe_session_id: None,
    authentication_methods: Vec::new(),
    maybe_acr: None,
    is_exempt_from_session_policy: true,
    maybe_organization: None,
  }
}

/// The identity of a meeting guest: an account under the server's own guest issuer, keyed by the
/// redeemed invite.
pub fn guest_identity(invite_subject: String, display_name: String, issued_at: i64) -> TokenIdentity {
  TokenIdentity {
    issuer: guest::GUEST_ISSUER.to_owned(),
    subject: invite_subject,
    maybe_username: None,
    display_name,
    maybe_email: None,
    is_email_verified: false,
    role: Role::Guest,
    issued_at,
    authenticated_at: issued_at,
    maybe_session_id: None,
    authentication_methods: Vec::new(),
    maybe_acr: None,
    is_exempt_from_session_policy: true,
    maybe_organization: None,
  }
}

/// Turns a validated token into the user: creates or refreshes the account on first sight, applies
/// pending invites for the verified email and rejects deactivated accounts.
pub async fn authenticate(state: &AppState, identity: TokenIdentity) -> Result<AuthenticatedUser, ApiError> {
  if state.revocations.is_revoked(
    &identity.issuer,
    &identity.subject,
    identity.maybe_session_id.as_deref(),
    identity.issued_at,
  ) {
    return Err(AuthError::SessionEnded.into());
  }
  ensure_not_released(state, &identity).await?;
  super::session::enforce(state, &identity).await?;
  let kind = match identity.role {
    _ if identity.issuer == BOT_ISSUER => UserKind::Bot,
    Role::Guest => UserKind::Guest,
    _ => UserKind::Member,
  };
  let synced = state
    .database
    .sync_oidc_user_flagged(
      &OidcIdentity {
        issuer: &identity.issuer,
        subject: &identity.subject,
        maybe_username: identity.maybe_username.as_deref(),
        maybe_email: identity.maybe_email.as_deref(),
        display_name: &identity.display_name,
        kind,
        maybe_oidc_role: Some(identity.role.as_str()),
        maybe_organization: identity.maybe_organization.as_deref(),
        should_sync_organization: state.settings.get().maybe_organization_claim.is_some(),
      },
      state.next_id()?,
    )
    .await?;
  let user = synced.user;
  if user.is_deactivated {
    return Err(AuthError::AccountDeactivated.into());
  }

  let may_use_email =
    identity.is_email_verified || !state.settings.get().policy.invites_require_verified_email;
  if let Some(email) = identity.maybe_email.as_deref().filter(|_| may_use_email) {
    apply_pending_invites(state, &user, email).await?;
  }
  // The flag comes with the sign-in read: members who already got their defaults cost nothing.
  if user.kind == UserKind::Member && synced.is_awaiting_default_memberships {
    apply_default_memberships(state, &user).await?;
  }
  Ok(AuthenticatedUser {
    user,
    role: identity.role,
    maybe_oidc_session: identity.maybe_session_id,
  })
}

/// The identity of a deleted account was released (admin.md §3.2.1): only a sign-in after the
/// deletion may create the new account. A token whose sign-in happened before it, also one
/// refreshed since, is refused with `account_deleted`, so a device of the deleted person can't
/// quietly open a new account. One key lookup per request; the REST, gateway and rendezvous paths
/// all come through here.
async fn ensure_not_released(state: &AppState, identity: &TokenIdentity) -> Result<(), ApiError> {
  let Some(released_at) = state
    .database
    .identity_released_at(&identity.issuer, &identity.subject)
    .await?
  else {
    return Ok(());
  };
  if identity.authenticated_at <= released_at.unix_timestamp() {
    return Err(AuthError::AccountDeleted.into());
  }
  Ok(())
}

/// The expiry guests get when their invite has none, in seconds from now.
fn guest_default_expiry_secs(state: &AppState) -> Option<f64> {
  let days = super::workspace::effective_guest_expiry_days(state);
  (days > 0).then(|| f64::from(days) * 86_400.0)
}

/// Adds a new member to the configured default spaces and channels (once per account).
async fn apply_default_memberships(state: &AppState, user: &teamcraft_db::User) -> Result<(), ApiError> {
  let policy = &state.settings.get().policy;
  let applied = state
    .database
    .apply_default_memberships(user.id, &policy.default_spaces, &policy.default_channels)
    .await?;
  announce_applied(state, user, applied).await
}

async fn apply_pending_invites(
  state: &AppState,
  user: &teamcraft_db::User,
  email: &str,
) -> Result<(), ApiError> {
  let applied = state
    .database
    .accept_pending_invites(user.id, email, guest_default_expiry_secs(state))
    .await?;
  announce_applied(state, user, applied).await
}

async fn announce_applied(
  state: &AppState,
  user: &teamcraft_db::User,
  applied: Vec<AppliedInvite>,
) -> Result<(), ApiError> {
  for invite in applied {
    info!(space_id = %invite.space_id, user_id = %user.id, "membership applied");
    let summary = super::convert::user_summary(state, user);
    let members = state.database.list_space_members(invite.space_id).await?;
    state.hub.publish(
      members.iter().map(|member| member.user.id),
      &GatewayEvent::SpaceMemberAdd {
        space_id: invite.space_id,
        user: summary.clone(),
      },
    );
    for channel in invite.joined_channels {
      let members = state.database.conversation_member_ids(channel).await?;
      state.hub.publish(
        members,
        &GatewayEvent::MembershipAdd {
          conversation_id: channel,
          user: summary.clone(),
        },
      );
    }
  }
  Ok(())
}
