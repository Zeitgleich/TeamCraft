//! Postgres access for TeamCraft: the only crate that talks SQL (code-style.md).
//!
//! Queries use compile-time checked `sqlx::query!` macros; the offline data in `.sqlx/` is committed.
//! Regenerate it with `cargo sqlx prepare --workspace` against a migrated dev database.

mod attachment;
mod conversation;
mod database;
mod device;
mod error;
mod identity;
mod ids;
mod invite;
mod key_bag;
mod key_package;
mod message;
mod mls;
mod snowflake;
mod space;
#[cfg(feature = "integration")]
pub mod testing;
mod user;

pub use attachment::{Attachment, NewAttachment};
pub use conversation::{
  Conversation, ConversationChanges, ConversationFilter, ConversationMember, ConversationView, Membership,
  MembershipChanges, NewConversation, participants_key,
};
pub use database::{Database, DatabaseOptions};
pub use device::{Device, NewDevice};
pub use error::DbError;
pub use identity::{IdentityKey, IdentityKeyOutcome, NewIdentityKey};
pub use invite::{AppliedInvite, Invite, NewInvite};
pub use key_bag::{KeyBag, KeyBagOutcome};
pub use key_package::{AddPackagesOutcome, ClaimedPackage, KeyPackageCounts};
pub use message::{HistoryQuery, NewMessage, StoreOutcome, StoredMessage};
pub use mls::{CommitOutcome, MlsBacklogQuery, NewMlsMessage, PostOutcome, StoredMlsMessage};
pub use snowflake::{Clock, SnowflakeError, SnowflakeGenerator, SystemClock};
pub use space::{
  Category, NewSpace, NewSpaceMember, Space, SpaceChanges, SpaceListing, SpaceMember, SpaceView,
};
pub use user::{OidcIdentity, PresencePreference, User, UserSearch};
