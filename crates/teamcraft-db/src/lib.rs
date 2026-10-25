//! Postgres access for TeamCraft: the only crate that talks SQL (code-style.md).
//!
//! Queries use compile-time checked `sqlx::query!` macros; the offline data in `.sqlx/` is committed.
//! Regenerate it with `cargo sqlx prepare --workspace` against a migrated dev database.

mod admin;
mod announcement;
mod attachment;
mod audit;
mod background;
mod bot;
mod call_history;
mod conversation;
mod database;
mod device;
mod device_list;
mod email_digest;
mod error;
mod idempotency;
mod identity;
mod ids;
mod invite;
mod key_bag;
mod key_package;
mod last_seen;
mod local_account;
mod meeting;
mod message;
mod mls;
mod notification;
mod org_background;
mod org_recovery;
mod permission_override;
mod profile;
mod push;
mod push_retry;
mod retention;
mod server_settings;
mod share_link;
mod snowflake;
mod space;
mod status;
mod system_event;
#[cfg(feature = "integration")]
pub mod testing;
mod thread;
mod urgent;
mod user;
mod user_group;
mod workspace_log;

pub use admin::{
  AdminConversationFilter, AdminConversationRow, AdminDeviceRow, AdminSpaceRow, AdminUserRow,
  DeactivationEffects, PushDeviceCount, StorageTotals, UserStatusFilter,
};
pub use announcement::{NewAnnouncement, StoredAnnouncement};
pub use attachment::{Attachment, NewAttachment};
pub use audit::{AuditEntry, AuditQuery, NewAuditEntry};
pub use background::UserBackground;
pub use bot::{BOT_ISSUER, StoredBot};
pub use call_history::{MissedCall, StoredCall};
pub use conversation::{
  Conversation, ConversationChanges, ConversationFilter, ConversationMember, ConversationView, ImageChange,
  Membership, MembershipChanges, NewConversation, participants_key,
};
pub use database::{Database, DatabaseOptions};
pub use device::{Device, NewDevice};
pub use device_list::{DeviceList, DeviceListOutcome};
pub use email_digest::{DueDigest, StoredEmailDigest};
pub use error::DbError;
pub use idempotency::Created;
pub use identity::{IdentityKey, IdentityKeyOutcome, InitialKeyBag, NewIdentityKey};
pub use invite::{AppliedInvite, Invite, NewInvite};
pub use key_bag::{KeyBag, KeyBagOutcome};
pub use key_package::{AddPackagesOutcome, ClaimedPackage, KeyPackageCounts, NewKeyPackage};
pub use last_seen::StoredLastSeen;
pub use local_account::{LOCAL_ISSUER, LocalAccount, SessionLimits, SessionState};
pub use meeting::{
  AttendeeDiff, AttendeeKey, LobbyRow, MeetingChanges, NewAttendee, NewMeeting, StoredAttendee,
  StoredMeeting, StoredMeetingInvite,
};
pub use message::{HistoryQuery, NewMessage, StoreOutcome, StoredMessage};
pub use mls::{CommitOutcome, MlsBacklogQuery, NewMlsMessage, PostOutcome, StoredMlsMessage};
pub use notification::{ReceiptRow, StoredSpaceNotifications, StoredUserSettings};
pub use org_background::OrgBackgroundObject;
pub use org_recovery::{
  ApprovalOutcome, NewRecoveryRequest, ShareOutcome, StoredApproval, StoredRecoveryRequest, StoredShare,
};
pub use permission_override::{OverrideEdit, StoredOverride};
pub use profile::{StoredAvatar, StoredProfile, StoredWorkingHours};
pub use push::{PushDeviceRow, PushRecipientRow};
pub use push_retry::{NewPushRetry, PushRetryClaim, PushRetryRow};
pub use retention::{RetentionTarget, StoredObject, StoredRetention};
pub use server_settings::{StoredBrand, StoredBrandAsset, StoredSetting};
pub use share_link::{NewShareLink, ShareLink, ShareTarget};
pub use snowflake::{Clock, SnowflakeError, SnowflakeGenerator, SystemClock};
pub use space::{
  Category, ExpiredGuest, ExpiryChange, NewSpace, NewSpaceMember, Space, SpaceChanges, SpaceListing,
  SpaceMember, SpaceView, StoredSpaceImage,
};
pub use status::StoredStatus;
pub use system_event::{NewSystemEvent, StoredSystemEvent};
pub use thread::{ConversationUnreadRow, FollowedThread, ThreadActivity, ThreadQuery, ThreadUnreadTotals};
pub use urgent::{Acknowledgement, DueReminder};
pub use user::{OidcIdentity, OidcUserRow, PresencePreference, SyncedUser, User, UserSearch};
pub use user_group::{GroupCreateOutcome, NewUserGroup, UserGroup};
pub use workspace_log::{AppendOutcome, WorkspaceLogEntry};
