//! Postgres access for TeamCraft: the only crate that talks SQL (code-style.md).
//!
//! Queries use compile-time checked `sqlx::query!` macros; the offline data in `.sqlx/` is committed.
//! Regenerate it with `cargo sqlx prepare --workspace` against a migrated dev database.

mod database;
mod error;
mod snowflake;
mod user;

pub use database::{Database, DatabaseOptions};
pub use error::DbError;
pub use snowflake::{Clock, SnowflakeError, SnowflakeGenerator, SystemClock};
pub use user::{OidcIdentity, User, UserKind};
