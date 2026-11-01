//! [`ChatBackend`] on top of the real `teamcraft-server`, read-only for now (the browser build; the
//! desktop app uses the client core instead, see `remote_backend`).
//!
//! Spaces, conversations, members, users and presence come from the REST API. Messages are not
//! available yet (they arrive end-to-end encrypted through `teamcraft-core`), so messages written
//! here live in memory for the session. Search, own devices and keys need a core and answer empty here.

use std::cell::RefCell;
use std::collections::HashMap;

use teamcraft_proto::{
  ConversationKind as ConversationKindProto, ConversationResponse, ConversationVisibility, ErrorBody,
  GetMeResponse, ListConversationMembersResponse, ListConversationsResponse, ListPresenceResponse,
  ListSpacesResponse, PresenceState, Role, SpaceResponse, UserKind, UserPresence, UserSummary,
};

use crate::auth::{AuthError, Authenticator};
use crate::backend::ChatBackend;
use crate::model::{
  Conversation, ConversationId, ConversationKind, CustomStatus, Message, MessageId, MessageKind,
  PresenceStatus, SidebarCategory, Space, SpaceId, ThreadId, ThreadSummary, TimestampMs, User, UserId,
  hue_from_hex, id_from_raw, raw_id,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoadError {
  #[error("{0}")]
  Auth(#[from] AuthError),
  #[error("the server answered {status} for {path}")]
  Status { path: String, status: u16 },
  #[error("unreadable response for {path}: {reason}")]
  Parse { path: String, reason: String },
}

/// Everything fetched from the server, mapped to the UI's view models.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
  pub me: User,
  pub role: Role,
  pub users: Vec<User>,
  pub spaces: Vec<Space>,
  pub conversations: Vec<Conversation>,
}

pub struct RestChat {
  snapshot: RefCell<Snapshot>,
  /// Messages written in this session, per conversation.
  local_messages: RefCell<HashMap<ConversationId, Vec<Message>>>,
  next_message: RefCell<u64>,
  now: TimestampMs,
}

impl RestChat {
  /// The role of the signed-in user, for the admin check.
  pub fn role(&self) -> Role {
    self.snapshot.borrow().role.clone()
  }

  /// Fetches spaces, conversations, members and presence for the signed-in user.
  pub async fn load(
    auth: &dyn Authenticator,
    me: &GetMeResponse,
    now: TimestampMs,
  ) -> Result<Self, LoadError> {
    let spaces: ListSpacesResponse = get_json(auth, "/api/v1/spaces").await?;
    let conversations: ListConversationsResponse = get_json(auth, "/api/v1/conversations").await?;
    let mut members = HashMap::new();
    for conversation in conversations
      .conversations
      .iter()
      .filter(|c| c.maybe_my_membership.is_some())
    {
      let path = format!("/api/v1/conversations/{}/members", conversation.id);
      let listed: ListConversationMembersResponse = get_json(auth, &path).await?;
      members.insert(
        conversation.id,
        listed
          .members
          .into_iter()
          .map(|member| member.user)
          .collect::<Vec<_>>(),
      );
    }
    let mut ids: Vec<String> = vec![me.id.to_string()];
    for user in members
      .values()
      .flatten()
      .chain(conversations.conversations.iter().flat_map(|c| &c.participants))
    {
      let id = user.id.to_string();
      if !ids.contains(&id) {
        ids.push(id);
      }
    }
    let presence: ListPresenceResponse =
      get_json(auth, &format!("/api/v1/presence?user_ids={}", ids.join(","))).await?;
    let snapshot = build_snapshot(
      me,
      &spaces.spaces,
      &conversations.conversations,
      &members,
      &presence.presences,
    );
    Ok(Self {
      snapshot: RefCell::new(snapshot),
      local_messages: RefCell::new(HashMap::new()),
      next_message: RefCell::new(1),
      now,
    })
  }
}

async fn get_json<T: serde::de::DeserializeOwned>(
  auth: &dyn Authenticator,
  path: &str,
) -> Result<T, LoadError> {
  let response = auth.api_get(path).await?;
  if response.status != 200 {
    if let Ok(body) = serde_json::from_str::<ErrorBody>(&response.body) {
      tracing::warn!(path, status = response.status, code = %body.error_code, "server refused a read");
    }
    return Err(LoadError::Status {
      path: path.to_owned(),
      status: response.status,
    });
  }
  serde_json::from_str(&response.body).map_err(|error| LoadError::Parse {
    path: path.to_owned(),
    reason: error.to_string(),
  })
}

/// Maps the API's shapes to view models. Pure, so it is tested with fixtures.
pub fn build_snapshot(
  me: &GetMeResponse,
  spaces: &[SpaceResponse],
  conversations: &[ConversationResponse],
  members: &HashMap<teamcraft_proto::ConversationId, Vec<UserSummary>>,
  presences: &[UserPresence],
) -> Snapshot {
  let presence_of = |id: teamcraft_proto::UserId| {
    presences
      .iter()
      .find(|entry| entry.user_id == id)
      .map_or(PresenceStatus::Offline, |entry| match &entry.state {
        PresenceState::Online => PresenceStatus::Online,
        PresenceState::Away => PresenceStatus::Away,
        PresenceState::DoNotDisturb => PresenceStatus::DoNotDisturb,
        PresenceState::Offline | PresenceState::Unknown(_) => PresenceStatus::Offline,
      })
  };
  let to_user = |summary: &UserSummary| User {
    id: summary.id,
    username: summary.maybe_username.clone().unwrap_or_default(),
    display_name: summary.display_name.clone(),
    presence: presence_of(summary.id),
    maybe_custom_status: None,
    avatar_hue: hue_from_seed(&summary.id.to_string()),
    is_guest: summary.kind == UserKind::Guest,
    is_bot: false,
    maybe_organization: summary.maybe_organization.clone(),
    is_deactivated: false,
    is_deleted: false,
    device_setup: crate::model::DeviceSetup::Unknown,
    maybe_email: summary.maybe_email.clone(),
  };

  let me_user = User {
    id: me.id,
    username: me.maybe_username.clone().unwrap_or_default(),
    display_name: me.display_name.clone(),
    // The caller is online by definition: this request proves it.
    presence: PresenceStatus::Online,
    maybe_custom_status: None,
    avatar_hue: hue_from_seed(&me.id.to_string()),
    is_guest: me.role == Role::Guest,
    is_bot: false,
    maybe_organization: None,
    is_deactivated: false,
    is_deleted: false,
    device_setup: crate::model::DeviceSetup::Unknown,
    maybe_email: None,
  };

  let mut users: Vec<User> = vec![me_user.clone()];
  let mut remember = |summary: &UserSummary| {
    let user = to_user(summary);
    if users.iter().all(|known| known.id != user.id) {
      users.push(user);
    }
  };
  for summary in members
    .values()
    .flatten()
    .chain(conversations.iter().flat_map(|c| &c.participants))
  {
    remember(summary);
  }

  let spaces: Vec<Space> = spaces
    .iter()
    .filter(|space| space.maybe_my_role.is_some() && !space.is_archived)
    .map(|space| Space {
      id: space.id,
      name: space.name.clone(),
      hue: space
        .maybe_color
        .as_deref()
        .and_then(hue_from_hex)
        .unwrap_or_else(|| hue_from_seed(&space.name)),
      maybe_color: space.maybe_color.clone(),
      maybe_image_version: space.maybe_image.as_ref().map(|image| image.version.clone()),
      unread_count: 0,
      mention_count: 0,
      maybe_description: space.maybe_description.clone(),
      member_count: space.member_count,
      join_policy: space.join_policy.clone(),
      can_add_members: space.can_add_members,
      can_invite_guests: space.can_invite_guests,
      can_manage_space: space.can_manage_space,
      can_create_channels: space.can_create_channels,
      maybe_my_role: space.maybe_my_role.clone(),
    })
    .collect();

  let conversations: Vec<Conversation> = conversations
    .iter()
    .filter(|conversation| conversation.maybe_my_membership.is_some() && !conversation.is_archived)
    .map(|conversation| {
      let member_ids = match members.get(&conversation.id) {
        Some(list) => list.iter().map(|user| user.id).collect(),
        None => conversation
          .participants
          .iter()
          .map(|user| user.id)
          .filter(|id| *id != me_user.id)
          .chain([me_user.id])
          .collect(),
      };
      to_conversation(conversation, member_ids, me.id)
    })
    .collect();

  Snapshot {
    me: me_user,
    role: me.role.clone(),
    users,
    spaces,
    conversations,
  }
}

/// `me` is left out of generated DM names; the server lists every participant including the caller.
fn to_conversation(conversation: &ConversationResponse, member_ids: Vec<UserId>, me: UserId) -> Conversation {
  let kind = match (&conversation.kind, &conversation.visibility) {
    (ConversationKindProto::Dm, _) => ConversationKind::Direct,
    (ConversationKindProto::Group, _) => ConversationKind::Group,
    (_, ConversationVisibility::Private) => ConversationKind::Private,
    _ => ConversationKind::Public,
  };
  let name = conversation
    .maybe_name
    .clone()
    .filter(|name| !name.is_empty())
    .unwrap_or_else(|| {
      conversation
        .participants
        .iter()
        .filter(|user| user.id != me)
        .map(|user| user.display_name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
    });
  let membership = conversation.maybe_my_membership.as_ref();
  Conversation {
    id: conversation.id,
    maybe_space: conversation.maybe_space_id,
    kind,
    category: if kind.is_direct_like() {
      SidebarCategory::DirectMessages
    } else {
      SidebarCategory::Channels
    },
    name,
    topic: conversation.maybe_topic.clone().unwrap_or_default(),
    member_ids,
    unread_count: 0,
    mention_count: 0,
    is_favorite: membership.is_some_and(|membership| membership.is_favorite),
    is_muted: membership
      .is_some_and(|membership| membership.notification_level == teamcraft_proto::NotificationLevel::None),
    can_post: conversation.may_post(),
    bookmarks: Vec::new(),
    is_catching_up: false,
    is_notes_to_self: conversation.is_notes_to_self(me),
    maybe_permissions: conversation.maybe_my_permissions.clone(),
    has_only_deactivated_others: conversation
      .has_only_deactivated_others(me, |participant| participant.is_deactivated),
    maybe_image_version: conversation
      .maybe_image
      .as_ref()
      .map(|image| image.version.clone()),
  }
}

/// A stable hue (0–359) from any text, for generated avatars and space colours.
pub(crate) fn hue_from_seed(seed: &str) -> u16 {
  let hash = seed.bytes().fold(5381_u32, |acc, byte| {
    acc.wrapping_mul(33).wrapping_add(u32::from(byte))
  });
  (hash % 360) as u16
}

impl ChatBackend for RestChat {
  fn me(&self) -> User {
    self.snapshot.borrow().me.clone()
  }

  fn user(&self, id: UserId) -> Option<User> {
    self
      .snapshot
      .borrow()
      .users
      .iter()
      .find(|user| user.id == id)
      .cloned()
  }

  fn users(&self) -> Vec<User> {
    self.snapshot.borrow().users.clone()
  }

  fn spaces(&self) -> Vec<Space> {
    self.snapshot.borrow().spaces.clone()
  }

  fn space_conversations(&self, space: SpaceId) -> Vec<Conversation> {
    let snapshot = self.snapshot.borrow();
    snapshot
      .conversations
      .iter()
      .filter(|conversation| conversation.maybe_space == Some(space))
      .cloned()
      .collect()
  }

  fn direct_conversations(&self) -> Vec<Conversation> {
    let snapshot = self.snapshot.borrow();
    snapshot
      .conversations
      .iter()
      .filter(|conversation| conversation.kind.is_direct_like())
      .cloned()
      .collect()
  }

  fn conversation(&self, id: ConversationId) -> Option<Conversation> {
    self
      .snapshot
      .borrow()
      .conversations
      .iter()
      .find(|conversation| conversation.id == id)
      .cloned()
  }

  fn message_count(&self, conversation: ConversationId) -> usize {
    self
      .local_messages
      .borrow()
      .get(&conversation)
      .map_or(0, Vec::len)
  }

  fn messages(&self, conversation: ConversationId, start: usize, len: usize) -> Vec<Message> {
    let local = self.local_messages.borrow();
    let Some(all) = local.get(&conversation) else {
      return Vec::new();
    };
    let end = all.len().min(start.saturating_add(len));
    if start >= end {
      Vec::new()
    } else {
      all[start..end].to_vec()
    }
  }

  fn thread(&self, _conversation: ConversationId, _thread: ThreadId) -> Vec<Message> {
    Vec::new()
  }

  fn followed_threads(&self) -> Vec<ThreadSummary> {
    Vec::new()
  }

  fn post_message(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>, body: &str) {
    let body = body.trim();
    // Replies need threads from the server; until then only channel messages are kept.
    if body.is_empty() || maybe_thread.is_some() {
      return;
    }
    let sequence = {
      let mut next = self.next_message.borrow_mut();
      *next += 1;
      *next
    };
    let author = self.snapshot.borrow().me.id;
    let message = Message {
      id: id_from_raw::<MessageId>(raw_id(conversation) * 1_000 + sequence),
      conversation,
      author,
      kind: MessageKind::User,
      sent_at: self.now + sequence as i64,
      body: body.to_owned(),
      is_edited: false,
      reactions: Vec::new(),
      reply_count: 0,
      maybe_thread: None,
      maybe_imported: None,
    };
    self
      .local_messages
      .borrow_mut()
      .entry(conversation)
      .or_default()
      .push(message);
  }

  fn toggle_reaction(&self, _conversation: ConversationId, _message: MessageId, _emoji: &str) {}

  fn mark_read(&self, _conversation: ConversationId) {}

  fn set_favorite(&self, conversation: ConversationId, is_favorite: bool) {
    let mut snapshot = self.snapshot.borrow_mut();
    if let Some(found) = snapshot
      .conversations
      .iter_mut()
      .find(|candidate| candidate.id == conversation)
    {
      found.is_favorite = is_favorite;
    }
  }

  fn set_presence(&self, presence: PresenceStatus) {
    self.snapshot.borrow_mut().me.presence = presence;
  }

  fn set_custom_status(&self, maybe_status: Option<CustomStatus>) {
    self.snapshot.borrow_mut().me.maybe_custom_status = maybe_status;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const ME: &str = r#"{"id":"1","username":"alice","display_name":"Alice Owner","role":"owner"}"#;
  const SPACES: &str = r##"{"spaces":[
    {"id":"10","name":"Project A","color":"#2f6fed","join_policy":"open","is_archived":false,"my_role":"admin"},
    {"id":"11","name":"Open Space","join_policy":"open","is_archived":false},
    {"id":"12","name":"Old","join_policy":"open","is_archived":true,"my_role":"member"}]}"##;
  const CONVERSATIONS: &str = r#"{"conversations":[
    {"id":"100","space_id":"10","kind":"channel","name":"general","topic":"Hello","visibility":"public",
     "history_policy":"from_join","is_archived":false,"moderation":{},"contains_guests":false,
     "my_membership":{"role":"owner","notification_level":"all","is_favorite":true},"participants":[]},
    {"id":"101","space_id":"10","kind":"channel","name":"hidden","visibility":"public",
     "history_policy":"from_join","is_archived":false,"moderation":{},"contains_guests":false,"participants":[]},
    {"id":"102","kind":"dm","visibility":"private","history_policy":"from_join","is_archived":false,
     "moderation":{},"contains_guests":false,
     "my_membership":{"role":"member","notification_level":"all","is_favorite":false},
     "participants":[{"id":"1","username":"alice","display_name":"Alice Owner","kind":"member"},{"id":"2","username":"bob","display_name":"Bob Admin","kind":"member"}]},
    {"id":"103","space_id":"10","kind":"channel","name":"leadership","visibility":"private",
     "history_policy":"full","is_archived":false,"moderation":{},"contains_guests":true,
     "my_membership":{"role":"member","notification_level":"none","is_favorite":false},"participants":[]}]}"#;

  fn snapshot() -> Snapshot {
    let me: GetMeResponse = serde_json::from_str(ME).expect("me");
    let spaces: ListSpacesResponse = serde_json::from_str(SPACES).expect("spaces");
    let conversations: ListConversationsResponse =
      serde_json::from_str(CONVERSATIONS).expect("conversations");
    let carol: UserSummary =
      serde_json::from_str(r#"{"id":"3","username":"carol","display_name":"Carol Member","kind":"guest"}"#)
        .expect("user");
    let mut members = HashMap::new();
    members.insert(conversations.conversations[0].id, vec![carol]);
    let presences: ListPresenceResponse =
      serde_json::from_str(r#"{"presences":[{"user_id":"3","state":"away"},{"user_id":"2","state":"dnd"}]}"#)
        .expect("presence");
    build_snapshot(
      &me,
      &spaces.spaces,
      &conversations.conversations,
      &members,
      &presences.presences,
    )
  }

  #[test]
  fn only_joined_and_active_spaces_are_listed() {
    let spaces = snapshot().spaces;
    assert_eq!(spaces.len(), 1);
    assert_eq!(spaces[0].name, "Project A");
    assert_eq!(spaces[0].hue, hue_from_hex("#2f6fed").expect("colour"));
  }

  #[test]
  fn only_joined_conversations_are_listed_and_mapped() {
    let conversations = snapshot().conversations;
    let names: Vec<(&str, ConversationKind, SidebarCategory)> = conversations
      .iter()
      .map(|c| (c.name.as_str(), c.kind, c.category))
      .collect();
    assert_eq!(
      names,
      [
        ("general", ConversationKind::Public, SidebarCategory::Channels),
        (
          "Bob Admin",
          ConversationKind::Direct,
          SidebarCategory::DirectMessages
        ),
        ("leadership", ConversationKind::Private, SidebarCategory::Channels),
      ]
    );
    assert!(conversations[0].is_favorite);
    assert!(conversations[2].is_muted, "notification level none means muted");
    assert_eq!(
      conversations[1].member_ids.len(),
      2,
      "a DM lists the other person and the caller"
    );
  }

  #[test]
  fn users_carry_presence_kind_and_the_caller_is_online() {
    let snapshot = snapshot();
    assert_eq!(snapshot.me.presence, PresenceStatus::Online);
    assert_eq!(snapshot.role, Role::Owner);
    let by_name = |name: &str| {
      snapshot
        .users
        .iter()
        .find(|user| user.display_name == name)
        .cloned()
    };
    let carol = by_name("Carol Member").expect("carol");
    assert_eq!((carol.presence, carol.is_guest), (PresenceStatus::Away, true));
    assert_eq!(
      by_name("Bob Admin").expect("bob").presence,
      PresenceStatus::DoNotDisturb
    );
  }

  #[test]
  fn hues_are_stable_and_in_range() {
    assert_eq!(hue_from_seed("Project A"), hue_from_seed("Project A"));
    assert!(hue_from_seed("anything") < 360);
  }
}
