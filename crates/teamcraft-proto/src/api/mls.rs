use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{Base64Bytes, ConversationId, DeviceId, MlsMessageId};

/// `POST /api/v1/devices/{id}/key-packages`: MLS KeyPackages other members consume to add this device
/// to groups. Each is used once; the last-resort package (replaced on upload) is handed out only
/// when no regular one is left.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UploadKeyPackagesRequest {
  #[serde(default)]
  pub key_packages: Vec<Base64Bytes>,
  #[serde(rename = "last_resort", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_resort: Option<Base64Bytes>,
}

/// How many key packages of a device are left; clients top up when `available` runs low.
/// Packages that expire within the server's renewal window don't count as available, and an
/// expiring last-resort package as none, so clients replace them in time (e2ee.md §4.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct KeyPackageStatusResponse {
  pub available: u32,
  /// Regular packages that expire within the renewal window (still handed out while valid enough).
  #[serde(default)]
  pub expiring_soon: u32,
  /// A last-resort package that stays valid beyond the renewal window.
  pub has_last_resort: bool,
}

/// `POST /api/v1/users/{id}/key-packages/claim`: takes one key package per device of the user, to
/// add them to `conversation_id` (which both must be members of). The device filters keep devices
/// that are already in the group from wasting packages. If the filters leave no device, the answer is
/// an empty list; `key_package_exhausted` means devices matched but none had a package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ClaimKeyPackagesRequest {
  pub conversation_id: ConversationId,
  /// Only these devices of the user (the ones not in the group yet). Absent: all devices.
  #[serde(rename = "only_device_ids", default, skip_serializing_if = "Option::is_none")]
  pub maybe_only_device_ids: Option<Vec<DeviceId>>,
  /// Never claim for these devices, e.g. the claimer's own or ones already in the group.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub exclude_device_ids: Vec<DeviceId>,
}

impl ClaimKeyPackagesRequest {
  /// A claim for all devices of the user.
  pub fn new(conversation_id: ConversationId) -> Self {
    Self {
      conversation_id,
      maybe_only_device_ids: None,
      exclude_device_ids: Vec::new(),
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ClaimedKeyPackage {
  pub device_id: DeviceId,
  pub key_package: Base64Bytes,
  /// The device had no regular package left; this one may be reused by others.
  pub is_last_resort: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ClaimKeyPackagesResponse {
  pub packages: Vec<ClaimedKeyPackage>,
}

wire_enum! {
  /// What an entry of the MLS delivery queue is (RFC 9420).
  pub enum MlsMessageKind {
    Commit => "commit",
    Proposal => "proposal",
    /// Addressed to one device that is added to the group.
    Welcome => "welcome",
    /// Group-encrypted application data, e.g. content key distribution.
    Application => "application",
  }
}

/// One entry of a conversation's MLS delivery queue.
///
/// `epoch` is the epoch the sender was in. A commit moves the group to `epoch + 1`; its welcomes
/// carry the new epoch. The server orders entries but never looks inside `payload`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MlsMessageResponse {
  pub id: MlsMessageId,
  pub conversation_id: ConversationId,
  pub epoch: u64,
  pub kind: MlsMessageKind,
  pub sender_device_id: DeviceId,
  #[serde(
    rename = "recipient_device_id",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_recipient_device_id: Option<DeviceId>,
  pub payload: Base64Bytes,
}

/// A welcome for one device that the commit adds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MlsWelcome {
  pub device_id: DeviceId,
  pub welcome: Base64Bytes,
}

/// `POST /api/v1/conversations/{id}/mls/commits`
///
/// First commit for an epoch wins: if the group already left `epoch`, the request fails with
/// `epoch_conflict` and the client fetches the backlog, rebases and retries. Commit and welcomes are
/// stored atomically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PostMlsCommitRequest {
  pub device_id: DeviceId,
  /// The epoch the commit was created in.
  pub epoch: u64,
  pub commit: Base64Bytes,
  #[serde(default)]
  pub welcomes: Vec<MlsWelcome>,
  /// The group's information for the new epoch (MLS `GroupInfo` with ratchet tree and external
  /// public key), signed by the committer. Stored so a new device of a member can join by external
  /// commit; the server never looks inside (e2ee.md §4.6).
  #[serde(rename = "group_info", default, skip_serializing_if = "Option::is_none")]
  pub maybe_group_info: Option<Base64Bytes>,
}

/// `GET /api/v1/conversations/{id}/mls/group-info`: the newest group information a member published,
/// for the group's current epoch. `404 mls_group_info_not_found` if there is none for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MlsGroupInfoResponse {
  pub epoch: u64,
  pub group_info: Base64Bytes,
}

/// `PUT /api/v1/conversations/{id}/mls/group-info`: a member publishes the group information for
/// the current epoch (when the last commit came without one). `409 epoch_conflict` if the group
/// moved on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PutMlsGroupInfoRequest {
  pub device_id: DeviceId,
  pub epoch: u64,
  pub group_info: Base64Bytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PostMlsCommitResponse {
  pub new_epoch: u64,
  pub commit: MlsMessageResponse,
}

/// `POST /api/v1/conversations/{id}/mls/messages`: a proposal or application message for the current epoch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PostMlsMessageRequest {
  pub device_id: DeviceId,
  pub epoch: u64,
  /// `proposal` or `application`.
  pub kind: MlsMessageKind,
  pub payload: Base64Bytes,
}

/// Query of the MLS backlog endpoints. Welcomes are only returned to the owner of the addressed device.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListMlsMessagesQuery {
  /// Only entries after this one (the last one the client processed).
  #[serde(rename = "after", default, skip_serializing_if = "Option::is_none")]
  pub maybe_after: Option<MlsMessageId>,
  #[serde(rename = "limit", default, skip_serializing_if = "Option::is_none")]
  pub maybe_limit: Option<u32>,
}

/// Entries oldest first, so the client replays them in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListMlsMessagesResponse {
  pub messages: Vec<MlsMessageResponse>,
  pub has_more: bool,
  /// The conversation's current epoch (0 before the first commit).
  pub current_epoch: u64,
}
