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
  /// These packages replace the device's whole stock: its unused regular packages are dropped in
  /// the same transaction. A device sends this when its packages start announcing something new,
  /// so nobody adds it from one made before (e2ee.md §4.6.2, "Older clients"). Older servers
  /// ignore it and keep the old stock until it is used or expires.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub replaces_stock: bool,
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
  /// A strict channel's roles commit (e2ee.md §4.6.2) tells the server what it set, for the UI and
  /// for a workspace admin outside the group ([`StrictChannelHint`]). Stored with the commit; the
  /// server never looks inside the commit, and no member decides on it.
  #[serde(rename = "strict", default, skip_serializing_if = "Option::is_none")]
  pub maybe_strict: Option<StrictChannelHint>,
  /// For a rewind (`POST …/mls/rewind`, e2ee.md §4.6.2, D4): the refused commit this one replaces,
  /// where the admin's device stopped. The server rewinds only while that commit still holds the
  /// epoch; otherwise it is an epoch conflict, so a second admin continuing at the same time
  /// catches up instead of dropping the first one's continuation. Absent for an ordinary commit
  /// and from older apps (then any commit of the epoch is replaced).
  #[serde(rename = "replaces", default, skip_serializing_if = "Option::is_none")]
  pub maybe_replaces: Option<MlsMessageId>,
}

/// Largest signed roles change a commit's strict hint may carry ([`StrictChannelHint::maybe_change`]):
/// what a device parses of a strict channel's roles (64 roles, the signed entry and headroom).
pub const MAX_STRICT_CHANGE_BYTES: usize = 8 * 1024;
/// Most signed roles changes one `GET …/mls/strict?after=` answer holds
/// ([`MlsStrictHintResponse::changes`]).
pub const MAX_STRICT_CHANGES_PAGE: usize = 64;

/// What a channel's members told the server about its strict roles (e2ee.md §4.6.2): whether the
/// channel is strict, and the version and hash of the roles change that made it so (or turned it
/// off). A hint, never a decision: the group context alone switches strict, and a wrong hint only
/// gets a workspace admin's change refused or the UI shows the wrong state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StrictChannelHint {
  pub is_strict: bool,
  /// The version of the last roles change.
  pub version: u64,
  /// Its chain hash (32 bytes): what the next change names as `prev_hash`.
  pub hash: Base64Bytes,
  /// The signed roles change itself, as it sits in the commit's authenticated data after its
  /// marker (a `SignedStrictRolesChange`, at most [`MAX_STRICT_CHANGE_BYTES`]). Sent with a roles
  /// commit; the server keeps it with the commit under `version` for a device that fell out, which
  /// verifies the chain itself (`GET …/mls/strict?after=`). The server never decides on it, and
  /// never shows it in the hint. Absent from older apps.
  #[serde(rename = "change", default, skip_serializing_if = "Option::is_none")]
  pub maybe_change: Option<Base64Bytes>,
}

/// Query of `GET /api/v1/conversations/{id}/mls/strict`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MlsStrictHintQuery {
  /// Also return the signed roles changes after this version ([`MlsStrictHintResponse::changes`]):
  /// the version of the roles the device knew. Absent: none.
  #[serde(rename = "after", default, skip_serializing_if = "Option::is_none")]
  pub maybe_after: Option<u64>,
}

/// `GET /api/v1/conversations/{id}/mls/strict`: the group's epoch and the strict hint, for a
/// workspace owner or admin outside the group who signs a roles change for the members to commit
/// (e2ee.md §4.6.2, "Role changes while strict"); with `after`, the signed roles changes the
/// members committed since, for a device that fell out ("Falling out in a strict channel").
/// Members, holders of `manage_members` and so anybody who may post a rejoin request read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MlsStrictHintResponse {
  /// The group's current epoch (0 before the first commit): what the change is signed for.
  pub epoch: u64,
  /// Absent while no member told the server anything.
  #[serde(rename = "strict", default, skip_serializing_if = "Option::is_none")]
  pub maybe_strict: Option<StrictChannelHint>,
  /// With `after`: the signed roles changes the members committed after that version, ascending by
  /// version, at most [`MAX_STRICT_CHANGES_PAGE`]. Only what commits still in the group's queue
  /// carried: a rewind drops the changes of the commits it drops. Unverified: the device checks
  /// every link.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub changes: Vec<StrictChainChange>,
  /// More changes follow the last one: ask again after its version.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub has_more: bool,
}

/// One signed roles change of a strict channel's chain ([`MlsStrictHintResponse::changes`]), as
/// the committer told it ([`StrictChannelHint::maybe_change`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StrictChainChange {
  pub version: u64,
  pub change: Base64Bytes,
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
  /// The meeting organizer's device founds the group again and replaces the information that
  /// holds epoch 0 (it could not be joined). Only the organizer, only while the group is in epoch 0.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub is_replacement: bool,
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

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;

  /// The hash of the fixtures: 32 bytes of 7, as base64.
  const HASH: &str = "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=";

  #[test]
  fn a_strict_hint_carries_the_signed_change_only_when_given() {
    let hint = StrictChannelHint {
      is_strict: true,
      version: 3,
      hash: Base64Bytes::new(vec![7; 32]),
      maybe_change: None,
    };
    let plain = json!({"is_strict": true, "version": 3, "hash": HASH});
    assert_eq!(serde_json::to_value(&hint).unwrap(), plain);
    // An older app's hint, and what the server shows, decode without it.
    assert_eq!(serde_json::from_value::<StrictChannelHint>(plain).unwrap(), hint);

    let with_change = StrictChannelHint {
      maybe_change: Some(Base64Bytes::new(b"signed".to_vec())),
      ..hint
    };
    let json = serde_json::to_value(&with_change).unwrap();
    assert_eq!(json["change"], "c2lnbmVk");
    assert_eq!(
      serde_json::from_value::<StrictChannelHint>(json).unwrap(),
      with_change
    );
  }

  #[test]
  fn the_strict_answer_lists_changes_only_when_asked() {
    let bare = MlsStrictHintResponse {
      epoch: 4,
      maybe_strict: None,
      changes: Vec::new(),
      has_more: false,
    };
    assert_eq!(serde_json::to_value(&bare).unwrap(), json!({"epoch": 4}));
    assert_eq!(
      serde_json::from_value::<MlsStrictHintResponse>(json!({"epoch": 4})).unwrap(),
      bare
    );

    let paged = MlsStrictHintResponse {
      changes: vec![StrictChainChange {
        version: 2,
        change: Base64Bytes::new(b"signed".to_vec()),
      }],
      has_more: true,
      ..bare
    };
    assert_eq!(
      serde_json::to_value(&paged).unwrap(),
      json!({"epoch": 4, "changes": [{"version": 2, "change": "c2lnbmVk"}], "has_more": true})
    );
  }

  #[test]
  fn the_strict_query_names_the_known_version() {
    let query = MlsStrictHintQuery { maybe_after: Some(5) };
    assert_eq!(serde_json::to_value(&query).unwrap(), json!({"after": 5}));
    assert_eq!(
      serde_json::to_value(MlsStrictHintQuery::default()).unwrap(),
      json!({})
    );
  }
}
