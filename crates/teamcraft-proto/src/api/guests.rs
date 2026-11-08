//! Guest lifecycle (PLAN.md §12, issue #212): notices before a guest's membership ends, extending
//! it, and the admin console's overview. The expiry is the server's record, like the membership it
//! ends: nothing here is signed (admin.md §3.2.2).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{SpaceId, UnixMillis, UserId, UserSummary, wire_enum::wire_enum};

/// Body of `POST /api/v1/spaces/{space_id}/guests/{user_id}/extend`. An absolute time, so a retry
/// after a lost response doesn't extend twice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ExtendGuestRequest {
  pub expires_at: UnixMillis,
}

/// A guest membership ending soon, as `GET /api/v1/guests/expiring` lists it for the sponsor, the
/// space's admins and the guest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ExpiringGuestResponse {
  pub space_id: SpaceId,
  pub space_name: String,
  pub user: UserSummary,
  pub expires_at: UnixMillis,
  /// It's the caller's own membership: the notice speaks to them.
  pub is_own: bool,
  /// What the caller may extend this membership to; empty: nothing. Never the guest themself.
  #[serde(default)]
  pub extend_offers: Vec<GuestExtendOffer>,
}

/// One way to extend a guest. The server decides them, so that all apps offer the same and what is
/// offered is accepted. Stable through the day, so a list read again doesn't count as changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GuestExtendOffer {
  /// What to send as `expires_at`.
  pub until: UnixMillis,
  /// By this many days; absent: as far as the caller's limit goes, which is less than the days on
  /// offer, so the apps name the date.
  #[serde(rename = "days", default, skip_serializing_if = "Option::is_none")]
  pub maybe_days: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListExpiringGuestsResponse {
  pub guests: Vec<ExpiringGuestResponse>,
}

wire_enum! {
  /// Which guests the admin console's overview shows.
  pub enum AdminGuestFilter {
    /// Within the longest notice lead time of the policy.
    ExpiringSoon => "expiring_soon",
    /// Nobody responsible: no sponsor, or a deactivated or deleted one.
    NoSponsor => "no_sponsor",
    /// No device ever registered.
    NeverSignedIn => "never_signed_in",
  }
}

/// Query of `GET /api/v1/admin/guests`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListAdminGuestsQuery {
  #[serde(rename = "space_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_space_id: Option<SpaceId>,
  #[serde(rename = "sponsor_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_sponsor_id: Option<UserId>,
  /// Only guests whose sponsor's name or username contains this text (case-insensitive).
  #[serde(rename = "sponsor_query", default, skip_serializing_if = "Option::is_none")]
  pub maybe_sponsor_query: Option<String>,
  #[serde(rename = "filter", default, skip_serializing_if = "Option::is_none")]
  pub maybe_filter: Option<AdminGuestFilter>,
  #[serde(rename = "limit", default, skip_serializing_if = "Option::is_none")]
  pub maybe_limit: Option<u32>,
}

/// One guest membership in the overview: the same person in two spaces is two rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminGuestResponse {
  pub user: UserSummary,
  pub space_id: SpaceId,
  pub space_name: String,
  #[serde(rename = "expires_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_expires_at: Option<UnixMillis>,
  #[serde(rename = "sponsor", default, skip_serializing_if = "Option::is_none")]
  pub maybe_sponsor: Option<UserSummary>,
  pub has_signed_in: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListAdminGuestsResponse {
  pub guests: Vec<AdminGuestResponse>,
  pub has_more: bool,
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;
  use crate::GuestPolicy;

  #[test]
  fn the_filter_keeps_its_wire_names_and_unknown_ones() {
    assert_eq!(json!(AdminGuestFilter::NeverSignedIn), json!("never_signed_in"));
    let newer: AdminGuestFilter = serde_json::from_value(json!("newer")).unwrap();
    assert_eq!(newer, AdminGuestFilter::Unknown("newer".to_owned()));
  }

  #[test]
  fn extending_is_an_absolute_time_on_the_wire() {
    let request = ExtendGuestRequest {
      expires_at: UnixMillis(1000),
    };
    assert_eq!(
      serde_json::to_value(&request).unwrap(),
      json!({"expires_at": 1000})
    );
  }

  #[test]
  fn a_policy_without_the_new_settings_keeps_its_bytes_and_gets_the_defaults() {
    let policy: GuestPolicy = serde_json::from_value(json!({})).unwrap();
    let bytes = serde_json::to_value(&policy).unwrap();
    assert!(bytes.get("expiry_notice_days").is_none());
    assert!(bytes.get("sponsor_extension_days").is_none());
    assert_eq!(policy.effective_notice_days(), vec![14, 3]);
    assert_eq!(policy.effective_sponsor_extension_days(), 120);
    let off: GuestPolicy =
      serde_json::from_value(json!({"expiry_notice_days": [], "sponsor_extension_days": 0})).unwrap();
    assert!(off.effective_notice_days().is_empty());
    assert_eq!(off.effective_sponsor_extension_days(), 0);
  }
}
