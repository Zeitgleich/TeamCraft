//! Postgres access for TeamCraft: the only crate that talks SQL (code-style.md).
//!
//! Queries use compile-time checked `sqlx::query!` macros; the offline data in `.sqlx/` is committed.
//! Regenerate it with `cargo sqlx prepare --workspace` against a migrated dev database.

mod admin;
mod attachment;
mod audit;
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
mod push;
mod retention;
mod snowflake;
mod space;
#[cfg(feature = "integration")]
pub mod testing;
mod user;
mod workspace_log;

pub use admin::{
  AdminConversationFilter, AdminConversationRow, AdminDeviceRow, AdminSpaceRow, AdminUserRow,
  DeactivationEffects, UserStatusFilter,
};
pub use attachment::{Attachment, NewAttachment};
pub use audit::{AuditEntry, AuditQuery, NewAuditEntry};
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
pub use push::{PushDeviceRow, PushRecipientRow};
pub use retention::{RetentionTarget, StoredObject, StoredRetention};
pub use snowflake::{Clock, SnowflakeError, SnowflakeGenerator, SystemClock};
pub use space::{
  Category, NewSpace, NewSpaceMember, Space, SpaceChanges, SpaceListing, SpaceMember, SpaceView,
};
pub use user::{OidcIdentity, OidcUserRow, PresencePreference, User, UserSearch};
pub use workspace_log::{AppendOutcome, WorkspaceLogEntry};
