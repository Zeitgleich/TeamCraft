use teamcraft_proto::{Id, Snowflake};

use crate::error::DbError;

/// Typed ID to the `BIGINT` stored in Postgres.
pub(crate) fn to_db<T: Id>(id: T) -> i64 {
  id.snowflake().get()
}

pub(crate) fn to_db_all<T: Id>(ids: &[T]) -> Vec<i64> {
  ids.iter().map(|id| to_db(*id)).collect()
}

/// `BIGINT` from Postgres to a typed ID; fails if the stored value isn't a valid Snowflake.
pub(crate) fn from_db<T: Id>(raw: i64) -> Result<T, DbError> {
  Snowflake::new(raw)
    .map(T::from_snowflake)
    .map_err(|e| DbError::InvalidStoredValue(e.to_string()))
}

pub(crate) fn from_db_opt<T: Id>(raw: Option<i64>) -> Result<Option<T>, DbError> {
  raw.map(from_db).transpose()
}

pub(crate) fn from_db_all<T: Id>(raw: &[i64]) -> Result<Vec<T>, DbError> {
  raw.iter().map(|id| from_db(*id)).collect()
}
