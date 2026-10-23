//! How the server's mail went since it started, for the admin console (delivery status and the
//! last error). Kept in memory only.

use std::sync::{Arc, Mutex, PoisonError};

use teamcraft_proto::{MailDeliveryStatus, UnixMillis};
use time::OffsetDateTime;

use crate::auth::BoxFuture;
use crate::meetings::mailer::{MailError, Mailer, OutgoingMail};

/// Longest error text kept; mail servers can answer with long multi-line replies.
const MAX_ERROR_CHARS: usize = 500;

#[derive(Default)]
pub struct MailStatus {
  inner: Mutex<MailDeliveryStatus>,
}

/// A mailer that records each result in a [`MailStatus`].
pub struct TrackedMailer {
  inner: Arc<dyn Mailer>,
  status: Arc<MailStatus>,
}

impl MailStatus {
  pub fn snapshot(&self) -> MailDeliveryStatus {
    self.inner.lock().unwrap_or_else(PoisonError::into_inner).clone()
  }

  pub fn record(&self, result: &Result<(), MailError>, at: OffsetDateTime) {
    let millis = UnixMillis(i64::try_from(at.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX));
    let mut status = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
    match result {
      Ok(()) => status.maybe_last_success_at = Some(millis),
      Err(error) => {
        status.maybe_last_failure_at = Some(millis);
        status.maybe_last_error = Some(error.to_string().chars().take(MAX_ERROR_CHARS).collect());
      },
    }
  }

  /// Forgets the history, e.g. when the mail server changes.
  pub fn clear(&self) {
    *self.inner.lock().unwrap_or_else(PoisonError::into_inner) = MailDeliveryStatus::default();
  }
}

impl TrackedMailer {
  pub fn new(inner: Arc<dyn Mailer>, status: Arc<MailStatus>) -> Self {
    Self { inner, status }
  }
}

impl Mailer for TrackedMailer {
  fn send<'a>(&'a self, mail: &'a OutgoingMail) -> BoxFuture<'a, Result<(), MailError>> {
    Box::pin(async move {
      let result = self.inner.send(mail).await;
      self.status.record(&result, OffsetDateTime::now_utc());
      result
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn failures_keep_the_last_success_and_cap_the_error() {
    let status = MailStatus::default();
    let at = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
    status.record(&Ok(()), at);
    status.record(&Err(MailError::Send("x".repeat(2000))), at);
    let snapshot = status.snapshot();
    assert_eq!(
      snapshot.maybe_last_success_at,
      Some(UnixMillis(1_700_000_000_000))
    );
    assert_eq!(
      snapshot.maybe_last_failure_at,
      Some(UnixMillis(1_700_000_000_000))
    );
    assert_eq!(
      snapshot.maybe_last_error.unwrap().chars().count(),
      MAX_ERROR_CHARS
    );
    status.clear();
    assert_eq!(status.snapshot(), MailDeliveryStatus::default());
  }
}
