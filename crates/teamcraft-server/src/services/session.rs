//! Session policy enforced by the server (PLAN.md §11): session length, idle timeout and the
//! requirement of a second factor. The limits come from the signed workspace policy.

use std::sync::{Arc, RwLock};

use teamcraft_db::{LOCAL_ISSUER, SessionLimits, SessionState};
use teamcraft_proto::{Role, WorkspacePolicy};
use time::{Duration, OffsetDateTime};

use super::invites::is_domain_allowed;
use super::workspace::current_policy;
use crate::auth::{AuthError, TokenIdentity};
use crate::error::ApiError;
use crate::state::AppState;

/// `amr` values (RFC 8176) that name a second factor.
const SECOND_FACTOR_METHODS: [&str; 12] = [
  "mfa", "otp", "hwk", "swk", "fpt", "face", "iris", "retina", "vbm", "sms", "tel", "pop",
];
/// `acr` values that stand for a sign-in with more than a password.
const MFA_ACR_VALUES: [&str; 4] = [
  "mfa",
  "2",
  "3",
  "http://schemas.openid.net/pape/policies/2007/06/multi-factor",
];

/// The workspace policy in force, kept in memory because every request consults it. The database
/// is the source of truth; the policy endpoints and the startup load keep this copy current.
pub struct PolicyHolder {
  inner: RwLock<Arc<WorkspacePolicy>>,
}

impl PolicyHolder {
  pub fn new(policy: WorkspacePolicy) -> Self {
    Self {
      inner: RwLock::new(Arc::new(policy)),
    }
  }

  pub fn get(&self) -> Arc<WorkspacePolicy> {
    match self.inner.read() {
      Ok(guard) => guard.clone(),
      Err(poisoned) => poisoned.into_inner().clone(),
    }
  }

  pub fn set(&self, policy: WorkspacePolicy) {
    match self.inner.write() {
      Ok(mut guard) => *guard = Arc::new(policy),
      Err(poisoned) => *poisoned.into_inner() = Arc::new(policy),
    }
  }
}

impl Default for PolicyHolder {
  fn default() -> Self {
    Self::new(WorkspacePolicy::default())
  }
}

/// Reads the latest signed policy from the database into the in-memory copy (at startup).
pub async fn load_policy(state: &AppState) -> Result<(), ApiError> {
  let (_, policy, _) = current_policy(state).await?;
  state.policy.set(policy);
  Ok(())
}

/// Sessions nobody used for this long are forgotten (they are past any sensible limit).
const SESSION_RETENTION: Duration = Duration::days(30);

/// Deletes the records of sessions unused for [`SESSION_RETENTION`].
pub async fn forget_stale_sessions(state: &AppState, now: OffsetDateTime) -> Result<u64, ApiError> {
  Ok(
    state
      .database
      .delete_stale_sessions(now - SESSION_RETENTION)
      .await?,
  )
}

/// The limits the policy puts on sessions.
pub fn limits_for(state: &AppState) -> SessionLimits {
  let policy = state.policy.get();
  SessionLimits {
    maybe_max_age: policy
      .session
      .maybe_max_age_hours
      .map(|hours| Duration::hours(i64::from(hours))),
    maybe_idle_timeout: policy
      .session
      .maybe_idle_timeout_minutes
      .map(|minutes| Duration::minutes(i64::from(minutes))),
  }
}

/// Applies the session policy to a token of the identity provider: the session must be within its
/// limits and, if the policy requires it, signed in with a second factor. Local accounts check
/// their sessions when the token is validated; bots and meeting guests are exempt.
pub async fn enforce(state: &AppState, identity: &TokenIdentity) -> Result<(), ApiError> {
  // Local guests are exempt from the rest of the sign-in policy (their sessions are checked with
  // the token) but not from the guest rules; bots and meeting guests have no mailbox to check.
  if identity.issuer == LOCAL_ISSUER || !identity.is_exempt_from_session_policy {
    check_guest_sign_in(state, identity)?;
  }
  if identity.is_exempt_from_session_policy || identity.issuer == LOCAL_ISSUER {
    return Ok(());
  }
  let policy = state.policy.get();
  if policy.session.requires_mfa && !has_second_factor(identity) {
    return Err(ApiError::MfaRequired);
  }
  let limits = limits_for(state);
  let Some(session) = &identity.maybe_session_id else {
    return Ok(());
  };
  if limits == SessionLimits::default() {
    return Ok(());
  }
  match state
    .database
    .touch_session(
      &identity.issuer,
      &identity.subject,
      session,
      OffsetDateTime::now_utc(),
      limits,
      true,
    )
    .await?
  {
    SessionState::Active => Ok(()),
    SessionState::Expired | SessionState::Unknown => Err(AuthError::SessionTimedOut.into()),
  }
}

/// Guests may only sign in with a verified e-mail address of an allowed domain, also when the list
/// changed after they were invited. Without a verified address there is nothing to check, so no
/// sign-in while a restriction is in force (an unverified address could be claimed by anybody).
fn check_guest_sign_in(state: &AppState, identity: &TokenIdentity) -> Result<(), ApiError> {
  let policy = state.policy.get();
  let lists = [
    &state.settings.get().guests.allowed_email_domains,
    &policy.guests.allowed_email_domains,
  ];
  if is_guest_sign_in_allowed(&lists, identity) {
    Ok(())
  } else {
    Err(ApiError::Forbidden(
      "guests from this e-mail domain are not allowed".into(),
    ))
  }
}

fn is_guest_sign_in_allowed(lists: &[&Vec<String>], identity: &TokenIdentity) -> bool {
  identity.role != Role::Guest
    || lists.iter().all(|allowed| {
      allowed.is_empty()
        || (identity.is_email_verified
          && identity
            .maybe_email
            .as_deref()
            .is_some_and(|email| is_domain_allowed(allowed, email)))
    })
}

fn has_second_factor(identity: &TokenIdentity) -> bool {
  let has_method = identity
    .authentication_methods
    .iter()
    .any(|method| SECOND_FACTOR_METHODS.contains(&method.as_str()));
  let has_assurance = identity
    .maybe_acr
    .as_deref()
    .is_some_and(|acr| MFA_ACR_VALUES.contains(&acr));
  has_method || has_assurance
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::Role;

  use super::*;

  #[test]
  fn guests_need_a_verified_address_of_an_allowed_domain() {
    let list = vec!["partner.test".to_owned()];
    let none: Vec<String> = Vec::new();
    let guest = |email: Option<&str>, is_verified: bool| TokenIdentity {
      role: Role::Guest,
      maybe_email: email.map(str::to_owned),
      is_email_verified: is_verified,
      ..identity(&[], None)
    };
    assert!(
      is_guest_sign_in_allowed(&[&none, &none], &guest(None, false)),
      "no restriction"
    );
    assert!(is_guest_sign_in_allowed(
      &[&none, &list],
      &guest(Some("g@partner.test"), true)
    ));
    assert!(
      !is_guest_sign_in_allowed(&[&none, &list], &guest(Some("g@partner.test"), false)),
      "unverified"
    );
    assert!(!is_guest_sign_in_allowed(
      &[&none, &list],
      &guest(Some("g@evil.test"), true)
    ));
    assert!(!is_guest_sign_in_allowed(&[&none, &list], &guest(None, true)));
    assert!(!is_guest_sign_in_allowed(
      &[&list, &none],
      &guest(Some("g@evil.test"), true)
    ));
    // Members aren't touched.
    assert!(is_guest_sign_in_allowed(&[&none, &list], &identity(&[], None)));
  }

  #[test]
  fn second_factors_are_recognized_by_amr_and_acr() {
    assert!(!has_second_factor(&identity(&["pwd"], None)));
    assert!(!has_second_factor(&identity(&[], Some("1"))));
    assert!(!has_second_factor(&identity(&[], None)));
    assert!(has_second_factor(&identity(&["pwd", "otp"], None)));
    assert!(has_second_factor(&identity(&["hwk"], None)));
    assert!(has_second_factor(&identity(&["mfa"], None)));
    assert!(has_second_factor(&identity(&["pwd"], Some("2"))));
    assert!(has_second_factor(&identity(&[], Some("mfa"))));
  }

  fn identity(methods: &[&str], maybe_acr: Option<&str>) -> TokenIdentity {
    TokenIdentity {
      issuer: "https://idp.example.com".into(),
      subject: "s".into(),
      maybe_username: None,
      display_name: "S".into(),
      maybe_email: None,
      is_email_verified: false,
      role: Role::Member,
      issued_at: 0,
      authenticated_at: 0,
      maybe_session_id: Some("sid".into()),
      authentication_methods: methods.iter().map(|m| (*m).to_owned()).collect(),
      maybe_acr: maybe_acr.map(str::to_owned),
      is_exempt_from_session_policy: false,
      maybe_organization: None,
    }
  }
}
