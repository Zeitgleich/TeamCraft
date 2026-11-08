//! The identity provider sync in the admin console: how it stands, what it would do, and the
//! confirmation of what its safety brake holds back (ARCHITECTURE.md §5.1.1, admin.md §3.2.3).

use teamcraft_db::{BrakeConfirmation, HeldDeactivation, OidcSyncStatusRow, User};
use teamcraft_proto::{
  AdminOidcSyncPreviewResponse, AdminOidcSyncResponse, ConfirmOidcSyncBrakeRequest, DeactivationCause,
  HeldDeactivationResponse, OidcSyncBrakeSettings, OidcSyncError, OidcSyncMode, OidcSyncOutcome,
  OidcSyncRunResponse, OidcSyncState, Permission, PlannedDeactivationResponse, Target, UserId,
};
use time::{Duration, OffsetDateTime};
use tracing::warn;

use super::access::require;
use super::audit::{self, Target as AuditTarget};
use super::convert::unix_millis;
use super::oidc::{self, preview_plan};
use super::oidc_brake::PlannedDeactivation;
use crate::auth::AuthenticatedUser;
use crate::config::OidcSyncRunMode;
use crate::error::ApiError;
use crate::state::AppState;

/// The rank of an account as the console's rules see it: only owners manage admins and owners.
fn rank(role: Option<&str>) -> u8 {
  match role {
    Some("owner") => 3,
    Some("admin") => 2,
    _ => 1,
  }
}

pub async fn status(state: &AppState, caller: &AuthenticatedUser) -> Result<AdminOidcSyncResponse, ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  read_status(state).await
}

async fn read_status(state: &AppState) -> Result<AdminOidcSyncResponse, ApiError> {
  let settings = state.settings.get();
  let row = state.database.oidc_sync_status().await?;
  let held = state.database.list_held_deactivations().await?;
  let pending_removals = state.database.count_pending_chat_removals().await?;
  let unfinished_deactivations = state.database.count_deactivation_followups().await?;
  let sync = settings.maybe_oidc_sync.as_ref();
  let sync_state = match sync {
    Some(_) => OidcSyncState::Active,
    None if settings.is_oidc_sync_switched_off => OidcSyncState::SwitchedOff,
    None => OidcSyncState::NotConfigured,
  };
  let brake = sync.map(|settings| settings.brake).unwrap_or_default();
  let (interval_secs, stale_after_intervals) = sync.map_or((0, 0), |settings| {
    (settings.interval.as_secs(), settings.stale_after_intervals)
  });
  let allowed = Duration::seconds(i64::try_from(interval_secs).unwrap_or(i64::MAX))
    * i32::try_from(stale_after_intervals).unwrap_or(i32::MAX);
  let is_stale = sync.is_some()
    && match row.maybe_last_success_at {
      Some(at) => OffsetDateTime::now_utc() - at > allowed,
      // Never succeeded: late once it has failed for as many runs as it may miss.
      None => row.consecutive_failures >= stale_after_intervals.max(1),
    };
  Ok(AdminOidcSyncResponse {
    state: sync_state,
    mode: match sync.map(|settings| settings.mode) {
      Some(OidcSyncRunMode::DryRun) => OidcSyncMode::DryRun,
      _ => OidcSyncMode::Apply,
    },
    interval_secs,
    stale_after_intervals,
    brake: OidcSyncBrakeSettings {
      max_percent: brake.max_percent(),
      min_allowed: brake.min_allowed(),
    },
    maybe_last_success_at: row.maybe_last_success_at.map(unix_millis),
    maybe_last_run: last_run(&row),
    is_stale,
    held: held.iter().map(held_response).collect(),
    held_revision: row.held_revision,
    pending_removals,
    unfinished_deactivations,
  })
}

fn last_run(row: &OidcSyncStatusRow) -> Option<OidcSyncRunResponse> {
  Some(OidcSyncRunResponse {
    started_at: unix_millis(row.maybe_started_at?),
    finished_at: unix_millis(row.maybe_finished_at?),
    duration_ms: row.maybe_duration_ms.unwrap_or(0),
    outcome: row
      .maybe_outcome
      .clone()
      .map(OidcSyncOutcome::from)
      .unwrap_or(OidcSyncOutcome::Ok),
    maybe_error: row.maybe_error.clone().map(OidcSyncError::from),
    consecutive_failures: row.consecutive_failures,
    deactivated: row.deactivated,
    reactivated: row.reactivated,
    role_changes: row.role_changes,
    groups_changed: row.groups_changed,
    failed: row.failed,
  })
}

fn held_response(held: &HeldDeactivation) -> HeldDeactivationResponse {
  HeldDeactivationResponse {
    user_id: held.user,
    display_name: held.display_name.clone(),
    maybe_username: held.maybe_username.clone(),
    cause: DeactivationCause::from(held.cause.clone()),
    held_at: unix_millis(held.held_at),
    is_confirmed: held.is_confirmed,
  }
}

/// What the sync would do now; changes nothing.
pub async fn preview(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<AdminOidcSyncPreviewResponse, ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  let settings = state.settings.get();
  let Some(sync) = settings.maybe_oidc_sync.as_ref() else {
    return Err(ApiError::Unavailable("identity provider sync (oidc.sync)".into()));
  };
  let (decision, plan) = Box::pin(preview_plan(state)).await?;
  let users = named_users(state, decision.apply.iter().chain(&decision.hold)).await?;
  let response = |deactivation: &PlannedDeactivation| PlannedDeactivationResponse {
    user_id: deactivation.user,
    display_name: users
      .iter()
      .find(|user| user.id == deactivation.user)
      .map(|user| user.display_name.clone())
      .unwrap_or_default(),
    maybe_username: users
      .iter()
      .find(|user| user.id == deactivation.user)
      .and_then(|user| user.maybe_username.clone()),
    cause: deactivation.cause.clone(),
  };
  Ok(AdminOidcSyncPreviewResponse {
    would_deactivate: decision.apply.iter().map(response).collect(),
    would_hold: decision.hold.iter().map(response).collect(),
    would_reactivate: plan_count(plan.reactivation_count()),
    would_change_roles: plan_count(plan.role_change_count()),
    active_users: plan_count(plan.active_user_count()),
    allowed: plan_count(sync.brake.allowed(plan.active_user_count())),
  })
}

fn plan_count(count: usize) -> u32 {
  u32::try_from(count).unwrap_or(u32::MAX)
}

async fn named_users<'a>(
  state: &AppState,
  deactivations: impl Iterator<Item = &'a PlannedDeactivation>,
) -> Result<Vec<User>, ApiError> {
  let ids: Vec<UserId> = deactivations.map(|deactivation| deactivation.user).collect();
  Ok(state.database.get_users(&ids).await?)
}

/// Confirms what the safety brake holds back and applies it. The confirmation is stored first, so
/// a provider that is unreachable right now, or a deactivation that fails, doesn't need a second
/// one: the next run finishes it.
pub async fn confirm_brake(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: ConfirmOidcSyncBrakeRequest,
) -> Result<AdminOidcSyncResponse, ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  if state.settings.get().maybe_oidc_sync.is_none() {
    return Err(ApiError::Unavailable("identity provider sync (oidc.sync)".into()));
  }
  let held = state.database.list_held_deactivations().await?;
  let caller_rank = rank(Some(caller.role.as_str()));
  let may_confirm_all = held
    .iter()
    .all(|entry| caller_rank > rank(entry.maybe_oidc_role.as_deref()) || caller_rank == 3);
  if !may_confirm_all {
    return Err(ApiError::Forbidden(
      "an owner has to confirm: owners or admins are among those held back".into(),
    ));
  }
  let entry = audit::prepared(
    state,
    caller.user.id,
    "oidc_sync_brake_confirmed",
    &AuditTarget::new("user", caller.user.id),
  )?;
  match state
    .database
    .confirm_held_deactivations(caller.user.id, request.revision, &entry)
    .await?
  {
    BrakeConfirmation::StaleRevision => {
      return Err(ApiError::Conflict(
        "the list of held deactivations changed: look at it again".into(),
      ));
    },
    BrakeConfirmation::Confirmed(0) => {},
    BrakeConfirmation::Confirmed(_) => {
      if let Err(error) = oidc::sync_once(state).await {
        // The confirmation stands: the next run applies it.
        warn!(%error, "the sync after a confirmation failed");
      }
    },
  }
  read_status(state).await
}
