//! Join token verification.
//!
//! In production `teamcraft-server` issues short-lived signed join tokens and tells the SFU the
//! call roster (ARCHITECTURE.md §6.3). The spike only has the trait and a development stub.

use thiserror::Error;

use crate::ids::{CallId, DeviceId};

/// Prefix of development tokens: `dev:<call id>:<participant id>`.
const DEV_TOKEN_PREFIX: &str = "dev";

/// Decides whether a token lets its holder into a call, and as whom.
pub trait JoinTokenVerifier: Send + Sync {
  fn verify(&self, token: &str) -> Result<JoinGrant, JoinTokenError>;
}

/// What a valid token grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinGrant {
  pub call_id: CallId,
  pub participant_id: DeviceId,
}

/// Accepts unsigned `dev:<call id>:<participant id>` tokens. Never use outside development.
#[derive(Debug, Default)]
pub struct DevTokenVerifier;

/// Rejects every token; the default until real verification exists.
#[derive(Debug, Default)]
pub struct RejectAllVerifier;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum JoinTokenError {
  #[error("malformed join token")]
  Malformed,
  #[error("join token rejected")]
  Rejected,
}

impl JoinTokenVerifier for DevTokenVerifier {
  fn verify(&self, token: &str) -> Result<JoinGrant, JoinTokenError> {
    let mut parts = token.split(':');
    let (Some(DEV_TOKEN_PREFIX), Some(call), Some(participant), None) =
      (parts.next(), parts.next(), parts.next(), parts.next())
    else {
      return Err(JoinTokenError::Malformed);
    };
    let call_id = CallId(call.parse().map_err(|_| JoinTokenError::Malformed)?);
    let participant_id: DeviceId = participant.parse().map_err(|_| JoinTokenError::Malformed)?;
    Ok(JoinGrant {
      call_id,
      participant_id,
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
  use super::*;

  #[test]
  fn dev_token_is_parsed() {
    assert_eq!(
      DevTokenVerifier.verify("dev:7:42"),
      Ok(JoinGrant {
        call_id: CallId(7),
        participant_id: device(42)
      })
    );
  }

  #[test]
  fn malformed_dev_tokens() {
    for token in [
      "",
      "dev",
      "dev:7",
      "dev:7:42:1",
      "prod:7:42",
      "dev:x:42",
      "dev:7:-1",
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
      RejectAllVerifier.verify("dev:7:42"),
      Err(JoinTokenError::Rejected)
    );
  }

  fn device(raw: u64) -> DeviceId {
    crate::ids::device_id_from_dave(teamcraft_media::dave::DaveParticipantId(raw)).unwrap()
  }
}
