//! Local guest accounts (PLAN.md §12): guests that are not in the identity provider sign in with a
//! password and a TOTP code. Sessions are tokens signed by this server and tracked in the database,
//! so they can end early (logout, password change, idle timeout).
//!
//! The first sign-in applies the guest's pending invite (space, channels, expiry) like any other
//! verified e-mail address does: the activation link proved the mailbox.

use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand_core::OsRng;
use secrecy::ExposeSecret as _;
use serde::{Deserialize, Serialize};
use serde_json::json;
use teamcraft_crypto::account::{
  self, PasswordCost, TotpSecret, derive_account_key, digest_token, hash_password, new_one_time_token,
  open_secret, seal_secret, verify_password, verify_password_for_unknown_account,
};
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_db::{LOCAL_ISSUER, LocalAccount, NewInvite, SessionState};
use teamcraft_proto::{
  ActivateLocalAccountRequest, ActivateLocalAccountResponse, ChangeLocalPasswordRequest,
  CreateLocalGuestRequest, LocalGuestResponse, LocalLoginRequest, LocalLoginResponse, Role, SpaceId,
  SpaceRole, UserId,
};
use time::{Duration, OffsetDateTime};

use super::access::load_space;
use super::audit::{self, Target as AuditTarget};
use super::convert::{presented_name, unix_millis, user_summary};
use super::invites::{check_channels, check_guest_domain, future_expiry, normalize_email};
use super::spaces::require_member_management;
use crate::auth::{AuthError, AuthenticatedUser, TokenIdentity};
use crate::config::LocalAccountsConfig;
use crate::error::ApiError;
use crate::meetings::ical::Method;
use crate::meetings::mailer::OutgoingMail;
use crate::state::AppState;
use tracing::warn;

const SESSION_AUDIENCE: &str = "teamcraft-local-session";
const MIN_SECRET_BYTES: usize = 32;
const MAX_PASSWORD_CHARS: usize = 256;
const MAX_DISPLAY_NAME_CHARS: usize = 100;
/// Codes of the previous and the next 30 seconds are accepted (clock drift).
const TOTP_WINDOW: u64 = 1;

/// Keys and settings of local accounts.
pub struct LocalAuth {
  encoding: EncodingKey,
  decoding: DecodingKey,
  account_key: SymmetricKey,
  cost: PasswordCost,
}

#[derive(Debug, thiserror::Error)]
pub enum LocalAuthError {
  #[error("local_accounts.session_secret must be set and have at least {MIN_SECRET_BYTES} bytes")]
  WeakSecret,
  #[error("deriving the account key failed")]
  KeyDerivation,
}

#[derive(Debug, Serialize, Deserialize)]
struct SessionClaims {
  iss: String,
  aud: String,
  /// The user ID.
  sub: String,
  /// The session key in `session_activity`.
  sid: String,
  iat: i64,
  exp: i64,
}

impl LocalAuth {
  pub fn new(config: &LocalAccountsConfig) -> Result<Self, LocalAuthError> {
    let secret = config
      .maybe_session_secret
      .as_ref()
      .map(|secret| secret.expose_secret().as_bytes())
      .filter(|secret| secret.len() >= MIN_SECRET_BYTES)
      .ok_or(LocalAuthError::WeakSecret)?;
    Ok(Self {
      encoding: EncodingKey::from_secret(secret),
      decoding: DecodingKey::from_secret(secret),
      account_key: derive_account_key(secret).map_err(|_| LocalAuthError::KeyDerivation)?,
      cost: PasswordCost {
        memory_kib: config.password_memory_kib,
        iterations: config.password_iterations,
        parallelism: config.password_parallelism,
      },
    })
  }

  fn sign(&self, claims: &SessionClaims) -> Result<String, ApiError> {
    jsonwebtoken::encode(&Header::new(Algorithm::HS256), claims, &self.encoding)
      .map_err(|error| ApiError::Internal(format!("signing a session failed: {error}")))
  }

  fn verify(&self, token: &str) -> Option<SessionClaims> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_audience(&[SESSION_AUDIENCE]);
    validation.set_issuer(&[LOCAL_ISSUER]);
    validation.leeway = 0;
    validation.required_spec_claims = ["exp", "iss", "aud", "sub"].map(String::from).into();
    jsonwebtoken::decode::<SessionClaims>(token, &self.decoding, &validation)
      .ok()
      .map(|data| data.claims)
  }
}

/// Creates a guest account for `request.email`, queues the invite into the space and mails the
/// activation link.
pub async fn create_local_guest(
  state: &AppState,
  caller: &AuthenticatedUser,
  space: SpaceId,
  request: CreateLocalGuestRequest,
) -> Result<LocalGuestResponse, ApiError> {
  let local = local_auth(state)?;
  let _ = local;
  let mailer = state
    .mailer
    .get()
    .ok_or_else(|| ApiError::Unavailable("no mail server is configured".into()))?;
  let access = load_space(state, caller, space).await?;
  require_member_management(state, caller, &SpaceRole::Guest, &access.target())?;
  if access.view.space.is_archived {
    return Err(ApiError::Conflict("the space is archived".into()));
  }
  let email = normalize_email(&request.email)?;
  check_guest_domain(state, &email)?;
  let display_name = request.display_name.trim().to_owned();
  if display_name.is_empty() || display_name.chars().count() > MAX_DISPLAY_NAME_CHARS {
    return Err(ApiError::InvalidRequest("invalid display_name".into()));
  }
  check_channels(state, caller, space, &request.channel_ids).await?;
  let maybe_expires_at = future_expiry(request.maybe_expires_at)?;

  let user: UserId = state.next_id()?;
  let token = new_one_time_token(&mut OsRng);
  let activation_expires_at = OffsetDateTime::now_utc()
    + Duration::hours(i64::from(
      state.settings.get().local_accounts.activation_ttl_hours,
    ));
  let is_created = state
    .database
    .create_local_account(
      user,
      &email,
      &display_name,
      &digest_token(&token),
      activation_expires_at,
    )
    .await?;
  if !is_created {
    return Err(ApiError::Conflict(
      "a local account with this e-mail address exists".into(),
    ));
  }
  // The guest gets the space, the channels and the expiry through the invite, applied at the first
  // sign-in like for every verified e-mail address.
  state
    .database
    .create_invite(&NewInvite {
      id: state.next_id()?,
      space_id: space,
      email: &email,
      role: SpaceRole::Guest,
      channel_ids: &request.channel_ids,
      sponsor: caller.user.id,
      maybe_expires_at,
    })
    .await?;
  audit::record(
    state,
    Some(caller.user.id),
    "local_guest_created",
    Some(AuditTarget::new("space", space)),
    &json!({"user_id": user.to_string()}),
  )
  .await?;

  let inviter_brand = super::invites::inviter_brand(state, &caller.user.id).await;
  let brands = state.brands.current();
  let brand = brands.for_recipient(&email, &inviter_brand);
  let url = format!(
    "{}/activate#token={token}",
    brand.base_url(&state.settings.get().mail.public_base_url)
  );
  let (subject, body) = brand.activation_texts(&state.settings.get().mail);
  let fill = |template: &str| {
    template
      .replace(
        "{inviter}",
        &presented_name(&state.policy.get().directory, &caller.user),
      )
      .replace("{space}", &access.view.space.name)
      .replace("{url}", &url)
  };
  let mail = OutgoingMail {
    to_address: email.clone(),
    maybe_to_name: Some(display_name.clone()),
    subject: fill(&subject),
    text: fill(&body),
    method: Method::Request,
    ics: String::new(),
    maybe_from_name: None,
    maybe_from_address: None,
  }
  .as_brand(brand);
  mailer
    .send(&mail)
    .await
    .map_err(|error| ApiError::Unavailable(format!("sending the activation e-mail failed: {error}")))?;

  let created = state
    .database
    .get_user(user)
    .await?
    .ok_or_else(ApiError::user_not_found)?;
  Ok(LocalGuestResponse {
    user: user_summary(state, &created),
    email,
    activation_expires_at: unix_millis(activation_expires_at),
  })
}

/// Sends a new activation link to an existing local account and locks it until it is used (lost
/// password or authenticator). Instance admins only.
pub async fn reissue_activation(
  state: &AppState,
  caller: &AuthenticatedUser,
  user: UserId,
) -> Result<LocalGuestResponse, ApiError> {
  let _ = local_auth(state)?;
  let mailer = state
    .mailer
    .get()
    .ok_or_else(|| ApiError::Unavailable("no mail server is configured".into()))?;
  let account = state
    .database
    .get_local_account(user)
    .await?
    .ok_or_else(ApiError::user_not_found)?;
  let token = new_one_time_token(&mut OsRng);
  let activation_expires_at = OffsetDateTime::now_utc()
    + Duration::hours(i64::from(
      state.settings.get().local_accounts.activation_ttl_hours,
    ));
  state
    .database
    .reissue_local_activation(user, &digest_token(&token), activation_expires_at)
    .await?;
  state
    .database
    .end_sessions_of(LOCAL_ISSUER, &user.to_string())
    .await?;
  audit::record(
    state,
    Some(caller.user.id),
    "local_account_reset",
    Some(AuditTarget::new("user", user)),
    &json!({}),
  )
  .await?;
  let inviter_brand = super::invites::inviter_brand(state, &caller.user.id).await;
  let brands = state.brands.current();
  let brand = brands.for_recipient(&account.login, &inviter_brand);
  let url = format!(
    "{}/activate#token={token}",
    brand.base_url(&state.settings.get().mail.public_base_url)
  );
  let (subject, body) = brand.activation_texts(&state.settings.get().mail);
  let mail = OutgoingMail {
    to_address: account.login.clone(),
    maybe_to_name: None,
    subject: subject
      .replace(
        "{inviter}",
        &presented_name(&state.policy.get().directory, &caller.user),
      )
      .replace("{space}", &brand.display_name),
    text: body
      .replace(
        "{inviter}",
        &presented_name(&state.policy.get().directory, &caller.user),
      )
      .replace("{space}", &brand.display_name)
      .replace("{url}", &url),
    method: Method::Request,
    ics: String::new(),
    maybe_from_name: None,
    maybe_from_address: None,
  }
  .as_brand(brand);
  mailer
    .send(&mail)
    .await
    .map_err(|error| ApiError::Unavailable(format!("sending the activation e-mail failed: {error}")))?;
  let target = state
    .database
    .get_user(user)
    .await?
    .ok_or_else(ApiError::user_not_found)?;
  Ok(LocalGuestResponse {
    user: user_summary(state, &target),
    email: account.login,
    activation_expires_at: unix_millis(activation_expires_at),
  })
}

/// Spends the activation token and sets the password; returns the TOTP secret to enroll.
pub async fn activate(
  state: &AppState,
  request: ActivateLocalAccountRequest,
) -> Result<ActivateLocalAccountResponse, ApiError> {
  let local = local_auth(state)?;
  check_password_rules(state, &request.password)?;
  let now = OffsetDateTime::now_utc();
  let digest = digest_token(&request.token);
  let account = state
    .database
    .find_local_account_by_activation(&digest, now)
    .await?
    .ok_or_else(|| AuthError::InvalidToken("the activation link is invalid or expired".into()))?;
  let password_hash = hash_in_background(request.password, local.cost).await?;
  let secret = TotpSecret::generate(&mut OsRng);
  let sealed = seal_secret(
    &local.account_key,
    account.user.to_string().as_bytes(),
    secret.as_bytes(),
    &mut OsRng,
  )
  .map_err(|error| ApiError::Internal(error.to_string()))?;
  let activated = state
    .database
    .activate_local_account(&digest, now, &password_hash, &sealed)
    .await?;
  let Some((user, login)) = activated else {
    return Err(AuthError::InvalidToken("the activation link is invalid or expired".into()).into());
  };
  audit::record(
    state,
    Some(user),
    "local_account_activated",
    Some(AuditTarget::new("user", user)),
    &json!({}),
  )
  .await?;
  Ok(ActivateLocalAccountResponse {
    totp_secret: secret.to_base32(),
    totp_uri: secret.provisioning_uri(&state.settings.get().local_accounts.totp_issuer, &login),
  })
}

/// Checks password and TOTP code and opens a session.
pub async fn login(state: &AppState, request: LocalLoginRequest) -> Result<LocalLoginResponse, ApiError> {
  let local = local_auth(state)?;
  let settings = &state.settings.get().local_accounts;
  let now = OffsetDateTime::now_utc();
  let login = request.login.trim().to_lowercase();
  let Some(account) = state.database.find_local_account(&login).await? else {
    spend_time_like_a_check(request.password, local.cost).await;
    return Err(invalid_credentials());
  };
  if let Some(until) = account.maybe_locked_until
    && until > now
  {
    let retry_after_secs = u64::try_from((until - now).whole_seconds()).unwrap_or(1).max(1);
    return Err(ApiError::RateLimited { retry_after_secs });
  }
  let Some(stored_hash) = account.maybe_password_hash.clone() else {
    spend_time_like_a_check(request.password, local.cost).await;
    return Err(invalid_credentials());
  };
  let password = request.password.clone();
  let is_password_right = tokio::task::spawn_blocking(move || verify_password(&password, &stored_hash))
    .await
    .map_err(|error| ApiError::Internal(error.to_string()))?
    .map_err(|error| ApiError::Internal(error.to_string()))?;
  if !is_password_right {
    return fail(state, &account, now).await;
  }

  if let Some(sealed) = &account.maybe_totp_secret_sealed {
    let must_use_code =
      settings.require_totp || account.is_totp_confirmed || state.policy.get().session.requires_mfa;
    if must_use_code {
      let Some(code) = request.maybe_totp_code.as_deref() else {
        return Err(AuthError::InvalidToken("a one-time code is required".into()).into());
      };
      let secret = open_secret(&local.account_key, account.user.to_string().as_bytes(), sealed)
        .map_err(|error| ApiError::Internal(error.to_string()))?;
      let secret = TotpSecret::from_bytes(secret.expose_secret());
      let unix_secs = u64::try_from(now.unix_timestamp()).unwrap_or(0);
      let maybe_step = secret.verify(code, unix_secs, TOTP_WINDOW);
      let is_accepted = match maybe_step {
        Some(step) => {
          state
            .database
            .use_totp_step(
              account.user,
              i64::try_from(step).unwrap_or(i64::MAX),
              !account.is_totp_confirmed,
            )
            .await?
        },
        None => false,
      };
      if !is_accepted {
        return fail(state, &account, now).await;
      }
    }
  }

  state.database.clear_login_failures(account.user).await?;
  rehash_if_needed(state, local, &account, &request.password).await?;

  let session_key = new_one_time_token(&mut OsRng);
  let subject = account.user.to_string();
  state
    .database
    .start_session(LOCAL_ISSUER, &subject, &session_key, now)
    .await?;
  let ttl = session_ttl(state);
  let expires_at = now + ttl;
  let access_token = local.sign(&SessionClaims {
    iss: LOCAL_ISSUER.into(),
    aud: SESSION_AUDIENCE.into(),
    sub: subject,
    sid: session_key,
    iat: now.unix_timestamp(),
    exp: expires_at.unix_timestamp(),
  })?;
  audit::record(
    state,
    Some(account.user),
    "local_login",
    Some(AuditTarget::new("user", account.user)),
    &json!({}),
  )
  .await?;
  Ok(LocalLoginResponse {
    access_token,
    expires_at: unix_millis(expires_at),
    user_id: account.user,
  })
}

/// Ends the session of the token the request came with.
pub async fn logout(state: &AppState, caller: &AuthenticatedUser) -> Result<(), ApiError> {
  if let Some(session) = &caller.maybe_oidc_session
    && state.database.get_local_account(caller.user.id).await?.is_some()
  {
    state
      .database
      .end_session(LOCAL_ISSUER, &caller.user.id.to_string(), session)
      .await?;
  }
  Ok(())
}

/// Changes the password of a local account and ends all its sessions.
pub async fn change_password(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: ChangeLocalPasswordRequest,
) -> Result<(), ApiError> {
  let local = local_auth(state)?;
  check_password_rules(state, &request.new_password)?;
  let account = state
    .database
    .get_local_account(caller.user.id)
    .await?
    .ok_or_else(|| ApiError::Forbidden("only local accounts have a password here".into()))?;
  let stored_hash = account
    .maybe_password_hash
    .ok_or_else(|| ApiError::Forbidden("the account is not activated".into()))?;
  let current = request.current_password;
  let is_right = tokio::task::spawn_blocking(move || verify_password(&current, &stored_hash))
    .await
    .map_err(|error| ApiError::Internal(error.to_string()))?
    .map_err(|error| ApiError::Internal(error.to_string()))?;
  if !is_right {
    return Err(ApiError::Forbidden("the current password is wrong".into()));
  }
  let new_hash = hash_in_background(request.new_password, local.cost).await?;
  state
    .database
    .set_local_password(caller.user.id, &new_hash)
    .await?;
  state
    .database
    .end_sessions_of(LOCAL_ISSUER, &caller.user.id.to_string())
    .await?;
  audit::record(
    state,
    Some(caller.user.id),
    "local_password_changed",
    Some(AuditTarget::new("user", caller.user.id)),
    &json!({}),
  )
  .await?;
  Ok(())
}

/// The identity of a valid local session token, or `None` if the token is not one of ours. The
/// session must be known and within its limits.
pub async fn validate_session_token(
  state: &AppState,
  token: &str,
) -> Result<Option<TokenIdentity>, AuthError> {
  let Some(local) = &state.maybe_local else {
    return Ok(None);
  };
  let Some(claims) = local.verify(token) else {
    return Ok(None);
  };
  let internal = |error: &dyn std::fmt::Display| AuthError::KeysUnavailable(error.to_string());
  let limits = super::session::limits_for(state);
  let state_of_session = state
    .database
    .touch_session(
      LOCAL_ISSUER,
      &claims.sub,
      &claims.sid,
      OffsetDateTime::now_utc(),
      limits,
      false,
    )
    .await
    .map_err(|error| internal(&error))?;
  match state_of_session {
    SessionState::Active => {},
    SessionState::Expired => return Err(AuthError::Expired("the session timed out".into())),
    SessionState::Unknown => return Err(AuthError::Expired("the session has ended".into())),
  }
  let user: UserId = claims
    .sub
    .parse()
    .map_err(|_| AuthError::InvalidToken("invalid session".into()))?;
  let account = state
    .database
    .get_local_account(user)
    .await
    .map_err(|error| internal(&error))?
    .ok_or_else(|| AuthError::InvalidToken("unknown account".into()))?;
  let stored = state
    .database
    .get_user(user)
    .await
    .map_err(|error| internal(&error))?
    .ok_or_else(|| AuthError::InvalidToken("unknown account".into()))?;
  Ok(Some(TokenIdentity {
    issuer: LOCAL_ISSUER.to_owned(),
    subject: claims.sub,
    maybe_username: stored.maybe_username,
    display_name: stored.display_name,
    maybe_email: Some(account.login),
    is_email_verified: true,
    role: Role::Guest,
    issued_at: claims.iat,
    maybe_session_id: Some(claims.sid),
    // Checked at sign-in; the policy's session limits were checked just now.
    authentication_methods: vec!["pwd".into(), "otp".into()],
    maybe_acr: None,
    is_exempt_from_session_policy: true,
    maybe_organization: None,
  }))
}

fn local_auth(state: &AppState) -> Result<&LocalAuth, ApiError> {
  state
    .maybe_local
    .as_deref()
    .ok_or_else(|| ApiError::Unavailable("local accounts are not enabled".into()))
}

fn invalid_credentials() -> ApiError {
  AuthError::InvalidToken("invalid credentials".into()).into()
}

/// Counts a failed attempt (which may lock the account) and answers like any wrong credential.
async fn fail(
  state: &AppState,
  account: &LocalAccount,
  now: OffsetDateTime,
) -> Result<LocalLoginResponse, ApiError> {
  let settings = &state.settings.get().local_accounts;
  let lock_until = now + Duration::seconds(i64::try_from(settings.lockout_secs).unwrap_or(i64::MAX / 2));
  state
    .database
    .record_login_failure(
      account.user,
      i32::try_from(settings.max_failed_attempts).unwrap_or(i32::MAX),
      lock_until,
    )
    .await?;
  warn!(user_id = %account.user, "local sign-in failed");
  Err(invalid_credentials())
}

fn check_password_rules(state: &AppState, password: &str) -> Result<(), ApiError> {
  let length = password.chars().count();
  let minimum = super::workspace::effective_min_password_chars(state);
  if length < minimum {
    return Err(ApiError::InvalidRequest(format!(
      "the password needs at least {minimum} characters"
    )));
  }
  if length > MAX_PASSWORD_CHARS {
    return Err(ApiError::InvalidRequest("the password is too long".into()));
  }
  Ok(())
}

async fn hash_in_background(password: String, cost: PasswordCost) -> Result<String, ApiError> {
  tokio::task::spawn_blocking(move || hash_password(&password, &cost, &mut OsRng))
    .await
    .map_err(|error| ApiError::Internal(error.to_string()))?
    .map_err(|error| ApiError::Internal(error.to_string()))
}

async fn spend_time_like_a_check(password: String, cost: PasswordCost) {
  // Ignoring a join error is fine: the result is always `false` and only the time matters.
  let _ = tokio::task::spawn_blocking(move || verify_password_for_unknown_account(&password, &cost)).await;
}

async fn rehash_if_needed(
  state: &AppState,
  local: &LocalAuth,
  account: &LocalAccount,
  password: &str,
) -> Result<(), ApiError> {
  let needs = account
    .maybe_password_hash
    .as_deref()
    .is_some_and(|stored| account::password_needs_rehash(stored, &local.cost));
  if needs {
    let hash = hash_in_background(password.to_owned(), local.cost).await?;
    state.database.set_local_password(account.user, &hash).await?;
  }
  Ok(())
}

/// How long a new session may last: the configured lifetime, shortened by the workspace policy.
fn session_ttl(state: &AppState) -> Duration {
  let configured = Duration::hours(i64::from(state.settings.get().local_accounts.session_ttl_hours));
  let limits = super::session::limits_for(state);
  limits.maybe_max_age.map_or(configured, |max| max.min(configured))
}
