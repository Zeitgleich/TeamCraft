//! Organization recovery (e2ee.md §6.3.1, admin.md): the server's part of recovering a backup key
//! with the org recovery key. It keeps sealed shares per admin device, a request with approvals,
//! and relays sealed material; it also writes the audit trail the affected person sees in their
//! security log. All cryptography happens on the clients.

use serde_json::json;
use teamcraft_crypto::org_recovery::RecoveryRequestPublicKey;
use teamcraft_db::{ApprovalOutcome, NewRecoveryRequest, ShareOutcome, StoredRecoveryRequest};
use teamcraft_proto::{
  ApproveRecoveryRequest, Base64Bytes, CreateRecoveryRequest, DeviceId, GatewayEvent,
  ListRecoveryRequestsResponse, OrgRecoverySharesResponse, Permission, PutOrgRecoverySharesRequest,
  PutRecoveryShareRequest, RecoveryApproval, RecoveryMaterialResponse, RecoveryRequestId,
  RecoveryRequestResponse, RecoveryRequestStatus, RecoveryResultPayload, RecoveryShare, SealedShare, Target,
  UserId,
};
use time::{Duration, OffsetDateTime};

use super::access::{is_guest, is_instance_admin, require};
use super::audit::{self, Target as AuditTarget};
use super::convert::{unix_millis, user_summary};
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

const MAX_SHARES: usize = 255;
const PUBLIC_KEY_LEN: usize = 32;

/// An owner stores the sealed shares after setting up (or rotating) the org key. They replace the
/// earlier ones; the key version only grows.
pub async fn put_shares(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: PutOrgRecoverySharesRequest,
) -> Result<(), ApiError> {
  require(state, caller, Permission::SignWorkspace, &Target::Instance)?;
  if request.key_version == 0 || request.shares.is_empty() || request.shares.len() > MAX_SHARES {
    return Err(ApiError::InvalidRequest("invalid shares".into()));
  }
  let max = state.settings.limits.max_recovery_blob_bytes;
  if request
    .shares
    .iter()
    .any(|share| share.sealed.is_empty() || share.sealed.len() > max)
  {
    return Err(ApiError::InvalidRequest(
      "a sealed share has an invalid size".into(),
    ));
  }
  let devices: Vec<DeviceId> = request.shares.iter().map(|share| share.device_id).collect();
  let owners = state.database.admin_devices(&devices).await?;
  let mut stored = Vec::with_capacity(devices.len());
  for share in &request.shares {
    let Some((_, admin)) = owners.iter().find(|(device, _)| *device == share.device_id) else {
      return Err(ApiError::InvalidRequest(
        "shares can only go to active devices of admins".into(),
      ));
    };
    if stored
      .iter()
      .any(|(device, _, _): &(DeviceId, UserId, Vec<u8>)| *device == share.device_id)
    {
      return Err(ApiError::InvalidRequest("a device appears twice".into()));
    }
    stored.push((share.device_id, *admin, share.sealed.as_slice().to_vec()));
  }
  let holders: std::collections::HashSet<UserId> = stored.iter().map(|(_, admin, _)| *admin).collect();
  let required = state.policy.get().org_recovery.admins_required;
  if holders.len() < usize::from(required) {
    return Err(ApiError::InvalidRequest(format!(
      "the shares must reach at least {required} admins"
    )));
  }
  match state
    .database
    .replace_org_recovery_shares(request.key_version, &stored)
    .await?
  {
    ShareOutcome::Stored => {},
    ShareOutcome::StaleVersion { current } => {
      return Err(ApiError::VersionConflict {
        current_version: current,
      });
    },
  }
  audit::record(
    state,
    Some(caller.user.id),
    "org_recovery_shares_stored",
    None,
    &json!({"key_version": request.key_version, "admins": holders.len()}),
  )
  .await?;
  Ok(())
}

/// The shares sealed to the caller's devices.
pub async fn my_shares(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<OrgRecoverySharesResponse, ApiError> {
  require_admin(caller)?;
  let shares = state.database.list_org_recovery_shares_of(caller.user.id).await?;
  Ok(OrgRecoverySharesResponse {
    key_version: shares.iter().map(|share| share.key_version).max().unwrap_or(0),
    shares: shares
      .into_iter()
      .map(|share| SealedShare {
        device_id: share.device,
        sealed: Base64Bytes::from(share.sealed),
      })
      .collect(),
  })
}

/// A new device of the caller asks for recovery with the org key.
pub async fn create_request(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: CreateRecoveryRequest,
) -> Result<RecoveryRequestResponse, ApiError> {
  let policy = state.policy.get();
  let is_enabled = if is_guest(caller) {
    policy.org_recovery.is_enabled_for_guests
  } else {
    policy.org_recovery.is_enabled_for_members
  };
  if !is_enabled {
    return Err(ApiError::Forbidden(
      "organization recovery is switched off for you".into(),
    ));
  }
  if request.request_public_key.len() != PUBLIC_KEY_LEN {
    return Err(ApiError::InvalidRequest(
      "the request key must have 32 bytes".into(),
    ));
  }
  if state.database.org_recovery_key_version().await? == 0 {
    return Err(ApiError::Conflict("organization recovery is not set up".into()));
  }
  if state.database.get_key_bag(caller.user.id).await?.is_none() {
    return Err(ApiError::Conflict("there is no key bag to recover".into()));
  }
  let key: [u8; PUBLIC_KEY_LEN] = request
    .request_public_key
    .as_slice()
    .try_into()
    .map_err(|_| ApiError::InvalidRequest("the request key must have 32 bytes".into()))?;
  let fingerprint = RecoveryRequestPublicKey::from_bytes(key).fingerprint();
  let now = OffsetDateTime::now_utc();
  state.database.expire_recovery_requests(now).await?;
  let id: RecoveryRequestId = state.next_id()?;
  let ttl = Duration::seconds(
    i64::try_from(state.settings.limits.org_recovery_request_ttl_secs).unwrap_or(i64::MAX / 4),
  );
  let is_created = state
    .database
    .create_recovery_request(&NewRecoveryRequest {
      id,
      user: caller.user.id,
      request_public_key: &key,
      fingerprint: &fingerprint,
      required_approvals: u32::from(policy.org_recovery.admins_required),
      expires_at: now + ttl,
    })
    .await?;
  if !is_created {
    return Err(ApiError::Conflict("you have an open recovery request".into()));
  }
  audit_user(
    state,
    caller.user.id,
    caller.user.id,
    "org_recovery_requested",
    id,
    json!({"fingerprint": fingerprint}),
  )
  .await?;
  notify(state, id, caller.user.id).await?;
  response_of(state, id).await
}

/// Open requests, for admins.
pub async fn list_requests(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<ListRecoveryRequestsResponse, ApiError> {
  require_admin(caller)?;
  state
    .database
    .expire_recovery_requests(OffsetDateTime::now_utc())
    .await?;
  let requests = state.database.list_open_recovery_requests().await?;
  let mut out = Vec::with_capacity(requests.len());
  for request in &requests {
    out.push(response(state, request).await?);
  }
  Ok(ListRecoveryRequestsResponse { requests: out })
}

/// One request: admins and the person who asked see it.
pub async fn get_request(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: RecoveryRequestId,
) -> Result<RecoveryRequestResponse, ApiError> {
  let request = load_visible(state, caller, id).await?;
  response(state, &request).await
}

/// An admin approves on one of their devices. The approval that reaches the threshold makes that
/// device the one that combines the shares.
pub async fn approve(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: RecoveryRequestId,
  body: ApproveRecoveryRequest,
) -> Result<RecoveryRequestResponse, ApiError> {
  require_admin(caller)?;
  let request = load_visible(state, caller, id).await?;
  if request.user == caller.user.id {
    return Err(ApiError::Forbidden("you cannot approve your own recovery".into()));
  }
  if !state
    .database
    .is_active_device_of(caller.user.id, body.device_id)
    .await?
  {
    return Err(ApiError::device_not_found());
  }
  if state
    .database
    .list_org_recovery_shares_of(caller.user.id)
    .await?
    .is_empty()
  {
    return Err(ApiError::Conflict("you hold no recovery share".into()));
  }
  match state
    .database
    .approve_recovery_request(id, caller.user.id, body.device_id)
    .await?
  {
    ApprovalOutcome::NotPending => {
      return Err(ApiError::Conflict(
        "the request is not waiting for approvals".into(),
      ));
    },
    ApprovalOutcome::AlreadyApproved => {
      return Err(ApiError::Conflict("you approved this request already".into()));
    },
    ApprovalOutcome::Recorded | ApprovalOutcome::ThresholdReached => {},
  }
  audit_user(
    state,
    caller.user.id,
    request.user,
    "org_recovery_approved",
    id,
    json!({}),
  )
  .await?;
  notify(state, id, request.user).await?;
  response_of(state, id).await
}

/// An admin turns the request down.
pub async fn deny(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: RecoveryRequestId,
) -> Result<(), ApiError> {
  require_admin(caller)?;
  let request = load_visible(state, caller, id).await?;
  if !state.database.close_recovery_request(id, "denied").await? {
    return Err(ApiError::Conflict("the request is not open".into()));
  }
  audit_user(
    state,
    caller.user.id,
    request.user,
    "org_recovery_denied",
    id,
    json!({}),
  )
  .await?;
  notify(state, id, request.user).await
}

/// The person withdraws the request.
pub async fn cancel(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: RecoveryRequestId,
) -> Result<(), ApiError> {
  let request = load_visible(state, caller, id).await?;
  if request.user != caller.user.id {
    return Err(ApiError::Forbidden(
      "only the person who asked can withdraw it".into(),
    ));
  }
  if !state.database.close_recovery_request(id, "cancelled").await? {
    return Err(ApiError::Conflict("the request is not open".into()));
  }
  audit_user(
    state,
    caller.user.id,
    request.user,
    "org_recovery_cancelled",
    id,
    json!({}),
  )
  .await?;
  notify(state, id, request.user).await
}

/// An approver (other than the combining admin) hands in their share, sealed to the combining
/// admin's device.
pub async fn put_share(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: RecoveryRequestId,
  body: PutRecoveryShareRequest,
) -> Result<(), ApiError> {
  require_admin(caller)?;
  let request = load_visible(state, caller, id).await?;
  check_blob(state, &body.sealed)?;
  if request.status != "approved" {
    return Err(ApiError::Conflict("the request is not waiting for shares".into()));
  }
  if !request
    .approvals
    .iter()
    .any(|approval| approval.admin == caller.user.id)
  {
    return Err(ApiError::Forbidden("only approvers hand in shares".into()));
  }
  if request.maybe_combining_admin == Some(caller.user.id) {
    return Err(ApiError::InvalidRequest(
      "the combining admin keeps their own share".into(),
    ));
  }
  state
    .database
    .put_recovery_request_share(id, caller.user.id, body.sealed.as_slice())
    .await?;
  notify(state, id, request.user).await
}

/// What the combining admin's device needs: the other shares and the person's wrapped backup keys.
pub async fn material(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: RecoveryRequestId,
) -> Result<RecoveryMaterialResponse, ApiError> {
  let request = load_combiner_request(state, caller, id).await?;
  if (request.shares_received as usize) + 1 < request.required_approvals as usize {
    return Err(ApiError::Conflict(
      "the other approvers have not handed in their shares yet".into(),
    ));
  }
  let key_bag = state
    .database
    .get_key_bag(request.user)
    .await?
    .ok_or_else(ApiError::key_bag_not_found)?;
  let shares = state.database.list_recovery_request_shares(id).await?;
  audit_user(
    state,
    caller.user.id,
    request.user,
    "org_recovery_material_fetched",
    id,
    json!({}),
  )
  .await?;
  Ok(RecoveryMaterialResponse {
    wrapped_backup_keys: Base64Bytes::from(key_bag.wrapped_backup_keys),
    shares: shares
      .into_iter()
      .map(|(from_admin_id, sealed)| RecoveryShare {
        from_admin_id,
        sealed: Base64Bytes::from(sealed),
      })
      .collect(),
  })
}

/// The combining admin hands over the backup key, sealed to the request key.
pub async fn put_result(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: RecoveryRequestId,
  body: RecoveryResultPayload,
) -> Result<(), ApiError> {
  let request = load_combiner_request(state, caller, id).await?;
  check_blob(state, &body.sealed_backup_key)?;
  if !state
    .database
    .complete_recovery_request(id, body.sealed_backup_key.as_slice(), OffsetDateTime::now_utc())
    .await?
  {
    return Err(ApiError::Conflict(
      "the request is not waiting for the result".into(),
    ));
  }
  audit_user(
    state,
    caller.user.id,
    request.user,
    "org_recovery_completed",
    id,
    json!({}),
  )
  .await?;
  notify(state, id, request.user).await
}

/// The recovering device fetches the sealed backup key.
pub async fn get_result(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: RecoveryRequestId,
) -> Result<RecoveryResultPayload, ApiError> {
  let request = load_visible(state, caller, id).await?;
  if request.user != caller.user.id {
    return Err(ApiError::Forbidden(
      "only the person who asked gets the result".into(),
    ));
  }
  let sealed = state
    .database
    .get_recovery_result(id)
    .await?
    .ok_or_else(|| ApiError::Conflict("the result is not ready".into()))?;
  Ok(RecoveryResultPayload {
    sealed_backup_key: Base64Bytes::from(sealed),
  })
}

async fn load_combiner_request(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: RecoveryRequestId,
) -> Result<StoredRecoveryRequest, ApiError> {
  require_admin(caller)?;
  let request = load_visible(state, caller, id).await?;
  if request.status != "approved" {
    return Err(ApiError::Conflict("the request is not approved".into()));
  }
  if request.maybe_combining_admin != Some(caller.user.id) {
    return Err(ApiError::Forbidden("only the combining admin may do that".into()));
  }
  Ok(request)
}

/// The request, if the caller is an admin or the person who asked.
async fn load_visible(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: RecoveryRequestId,
) -> Result<StoredRecoveryRequest, ApiError> {
  state
    .database
    .expire_recovery_requests(OffsetDateTime::now_utc())
    .await?;
  let request = state
    .database
    .get_recovery_request(id)
    .await?
    .ok_or_else(ApiError::recovery_request_not_found)?;
  if request.user != caller.user.id && !is_instance_admin(caller) {
    return Err(ApiError::recovery_request_not_found());
  }
  Ok(request)
}

fn require_admin(caller: &AuthenticatedUser) -> Result<(), ApiError> {
  if is_instance_admin(caller) {
    Ok(())
  } else {
    Err(ApiError::Forbidden("only admins can do that".into()))
  }
}

fn check_blob(state: &AppState, blob: &Base64Bytes) -> Result<(), ApiError> {
  if blob.is_empty() || blob.len() > state.settings.limits.max_recovery_blob_bytes {
    return Err(ApiError::InvalidRequest(
      "the sealed value has an invalid size".into(),
    ));
  }
  Ok(())
}

/// Writes a recovery step into the audit log with the affected person as target, which is what
/// their security log shows.
async fn audit_user(
  state: &AppState,
  actor: UserId,
  affected: UserId,
  action: &str,
  request: RecoveryRequestId,
  mut detail: serde_json::Value,
) -> Result<(), ApiError> {
  detail["request_id"] = json!(request.to_string());
  audit::record(
    state,
    Some(actor),
    action,
    Some(AuditTarget::new("user", affected)),
    &detail,
  )
  .await
}

/// Tells the admins and the person that the request changed.
async fn notify(state: &AppState, id: RecoveryRequestId, requester: UserId) -> Result<(), ApiError> {
  let mut audience = state.database.list_active_admin_ids().await?;
  audience.push(requester);
  state
    .hub
    .publish(audience, &GatewayEvent::OrgRecoveryUpdate { request_id: id });
  Ok(())
}

async fn response_of(state: &AppState, id: RecoveryRequestId) -> Result<RecoveryRequestResponse, ApiError> {
  let request = state
    .database
    .get_recovery_request(id)
    .await?
    .ok_or_else(ApiError::recovery_request_not_found)?;
  response(state, &request).await
}

async fn response(
  state: &AppState,
  request: &StoredRecoveryRequest,
) -> Result<RecoveryRequestResponse, ApiError> {
  let user = state
    .database
    .get_user(request.user)
    .await?
    .ok_or_else(ApiError::user_not_found)?;
  Ok(RecoveryRequestResponse {
    id: request.id,
    user: user_summary(state, &user),
    fingerprint: request.fingerprint.clone(),
    request_public_key: Base64Bytes::from(request.request_public_key.clone()),
    status: RecoveryRequestStatus::from(request.status.clone()),
    created_at: unix_millis(request.created_at),
    expires_at: unix_millis(request.expires_at),
    required_approvals: request.required_approvals,
    approvals: request
      .approvals
      .iter()
      .map(|approval| RecoveryApproval {
        admin_id: approval.admin,
        device_id: approval.device,
        approved_at: unix_millis(approval.approved_at),
      })
      .collect(),
    maybe_combining_admin_id: request.maybe_combining_admin,
    maybe_combining_device_id: request.maybe_combining_device,
    shares_received: request.shares_received,
    has_result: request.has_result,
  })
}
