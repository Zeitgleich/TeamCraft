//! Deprovisioning through the identity provider (ARCHITECTURE.md §5.1.1, admin.md §3.2).
//!
//! Back-channel logout ends sessions the moment the provider ends them. The periodic sync catches
//! what logout can't: users disabled or deleted at the provider and role changes. The provider is
//! the authority; a user who loses every TeamCraft role is deactivated like a disabled one.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use secrecy::ExposeSecret;
use serde::Deserialize;
use serde_json::json;
use teamcraft_db::{
  DeletionGuard, GroupCreateOutcome, HeldDeactivation, NewHeldDeactivation, NewUserGroup, OidcIdentity,
  OidcSyncRun, OidcUserRow,
};
use teamcraft_proto::{
  DeactivationCause, DeactivationReason, OidcSyncError, OidcSyncOutcome, ProvisionStatus, ProvisionedUser,
  Role, UserGroupId, UserId, UserKind,
};
use time::OffsetDateTime;
use tracing::{debug, info, warn};

use super::admin::{
  AccountDeletion, AfterDeactivation, deactivate_user, delete_account, reactivate_and_announce,
};
use super::audit::{self, Target as AuditTarget};
use super::oidc_brake::{Decision, HeldEntry, PlannedDeactivation};
use super::user_groups;
use crate::auth::RoleMapping;
use crate::config::OidcSyncRunMode;
use crate::error::ApiError;
use crate::health::SecretReading;
use crate::state::{AppState, OidcSyncSettings};

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Where Keycloak's client secret rotation keeps when the current secret expires (Unix seconds).
const SECRET_EXPIRY_ATTRIBUTE: &str = "client.secret.expiration.time";

/// Why a deactivated user is deactivated, in words for the audit log.
const NOTE_REMOVED: &str = "removed from the identity provider";
const NOTE_DISABLED: &str = "disabled at the identity provider";
const NOTE_NO_ROLE: &str = "no TeamCraft role at the identity provider";

/// One run of the sync at a time (the scheduled ones and an admin's confirmation can meet), and how
/// many deactivations the brake holds back, for the health check that may not query the database.
#[derive(Debug, Default)]
pub struct OidcSyncRuntime {
  run: tokio::sync::Mutex<()>,
  held: AtomicU32,
  /// A run was asked for (an identity provider event, a first sign-in). One stored permit: any
  /// number of requests while one waits start a single run.
  wake: tokio::sync::Notify,
  /// When the server client's secret expires, as the last run read it (health check
  /// `secret_expiry`).
  secret_expiry: Mutex<SecretReading>,
  /// The subject the last run's role lookups for new accounts stopped at; the next run goes on
  /// after it (docs/provisioning.md §6). Lost on a restart, which only starts the round again.
  maybe_precreate_cursor: Mutex<Option<String>>,
}

/// Whether the sync makes the accounts of the provider's members before their first sign-in
/// (docs/provisioning.md §6), `[oidc.sync] precreate_accounts` and `max_created_per_run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountPrecreation {
  Off,
  /// One run looks up the roles of at most this many people without an account, and so makes at
  /// most this many accounts.
  UpTo(u32),
}

impl OidcSyncRuntime {
  /// Asks the sync loop for a run soon (docs/provisioning.md §4).
  pub fn request_run(&self) {
    self.wake.notify_one();
  }

  /// Resolves once a run was asked for.
  pub async fn requested(&self) {
    self.wake.notified().await;
  }

  pub fn held(&self) -> u32 {
    self.held.load(Ordering::Relaxed)
  }

  fn set_held(&self, count: u32) {
    self.held.store(count, Ordering::Relaxed);
  }

  pub fn secret_expiry(&self) -> SecretReading {
    *self.secret_expiry.lock().unwrap_or_else(PoisonError::into_inner)
  }

  fn set_secret_expiry(&self, reading: SecretReading) {
    *self.secret_expiry.lock().unwrap_or_else(PoisonError::into_inner) = reading;
  }

  fn precreate_cursor(&self) -> Option<String> {
    self
      .maybe_precreate_cursor
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
  }

  fn set_precreate_cursor(&self, maybe_cursor: Option<String>) {
    *self
      .maybe_precreate_cursor
      .lock()
      .unwrap_or_else(PoisonError::into_inner) = maybe_cursor;
  }
}

/// Ends the sessions a back-channel logout token names (the provider ended them).
///
/// A token with `sid` ends that provider session only, even when it also names the subject
/// (Back-Channel Logout 1.0 §2.4; Keycloak always sends both): signing out on one device must not
/// sign the user's other devices out. Only a token without `sid` ends all of the subject's sessions.
pub async fn backchannel_logout(state: &AppState, logout_token: &str) -> Result<(), ApiError> {
  let identity = state.validator.validate_logout_token(logout_token).await?;
  let now = i64::try_from(jsonwebtoken::get_current_timestamp()).unwrap_or(i64::MAX);
  let issuer = state.validator.issuer();
  let maybe_user = match &identity.maybe_subject {
    Some(subject) => state.database.find_user_by_oidc_subject(issuer, subject).await?,
    None => None,
  };

  let scope = match (&identity.maybe_session_id, &identity.maybe_subject) {
    (Some(session), maybe_subject) => {
      state
        .revocations
        .revoke_session(issuer, maybe_subject.as_deref(), session, now);
      state.hub.revoke_oidc_session(session, maybe_user);
      "session"
    },
    (None, Some(subject)) => {
      state.revocations.revoke_subject(issuer, subject, now);
      if let Some(user) = maybe_user {
        state.hub.revoke_user(user);
      }
      "subject"
    },
    // The validator refuses tokens that name neither.
    (None, None) => return Ok(()),
  };
  let target = maybe_user.map(|user| AuditTarget::new("user", user));
  audit::record(
    state,
    None,
    "oidc_backchannel_logout",
    target,
    &json!({"scope": scope}),
  )
  .await?;
  Ok(())
}

/// What one sync run changed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
  pub deactivated: u32,
  pub reactivated: u32,
  pub role_changes: u32,
  pub groups_created: u32,
  pub groups_changed: u32,
  pub groups_deleted: u32,
  /// Space and channel memberships the group mapping added (docs/provisioning.md §5).
  pub mapped_memberships: u32,
  /// Accounts made before their first sign-in (docs/provisioning.md §6).
  pub created: u32,
  /// Accounts that never signed in and that the provider disabled or deleted: removed outright
  /// instead of deactivated.
  pub deleted: u32,
  /// Deactivations the safety brake holds back after this run.
  pub held: u32,
  /// Changes that failed; the next run tries them again.
  pub failed: u32,
}

/// One planned change of a role.
struct RoleChange {
  user: UserId,
  maybe_from: Option<String>,
  role: Role,
}

/// An account to make before its first sign-in.
struct PlannedCreation {
  remote: RemoteUser,
  role: Role,
}

/// What the provider says should change, read in full before anything is applied.
pub(super) struct Plan {
  provider_session: ProviderSession,
  deactivations: Vec<PlannedDeactivation>,
  reactivations: Vec<UserId>,
  role_changes: Vec<RoleChange>,
  creations: Vec<PlannedCreation>,
  /// Where the next run's lookups for new accounts start.
  maybe_next_cursor: Option<String>,
  active_users: usize,
}

impl Plan {
  pub(super) fn creation_count(&self) -> usize {
    self.creations.len()
  }

  pub(super) fn reactivation_count(&self) -> usize {
    self.reactivations.len()
  }

  pub(super) fn role_change_count(&self) -> usize {
    self.role_changes.len()
  }

  pub(super) fn active_user_count(&self) -> usize {
    self.active_users
  }
}

/// Compares our users with the provider and applies the differences. Does nothing if the sync
/// is not configured. Fails without changing anything if the provider can't be read completely;
/// a change that fails doesn't stop the others and is tried again by the next run. In dry-run
/// mode, reports what it would do and changes nothing.
pub async fn sync_once(state: &AppState) -> Result<SyncReport, ApiError> {
  let Some(settings) = &state.settings.get().maybe_oidc_sync else {
    return Ok(SyncReport::default());
  };
  let _running = state.oidc_sync.run.lock().await;
  let started_at = OffsetDateTime::now_utc();
  let started = Instant::now();
  let result = Box::pin(run(state, settings)).await;
  record_run(state, started_at, started, settings, &result).await;
  result.map_err(|failure| failure.into_api_error())
}

/// Why a run failed before it applied anything.
#[derive(Debug)]
enum SyncFailure {
  Provider(ProviderError),
  ProviderListedNoUsers,
  Database(ApiError),
}

impl SyncFailure {
  fn code(&self) -> OidcSyncError {
    match self {
      Self::Provider(ProviderError::Unexpected(_)) => OidcSyncError::ProviderInvalidAnswer,
      Self::Provider(ProviderError::Request(error)) => match error.status() {
        Some(status) if status.as_u16() == 401 || status.as_u16() == 403 => OidcSyncError::ProviderRefused,
        Some(_) => OidcSyncError::ProviderInvalidAnswer,
        None if error.is_decode() => OidcSyncError::ProviderInvalidAnswer,
        None => OidcSyncError::ProviderUnreachable,
      },
      Self::ProviderListedNoUsers => OidcSyncError::ProviderListedNoUsers,
      Self::Database(_) => OidcSyncError::Database,
    }
  }

  fn into_api_error(self) -> ApiError {
    match self {
      Self::Provider(error) => unavailable(error),
      Self::ProviderListedNoUsers => {
        warn!("the identity provider listed no users although the instance has some: nothing changed");
        ApiError::Unavailable("identity provider".into())
      },
      Self::Database(error) => error,
    }
  }
}

impl From<ProviderError> for SyncFailure {
  fn from(error: ProviderError) -> Self {
    Self::Provider(error)
  }
}

impl From<teamcraft_db::DbError> for SyncFailure {
  fn from(error: teamcraft_db::DbError) -> Self {
    Self::Database(error.into())
  }
}

/// Reads everything the sync needs from the provider. Nothing is changed before all of it is
/// known, so a provider that fails halfway leaves the instance as it was.
async fn read_plan(state: &AppState, settings: &OidcSyncSettings) -> Result<Plan, SyncFailure> {
  let provider = Provider::new(settings).await?;
  let remote_users = provider.list_users().await?;
  let client_uuid = provider.client_uuid().await?;
  if let Some(reading) = provider.server_client_secret_expiry().await {
    state.oidc_sync.set_secret_expiry(reading);
  }
  let mapping = RoleMapping::new(settings.roles.clone());
  let remote: HashMap<&str, &RemoteUser> = remote_users.iter().map(|user| (user.id.as_str(), user)).collect();

  let users = state.database.list_oidc_users(&settings.issuer).await?;
  let active_users = users.iter().filter(|user| !user.is_deactivated).count();
  if remote_users.is_empty() && active_users > 0 {
    return Err(SyncFailure::ProviderListedNoUsers);
  }

  let plan_deactivation = |user: &OidcUserRow, cause, note| {
    (!user.is_deactivated).then_some(PlannedDeactivation {
      user: user.id,
      cause,
      note,
    })
  };
  let known: HashSet<String> = users.iter().map(|user| user.subject.clone()).collect();
  let (mut deactivations, mut reactivations, mut role_changes) = (Vec::new(), Vec::new(), Vec::new());
  for user in users {
    let Some(remote_user) = remote.get(user.subject.as_str()) else {
      deactivations.extend(plan_deactivation(&user, DeactivationCause::Removed, NOTE_REMOVED));
      continue;
    };
    if !remote_user.enabled {
      deactivations.extend(plan_deactivation(
        &user,
        DeactivationCause::Disabled,
        NOTE_DISABLED,
      ));
      continue;
    }
    let roles = provider.client_roles(&remote_user.id, &client_uuid).await?;
    match mapping.resolve(roles.iter().map(String::as_str)) {
      None => deactivations.extend(plan_deactivation(&user, DeactivationCause::NoRole, NOTE_NO_ROLE)),
      Some(role) => {
        let was_deactivated_by_sync =
          user.is_deactivated && user.maybe_deactivation_reason.as_deref() == Some("oidc");
        if was_deactivated_by_sync {
          reactivations.push(user.id);
        }
        if !user.is_deactivated && user.maybe_oidc_role.as_deref() != Some(role.as_str()) {
          role_changes.push(RoleChange {
            user: user.id,
            maybe_from: user.maybe_oidc_role.clone(),
            role,
          });
        }
      },
    }
  }
  let (creations, maybe_next_cursor) = match settings.precreation {
    AccountPrecreation::Off => (Vec::new(), None),
    AccountPrecreation::UpTo(budget) => {
      let released = state.database.released_subjects(&settings.issuer).await?;
      let candidates: Vec<&RemoteUser> = remote_users
        .iter()
        .filter(|user| user.enabled && !known.contains(&user.id) && !released.contains(&user.id))
        .collect();
      let cursor = state.oidc_sync.precreate_cursor();
      let (batch, maybe_next_cursor) = next_round(candidates, cursor.as_deref(), budget as usize);
      let mut creations = Vec::new();
      for remote_user in batch {
        let roles = provider.client_roles(&remote_user.id, &client_uuid).await?;
        // Guests are not made in advance: they come from outside and appear at their first
        // sign-in (docs/provisioning.md §6).
        match mapping.resolve(roles.iter().map(String::as_str)) {
          Some(role) if role != Role::Guest => creations.push(PlannedCreation {
            remote: remote_user.clone(),
            role,
          }),
          _ => {},
        }
      }
      (creations, maybe_next_cursor)
    },
  };
  Ok(Plan {
    provider_session: provider.into_session(),
    deactivations,
    reactivations,
    role_changes,
    creations,
    maybe_next_cursor,
    active_users,
  })
}

/// The next at most `budget` people of a round over `candidates` (in subject order), starting
/// after `maybe_cursor` and wrapping around, and where the following round starts: `None` once
/// one round covered everybody. So people the provider gives no TeamCraft role, looked up and
/// skipped every run, never keep the others waiting.
fn next_round<'a>(
  mut candidates: Vec<&'a RemoteUser>,
  maybe_cursor: Option<&str>,
  budget: usize,
) -> (Vec<&'a RemoteUser>, Option<String>) {
  candidates.sort_by(|a, b| a.id.cmp(&b.id));
  if candidates.len() <= budget {
    return (candidates, None);
  }
  let start = maybe_cursor.map_or(0, |cursor| {
    candidates
      .iter()
      .position(|user| user.id.as_str() > cursor)
      .unwrap_or(0)
  });
  let batch: Vec<&RemoteUser> = candidates
    .iter()
    .cycle()
    .skip(start)
    .take(budget)
    .copied()
    .collect();
  let maybe_next = batch.last().map(|user| user.id.clone());
  (batch, maybe_next)
}

fn decide(settings: &OidcSyncSettings, plan: &Plan, held: &[HeldDeactivation]) -> Decision {
  let entries: Vec<HeldEntry> = held
    .iter()
    .map(|entry| HeldEntry {
      user: entry.user,
      is_confirmed: entry.is_confirmed,
    })
    .collect();
  settings
    .brake
    .decide(plan.deactivations.clone(), &entries, plan.active_users)
}

async fn run(state: &AppState, settings: &OidcSyncSettings) -> Result<SyncReport, SyncFailure> {
  // Deactivations earlier work left unfinished come first: nothing here depends on the provider.
  if let Err(error) = Box::pin(super::admin::retry_deactivation_followups(state)).await {
    warn!(%error, "finishing earlier deactivations failed");
  }
  let plan = read_plan(state, settings).await?;
  let held = state.database.list_held_deactivations().await?;
  let decision = decide(settings, &plan, &held);

  if settings.mode == OidcSyncRunMode::DryRun {
    info!(
      would_deactivate = decision.apply.len(),
      would_hold = decision.hold.len(),
      would_reactivate = plan.reactivations.len(),
      would_change_roles = plan.role_changes.len(),
      would_create = plan.creations.len(),
      "oidc sync dry run"
    );
    return Ok(SyncReport {
      deactivated: count(decision.apply.len()),
      reactivated: count(plan.reactivations.len()),
      role_changes: count(plan.role_changes.len()),
      created: count(plan.creations.len()),
      held: count(decision.hold.len()),
      ..SyncReport::default()
    });
  }

  Box::pin(apply(state, settings, plan, decision, &held)).await
}

/// Applies a run: the brake's verdict is stored first, so that a crash cannot forget it. One change
/// that fails doesn't stop the others.
async fn apply(
  state: &AppState,
  settings: &OidcSyncSettings,
  plan: Plan,
  decision: Decision,
  previously_held: &[HeldDeactivation],
) -> Result<SyncReport, SyncFailure> {
  // A confirmed deactivation stays on the list until it has happened: if it fails, the next run
  // still has the confirmation.
  let confirmed: HashSet<UserId> = previously_held
    .iter()
    .filter(|entry| entry.is_confirmed)
    .map(|entry| entry.user)
    .collect();
  let retained = |deactivation: &&PlannedDeactivation| confirmed.contains(&deactivation.user);
  let held = store_held(
    state,
    decision.hold.iter().chain(decision.apply.iter().filter(retained)),
  )
  .await?;

  let mut report = SyncReport {
    held: count(decision.hold.len()),
    ..SyncReport::default()
  };
  if decision.is_tripped {
    warn!(
      held,
      active = plan.active_users,
      "the identity provider sync holds deactivations back (safety brake): admins have to look at them"
    );
    // Best effort in the log: the held rows are what counts.
    if let Err(error) = audit::record(
      state,
      None,
      "oidc_sync_brake_engaged",
      None,
      &json!({"held": held, "active_users": plan.active_users, "was_held": previously_held.len()}),
    )
    .await
    {
      warn!(%error, "recording that the safety brake engaged failed");
    }
  }
  let mut still_confirmed: Vec<&PlannedDeactivation> = Vec::new();
  for deactivation in &decision.apply {
    match Box::pin(deprovision(state, deactivation)).await {
      Ok(Deprovisioned::Deactivated) => report.deactivated += 1,
      Ok(Deprovisioned::Deleted) => report.deleted += 1,
      Ok(Deprovisioned::Nothing) => {},
      Err(error) => {
        warn!(user_id = %deactivation.user, %error, "deactivating a user failed");
        report.failed += 1;
        if confirmed.contains(&deactivation.user) {
          still_confirmed.push(deactivation);
        }
      },
    }
  }
  store_held(state, decision.hold.iter().chain(still_confirmed)).await?;
  if decision.is_tripped {
    // The provider's answer as a whole looks wrong: roles and groups wait for a better one.
    return Ok(report);
  }

  for user in &plan.reactivations {
    match reactivate_from_sync(state, *user).await {
      Ok(true) => report.reactivated += 1,
      Ok(false) => {},
      Err(error) => {
        warn!(user_id = %user, %error, "reactivating a user failed");
        report.failed += 1;
      },
    }
  }
  for change in &plan.role_changes {
    match apply_role_change(state, change).await {
      Ok(()) => report.role_changes += 1,
      Err(error) => {
        warn!(user_id = %change.user, %error, "changing a user's role failed");
        report.failed += 1;
      },
    }
  }
  // Before the group mapping, so that new accounts land in their spaces in the same run.
  let mut has_failed_creation = false;
  for creation in &plan.creations {
    match create_account(state, settings, creation).await {
      Ok(true) => report.created += 1,
      Ok(false) => {},
      Err(error) => {
        warn!(%error, "making an account before its first sign-in failed");
        report.failed += 1;
        has_failed_creation = true;
      },
    }
  }
  // After a failure the next run looks at the same people again instead of going on.
  if !has_failed_creation {
    state
      .oidc_sync
      .set_precreate_cursor(plan.maybe_next_cursor.clone());
  }
  if settings.sync_groups {
    let provider = Provider::resume(settings, &plan.provider_session);
    if let Err(error) = sync_groups(state, settings, &provider, &mut report).await {
      warn!(%error, "syncing the groups failed");
      report.failed += 1;
    }
  }
  if settings.mode == OidcSyncRunMode::Apply {
    let provider = Provider::resume(settings, &plan.provider_session);
    if let Err(error) = apply_group_mappings(state, settings, &provider, &mut report).await {
      warn!(%error, "applying the group mapping failed");
      report.failed += 1;
    }
  }
  if report != SyncReport::default() {
    info!(?report, "oidc sync finished");
  }
  Ok(report)
}

/// Stores who is held back from now on and tells the health check. Returns how many.
async fn store_held(
  state: &AppState,
  held: impl Iterator<Item = &PlannedDeactivation>,
) -> Result<u32, SyncFailure> {
  let entries: Vec<NewHeldDeactivation<'_>> = held
    .map(|entry| NewHeldDeactivation {
      user: entry.user,
      cause: entry.cause.as_str(),
    })
    .collect();
  let held = state.database.set_held_deactivations(&entries).await?;
  state.oidc_sync.set_held(held);
  Ok(held)
}

/// What [`deprovision`] did.
enum Deprovisioned {
  Deactivated,
  Deleted,
  /// Deactivated before: nothing to do.
  Nothing,
}

/// Takes away the account of somebody the provider disabled, deleted or stripped of their roles.
/// An account that never signed in (docs/provisioning.md §6) is deleted outright: it has no device
/// in any group, so there is nothing to suspend or rotate. Every other account is suspended like by
/// an admin: the provider may enable the user again, and the sync then reactivates them with
/// everything intact; the leaver round (§3) covers people who left.
async fn deprovision(
  state: &AppState,
  deactivation: &PlannedDeactivation,
) -> Result<Deprovisioned, ApiError> {
  let deleted = Box::pin(delete_account(
    state,
    None,
    deactivation.user,
    DeletionGuard::NeverSignedIn,
    &json!({"source": "oidc_sync", "reason": "never_signed_in", "cause": deactivation.cause.as_str()}),
  ))
  .await?;
  match deleted {
    AccountDeletion::Deleted | AccountDeletion::WasAlreadyDeleted => return Ok(Deprovisioned::Deleted),
    AccountDeletion::SignedIn => {},
  }
  let is_deactivated = Box::pin(deactivate_user(
    state,
    None,
    deactivation.user,
    DeactivationReason::Oidc,
    deactivation.note,
    AfterDeactivation::KeepChats,
  ))
  .await?;
  Ok(if is_deactivated {
    Deprovisioned::Deactivated
  } else {
    Deprovisioned::Nothing
  })
}

/// Makes one account before its first sign-in; `false` if it was there already (a sign-in at the
/// same moment).
async fn create_account(
  state: &AppState,
  settings: &OidcSyncSettings,
  creation: &PlannedCreation,
) -> Result<bool, ApiError> {
  let (user, is_created) = provision_account(state, settings, &creation.remote, &creation.role).await?;
  if is_created {
    audit::record(
      state,
      None,
      "user_provisioned",
      Some(AuditTarget::new("user", user)),
      &json!({"source": "oidc_sync", "role": creation.role.as_str()}),
    )
    .await?;
  }
  Ok(is_created)
}

/// Makes the account of `remote` before its first sign-in, marked as awaiting it, or finds the one
/// there. Returns it and whether it was made now.
async fn provision_account(
  state: &AppState,
  settings: &OidcSyncSettings,
  remote: &RemoteUser,
  role: &Role,
) -> Result<(UserId, bool), ApiError> {
  let kind = if *role == Role::Guest {
    UserKind::Guest
  } else {
    UserKind::Member
  };
  let display_name = remote.display_name();
  let account = state
    .database
    .provision_oidc_user(
      &OidcIdentity {
        issuer: &settings.issuer,
        subject: &remote.id,
        maybe_username: remote.maybe_username.as_deref(),
        maybe_email: remote.maybe_email.as_deref(),
        display_name: &display_name,
        kind,
        maybe_oidc_role: Some(role.as_str()),
        maybe_organization: None,
        should_sync_organization: false,
      },
      state.next_id()?,
    )
    .await?;
  if account.is_created {
    info!(user_id = %account.user, "provisioned an account from the identity provider");
  }
  Ok((account.user, account.is_created))
}

async fn reactivate_from_sync(state: &AppState, user: UserId) -> Result<bool, ApiError> {
  if !reactivate_and_announce(state, user).await? {
    return Ok(false);
  }
  audit::record(
    state,
    None,
    "user_reactivated",
    Some(AuditTarget::new("user", user)),
    &json!({"reason": "oidc", "source": "oidc_sync"}),
  )
  .await?;
  Ok(true)
}

async fn apply_role_change(state: &AppState, change: &RoleChange) -> Result<(), ApiError> {
  let kind = if change.role == Role::Guest {
    UserKind::Guest
  } else {
    UserKind::Member
  };
  state
    .database
    .set_user_oidc_role(change.user, change.role.as_str(), &kind)
    .await?;
  audit::record(
    state,
    None,
    "oidc_role_changed",
    Some(AuditTarget::new("user", change.user)),
    &json!({"from": change.maybe_from, "to": change.role.as_str()}),
  )
  .await?;
  Ok(())
}

fn count(items: usize) -> u32 {
  u32::try_from(items).unwrap_or(u32::MAX)
}

/// Stores how the run went for the admin console. Never fails the run: it is only a report.
async fn record_run(
  state: &AppState,
  started_at: OffsetDateTime,
  started: Instant,
  settings: &OidcSyncSettings,
  result: &Result<SyncReport, SyncFailure>,
) {
  let (outcome, maybe_error, report) = match result {
    Ok(report) if settings.mode == OidcSyncRunMode::DryRun => (OidcSyncOutcome::DryRun, None, report.clone()),
    Ok(report) if report.failed > 0 => (OidcSyncOutcome::Partial, None, report.clone()),
    Ok(report) if report.held > 0 => (OidcSyncOutcome::Held, None, report.clone()),
    Ok(report) => (OidcSyncOutcome::Ok, None, report.clone()),
    Err(failure) => (
      OidcSyncOutcome::Failed,
      Some(failure.code()),
      SyncReport::default(),
    ),
  };
  let run = OidcSyncRun {
    started_at,
    finished_at: OffsetDateTime::now_utc(),
    duration_ms: i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX),
    outcome: outcome.as_str(),
    maybe_error: maybe_error.as_ref().map(OidcSyncError::as_str),
    is_success: result.is_ok() && report.failed == 0,
    deactivated: report.deactivated,
    reactivated: report.reactivated,
    role_changes: report.role_changes,
    groups_changed: report.groups_created + report.groups_changed + report.groups_deleted,
    failed: report.failed,
    created: report.created,
    deleted: report.deleted,
  };
  if let Err(error) = state.database.record_oidc_sync_run(&run).await {
    warn!(%error, "storing how the oidc sync went failed");
  }
}

/// What the sync would do right now, nothing applied (the console's preview, and to check the
/// configuration before relying on it).
pub(super) async fn preview_plan(state: &AppState) -> Result<(Decision, Plan), ApiError> {
  let Some(settings) = &state.settings.get().maybe_oidc_sync else {
    return Err(ApiError::Unavailable("identity provider sync (oidc.sync)".into()));
  };
  let _running = state.oidc_sync.run.lock().await;
  let plan = Box::pin(read_plan(state, settings))
    .await
    .map_err(SyncFailure::into_api_error)?;
  let held = state.database.list_held_deactivations().await?;
  let decision = decide(settings, &plan, &held);
  Ok((decision, plan))
}

/// Reads what the brake holds after a start, for the health check.
pub async fn load_brake_state(state: &AppState) {
  match state.database.list_held_deactivations().await {
    Ok(held) => state.oidc_sync.set_held(count(held.len())),
    Err(error) => warn!(%error, "reading what the identity provider sync holds back failed"),
  }
}

/// Creates the accounts of `usernames` from the provider before their first sign-in (admin.md §3.8),
/// so an import can make them members. A sign-in later finds the account by issuer and subject and
/// refreshes it from the token. Fails when the sync isn't configured or the provider can't be read.
pub async fn provision_users(
  state: &AppState,
  usernames: &[String],
) -> Result<Vec<ProvisionedUser>, ApiError> {
  let Some(settings) = &state.settings.get().maybe_oidc_sync else {
    return Err(ApiError::Unavailable("identity provider sync (oidc.sync)".into()));
  };
  let provider = Provider::new(settings).await.map_err(unavailable)?;
  let client_uuid = provider.client_uuid().await.map_err(unavailable)?;
  let mapping = RoleMapping::new(settings.roles.clone());
  let mut provisioned = Vec::with_capacity(usernames.len());
  for username in usernames {
    let maybe_remote = provider.find_user(username).await.map_err(unavailable)?;
    let entry = match maybe_remote {
      Some(remote) if remote.enabled => {
        let roles = provider
          .client_roles(&remote.id, &client_uuid)
          .await
          .map_err(unavailable)?;
        match mapping.resolve(roles.iter().map(String::as_str)) {
          Some(role) => provision_one(state, settings, &remote, role).await?,
          None => (None, ProvisionStatus::NotFound),
        }
      },
      _ => (None, ProvisionStatus::NotFound),
    };
    provisioned.push(ProvisionedUser {
      username: username.clone(),
      maybe_user_id: entry.0,
      status: entry.1,
    });
  }
  Ok(provisioned)
}

async fn provision_one(
  state: &AppState,
  settings: &OidcSyncSettings,
  remote: &RemoteUser,
  role: teamcraft_proto::Role,
) -> Result<(Option<UserId>, ProvisionStatus), ApiError> {
  if let Some(existing) = state
    .database
    .find_user_by_oidc_subject(&settings.issuer, &remote.id)
    .await?
  {
    let is_available = state
      .database
      .get_users(&[existing])
      .await?
      .iter()
      .any(|user| !user.is_deactivated);
    let status = if is_available {
      ProvisionStatus::Existing
    } else {
      ProvisionStatus::Unavailable
    };
    return Ok((Some(existing), status));
  }
  let (user, is_created) = provision_account(state, settings, remote, &role).await?;
  let status = if is_created {
    ProvisionStatus::Created
  } else {
    ProvisionStatus::Existing
  };
  Ok((Some(user), status))
}

/// Adds the provider's members of each mapped group to the rule's spaces and public channels, once
/// per account and revision of the rule (docs/provisioning.md §5). Additions only. A group the
/// provider doesn't have (renamed, deleted) is skipped with a warning; the others go on.
async fn apply_group_mappings(
  state: &AppState,
  settings: &OidcSyncSettings,
  provider: &Provider<'_>,
  report: &mut SyncReport,
) -> Result<(), ApiError> {
  for mapping in state.database.list_group_mappings().await? {
    let Some(group) = provider
      .group_by_path(&mapping.group_path)
      .await
      .map_err(unavailable)?
    else {
      warn!(group_path = %mapping.group_path, "a mapped group is not at the identity provider; skipped");
      continue;
    };
    let subjects = provider.group_member_ids(&group.id).await.map_err(unavailable)?;
    let members = state
      .database
      .find_users_by_subjects(&settings.issuer, &subjects)
      .await?;
    for member in members {
      let applied = state.database.apply_group_mapping(&mapping, member).await?;
      if applied.is_empty() {
        continue;
      }
      report.mapped_memberships += u32::try_from(
        applied
          .iter()
          .map(|invite| 1 + invite.joined_channels.len())
          .sum::<usize>(),
      )
      .unwrap_or(u32::MAX);
      super::audit::record(
        state,
        None,
        "group_mapping_applied",
        Some(AuditTarget::new("user", member)),
        &json!({
          "source": "oidc_sync",
          "group_path": mapping.group_path,
          "spaces": applied.iter().map(|invite| invite.space_id.to_string()).collect::<Vec<_>>(),
          "channels": applied.iter().flat_map(|invite| invite.joined_channels.iter().map(ToString::to_string)).collect::<Vec<_>>(),
        }),
      )
      .await?;
      if let Some(user) = state.database.get_user(member).await? {
        super::accounts::announce_applied(state, &user, applied).await?;
      }
    }
  }
  Ok(())
}

/// Mirrors the provider's top-level groups as user groups. The provider is the source of truth for
/// their name and members; a group that vanished there is deleted here.
async fn sync_groups(
  state: &AppState,
  settings: &OidcSyncSettings,
  provider: &Provider<'_>,
  report: &mut SyncReport,
) -> Result<(), ApiError> {
  let remote_groups = provider.list_groups().await.map_err(unavailable)?;
  let mut seen = HashSet::new();
  for remote in &remote_groups {
    seen.insert(remote.id.clone());
    let subjects = provider.group_member_ids(&remote.id).await.map_err(unavailable)?;
    let members = state
      .database
      .find_users_by_subjects(&settings.issuer, &subjects)
      .await?;

    let (group_id, is_created, is_renamed) =
      match state.database.find_user_group_by_external_id(&remote.id).await? {
        Some(group) => {
          let is_renamed = group.name != remote.name;
          if is_renamed {
            state
              .database
              .update_user_group(group.id, Some(&remote.name), None, false)
              .await?;
          }
          (group.id, false, is_renamed)
        },
        None => {
          let Some(handle) = user_groups::handle_from_name(&remote.name) else {
            warn!(group = %remote.id, "the group name cannot become a handle; skipped");
            continue;
          };
          let id: UserGroupId = state.next_id()?;
          let outcome = state
            .database
            .create_user_group(&NewUserGroup {
              id,
              handle: &handle,
              name: &remote.name,
              maybe_description: None,
              maybe_external_id: Some(&remote.id),
              maybe_created_by: None,
            })
            .await?;
          if outcome == GroupCreateOutcome::HandleTaken {
            warn!(%handle, "the handle of a provider group is taken; skipped");
            continue;
          }
          report.groups_created += 1;
          (id, true, false)
        },
      };
    let has_new_members = state.database.set_user_group_members(group_id, &members).await?;
    if !is_created && (is_renamed || has_new_members) {
      report.groups_changed += 1;
    }
    if is_created || is_renamed || has_new_members {
      user_groups::announce(state, group_id);
    }
  }
  for (id, external_id) in state.database.list_synced_group_external_ids().await? {
    if !seen.contains(&external_id) {
      state.database.delete_user_group(id).await?;
      user_groups::announce(state, id);
      report.groups_deleted += 1;
    }
  }
  Ok(())
}

fn unavailable(error: ProviderError) -> ApiError {
  warn!(%error, "identity provider request failed");
  ApiError::Unavailable("identity provider".into())
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProviderError {
  #[error("request failed: {0}")]
  Request(#[from] reqwest::Error),
  #[error("unexpected answer: {0}")]
  Unexpected(String),
}

#[derive(Clone, Deserialize)]
struct RemoteUser {
  id: String,
  #[serde(default)]
  enabled: bool,
  #[serde(rename = "username", default)]
  maybe_username: Option<String>,
  #[serde(rename = "email", default)]
  maybe_email: Option<String>,
  #[serde(rename = "firstName", default)]
  maybe_first_name: Option<String>,
  #[serde(rename = "lastName", default)]
  maybe_last_name: Option<String>,
}

impl RemoteUser {
  /// The name a token's `name` claim would carry (Keycloak: first and last name), else the user
  /// name, else the subject, as the token validator does.
  fn display_name(&self) -> String {
    let full = [self.maybe_first_name.as_deref(), self.maybe_last_name.as_deref()]
      .into_iter()
      .flatten()
      .map(str::trim)
      .filter(|part| !part.is_empty())
      .collect::<Vec<_>>()
      .join(" ");
    if !full.is_empty() {
      return full;
    }
    self.maybe_username.clone().unwrap_or_else(|| self.id.clone())
  }
}

#[derive(Deserialize)]
struct RemoteGroup {
  id: String,
  name: String,
}

#[derive(Deserialize)]
struct RemoteClient {
  id: String,
}

/// The attributes of a client, where Keycloak keeps the secret's expiry.
#[derive(Deserialize)]
struct RemoteClientAttributes {
  #[serde(default)]
  attributes: HashMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct RemoteRole {
  name: String,
}

#[derive(Deserialize)]
struct TokenAnswer {
  access_token: String,
}

/// A logged-in view of the provider's admin API.
struct Provider<'a> {
  http: reqwest::Client,
  settings: &'a OidcSyncSettings,
  access_token: String,
}

/// The parts of a [`Provider`] that outlive its borrow of the settings. The access token stays in
/// here; it is never logged or stored.
struct ProviderSession {
  http: reqwest::Client,
  access_token: String,
}

impl<'a> Provider<'a> {
  async fn new(settings: &'a OidcSyncSettings) -> Result<Self, ProviderError> {
    let http = reqwest::Client::builder().timeout(HTTP_TIMEOUT).build()?;
    let answer: TokenAnswer = http
      .post(&settings.token_url)
      .form(&[
        ("grant_type", "client_credentials"),
        ("client_id", settings.server_client.as_str()),
        ("client_secret", settings.client_secret.expose_secret()),
      ])
      .send()
      .await?
      .error_for_status()?
      .json()
      .await?;
    Ok(Self {
      http,
      settings,
      access_token: answer.access_token,
    })
  }

  fn into_session(self) -> ProviderSession {
    ProviderSession {
      http: self.http,
      access_token: self.access_token,
    }
  }

  fn resume(settings: &'a OidcSyncSettings, session: &ProviderSession) -> Self {
    Self {
      http: session.http.clone(),
      settings,
      access_token: session.access_token.clone(),
    }
  }

  async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, ProviderError> {
    let url = format!("{}{path}", self.settings.admin_url.trim_end_matches('/'));
    debug!(%url, "asking the identity provider");
    Ok(
      self
        .http
        .get(url)
        .bearer_auth(&self.access_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?,
    )
  }

  /// All users, page by page.
  async fn list_users(&self) -> Result<Vec<RemoteUser>, ProviderError> {
    let page_size = self.settings.page_size.max(1) as usize;
    let mut users = Vec::new();
    loop {
      let page: Vec<RemoteUser> = self
        .get(&format!(
          "/users?first={}&max={page_size}&briefRepresentation=true",
          users.len()
        ))
        .await?;
      let count = page.len();
      users.extend(page);
      if count < page_size {
        return Ok(users);
      }
    }
  }

  /// The top-level groups, page by page.
  async fn list_groups(&self) -> Result<Vec<RemoteGroup>, ProviderError> {
    let page_size = self.settings.page_size.max(1) as usize;
    let mut groups = Vec::new();
    loop {
      let page: Vec<RemoteGroup> = self
        .get(&format!(
          "/groups?first={}&max={page_size}&briefRepresentation=true",
          groups.len()
        ))
        .await?;
      let count = page.len();
      groups.extend(page);
      if count < page_size {
        return Ok(groups);
      }
    }
  }

  /// The group at `path` (e.g. `/engineering`), or `None` if the provider has none there.
  async fn group_by_path(&self, path: &str) -> Result<Option<RemoteGroup>, ProviderError> {
    let encoded: Vec<String> = path
      .trim_matches('/')
      .split('/')
      .map(|segment| url::form_urlencoded::byte_serialize(segment.as_bytes()).collect())
      .collect();
    match self.get(&format!("/group-by-path/{}", encoded.join("/"))).await {
      Ok(group) => Ok(Some(group)),
      Err(ProviderError::Request(error)) if error.status() == Some(reqwest::StatusCode::NOT_FOUND) => {
        Ok(None)
      },
      Err(error) => Err(error),
    }
  }

  /// The subjects of a group's direct members, page by page.
  async fn group_member_ids(&self, group: &str) -> Result<Vec<String>, ProviderError> {
    let page_size = self.settings.page_size.max(1) as usize;
    let mut ids = Vec::new();
    loop {
      let page: Vec<RemoteUser> = self
        .get(&format!(
          "/groups/{group}/members?first={}&max={page_size}&briefRepresentation=true",
          ids.len()
        ))
        .await?;
      let count = page.len();
      ids.extend(page.into_iter().map(|user| user.id));
      if count < page_size {
        return Ok(ids);
      }
    }
  }

  /// The user with exactly this user name.
  async fn find_user(&self, username: &str) -> Result<Option<RemoteUser>, ProviderError> {
    let encoded: String = url::form_urlencoded::byte_serialize(username.as_bytes()).collect();
    let users: Vec<RemoteUser> = self.get(&format!("/users?username={encoded}&exact=true")).await?;
    Ok(users.into_iter().find(|user| {
      user
        .maybe_username
        .as_deref()
        .is_some_and(|name| name.eq_ignore_ascii_case(username))
    }))
  }

  async fn client_uuid(&self) -> Result<String, ProviderError> {
    let clients: Vec<RemoteClient> = self
      .get(&format!("/clients?clientId={}", self.settings.client_id))
      .await?;
    clients
      .into_iter()
      .next()
      .map(|client| client.id)
      .ok_or_else(|| ProviderError::Unexpected(format!("client {} not found", self.settings.client_id)))
  }

  /// When the secret of the server's own client expires, if the provider keeps that (Keycloak's
  /// client secret rotation). `None` when it can't be read: the last reading stays, and the sync
  /// goes on either way.
  async fn server_client_secret_expiry(&self) -> Option<SecretReading> {
    let encoded: String =
      url::form_urlencoded::byte_serialize(self.settings.server_client.as_bytes()).collect();
    match self
      .get::<Vec<RemoteClientAttributes>>(&format!("/clients?clientId={encoded}"))
      .await
    {
      Ok(clients) => Some(clients.first().map_or(SecretReading::Unknown, |client| {
        secret_expiry_of(&client.attributes)
      })),
      Err(error) => {
        debug!(%error, "the server client's secret expiry could not be read");
        None
      },
    }
  }

  /// Client roles of the user including those inherited from groups.
  async fn client_roles(&self, user: &str, client_uuid: &str) -> Result<Vec<String>, ProviderError> {
    let roles: Vec<RemoteRole> = self
      .get(&format!(
        "/users/{user}/role-mappings/clients/{client_uuid}/composite"
      ))
      .await?;
    Ok(roles.into_iter().map(|role| role.name).collect())
  }
}

/// Derives the admin API URL of a Keycloak realm from its issuer (`<base>/realms/<realm>`).
/// The secret's expiry from a client's attributes: Unix seconds, as text or a number; absent or 0
/// means it doesn't expire.
fn secret_expiry_of(attributes: &HashMap<String, serde_json::Value>) -> SecretReading {
  let maybe_seconds = match attributes.get(SECRET_EXPIRY_ATTRIBUTE) {
    None => None,
    Some(serde_json::Value::String(text)) => text.trim().parse::<i64>().ok(),
    Some(value) => value.as_i64(),
  };
  match maybe_seconds {
    Some(seconds) if seconds > 0 => SecretReading::ExpiresAt(seconds),
    _ => SecretReading::NoExpiry,
  }
}

pub fn admin_url_from_issuer(issuer: &str) -> Option<String> {
  let (base, realm) = issuer.trim_end_matches('/').rsplit_once("/realms/")?;
  (!realm.is_empty() && !realm.contains('/')).then(|| format!("{base}/admin/realms/{realm}"))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_admin_url_is_derived_from_a_keycloak_issuer() {
    assert_eq!(
      admin_url_from_issuer("https://auth.example.com/realms/company").as_deref(),
      Some("https://auth.example.com/admin/realms/company")
    );
    assert_eq!(
      admin_url_from_issuer("http://127.0.0.1:8080/auth/realms/dev/").as_deref(),
      Some("http://127.0.0.1:8080/auth/admin/realms/dev")
    );
    assert_eq!(admin_url_from_issuer("https://accounts.example.com"), None);
    assert_eq!(admin_url_from_issuer("https://auth.example.com/realms/a/b"), None);
  }

  mod next_round {
    use super::*;

    #[test]
    fn everybody_fits_in_one_round() {
      let users = remote_users(&["c", "a", "b"]);
      let (batch, maybe_next) = next_round(users.iter().collect(), Some("b"), 3);
      assert_eq!(ids(&batch), ["a", "b", "c"]);
      assert_eq!(maybe_next, None);
    }

    #[test]
    fn a_round_goes_on_after_the_cursor_and_wraps_around() {
      let users = remote_users(&["a", "b", "c", "d", "e"]);
      let (batch, maybe_next) = next_round(users.iter().collect(), None, 2);
      assert_eq!(ids(&batch), ["a", "b"]);
      let (batch, maybe_next) = next_round(users.iter().collect(), maybe_next.as_deref(), 2);
      assert_eq!(ids(&batch), ["c", "d"]);
      let (batch, maybe_next) = next_round(users.iter().collect(), maybe_next.as_deref(), 2);
      assert_eq!(ids(&batch), ["e", "a"]);
      assert_eq!(maybe_next.as_deref(), Some("a"));
    }

    #[test]
    fn a_cursor_past_everybody_starts_again() {
      let users = remote_users(&["a", "b", "c"]);
      let (batch, _) = next_round(users.iter().collect(), Some("z"), 2);
      assert_eq!(ids(&batch), ["a", "b"]);
    }

    fn remote_users(ids: &[&str]) -> Vec<RemoteUser> {
      ids
        .iter()
        .map(|id| serde_json::from_value(json!({"id": id, "enabled": true})).unwrap())
        .collect()
    }

    fn ids<'a>(users: &[&'a RemoteUser]) -> Vec<&'a str> {
      users.iter().map(|user| user.id.as_str()).collect()
    }
  }

  #[test]
  fn the_client_secret_expiry_is_read_from_keycloaks_attribute() {
    let attributes = |value: serde_json::Value| HashMap::from([(SECRET_EXPIRY_ATTRIBUTE.to_owned(), value)]);
    assert_eq!(
      secret_expiry_of(&attributes(json!("1800000000"))),
      SecretReading::ExpiresAt(1_800_000_000)
    );
    assert_eq!(
      secret_expiry_of(&attributes(json!(1_800_000_000))),
      SecretReading::ExpiresAt(1_800_000_000)
    );
    assert_eq!(secret_expiry_of(&attributes(json!("0"))), SecretReading::NoExpiry);
    assert_eq!(secret_expiry_of(&HashMap::new()), SecretReading::NoExpiry);
  }
}
