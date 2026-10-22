//! Signed tokens for meeting guests without an account (ARCHITECTURE.md §6.4, §3).
//!
//! Two kinds, both HS256 with the server's `guest_token_secret`:
//!
//! - the **invite token** in a personal join link names one `meeting_invites` row. It is one-time:
//!   redeeming it marks the row, and the database decides whether it is still good;
//! - the **session token** is what a redeemed invite is exchanged for. It authenticates the guest's
//!   API and gateway calls like an identity provider token and is tied to one meeting.

use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use teamcraft_proto::{MeetingId, MeetingInviteId};

/// `iss` of both token kinds, and the identity provider name guest accounts are stored under.
pub const GUEST_ISSUER: &str = "teamcraft-meeting-guest";
const INVITE_AUDIENCE: &str = "teamcraft-meeting-invite";
const SESSION_AUDIENCE: &str = "teamcraft-meeting-session";
/// Shorter secrets are refused at startup.
pub const MIN_SECRET_BYTES: usize = 32;

#[derive(Debug, Serialize, Deserialize)]
struct InviteClaims {
  iss: String,
  aud: String,
  sub: String,
  meeting: String,
}

/// What a guest session token says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionClaims {
  pub iss: String,
  pub aud: String,
  /// The redeemed invite; also the guest account's subject.
  pub sub: String,
  pub meeting: String,
  pub name: String,
  pub iat: i64,
  pub exp: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum GuestTokenError {
  #[error("the guest token secret must have at least {MIN_SECRET_BYTES} bytes")]
  WeakSecret,
  #[error("the token is invalid")]
  Invalid,
  #[error("signing failed: {0}")]
  Signing(String),
}

pub struct GuestTokens {
  encoding: EncodingKey,
  decoding: DecodingKey,
}

impl GuestTokens {
  pub fn new(secret: &SecretString) -> Result<Self, GuestTokenError> {
    let bytes = secret.expose_secret().as_bytes();
    if bytes.len() < MIN_SECRET_BYTES {
      return Err(GuestTokenError::WeakSecret);
    }
    Ok(Self {
      encoding: EncodingKey::from_secret(bytes),
      decoding: DecodingKey::from_secret(bytes),
    })
  }

  /// The token for a personal join link.
  pub fn sign_invite(&self, invite: MeetingInviteId, meeting: MeetingId) -> Result<String, GuestTokenError> {
    let claims = InviteClaims {
      iss: GUEST_ISSUER.into(),
      aud: INVITE_AUDIENCE.into(),
      sub: invite.to_string(),
      meeting: meeting.to_string(),
    };
    self.sign(&claims)
  }

  /// The invite and meeting a link token names, if the signature is ours.
  pub fn verify_invite(&self, token: &str) -> Result<(MeetingInviteId, MeetingId), GuestTokenError> {
    let claims: InviteClaims = self.verify(token, INVITE_AUDIENCE, false)?;
    let invite = claims.sub.parse().map_err(|_| GuestTokenError::Invalid)?;
    let meeting = claims.meeting.parse().map_err(|_| GuestTokenError::Invalid)?;
    Ok((invite, meeting))
  }

  /// A session token valid until `expires_at` (unix seconds).
  pub fn sign_session(
    &self,
    invite: MeetingInviteId,
    meeting: MeetingId,
    display_name: &str,
    now: i64,
    expires_at: i64,
  ) -> Result<String, GuestTokenError> {
    self.sign(&SessionClaims {
      iss: GUEST_ISSUER.into(),
      aud: SESSION_AUDIENCE.into(),
      sub: invite.to_string(),
      meeting: meeting.to_string(),
      name: display_name.to_owned(),
      iat: now,
      exp: expires_at,
    })
  }

  /// Verifies signature, audience and expiry of a session token.
  pub fn verify_session(&self, token: &str) -> Result<SessionClaims, GuestTokenError> {
    self.verify(token, SESSION_AUDIENCE, true)
  }

  fn sign<T: Serialize>(&self, claims: &T) -> Result<String, GuestTokenError> {
    jsonwebtoken::encode(&Header::new(Algorithm::HS256), claims, &self.encoding)
      .map_err(|error| GuestTokenError::Signing(error.to_string()))
  }

  fn verify<T: for<'de> Deserialize<'de>>(
    &self,
    token: &str,
    audience: &str,
    has_expiry: bool,
  ) -> Result<T, GuestTokenError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_audience(&[audience]);
    validation.set_issuer(&[GUEST_ISSUER]);
    validation.leeway = 0;
    validation.validate_exp = has_expiry;
    validation.required_spec_claims = if has_expiry {
      ["exp", "iss", "aud", "sub"].map(String::from).into()
    } else {
      ["iss", "aud", "sub"].map(String::from).into()
    };
    jsonwebtoken::decode::<T>(token, &self.decoding, &validation)
      .map(|data| data.claims)
      .map_err(|_| GuestTokenError::Invalid)
  }
}

/// Whether the token looks like one of ours (HS256), without verifying anything: lets the auth layer
/// route it to [`GuestTokens`] instead of the identity provider's key set.
pub fn is_guest_token_header(token: &str) -> bool {
  jsonwebtoken::decode_header(token)
    .map(|header| header.alg == Algorithm::HS256)
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  const SECRET: &str = "0123456789abcdef0123456789abcdef";

  mod invites {
    use super::*;

    #[test]
    fn an_invite_token_round_trips() {
      let tokens = tokens(SECRET);
      let token = tokens.sign_invite(id(7), id(9)).unwrap();
      assert_eq!(tokens.verify_invite(&token).unwrap(), (id(7), id(9)));
    }

    #[test]
    fn tokens_of_other_keys_other_kinds_and_garbage_are_rejected() {
      let ours = tokens(SECRET);
      let foreign = tokens("fedcba9876543210fedcba9876543210");
      let invite = ours.sign_invite(id(7), id(9)).unwrap();
      let session = ours
        .sign_session(id(7), id(9), "Gus", 1_000, i64::MAX / 2)
        .unwrap();
      assert!(foreign.verify_invite(&invite).is_err());
      assert!(
        ours.verify_invite(&session).is_err(),
        "a session is not an invite"
      );
      assert!(
        ours.verify_session(&invite).is_err(),
        "an invite is not a session"
      );
      assert!(ours.verify_invite("not.a.token").is_err());
    }

    #[test]
    fn weak_secrets_are_refused() {
      assert!(matches!(
        GuestTokens::new(&"short".to_owned().into()),
        Err(GuestTokenError::WeakSecret)
      ));
    }
  }

  mod sessions {
    use super::*;

    #[test]
    fn a_session_carries_meeting_and_name_until_it_expires() {
      let tokens = tokens(SECRET);
      let now = i64::try_from(
        std::time::SystemTime::now()
          .duration_since(std::time::UNIX_EPOCH)
          .unwrap()
          .as_secs(),
      )
      .unwrap();
      let valid = tokens.sign_session(id(7), id(9), "Gus", now, now + 600).unwrap();
      let claims = tokens.verify_session(&valid).unwrap();
      assert_eq!((claims.meeting.as_str(), claims.name.as_str()), ("9", "Gus"));

      let expired = tokens
        .sign_session(id(7), id(9), "Gus", now - 600, now - 10)
        .unwrap();
      assert!(tokens.verify_session(&expired).is_err());
    }

    #[test]
    fn only_hs256_tokens_are_routed_to_the_guest_verifier() {
      let tokens = tokens(SECRET);
      assert!(is_guest_token_header(&tokens.sign_invite(id(1), id(2)).unwrap()));
      // An RS256 header, as the identity provider's tokens have.
      assert!(!is_guest_token_header(
        "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.e30.sig"
      ));
      assert!(!is_guest_token_header("garbage"));
    }
  }

  fn tokens(secret: &str) -> GuestTokens {
    GuestTokens::new(&secret.to_owned().into()).unwrap()
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
