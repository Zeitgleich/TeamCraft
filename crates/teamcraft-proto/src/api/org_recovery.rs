//! Organization recovery (e2ee.md §6.3.1). Everything secret is sealed on the clients; the server
//! stores the sealed bytes, counts approvals, relays material to the right device and keeps the
//! audit trail the affected user sees.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{Base64Bytes, DeviceId, RecoveryRequestId, UnixMillis, UserId, UserSummary};

wire_enum! {
  pub enum RecoveryRequestStatus {
    /// Waiting for approvals.
    Pending => "pending",
    /// Enough admins approved; the combining admin's device now collects the shares.
    Approved => "approved",
    /// The backup key reached the recovering device.
    Completed => "completed",
    Denied => "denied",
    /// The person withdrew it.
    Cancelled => "cancelled",
    Expired => "expired",
  }
}

/// A share of the org secret sealed to one device of one admin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SealedShare {
  pub device_id: DeviceId,
  pub sealed: Base64Bytes,
}

/// `PUT /api/v1/org-recovery/shares`: an owner replaces the shares after setting up or rotating the
/// org key. `key_version` only grows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PutOrgRecoverySharesRequest {
  pub key_version: u64,
  pub shares: Vec<SealedShare>,
}

/// `GET /api/v1/org-recovery/shares/mine`: the shares sealed to the caller's devices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OrgRecoverySharesResponse {
  /// 0 if no shares exist.
  pub key_version: u64,
  pub shares: Vec<SealedShare>,
}

/// `POST /api/v1/org-recovery/requests`: a new device of the person asks for recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateRecoveryRequest {
  /// The one-time X25519 public key admins seal the backup key to.
  pub request_public_key: Base64Bytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RecoveryApproval {
  pub admin_id: UserId,
  pub device_id: DeviceId,
  pub approved_at: UnixMillis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RecoveryRequestResponse {
  pub id: RecoveryRequestId,
  pub user: UserSummary,
  /// Computed by the server from the key; the recovering device shows the same one and admins
  /// compare them before approving.
  pub fingerprint: String,
  pub request_public_key: Base64Bytes,
  pub status: RecoveryRequestStatus,
  pub created_at: UnixMillis,
  pub expires_at: UnixMillis,
  pub required_approvals: u32,
  pub approvals: Vec<RecoveryApproval>,
  /// Set once enough admins approved: the approvers seal their shares to this admin's device.
  #[serde(
    rename = "combining_admin_id",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_combining_admin_id: Option<UserId>,
  #[serde(
    rename = "combining_device_id",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_combining_device_id: Option<DeviceId>,
  /// Shares the other approvers handed in so far.
  pub shares_received: u32,
  pub has_result: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListRecoveryRequestsResponse {
  pub requests: Vec<RecoveryRequestResponse>,
}

/// `POST /api/v1/org-recovery/requests/{id}/approve`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ApproveRecoveryRequest {
  /// The admin device the approval is made on; the last approver combines the shares there.
  pub device_id: DeviceId,
}

/// `PUT /api/v1/org-recovery/requests/{id}/share`: an approver's share, sealed to the combining
/// admin's device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PutRecoveryShareRequest {
  pub sealed: Base64Bytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RecoveryShare {
  pub from_admin_id: UserId,
  pub sealed: Base64Bytes,
}

/// `GET /api/v1/org-recovery/requests/{id}/material`: what the combining admin's device needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RecoveryMaterialResponse {
  /// The user's wrapped backup keys as stored with the key bag; the org-wrapped entry is in there.
  pub wrapped_backup_keys: Base64Bytes,
  pub shares: Vec<RecoveryShare>,
}

/// `PUT` and `GET /api/v1/org-recovery/requests/{id}/result`: the backup key sealed to the
/// request key, from the combining admin to the recovering device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RecoveryResultPayload {
  pub sealed_backup_key: Base64Bytes,
}
