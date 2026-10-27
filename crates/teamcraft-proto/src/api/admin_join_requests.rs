//! Admins asking to join a private channel under `admin_self_join = owner_approval` (admin.md §2.4,
//! issue #776). The server only stores and relays the request: the channel owner's client decides,
//! and an approval is the owner's own MLS add, which members' clients check like any other.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{ConversationId, UnixMillis, UserSummary, wire_enum::wire_enum};

/// One open request: `user`, a workspace admin as far as the server knows, asks the owners of the
/// private channel `conversation_id` to let them in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminJoinRequestResponse {
  pub conversation_id: ConversationId,
  /// Who asks. Their admin role is the server's word: clients check it against the signed roster
  /// before they offer to approve (admin.md §2.3).
  pub user: UserSummary,
  pub requested_at: UnixMillis,
  /// The server closes the request then (instance setting `admin_join_request_days`).
  pub expires_at: UnixMillis,
}

/// `GET /api/v1/admin-join-requests`: the open requests the caller decides on (channels they own)
/// and the caller's own, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListAdminJoinRequestsResponse {
  pub requests: Vec<AdminJoinRequestResponse>,
}

wire_enum! {
  /// What happened to a request (gateway event `admin_join_request_update`).
  pub enum AdminJoinRequestChange {
    /// An admin asked; the channel's owners decide.
    Requested => "requested",
    /// An owner let the admin in (the server membership is there; the MLS add follows).
    Approved => "approved",
    Declined => "declined",
    /// The admin took it back.
    Withdrawn => "withdrawn",
    /// Its time ran out, or the workspace policy no longer asks for an owner's approval.
    Expired => "expired",
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{Id, Snowflake, UserId, UserKind};

  #[test]
  fn a_request_has_a_stable_shape() {
    let request = AdminJoinRequestResponse {
      conversation_id: ConversationId::from_snowflake(Snowflake::new(7).unwrap()),
      user: UserSummary {
        id: UserId::from_snowflake(Snowflake::new(2).unwrap()),
        maybe_username: None,
        display_name: "Admin A".into(),
        kind: UserKind::Member,
        maybe_email: None,
        maybe_organization: None,
        maybe_has_devices: None,
        is_deactivated: false,
        is_deleted: false,
      },
      requested_at: UnixMillis(1_000),
      expires_at: UnixMillis(2_000),
    };
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["conversation_id"], "7");
    assert_eq!(json["user"]["id"], "2");
    assert_eq!(json["requested_at"], 1_000);
    assert_eq!(json["expires_at"], 2_000);
    let back: AdminJoinRequestResponse = serde_json::from_value(json).unwrap();
    assert_eq!(back, request);
  }

  #[test]
  fn changes_keep_unknown_values() {
    assert_eq!(
      serde_json::to_string(&AdminJoinRequestChange::Declined).unwrap(),
      r#""declined""#
    );
    assert_eq!(
      serde_json::from_str::<AdminJoinRequestChange>(r#""escalated""#).unwrap(),
      AdminJoinRequestChange::Unknown("escalated".into())
    );
  }
}
