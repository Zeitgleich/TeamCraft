use super::dpop::DpopRefusal;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
  #[error("missing bearer token")]
  MissingToken,
  #[error("invalid token: {0}")]
  InvalidToken(String),
  /// A token or session that was valid and ran out: a client that slept or stayed offline, not
  /// someone guessing. Same answer on the wire as [`Self::InvalidToken`].
  #[error("invalid token: {0}")]
  Expired(String),
  /// The session was ended before its token ran out: signed out at the identity provider
  /// (back-channel logout), on another device, or by a password change. Clients say so.
  #[error("the session has ended")]
  SessionEnded,
  /// The session policy's idle timeout or longest session ran out (admin console, security).
  #[error("the session timed out; sign in again")]
  SessionTimedOut,
  #[error("the user has none of the TeamCraft roles")]
  NoTeamcraftRole,
  #[error("the account is deactivated")]
  AccountDeactivated,
  /// The token comes from a sign-in before the account was deleted; signing in again creates a
  /// new account.
  #[error("the account was deleted; sign in again")]
  AccountDeleted,
  #[error("signing keys unavailable: {0}")]
  KeysUnavailable(String),
  /// The token is fine as a token, but not without a valid DPoP proof (RFC 9449).
  #[error(transparent)]
  Dpop(#[from] DpopRefusal),
  /// This account has as many DPoP proofs in the replay cache as one may: 429, for it alone.
  #[error("too many requests with DPoP proofs from this account")]
  DpopUserLimit,
  /// The DPoP replay cache is full and nothing in it has expired. Logged at most once per interval.
  #[error("the DPoP replay cache is full")]
  DpopReplayCacheFull,
}

impl From<crate::dependencies::CircuitOpen> for AuthError {
  fn from(open: crate::dependencies::CircuitOpen) -> Self {
    Self::KeysUnavailable(open.to_string())
  }
}

impl AuthError {
  /// A proof that was refused: counts as a failed authentication everywhere. The nonce challenge
  /// doesn't, since everybody meets it after a restart (`DpopNonceChallenge`).
  pub fn is_refused_dpop_proof(&self) -> bool {
    matches!(self, Self::Dpop(refusal) if *refusal != DpopRefusal::UseNonce)
  }

  /// What a token guesser produces (missing, forged or malformed tokens). Rate limits count these;
  /// expired tokens and refused roles say nothing about guessing.
  pub fn is_guess(&self) -> bool {
    matches!(self, Self::MissingToken | Self::InvalidToken(_))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_missing_and_forged_tokens_count_as_guesses() {
    assert!(AuthError::MissingToken.is_guess());
    assert!(AuthError::InvalidToken("bad signature".into()).is_guess());
    assert!(!AuthError::Expired("token expired".into()).is_guess());
    assert!(!AuthError::SessionEnded.is_guess());
    assert!(!AuthError::SessionTimedOut.is_guess());
    assert!(!AuthError::NoTeamcraftRole.is_guess());
    assert!(!AuthError::AccountDeactivated.is_guess());
    assert!(!AuthError::KeysUnavailable("down".into()).is_guess());
    assert!(!AuthError::Dpop(DpopRefusal::UseNonce).is_guess());
    assert!(!AuthError::Dpop(DpopRefusal::Replayed).is_guess());
    assert!(AuthError::Dpop(DpopRefusal::Replayed).is_refused_dpop_proof());
    assert!(AuthError::Dpop(DpopRefusal::ProofMissing).is_refused_dpop_proof());
    assert!(!AuthError::Dpop(DpopRefusal::UseNonce).is_refused_dpop_proof());
    assert!(!AuthError::DpopUserLimit.is_refused_dpop_proof());
  }
}
