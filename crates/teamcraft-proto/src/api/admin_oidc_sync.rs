//! How the sync with the identity provider stands, in the admin console (issue #210,
//! ARCHITECTURE.md §5.1.1, admin.md §3.2.3). Names and counts only: the error is a fixed code,
//! never the provider's answer, and no token or address appears.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{UnixMillis, UserId};

wire_enum! {
  /// Whether the sync runs on this instance.
  pub enum OidcSyncState {
    /// The service account is configured and the sync runs.
    Active => "active",
    /// The service account is configured, but `[oidc.sync] is_enabled = false` switches it off.
    SwitchedOff => "switched_off",
    /// No service account (`[oidc.sync] client_secret`) is configured.
    NotConfigured => "not_configured",
  }
}

wire_enum! {
  /// What the sync does with its findings.
  pub enum OidcSyncMode {
    /// Applies the changes.
    Apply => "apply",
    /// Only reports what it would change.
    DryRun => "dry_run",
  }
}

wire_enum! {
  /// How the last run ended.
  pub enum OidcSyncOutcome {
    /// Finished, everything applied.
    Ok => "ok",
    /// Finished, but some changes failed; the next run tries them again.
    Partial => "partial",
    /// Finished, and the safety brake is holding deactivations back.
    Held => "held",
    /// Only reported what it would change.
    DryRun => "dry_run",
    /// Could not read the provider or the database: nothing was changed.
    Failed => "failed",
  }
}

wire_enum! {
  /// Why a run failed. A fixed list: the value is shown, never built from a provider's answer.
  pub enum OidcSyncError {
    /// The provider or its network did not answer.
    ProviderUnreachable => "provider_unreachable",
    /// The provider refused the service account's credentials or permissions.
    ProviderRefused => "provider_refused",
    /// The provider answered something else than expected.
    ProviderInvalidAnswer => "provider_invalid_answer",
    /// The provider listed no users although the instance has some: a misconfiguration, not a fact.
    ProviderListedNoUsers => "provider_listed_no_users",
    /// The database refused.
    Database => "database",
  }
}

wire_enum! {
  /// Why the sync wants to deactivate someone.
  pub enum DeactivationCause {
    /// Removed from the provider.
    Removed => "removed",
    /// Disabled at the provider.
    Disabled => "disabled",
    /// No TeamCraft role left at the provider.
    NoRole => "no_role",
  }
}

wire_enum! {
  /// Who caused a deactivation (audit log, `source`).
  pub enum DeactivationSource {
    /// An admin in the console.
    Manual => "manual",
    /// The sync with the identity provider.
    OidcSync => "oidc_sync",
    /// A SCIM client (reserved).
    Scim => "scim",
    /// The server itself: a meeting guest's access ended, a bot's creator left.
    System => "system",
  }
}

/// The safety brake as set: one run may deactivate at most `max(min_allowed, max_percent % of the
/// active users)` and never everybody.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OidcSyncBrakeSettings {
  pub max_percent: u32,
  pub min_allowed: u32,
}

/// The last run of the sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OidcSyncRunResponse {
  pub started_at: UnixMillis,
  pub finished_at: UnixMillis,
  pub duration_ms: i64,
  pub outcome: OidcSyncOutcome,
  #[serde(rename = "error", default, skip_serializing_if = "Option::is_none")]
  pub maybe_error: Option<OidcSyncError>,
  /// Runs that failed in a row.
  pub consecutive_failures: u32,
  pub deactivated: u32,
  pub reactivated: u32,
  pub role_changes: u32,
  pub groups_changed: u32,
  /// Changes that failed in this run; the next run tries them again.
  pub failed: u32,
  /// Accounts made before their first sign-in (docs/provisioning.md §6). Absent (0) from older
  /// servers.
  #[serde(default)]
  pub created: u32,
  /// Accounts that never signed in, removed outright because the provider disabled or deleted
  /// them. Absent (0) from older servers.
  #[serde(default)]
  pub deleted: u32,
}

/// A deactivation the safety brake holds back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HeldDeactivationResponse {
  pub user_id: UserId,
  pub display_name: String,
  #[serde(rename = "username", default, skip_serializing_if = "Option::is_none")]
  pub maybe_username: Option<String>,
  pub cause: DeactivationCause,
  pub held_at: UnixMillis,
  /// Confirmed by an admin: it happens with the next run that can reach the provider.
  pub is_confirmed: bool,
}

/// `GET /api/v1/admin/oidc-sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminOidcSyncResponse {
  pub state: OidcSyncState,
  pub mode: OidcSyncMode,
  pub interval_secs: u64,
  /// The sync counts as overdue after this many intervals without a run.
  pub stale_after_intervals: u32,
  pub brake: OidcSyncBrakeSettings,
  /// When a run last finished without failing.
  #[serde(rename = "last_success_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_success_at: Option<UnixMillis>,
  #[serde(rename = "last_run", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_run: Option<OidcSyncRunResponse>,
  /// Overdue: no run has succeeded for the configured number of intervals.
  pub is_stale: bool,
  /// Held back by the safety brake, oldest first.
  pub held: Vec<HeldDeactivationResponse>,
  /// Names this set of held deactivations; a confirmation has to give it back.
  pub held_revision: i64,
  /// Deactivated accounts whose devices are still in the encryption groups: they wait for an
  /// admin's "Remove from all chats" (admin.md §3.2.1).
  pub pending_removals: u32,
  /// Deactivations committed whose sessions or announcements are still being finished.
  pub unfinished_deactivations: u32,
}

/// A change a dry run found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PlannedDeactivationResponse {
  pub user_id: UserId,
  pub display_name: String,
  #[serde(rename = "username", default, skip_serializing_if = "Option::is_none")]
  pub maybe_username: Option<String>,
  pub cause: DeactivationCause,
}

/// `POST /api/v1/admin/oidc-sync/preview`: what the sync would do now, nothing applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminOidcSyncPreviewResponse {
  /// Would be deactivated in a run now.
  pub would_deactivate: Vec<PlannedDeactivationResponse>,
  /// Would be held back by the safety brake.
  pub would_hold: Vec<PlannedDeactivationResponse>,
  pub would_reactivate: u32,
  pub would_change_roles: u32,
  /// Accounts a run now would make before their first sign-in (at most `max_created_per_run`).
  /// Absent (0) from older servers.
  #[serde(default)]
  pub would_create: u32,
  /// Active users of the provider, the base of the safety brake's share.
  pub active_users: u32,
  /// How many one run may deactivate with this many active users.
  pub allowed: u32,
}

/// `POST /api/v1/admin/oidc-sync/brake/confirm`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ConfirmOidcSyncBrakeRequest {
  /// The `held_revision` the admin looked at; a set that changed since is not confirmed.
  pub revision: i64,
}
