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
  #[error("the user has none of the TeamCraft roles")]
  NoTeamcraftRole,
  #[error("the account is deactivated")]
  AccountDeactivated,
  #[error("signing keys unavailable: {0}")]
  KeysUnavailable(String),
}

impl AuthError {
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
    assert!(!AuthError::NoTeamcraftRole.is_guess());
    assert!(!AuthError::AccountDeactivated.is_guess());
    assert!(!AuthError::KeysUnavailable("down".into()).is_guess());
  }
}
