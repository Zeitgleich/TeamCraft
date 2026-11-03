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
  /// shortly: an [outage](Self::is_outage), or a transaction rolled back by the server because of a
  /// deadlock (40P01) or serialization failure (40001). Callers answer "try again later", not a
  /// server fault.
  pub fn is_transient(&self) -> bool {
    self.is_outage()
      || matches!(
        self,
        Self::Query(sqlx::Error::Database(error))
          if matches!(error.code().as_deref(), Some("40P01" | "40001"))
      )
  }

  /// Postgres itself is in trouble, whatever the request was: no free pooled connection in time, the
  /// connection refused or reset (a restart, a failover), a connection failure reported by the
  /// server (SQLSTATE class 08), the server out of resources such as connection slots (class 53),
  /// shutting down (57P01, 57P02) or still starting (57P03).
  pub fn is_outage(&self) -> bool {
    match self {
      Self::Query(sqlx::Error::PoolTimedOut | sqlx::Error::Io(_)) => true,
      Self::Query(sqlx::Error::Database(error)) => error.code().is_some_and(|code| {
        code.starts_with("08")
          || code.starts_with("53")
          || matches!(code.as_ref(), "57P01" | "57P02" | "57P03")
      }),
      _ => false,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_overload_outages_and_rolled_back_transactions_are_transient() {
    assert!(DbError::Query(sqlx::Error::PoolTimedOut).is_transient());
    let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
    assert!(DbError::Query(sqlx::Error::Io(refused)).is_transient());
    assert!(!DbError::Query(sqlx::Error::RowNotFound).is_transient());
    assert!(!DbError::InvalidStoredValue("x".into()).is_transient());
  }

  #[test]
  fn an_outage_is_a_failure_of_postgres_not_of_one_query() {
    assert!(DbError::Query(sqlx::Error::PoolTimedOut).is_outage());
    let reset = std::io::Error::from(std::io::ErrorKind::ConnectionReset);
    assert!(DbError::Query(sqlx::Error::Io(reset)).is_outage());
    assert!(!DbError::Query(sqlx::Error::RowNotFound).is_outage());
    assert!(!DbError::InvalidStoredValue("x".into()).is_outage());
  }
}
