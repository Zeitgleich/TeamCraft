//! The clients' one retry policy (issue #743): capped exponential backoff with equal jitter, and
//! the server's `Retry-After` as the lower bound of the wait. Every retry loop of the core uses it
//! (gateway reconnects, the offline queue, removals, rejoin requests); the arithmetic is shared with
//! the server (`teamcraft_net::retry`), so both sides back off the same way.

use std::time::Duration;

pub use teamcraft_net::retry::{Jitter, JitterMode, RetryPolicy, parse_retry_after};

use crate::api::ApiError;
use crate::error::CoreError;

/// Shortest wait of a [`RetryRounds`] for pictures.
const PICTURE_RETRY_FIRST: Duration = Duration::from_secs(5);
/// Longest wait of a [`RetryRounds`] for pictures.
const PICTURE_RETRY_MAX: Duration = Duration::from_secs(5 * 60);

/// One pause for everything that failed for the same reason, in place of a timer per item: while the
/// storage is down, every picture on screen waits for the same round instead of asking on its own
/// schedule. The caller keeps the timer and runs a round when it ends.
#[derive(Clone, Debug)]
pub struct RetryRounds {
  policy: RetryPolicy,
  jitter: Jitter,
  rounds_failed: u32,
  is_waiting: bool,
}

impl RetryRounds {
  pub fn new(policy: RetryPolicy, jitter_seed: u64) -> Self {
    Self {
      policy,
      jitter: Jitter::new(jitter_seed),
      rounds_failed: 0,
      is_waiting: false,
    }
  }

  /// The policy for pictures: 5 seconds, doubling up to 5 minutes, with jitter.
  pub fn for_pictures(jitter_seed: u64) -> Self {
    Self::new(
      RetryPolicy::new(PICTURE_RETRY_FIRST, PICTURE_RETRY_MAX),
      jitter_seed,
    )
  }

  /// Something failed. `Some(wait)`: no round is planned yet, the caller runs one after `wait`;
  /// `None`: it is part of the round already planned. `Retry-After` is the lower bound of the wait.
  pub fn failed(&mut self, maybe_server_wait: Option<Duration>) -> Option<Duration> {
    if self.is_waiting {
      return None;
    }
    self.is_waiting = true;
    let wait = self
      .policy
      .delay(self.rounds_failed, &mut self.jitter, maybe_server_wait);
    self.rounds_failed = self.rounds_failed.saturating_add(1);
    Some(wait)
  }

  /// The wait is over and the round runs.
  pub fn round_started(&mut self) {
    self.is_waiting = false;
  }

  /// Something came through: the next failure starts from the shortest wait again.
  pub fn succeeded(&mut self) {
    self.rounds_failed = 0;
  }
}

/// What the server asked a failed request to wait (`retry_after_secs` in the error body, filled
/// from the `Retry-After` header when only that was sent). Every 503 and 429 of our server carries
/// it.
pub fn retry_after(error: &ApiError) -> Option<Duration> {
  match error {
    ApiError::Status {
      maybe_body: Some(body),
      ..
    } => body.maybe_retry_after_secs.map(Duration::from_secs),
    _ => None,
  }
}

/// [`retry_after`] of a core error that came from the server.
pub fn core_retry_after(error: &CoreError) -> Option<Duration> {
  match error {
    CoreError::Api(api) => retry_after(api),
    _ => None,
  }
}

/// What the server asked a failed transfer to wait: the `Retry-After` header, else the error body's
/// `retry_after_secs`.
pub fn server_wait(error: &CoreError) -> Option<Duration> {
  match error {
    CoreError::Api(api) => api.retry_after().or_else(|| retry_after(api)),
    _ => None,
  }
}

/// Milliseconds for the core's `i64` clock.
pub fn millis(duration: Duration) -> i64 {
  i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{ErrorBody, ErrorCode};

  use super::*;

  #[test]
  fn retry_after_comes_from_the_error_body() {
    let unavailable = ApiError::Status {
      status: 503,
      maybe_body: Some(ErrorBody::new(ErrorCode::Unavailable, "later").with_retry_after_secs(30)),
      maybe_retry_after: None,
    };
    assert_eq!(retry_after(&unavailable), Some(Duration::from_secs(30)));
    assert_eq!(
      core_retry_after(&CoreError::Api(unavailable)),
      Some(Duration::from_secs(30))
    );
    let bare = ApiError::Status {
      status: 503,
      maybe_body: None,
      maybe_retry_after: None,
    };
    assert_eq!(retry_after(&bare), None);
    assert_eq!(retry_after(&ApiError::Network("down".into())), None);
  }

  mod rounds {
    use super::*;

    fn rounds() -> RetryRounds {
      RetryRounds::new(
        RetryPolicy::new(Duration::from_secs(5), Duration::from_secs(20)).with_jitter(JitterMode::None),
        1,
      )
    }

    #[test]
    fn everything_failing_at_once_waits_for_one_round() {
      let mut rounds = rounds();
      assert_eq!(rounds.failed(None), Some(Duration::from_secs(5)));
      assert_eq!(rounds.failed(None), None, "part of the planned round");
      assert_eq!(rounds.failed(Some(Duration::from_secs(60))), None);
    }

    #[test]
    fn rounds_that_keep_failing_wait_longer_up_to_the_ceiling() {
      let mut rounds = rounds();
      let waits: Vec<_> = (0..4)
        .map(|_| {
          let wait = rounds.failed(None);
          rounds.round_started();
          wait.map(|wait| wait.as_secs())
        })
        .collect();
      assert_eq!(waits, [Some(5), Some(10), Some(20), Some(20)]);
    }

    #[test]
    fn a_success_starts_the_backoff_over_and_the_server_can_ask_for_longer() {
      let mut rounds = rounds();
      for _ in 0..3 {
        rounds.failed(None);
        rounds.round_started();
      }
      rounds.succeeded();
      assert_eq!(rounds.failed(None), Some(Duration::from_secs(5)));
      rounds.round_started();
      rounds.succeeded();
      assert_eq!(
        rounds.failed(Some(Duration::from_secs(15))),
        Some(Duration::from_secs(15))
      );
    }

    #[test]
    fn the_pictures_policy_stays_within_five_minutes_and_spreads_clients() {
      let waits: std::collections::HashSet<_> = (0..40)
        .map(|seed| {
          let mut rounds = RetryRounds::for_pictures(seed);
          rounds.failed(None).expect("the first failure plans a round")
        })
        .collect();
      assert!(waits.len() > 1, "not every client comes back at once");
      assert!(waits.iter().all(|wait| *wait <= Duration::from_secs(5)));
    }
  }
}
