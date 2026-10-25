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
  /// (Issuer, subject) -> time of the logout (Unix seconds). Subjects of different issuers (the
  /// identity provider, bots, guests) never collide.
  subjects: HashMap<(String, String), i64>,
  /// (Issuer, provider session ID) -> time of the logout.
  sessions: HashMap<(String, String), i64>,
  /// (Issuer, subject) -> time of the last logout of one of the subject's provider sessions. Only
  /// tokens without a session ID are refused by it: they can't say which session they belong to.
  sessionless: HashMap<(String, String), i64>,
}

impl Revocations {
  /// Tokens of `subject` issued at or before `at` are no longer accepted.
  pub fn revoke_subject(&self, issuer: &str, subject: &str, at: i64) {
    let mut inner = self.lock();
    inner.subjects.insert((issuer.to_owned(), subject.to_owned()), at);
    inner.prune(at);
  }

  /// Tokens of the provider session `session_id` are no longer accepted, whenever they were issued.
  /// With the session's `maybe_subject`, also that subject's tokens without a session ID issued at
  /// or before `at`; the subject's other sessions stay (Back-Channel Logout 1.0 §2.4: a token with
  /// `sid` ends that session only).
  pub fn revoke_session(&self, issuer: &str, maybe_subject: Option<&str>, session_id: &str, at: i64) {
    let mut inner = self.lock();
    inner
      .sessions
      .insert((issuer.to_owned(), session_id.to_owned()), at);
    if let Some(subject) = maybe_subject {
      inner
        .sessionless
        .insert((issuer.to_owned(), subject.to_owned()), at);
    }
    inner.prune(at);
  }

  pub fn is_revoked(
    &self,
    issuer: &str,
    subject: &str,
    maybe_session_id: Option<&str>,
    issued_at: i64,
  ) -> bool {
    let inner = self.lock();
    let key = (issuer.to_owned(), subject.to_owned());
    let issued_before =
      |logouts: &HashMap<(String, String), i64>| logouts.get(&key).is_some_and(|at| issued_at <= *at);
    let session_revoked = match maybe_session_id {
      Some(sid) => inner.sessions.contains_key(&(issuer.to_owned(), sid.to_owned())),
      None => issued_before(&inner.sessionless),
    };
    issued_before(&inner.subjects) || session_revoked
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
    self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }
}

impl Inner {
  fn prune(&mut self, now: i64) {
    self.subjects.retain(|_, at| now - *at < RETENTION_SECS);
    self.sessions.retain(|_, at| now - *at < RETENTION_SECS);
    self.sessionless.retain(|_, at| now - *at < RETENTION_SECS);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const ISSUER: &str = "https://auth.example.com/realms/company";

  #[test]
  fn a_logout_at_one_issuer_does_not_touch_the_same_subject_elsewhere() {
    let revocations = Revocations::default();
    revocations.revoke_subject(ISSUER, "42", 1000);
    assert!(revocations.is_revoked(ISSUER, "42", None, 900));
    assert!(!revocations.is_revoked("teamcraft-bot", "42", None, 900));
    revocations.revoke_session(ISSUER, None, "s", 1000);
    assert!(!revocations.is_revoked("teamcraft-guest", "x", Some("s"), 2000));
  }

  #[test]
  fn tokens_issued_before_or_at_the_logout_are_refused() {
    let revocations = Revocations::default();
    revocations.revoke_subject(ISSUER, "user-1", 1000);
    assert!(revocations.is_revoked(ISSUER, "user-1", None, 999));
    assert!(revocations.is_revoked(ISSUER, "user-1", None, 1000));
    assert!(
      !revocations.is_revoked(ISSUER, "user-1", None, 1001),
      "a later login works"
    );
    assert!(!revocations.is_revoked(ISSUER, "user-2", None, 500));
  }

  #[test]
  fn a_revoked_provider_session_is_refused_whenever_the_token_was_issued() {
    let revocations = Revocations::default();
    revocations.revoke_session(ISSUER, None, "sid-1", 1000);
    assert!(revocations.is_revoked(ISSUER, "anyone", Some("sid-1"), 5000));
    assert!(!revocations.is_revoked(ISSUER, "anyone", Some("sid-2"), 500));
    assert!(!revocations.is_revoked(ISSUER, "anyone", None, 500));
  }

  #[test]
  fn a_provider_session_logout_leaves_the_subjects_other_sessions() {
    let revocations = Revocations::default();
    revocations.revoke_session(ISSUER, Some("user-1"), "sid-phone", 1000);
    assert!(revocations.is_revoked(ISSUER, "user-1", Some("sid-phone"), 900));
    assert!(
      !revocations.is_revoked(ISSUER, "user-1", Some("sid-desktop"), 900),
      "the other device stays signed in"
    );
    assert!(
      revocations.is_revoked(ISSUER, "user-1", None, 900),
      "a token without a session ID may be the ended session's"
    );
    assert!(!revocations.is_revoked(ISSUER, "user-1", None, 1001));
    assert!(!revocations.is_revoked(ISSUER, "user-2", None, 900));
  }

  #[test]
  fn old_entries_are_forgotten() {
    let revocations = Revocations::default();
    revocations.revoke_subject(ISSUER, "old", 0);
    revocations.revoke_subject(ISSUER, "new", RETENTION_SECS + 10);
    assert!(!revocations.is_revoked(ISSUER, "old", None, 0));
    assert!(revocations.is_revoked(ISSUER, "new", None, 0));
  }
}
