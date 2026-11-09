//! Leavers leave the encryption groups with nobody asked (docs/provisioning.md §3): an owner's or
//! admin's app signs the removal from all chats of people the identity provider deactivated, once
//! their grace has passed, as "Remove from all chats" by hand would (admin.md §3.2.1).
//!
//! Who counts as a leaver is the server's word. So this device never removes anybody on the signed
//! roster, honours only a signed policy that says `automatic`, and removes at most
//! `leavers.max_per_day` people on its own per 24 hours of its own clock: a hostile server that
//! reports members as leavers gets that many removed per admin device and day, all announced in
//! their channels, and gains no access by it. Nothing runs on a timer while nobody is listed: the
//! list is read once per gateway connection, on `leaver_update`, and when a listed leaver's grace
//! ends.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{
  ClaimLeaverRequest, ErrorCode, LeaverPolicy, LeaverRemovalMode, ListLeaversQuery, UserId,
};

use super::Client;
use crate::api::{ApiError, ServerApi};
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};
use crate::workspace::WorkspaceState;

/// This device's automatic removals of the last 24 hours, kept across restarts so that restarting
/// the app doesn't reset its limit.
const AUTOMATIC_REMOVALS_KEY: &str = "leaver_removals";

/// The window of `leavers.max_per_day`.
const DAY_MS: i64 = 86_400_000;

/// The longest a failed round waits for the next, doubling up to it.
const RETRY_MAX_MS: i64 = 15 * 60_000;
/// The first wait after a failed round.
const RETRY_FIRST_MS: i64 = 30_000;

/// What a round leaves for the next.
#[derive(Debug, Default)]
pub(crate) struct LeaverRun {
  /// The gateway connection the list was read for.
  maybe_read_connection: Option<u64>,
  /// The server said the list changed.
  is_dirty: bool,
  /// When, by this device's clock, the next listed leaver's grace ends or the limit frees a slot.
  maybe_due_at_ms: Option<i64>,
  retries: u32,
  retry_at_ms: i64,
}

/// When this device removed somebody on its own, by its own clock.
#[derive(Debug, Default, Serialize, Deserialize)]
struct AutomaticRemovals {
  at_ms: Vec<i64>,
}

/// What one round did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LeaverRound {
  /// Leavers whose removal this device signed.
  pub removed: usize,
  /// Leavers left for an admin because this device reached its daily limit.
  pub held_by_limit: usize,
}

/// What [`Client::leaver_round`] found besides the removals.
struct RoundOutcome {
  round: LeaverRound,
  maybe_due_in_ms: Option<i64>,
  maybe_failure: Option<CoreError>,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// One round, if one is due: after a new gateway connection, a `leaver_update`, or when a grace
  /// ends. Returns what it did; nothing while this user is no owner or admin, the signed policy
  /// says manual, or this device is offline.
  pub async fn run_leaver_removal(&mut self) -> Result<LeaverRound, CoreError> {
    let me = self.account.user_id;
    let workspace = self.workspace()?;
    if !workspace.is_admin(me) {
      return Ok(LeaverRound::default());
    }
    let policy = workspace.policy.leavers.clone();
    // An unknown mode from a newer app counts as manual: only `automatic` removes anybody.
    if policy.mode != LeaverRemovalMode::Automatic {
      return Ok(LeaverRound::default());
    }
    let Some(connection) = self.gateway_connections() else {
      return Ok(LeaverRound::default());
    };
    let now = self.now_ms();
    let run = &self.leavers;
    let is_new_connection = run.maybe_read_connection != Some(connection);
    let is_due = run.maybe_due_at_ms.is_some_and(|due| now >= due);
    if !is_new_connection && !run.is_dirty && !is_due {
      return Ok(LeaverRound::default());
    }
    if now < run.retry_at_ms {
      return Ok(LeaverRound::default());
    }
    let outcome = self.leaver_round(&workspace, &policy, now).await;
    match outcome {
      Ok(RoundOutcome {
        round,
        maybe_due_in_ms,
        maybe_failure: None,
      }) => {
        self.leavers.maybe_read_connection = Some(connection);
        self.leavers.is_dirty = false;
        self.leavers.retries = 0;
        self.leavers.retry_at_ms = 0;
        self.leavers.maybe_due_at_ms = maybe_due_in_ms.map(|due_in| now.saturating_add(due_in));
        Ok(round)
      },
      Ok(RoundOutcome {
        maybe_failure: Some(failure),
        ..
      })
      | Err(failure) => {
        self.back_off_leavers(now);
        Err(failure)
      },
    }
  }

  /// The server said the list of leavers changed: the next round reads it.
  pub fn note_leaver_update(&mut self) {
    self.leavers.is_dirty = true;
    self.leavers.retry_at_ms = 0;
  }

  /// Reads every page, removes who is due within the limit, and says when the next one is due.
  /// One leaver that fails doesn't stop the others; the first failure is returned with the rest.
  async fn leaver_round(
    &mut self,
    workspace: &WorkspaceState,
    policy: &LeaverPolicy,
    now: i64,
  ) -> Result<RoundOutcome, CoreError> {
    let me = self.account.user_id;
    let device = self.account.device_id;
    let grace_secs = u64::from(policy.grace_minutes()) * 60;
    let limit = usize::try_from(policy.max_per_day()).unwrap_or(usize::MAX);
    let mut removals = self.automatic_removals(now)?;
    let mut outcome = RoundOutcome {
      round: LeaverRound::default(),
      maybe_due_in_ms: None,
      maybe_failure: None,
    };
    let mut maybe_after = None;
    loop {
      let query = ListLeaversQuery {
        maybe_after,
        maybe_device_id: Some(device),
      };
      let page = self.api.admin_list_leavers(&query).await?;
      for leaver in &page.leavers {
        let user = leaver.user_id;
        // Roster members leave the roster through role evidence first, and an owner decides
        // (e2ee.md §3.1.3): the server's word never takes an owner or admin out.
        if user == me || leaver.is_claimed || is_on_roster(workspace, user) {
          continue;
        }
        if leaver.deactivated_for_secs < grace_secs {
          let due_in_ms =
            i64::try_from((grace_secs - leaver.deactivated_for_secs) * 1000).unwrap_or(i64::MAX);
          outcome.maybe_due_in_ms = Some(
            outcome
              .maybe_due_in_ms
              .map_or(due_in_ms, |due| due.min(due_in_ms)),
          );
          continue;
        }
        if removals.at_ms.len() >= limit {
          outcome.round.held_by_limit += 1;
          continue;
        }
        match self
          .api
          .admin_claim_leaver(user, &ClaimLeaverRequest { device_id: device })
          .await
        {
          Ok(()) => {},
          // Another device took them, or they are no leaver any more.
          Err(error) if is_taken(&error) => continue,
          Err(error) => {
            outcome.maybe_failure.get_or_insert(error.into());
            continue;
          },
        }
        // Counted before the removal, so that a crash in the middle can't loop past the limit.
        removals.at_ms.push(now);
        records::put_json(&self.store, Table::SyncState, AUTOMATIC_REMOVALS_KEY, &removals)?;
        match self.admin_remove_from_all_chats(user).await {
          Ok(()) => {
            tracing::info!(user_id = %user, "a leaver was removed from all chats");
            outcome.round.removed += 1;
          },
          Err(error) => {
            tracing::warn!(user_id = %user, %error, "removing a leaver from all chats failed");
            outcome.maybe_failure.get_or_insert(error);
          },
        }
      }
      match page.maybe_next {
        Some(next) => maybe_after = Some(next),
        None => break,
      }
    }
    if outcome.round.held_by_limit > 0 {
      tracing::warn!(
        held = outcome.round.held_by_limit,
        "leavers wait for an admin: this device reached its daily limit of automatic removals"
      );
      // A slot frees up when the oldest removal in the window is a day old.
      if let Some(oldest) = removals.at_ms.iter().min() {
        let frees_in = oldest.saturating_add(DAY_MS).saturating_sub(now).max(0);
        outcome.maybe_due_in_ms = Some(outcome.maybe_due_in_ms.map_or(frees_in, |due| due.min(frees_in)));
      }
    }
    Ok(outcome)
  }

  /// This device's automatic removals within the last day. A clock that jumped back keeps entries
  /// "in the future" until it catches up, which errs on removing fewer.
  fn automatic_removals(&self, now: i64) -> Result<AutomaticRemovals, CoreError> {
    let mut removals: AutomaticRemovals =
      records::get_json(&self.store, Table::SyncState, AUTOMATIC_REMOVALS_KEY)?.unwrap_or_default();
    removals.at_ms.retain(|at| now.saturating_sub(*at) < DAY_MS);
    Ok(removals)
  }

  fn back_off_leavers(&mut self, now: i64) {
    let shift = self.leavers.retries.min(10);
    let wait = RETRY_FIRST_MS.saturating_mul(1 << shift).min(RETRY_MAX_MS);
    self.leavers.retries = self.leavers.retries.saturating_add(1);
    self.leavers.retry_at_ms = now.saturating_add(wait);
  }
}

/// Whether `user` is on the verified roster, in any role.
fn is_on_roster(workspace: &WorkspaceState, user: UserId) -> bool {
  workspace
    .roster
    .members
    .iter()
    .any(|member| member.user_id == user)
}

/// A claim that lost to another device, a leaver who is none any more, or one only an owner may
/// remove (an admin's app leaves an admin account to an owner's).
fn is_taken(error: &ApiError) -> bool {
  matches!(error.error_code(), Some(ErrorCode::LeaverClaimed))
    || matches!(
      error,
      ApiError::Status {
        status: 403 | 404,
        ..
      }
    )
}
