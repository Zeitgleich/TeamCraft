use crate::snowflake::SnowflakeError;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
  #[error("database error: {0}")]
  Query(#[from] sqlx::Error),
  #[error("migration failed: {0}")]
  Migration(#[from] sqlx::migrate::MigrateError),
  #[error("ID generation failed: {0}")]
  IdGeneration(#[from] SnowflakeError),
  #[error("stored value is invalid: {0}")]
  InvalidStoredValue(String),
}

impl DbError {
  /// The database could not take the work right now and the same request is likely to succeed
  /// shortly: no free pooled connection in time, the server out of connection slots
  /// (`too_many_connections`, SQLSTATE 53300), or a transaction rolled back by the server because of
  /// a deadlock (40P01) or serialization failure (40001). Callers answer "try again later", not a
  /// server fault.
  pub fn is_transient(&self) -> bool {
    match self {
      Self::Query(sqlx::Error::PoolTimedOut) => true,
      Self::Query(sqlx::Error::Database(error)) => {
        matches!(error.code().as_deref(), Some("53300" | "40P01" | "40001"))
      },
      _ => false,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_overload_and_rolled_back_transactions_are_transient() {
    assert!(DbError::Query(sqlx::Error::PoolTimedOut).is_transient());
    assert!(!DbError::Query(sqlx::Error::RowNotFound).is_transient());
    assert!(!DbError::InvalidStoredValue("x".into()).is_transient());
  }
}
