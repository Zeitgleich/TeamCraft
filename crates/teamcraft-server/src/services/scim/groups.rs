//! `/scim/v2/Groups`: groups a SCIM client pushes. Their members are users of the same client.
//! A group's members feed the group mapping (docs/provisioning.md §5): a rule whose group is the
//! SCIM group's display name (`/engineering` for `Engineering`) adds the members to its spaces and
//! public channels, once per member and revision of the rule. Additions only, like the sync's:
//! leaving or deleting a group removes nobody from a channel.

use std::collections::HashMap;

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use teamcraft_db::{ScimGroupCreation, ScimGroupLookup, ScimGroupRow, ScimGroupUpdate, ScimGroupWrite};
use teamcraft_proto::scim::{
  Filter, GROUP_SCHEMA, GroupAttributes, GroupChange, ListQuery, ListResponse, PatchRequest, group_changes,
  normalize_keys, project,
};
use teamcraft_proto::{DeactivationSource, ScimClientId, ScimGroupId, UserId};
use tracing::warn;

use super::error::ScimError;
use super::{ScimCaller, ScimResponse, expected_version, is_not_modified, location, meta, not_modified};
use crate::error::ApiError;
use crate::services::audit;
use crate::services::group_mappings::{apply_to_members, is_rule_for_scim_group};
use crate::state::AppState;

/// The most members a page of a group list carries in all; a page stops early beyond it (it always
/// carries at least one group), and `itemsPerPage` says how many it holds.
const MAX_LISTED_MEMBERS: i64 = 10_000;

pub async fn get_group(
  state: &AppState,
  caller: &ScimCaller,
  id: &str,
  headers: &HeaderMap,
  query: &ListQuery,
) -> Result<Response, ScimError> {
  let row = load(state, caller.client, id).await?;
  if is_not_modified(headers, row.version) {
    return Ok(not_modified(row.version));
  }
  let with_members = wants_members(query);
  let mut answer = answer(state, row, StatusCode::OK, with_members).await?;
  project(
    &mut answer.body,
    query.maybe_attributes.as_deref(),
    query.maybe_excluded_attributes.as_deref(),
  );
  Ok(answer.into_response())
}

pub async fn list_groups(
  state: &AppState,
  caller: &ScimCaller,
  query: &ListQuery,
) -> Result<ScimResponse, ScimError> {
  let (offset, count) = query.window();
  let with_members = wants_members(query);
  let (rows, total) = match query
    .maybe_filter
    .as_deref()
    .map(str::trim)
    .filter(|text| !text.is_empty())
  {
    None => {
      let (rows, total) = state
        .database
        .scim_groups_page(
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
      let needs_members = text.to_ascii_lowercase().contains("members");
      let members = if needs_members {
        within_filter_budget(state, &candidates).await?;
        members_of(state, &candidates).await?
      } else {
        HashMap::new()
      };
      let matching: Vec<ScimGroupRow> = candidates
        .into_iter()
        .filter(|row| {
          let resource = resource(state, row, members.get(&row.id).map(Vec::as_slice));
          normalize_keys(resource).is_ok_and(|normalized| filter.matches(&normalized))
        })
        .collect();
      let total = matching.len();
      (matching.into_iter().skip(offset).take(count).collect(), total)
    },
  };
  let rows = if with_members {
    within_member_budget(state, rows).await?
  } else {
    rows
  };
  let members = if with_members {
    members_of(state, &rows).await?
  } else {
    HashMap::new()
  };
  let resources = rows
    .iter()
    .map(|row| {
      let maybe_members = with_members.then(|| members.get(&row.id).map_or(&[][..], Vec::as_slice));
      let mut resource = resource(state, row, maybe_members);
      project(
        &mut resource,
        query.maybe_attributes.as_deref(),
        query.maybe_excluded_attributes.as_deref(),
      );
      resource
    })
    .collect();
  let body = ListResponse::new(total, offset + 1, resources);
  Ok(ScimResponse::ok(serde_json::to_value(body).map_err(|error| {
    ScimError::Api(ApiError::Internal(error.to_string()))
  })?))
}

pub async fn create_group(
  state: &AppState,
  caller: &ScimCaller,
  body: &Bytes,
) -> Result<ScimResponse, ScimError> {
  let attributes = GroupAttributes::from_resource(&super::parse_body(state, body)?)?;
  let members = member_ids(&attributes.members)?;
  // A retried create finds the group it made: the same display name and external ID.
  if let Some(existing) = state
    .database
    .find_scim_groups(
      caller.client,
      ScimGroupLookup::DisplayName(&attributes.display_name),
    )
    .await?
    .into_iter()
    .next()
  {
    if existing.maybe_external_id == attributes.maybe_external_id {
      let changes = [ScimGroupWrite::ReplaceMembers(members)];
      return Box::pin(update(
        state,
        caller,
        existing,
        &changes,
        None,
        StatusCode::CREATED,
      ))
      .await;
    }
    return Err(ScimError::Uniqueness(
      "this client has another group with the displayName",
    ));
  }
  let row = ScimGroupRow {
    id: state.next_id()?,
    display_name: attributes.display_name.clone(),
    maybe_external_id: attributes.maybe_external_id.clone(),
    version: 1,
    created_at: time::OffsetDateTime::now_utc(),
    updated_at: time::OffsetDateTime::now_utc(),
  };
  let max_members = state.settings.get().scim.max_group_members;
  match state
    .database
    .create_scim_group(caller.client, &row, &members, max_members)
    .await?
  {
    ScimGroupCreation::Created => {},
    ScimGroupCreation::NameTaken => {
      return Err(ScimError::Uniqueness(
        "this client has another group with the displayName",
      ));
    },
    ScimGroupCreation::UnknownMembers(unknown) => return Err(unknown_members(&unknown)),
    ScimGroupCreation::TooManyMembers => return Err(ScimError::TooManyMembers(max_members)),
  }
  audit::record(
    state,
    None,
    "scim_group_created",
    None,
    &json!({"scim_client": caller.client.to_string(), "group": row.id.to_string(), "members": members.len()}),
  )
  .await?;
  apply_mappings(state, &row.display_name, &members).await;
  let row = load_group(state, caller.client, row.id).await?;
  answer(state, row, StatusCode::CREATED, true).await
}

pub async fn replace_group(
  state: &AppState,
  caller: &ScimCaller,
  id: &str,
  headers: &HeaderMap,
  body: &Bytes,
) -> Result<ScimResponse, ScimError> {
  let row = load(state, caller.client, id).await?;
  let attributes = GroupAttributes::from_resource(&super::parse_body(state, body)?)?;
  let changes = [
    ScimGroupWrite::DisplayName(attributes.display_name),
    ScimGroupWrite::ExternalId(attributes.maybe_external_id),
    ScimGroupWrite::ReplaceMembers(member_ids(&attributes.members)?),
  ];
  Box::pin(update(
    state,
    caller,
    row,
    &changes,
    expected_version(headers),
    StatusCode::OK,
  ))
  .await
}

pub async fn patch_group(
  state: &AppState,
  caller: &ScimCaller,
  id: &str,
  headers: &HeaderMap,
  body: &Bytes,
) -> Result<Response, ScimError> {
  let row = load(state, caller.client, id).await?;
  let request: PatchRequest = serde_json::from_value(super::parse_body(state, body)?)
    .map_err(|error| ScimError::InvalidSyntax(error.to_string()))?;
  let request = request.checked()?;
  let changes = group_changes(&request.operations)?
    .into_iter()
    .map(write_of)
    .collect::<Result<Vec<_>, _>>()?;
  let answer = Box::pin(update(
    state,
    caller,
    row,
    &changes,
    expected_version(headers),
    // Entra ID and Okta don't read the answer of a group PATCH, and a large group's members would
    // go back for every change.
    StatusCode::NO_CONTENT,
  ))
  .await?;
  Ok(answer.into_response())
}

pub async fn delete_group(
  state: &AppState,
  caller: &ScimCaller,
  id: &str,
  headers: &HeaderMap,
) -> Result<(), ScimError> {
  let row = load(state, caller.client, id).await?;
  if expected_version(headers).is_some_and(|expected| expected != row.version) {
    return Err(ScimError::VersionMismatch);
  }
  if state.database.delete_scim_group(caller.client, row.id).await? {
    audit::record(
      state,
      None,
      "scim_group_deleted",
      None,
      &json!({"scim_client": caller.client.to_string(), "group": row.id.to_string()}),
    )
    .await?;
  }
  Ok(())
}

/// Applies every rule to the members of the SCIM groups it names: after the rules changed (a
/// changed rule applies once more to everybody, docs/provisioning.md §5).
pub async fn apply_mappings_to_all_groups(state: &AppState) -> Result<u32, ApiError> {
  let mut added: u32 = 0;
  for mapping in state.database.list_group_mappings().await? {
    let name = mapping.group_path.trim_matches('/');
    let members = state.database.scim_group_members_named(name).await?;
    if members.is_empty() {
      continue;
    }
    added =
      added.saturating_add(apply_to_members(state, &mapping, &members, DeactivationSource::Scim).await?);
  }
  Ok(added)
}

async fn update(
  state: &AppState,
  caller: &ScimCaller,
  row: ScimGroupRow,
  changes: &[ScimGroupWrite],
  maybe_expected_version: Option<i64>,
  status: StatusCode,
) -> Result<ScimResponse, ScimError> {
  let max_members = state.settings.get().scim.max_group_members;
  let (added, is_renamed) = match state
    .database
    .update_scim_group(
      caller.client,
      row.id,
      changes,
      maybe_expected_version,
      max_members,
    )
    .await?
  {
    ScimGroupUpdate::Written {
      added, is_renamed, ..
    } => (added, is_renamed),
    ScimGroupUpdate::NotFound => return Err(ScimError::NotFound),
    ScimGroupUpdate::VersionMismatch => return Err(ScimError::VersionMismatch),
    ScimGroupUpdate::NameTaken => {
      return Err(ScimError::Uniqueness(
        "this client has another group with the displayName",
      ));
    },
    ScimGroupUpdate::UnknownMembers(unknown) => return Err(unknown_members(&unknown)),
    ScimGroupUpdate::TooManyMembers => return Err(ScimError::TooManyMembers(max_members)),
  };
  let row = load_group(state, caller.client, row.id).await?;
  if is_renamed {
    // Another name may match other rules: they apply to everybody in the group.
    let everybody: Vec<UserId> = members_of(state, std::slice::from_ref(&row))
      .await?
      .remove(&row.id)
      .unwrap_or_default()
      .into_iter()
      .map(|(user, _)| user)
      .collect();
    apply_mappings(state, &row.display_name, &everybody).await;
  } else if !added.is_empty() {
    apply_mappings(state, &row.display_name, &added).await;
  }
  if is_renamed || !added.is_empty() {
    audit::record(
      state,
      None,
      "scim_group_changed",
      None,
      &json!({"scim_client": caller.client.to_string(), "group": row.id.to_string(), "added": added.len(), "is_renamed": is_renamed}),
    )
    .await?;
  }
  if status == StatusCode::NO_CONTENT {
    return Ok(ScimResponse::no_content(row.version));
  }
  answer(state, row, status, true).await
}

/// Adds `members` (the active ones) to what the rules for the group name map to. A failure is
/// logged: the group change stands, and the next change or a change of the rules applies again.
async fn apply_mappings(state: &AppState, display_name: &str, members: &[UserId]) {
  if let Err(error) = try_apply_mappings(state, display_name, members).await {
    warn!(%error, "applying the group mapping to a SCIM group failed");
  }
}

async fn try_apply_mappings(
  state: &AppState,
  display_name: &str,
  members: &[UserId],
) -> Result<(), ApiError> {
  if members.is_empty() {
    return Ok(());
  }
  let rules: Vec<_> = state
    .database
    .list_group_mappings()
    .await?
    .into_iter()
    .filter(|mapping| is_rule_for_scim_group(mapping, display_name))
    .collect();
  if rules.is_empty() {
    return Ok(());
  }
  let active: Vec<UserId> = state
    .database
    .get_users(members)
    .await?
    .into_iter()
    .filter(|user| !user.is_deactivated && !user.is_deleted)
    .map(|user| user.id)
    .collect();
  for mapping in rules {
    apply_to_members(state, &mapping, &active, DeactivationSource::Scim).await?;
  }
  Ok(())
}

fn write_of(change: GroupChange) -> Result<ScimGroupWrite, ScimError> {
  Ok(match change {
    GroupChange::DisplayName(name) => ScimGroupWrite::DisplayName(name),
    GroupChange::ExternalId(maybe_id) => ScimGroupWrite::ExternalId(maybe_id),
    GroupChange::AddMembers(ids) => ScimGroupWrite::AddMembers(member_ids(&ids)?),
    GroupChange::RemoveMembers(ids) => {
      // Removing somebody who is no user (any more) removes nothing.
      ScimGroupWrite::RemoveMembers(ids.iter().filter_map(|id| id.parse().ok()).collect())
    },
    GroupChange::ReplaceMembers(ids) => ScimGroupWrite::ReplaceMembers(member_ids(&ids)?),
    GroupChange::RemoveAllMembers => ScimGroupWrite::RemoveAllMembers,
  })
}

/// Member values as account IDs; one that is no ID is no user of this client.
fn member_ids(values: &[String]) -> Result<Vec<UserId>, ScimError> {
  values
    .iter()
    .map(|value| {
      value
        .parse()
        .map_err(|_| ScimError::UnknownMembers(truncated(value)))
    })
    .collect()
}

fn unknown_members(unknown: &[UserId]) -> ScimError {
  let listed: Vec<String> = unknown.iter().take(10).map(ToString::to_string).collect();
  ScimError::UnknownMembers(listed.join(", "))
}

/// A value echoed in an error, bounded.
fn truncated(value: &str) -> String {
  value.chars().take(64).collect()
}

/// Whether the answer carries members (unless `excludedAttributes` or `attributes` leave them out).
fn wants_members(query: &ListQuery) -> bool {
  let names = |list: &str| {
    list
      .to_ascii_lowercase()
      .split(',')
      .any(|name| name.trim().ends_with("members"))
  };
  let is_excluded = query.maybe_excluded_attributes.as_deref().is_some_and(names);
  let is_left_out = query.maybe_attributes.as_deref().is_some_and(|list| !names(list));
  !is_excluded && !is_left_out
}

async fn load(state: &AppState, client: ScimClientId, id: &str) -> Result<ScimGroupRow, ScimError> {
  let group: ScimGroupId = id.parse().map_err(|_| ScimError::NotFound)?;
  load_group(state, client, group).await
}

async fn load_group(
  state: &AppState,
  client: ScimClientId,
  group: ScimGroupId,
) -> Result<ScimGroupRow, ScimError> {
  state
    .database
    .scim_group(client, group)
    .await?
    .ok_or(ScimError::NotFound)
}

async fn candidates(
  state: &AppState,
  client: ScimClientId,
  filter: &Filter,
) -> Result<Vec<ScimGroupRow>, ScimError> {
  if let Some(name) = filter.equality_on("displayname") {
    return Ok(
      state
        .database
        .find_scim_groups(client, ScimGroupLookup::DisplayName(name))
        .await?,
    );
  }
  if let Some(external_id) = filter.equality_on("externalid") {
    return Ok(
      state
        .database
        .find_scim_groups(client, ScimGroupLookup::ExternalId(external_id))
        .await?,
    );
  }
  if let Some(id) = filter.equality_on("id") {
    let Ok(group) = id.parse::<ScimGroupId>() else {
      return Ok(Vec::new());
    };
    return Ok(
      state
        .database
        .scim_group(client, group)
        .await?
        .into_iter()
        .collect(),
    );
  }
  let max = i64::from(state.settings.get().scim.max_filter_scan);
  let rows = state
    .database
    .find_scim_groups(client, ScimGroupLookup::All { limit: max + 1 })
    .await?;
  if rows.len() > usize::try_from(max).unwrap_or(usize::MAX) {
    return Err(ScimError::FilterTooBroad);
  }
  Ok(rows)
}

type MembersByGroup = HashMap<ScimGroupId, Vec<(UserId, String)>>;

async fn members_of(state: &AppState, rows: &[ScimGroupRow]) -> Result<MembersByGroup, ScimError> {
  let groups: Vec<ScimGroupId> = rows.iter().map(|row| row.id).collect();
  Ok(state.database.scim_group_members(&groups).await?)
}

/// Refuses (`tooMany`) a filter on members whose candidate groups have more members together than
/// `[scim] max_filter_members`: they would all be read into memory to evaluate it.
async fn within_filter_budget(state: &AppState, candidates: &[ScimGroupRow]) -> Result<(), ScimError> {
  let groups: Vec<ScimGroupId> = candidates.iter().map(|row| row.id).collect();
  let total: i64 = state
    .database
    .scim_group_member_counts(&groups)
    .await?
    .values()
    .sum();
  if total > i64::from(state.settings.get().scim.max_filter_members) {
    return Err(ScimError::FilterTooBroad);
  }
  Ok(())
}

/// The first groups of `rows` whose members together stay within [`MAX_LISTED_MEMBERS`], at
/// least one.
async fn within_member_budget(
  state: &AppState,
  rows: Vec<ScimGroupRow>,
) -> Result<Vec<ScimGroupRow>, ScimError> {
  let groups: Vec<ScimGroupId> = rows.iter().map(|row| row.id).collect();
  let counts = state.database.scim_group_member_counts(&groups).await?;
  let mut listed: i64 = 0;
  let mut kept = Vec::with_capacity(rows.len());
  for row in rows {
    let count = counts.get(&row.id).copied().unwrap_or(0);
    if !kept.is_empty() && listed + count > MAX_LISTED_MEMBERS {
      break;
    }
    listed += count;
    kept.push(row);
  }
  Ok(kept)
}

async fn answer(
  state: &AppState,
  row: ScimGroupRow,
  status: StatusCode,
  with_members: bool,
) -> Result<ScimResponse, ScimError> {
  let members = if with_members {
    members_of(state, std::slice::from_ref(&row)).await?
  } else {
    HashMap::new()
  };
  let maybe_members = with_members.then(|| members.get(&row.id).map_or(&[][..], Vec::as_slice));
  let body = resource(state, &row, maybe_members);
  let id = row.id.to_string();
  Ok(ScimResponse::resource(
    status,
    body,
    row.version,
    location(state, "Group", &id),
  ))
}

fn resource(state: &AppState, row: &ScimGroupRow, maybe_members: Option<&[(UserId, String)]>) -> Value {
  let id = row.id.to_string();
  let mut resource = json!({
    "schemas": [GROUP_SCHEMA],
    "id": id,
    "displayName": row.display_name,
    "meta": meta(state, "Group", &id, row.created_at, row.updated_at, row.version),
  });
  if let Some(external_id) = &row.maybe_external_id {
    resource["externalId"] = json!(external_id);
  }
  if let Some(members) = maybe_members {
    resource["members"] = Value::Array(
      members
        .iter()
        .map(|(user, name)| {
          let user = user.to_string();
          json!({"value": user, "display": name, "$ref": location(state, "User", &user), "type": "User"})
        })
        .collect(),
    );
  }
  resource
}
