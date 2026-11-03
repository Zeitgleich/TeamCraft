//! Deprovisioning through the identity provider (ARCHITECTURE.md §5.1.1, admin.md §3.2).
//!
//! Back-channel logout ends sessions the moment the provider ends them. The periodic sync catches
//! what logout can't: users disabled or deleted at the provider and role changes. The provider is
//! the authority; a user who loses every TeamCraft role is deactivated like a disabled one.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use secrecy::ExposeSecret;
use serde::Deserialize;
use serde_json::json;
use teamcraft_db::{GroupCreateOutcome, NewUserGroup, OidcIdentity, OidcUserRow};
use teamcraft_proto::{DeactivationReason, ProvisionStatus, ProvisionedUser, UserGroupId, UserId, UserKind};
use tracing::{debug, info, warn};

use super::admin::{AfterDeactivation, announce_reactivation, deactivate_user};
use super::audit::{self, Target as AuditTarget};
use super::user_groups;
use crate::auth::RoleMapping;
use crate::error::ApiError;
use crate::state::{AppState, OidcSyncSettings};

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Ends the sessions a back-channel logout token names (the provider ended them).
///
/// A token with `sid` ends that provider session only, even when it also names the subject
/// (Back-Channel Logout 1.0 §2.4; Keycloak always sends both): signing out on one device must not
/// sign the user's other devices out. Only a token without `sid` ends all of the subject's sessions.
pub async fn backchannel_logout(state: &AppState, logout_token: &str) -> Result<(), ApiError> {
  let identity = state.validator.validate_logout_token(logout_token).await?;
  let now = i64::try_from(jsonwebtoken::get_current_timestamp()).unwrap_or(i64::MAX);
  let issuer = state.validator.issuer();
  let maybe_user = match &identity.maybe_subject {
    Some(subject) => state.database.find_user_by_oidc_subject(issuer, subject).await?,
    None => None,
  };

  let scope = match (&identity.maybe_session_id, &identity.maybe_subject) {
    (Some(session), maybe_subject) => {
      state
        .revocations
        .revoke_session(issuer, maybe_subject.as_deref(), session, now);
      state.hub.revoke_oidc_session(session, maybe_user);
      "session"
    },
    (None, Some(subject)) => {
      state.revocations.revoke_subject(issuer, subject, now);
      if let Some(user) = maybe_user {
        state.hub.revoke_user(user);
      }
      "subject"
    },
    // The validator refuses tokens that name neither.
    (None, None) => return Ok(()),
  };
  let target = maybe_user.map(|user| AuditTarget::new("user", user));
  audit::record(
    state,
    None,
    "oidc_backchannel_logout",
    target,
    &json!({"scope": scope}),
  )
  .await?;
  Ok(())
}

/// What one sync run changed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
  pub deactivated: u32,
  pub reactivated: u32,
  pub role_changes: u32,
  pub groups_created: u32,
  pub groups_changed: u32,
  pub groups_deleted: u32,
}

/// Compares our users with the provider and applies the differences. Does nothing if the sync
/// is not configured. Fails without changing anything if the provider can't be read completely.
pub async fn sync_once(state: &AppState) -> Result<SyncReport, ApiError> {
  let Some(settings) = &state.settings.get().maybe_oidc_sync else {
    return Ok(SyncReport::default());
  };
  let provider = Provider::new(settings).await.map_err(unavailable)?;
  let remote_users = provider.list_users().await.map_err(unavailable)?;
  let client_uuid = provider.client_uuid().await.map_err(unavailable)?;
  let mapping = RoleMapping::new(settings.roles.clone());
  let remote: HashMap<&str, &RemoteUser> = remote_users.iter().map(|user| (user.id.as_str(), user)).collect();

  let mut report = SyncReport::default();
  for user in state.database.list_oidc_users(&settings.issuer).await? {
    let Some(remote_user) = remote.get(user.subject.as_str()) else {
      // Deleted at the provider.
      if deactivate(state, &user, "removed from the identity provider").await? {
        report.deactivated += 1;
      }
      continue;
    };
    if !remote_user.enabled {
      if deactivate(state, &user, "disabled at the identity provider").await? {
        report.deactivated += 1;
      }
      continue;
    }

    let roles = provider
      .client_roles(&remote_user.id, &client_uuid)
      .await
      .map_err(unavailable)?;
    match mapping.resolve(roles.iter().map(String::as_str)) {
      None => {
        if deactivate(state, &user, "no TeamCraft role at the identity provider").await? {
          report.deactivated += 1;
        }
      },
      Some(role) => {
        let was_deactivated_by_sync =
          user.is_deactivated && user.maybe_deactivation_reason.as_deref() == Some("oidc");
        if was_deactivated_by_sync && state.database.reactivate_user(user.id).await? {
          announce_reactivation(state, user.id).await?;
          audit::record(
            state,
            None,
            "user_reactivated",
            Some(AuditTarget::new("user", user.id)),
            &json!({"reason": "oidc"}),
          )
          .await?;
          report.reactivated += 1;
        }
        if !user.is_deactivated && user.maybe_oidc_role.as_deref() != Some(role.as_str()) {
          let kind = if role == teamcraft_proto::Role::Guest {
            UserKind::Guest
          } else {
            UserKind::Member
          };
          state
            .database
            .set_user_oidc_role(user.id, role.as_str(), &kind)
            .await?;
          audit::record(
            state,
            None,
            "oidc_role_changed",
            Some(AuditTarget::new("user", user.id)),
            &json!({"from": user.maybe_oidc_role, "to": role.as_str()}),
          )
          .await?;
          report.role_changes += 1;
        }
      },
    }
  }
  if settings.sync_groups {
    sync_groups(state, settings, &provider, &mut report).await?;
  }
  if report != SyncReport::default() {
    info!(?report, "oidc sync finished");
  }
  Ok(report)
}

/// Creates the accounts of `usernames` from the provider before their first sign-in (admin.md §3.8),
/// so an import can make them members. A sign-in later finds the account by issuer and subject and
/// refreshes it from the token. Fails when the sync isn't configured or the provider can't be read.
pub async fn provision_users(
  state: &AppState,
  usernames: &[String],
) -> Result<Vec<ProvisionedUser>, ApiError> {
  let Some(settings) = &state.settings.get().maybe_oidc_sync else {
    return Err(ApiError::Unavailable("identity provider sync (oidc.sync)".into()));
  };
  let provider = Provider::new(settings).await.map_err(unavailable)?;
  let client_uuid = provider.client_uuid().await.map_err(unavailable)?;
  let mapping = RoleMapping::new(settings.roles.clone());
  let mut provisioned = Vec::with_capacity(usernames.len());
  for username in usernames {
    let maybe_remote = provider.find_user(username).await.map_err(unavailable)?;
    let entry = match maybe_remote {
      Some(remote) if remote.enabled => {
        let roles = provider
          .client_roles(&remote.id, &client_uuid)
          .await
          .map_err(unavailable)?;
        match mapping.resolve(roles.iter().map(String::as_str)) {
          Some(role) => provision_one(state, settings, &remote, role).await?,
          None => (None, ProvisionStatus::NotFound),
        }
      },
      _ => (None, ProvisionStatus::NotFound),
    };
    provisioned.push(ProvisionedUser {
      username: username.clone(),
      maybe_user_id: entry.0,
      status: entry.1,
    });
  }
  Ok(provisioned)
}

async fn provision_one(
  state: &AppState,
  settings: &OidcSyncSettings,
  remote: &RemoteUser,
  role: teamcraft_proto::Role,
) -> Result<(Option<UserId>, ProvisionStatus), ApiError> {
  if let Some(existing) = state
    .database
    .find_user_by_oidc_subject(&settings.issuer, &remote.id)
    .await?
  {
    let is_available = state
      .database
      .get_users(&[existing])
      .await?
      .iter()
      .any(|user| !user.is_deactivated);
    let status = if is_available {
      ProvisionStatus::Existing
    } else {
      ProvisionStatus::Unavailable
    };
    return Ok((Some(existing), status));
  }
  let kind = if role == teamcraft_proto::Role::Guest {
    UserKind::Guest
  } else {
    UserKind::Member
  };
  let display_name = remote.display_name();
  let user = state
    .database
    .upsert_oidc_user(
      &OidcIdentity {
        issuer: &settings.issuer,
        subject: &remote.id,
        maybe_username: remote.maybe_username.as_deref(),
        maybe_email: remote.maybe_email.as_deref(),
        display_name: &display_name,
        kind,
        maybe_oidc_role: Some(role.as_str()),
        maybe_organization: None,
        should_sync_organization: false,
      },
      state.next_id()?,
    )
    .await?;
  info!(user_id = %user.id, "provisioned an account from the identity provider");
  Ok((Some(user.id), ProvisionStatus::Created))
}

/// Mirrors the provider's top-level groups as user groups. The provider is the source of truth for
/// their name and members; a group that vanished there is deleted here.
async fn sync_groups(
  state: &AppState,
  settings: &OidcSyncSettings,
  provider: &Provider<'_>,
  report: &mut SyncReport,
) -> Result<(), ApiError> {
  let remote_groups = provider.list_groups().await.map_err(unavailable)?;
  let mut seen = HashSet::new();
  for remote in &remote_groups {
    seen.insert(remote.id.clone());
    let subjects = provider.group_member_ids(&remote.id).await.map_err(unavailable)?;
    let members = state
      .database
      .find_users_by_subjects(&settings.issuer, &subjects)
      .await?;

    let (group_id, is_created, is_renamed) =
      match state.database.find_user_group_by_external_id(&remote.id).await? {
        Some(group) => {
          let is_renamed = group.name != remote.name;
          if is_renamed {
            state
              .database
              .update_user_group(group.id, Some(&remote.name), None, false)
              .await?;
          }
          (group.id, false, is_renamed)
        },
        None => {
          let Some(handle) = user_groups::handle_from_name(&remote.name) else {
            warn!(group = %remote.id, "the group name cannot become a handle; skipped");
            continue;
          };
          let id: UserGroupId = state.next_id()?;
          let outcome = state
            .database
            .create_user_group(&NewUserGroup {
              id,
              handle: &handle,
              name: &remote.name,
              maybe_description: None,
              maybe_external_id: Some(&remote.id),
              maybe_created_by: None,
            })
            .await?;
          if outcome == GroupCreateOutcome::HandleTaken {
            warn!(%handle, "the handle of a provider group is taken; skipped");
            continue;
          }
          report.groups_created += 1;
          (id, true, false)
        },
      };
    let has_new_members = state.database.set_user_group_members(group_id, &members).await?;
    if !is_created && (is_renamed || has_new_members) {
      report.groups_changed += 1;
    }
    if is_created || is_renamed || has_new_members {
      user_groups::announce(state, group_id);
    }
  }
  for (id, external_id) in state.database.list_synced_group_external_ids().await? {
    if !seen.contains(&external_id) {
      state.database.delete_user_group(id).await?;
      user_groups::announce(state, id);
      report.groups_deleted += 1;
    }
  }
  Ok(())
}

async fn deactivate(state: &AppState, user: &OidcUserRow, note: &str) -> Result<bool, ApiError> {
  if user.is_deactivated {
    return Ok(false);
  }
  // Suspended like by an admin: the provider may enable the user again, and the sync then
  // reactivates them with everything intact. The automatic removal setting covers people who left.
  deactivate_user(
    state,
    None,
    user.id,
    DeactivationReason::Oidc,
    note,
    AfterDeactivation::KeepChats,
  )
  .await
}

fn unavailable(error: ProviderError) -> ApiError {
  warn!(%error, "identity provider request failed");
  ApiError::Unavailable("identity provider".into())
}

#[derive(Debug, thiserror::Error)]
enum ProviderError {
  #[error("request failed: {0}")]
  Request(#[from] reqwest::Error),
  #[error("unexpected answer: {0}")]
  Unexpected(String),
}

#[derive(Deserialize)]
struct RemoteUser {
  id: String,
  #[serde(default)]
  enabled: bool,
  #[serde(rename = "username", default)]
  maybe_username: Option<String>,
  #[serde(rename = "email", default)]
  maybe_email: Option<String>,
  #[serde(rename = "firstName", default)]
  maybe_first_name: Option<String>,
  #[serde(rename = "lastName", default)]
  maybe_last_name: Option<String>,
}

impl RemoteUser {
  /// The name a token's `name` claim would carry (Keycloak: first and last name), else the user
  /// name, else the subject, as the token validator does.
  fn display_name(&self) -> String {
    let full = [self.maybe_first_name.as_deref(), self.maybe_last_name.as_deref()]
      .into_iter()
      .flatten()
      .map(str::trim)
      .filter(|part| !part.is_empty())
      .collect::<Vec<_>>()
      .join(" ");
    if !full.is_empty() {
      return full;
    }
    self.maybe_username.clone().unwrap_or_else(|| self.id.clone())
  }
}

#[derive(Deserialize)]
struct RemoteGroup {
  id: String,
  name: String,
}

#[derive(Deserialize)]
struct RemoteClient {
  id: String,
}

#[derive(Deserialize)]
struct RemoteRole {
  name: String,
}

#[derive(Deserialize)]
struct TokenAnswer {
  access_token: String,
}

/// A logged-in view of the provider's admin API.
struct Provider<'a> {
  http: reqwest::Client,
  settings: &'a OidcSyncSettings,
  access_token: String,
}

impl<'a> Provider<'a> {
  async fn new(settings: &'a OidcSyncSettings) -> Result<Self, ProviderError> {
    let http = reqwest::Client::builder().timeout(HTTP_TIMEOUT).build()?;
    let answer: TokenAnswer = http
      .post(&settings.token_url)
      .form(&[
        ("grant_type", "client_credentials"),
        ("client_id", settings.server_client.as_str()),
        ("client_secret", settings.client_secret.expose_secret()),
      ])
      .send()
      .await?
      .error_for_status()?
      .json()
      .await?;
    Ok(Self {
      http,
      settings,
      access_token: answer.access_token,
    })
  }

  async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, ProviderError> {
    let url = format!("{}{path}", self.settings.admin_url.trim_end_matches('/'));
    debug!(%url, "asking the identity provider");
    Ok(
      self
        .http
        .get(url)
        .bearer_auth(&self.access_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?,
    )
  }

  /// All users, page by page.
  async fn list_users(&self) -> Result<Vec<RemoteUser>, ProviderError> {
    let page_size = self.settings.page_size.max(1) as usize;
    let mut users = Vec::new();
    loop {
      let page: Vec<RemoteUser> = self
        .get(&format!(
          "/users?first={}&max={page_size}&briefRepresentation=true",
          users.len()
        ))
        .await?;
      let count = page.len();
      users.extend(page);
      if count < page_size {
        return Ok(users);
      }
    }
  }

  /// The top-level groups, page by page.
  async fn list_groups(&self) -> Result<Vec<RemoteGroup>, ProviderError> {
    let page_size = self.settings.page_size.max(1) as usize;
    let mut groups = Vec::new();
    loop {
      let page: Vec<RemoteGroup> = self
        .get(&format!(
          "/groups?first={}&max={page_size}&briefRepresentation=true",
          groups.len()
        ))
        .await?;
      let count = page.len();
      groups.extend(page);
      if count < page_size {
        return Ok(groups);
      }
    }
  }

  /// The subjects of a group's direct members, page by page.
  async fn group_member_ids(&self, group: &str) -> Result<Vec<String>, ProviderError> {
    let page_size = self.settings.page_size.max(1) as usize;
    let mut ids = Vec::new();
    loop {
      let page: Vec<RemoteUser> = self
        .get(&format!(
          "/groups/{group}/members?first={}&max={page_size}&briefRepresentation=true",
          ids.len()
        ))
        .await?;
      let count = page.len();
      ids.extend(page.into_iter().map(|user| user.id));
      if count < page_size {
        return Ok(ids);
      }
    }
  }

  /// The user with exactly this user name.
  async fn find_user(&self, username: &str) -> Result<Option<RemoteUser>, ProviderError> {
    let encoded: String = url::form_urlencoded::byte_serialize(username.as_bytes()).collect();
    let users: Vec<RemoteUser> = self.get(&format!("/users?username={encoded}&exact=true")).await?;
    Ok(users.into_iter().find(|user| {
      user
        .maybe_username
        .as_deref()
        .is_some_and(|name| name.eq_ignore_ascii_case(username))
    }))
  }

  async fn client_uuid(&self) -> Result<String, ProviderError> {
    let clients: Vec<RemoteClient> = self
      .get(&format!("/clients?clientId={}", self.settings.client_id))
      .await?;
    clients
      .into_iter()
      .next()
      .map(|client| client.id)
      .ok_or_else(|| ProviderError::Unexpected(format!("client {} not found", self.settings.client_id)))
  }

  /// Client roles of the user including those inherited from groups.
  async fn client_roles(&self, user: &str, client_uuid: &str) -> Result<Vec<String>, ProviderError> {
    let roles: Vec<RemoteRole> = self
      .get(&format!(
        "/users/{user}/role-mappings/clients/{client_uuid}/composite"
      ))
      .await?;
    Ok(roles.into_iter().map(|role| role.name).collect())
  }
}

/// Derives the admin API URL of a Keycloak realm from its issuer (`<base>/realms/<realm>`).
pub fn admin_url_from_issuer(issuer: &str) -> Option<String> {
  let (base, realm) = issuer.trim_end_matches('/').rsplit_once("/realms/")?;
  (!realm.is_empty() && !realm.contains('/')).then(|| format!("{base}/admin/realms/{realm}"))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_admin_url_is_derived_from_a_keycloak_issuer() {
    assert_eq!(
      admin_url_from_issuer("https://auth.example.com/realms/company").as_deref(),
      Some("https://auth.example.com/admin/realms/company")
    );
    assert_eq!(
      admin_url_from_issuer("http://127.0.0.1:8080/auth/realms/dev/").as_deref(),
      Some("http://127.0.0.1:8080/auth/admin/realms/dev")
    );
    assert_eq!(admin_url_from_issuer("https://accounts.example.com"), None);
    assert_eq!(admin_url_from_issuer("https://auth.example.com/realms/a/b"), None);
  }
}
