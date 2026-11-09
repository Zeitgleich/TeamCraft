//! Postgres access for TeamCraft: the only crate that talks SQL (code-style.md).
//!
//! Queries use compile-time checked `sqlx::query!` macros; the offline data in `.sqlx/` is committed.
//! Regenerate it with `cargo sqlx prepare --workspace` against a migrated dev database.

mod admin;
mod admin_join_request;
mod announcement;
mod attachment;
mod audit;
mod background;
mod bot;
mod call_history;
mod channel_deletion;
mod channel_role_log;
mod conversation;
mod database;
mod device;
mod device_list;
mod directory;
mod email_digest;
mod error;
mod idempotency;
mod identity;
mod ids;
mod import;
mod instance_stats;
mod invite;
mod key_bag;
mod key_package;
mod last_seen;
mod local_account;
mod meeting;
mod meeting_notice;
mod message;
mod mls;
mod notification;
mod oidc_sync;
mod org_background;
mod org_recovery;
mod permission_override;
mod profile;
mod push;
mod push_retry;
mod retention;
mod role_evidence;
mod scheduled_message;
mod server_settings;
mod share_link;
mod snowflake;
mod space;
mod status;
mod storage_cleanup;
mod sync_heads;
mod system_event;
#[cfg(feature = "integration")]
pub mod testing;
mod thread;
mod urgent;
mod user;
mod user_deletion;
mod user_group;
mod webhook;
mod workspace_log;

pub use admin::{
  AdminConversationFilter, AdminConversationRow, AdminDeviceRow, AdminSpaceRow, AdminUserRow,
  CREATOR_DEACTIVATED_REASON, ChatRemoval, ChatRemovalEffects, Deactivation, DeactivationAudit,
  PushDeviceCount, StorageTotals, UserStatusFilter,
};
pub use admin_join_request::{
  AdminJoinApproval, AdminJoinRequest, AdminJoinRequestClosing, AdminJoinRequestOutcome,
  RemovedAdminJoinRequest,
};
pub use announcement::{NewAnnouncement, StoredAnnouncement};
pub use attachment::{Attachment, NewAttachment, RemovedAttachment};
pub use audit::{AuditEntry, AuditQuery, NewAuditEntry};
pub use background::UserBackground;
pub use bot::{BOT_ISSUER, StoredBot};
pub use call_history::{MissedCall, StoredCall};
pub use channel_deletion::{
  ChannelFacts, DeletionSignature, RemovalDone, RemovalStep, SpaceChannelCounts, SpaceChannelFacts,
  SpaceDeletion, SpaceLeft, StoredDeletionSignature,
};
pub use channel_role_log::ChannelRoleLogEntry;
pub use conversation::{
  Conversation, ConversationChanges, ConversationFilter, ConversationMember, ConversationView, ImageChange,
  Membership, MembershipChanges, NewConversation, participants_key,
};
pub use database::{Database, DatabaseOptions};
pub use device::{Device, NewDevice};
pub use device_list::{DeviceList, DeviceListOutcome};
pub use directory::{
  DirectoryChannel, DirectoryChannelFilter, DirectoryFilterValues, DirectoryInvitation,
  DirectoryInvitationFilter, DirectoryPeopleFilter, DirectoryPerson,
};
pub use email_digest::{DueDigest, StoredEmailDigest};
pub use error::DbError;
pub use idempotency::Created;
pub use identity::{IdentityKey, IdentityKeyOutcome, InitialKeyBag, NewIdentityKey};
pub use import::{ImportOutcome, NewImportedMessage};
pub use instance_stats::{InstanceStats, PoolStats};
pub use invite::{AppliedInvite, Invite, NewInvite};
pub use key_bag::{KeyBag, KeyBagOutcome};
pub use key_package::{AddPackagesOutcome, ClaimedPackage, KeyPackageCounts, NewKeyPackage};
pub use last_seen::StoredLastSeen;
pub use local_account::{LOCAL_ISSUER, LocalAccount, SessionLimits, SessionState};
pub use meeting::{
  AttendeeDiff, AttendeeKey, LobbyRow, MeetingChanges, NewAttendee, NewMeeting, SharedInviteClaim,
  StoredAttendee, StoredMeeting, StoredMeetingInvite,
};
pub use meeting_notice::{MeetingNoticeDismissal, MeetingNoticeKind, MeetingNoticeRecipient};
pub use message::{HistoryQuery, NewMessage, StoreOutcome, StoredMessage};
pub use mls::{CommitOutcome, MlsBacklogQuery, NewMlsMessage, PostOutcome, StoredMlsMessage};
pub use notification::{DeliveryRow, ReceiptRow, StoredSpaceNotifications, StoredUserSettings};
pub use oidc_sync::{
  BrakeConfirmation, HeldDeactivation, NewHeldDeactivation, OidcSyncRun, OidcSyncStatusRow,
};
pub use org_background::OrgBackgroundObject;
pub use org_recovery::{
  ApprovalOutcome, NewRecoveryRequest, ShareOutcome, StoredApproval, StoredRecoveryRequest, StoredShare,
};
pub use permission_override::{OverrideEdit, StoredOverride};
pub use profile::{StoredAvatar, StoredProfile, StoredWorkingHours};
pub use push::{PushDeviceRow, PushRecipientRow};
pub use push_retry::{NewPushRetry, PushRetryClaim, PushRetryRow};
pub use retention::{RetentionTarget, StoredObject, StoredRetention};
pub use role_evidence::{RoleStanding, StoredRoleEvidence, StoredRoleLoss};
pub use scheduled_message::{
  NewScheduledMessage, ReleaseOutcome, ScheduleOutcome, ScheduledUpdateOutcome, SealedParts, StaleWakes,
  StoredScheduledMessage,
};
pub use server_settings::{StoredBrand, StoredBrandAsset, StoredBrandMailServer, StoredSetting};
pub use share_link::{NewShareLink, ShareLink, ShareTarget};
pub use snowflake::{Clock, SnowflakeError, SnowflakeGenerator, SystemClock};
pub use space::{
  AdminGuestListFilter, AdminGuestRow, Category, ExpiredGuest, ExpiringGuest, ExpiryChange, GuestNoticeDue,
  NewSpace, NewSpaceMember, Space, SpaceChanges, SpaceListing, SpaceMember, SpaceView, StoredSpaceImage,
};
pub use status::StoredStatus;
pub use storage_cleanup::StorageCleanup;
pub use sync_heads::ConversationHeadRow;
pub use system_event::{NewSystemEvent, StoredSystemEvent};
pub use thread::{ConversationUnreadRow, FollowedThread, ThreadActivity, ThreadQuery, ThreadUnreadTotals};
pub use urgent::{Acknowledgement, DueReminder};
pub use user::{OidcIdentity, OidcUserRow, PresencePreference, SyncedUser, User, UserSearch};
pub use user_deletion::{ActiveAdminCounts, DELETED_USER_NAME, DirectConversationToDelete, UserDeletion};
pub use user_group::{GroupCreateOutcome, NewUserGroup, UserGroup};
pub use webhook::{
  EnqueueOutcome, NewWebhook, NewWebhookLogEntry, StoredWebhook, StoredWebhookLogEntry, WebhookChange,
  WebhookDeliveryClaim, WebhookDeliveryRow,
};
pub use workspace_log::{AppendOutcome, WorkspaceLogEntry};
