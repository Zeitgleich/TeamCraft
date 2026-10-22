use std::collections::HashMap;
use std::sync::Mutex;

/// How long a logout is remembered: longer than any access token lives.
const RETENTION_SECS: i64 = 24 * 60 * 60;

/// Sessions ended by the identity provider (back-channel logout).
///
/// Access tokens are self-contained and stay valid until they expire, so a logout is remembered:
/// a token issued before the user's logout, or belonging to a revoked provider session, is refused.
/// State is in memory per process; after a restart the token lifetime (minutes) bounds the gap.
#[derive(Default)]
pub struct Revocations {
  inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
  /// Subject -> time of the logout (Unix seconds).
  subjects: HashMap<String, i64>,
  /// Provider session ID -> time of the logout.
  sessions: HashMap<String, i64>,
}

impl Revocations {
  /// Tokens of `subject` issued at or before `at` are no longer accepted.
  pub fn revoke_subject(&self, subject: &str, at: i64) {
    let mut inner = self.lock();
    inner.subjects.insert(subject.to_owned(), at);
    inner.prune(at);
  }

  pub fn revoke_session(&self, session_id: &str, at: i64) {
    let mut inner = self.lock();
    inner.sessions.insert(session_id.to_owned(), at);
    inner.prune(at);
  }

  pub fn is_revoked(&self, subject: &str, maybe_session_id: Option<&str>, issued_at: i64) -> bool {
    let inner = self.lock();
    let subject_revoked = inner.subjects.get(subject).is_some_and(|at| issued_at <= *at);
    let session_revoked = maybe_session_id.is_some_and(|sid| inner.sessions.contains_key(sid));
    subject_revoked || session_revoked
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
    self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }
}

impl Inner {
  fn prune(&mut self, now: i64) {
    self.subjects.retain(|_, at| now - *at < RETENTION_SECS);
    self.sessions.retain(|_, at| now - *at < RETENTION_SECS);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn tokens_issued_before_or_at_the_logout_are_refused() {
    let revocations = Revocations::default();
    revocations.revoke_subject("user-1", 1000);
    assert!(revocations.is_revoked("user-1", None, 999));
    assert!(revocations.is_revoked("user-1", None, 1000));
    assert!(
      !revocations.is_revoked("user-1", None, 1001),
      "a later login works"
    );
    assert!(!revocations.is_revoked("user-2", None, 500));
  }

  #[test]
  fn a_revoked_provider_session_is_refused_whenever_the_token_was_issued() {
    let revocations = Revocations::default();
    revocations.revoke_session("sid-1", 1000);
    assert!(revocations.is_revoked("anyone", Some("sid-1"), 5000));
    assert!(!revocations.is_revoked("anyone", Some("sid-2"), 500));
    assert!(!revocations.is_revoked("anyone", None, 500));
  }

  #[test]
  fn old_entries_are_forgotten() {
    let revocations = Revocations::default();
    revocations.revoke_subject("old", 0);
    revocations.revoke_subject("new", RETENTION_SECS + 10);
    assert!(!revocations.is_revoked("old", None, 0));
    assert!(revocations.is_revoked("new", None, 0));
  }
}
