//! The clients' one retry policy (issue #743): capped exponential backoff with equal jitter, and
//! the server's `Retry-After` as the lower bound of the wait. Every retry loop of the core uses it
//! (gateway reconnects, the offline queue, removals, rejoin requests); the arithmetic is shared with
//! the server (`teamcraft_net::retry`), so both sides back off the same way.

use std::time::Duration;

pub use teamcraft_net::retry::{Jitter, JitterMode, RetryPolicy, parse_retry_after};

use crate::api::ApiError;
use crate::error::CoreError;

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
    };
    assert_eq!(retry_after(&unavailable), Some(Duration::from_secs(30)));
    assert_eq!(
      core_retry_after(&CoreError::Api(unavailable)),
      Some(Duration::from_secs(30))
    );
    let bare = ApiError::Status {
      status: 503,
      maybe_body: None,
    };
    assert_eq!(retry_after(&bare), None);
    assert_eq!(retry_after(&ApiError::Network("down".into())), None);
  }
}
