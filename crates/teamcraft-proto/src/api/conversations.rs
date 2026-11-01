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
  /// The reactions to the member's own messages they saw (notifications.md, "Reactions to my
  /// messages"). Missing from older servers: nothing seen.
  #[serde(default, skip_serializing_if = "ReactionsSeen::is_empty")]
  pub reactions_seen: ReactionsSeen,
}

/// Which reactions to their own messages a member saw, synced across their devices. Reactions are
/// encrypted messages, so the server keeps message IDs only and can't tell which are reactions.
/// It only grows: `through` never moves back, an ID once seen stays seen.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReactionsSeen {
  /// Every message up to this ID counts as seen.
  #[serde(rename = "through_message_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_through: Option<MessageId>,
  /// Reaction messages after `through` seen one by one, ascending.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub message_ids: Vec<MessageId>,
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
  /// The import account while it is a member of this DM or notes without being a participant
  /// (e2ee.md §4.10): clients leave it out of the name and member list and show the import banner,
  /// if the signed policy names it.
  #[serde(rename = "importer_user_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_importer_user_id: Option<UserId>,
  /// The newest imported message once an import into this conversation finished; when it changes,
  /// clients that had loaded the history's start load it again.
  #[serde(rename = "imported_through", default, skip_serializing_if = "Option::is_none")]
  pub maybe_imported_through: Option<MessageId>,
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
  /// Direct messages deleted with a deleted account since this user's devices last looked
  /// (admin.md §3.2.1): devices drop them with their local history. Absent from older servers.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub deleted_conversation_ids: Vec<ConversationId>,
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

/// `PUT /api/v1/conversations/{id}/reactions-seen`: reaction messages (to the caller's own messages)
/// the caller saw; added to [`MyMembership::reactions_seen`]. At most
/// [`MAX_REACTIONS_SEEN_PER_REQUEST`], all messages of the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MarkReactionsSeenRequest {
  pub message_ids: Vec<MessageId>,
}

/// Reaction messages one [`MarkReactionsSeenRequest`] names at most.
pub const MAX_REACTIONS_SEEN_PER_REQUEST: usize = 100;

impl ReactionsSeen {
  pub fn is_empty(&self) -> bool {
    self.maybe_through.is_none() && self.message_ids.is_empty()
  }

  pub fn is_seen(&self, message: MessageId) -> bool {
    self.maybe_through.is_some_and(|through| message <= through) || self.message_ids.binary_search(&message).is_ok()
  }

  /// Adds seen messages and keeps at most `max_ids` of them one by one: beyond it `through` moves up
  /// past the oldest ones, which then count as seen together with everything before them. Nothing
  /// seen ever becomes unseen.
  pub fn add(&mut self, seen: impl IntoIterator<Item = MessageId>, max_ids: usize) {
    self.message_ids.extend(seen);
    self.message_ids.sort_unstable();
    self.message_ids.dedup();
    let excess = self.message_ids.len().saturating_sub(max_ids);
    if excess > 0 {
      let dropped = self.message_ids[excess - 1];
      self.maybe_through = self.maybe_through.max(Some(dropped));
    }
    let maybe_through = self.maybe_through;
    self
      .message_ids
      .retain(|id| maybe_through.is_none_or(|through| *id > through));
  }

  /// Both states together, as [`Self::add`] keeps them (another device's state, a queued change).
  pub fn merge(&mut self, other: &ReactionsSeen, max_ids: usize) {
    self.maybe_through = self.maybe_through.max(other.maybe_through);
    self.add(other.message_ids.iter().copied(), max_ids);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn deleted_conversations_are_listed_only_when_there_are_some() {
    let empty = ListConversationsResponse {
      conversations: Vec::new(),
      deleted_conversation_ids: Vec::new(),
    };
    assert_eq!(serde_json::to_string(&empty).unwrap(), r#"{"conversations":[]}"#);
    let listed: ListConversationsResponse =
      serde_json::from_str(r#"{"conversations":[],"deleted_conversation_ids":["8"]}"#).unwrap();
    assert_eq!(listed.deleted_conversation_ids.len(), 1);
    let older: ListConversationsResponse = serde_json::from_str(r#"{"conversations":[]}"#).unwrap();
    assert!(older.deleted_conversation_ids.is_empty());
  }

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
      maybe_importer_user_id: None,
      maybe_imported_through: None,
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
        is_deleted: false,
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
      maybe_importer_user_id: None,
      maybe_imported_through: None,
    }
  }

  mod reactions_seen {
    use super::*;

    #[test]
    fn seen_messages_stay_seen_and_through_only_moves_up() {
      let mut seen = ReactionsSeen::default();
      seen.add([message_id(30), message_id(10), message_id(30)], 10);
      assert_eq!(seen.message_ids, vec![message_id(10), message_id(30)]);
      assert!(seen.is_seen(message_id(10)) && !seen.is_seen(message_id(20)));
      let older = ReactionsSeen {
        maybe_through: Some(message_id(5)),
        message_ids: vec![message_id(20)],
      };
      seen.merge(&older, 10);
      assert_eq!(seen.maybe_through, Some(message_id(5)));
      assert!(seen.is_seen(message_id(20)) && seen.is_seen(message_id(30)));
      let newer = ReactionsSeen {
        maybe_through: Some(message_id(25)),
        message_ids: Vec::new(),
      };
      seen.merge(&newer, 10);
      seen.merge(&older, 10);
      assert_eq!(seen.maybe_through, Some(message_id(25)));
      assert_eq!(seen.message_ids, vec![message_id(30)]);
    }

    #[test]
    fn beyond_the_cap_the_oldest_are_folded_into_through() {
      let mut seen = ReactionsSeen::default();
      seen.add((1..=5).map(message_id), 3);
      assert_eq!(seen.maybe_through, Some(message_id(2)));
      assert_eq!(seen.message_ids, vec![message_id(3), message_id(4), message_id(5)]);
    }

    #[test]
    fn the_wire_form_is_stable_and_older_servers_send_nothing() {
      let seen = ReactionsSeen {
        maybe_through: Some(message_id(7)),
        message_ids: vec![message_id(9)],
      };
      assert_eq!(
        serde_json::to_string(&seen).unwrap(),
        r#"{"through_message_id":"7","message_ids":["9"]}"#
      );
      let membership: MyMembership =
        serde_json::from_str(r#"{"role":"member","notification_level":"all","is_favorite":false}"#).unwrap();
      assert!(membership.reactions_seen.is_empty());
    }
  }

  fn message_id(raw: i64) -> MessageId {
    MessageId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn user_id(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
