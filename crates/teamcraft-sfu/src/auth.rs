//! Join token verification.
//!
//! `teamcraft-server` decides who may join which call and issues short-lived signed join tokens
//! (ARCHITECTURE.md §6.3); the SFU only verifies them. The development stub accepts unsigned tokens.
//!
//! Link probe tickets (#1959) come from the same server with the same key, under their own audience
//! and kind: a ticket never joins a call, and a join token never opens a probe.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use teamcraft_proto::{
  CALL_JOIN_TOKEN_AUDIENCE, CALL_PROBE_TICKET_AUDIENCE, CallId, CallJoinClaims, CallProbeClaims,
  CallTokenKind, ConversationId, DeviceId, UserId,
};
use thiserror::Error;

/// Prefix of development tokens: `dev:<call id>:<user id>:<device id>[:<conversation id>]`.
const DEV_TOKEN_PREFIX: &str = "dev";
/// Prefix of development probe tickets: `dev-probe:<user id>:<device id>:<ticket id>`.
const DEV_PROBE_PREFIX: &str = "dev-probe";
/// The longest a probe ticket may live, whatever the join tokens' limit (#1959).
const MAX_PROBE_TICKET_LIFETIME: Duration = Duration::from_secs(60);

/// Decides whether a token lets its holder into a call, and as whom.
pub trait JoinTokenVerifier: Send + Sync {
  fn verify(&self, token: &str) -> Result<JoinGrant, JoinTokenError>;

  /// Verifies a link probe ticket (#1959). A verifier that knows none refuses it.
  fn verify_probe(&self, ticket: &str) -> Result<ProbeGrant, JoinTokenError> {
    let _ = ticket;
    Err(JoinTokenError::Rejected)
  }
}

/// What a valid token grants: one device of one user may join one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinGrant {
  pub call_id: CallId,
  pub user_id: UserId,
  pub device_id: DeviceId,
  /// The call's conversation, as the server signed it; reported with the roster so a restarted
  /// server takes the call over. `None` from tokens of older servers.
  pub maybe_conversation_id: Option<ConversationId>,
}

/// What a valid probe ticket grants: one device may measure its link, once (#1959).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeGrant {
  pub user_id: UserId,
  pub device_id: DeviceId,
  /// The ticket's id: the SFU takes each once.
  pub ticket_id: String,
  /// When the ticket expires, Unix seconds: its id needs remembering until then.
  pub expires_at_unix_secs: u64,
}

/// Verifies join tokens signed by `teamcraft-server` (EdDSA JWT with [`CallJoinClaims`]), and
/// link probe tickets ([`CallProbeClaims`]).
pub struct JwtJoinTokenVerifier {
  key: DecodingKey,
  validation: Validation,
  probe_validation: Validation,
  leeway: Duration,
  max_lifetime: Duration,
}

/// Settings of [`JwtJoinTokenVerifier`].
#[derive(Debug, Clone)]
pub struct JwtJoinTokenConfig {
  /// Expected `iss`: the `[calls] issuer` of the server.
  pub issuer: String,
  /// Tolerated clock difference between server and SFU.
  pub leeway: Duration,
  /// Tokens valid for longer than this are rejected even if signed (defense in depth against a
  /// misconfigured server handing out long-lived tokens).
  pub max_lifetime: Duration,
}

/// Accepts unsigned `dev:<call id>:<user id>:<device id>` tokens. Never use outside development.
#[derive(Debug, Default)]
pub struct DevTokenVerifier;

/// Rejects every token; used when no verification key is configured.
#[derive(Debug, Default)]
pub struct RejectAllVerifier;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum JoinTokenError {
  #[error("malformed join token")]
  Malformed,
  #[error("join token rejected")]
  Rejected,
  #[error("join token signature, issuer or audience is invalid")]
  InvalidSignature,
  #[error("join token expired")]
  Expired,
  #[error("join token is not valid yet")]
  NotYetValid,
  #[error("join token lifetime exceeds the allowed maximum")]
  LifetimeTooLong,
  #[error("the token is not a link probe ticket")]
  NotAProbeTicket,
}

impl JwtJoinTokenVerifier {
  /// `public_key_pem`: the server's Ed25519 public key (`openssl pkey -pubout`).
  pub fn new(
    public_key_pem: &[u8],
    config: &JwtJoinTokenConfig,
  ) -> Result<Self, jsonwebtoken::errors::Error> {
    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.set_audience(&[CALL_JOIN_TOKEN_AUDIENCE]);
    validation.set_issuer(&[config.issuer.as_str()]);
    validation.set_required_spec_claims(&["exp", "iat", "iss", "aud"]);
    // Times are checked in `verify_at` against an injectable clock.
    validation.validate_exp = false;
    let mut probe_validation = validation.clone();
    probe_validation.set_audience(&[CALL_PROBE_TICKET_AUDIENCE]);
    Ok(Self {
      key: DecodingKey::from_ed_pem(public_key_pem)?,
      validation,
      probe_validation,
      leeway: config.leeway,
      max_lifetime: config.max_lifetime,
    })
  }

  fn verify_at(&self, token: &str, now_unix_secs: u64) -> Result<JoinGrant, JoinTokenError> {
    let claims = jsonwebtoken::decode::<CallJoinClaims>(token, &self.key, &self.validation)
      .map_err(decode_error)?
      .claims;
    check_times(
      claims.iat,
      claims.exp,
      now_unix_secs,
      self.leeway,
      self.max_lifetime,
    )?;
    Ok(JoinGrant {
      call_id: claims.call_id,
      user_id: claims.user_id,
      device_id: claims.device_id,
      maybe_conversation_id: claims.maybe_conversation_id,
    })
  }

  fn verify_probe_at(&self, ticket: &str, now_unix_secs: u64) -> Result<ProbeGrant, JoinTokenError> {
    let claims = jsonwebtoken::decode::<CallProbeClaims>(ticket, &self.key, &self.probe_validation)
      .map_err(decode_error)?
      .claims;
    if claims.kind != CallTokenKind::Probe {
      return Err(JoinTokenError::NotAProbeTicket);
    }
    let max_lifetime = self.max_lifetime.min(MAX_PROBE_TICKET_LIFETIME);
    check_times(claims.iat, claims.exp, now_unix_secs, self.leeway, max_lifetime)?;
    Ok(ProbeGrant {
      user_id: claims.user_id,
      device_id: claims.device_id,
      ticket_id: claims.jti,
      expires_at_unix_secs: claims.exp.saturating_add(self.leeway.as_secs()),
    })
  }
}

impl JoinTokenVerifier for JwtJoinTokenVerifier {
  fn verify(&self, token: &str) -> Result<JoinGrant, JoinTokenError> {
    self.verify_at(token, unix_now_secs())
  }

  fn verify_probe(&self, ticket: &str) -> Result<ProbeGrant, JoinTokenError> {
    self.verify_probe_at(ticket, unix_now_secs())
  }
}

/// Now in Unix seconds. A clock before 1970 makes every token look expired, which fails closed.
fn unix_now_secs() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|elapsed| elapsed.as_secs())
    .unwrap_or_default()
}

/// How a token that didn't decode is refused.
fn decode_error(error: jsonwebtoken::errors::Error) -> JoinTokenError {
  match error.kind() {
    jsonwebtoken::errors::ErrorKind::InvalidToken
    | jsonwebtoken::errors::ErrorKind::Base64(_)
    | jsonwebtoken::errors::ErrorKind::Json(_)
    | jsonwebtoken::errors::ErrorKind::Utf8(_)
    | jsonwebtoken::errors::ErrorKind::MissingRequiredClaim(_) => JoinTokenError::Malformed,
    _ => JoinTokenError::InvalidSignature,
  }
}

/// A token's issue and expiry times against now, with the leeway and the longest lifetime.
fn check_times(
  iat: u64,
  exp: u64,
  now_unix_secs: u64,
  leeway: Duration,
  max_lifetime: Duration,
) -> Result<(), JoinTokenError> {
  let leeway = leeway.as_secs();
  if exp.saturating_add(leeway) < now_unix_secs {
    return Err(JoinTokenError::Expired);
  }
  if iat > now_unix_secs.saturating_add(leeway) {
    return Err(JoinTokenError::NotYetValid);
  }
  if exp.saturating_sub(iat) > max_lifetime.as_secs() {
    return Err(JoinTokenError::LifetimeTooLong);
  }
  Ok(())
}

impl JoinTokenVerifier for DevTokenVerifier {
  fn verify(&self, token: &str) -> Result<JoinGrant, JoinTokenError> {
    let mut parts = token.split(':');
    let (Some(DEV_TOKEN_PREFIX), Some(call), Some(user), Some(device), maybe_conversation, None) = (
      parts.next(),
      parts.next(),
      parts.next(),
      parts.next(),
      parts.next(),
      parts.next(),
    ) else {
      return Err(JoinTokenError::Malformed);
    };
    Ok(JoinGrant {
      call_id: call.parse().map_err(|_| JoinTokenError::Malformed)?,
      user_id: user.parse().map_err(|_| JoinTokenError::Malformed)?,
      device_id: device.parse().map_err(|_| JoinTokenError::Malformed)?,
      maybe_conversation_id: maybe_conversation
        .map(|conversation| conversation.parse().map_err(|_| JoinTokenError::Malformed))
        .transpose()?,
    })
  }

  fn verify_probe(&self, ticket: &str) -> Result<ProbeGrant, JoinTokenError> {
    Self::verify_dev_probe(ticket)
  }
}

impl DevTokenVerifier {
  fn verify_dev_probe(ticket: &str) -> Result<ProbeGrant, JoinTokenError> {
    let mut parts = ticket.split(':');
    let (Some(DEV_PROBE_PREFIX), Some(user), Some(device), Some(ticket_id), None) = (
      parts.next(),
      parts.next(),
      parts.next(),
      parts.next(),
      parts.next(),
    ) else {
      return Err(JoinTokenError::Malformed);
    };
    Ok(ProbeGrant {
      user_id: user.parse().map_err(|_| JoinTokenError::Malformed)?,
      device_id: device.parse().map_err(|_| JoinTokenError::Malformed)?,
      ticket_id: ticket_id.to_owned(),
      expires_at_unix_secs: unix_now_secs() + MAX_PROBE_TICKET_LIFETIME.as_secs(),
    })
  }
}

impl JoinTokenVerifier for RejectAllVerifier {
  fn verify(&self, _token: &str) -> Result<JoinGrant, JoinTokenError> {
    Err(JoinTokenError::Rejected)
  }
}

#[cfg(test)]
mod tests {
  use jsonwebtoken::{EncodingKey, Header};

  use super::*;

  /// Throwaway Ed25519 key pair used only by these tests. It protects nothing.
  const SIGNING_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MC4CAQAwBQYDK2VwBCIEIAxxS2yuWa9SUUAfZhI0772UQ48VXZLz45+L2A5IfcWA
-----END PRIVATE KEY-----
";
  const PUBLIC_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEA8YVjY2Zv9XPhv003tFAgzW7J6gfDn+ICj5xCB2LXMVg=
-----END PUBLIC KEY-----
";
  /// A different key: tokens signed with it must be rejected.
  const OTHER_SIGNING_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MC4CAQAwBQYDK2VwBCIEIIsW2rzm2L3/SlRvzIsu6493OAsvVJHwPVx2J+Nb2LsN
-----END PRIVATE KEY-----
";
  const ISSUER: &str = "https://chat.example.com";
  const NOW: u64 = 1_800_000_000;

  mod jwt {
    use super::*;

    #[test]
    fn valid_token_grants_its_claims() {
      let claims = claims(NOW, NOW + 60);
      assert_eq!(
        verifier().verify_at(&sign(&claims, SIGNING_KEY_PEM), NOW + 10),
        Ok(JoinGrant {
          call_id: claims.call_id,
          user_id: claims.user_id,
          device_id: claims.device_id,
          maybe_conversation_id: claims.maybe_conversation_id,
        })
      );
    }

    #[test]
    fn expiry_respects_leeway() {
      let token = sign(&claims(NOW, NOW + 60), SIGNING_KEY_PEM);
      assert!(verifier().verify_at(&token, NOW + 65).is_ok());
      assert_eq!(
        verifier().verify_at(&token, NOW + 66),
        Err(JoinTokenError::Expired)
      );
    }

    #[test]
    fn future_token_is_rejected() {
      let token = sign(&claims(NOW + 100, NOW + 160), SIGNING_KEY_PEM);
      assert_eq!(
        verifier().verify_at(&token, NOW),
        Err(JoinTokenError::NotYetValid)
      );
    }

    #[test]
    fn long_lived_token_is_rejected() {
      let token = sign(&claims(NOW, NOW + 3600), SIGNING_KEY_PEM);
      assert_eq!(
        verifier().verify_at(&token, NOW),
        Err(JoinTokenError::LifetimeTooLong)
      );
    }

    #[test]
    fn wrong_key_issuer_or_audience_is_rejected() {
      let wrong_key = sign(&claims(NOW, NOW + 60), OTHER_SIGNING_KEY_PEM);
      let mut wrong_issuer = claims(NOW, NOW + 60);
      wrong_issuer.iss = "https://evil.example.com".into();
      let mut wrong_audience = claims(NOW, NOW + 60);
      wrong_audience.aud = "teamcraft-server".into();
      for token in [
        wrong_key,
        sign(&wrong_issuer, SIGNING_KEY_PEM),
        sign(&wrong_audience, SIGNING_KEY_PEM),
      ] {
        assert_eq!(
          verifier().verify_at(&token, NOW),
          Err(JoinTokenError::InvalidSignature)
        );
      }
    }

    #[test]
    fn tampered_or_garbage_tokens_are_rejected() {
      let token = sign(&claims(NOW, NOW + 60), SIGNING_KEY_PEM);
      let mut parts: Vec<&str> = token.split('.').collect();
      // Same signature, payload claiming another device.
      let mut other_device = claims(NOW, NOW + 60);
      other_device.device_id = "43".parse().unwrap();
      let forged_payload = sign(&other_device, SIGNING_KEY_PEM);
      parts[1] = forged_payload.split('.').nth(1).unwrap();
      assert_eq!(
        verifier().verify_at(&parts.join("."), NOW),
        Err(JoinTokenError::InvalidSignature)
      );
      for garbage in ["", "abc", "a.b.c", "dev:7:3:42"] {
        assert!(verifier().verify_at(garbage, NOW).is_err(), "{garbage}");
      }
    }

    #[test]
    fn unsigned_token_is_rejected() {
      // alg "none" must never be accepted.
      let header = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0";
      let payload = sign(&claims(NOW, NOW + 60), SIGNING_KEY_PEM);
      let body = payload.split('.').nth(1).unwrap();
      assert!(verifier().verify_at(&format!("{header}.{body}."), NOW).is_err());
    }

    /// #1959: a probe ticket opens a probe, and nothing else; a join token never opens one.
    mod probe_tickets {
      use super::*;

      #[test]
      fn a_valid_ticket_grants_its_device_once_named() {
        let ticket = sign_probe(&probe_claims(NOW, NOW + 30), SIGNING_KEY_PEM);
        let grant = verifier().verify_probe_at(&ticket, NOW + 1).unwrap();
        assert_eq!(grant.device_id, "42".parse().unwrap());
        assert_eq!(grant.ticket_id, "t1");
        assert_eq!(grant.expires_at_unix_secs, NOW + 35, "with the leeway");
      }

      #[test]
      fn a_probe_ticket_joins_nothing_and_a_join_token_probes_nothing() {
        let ticket = sign_probe(&probe_claims(NOW, NOW + 30), SIGNING_KEY_PEM);
        assert!(
          verifier().verify_at(&ticket, NOW).is_err(),
          "its audience and claims are not the join tokens'"
        );
        let join = sign(&claims(NOW, NOW + 60), SIGNING_KEY_PEM);
        assert!(verifier().verify_probe_at(&join, NOW).is_err());
        // A join token's claims under the probe audience still lack a ticket's kind and id.
        let mut retargeted = claims(NOW, NOW + 60);
        retargeted.aud = CALL_PROBE_TICKET_AUDIENCE.into();
        assert_eq!(
          verifier().verify_probe_at(&sign(&retargeted, SIGNING_KEY_PEM), NOW),
          Err(JoinTokenError::Malformed)
        );
      }

      #[test]
      fn another_kind_a_long_life_an_old_ticket_or_another_key_is_refused() {
        let mut other_kind = probe_claims(NOW, NOW + 30);
        other_kind.kind = CallTokenKind::Unknown;
        assert!(
          verifier()
            .verify_probe_at(&sign_probe(&other_kind, SIGNING_KEY_PEM), NOW)
            .is_err()
        );
        assert_eq!(
          verifier().verify_probe_at(&sign_probe(&probe_claims(NOW, NOW + 61), SIGNING_KEY_PEM), NOW),
          Err(JoinTokenError::LifetimeTooLong),
          "at most 60 s, whatever join tokens may"
        );
        assert_eq!(
          verifier().verify_probe_at(
            &sign_probe(&probe_claims(NOW, NOW + 30), SIGNING_KEY_PEM),
            NOW + 36
          ),
          Err(JoinTokenError::Expired)
        );
        assert_eq!(
          verifier().verify_probe_at(
            &sign_probe(&probe_claims(NOW, NOW + 30), OTHER_SIGNING_KEY_PEM),
            NOW
          ),
          Err(JoinTokenError::InvalidSignature)
        );
      }

      fn probe_claims(iat: u64, exp: u64) -> CallProbeClaims {
        CallProbeClaims {
          iss: ISSUER.into(),
          aud: CALL_PROBE_TICKET_AUDIENCE.into(),
          iat,
          exp,
          jti: "t1".into(),
          kind: CallTokenKind::Probe,
          user_id: "3".parse().unwrap(),
          device_id: "42".parse().unwrap(),
        }
      }

      fn sign_probe(claims: &CallProbeClaims, private_key_pem: &str) -> String {
        let key = EncodingKey::from_ed_pem(private_key_pem.as_bytes()).unwrap();
        jsonwebtoken::encode(&Header::new(Algorithm::EdDSA), claims, &key).unwrap()
      }
    }

    fn verifier() -> JwtJoinTokenVerifier {
      let config = JwtJoinTokenConfig {
        issuer: ISSUER.into(),
        leeway: Duration::from_secs(5),
        max_lifetime: Duration::from_secs(300),
      };
      JwtJoinTokenVerifier::new(PUBLIC_KEY_PEM.as_bytes(), &config).unwrap()
    }

    fn claims(iat: u64, exp: u64) -> CallJoinClaims {
      CallJoinClaims {
        iss: ISSUER.into(),
        aud: CALL_JOIN_TOKEN_AUDIENCE.into(),
        iat,
        exp,
        call_id: "7".parse().unwrap(),
        user_id: "3".parse().unwrap(),
        device_id: "42".parse().unwrap(),
        maybe_conversation_id: Some("9".parse().unwrap()),
      }
    }

    fn sign(claims: &CallJoinClaims, private_key_pem: &str) -> String {
      let key = EncodingKey::from_ed_pem(private_key_pem.as_bytes()).unwrap();
      jsonwebtoken::encode(&Header::new(Algorithm::EdDSA), claims, &key).unwrap()
    }
  }

  #[test]
  fn dev_probe_tickets_are_parsed_and_others_refused() {
    let grant = DevTokenVerifier.verify_probe("dev-probe:3:42:t9").unwrap();
    assert_eq!(
      (grant.device_id, grant.ticket_id.as_str()),
      ("42".parse().unwrap(), "t9")
    );
    assert!(
      DevTokenVerifier.verify_probe("dev:7:3:42").is_err(),
      "a join token"
    );
    assert!(
      DevTokenVerifier.verify("dev-probe:3:42:t9").is_err(),
      "joins nothing"
    );
    assert_eq!(
      RejectAllVerifier.verify_probe("dev-probe:3:42:t9"),
      Err(JoinTokenError::Rejected)
    );
  }

  #[test]
  fn dev_token_is_parsed() {
    assert_eq!(
      DevTokenVerifier.verify("dev:7:3:42"),
      Ok(JoinGrant {
        call_id: "7".parse().unwrap(),
        user_id: "3".parse().unwrap(),
        device_id: "42".parse().unwrap(),
        maybe_conversation_id: None,
      })
    );
    assert_eq!(
      DevTokenVerifier
        .verify("dev:7:3:42:9")
        .map(|grant| grant.maybe_conversation_id),
      Ok(Some("9".parse().unwrap()))
    );
  }

  #[test]
  fn malformed_dev_tokens() {
    for token in [
      "",
      "dev",
      "dev:7:3",
      "dev:7:3:42:9:1",
      "dev:7:3:42:x",
      "prod:7:3:42",
      "dev:x:3:42",
      "dev:7:3:-1",
    ] {
      assert_eq!(
        DevTokenVerifier.verify(token),
        Err(JoinTokenError::Malformed),
        "{token}"
      );
    }
  }

  #[test]
  fn reject_all() {
    assert_eq!(
      RejectAllVerifier.verify("dev:7:3:42"),
      Err(JoinTokenError::Rejected)
    );
  }
}
