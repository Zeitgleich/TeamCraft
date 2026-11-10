//! When pictures that did not load are asked for again (issues #743, #1616): the core's one policy
//! for it, [`RetryRounds`], for the mobile apps. The app keeps the timer and the pictures; this
//! object keeps the arithmetic, so Android and iOS back off like the web and desktop apps: one
//! pause for everything that failed for the same reason, 5 seconds doubling up to 5 minutes, with
//! jitter.

use std::sync::Mutex;
use std::time::Duration;

use rand_core::{OsRng, RngCore};
use teamcraft_core::retry::{RetryRounds, millis};

/// The rounds of one app. Cheap to share between threads; every method returns at once.
#[derive(uniffi::Object)]
pub struct PictureRetryRounds {
  rounds: Mutex<RetryRounds>,
}

#[uniffi::export]
impl PictureRetryRounds {
  #[uniffi::constructor]
  pub fn new() -> Self {
    Self::with_seed(OsRng.next_u64())
  }

  /// A picture did not load because of trouble that passes (network, 5xx, 429). `Some(wait_ms)`:
  /// no round is planned yet, run one after that wait; `None`: it is part of the planned round.
  /// `maybe_server_wait_ms` is what the server asked to wait, if it said; it is the lower bound.
  pub fn failed(&self, maybe_server_wait_ms: Option<u64>) -> Option<u64> {
    self
      .with_rounds(|rounds| rounds.failed(maybe_server_wait_ms.map(Duration::from_millis)))
      .map(millis_u64)
  }

  /// The wait is over and the round runs.
  pub fn round_started(&self) {
    self.with_rounds(RetryRounds::round_started);
  }

  /// A picture came through: the next failure starts from the shortest wait again.
  pub fn succeeded(&self) {
    self.with_rounds(RetryRounds::succeeded);
  }

  /// The network came back: run a round now (the planned one is dropped) and start the backoff over.
  pub fn restart(&self) {
    self.with_rounds(RetryRounds::restart);
  }
}

impl PictureRetryRounds {
  fn with_seed(jitter_seed: u64) -> Self {
    Self {
      rounds: Mutex::new(RetryRounds::for_pictures(jitter_seed)),
    }
  }

  fn with_rounds<T>(&self, action: impl FnOnce(&mut RetryRounds) -> T) -> T {
    // A panic elsewhere must not stop pictures from being asked for: the state is plain counters.
    let mut rounds = self
      .rounds
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    action(&mut rounds)
  }
}

impl Default for PictureRetryRounds {
  fn default() -> Self {
    Self::new()
  }
}

fn millis_u64(duration: Duration) -> u64 {
  u64::try_from(millis(duration)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
  use super::*;

  const FIRST_MS: u64 = 5_000;
  const MAX_MS: u64 = 5 * 60 * 1_000;

  #[test]
  fn everything_failing_at_once_waits_for_one_round() {
    let rounds = PictureRetryRounds::with_seed(1);
    let wait = rounds.failed(None).expect("the first failure plans a round");
    assert!(
      (FIRST_MS / 2..=FIRST_MS).contains(&wait),
      "{wait} ms is the first wait with jitter"
    );
    assert_eq!(rounds.failed(None), None);
    assert_eq!(rounds.failed(Some(60_000)), None);
  }

  #[test]
  fn rounds_that_keep_failing_wait_longer_up_to_the_ceiling() {
    let rounds = PictureRetryRounds::with_seed(2);
    let mut previous_ceiling = 0;
    for round in 0..12 {
      let wait = rounds.failed(None).expect("planned");
      rounds.round_started();
      let ceiling = (FIRST_MS << round.min(10)).min(MAX_MS);
      assert!(
        wait <= ceiling && wait >= ceiling / 2,
        "round {round}: {wait} ms of {ceiling}"
      );
      assert!(ceiling >= previous_ceiling);
      previous_ceiling = ceiling;
    }
    assert_eq!(previous_ceiling, MAX_MS);
  }

  #[test]
  fn the_servers_wait_is_the_lower_bound() {
    let rounds = PictureRetryRounds::with_seed(3);
    let wait = rounds.failed(Some(30_000)).expect("planned");
    assert!(wait >= 30_000, "{wait} ms");
  }

  #[test]
  fn a_network_change_runs_a_round_at_once_and_starts_over() {
    let rounds = PictureRetryRounds::with_seed(4);
    for _ in 0..4 {
      rounds.failed(None);
      rounds.round_started();
    }
    let long = rounds.failed(None).expect("planned");
    assert!(long > FIRST_MS, "{long} ms after four failed rounds");
    rounds.restart();
    let wait = rounds.failed(None).expect("a round can be planned again");
    assert!(wait <= FIRST_MS, "{wait} ms: the backoff started over");
  }

  #[test]
  fn a_success_starts_the_backoff_over() {
    let rounds = PictureRetryRounds::with_seed(5);
    for _ in 0..4 {
      rounds.failed(None);
      rounds.round_started();
    }
    rounds.succeeded();
    let wait = rounds.failed(None).expect("planned");
    assert!(wait <= FIRST_MS, "{wait} ms");
  }
}
