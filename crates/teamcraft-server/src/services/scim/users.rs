//! `/scim/v2/Users`: accounts as SCIM resources.
//!
//! - **Create** makes the account the first sign-in finds by its subject (the client's identity
//!   attribute), as the sync's pre-creation does (docs/provisioning.md §6), or links the account
//!   that is there. A retried create answers with the same resource, so retries are idempotent.
//! - **Disable** (`active: false`) runs the deactivation sequence with `source: scim`; the account
//!   stays a resource and comes back intact when enabled again. **Delete** removes the resource:
//!   an account that never signed in is deleted outright, any other is deactivated.
//! - Both pass the safety brake's time window; beyond it, and always for owners and admins, the
//!   deactivation is held for an admin to confirm (admin.md §3.2.3).

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use teamcraft_db::{
  DeletionGuard, OidcIdentity, ScimLink, ScimUserLookup, ScimUserRow, ScimUserWrite, ScimWrite,
};
use teamcraft_proto::scim::{
  Filter, ListQuery, ListResponse, PatchRequest, USER_SCHEMA, UserAttributes, apply_user_patch,
  normalize_keys, project,
};
use teamcraft_proto::{
  DeactivationCause, DeactivationReason, DeactivationSource, ScimClientId, ScimGroupId,
  ScimIdentityAttribute, UserId, UserKind,
};
use tracing::{info, warn};

use super::error::ScimError;
use super::{ScimCaller, ScimResponse, expected_version, is_not_modified, location, meta, not_modified};
use crate::error::ApiError;
use crate::services::admin::{
  AccountDeletion, AfterDeactivation, deactivate_user_from, delete_account, reactivate_and_announce,
};
use crate::services::audit::{self, Target as AuditTarget};
use crate::services::oidc_brake::Brake;
use crate::state::AppState;

const NOTE_DISABLED: &str = "disabled through SCIM";
const NOTE_DELETED: &str = "deleted through SCIM";
/// The role of an account SCIM makes: SCIM carries no TeamCraft role, the first sign-in's token
/// sets the real one. Never more than a member, so a SCIM token can't make an admin.
const PROVISIONED_ROLE: &str = "member";

pub async fn get_user(
  state: &AppState,
  caller: &ScimCaller,
  id: &str,
  headers: &HeaderMap,
) -> Result<Response, ScimError> {
  let row = load(state, caller.client, id).await?;
  if is_not_modified(headers, row.version) {
    return Ok(not_modified(row.version));
  }
  Ok(
    answer(state, caller.client, row, StatusCode::OK)
      .await?
      .into_response(),
  )
}

pub async fn list_users(
  state: &AppState,
  caller: &ScimCaller,
  query: &ListQuery,
) -> Result<ScimResponse, ScimError> {
  let (offset, count) = query.window();
  let (rows, total) = match query
    .maybe_filter
    .as_deref()
    .map(str::trim)
    .filter(|text| !text.is_empty())
  {
    None => {
      let (rows, total) = state
        .database
        .scim_users_page(
          caller.client,
          i64::try_from(offset).unwrap_or(i64::MAX),
          i64::try_from(count).unwrap_or(0),
        )
        .await?;
      (rows, usize::try_from(total).unwrap_or(0))
    },
    Some(text) => {
      let filter = Filter::parse(text).map_err(|error| ScimError::InvalidFilter(error.to_string()))?;
      let candidates = candidates(state, caller.client, &filter).await?;
      let groups = groups_of(state, caller.client, &candidates).await?;
      let matching: Vec<ScimUserRow> = candidates
        .into_iter()
        .filter(|row| {
          let resource = resource(state, row, groups_for(&groups, row.user));
          normalize_keys(resource).is_ok_and(|normalized| filter.matches(&normalized))
        })
        .collect();
      let total = matching.len();
      (matching.into_iter().skip(offset).take(count).collect(), total)
    },
  };
  let groups = groups_of(state, caller.client, &rows).await?;
  let resources = rows
    .iter()
    .map(|row| {
      let mut resource = resource(state, row, groups_for(&groups, row.user));
      project(
        &mut resource,
        query.maybe_attributes.as_deref(),
        query.maybe_excluded_attributes.as_deref(),
      );
      resource
    })
    .collect();
  let body = ListResponse::new(total, offset + 1, resources);
  Ok(ScimResponse::ok(serde_json::to_value(body).map_err(internal)?))
}

pub async fn create_user(
  state: &AppState,
  caller: &ScimCaller,
  body: &Bytes,
) -> Result<ScimResponse, ScimError> {
  let attributes = UserAttributes::from_resource(&super::parse_body(state, body)?)?;
  let subject = identity_value(caller.identity_attribute.clone(), &attributes)?.to_owned();
  let issuer = issuer(state);
  // Another user of this client with the user name: refused before an account is made for it.
  let is_name_taken = state
    .database
    .find_scim_users(caller.client, ScimUserLookup::UserName(&attributes.user_name))
    .await?
    .iter()
    .any(|row| identity_of(caller.identity_attribute.clone(), row) != Some(subject.as_str()));
  if is_name_taken {
    return Err(ScimError::Uniqueness(
      "this client has another user with the userName",
    ));
  }
  let (user, is_created) = match state
    .database
    .find_user_by_oidc_subject(&issuer, &subject)
    .await?
  {
    Some(user) => (user, false),
    None => {
      if state
        .database
        .released_subjects(&issuer)
        .await?
        .contains(&subject)
      {
        return Err(ScimError::IdentityReleased);
      }
      provision(state, &issuer, &subject, &attributes).await?
    },
  };
  let shown_name = attributes.shown_name();
  let write = write_of(&attributes, &shown_name);
  match state.database.link_scim_user(caller.client, user, &write).await? {
    ScimLink::Linked => {
      let action = if is_created {
        "user_provisioned"
      } else {
        "scim_user_linked"
      };
      audit::record(
        state,
        None,
        action,
        Some(AuditTarget::new("user", user)),
        &json!({"source": DeactivationSource::Scim.as_str(), "scim_client": caller.client.to_string(), "role": PROVISIONED_ROLE}),
      )
      .await?;
    },
    // A retried create: the resource is there, take the attributes again.
    ScimLink::AlreadyLinked => match state
      .database
      .update_scim_user(caller.client, user, &write, None)
      .await?
    {
      ScimWrite::Written(_) | ScimWrite::NotFound | ScimWrite::VersionMismatch => {},
      ScimWrite::NameTaken => {
        return Err(ScimError::Uniqueness(
          "this client has another user with the userName",
        ));
      },
    },
    ScimLink::UserNameTaken => {
      undo_creation(state, user, is_created).await;
      return Err(ScimError::Uniqueness(
        "this client has another user with the userName",
      ));
    },
    ScimLink::LinkedElsewhere => {
      undo_creation(state, user, is_created).await;
      return Err(ScimError::LinkedElsewhere);
    },
  }
  set_active(
    state,
    caller,
    user,
    attributes.is_active,
    DeactivationCause::Disabled,
  )
  .await?;
  let row = load_user(state, caller.client, user).await?;
  answer(state, caller.client, row, StatusCode::CREATED).await
}

pub async fn replace_user(
  state: &AppState,
  caller: &ScimCaller,
  id: &str,
  headers: &HeaderMap,
  body: &Bytes,
) -> Result<ScimResponse, ScimError> {
  let row = load(state, caller.client, id).await?;
  let attributes = UserAttributes::from_resource(&super::parse_body(state, body)?)?;
  write(state, caller, row, &attributes, expected_version(headers)).await
}

pub async fn patch_user(
  state: &AppState,
  caller: &ScimCaller,
  id: &str,
  headers: &HeaderMap,
  body: &Bytes,
) -> Result<ScimResponse, ScimError> {
  let row = load(state, caller.client, id).await?;
  let request: PatchRequest = serde_json::from_value(super::parse_body(state, body)?)
    .map_err(|error| ScimError::InvalidSyntax(error.to_string()))?;
  let request = request.checked()?;
  let mut resource = attributes_of(&row).to_patchable();
  apply_user_patch(&mut resource, &request.operations)?;
  let attributes = UserAttributes::from_resource(&resource)?;
  write(state, caller, row, &attributes, expected_version(headers)).await
}

pub async fn delete_user(
  state: &AppState,
  caller: &ScimCaller,
  id: &str,
  headers: &HeaderMap,
) -> Result<(), ScimError> {
  let row = load(state, caller.client, id).await?;
  if expected_version(headers).is_some_and(|expected| expected != row.version) {
    return Err(ScimError::VersionMismatch);
  }
  deprovision(state, caller.client, row.user, DeactivationCause::Removed).await?;
  state.database.unlink_scim_user(caller.client, row.user).await?;
  audit::record(
    state,
    None,
    "scim_user_removed",
    Some(AuditTarget::new("user", row.user)),
    &json!({"source": DeactivationSource::Scim.as_str(), "scim_client": caller.client.to_string()}),
  )
  .await?;
  Ok(())
}

/// Applies the SCIM deactivations an admin confirmed (the console's brake confirmation). A failure
/// leaves the hold confirmed for the next confirmation or the next SCIM request to retry.
pub async fn apply_confirmed_holds(state: &AppState) -> Result<u32, ApiError> {
  let mut applied = 0;
  for (user, cause) in state.database.confirmed_scim_holds().await? {
    let cause = DeactivationCause::from(cause);
    match Box::pin(apply_deprovision(state, user, &cause)).await {
      Ok(()) => {
        state.database.release_scim_hold(user).await?;
        applied += 1;
      },
      Err(error) => warn!(user_id = %user, %error, "a confirmed SCIM deactivation failed"),
    }
  }
  Ok(applied)
}

/// A PUT or PATCH: the attributes are written (the sign-in subject may not change), then `active`.
async fn write(
  state: &AppState,
  caller: &ScimCaller,
  row: ScimUserRow,
  attributes: &UserAttributes,
  maybe_expected_version: Option<i64>,
) -> Result<ScimResponse, ScimError> {
  let attribute = caller.identity_attribute.clone();
  let new_subject = identity_value(attribute.clone(), attributes)?;
  if identity_of(attribute, &row) != Some(new_subject) {
    return Err(ScimError::IdentityImmutable(
      "the attribute that is the sign-in subject can't change; deprovision the user and create a new one"
        .into(),
    ));
  }
  let shown_name = attributes.shown_name();
  let write = write_of(attributes, &shown_name);
  match state
    .database
    .update_scim_user(caller.client, row.user, &write, maybe_expected_version)
    .await?
  {
    ScimWrite::Written(_) => {},
    ScimWrite::NotFound => return Err(ScimError::NotFound),
    ScimWrite::VersionMismatch => return Err(ScimError::VersionMismatch),
    ScimWrite::NameTaken => {
      return Err(ScimError::Uniqueness(
        "this client has another user with the userName",
      ));
    },
  }
  if attributes.is_active != row.is_active {
    set_active(
      state,
      caller,
      row.user,
      attributes.is_active,
      DeactivationCause::Disabled,
    )
    .await?;
    state.database.touch_scim_user(caller.client, row.user).await?;
  }
  let row = load_user(state, caller.client, row.user).await?;
  answer(state, caller.client, row, StatusCode::OK).await
}

/// Makes the account of a SCIM user before its first sign-in.
async fn provision(
  state: &AppState,
  issuer: &str,
  subject: &str,
  attributes: &UserAttributes,
) -> Result<(UserId, bool), ScimError> {
  let shown_name = attributes.shown_name();
  let account = state
    .database
    .provision_oidc_user(
      &OidcIdentity {
        issuer,
        subject,
        maybe_username: Some(&attributes.user_name),
        maybe_email: attributes.maybe_email.as_deref(),
        display_name: &shown_name,
        kind: UserKind::Member,
        maybe_oidc_role: Some(PROVISIONED_ROLE),
        maybe_organization: None,
        should_sync_organization: false,
      },
      state.next_id()?,
    )
    .await?;
  if account.is_created {
    info!(user_id = %account.user, "provisioned an account through SCIM");
  }
  Ok((account.user, account.is_created))
}

/// Removes an account this request just made but couldn't link. Best effort: a leftover is a
/// directory entry nobody signed in to, which the next create or an admin removes.
async fn undo_creation(state: &AppState, user: UserId, is_created: bool) {
  if !is_created {
    return;
  }
  let detail = json!({"source": DeactivationSource::Scim.as_str(), "reason": "never_signed_in"});
  if let Err(error) = Box::pin(delete_account(
    state,
    None,
    user,
    DeletionGuard::NeverSignedIn,
    &detail,
  ))
  .await
  {
    warn!(user_id = %user, %error, "removing an account SCIM could not link failed");
  }
}

/// Enables or disables the account. Enabling lifts a deactivation by the identity provider (not one
/// by an admin) and a hold; disabling passes the brake.
async fn set_active(
  state: &AppState,
  caller: &ScimCaller,
  user: UserId,
  is_active: bool,
  cause: DeactivationCause,
) -> Result<(), ScimError> {
  if !is_active {
    return deprovision(state, caller.client, user, cause).await;
  }
  state.database.release_scim_hold(user).await?;
  let Some(account) = state.database.scim_account_state(user).await? else {
    return Ok(());
  };
  let is_by_provider =
    account.maybe_deactivation_reason.as_deref() == Some(DeactivationReason::Oidc.as_str());
  if account.is_deactivated && is_by_provider && reactivate_and_announce(state, user).await? {
    audit::record(
      state,
      None,
      "user_reactivated",
      Some(AuditTarget::new("user", user)),
      &json!({"reason": "oidc", "source": DeactivationSource::Scim.as_str(), "scim_client": caller.client.to_string()}),
    )
    .await?;
  }
  Ok(())
}

/// Deactivates (or, for a delete of an account that never signed in, deletes) through the brake.
async fn deprovision(
  state: &AppState,
  client: ScimClientId,
  user: UserId,
  cause: DeactivationCause,
) -> Result<(), ScimError> {
  let Some(account) = state.database.scim_account_state(user).await? else {
    return Ok(());
  };
  let is_deletion = cause == DeactivationCause::Removed && account.is_awaiting_first_sign_in;
  if account.is_deactivated && !is_deletion {
    return Ok(());
  }
  let is_privileged = matches!(account.maybe_oidc_role.as_deref(), Some("owner" | "admin"));
  let may_proceed = !is_privileged && reserve(state, user).await?;
  if !may_proceed {
    state
      .database
      .hold_scim_deactivation(user, cause.as_str())
      .await?;
    warn!(user_id = %user, is_privileged, "the safety brake holds a SCIM deactivation for an admin to confirm");
    audit::record(
      state,
      None,
      "scim_deactivation_held",
      Some(AuditTarget::new("user", user)),
      &json!({"source": DeactivationSource::Scim.as_str(), "scim_client": client.to_string(), "cause": cause.as_str(), "is_privileged": is_privileged}),
    )
    .await?;
    return Ok(());
  }
  Ok(Box::pin(apply_deprovision(state, user, &cause)).await?)
}

/// Takes a place in the brake's window.
async fn reserve(state: &AppState, user: UserId) -> Result<bool, ScimError> {
  let settings = state.settings.get();
  let config = &settings.scim;
  let brake = Brake::new(config.brake_max_percent, config.brake_min_allowed);
  let window = std::time::Duration::from_secs(config.brake_window_secs.max(1));
  let issuer = settings.sign_in.issuer.clone();
  Ok(
    state
      .database
      .reserve_scim_deprovision(user, &issuer, window, |recent, active| {
        brake.allows_one_more(recent, active)
      })
      .await?,
  )
}

/// The deactivation or deletion itself, past the brake.
async fn apply_deprovision(
  state: &AppState,
  user: UserId,
  cause: &DeactivationCause,
) -> Result<(), ApiError> {
  if *cause == DeactivationCause::Removed {
    let detail = json!({
      "source": DeactivationSource::Scim.as_str(), "reason": "never_signed_in", "cause": cause.as_str(),
    });
    match Box::pin(delete_account(
      state,
      None,
      user,
      DeletionGuard::NeverSignedIn,
      &detail,
    ))
    .await?
    {
      AccountDeletion::Deleted | AccountDeletion::WasAlreadyDeleted => return Ok(()),
      AccountDeletion::SignedIn => {},
    }
  }
  let note = if *cause == DeactivationCause::Removed {
    NOTE_DELETED
  } else {
    NOTE_DISABLED
  };
  Box::pin(deactivate_user_from(
    state,
    None,
    user,
    DeactivationReason::Oidc,
    DeactivationSource::Scim,
    note,
    AfterDeactivation::KeepChats,
  ))
  .await?;
  Ok(())
}

/// The value of the client's identity attribute: the sign-in subject. Required.
fn identity_value(attribute: ScimIdentityAttribute, attributes: &UserAttributes) -> Result<&str, ScimError> {
  match attribute {
    ScimIdentityAttribute::UserName => Ok(&attributes.user_name),
    _ => attributes.maybe_external_id.as_deref().ok_or_else(|| {
      ScimError::IdentityImmutable(
        "externalId is required: it is the sign-in subject for this SCIM client".into(),
      )
    }),
  }
}

fn identity_of(attribute: ScimIdentityAttribute, row: &ScimUserRow) -> Option<&str> {
  match attribute {
    ScimIdentityAttribute::UserName => Some(&row.user_name),
    _ => row.maybe_external_id.as_deref(),
  }
}

fn write_of<'a>(attributes: &'a UserAttributes, shown_name: &'a str) -> ScimUserWrite<'a> {
  ScimUserWrite {
    user_name: &attributes.user_name,
    maybe_external_id: attributes.maybe_external_id.as_deref(),
    maybe_given_name: attributes.maybe_given_name.as_deref(),
    maybe_family_name: attributes.maybe_family_name.as_deref(),
    maybe_display_name: attributes.maybe_display_name.as_deref(),
    shown_name,
    maybe_email: attributes.maybe_email.as_deref(),
  }
}

fn attributes_of(row: &ScimUserRow) -> UserAttributes {
  UserAttributes {
    user_name: row.user_name.clone(),
    maybe_external_id: row.maybe_external_id.clone(),
    maybe_given_name: row.maybe_given_name.clone(),
    maybe_family_name: row.maybe_family_name.clone(),
    maybe_display_name: row.maybe_display_name.clone(),
    maybe_email: row.maybe_email.clone(),
    is_active: row.is_active,
  }
}

fn issuer(state: &AppState) -> String {
  state.settings.get().sign_in.issuer.clone()
}

async fn load(state: &AppState, client: ScimClientId, id: &str) -> Result<ScimUserRow, ScimError> {
  let user: UserId = id.parse().map_err(|_| ScimError::NotFound)?;
  load_user(state, client, user).await
}

async fn load_user(state: &AppState, client: ScimClientId, user: UserId) -> Result<ScimUserRow, ScimError> {
  state
    .database
    .scim_user(client, user)
    .await?
    .ok_or(ScimError::NotFound)
}

/// The resources a filter could match: looked up by an equality on an indexed attribute, else
/// all of the client's, up to `[scim] max_filter_scan`.
async fn candidates(
  state: &AppState,
  client: ScimClientId,
  filter: &Filter,
) -> Result<Vec<ScimUserRow>, ScimError> {
  if let Some(user_name) = filter.equality_on("username") {
    return Ok(
      state
        .database
        .find_scim_users(client, ScimUserLookup::UserName(user_name))
        .await?,
    );
  }
  if let Some(external_id) = filter.equality_on("externalid") {
    return Ok(
      state
        .database
        .find_scim_users(client, ScimUserLookup::ExternalId(external_id))
        .await?,
    );
  }
  if let Some(id) = filter.equality_on("id") {
    let Ok(user) = id.parse::<UserId>() else {
      return Ok(Vec::new());
    };
    return Ok(
      state
        .database
        .scim_user(client, user)
        .await?
        .into_iter()
        .collect(),
    );
  }
  let max = i64::from(state.settings.get().scim.max_filter_scan);
  let rows = state
    .database
    .find_scim_users(client, ScimUserLookup::All { limit: max + 1 })
    .await?;
  if rows.len() > usize::try_from(max).unwrap_or(usize::MAX) {
    return Err(ScimError::FilterTooBroad);
  }
  Ok(rows)
}

type GroupsByUser = std::collections::HashMap<UserId, Vec<(ScimGroupId, String)>>;

async fn groups_of(
  state: &AppState,
  client: ScimClientId,
  rows: &[ScimUserRow],
) -> Result<GroupsByUser, ScimError> {
  let users: Vec<UserId> = rows.iter().map(|row| row.user).collect();
  Ok(state.database.scim_groups_of_users(client, &users).await?)
}

fn groups_for(groups: &GroupsByUser, user: UserId) -> &[(ScimGroupId, String)] {
  groups.get(&user).map_or(&[], Vec::as_slice)
}

async fn answer(
  state: &AppState,
  client: ScimClientId,
  row: ScimUserRow,
  status: StatusCode,
) -> Result<ScimResponse, ScimError> {
  let groups = groups_of(state, client, std::slice::from_ref(&row)).await?;
  let body = resource(state, &row, groups_for(&groups, row.user));
  let id = row.user.to_string();
  Ok(ScimResponse::resource(
    status,
    body,
    row.version,
    location(state, "User", &id),
  ))
}

/// The resource as the client sees it, in the canonical case of RFC 7643.
fn resource(state: &AppState, row: &ScimUserRow, groups: &[(ScimGroupId, String)]) -> Value {
  let id = row.user.to_string();
  let attributes = attributes_of(row);
  let mut resource = json!({
    "schemas": [USER_SCHEMA],
    "id": id,
    "userName": row.user_name,
    "active": row.is_active,
    "meta": meta(state, "User", &id, row.created_at, row.updated_at, row.version),
  });
  if let Some(external_id) = &row.maybe_external_id {
    resource["externalId"] = json!(external_id);
  }
  let mut name = serde_json::Map::new();
  if let Some(given) = &row.maybe_given_name {
    name.insert("givenName".into(), json!(given));
  }
  if let Some(family) = &row.maybe_family_name {
    name.insert("familyName".into(), json!(family));
  }
  name.insert("formatted".into(), json!(attributes.shown_name()));
  resource["name"] = Value::Object(name);
  if let Some(display_name) = &row.maybe_display_name {
    resource["displayName"] = json!(display_name);
  }
  if let Some(email) = &row.maybe_email {
    resource["emails"] = json!([{"value": email, "type": "work", "primary": true}]);
  }
  if !groups.is_empty() {
    resource["groups"] = Value::Array(
      groups
        .iter()
        .map(|(group, name)| {
          let group = group.to_string();
          json!({"value": group, "display": name, "$ref": location(state, "Group", &group), "type": "direct"})
        })
        .collect(),
    );
  }
  resource
}

fn internal(error: serde_json::Error) -> ScimError {
  ScimError::Api(ApiError::Internal(error.to_string()))
}
