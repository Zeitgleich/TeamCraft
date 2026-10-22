use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{
  CategoryId, ChannelModeration, ChannelRole, ConversationId, ConversationKind, ConversationVisibility,
  HistoryPolicy, MessageId, SpaceId, UnixMillis, UserId, UserSummary,
};

wire_enum! {
  /// Which messages notify the member.
  pub enum NotificationLevel {
    All => "all",
    Mentions => "mentions",
    None => "none",
  }
}

/// The caller's own membership in a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MyMembership {
  pub role: ChannelRole,
  pub notification_level: NotificationLevel,
  pub is_favorite: bool,
  #[serde(
    rename = "last_read_message_id",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_last_read_message_id: Option<MessageId>,
  /// The conversation is muted until this time.
  #[serde(rename = "muted_until", default, skip_serializing_if = "Option::is_none")]
  pub maybe_muted_until: Option<UnixMillis>,
  /// Level and mute time follow the space's setting (the member never chose their own).
  #[serde(default)]
  pub is_notification_inherited: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ConversationResponse {
  pub id: ConversationId,
  #[serde(rename = "space_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_space_id: Option<SpaceId>,
  pub kind: ConversationKind,
  #[serde(rename = "name", default, skip_serializing_if = "Option::is_none")]
  pub maybe_name: Option<String>,
  #[serde(rename = "topic", default, skip_serializing_if = "Option::is_none")]
  pub maybe_topic: Option<String>,
  #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
  pub maybe_description: Option<String>,
  pub visibility: ConversationVisibility,
  pub history_policy: HistoryPolicy,
  #[serde(rename = "category_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_category_id: Option<CategoryId>,
  pub is_archived: bool,
  pub moderation: ChannelModeration,
  /// Receives machine output (logs, monitoring, CI): retention defaults for logs, no push unless
  /// mentioned, compact log view in clients.
  #[serde(default)]
  pub is_integration: bool,
  /// Badge "contains external people".
  pub contains_guests: bool,
  /// Absent if the caller is not a member (public channels they could join).
  #[serde(rename = "my_membership", default, skip_serializing_if = "Option::is_none")]
  pub maybe_my_membership: Option<MyMembership>,
  /// The other participants of DMs and group DMs; empty for channels.
  #[serde(default)]
  pub participants: Vec<UserSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListConversationsResponse {
  pub conversations: Vec<ConversationResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateChannelRequest {
  pub name: String,
  pub visibility: ConversationVisibility,
  #[serde(rename = "category_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_category_id: Option<CategoryId>,
  #[serde(rename = "history_policy", default, skip_serializing_if = "Option::is_none")]
  pub maybe_history_policy: Option<HistoryPolicy>,
  #[serde(rename = "topic", default, skip_serializing_if = "Option::is_none")]
  pub maybe_topic: Option<String>,
  #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
  pub maybe_description: Option<String>,
  /// Marks the channel as integration channel (see `ConversationResponse::is_integration`).
  #[serde(default)]
  pub is_integration: bool,
}

/// `PATCH /api/v1/conversations/{id}`: only present fields change; an empty string clears topic and
/// description. Changing `visibility` converts the channel (permission `convert_channel_privacy`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpdateConversationRequest {
  #[serde(rename = "is_integration", default, skip_serializing_if = "Option::is_none")]
  pub maybe_is_integration: Option<bool>,
  #[serde(rename = "name", default, skip_serializing_if = "Option::is_none")]
  pub maybe_name: Option<String>,
  #[serde(rename = "topic", default, skip_serializing_if = "Option::is_none")]
  pub maybe_topic: Option<String>,
  #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
  pub maybe_description: Option<String>,
  #[serde(rename = "visibility", default, skip_serializing_if = "Option::is_none")]
  pub maybe_visibility: Option<ConversationVisibility>,
  #[serde(rename = "category_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_category_id: Option<CategoryId>,
  /// Removes the channel from its category (ignored if `category_id` is set).
  #[serde(default)]
  pub clear_category: bool,
  #[serde(rename = "history_policy", default, skip_serializing_if = "Option::is_none")]
  pub maybe_history_policy: Option<HistoryPolicy>,
  #[serde(rename = "is_archived", default, skip_serializing_if = "Option::is_none")]
  pub maybe_is_archived: Option<bool>,
  #[serde(rename = "moderation", default, skip_serializing_if = "Option::is_none")]
  pub maybe_moderation: Option<ChannelModeration>,
}

/// `PUT /api/v1/conversations/{id}/visibility`: makes a channel public or private (permission
/// `convert_channel_privacy`). What it means for members, keys and history: docs/e2ee.md §4.7.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ConvertChannelRequest {
  pub visibility: ConversationVisibility,
  /// What people who join *after* the conversion get to read. Absent keeps the current policy;
  /// converting to public is the moment to decide it, because everybody in the space may join.
  #[serde(rename = "history_policy", default, skip_serializing_if = "Option::is_none")]
  pub maybe_history_policy: Option<HistoryPolicy>,
}

/// `PUT /api/v1/conversations/{id}/space`: moves a channel to another space. The MLS group, the
/// members and the history stay as they are; the channel leaves its category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MoveChannelRequest {
  pub space_id: SpaceId,
  /// A category of the target space to file the channel under.
  #[serde(rename = "category_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_category_id: Option<CategoryId>,
}

/// `POST /api/v1/dms`: opens (or returns the existing) DM with one person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateDmRequest {
  pub user_id: UserId,
}

/// `POST /api/v1/group-dms`: opens (or returns the existing) group DM with these people.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateGroupDmRequest {
  pub user_ids: Vec<UserId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ConversationMemberResponse {
  pub user: UserSummary,
  pub role: ChannelRole,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListConversationMembersResponse {
  pub members: Vec<ConversationMemberResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AddConversationMemberRequest {
  pub user_id: UserId,
  #[serde(rename = "role", default, skip_serializing_if = "Option::is_none")]
  pub maybe_role: Option<ChannelRole>,
}

/// `PATCH /api/v1/conversations/{id}/members/{user_id}`: role (needs `manage_channel_roles`) or the
/// caller's own preferences.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpdateConversationMemberRequest {
  #[serde(rename = "role", default, skip_serializing_if = "Option::is_none")]
  pub maybe_role: Option<ChannelRole>,
  #[serde(
    rename = "notification_level",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_notification_level: Option<NotificationLevel>,
  #[serde(rename = "is_favorite", default, skip_serializing_if = "Option::is_none")]
  pub maybe_is_favorite: Option<bool>,
}

/// Query of `GET /api/v1/conversations`. Without parameters: all conversations the caller is in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListConversationsQuery {
  /// Only conversations of this space.
  #[serde(rename = "space_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_space_id: Option<SpaceId>,
  /// Channel browser: public channels of `space_id` the caller has not joined yet (needs `space_id`).
  #[serde(default)]
  pub joinable: bool,
}

/// `PUT /api/v1/conversations/{id}/read-position`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SetReadPositionRequest {
  pub message_id: MessageId,
}
