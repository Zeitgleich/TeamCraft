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
