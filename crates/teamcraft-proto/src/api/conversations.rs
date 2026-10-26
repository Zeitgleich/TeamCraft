use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{
  CategoryId, ChannelModeration, ChannelRole, ConversationId, ConversationKind, ConversationVisibility,
  HistoryPolicy, IdempotencyKey, MessageId, Permission, SpaceId, SpaceImage, UnixMillis, UserId, UserSummary,
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
  /// The participants of DMs and group DMs, the caller included; empty for channels. A DM's are
  /// the people it was opened for, also once one of them is deactivated.
  #[serde(default)]
  pub participants: Vec<UserSummary>,
  /// The channel's picture (`PUT /api/v1/conversations/{id}/image`, `manage_channel_properties`).
  #[serde(rename = "image", default, skip_serializing_if = "Option::is_none")]
  pub maybe_image: Option<SpaceImage>,
  /// The organizer of a meeting chat; only in the answer for a single conversation
  /// (`GET /api/v1/conversations/{id}`). Clients check that a meeting chat's encryption group was
  /// founded by this user (e2ee.md §4.6, "Meeting chats"); the server's word, not a signature.
  #[serde(
    rename = "organizer_user_id",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_organizer_user_id: Option<UserId>,
  /// The channel permissions the caller holds in the conversation now (`create_post`,
  /// `manage_members`, ...), so clients offer only what will work (issue #691). A hint: every
  /// request is checked again. Absent from older servers; clients then evaluate the built-in
  /// scheme themselves.
  #[serde(rename = "my_permissions", default, skip_serializing_if = "Option::is_none")]
  pub maybe_my_permissions: Option<Vec<Permission>>,
}

impl ConversationResponse {
  /// A DM or group DM whose other participants are all deactivated (issue #698): clients list it
  /// apart from the active conversations, its history stays readable. `is_deactivated` judges a
  /// participant by the freshest account state the client has.
  pub fn has_only_deactivated_others(
    &self,
    me: UserId,
    is_deactivated: impl Fn(&UserSummary) -> bool,
  ) -> bool {
    if !matches!(self.kind, ConversationKind::Dm | ConversationKind::Group) {
      return false;
    }
    let mut others = self.participants.iter().filter(|user| user.id != me).peekable();
    others.peek().is_some() && others.all(is_deactivated)
  }

  /// Whether the caller may post: the server's word ([`Self::maybe_my_permissions`]) when it gave
  /// one, otherwise anything that isn't archived (an older server checks on send).
  pub fn may_post(&self) -> bool {
    self
      .maybe_my_permissions
      .as_ref()
      .map_or(!self.is_archived, |granted| {
        granted.contains(&Permission::CreatePost)
      })
  }

  /// The user's notes to themself: a DM opened with oneself (e2ee.md, "Notes to self"). Decided by
  /// the DM's participants, never by its current members: a DM whose other person was
  /// deactivated has only the user left as member and is still not their notes (issue #643).
  pub fn is_notes_to_self(&self, me: UserId) -> bool {
    self.kind == ConversationKind::Dm
      && !self.participants.is_empty()
      && self.participants.iter().all(|user| user.id == me)
  }
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
  /// Client-generated key that makes a retry of this request return the original result instead
  /// of creating a second one. Optional so older clients keep working.
  #[serde(rename = "idempotency_key", default, skip_serializing_if = "Option::is_none")]
  pub maybe_idempotency_key: Option<IdempotencyKey>,
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

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{Id, Snowflake, UserKind};

  const ME: i64 = 1;

  #[test]
  fn my_permissions_are_optional_on_the_wire_and_decide_posting() {
    let older = conversation(ConversationKind::Channel, &[]);
    let json = serde_json::to_value(&older).unwrap();
    assert!(
      json.get("my_permissions").is_none(),
      "older answers stay as they were"
    );
    // Without the server's word, anything not archived offers posting.
    assert!(older.may_post());
    let archived = ConversationResponse {
      is_archived: true,
      ..older.clone()
    };
    assert!(!archived.may_post());
    let announcement = ConversationResponse {
      maybe_my_permissions: Some(vec![Permission::AddReaction]),
      ..older.clone()
    };
    let json = serde_json::to_value(&announcement).unwrap();
    assert_eq!(json["my_permissions"], serde_json::json!(["add_reaction"]));
    assert!(!announcement.may_post());
    let parsed: ConversationResponse = serde_json::from_value(json).unwrap();
    assert_eq!(parsed.maybe_my_permissions, Some(vec![Permission::AddReaction]));
  }

  #[test]
  fn only_dms_whose_other_people_are_all_deactivated_count_as_deactivated() {
    let deactivated = |user: &UserSummary| user.is_deactivated;
    let me = user_id(ME);
    assert!(conversation(ConversationKind::Dm, &[(2, true)]).has_only_deactivated_others(me, deactivated));
    assert!(!conversation(ConversationKind::Dm, &[(2, false)]).has_only_deactivated_others(me, deactivated));
    let group = conversation(ConversationKind::Group, &[(2, true), (3, false)]);
    assert!(
      !group.has_only_deactivated_others(me, deactivated),
      "one is still there"
    );
    let gone = conversation(ConversationKind::Group, &[(2, true), (3, true)]);
    assert!(gone.has_only_deactivated_others(me, deactivated));
    // Notes to self have nobody else; channels never count.
    assert!(!conversation(ConversationKind::Dm, &[]).has_only_deactivated_others(me, deactivated));
    assert!(
      !conversation(ConversationKind::Channel, &[(2, true)]).has_only_deactivated_others(me, deactivated)
    );
  }

  fn conversation(kind: ConversationKind, others: &[(i64, bool)]) -> ConversationResponse {
    let participants = std::iter::once((ME, false))
      .chain(others.iter().copied())
      .map(|(raw, is_deactivated)| UserSummary {
        id: user_id(raw),
        maybe_username: None,
        display_name: format!("Person {raw}"),
        kind: UserKind::Member,
        maybe_email: None,
        maybe_organization: None,
        maybe_has_devices: None,
        is_deactivated,
      })
      .collect();
    ConversationResponse {
      id: ConversationId::from_snowflake(Snowflake::new(9).unwrap()),
      maybe_space_id: None,
      kind,
      maybe_name: None,
      maybe_topic: None,
      maybe_description: None,
      visibility: ConversationVisibility::Private,
      history_policy: HistoryPolicy::FromJoin,
      maybe_category_id: None,
      is_archived: false,
      moderation: ChannelModeration::default(),
      is_integration: false,
      contains_guests: false,
      maybe_my_membership: None,
      participants,
      maybe_image: None,
      maybe_organizer_user_id: None,
      maybe_my_permissions: None,
    }
  }

  fn user_id(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
