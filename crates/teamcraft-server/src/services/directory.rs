use std::collections::HashMap;
use std::fmt::Display;
use std::str::FromStr;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use teamcraft_db::{DirectoryChannelFilter, DirectoryFilterValues, DirectoryPeopleFilter, UserSearch};
use teamcraft_proto::{
  DirectoryChannel, DirectoryChannelsQuery, DirectoryChannelsResponse, DirectoryFiltersQuery,
  DirectoryFiltersResponse, DirectoryPeopleQuery, DirectoryPeopleResponse, DirectoryPerson,
  ListOrganizationsResponse, SearchUsersResponse, SpaceId, UserId, UserKind, UserSummary,
};

use super::access::is_guest;
use super::convert::user_summary;
use super::spaces::require_roster_access;
use super::validate_text;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Sets of filter values kept at most; a full cache starts over. One per kind and space asked for.
const MAX_CACHED_FILTER_SETS: usize = 256;

/// Directory search. Guests only find people they share a conversation with (PLAN.md §12).
pub async fn search_users(
  state: &AppState,
  caller: &AuthenticatedUser,
  query: &str,
  maybe_organization: Option<&str>,
) -> Result<SearchUsersResponse, ApiError> {
  let users = state
    .database
    .search_users(&UserSearch {
      viewer: caller.user.id,
      query: query.trim(),
      is_restricted_to_shared: is_guest(caller),
      maybe_organization,
      limit: i64::from(state.settings.get().limits.max_directory_results),
    })
    .await?;
  let mut summaries: Vec<UserSummary> = users.iter().map(|user| user_summary(state, user)).collect();
  add_emails(state, caller, &mut summaries).await?;
  add_device_presence(state, &mut summaries).await?;
  Ok(SearchUsersResponse { users: summaries })
}

/// One page of the people directory. Guests only find people they share a conversation with, like
/// in [`search_users`], and can't narrow it to a space's roster.
pub async fn list_people(
  state: &AppState,
  caller: &AuthenticatedUser,
  query: &DirectoryPeopleQuery,
) -> Result<DirectoryPeopleResponse, ApiError> {
  for text in [
    Some(query.query.as_str()),
    query.maybe_organization.as_deref(),
    query.maybe_department.as_deref(),
    query.maybe_title.as_deref(),
  ]
  .into_iter()
  .flatten()
  {
    check_search_text(state, text)?;
  }
  let maybe_after = query
    .maybe_after
    .as_deref()
    .map(|after| parse_cursor(state, after))
    .transpose()?;
  if let Some(space) = query.maybe_space_id {
    require_roster_access(state, caller, space).await?;
  }
  let page_size = page_size(state);
  let mut found = state
    .database
    .directory_people(&DirectoryPeopleFilter {
      viewer: caller.user.id,
      query: query.query.trim(),
      is_restricted_to_shared: is_guest(caller),
      maybe_kind: query.maybe_kind.clone(),
      maybe_organization: query.maybe_organization.as_deref(),
      maybe_department: query.maybe_department.as_deref(),
      maybe_title: query.maybe_title.as_deref(),
      maybe_space: query.maybe_space_id,
      hides_full_names: !state.policy.get().directory.shows_full_name,
      maybe_after: maybe_after.as_ref().map(|(name, id)| (name.as_str(), *id)),
      limit: page_size + 1,
    })
    .await?;
  let has_more = found.len() as i64 > page_size;
  found.truncate(page_size as usize);
  let maybe_next_after = has_more
    .then(|| {
      found
        .last()
        .map(|person| cursor(&person.sort_name, person.user.id))
    })
    .flatten();
  let mut summaries: Vec<UserSummary> = found
    .iter()
    .map(|person| user_summary(state, &person.user))
    .collect();
  add_emails(state, caller, &mut summaries).await?;
  add_device_presence(state, &mut summaries).await?;
  let people = found
    .into_iter()
    .zip(summaries)
    .map(|(person, user)| DirectoryPerson {
      presence: state.hub.last_presence(user.id).state,
      user,
      maybe_title: person.maybe_title,
      maybe_department: person.maybe_department,
    })
    .collect();
  Ok(DirectoryPeopleResponse {
    people,
    maybe_next_after,
  })
}

/// The values the people filters offer; guests have no directory and get none.
pub async fn list_filters(
  state: &AppState,
  caller: &AuthenticatedUser,
  query: &DirectoryFiltersQuery,
) -> Result<DirectoryFiltersResponse, ApiError> {
  if is_guest(caller) {
    return Ok(DirectoryFiltersResponse::default());
  }
  if let Some(space) = query.maybe_space_id {
    require_roster_access(state, caller, space).await?;
  }
  let limits = &state.settings.get().limits;
  let max_age = Duration::from_secs(limits.directory_filters_cache_secs);
  let key = (query.maybe_kind.clone(), query.maybe_space_id);
  let values = match state.directory_filters.get(&key, max_age) {
    Some(values) => values,
    None => {
      let values = state
        .database
        .directory_filter_values(
          query.maybe_kind.as_ref(),
          query.maybe_space_id,
          i64::from(limits.max_directory_filter_values),
        )
        .await?;
      state.directory_filters.put(key, values.clone());
      values
    },
  };
  Ok(DirectoryFiltersResponse {
    organizations: values.organizations,
    departments: values.departments,
    titles: values.titles,
  })
}

/// One page of channels across the caller's spaces. Guests have no channel browser (PLAN.md §1).
pub async fn list_channels(
  state: &AppState,
  caller: &AuthenticatedUser,
  query: &DirectoryChannelsQuery,
) -> Result<DirectoryChannelsResponse, ApiError> {
  if is_guest(caller) {
    return Err(ApiError::Forbidden(
      "the channel directory is not available to guests".into(),
    ));
  }
  check_search_text(state, &query.query)?;
  let maybe_after = query
    .maybe_after
    .as_deref()
    .map(|after| parse_cursor(state, after))
    .transpose()?;
  let page_size = page_size(state);
  let mut found = state
    .database
    .directory_channels(&DirectoryChannelFilter {
      viewer: caller.user.id,
      query: query.query.trim(),
      maybe_space: query.maybe_space_id,
      maybe_after: maybe_after.as_ref().map(|(name, id)| (name.as_str(), *id)),
      limit: page_size + 1,
    })
    .await?;
  let has_more = found.len() as i64 > page_size;
  found.truncate(page_size as usize);
  let maybe_next_after = has_more
    .then(|| found.last().map(|channel| cursor(&channel.name, channel.id)))
    .flatten();
  Ok(DirectoryChannelsResponse {
    channels: found
      .into_iter()
      .map(|channel| DirectoryChannel {
        id: channel.id,
        space_id: channel.space_id,
        space_name: channel.space_name,
        name: channel.name,
        maybe_topic: channel.maybe_topic,
        visibility: channel.visibility,
        member_count: u32::try_from(channel.member_count).unwrap_or(u32::MAX),
        is_member: channel.is_member,
      })
      .collect(),
    maybe_next_after,
  })
}

/// People or channels one directory page holds.
pub(super) fn page_size(state: &AppState) -> i64 {
  i64::from(state.settings.get().limits.max_directory_results)
}

/// Text a client searches or filters by. Postgres refuses NUL in text, which would be a 500.
pub(super) fn check_search_text(state: &AppState, text: &str) -> Result<(), ApiError> {
  if text.contains('\0') {
    return Err(ApiError::InvalidRequest(
      "search text holds a NUL character".into(),
    ));
  }
  validate_text(text, "search text", state.settings.get().limits.max_text_length)
}

/// A page's end, as the next request gives it back: the ID and the sort name. The caller has just
/// been shown this name, so the cursor reveals nothing.
fn cursor(name: &str, id: impl Display) -> String {
  format!("{id}:{name}")
}

fn parse_cursor<T: FromStr>(state: &AppState, after: &str) -> Result<(String, T), ApiError> {
  check_search_text(state, after)?;
  let invalid = || ApiError::InvalidRequest("invalid after".into());
  let (id, name) = after.split_once(':').ok_or_else(invalid)?;
  Ok((name.to_owned(), id.parse().map_err(|_| invalid())?))
}

/// The values of the directory filters for a little while: computing them reads all users, and every
/// visit to People asks. Per server and safe to lose; who may ask is checked before the cache.
#[derive(Default)]
pub struct FilterValuesCache {
  entries: Mutex<HashMap<FilterValuesKey, (Instant, DirectoryFilterValues)>>,
}

type FilterValuesKey = (Option<UserKind>, Option<SpaceId>);

impl FilterValuesCache {
  fn get(&self, key: &FilterValuesKey, max_age: Duration) -> Option<DirectoryFilterValues> {
    let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
    let (stored_at, values) = entries.get(key)?;
    (stored_at.elapsed() < max_age).then(|| values.clone())
  }

  fn put(&self, key: FilterValuesKey, values: DirectoryFilterValues) {
    let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
    if entries.len() >= MAX_CACHED_FILTER_SETS {
      entries.clear();
    }
    entries.insert(key, (Instant::now(), values));
  }
}

/// The organizations for the directory filter; guests have no directory and get none.
pub async fn list_organizations(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<ListOrganizationsResponse, ApiError> {
  let organizations = if is_guest(caller) {
    Vec::new()
  } else {
    state.database.list_organizations().await?
  };
  Ok(ListOrganizationsResponse { organizations })
}

/// One user; guests only see themselves and people they share a conversation with.
pub async fn get_user(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: UserId,
) -> Result<UserSummary, ApiError> {
  if let Some(tombstone) = state.database.get_user(id).await?.filter(|user| user.is_deleted) {
    // A deleted account (issue #630) still names old messages; its tombstone holds nothing about
    // the person, so clients may show it as "Deleted user" instead of a cached name.
    return Ok(user_summary(state, &tombstone));
  }
  let mut summary = vec![user_summary(state, &visible_user(state, caller, id).await?)];
  add_emails(state, caller, &mut summary).await?;
  add_device_presence(state, &mut summary).await?;
  summary
    .pop()
    .ok_or_else(|| ApiError::Internal("summary missing".into()))
}

/// Adds e-mail addresses if the directory policy shows them. Guests never see any.
async fn add_emails(
  state: &AppState,
  caller: &AuthenticatedUser,
  summaries: &mut [UserSummary],
) -> Result<(), ApiError> {
  if !state.policy.get().directory.shows_email || is_guest(caller) {
    return Ok(());
  }
  let ids: Vec<UserId> = summaries.iter().map(|summary| summary.id).collect();
  let emails = state.database.get_user_emails(&ids).await?;
  for summary in summaries {
    summary.maybe_email = emails.get(&summary.id).cloned();
  }
  Ok(())
}

/// Marks who has a device and who never signed in (an account made from the identity provider,
/// docs/provisioning.md §6), so the directory and the pickers can tell who hasn't set up the app
/// yet. Only for people the caller may see anyway; two queries for the whole page.
async fn add_device_presence(state: &AppState, summaries: &mut [UserSummary]) -> Result<(), ApiError> {
  let ids: Vec<UserId> = summaries.iter().map(|summary| summary.id).collect();
  let with_devices = state.database.users_with_active_devices(&ids).await?;
  let awaiting = state.database.users_awaiting_first_sign_in(&ids).await?;
  for summary in summaries {
    summary.maybe_has_devices = Some(with_devices.contains(&summary.id));
    summary.is_awaiting_first_sign_in = awaiting.contains(&summary.id);
  }
  Ok(())
}

/// The active user the caller may know about, or `user_not_found`.
pub async fn visible_user(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: UserId,
) -> Result<teamcraft_db::User, ApiError> {
  let user = state
    .database
    .get_user(id)
    .await?
    .filter(|user| !user.is_deactivated);
  let Some(user) = user else {
    return Err(ApiError::user_not_found());
  };
  if is_guest(caller)
    && user.id != caller.user.id
    && !state
      .database
      .users_share_conversation(caller.user.id, id)
      .await?
  {
    return Err(ApiError::user_not_found());
  }
  Ok(user)
}

#[cfg(test)]
mod filter_cache_tests {
  use super::*;

  fn values(title: &str) -> DirectoryFilterValues {
    DirectoryFilterValues {
      titles: vec![title.to_owned()],
      ..DirectoryFilterValues::default()
    }
  }

  #[test]
  fn values_are_reused_per_kind_until_they_are_too_old() {
    let cache = FilterValuesCache::default();
    let long = Duration::from_secs(60);
    let key = (Some(UserKind::Guest), None);
    assert_eq!(cache.get(&key, long), None);
    cache.put(key.clone(), values("Lead"));
    assert_eq!(cache.get(&key, long), Some(values("Lead")));
    assert_eq!(cache.get(&(Some(UserKind::Member), None), long), None);
    assert_eq!(cache.get(&key, Duration::ZERO), None, "0 means never reused");
  }

  #[test]
  fn the_cache_is_bounded() {
    let cache = FilterValuesCache::default();
    for raw in 0..=MAX_CACHED_FILTER_SETS {
      let space: SpaceId = (raw + 1).to_string().parse().unwrap();
      cache.put((None, Some(space)), values("Lead"));
    }
    assert!(cache.entries.lock().unwrap().len() <= MAX_CACHED_FILTER_SETS);
  }
}
