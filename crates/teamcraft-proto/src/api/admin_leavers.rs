//! Leavers the identity provider deactivated and who are still in encryption groups, for the
//! owners' and admins' apps that take them out with signed removals (docs/provisioning.md §3).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{DeviceId, UserId};

/// Most leavers in one page of `GET /api/v1/admin/leavers`.
pub const LEAVERS_PAGE_SIZE: u32 = 100;

/// How long a claim keeps other devices off a leaver: long enough to sign the removals from a
/// few hundred channels, short enough that a crashed app holds nobody up for long.
pub const LEAVER_CLAIM_LEASE_SECS: u64 = 600;

/// Query of `GET /api/v1/admin/leavers`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListLeaversQuery {
  /// The asking device: its own claims are not reported as taken.
  #[serde(rename = "device_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_device_id: Option<DeviceId>,
  /// The `next` of the previous page.
  #[serde(rename = "after", default, skip_serializing_if = "Option::is_none")]
  pub maybe_after: Option<UserId>,
}

/// Response of `GET /api/v1/admin/leavers`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListLeaversResponse {
  /// Ordered by user ID.
  pub leavers: Vec<Leaver>,
  /// Set when there may be more: pass it as `after`.
  #[serde(rename = "next", default, skip_serializing_if = "Option::is_none")]
  pub maybe_next: Option<UserId>,
}

/// An account the identity provider deactivated, with devices, memberships or spaces left. The
/// server's word: an app removes nobody on the roster and limits itself (docs/provisioning.md §3.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Leaver {
  pub user_id: UserId,
  /// How long ago the deactivation was, by the server's clock: the apps judge the grace by it, so a
  /// device's wrong clock can't shorten or stretch it.
  pub deactivated_for_secs: u64,
  /// Another device holds a claim on this leaver that hasn't run out.
  #[serde(default)]
  pub is_claimed: bool,
}

/// Query of `POST /api/v1/admin/users/{user_id}/remove-from-chats`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RemoveFromChatsQuery {
  /// The removal comes from an app's automatic claim (docs/provisioning.md §3): refused with 409
  /// unless the user is still a leaver, e.g. because the provider enabled them again meanwhile.
  #[serde(default)]
  pub as_leaver: bool,
}

/// Body of `POST /api/v1/admin/leavers/{user_id}/claim`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ClaimLeaverRequest {
  /// The claiming device: a second claim by the same device renews the lease.
  pub device_id: DeviceId,
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{Id, Snowflake};

  #[test]
  fn leaver_wire_format_is_stable() {
    let leaver = Leaver {
      user_id: UserId::from_snowflake(Snowflake::new(7).unwrap()),
      deactivated_for_secs: 3600,
      is_claimed: true,
    };
    let json = serde_json::to_value(&leaver).unwrap();
    assert_eq!(
      json,
      serde_json::json!({"user_id": "7", "deactivated_for_secs": 3600, "is_claimed": true})
    );
    let without_claim: Leaver =
      serde_json::from_value(serde_json::json!({"user_id": "7", "deactivated_for_secs": 1})).unwrap();
    assert!(!without_claim.is_claimed);
  }

  #[test]
  fn page_without_next_omits_it() {
    let page = ListLeaversResponse {
      leavers: Vec::new(),
      maybe_next: None,
    };
    assert_eq!(
      serde_json::to_value(&page).unwrap(),
      serde_json::json!({"leavers": []})
    );
  }
}
