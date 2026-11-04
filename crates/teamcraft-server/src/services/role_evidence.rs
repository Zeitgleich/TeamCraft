//! Identity provider tokens that name a person's identity key and roles (e2ee.md §3.1.3). The server
//! passes them from the person's app to the owners' apps. It makes no trust decision: the apps verify
//! them against the provider. What it checks keeps out junk and other people's tokens, nothing more.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use teamcraft_db::RoleStanding;
use teamcraft_proto::{
  Base64Bytes, GatewayEvent, ListRoleEvidenceResponse, Permission, PutRoleEvidenceRequest, Role,
  RoleEvidenceEntry, Target, UserId, WithdrawnReason, WithdrawnRole, WorkspaceLogKind,
};

use super::access::require;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// As for the attestation of an identity key; ID tokens are a few kilobytes at most.
const MAX_TOKEN_CHARS: usize = 16 * 1024;

/// The claims the server looks at. Not verified here.
#[derive(Deserialize)]
struct TokenOutline {
  sub: String,
  iat: i64,
}

/// Just enough of a roster document to know who is on it.
#[derive(Deserialize)]
struct RosterOutline {
  members: Vec<RosterMemberOutline>,
}

#[derive(Deserialize)]
struct RosterMemberOutline {
  user_id: UserId,
}

/// Keeps the caller's token if it is newer than the one stored and tells the owners' apps.
pub async fn put(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: PutRoleEvidenceRequest,
) -> Result<(), ApiError> {
  if request.token.chars().count() > MAX_TOKEN_CHARS {
    return Err(ApiError::InvalidRequest("token is too long".into()));
  }
  let outline = token_outline(&request.token)
    .ok_or_else(|| ApiError::InvalidRequest("this is not an identity provider token".into()))?;
  // Everybody's tokens would fill the table: a person only ever brings their own.
  let issuer = state.validator.issuer().to_owned();
  let maybe_owner = state
    .database
    .find_user_by_oidc_subject(&issuer, &outline.sub)
    .await?;
  if maybe_owner != Some(caller.user.id) {
    return Err(ApiError::Forbidden("this token belongs to somebody else".into()));
  }
  // Only owners, admins and who is on the roster have anything to say here. Nobody else fills the table
  // or sends the owners' apps to read it.
  if !caller.role.includes(&Role::Admin) && !roster_members(state).await?.contains(&caller.user.id) {
    return Ok(());
  }
  let is_stored = state
    .database
    .put_role_evidence(caller.user.id, &request.token, outline.iat)
    .await?;
  if is_stored {
    let owners = state.database.list_provider_owners().await?;
    state.hub.publish(owners, &GatewayEvent::RoleEvidenceUpdate);
  }
  Ok(())
}

/// What the owners' apps read. Only for those the server takes for owners; everybody else has no
/// use for other people's tokens, which name them, with their e-mail address.
pub async fn list(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<ListRoleEvidenceResponse, ApiError> {
  require(state, caller, Permission::SignWorkspace, &Target::Instance)?;
  let entries = state
    .database
    .list_role_evidence()
    .await?
    .into_iter()
    .map(|stored| RoleEvidenceEntry {
      user_id: stored.user_id,
      token: stored.token,
      public_key: Base64Bytes::new(stored.public_key),
    })
    .collect();
  Ok(ListRoleEvidenceResponse {
    entries,
    withdrawn: withdrawn_roles(state).await?,
  })
}

/// The people of the newest roster who, by the server's records, no longer hold their role.
async fn withdrawn_roles(state: &AppState) -> Result<Vec<WithdrawnRole>, ApiError> {
  let users = roster_members(state).await?;
  let standings = state.database.role_standings(&users).await?;
  Ok(standings.iter().filter_map(withdrawn_role).collect())
}

/// Who the newest roster lists. A roster the server can't read is one it stored for an older or a
/// newer client: it reports nobody rather than somebody wrong.
async fn roster_members(state: &AppState) -> Result<Vec<UserId>, ApiError> {
  let Some(entry) = state
    .database
    .latest_workspace_log_entry(&WorkspaceLogKind::Roster)
    .await?
  else {
    return Ok(Vec::new());
  };
  let Ok(roster) = serde_json::from_slice::<RosterOutline>(&entry.payload) else {
    return Ok(Vec::new());
  };
  Ok(roster.members.iter().map(|member| member.user_id).collect())
}

fn withdrawn_role(standing: &RoleStanding) -> Option<WithdrawnRole> {
  let reason = if standing.is_gone {
    WithdrawnReason::AccountGone
  } else {
    // An account the provider doesn't list (a local one) has no provider role to lose.
    let role = standing.maybe_oidc_role.as_deref()?;
    let is_admin = matches!(Role::from(role.to_owned()), Role::Owner | Role::Admin);
    if is_admin {
      return None;
    }
    WithdrawnReason::RoleGone
  };
  Some(WithdrawnRole {
    user_id: standing.user_id,
    reason,
  })
}

fn token_outline(token: &str) -> Option<TokenOutline> {
  let payload = token.split('.').nth(1)?;
  let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
  serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_token_needs_a_subject_and_a_time() {
    let token = |claims: &str| format!("h.{}.s", URL_SAFE_NO_PAD.encode(claims));
    let outline = token_outline(&token(r#"{"sub":"abc","iat":7,"email":"a@example.com"}"#)).unwrap();
    assert_eq!((outline.sub.as_str(), outline.iat), ("abc", 7));
    assert!(token_outline(&token(r#"{"sub":"abc"}"#)).is_none());
    assert!(token_outline(&token(r#"{"iat":7}"#)).is_none());
    assert!(token_outline("not-a-token").is_none());
    assert!(token_outline("a.!!.c").is_none());
  }

  mod withdrawn {
    use teamcraft_proto::Id as _;

    use super::*;

    fn user() -> UserId {
      UserId::from_snowflake(teamcraft_proto::Snowflake::new(5).unwrap())
    }

    #[test]
    fn a_gone_account_and_a_lost_role_are_reported() {
      assert_eq!(
        withdrawn_role(&standing(true, Some("owner"))).unwrap().reason,
        WithdrawnReason::AccountGone
      );
      assert_eq!(
        withdrawn_role(&standing(false, Some("member"))).unwrap().reason,
        WithdrawnReason::RoleGone
      );
    }

    #[test]
    fn holders_and_local_accounts_are_not() {
      assert!(withdrawn_role(&standing(false, Some("owner"))).is_none());
      assert!(withdrawn_role(&standing(false, Some("admin"))).is_none());
      assert!(withdrawn_role(&standing(false, None)).is_none());
    }

    fn standing(is_gone: bool, maybe_role: Option<&str>) -> RoleStanding {
      RoleStanding {
        user_id: user(),
        is_gone,
        maybe_oidc_role: maybe_role.map(str::to_owned),
      }
    }
  }
}
