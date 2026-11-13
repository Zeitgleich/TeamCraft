//! Identity provider tokens that name a person's identity key and roles (e2ee.md §3.1.3). The server
//! passes them from the person's app to the owners' apps. It makes no trust decision: the apps verify
//! them against the provider. What it checks keeps out junk and other people's tokens, nothing more.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use teamcraft_db::RoleStanding;
use teamcraft_proto::{
  Base64Bytes, GatewayEvent, ListRoleEvidenceResponse, Permission, PutRoleEvidenceRequest, Role,
  RoleEvidenceEntry, RoleLossEntry, Target, UserId, WithdrawnReason, WithdrawnRole, WorkspaceLogKind,
};

use super::access::require;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// As for the attestation of an identity key; ID tokens are a few kilobytes at most.
const MAX_TOKEN_CHARS: usize = 16 * 1024;

/// After this long nobody acts on a token any more; they name people, so they don't stay.
const KEEP_DAYS: i32 = 60;

/// Members' tokens waiting for their account to be noted wake the owners' apps when the first arrives
/// and at every this many after: each read and roster takes a page, and one event per member would send
/// every owner's app to read the list as often as members sign in (issue #1378).
const ACCOUNT_EVIDENCE_NUDGE: i64 = 50;

/// The claims the server looks at. Not verified here.
#[derive(Deserialize)]
struct TokenOutline {
  sub: String,
  iat: i64,
  #[serde(default)]
  exp: i64,
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

/// The marks of a roster document.
#[derive(Deserialize)]
struct RosterMarksOutline {
  #[serde(default)]
  provider_marks: std::collections::BTreeMap<UserId, MarkOutline>,
}

#[derive(Deserialize)]
struct MarkOutline {
  issued_at: i64,
  /// The account the owners noted for the person, if any.
  #[serde(default)]
  sub: Option<String>,
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
  // Owners, admins and who is on the roster speak about roles. Anybody else only so that an owner's app
  // notes their account (issue #1378): while the newest roster notes none for them, or another one than
  // this token's, which the owners have to see. Nobody else fills the table or sends the owners' apps to
  // read it.
  let for_account = if caller.role.includes(&Role::Admin) {
    false
  } else {
    let roster = latest_roster(state).await?;
    if roster.members.contains(&caller.user.id) {
      false
    } else if roster.maybe_marks.is_some_and(|marks| {
      marks
        .get(&caller.user.id)
        .and_then(|mark| mark.sub.as_deref())
        .is_none_or(|noted| noted != outline.sub)
    }) {
      true
    } else {
      return Ok(());
    }
  };
  let is_stored = state
    .database
    .put_role_evidence(caller.user.id, &request.token, outline.iat, for_account)
    .await?;
  if !is_stored {
    return Ok(());
  }
  if for_account {
    let waiting = state.database.count_account_evidence().await?;
    if waiting != 1 && waiting % ACCOUNT_EVIDENCE_NUDGE != 0 {
      return Ok(());
    }
  } else {
    state
      .database
      .delete_older_role_loss(caller.user.id, outline.iat)
      .await?;
  }
  let owners = state.database.list_provider_owners().await?;
  state.hub.publish(owners, &GatewayEvent::RoleEvidenceUpdate);
  Ok(())
}

/// A token the validator accepted, of somebody who now holds a lower role than they did, or none: the
/// provider's own signature on that, which doesn't depend on them publishing anything. Kept for the
/// owners' apps and handed out once it has expired, as it is still a credential until then. A failure
/// here costs nothing the request depends on.
pub async fn note_role_loss(state: &AppState, token: &str, user: UserId, lost_to: &str) {
  if let Err(error) = keep_role_loss(state, token, user, lost_to).await {
    tracing::warn!(%error, "could not keep the evidence of a lost role");
  }
}

async fn keep_role_loss(state: &AppState, token: &str, user: UserId, lost_to: &str) -> Result<(), ApiError> {
  let Some(outline) = token_outline(token) else {
    return Ok(());
  };
  // The validator still takes a token this long past its expiry.
  let leeway = i64::try_from(state.validator.leeway_secs()).unwrap_or(i64::MAX);
  let usable_until = outline.exp.saturating_add(leeway);
  let is_stored = state
    .database
    .put_role_loss(user, token, outline.iat, usable_until, lost_to)
    .await?;
  if !is_stored {
    return Ok(());
  }
  // Owners' apps read it once it can be handed out. A restart loses this, and their reconnect reads it.
  let waiting = usable_until.saturating_sub(unix_now());
  let later = state.clone();
  tokio::spawn(async move {
    tokio::time::sleep(std::time::Duration::from_secs(
      u64::try_from(waiting).unwrap_or(0),
    ))
    .await;
    match later.database.list_provider_owners().await {
      Ok(owners) => later.hub.publish(owners, &GatewayEvent::RoleEvidenceUpdate),
      Err(error) => tracing::warn!(%error, "could not tell the owners about a lost role"),
    }
  });
  Ok(())
}

/// A token of an account with no TeamCraft role: the validator refused it after checking it. Only for
/// somebody who had a role to lose.
pub async fn note_refused_for_no_role(state: &AppState, token: &str) {
  let Some(outline) = token_outline(token) else {
    return;
  };
  let issuer = state.validator.issuer().to_owned();
  let Ok(Some(user)) = state
    .database
    .find_user_by_oidc_subject(&issuer, &outline.sub)
    .await
  else {
    return;
  };
  let had_a_role = matches!(
    state.database.get_user_oidc_role(user).await,
    Ok(Some(Some(role))) if matches!(role.as_str(), "owner" | "admin")
  );
  let is_on_roster = match roster_members(state).await {
    Ok(members) => members.contains(&user),
    Err(_) => false,
  };
  if had_a_role || is_on_roster {
    note_role_loss(state, token, user, "none").await;
  }
}

/// A token holding `role` was accepted: a loss of it from before is over. Without this, the old loss could
/// still take somebody off who has long had the role back.
pub async fn note_role_held(state: &AppState, user: UserId, role: &Role, issued_at: i64) {
  let lost_to: &[&str] = match role {
    Role::Owner => &["none", "guest", "member", "admin"],
    Role::Admin => &["none", "guest", "member"],
    _ => return,
  };
  if let Err(error) = state
    .database
    .delete_regained_role_loss(user, issued_at, lost_to)
    .await
  {
    tracing::warn!(%error, "could not drop the evidence of a lost role");
  }
}

/// A new signed roster took tokens into account up to its marks: those go, as they name people.
pub async fn settle_after_roster(state: &AppState, payload: &[u8]) {
  let Ok(outline) = serde_json::from_slice::<RosterMarksOutline>(payload) else {
    return;
  };
  let marks: Vec<(UserId, i64)> = outline
    .provider_marks
    .into_iter()
    .map(|(user, mark)| (user, mark.issued_at))
    .collect();
  if let Err(error) = state.database.settle_role_evidence(&marks).await {
    tracing::warn!(%error, "could not drop role evidence the roster took into account");
  }
}

/// Drops tokens nobody took up in [`KEEP_DAYS`], from the server's housekeeping: they name people.
pub async fn prune(state: &AppState) -> Result<(), ApiError> {
  state.database.prune_role_evidence(KEEP_DAYS).await?;
  Ok(())
}

/// What the owners' apps read. Only for those the server takes for owners; everybody else has no
/// use for other people's tokens, which name them, with their e-mail address.
pub async fn list(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<ListRoleEvidenceResponse, ApiError> {
  require(state, caller, Permission::SignWorkspace, &Target::Instance)?;
  let role_losses = state
    .database
    .list_role_losses(unix_now())
    .await?
    .into_iter()
    .map(|loss| RoleLossEntry {
      user_id: loss.user_id,
      token: loss.token,
    })
    .collect();
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
    role_losses,
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
  Ok(latest_roster(state).await?.members)
}

/// What the server reads of the newest roster.
struct LatestRoster {
  members: Vec<UserId>,
  /// `None` without a roster, or one the server can't read: then nobody's account is to be noted.
  maybe_marks: Option<std::collections::BTreeMap<UserId, MarkOutline>>,
}

async fn latest_roster(state: &AppState) -> Result<LatestRoster, ApiError> {
  let nothing = LatestRoster {
    members: Vec::new(),
    maybe_marks: None,
  };
  let Some(entry) = state
    .database
    .latest_workspace_log_entry(&WorkspaceLogKind::Roster)
    .await?
  else {
    return Ok(nothing);
  };
  let Ok(roster) = serde_json::from_slice::<RosterOutline>(&entry.payload) else {
    return Ok(nothing);
  };
  let maybe_marks = serde_json::from_slice::<RosterMarksOutline>(&entry.payload)
    .ok()
    .map(|outline| outline.provider_marks);
  Ok(LatestRoster {
    members: roster.members.iter().map(|member| member.user_id).collect(),
    maybe_marks,
  })
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

fn unix_now() -> i64 {
  time::OffsetDateTime::now_utc().unix_timestamp()
}

/// The subject of an identity provider token, unverified: who it claims to be about.
pub(super) fn token_subject(token: &str) -> Option<String> {
  let payload = token.split('.').nth(1)?;
  let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
  let outline: SubjectOutline = serde_json::from_slice(&bytes).ok()?;
  Some(outline.sub)
}

#[derive(Deserialize)]
struct SubjectOutline {
  sub: String,
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
