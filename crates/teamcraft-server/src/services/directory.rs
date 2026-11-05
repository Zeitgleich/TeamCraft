use teamcraft_db::{DirectoryChannelFilter, DirectoryPeopleFilter, UserSearch};
use teamcraft_proto::{
  DirectoryChannel, DirectoryChannelsQuery, DirectoryChannelsResponse, DirectoryFiltersQuery,
  DirectoryFiltersResponse, DirectoryPeopleQuery, DirectoryPeopleResponse, DirectoryPerson,
  ListOrganizationsResponse, SearchUsersResponse, UserId, UserSummary,
};

use super::access::is_guest;
use super::convert::user_summary;
use super::spaces::require_roster_access;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

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
      maybe_after: query.maybe_after,
      limit: page_size + 1,
    })
    .await?;
  let has_more = found.len() as i64 > page_size;
  found.truncate(page_size as usize);
  let mut summaries: Vec<UserSummary> = found
    .iter()
    .map(|person| user_summary(state, &person.user))
    .collect();
  add_emails(state, caller, &mut summaries).await?;
  add_device_presence(state, &mut summaries).await?;
  let maybe_next_after = has_more.then(|| summaries.last().map(|user| user.id)).flatten();
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
  let values = state
    .database
    .directory_filter_values(
      query.maybe_kind.as_ref(),
      query.maybe_space_id,
      i64::from(state.settings.get().limits.max_directory_filter_values),
    )
    .await?;
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
  let page_size = page_size(state);
  let mut found = state
    .database
    .directory_channels(&DirectoryChannelFilter {
      viewer: caller.user.id,
      query: query.query.trim(),
      maybe_space: query.maybe_space_id,
      maybe_after: query.maybe_after,
      limit: page_size + 1,
    })
    .await?;
  let has_more = found.len() as i64 > page_size;
  found.truncate(page_size as usize);
  let maybe_next_after = has_more.then(|| found.last().map(|channel| channel.id)).flatten();
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

/// Marks who has a device, so the directory can tell who hasn't set up the app yet. Only for
/// people the caller may see anyway; one query for the whole page.
async fn add_device_presence(state: &AppState, summaries: &mut [UserSummary]) -> Result<(), ApiError> {
  let ids: Vec<UserId> = summaries.iter().map(|summary| summary.id).collect();
  let with_devices = state.database.users_with_active_devices(&ids).await?;
  for summary in summaries {
    summary.maybe_has_devices = Some(with_devices.contains(&summary.id));
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
