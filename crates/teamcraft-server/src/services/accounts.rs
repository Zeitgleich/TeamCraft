use teamcraft_db::OidcIdentity;
use teamcraft_proto::{GatewayEvent, Role, UserKind};
use tracing::info;

use crate::auth::{AuthError, AuthenticatedUser, TokenIdentity};
use crate::error::ApiError;
use crate::state::AppState;

/// Turns a validated token into the user: creates or refreshes the account on first sight, applies
/// pending invites for the verified email and rejects deactivated accounts.
pub async fn authenticate(state: &AppState, identity: TokenIdentity) -> Result<AuthenticatedUser, ApiError> {
  let kind = match identity.role {
    Role::Guest => UserKind::Guest,
    _ => UserKind::Member,
  };
  let user = state
    .database
    .sync_oidc_user(
      &OidcIdentity {
        issuer: &identity.issuer,
        subject: &identity.subject,
        maybe_username: identity.maybe_username.as_deref(),
        maybe_email: identity.maybe_email.as_deref(),
        display_name: &identity.display_name,
        kind,
      },
      state.next_id()?,
    )
    .await?;
  if user.is_deactivated {
    return Err(AuthError::AccountDeactivated.into());
  }

  let may_use_email = identity.is_email_verified || !state.settings.policy.invites_require_verified_email;
  if let Some(email) = identity.maybe_email.as_deref().filter(|_| may_use_email) {
    apply_pending_invites(state, &user, email).await?;
  }
  Ok(AuthenticatedUser {
    user,
    role: identity.role,
  })
}

async fn apply_pending_invites(
  state: &AppState,
  user: &teamcraft_db::User,
  email: &str,
) -> Result<(), ApiError> {
  let applied = state.database.accept_pending_invites(user.id, email).await?;
  for invite in applied {
    info!(space_id = %invite.space_id, user_id = %user.id, "invite accepted");
    let summary = super::convert::user_summary(user);
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
