//! Postgres access for TeamCraft: the only crate that talks SQL (code-style.md).
//!
//! Queries use compile-time checked `sqlx::query!` macros; the offline data in `.sqlx/` is committed.
//! Regenerate it with `cargo sqlx prepare --workspace` against a migrated dev database.

mod conversation;
mod database;
mod device;
mod error;
mod ids;
mod invite;
mod message;
mod snowflake;
mod space;
#[cfg(feature = "integration")]
pub mod testing;
mod user;

pub use conversation::{
  Conversation, ConversationChanges, ConversationFilter, ConversationMember, ConversationView, Membership,
  MembershipChanges, NewConversation, participants_key,
};
pub use database::{Database, DatabaseOptions};
pub use device::{Device, NewDevice};
pub use error::DbError;
pub use invite::{AppliedInvite, Invite, NewInvite};
pub use message::{HistoryQuery, NewMessage, StoreOutcome, StoredMessage};
pub use snowflake::{Clock, SnowflakeError, SnowflakeGenerator, SystemClock};
pub use space::{
  Category, NewSpace, NewSpaceMember, Space, SpaceChanges, SpaceListing, SpaceMember, SpaceView,
};
pub use user::{OidcIdentity, PresencePreference, User, UserSearch};
