#[derive(Debug, thiserror::Error)]
pub enum AuthError {
  #[error("missing bearer token")]
  MissingToken,
  #[error("invalid token: {0}")]
  InvalidToken(String),
  #[error("the user has none of the TeamCraft roles")]
  NoTeamcraftRole,
  #[error("the account is deactivated")]
  AccountDeactivated,
  #[error("signing keys unavailable: {0}")]
  KeysUnavailable(String),
}
