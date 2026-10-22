//! Calls: which call a conversation has, and who may join it (ARCHITECTURE.md §6.3).
//!
//! The server decides access and hands out short-lived join tokens (EdDSA-signed JWTs with
//! [`CallJoinClaims`]); the SFU verifies them with the server's public key and never asks the
//! server. Active calls are tracked in memory: a conversation keeps its call id while tokens are
//! issued, and gets a new one after `call_idle_secs` without any.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use teamcraft_proto::{
  CALL_JOIN_TOKEN_AUDIENCE, CallId, CallJoinClaims, CallJoinTokenResponse, ConversationId,
  CreateCallJoinTokenRequest, DeviceId, UnixMillis, UserId,
};
use thiserror::Error;

use super::access::load_conversation;
use crate::auth::AuthenticatedUser;
use crate::config::CallsConfig;
use crate::error::ApiError;
use crate::state::AppState;

/// Signs join tokens and tracks the active call of each conversation.
pub struct CallService {
  signing_key: EncodingKey,
  issuer: String,
  sfu_url: String,
  token_ttl: Duration,
  call_idle_timeout: Duration,
  active: Mutex<HashMap<ConversationId, ActiveCall>>,
}

#[derive(Debug, Clone, Copy)]
struct ActiveCall {
  call_id: CallId,
  last_token_at: Instant,
}

#[derive(Debug, Error)]
pub enum CallsSetupError {
  #[error("reading the join token signing key {path}: {source}")]
  ReadKey { path: String, source: std::io::Error },
  #[error("the join token signing key is not an Ed25519 PEM private key: {0}")]
  InvalidKey(#[from] jsonwebtoken::errors::Error),
}

impl CallService {
  pub fn from_config(config: &CallsConfig) -> Result<Self, CallsSetupError> {
    let pem = std::fs::read(&config.signing_key_path).map_err(|source| CallsSetupError::ReadKey {
      path: config.signing_key_path.display().to_string(),
      source,
    })?;
    Self::new(config, &pem)
  }

  pub fn new(config: &CallsConfig, signing_key_pem: &[u8]) -> Result<Self, CallsSetupError> {
    Ok(Self {
      signing_key: EncodingKey::from_ed_pem(signing_key_pem)?,
      issuer: config.issuer.clone(),
      sfu_url: config.sfu_url.clone(),
      token_ttl: Duration::from_secs(config.join_token_ttl_secs),
      call_idle_timeout: Duration::from_secs(config.call_idle_secs),
      active: Mutex::new(HashMap::new()),
    })
  }

  /// The conversation's current call, or a new one from `new_call_id`.
  fn call_for(
    &self,
    conversation_id: ConversationId,
    now: Instant,
    new_call_id: impl FnOnce() -> Result<CallId, ApiError>,
  ) -> Result<CallId, ApiError> {
    // No invariant spans a panic point, so a poisoned map is still consistent.
    let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
    active.retain(|_, call| now.saturating_duration_since(call.last_token_at) < self.call_idle_timeout);
    let call_id = match active.get(&conversation_id) {
      Some(call) => call.call_id,
      None => new_call_id()?,
    };
    active.insert(
      conversation_id,
      ActiveCall {
        call_id,
        last_token_at: now,
      },
    );
    Ok(call_id)
  }

  /// Claims for one device, valid from `now_unix_secs` for the configured lifetime.
  fn claims(
    &self,
    call_id: CallId,
    user_id: UserId,
    device_id: DeviceId,
    now_unix_secs: u64,
  ) -> CallJoinClaims {
    CallJoinClaims {
      iss: self.issuer.clone(),
      aud: CALL_JOIN_TOKEN_AUDIENCE.to_owned(),
      iat: now_unix_secs,
      exp: now_unix_secs + self.token_ttl.as_secs(),
      call_id,
      user_id,
      device_id,
    }
  }

  fn sign(&self, claims: &CallJoinClaims) -> Result<String, ApiError> {
    jsonwebtoken::encode(&Header::new(Algorithm::EdDSA), claims, &self.signing_key)
      .map_err(|error| ApiError::Internal(format!("signing a join token failed: {error}")))
  }
}

/// Issues a join token for the conversation's call. Only members may join, with one of their own
/// active devices.
pub async fn create_join_token(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation_id: ConversationId,
  request: CreateCallJoinTokenRequest,
) -> Result<CallJoinTokenResponse, ApiError> {
  let calls = state
    .maybe_calls
    .as_ref()
    .ok_or_else(|| ApiError::Unavailable("calls are not configured".into()))?;
  let access = load_conversation(state, caller, conversation_id).await?;
  if !access.is_member() {
    return Err(ApiError::Forbidden("only members can join the call".into()));
  }
  if access.view.conversation.is_archived {
    return Err(ApiError::Forbidden("the conversation is archived".into()));
  }
  if !state
    .database
    .is_active_device_of(caller.user.id, request.device_id)
    .await?
  {
    return Err(ApiError::device_not_found());
  }

  let call_id = calls.call_for(conversation_id, Instant::now(), || state.next_id())?;
  let claims = calls.claims(call_id, caller.user.id, request.device_id, unix_now_secs()?);
  Ok(CallJoinTokenResponse {
    call_id,
    token: calls.sign(&claims)?,
    sfu_url: calls.sfu_url.clone(),
    expires_at: UnixMillis(i64::try_from(claims.exp).unwrap_or(i64::MAX).saturating_mul(1000)),
  })
}

fn unix_now_secs() -> Result<u64, ApiError> {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|elapsed| elapsed.as_secs())
    .map_err(|_| ApiError::Internal("system clock is before 1970".into()))
}

#[cfg(test)]
mod tests {
  use std::path::PathBuf;

  use jsonwebtoken::{DecodingKey, Validation};
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  /// Throwaway Ed25519 key pair used only by these tests. It protects nothing.
  const TEST_SIGNING_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MC4CAQAwBQYDK2VwBCIEIAxxS2yuWa9SUUAfZhI0772UQ48VXZLz45+L2A5IfcWA
-----END PRIVATE KEY-----
";
  const TEST_PUBLIC_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEA8YVjY2Zv9XPhv003tFAgzW7J6gfDn+ICj5xCB2LXMVg=
-----END PUBLIC KEY-----
";
  const NOW_UNIX_SECS: u64 = 1_800_000_000;

  mod tokens {
    use super::*;

    #[test]
    fn signed_token_verifies_with_the_public_key() {
      let service = service();
      let claims = service.claims(id(7), id(4), id(5), NOW_UNIX_SECS);
      let token = service.sign(&claims).unwrap();

      let mut validation = Validation::new(Algorithm::EdDSA);
      validation.set_audience(&[CALL_JOIN_TOKEN_AUDIENCE]);
      validation.set_issuer(&["https://chat.example.com"]);
      validation.validate_exp = false;
      let decoded = jsonwebtoken::decode::<CallJoinClaims>(
        &token,
        &DecodingKey::from_ed_pem(TEST_PUBLIC_KEY_PEM.as_bytes()).unwrap(),
        &validation,
      )
      .unwrap();
      assert_eq!(decoded.claims, claims);
      assert_eq!(decoded.claims.exp - decoded.claims.iat, 60);
    }

    #[test]
    fn invalid_signing_key_is_rejected() {
      assert!(matches!(
        CallService::new(&config(), b"not a key"),
        Err(CallsSetupError::InvalidKey(_))
      ));
    }
  }

  mod active_calls {
    use super::*;

    #[test]
    fn conversation_keeps_its_call_while_tokens_are_issued() {
      let service = service();
      let start = Instant::now();
      let first = service.call_for(id(1), start, || Ok(id(100))).unwrap();
      let later = service
        .call_for(id(1), start + Duration::from_secs(3000), || Ok(id(101)))
        .unwrap();
      assert_eq!((first, later), (id(100), id(100)));
    }

    #[test]
    fn idle_call_is_replaced() {
      let service = service();
      let start = Instant::now();
      service.call_for(id(1), start, || Ok(id(100))).unwrap();
      let after_idle = service
        .call_for(id(1), start + Duration::from_secs(3600), || Ok(id(101)))
        .unwrap();
      assert_eq!(after_idle, id(101));
    }

    #[test]
    fn conversations_have_separate_calls() {
      let service = service();
      let now = Instant::now();
      let a = service.call_for(id(1), now, || Ok(id(100))).unwrap();
      let b = service.call_for(id(2), now, || Ok(id(200))).unwrap();
      assert_ne!(a, b);
    }
  }

  fn service() -> CallService {
    CallService::new(&config(), TEST_SIGNING_KEY_PEM.as_bytes()).unwrap()
  }

  fn config() -> CallsConfig {
    CallsConfig {
      sfu_url: "https://sfu.example.com:4433/call".into(),
      issuer: "https://chat.example.com".into(),
      signing_key_path: PathBuf::from("unused.pem"),
      join_token_ttl_secs: 60,
      call_idle_secs: 3600,
    }
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
