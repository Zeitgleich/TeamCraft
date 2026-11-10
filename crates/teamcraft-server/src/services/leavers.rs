//! Leavers for the owners' and admins' apps (docs/provisioning.md §3): who the identity provider
//! deactivated and is still in conversations, spaces or holds devices, and the claims that keep two
//! devices from removing the same person at once. The removal itself is the apps' signed change
//! followed by `POST /admin/users/{id}/remove-from-chats`; nothing here changes a group.

use serde_json::json;
use teamcraft_db::LeaverClaim;
use teamcraft_proto::{
  ClaimLeaverRequest, GatewayEvent, LEAVER_CLAIM_LEASE_SECS, LEAVERS_PAGE_SIZE, Leaver, ListLeaversQuery,
  ListLeaversResponse, Permission, Target, UserId,
};
use time::OffsetDateTime;
use tracing::warn;

use super::access::require;
use super::audit::{self, Target as AuditTarget};
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// One page of leavers.
pub async fn list(
  state: &AppState,
  caller: &AuthenticatedUser,
  query: ListLeaversQuery,
) -> Result<ListLeaversResponse, ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  let page = i64::from(LEAVERS_PAGE_SIZE);
  // Without a device, every live claim counts as somebody else's.
  let rows = state
    .database
    .list_leavers(query.maybe_after, query.maybe_device_id, page)
    .await?;
  // The page is cut before the rank filter, so `next` still pages through everybody.
  let maybe_next = (rows.len() == LEAVERS_PAGE_SIZE as usize)
    .then(|| rows.last().map(|row| row.user_id))
    .flatten();
  let is_owner = caller.role.as_str() == "owner";
  let now = OffsetDateTime::now_utc();
  let leavers = rows
    .into_iter()
    // Only owners act on admins and owners (admin.rs `ensure_may_act_on`): an admin's app isn't
    // shown who it couldn't remove, so it neither fails on them nor asks again.
    .filter(|row| is_owner || !matches!(row.maybe_oidc_role.as_deref(), Some("admin" | "owner")))
    .map(|row| Leaver {
      user_id: row.user_id,
      deactivated_for_secs: u64::try_from((now - row.deactivated_at).whole_seconds()).unwrap_or(0),
      is_claimed: row.is_claimed_by_other,
    })
    .collect();
  Ok(ListLeaversResponse { leavers, maybe_next })
}

/// Claims a leaver for the caller's device, for [`LEAVER_CLAIM_LEASE_SECS`]. The device must be one
/// of the caller's own active devices; the claim is audited, as it is where an automatic removal
/// starts.
pub async fn claim(
  state: &AppState,
  caller: &AuthenticatedUser,
  user: UserId,
  request: ClaimLeaverRequest,
) -> Result<(), ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  if !state
    .database
    .is_active_device_of(caller.user.id, request.device_id)
    .await?
  {
    return Err(ApiError::Forbidden("not one of the caller's devices".into()));
  }
  // The removal at the end needs the same rank, so a claim that couldn't finish is refused up front.
  super::admin::ensure_may_act_on(state, caller, user).await?;
  let lease = time::Duration::seconds(i64::try_from(LEAVER_CLAIM_LEASE_SECS).unwrap_or(i64::MAX));
  match state
    .database
    .claim_leaver(user, request.device_id, lease)
    .await?
  {
    LeaverClaim::Claimed => {},
    LeaverClaim::ClaimedByOther => return Err(ApiError::LeaverClaimed),
    LeaverClaim::NotALeaver => return Err(ApiError::user_not_found()),
  }
  audit::record(
    state,
    Some(caller.user.id),
    "leaver_removal_claimed",
    Some(AuditTarget::new("user", user)),
    &json!({"device_id": request.device_id.to_string()}),
  )
  .await
}

/// The identity provider deactivated `user`: owners' and admins' apps look at the leavers. A
/// failure to find them is logged; the apps also read the list once per gateway connection.
pub async fn announce(state: &AppState) {
  match state.database.list_active_admin_ids().await {
    Ok(admins) => state.hub.publish(admins, &GatewayEvent::LeaverUpdate),
    Err(error) => warn!(%error, "telling admins about a leaver failed"),
  }
}

/// The removal from all chats is done: a claim on nobody is dropped.
pub async fn release(state: &AppState, user: UserId) {
  if let Err(error) = state.database.release_leaver_claim(user).await {
    warn!(%user, %error, "dropping a leaver claim failed; it runs out by itself");
  }
}
