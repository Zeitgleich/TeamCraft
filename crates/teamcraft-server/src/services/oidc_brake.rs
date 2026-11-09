//! The safety brake of the identity provider sync (ARCHITECTURE.md §5.1.1, admin.md §3.2.3): a
//! misconfigured provider must not lock a company out. Pure decisions, so that every case is tested
//! without a provider.

use std::collections::HashSet;

use teamcraft_proto::{DeactivationCause, UserId};

/// The share of active users one run may deactivate.
const DEFAULT_MAX_PERCENT: u32 = 10;
/// The number of users one run may always deactivate, whatever the share.
const DEFAULT_MIN_ALLOWED: u32 = 2;

/// A run deactivates at most `max(min_allowed, max_percent % of the active users)`, and never
/// everybody on an instance of two or more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Brake {
  max_percent: u32,
  min_allowed: u32,
}

/// One deactivation a run wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedDeactivation {
  pub user: UserId,
  pub cause: DeactivationCause,
  pub note: &'static str,
}

/// A deactivation held back earlier, as far as the brake cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeldEntry {
  pub user: UserId,
  pub is_confirmed: bool,
}

/// What a run does with its planned deactivations.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Decision {
  pub apply: Vec<PlannedDeactivation>,
  pub hold: Vec<PlannedDeactivation>,
  /// This run itself is the alarm: its findings as a whole look wrong, so the run changes nothing
  /// else either (no roles, no groups).
  pub is_tripped: bool,
}

impl Brake {
  /// A brake of `max_percent`, at most 100; a run may always deactivate at least one user.
  pub fn new(max_percent: u32, min_allowed: u32) -> Self {
    Self {
      max_percent: max_percent.min(100),
      min_allowed: min_allowed.max(1),
    }
  }

  pub fn max_percent(&self) -> u32 {
    self.max_percent
  }

  pub fn min_allowed(&self) -> u32 {
    self.min_allowed
  }

  /// How many users one run may deactivate with this many active users. The share is rounded up.
  pub fn allowed(&self, active: usize) -> usize {
    let share = active.saturating_mul(self.max_percent as usize).div_ceil(100);
    share.max(self.min_allowed as usize)
  }

  /// Whether deactivating `planned` of `active` users is too many for one run. A single user
  /// always passes; a share of a tiny instance means nothing, so the allowance is a number there,
  /// but no run may take everybody on an instance of two or more.
  fn trips(&self, planned: usize, active: usize) -> bool {
    planned > self.allowed(active) || (active >= 2 && planned >= active)
  }

  /// Sorts the planned deactivations of a run.
  ///
  /// Confirmed ones go ahead. Of the others, a run as a whole within the allowance goes ahead too,
  /// which also ends a hold once the provider is fixed. If too many, only the newly appearing ones
  /// are judged by themselves: the brake holds what it has already held, and a leaver who shows up
  /// while it is engaged is not stopped by it. Only if those are too many as well is the run
  /// itself the alarm, and all of it waits.
  pub fn decide(&self, planned: Vec<PlannedDeactivation>, held: &[HeldEntry], active: usize) -> Decision {
    let confirmed: HashSet<UserId> = held
      .iter()
      .filter(|entry| entry.is_confirmed)
      .map(|entry| entry.user)
      .collect();
    let earlier: HashSet<UserId> = held.iter().map(|entry| entry.user).collect();
    let (apply_first, rest): (Vec<_>, Vec<_>) = planned
      .into_iter()
      .partition(|deactivation| confirmed.contains(&deactivation.user));
    let mut decision = Decision {
      apply: apply_first,
      ..Decision::default()
    };
    if !self.trips(rest.len(), active) {
      decision.apply.extend(rest);
      return decision;
    }
    let (still_held, fresh): (Vec<_>, Vec<_>) = rest
      .into_iter()
      .partition(|deactivation| earlier.contains(&deactivation.user));
    if !still_held.is_empty() && !self.trips(fresh.len(), active) {
      decision.apply.extend(fresh);
      decision.hold = still_held;
      return decision;
    }
    decision.hold = still_held;
    decision.hold.extend(fresh);
    decision.is_tripped = true;
    decision
  }
}

impl Default for Brake {
  fn default() -> Self {
    Self::new(DEFAULT_MAX_PERCENT, DEFAULT_MIN_ALLOWED)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn user(id: u64) -> UserId {
    serde_json::from_value(serde_json::json!(id.to_string())).unwrap()
  }

  fn planned(ids: impl IntoIterator<Item = u64>) -> Vec<PlannedDeactivation> {
    ids
      .into_iter()
      .map(|id| PlannedDeactivation {
        user: user(id),
        cause: DeactivationCause::Disabled,
        note: "disabled",
      })
      .collect()
  }

  fn held(ids: impl IntoIterator<Item = u64>, is_confirmed: bool) -> Vec<HeldEntry> {
    ids
      .into_iter()
      .map(|id| HeldEntry {
        user: user(id),
        is_confirmed,
      })
      .collect()
  }

  #[test]
  fn the_allowance_is_a_share_on_large_instances_and_a_number_on_small_ones() {
    let brake = Brake::default();
    assert_eq!(brake.allowed(1), 2);
    assert_eq!(brake.allowed(3), 2);
    assert_eq!(brake.allowed(20), 2);
    assert_eq!(brake.allowed(21), 3);
    assert_eq!(brake.allowed(1000), 100);
  }

  #[test]
  fn one_leaver_is_never_held_back() {
    for active in [1, 2, 3, 5, 50, 1000] {
      let decision = Brake::default().decide(planned([1]), &[], active);
      assert!(
        decision.hold.is_empty() && !decision.is_tripped,
        "held back on {active}"
      );
      assert_eq!(decision.apply.len(), 1);
    }
  }

  #[test]
  fn a_misconfigured_brake_still_lets_one_leaver_through() {
    let brake = Brake::new(0, 0);
    assert_eq!(brake.allowed(1000), 1);
    assert!(brake.decide(planned([1]), &[], 1000).hold.is_empty());
  }

  #[test]
  fn no_run_takes_everybody_of_a_small_instance_even_within_the_number() {
    let decision = Brake::new(10, 5).decide(planned([1, 2, 3]), &[], 3);
    assert!(decision.is_tripped);
    assert_eq!(decision.hold.len(), 3);
    assert!(decision.apply.is_empty());
  }

  #[test]
  fn the_last_user_of_a_one_person_instance_is_a_single_leaver() {
    let decision = Brake::default().decide(planned([1]), &[], 1);
    assert_eq!(decision.apply.len(), 1);
  }

  #[test]
  fn too_many_at_once_are_all_held_and_nothing_is_applied() {
    let decision = Brake::default().decide(planned(1..=30), &[], 100);
    assert!(decision.is_tripped);
    assert_eq!(decision.hold.len(), 30);
    assert!(decision.apply.is_empty());
  }

  #[test]
  fn a_new_leaver_is_not_stopped_by_what_is_already_held() {
    let earlier = held(1..=30, false);
    let decision = Brake::default().decide(planned((1..=30).chain([77])), &earlier, 100);
    assert!(!decision.is_tripped);
    assert_eq!(decision.apply.len(), 1);
    assert_eq!(decision.apply[0].user, user(77));
    assert_eq!(decision.hold.len(), 30);
  }

  #[test]
  fn fixing_the_provider_ends_the_hold() {
    let earlier = held(1..=30, false);
    let decision = Brake::default().decide(planned([7]), &earlier, 100);
    assert_eq!(decision.apply.len(), 1, "the one real leaver goes through");
    assert!(decision.hold.is_empty());
    assert!(!decision.is_tripped);
  }

  #[test]
  fn confirmed_ones_go_ahead_and_new_ones_are_judged_alone() {
    let earlier = held(1..=30, true);
    let decision = Brake::default().decide(planned((1..=30).chain(100..=140)), &earlier, 100);
    assert_eq!(decision.apply.len(), 30, "the confirmed ones");
    assert_eq!(decision.hold.len(), 41);
    assert!(decision.is_tripped);
  }

  #[test]
  fn a_growing_problem_trips_again_and_holds_the_new_ones_too() {
    let earlier = held(1..=30, false);
    let decision = Brake::default().decide(planned(1..=60), &earlier, 100);
    assert!(decision.is_tripped);
    assert_eq!(decision.hold.len(), 60);
    assert!(decision.apply.is_empty());
  }
}
