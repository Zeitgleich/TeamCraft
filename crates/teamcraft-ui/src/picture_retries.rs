//! When pictures that did not load are asked for again (issue #743). One pause for all of them: while
//! the object storage is down, avatars, space pictures and previews on screen wait for the same
//! round, with capped exponential backoff and jitter ([`RetryRounds`]), so an outage that ends needs
//! no reload and costs no flood of requests. What is on screen meanwhile stays: the cached picture,
//! the initials or the letter tile.

use std::time::Duration;

use leptos::prelude::*;
use rand_core::{OsRng, RngCore};
use teamcraft_core::retry::RetryRounds;
use teamcraft_proto::ErrorBody;

/// The rounds of the app; what failed subscribes through [`Self::round`].
#[derive(Clone, Copy)]
pub struct PictureRetries {
  rounds: StoredValue<RetryRounds>,
  round: RwSignal<u32>,
}

/// Why a picture was not delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PictureMiss {
  /// Trouble that passes (network, 5xx, 429), with what the server asked to wait, if it said.
  Later(Option<Duration>),
  /// The server says no (not found, forbidden): asking again doesn't help.
  Refused,
}

impl PictureRetries {
  pub fn new() -> Self {
    Self {
      rounds: StoredValue::new(RetryRounds::for_pictures(OsRng.next_u64())),
      round: RwSignal::new(0),
    }
  }

  /// Which round it is; read it where a failed picture should be asked for again. Reactive.
  pub fn round(&self) -> u32 {
    self.round.get()
  }

  /// A picture did not load: plans the next round unless one is planned.
  pub fn failed(&self, maybe_server_wait: Option<Duration>) {
    let Some(Some(wait)) = self
      .rounds
      .try_update_value(|rounds| rounds.failed(maybe_server_wait))
    else {
      return;
    };
    let retries = *self;
    set_timeout(move || retries.start_round(), wait);
  }

  /// A picture came through: the next failure starts from the shortest wait again.
  pub fn succeeded(&self) {
    self.rounds.try_update_value(RetryRounds::succeeded);
  }

  fn start_round(&self) {
    self.rounds.try_update_value(RetryRounds::round_started);
    self.round.try_update(|round| *round = round.wrapping_add(1));
  }
}

impl Default for PictureRetries {
  fn default() -> Self {
    Self::new()
  }
}

impl PictureMiss {
  /// What a picture's answer means. 5xx and 429 pass; other refusals are final.
  pub fn of_status(status: u16, body: &[u8]) -> Self {
    if status >= 500 || status == 429 || status == 408 {
      Self::Later(server_wait(body))
    } else {
      Self::Refused
    }
  }
}

pub fn use_picture_retries() -> Option<PictureRetries> {
  use_context::<PictureRetries>()
}

/// What an error body asked to wait: every 503 of the server says so.
fn server_wait(body: &[u8]) -> Option<Duration> {
  serde_json::from_slice::<ErrorBody>(body)
    .ok()
    .and_then(|body| body.maybe_retry_after_secs)
    .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
  use leptos::prelude::Owner;

  use super::*;

  #[test]
  fn outages_pass_and_refusals_are_final() {
    assert_eq!(PictureMiss::of_status(503, b"nope"), PictureMiss::Later(None));
    assert_eq!(PictureMiss::of_status(504, b""), PictureMiss::Later(None));
    assert_eq!(PictureMiss::of_status(429, b""), PictureMiss::Later(None));
    assert_eq!(PictureMiss::of_status(404, b""), PictureMiss::Refused);
    assert_eq!(PictureMiss::of_status(403, b""), PictureMiss::Refused);
  }

  #[test]
  fn the_server_s_wait_comes_from_the_error_body() {
    let body = serde_json::to_vec(
      &ErrorBody::new(teamcraft_proto::ErrorCode::Unavailable, "later").with_retry_after_secs(30),
    )
    .expect("serialize");
    assert_eq!(
      PictureMiss::of_status(503, &body),
      PictureMiss::Later(Some(Duration::from_secs(30)))
    );
  }

  #[test]
  fn a_round_moves_the_counter_and_a_dead_page_is_left_alone() {
    let owner = Owner::new();
    owner.set();
    let retries = PictureRetries::new();
    assert_eq!(retries.round.get_untracked(), 0);
    retries.start_round();
    assert_eq!(retries.round.get_untracked(), 1);
    owner.cleanup();
    // A timer that fires after the page went away must not panic.
    retries.start_round();
  }
}
