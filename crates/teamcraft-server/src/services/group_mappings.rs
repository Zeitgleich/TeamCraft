//! Identity provider groups to spaces and public channels (docs/provisioning.md §5): the rules the
//! console keeps. The sync applies them (`oidc::apply_group_mappings`).

use std::collections::{HashMap, HashSet};

use serde_json::json;
use teamcraft_db::{GroupMapping, NewGroupMapping};
use teamcraft_proto::{
  GroupMappingRule, GroupMappingsResponse, MAX_GROUP_MAPPINGS, MAX_GROUP_PATH_CHARS, MAX_MAPPED_CHANNELS,
  MAX_MAPPED_SPACES, Permission, PutGroupMappingsRequest, Target,
};

use super::access::require;
use super::audit;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

pub async fn list(state: &AppState, caller: &AuthenticatedUser) -> Result<GroupMappingsResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  read(state).await
}

/// Replaces the rules. A rule whose spaces or channels changed applies once more to everybody in
/// its group; the sync is asked for a run so it takes effect within seconds.
pub async fn put(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: PutGroupMappingsRequest,
) -> Result<GroupMappingsResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let rules = validated(state, &request.mappings).await?;
  let existing: HashMap<String, GroupMapping> = state
    .database
    .list_group_mappings()
    .await?
    .into_iter()
    .map(|mapping| (mapping.group_path.clone(), mapping))
    .collect();
  let mut mappings = Vec::with_capacity(rules.len());
  for (group_path, rule) in rules {
    let id = match existing.get(&group_path) {
      Some(mapping) => mapping.id,
      None => state.next_id()?,
    };
    mappings.push(NewGroupMapping {
      id,
      group_path,
      space_ids: rule.space_ids,
      channel_ids: rule.channel_ids,
    });
  }
  state.database.replace_group_mappings(&mappings).await?;
  audit::record(
    state,
    Some(caller.user.id),
    "group_mappings_changed",
    None,
    &json!({"rules": mappings.len()}),
  )
  .await?;
  state.oidc_sync.request_run();
  read(state).await
}

async fn read(state: &AppState) -> Result<GroupMappingsResponse, ApiError> {
  let mappings = state
    .database
    .list_group_mappings()
    .await?
    .into_iter()
    .map(|mapping| GroupMappingRule {
      group_path: mapping.group_path,
      space_ids: mapping.space_ids,
      channel_ids: mapping.channel_ids,
    })
    .collect();
  Ok(GroupMappingsResponse { mappings })
}

/// The rules with normalized paths, or the first problem, naming the rule by its position (1-based)
/// so the console can put it at its row.
async fn validated(
  state: &AppState,
  rules: &[GroupMappingRule],
) -> Result<Vec<(String, GroupMappingRule)>, ApiError> {
  if rules.len() > MAX_GROUP_MAPPINGS {
    return Err(ApiError::InvalidRequest(format!(
      "at most {MAX_GROUP_MAPPINGS} rules"
    )));
  }
  let mut paths = HashSet::new();
  let mut checked = Vec::with_capacity(rules.len());
  for (index, rule) in rules.iter().enumerate() {
    let number = index + 1;
    let path = rule.normalized_path();
    if path == "/" || path.chars().count() > MAX_GROUP_PATH_CHARS {
      return Err(ApiError::InvalidRequest(format!(
        "rule {number}: the group path must have 1 to {MAX_GROUP_PATH_CHARS} characters"
      )));
    }
    if !paths.insert(path.clone()) {
      return Err(ApiError::InvalidRequest(format!(
        "rule {number}: the group {path} has another rule"
      )));
    }
    if rule.space_ids.is_empty() || rule.space_ids.len() > MAX_MAPPED_SPACES {
      return Err(ApiError::InvalidRequest(format!(
        "rule {number}: name 1 to {MAX_MAPPED_SPACES} spaces"
      )));
    }
    if rule.channel_ids.len() > MAX_MAPPED_CHANNELS {
      return Err(ApiError::InvalidRequest(format!(
        "rule {number}: at most {MAX_MAPPED_CHANNELS} channels"
      )));
    }
    let (spaces, channels) = state
      .database
      .group_mapping_targets(&rule.space_ids, &rule.channel_ids)
      .await?;
    if spaces.len() != rule.space_ids.iter().collect::<HashSet<_>>().len() {
      return Err(ApiError::InvalidRequest(format!(
        "rule {number}: a space doesn't exist"
      )));
    }
    if channels.len() != rule.channel_ids.iter().collect::<HashSet<_>>().len() {
      return Err(ApiError::InvalidRequest(format!(
        "rule {number}: a channel is not a public channel of the rule's spaces"
      )));
    }
    checked.push((path, rule.clone()));
  }
  Ok(checked)
}
