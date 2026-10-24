//! [`ChatBackend`] of `teamcraft-client-api` over the local store (feature `ui`).
//!
//! The UI's trait is synchronous and reads the *already decrypted* store (crates/teamcraft-client-api
//! backend.rs), which is exactly what the store holds. Writes don't go to the network here:
//! messages and reactions are put into the outbox, everything else becomes a [`CoreCommand`] the
//! app's sync task drains and runs against the server. After either, the sync task flushes.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use teamcraft_client_api::actions::{
  BrowsableChannel, CategoryInfo, ChannelChanges, NewChannel, NewSpace, SpaceChanges, SpaceInvite,
};
use teamcraft_client_api::actions::{
  FileInfo, MessageExtras, OutgoingMessage, PermalinkTarget, SavedItem, ShareLink, ShareLinkOptions,
};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::calls::{
  BackgroundImageInfo, OrgBackgroundInfo, OrgBackgroundRule, OrgBackgrounds,
};
use teamcraft_client_api::model::{
  Bookmark, BookmarkKind, Conversation, ConversationId, ConversationKind, CustomStatus, JoinableSpace,
  Message, MessageKind, PENDING_ID_BIT, PresenceStatus, Reaction, SidebarCategory, Space, SpaceId,
  SystemEvent, ThreadId, ThreadSummary, TimestampMs, User, UserId,
};
use teamcraft_proto::{
  AttachmentId, ClientMessageId, ConversationKind as ConversationKindProto, ConversationResponse,
  ConversationVisibility, Id, JoinPolicy, ListConversationMembersResponse, MessageId, NotificationLevel,
  PresenceState, Snowflake, SpaceResponse, UserKind, UserSummary,
};
use teamcraft_proto::{BackgroundId, OrgBackgroundId};
use teamcraft_proto::{
  CategoryId, ConversationNotificationSettings, CreateSpaceRequest, HistoryPolicy, MoveChannelRequest,
  UpdateConversationRequest, UpdateSpaceRequest, effective_notifications,
};

use teamcraft_client_api::notify::{MentionRules, is_mentioned};

use crate::client::bookmarks_key;
use crate::client::workspace_admin::ChannelOptions;
use crate::clock::Clock;
use crate::error::CoreError;
use crate::notify_rules::{MentionPreferences, load_mention_preferences};
use crate::records::{self, Draft};
use crate::store::{
  BookmarkItem, ConversationEvent, LocalStore, MessageContent, MessageFilter, StoredMessage, Table,
};

/// Requests from the UI that need the server; the sync task drains and runs them.
#[derive(Clone, Debug, PartialEq)]
pub enum CoreCommand {
  /// New messages or reactions are in the outbox.
  FlushOutbox,
  /// The user types in a conversation (sent on the gateway by the sync loop, throttled).
  Typing {
    conversation: ConversationId,
  },
  MarkRead {
    conversation: ConversationId,
  },
  SetFavorite {
    conversation: ConversationId,
    is_favorite: bool,
  },
  SetPresence {
    presence: PresenceStatus,
    /// When a quick do-not-disturb ends.
    maybe_until: Option<TimestampMs>,
  },
  SetCustomStatus {
    maybe_status: Option<CustomStatus>,
  },
  /// Messages a search found only in shared index segments: fetch and decrypt them.
  #[cfg(not(target_arch = "wasm32"))]
  FetchSearchHits {
    hits: Vec<crate::search::SearchMatch>,
  },
  /// Account data (e2ee.md §6.4): applied to the key bag and synced to the user's devices.
  SetSaved {
    conversation: ConversationId,
    client_message_id: ClientMessageId,
    is_saved: bool,
  },
  SetDraft {
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
    body: String,
  },
  SaveSearch {
    id: u64,
    name: String,
    query: String,
  },
  DeleteSavedSearch {
    id: u64,
  },
  /// One of the user's own call backgrounds: encrypted, uploaded and recorded in the key bag.
  AddBackground {
    name: String,
    picture: Vec<u8>,
  },
  DeleteBackground {
    id: BackgroundId,
  },
  /// Organization backgrounds (owners; each change is a new signed policy version).
  AddOrgBackground {
    name: String,
    picture: Vec<u8>,
  },
  DeleteOrgBackground {
    id: OrgBackgroundId,
  },
  SetOrgBackgroundRule {
    rule: OrgBackgroundRule,
  },
  /// The server's pin flag (the encrypted pin message is already in the outbox).
  SetServerPin {
    conversation: ConversationId,
    target: ClientMessageId,
    is_pinned: bool,
  },
  Acknowledge {
    conversation: ConversationId,
    message: MessageId,
  },
  MarkUnread {
    conversation: ConversationId,
    from: MessageId,
  },
  /// The activity feed was read up to the entry sent at this time.
  MarkActivityRead {
    up_to_ms: i64,
  },
  /// Presence and last seen of these people.
  RefreshPresence {
    users: Vec<UserId>,
  },
  RefreshPrivacy,
  SetLastSeenVisibility {
    visibility: teamcraft_proto::LastSeenVisibility,
  },
  RefreshCallHistory,
  /// This user's own device list (the devices settings).
  RefreshOwnDevices,
  /// Signs out one of this user's other devices.
  RevokeOwnDevice {
    device: teamcraft_proto::DeviceId,
  },
  /// Files of outbox messages are waiting for upload.
  UploadFiles,
  /// Outbox messages wait for their link previews.
  GeneratePreviews,
  DownloadFile {
    conversation: ConversationId,
    attachment: AttachmentId,
  },
  CreateShareLink {
    conversation: ConversationId,
    attachment: AttachmentId,
    maybe_expires_at: Option<i64>,
    maybe_max_downloads: Option<u32>,
    maybe_password: Option<RedactedText>,
  },
  CreateSpace {
    request: Box<CreateSpaceRequest>,
  },
  UpdateSpace {
    space: SpaceId,
    request: Box<UpdateSpaceRequest>,
  },
  CreateChannelWith {
    space: SpaceId,
    options: Box<ChannelOptions>,
  },
  UpdateChannel {
    conversation: ConversationId,
    request: Box<UpdateConversationRequest>,
  },
  MoveChannel {
    conversation: ConversationId,
    request: MoveChannelRequest,
  },
  RefreshChannelBrowser {
    space: SpaceId,
  },
  JoinChannel {
    conversation: ConversationId,
  },
  JoinSpace {
    space: SpaceId,
  },
  LeaveChannel {
    conversation: ConversationId,
  },
  RefreshCategories {
    space: SpaceId,
  },
  CreateCategory {
    space: SpaceId,
    name: String,
  },
  RenameCategory {
    space: SpaceId,
    category: CategoryId,
    name: String,
  },
  DeleteCategory {
    space: SpaceId,
    category: CategoryId,
  },
  AddMemberWithHistory {
    conversation: ConversationId,
    user: UserId,
    history: HistoryPolicy,
  },
  RemoveMember {
    conversation: ConversationId,
    user: UserId,
  },
  InviteToSpace {
    space: SpaceId,
    email: String,
    is_guest: bool,
    channels: Vec<ConversationId>,
    maybe_expires_at: Option<i64>,
  },
  /// "Jump to date": load the history from `at_ms` on and note that the request is answered.
  LoadHistoryAt {
    conversation: ConversationId,
    at_ms: i64,
  },
}

/// Where the engine notes (in the sync state) which "jump to date" request it answered last.
pub(crate) fn history_jump_key(conversation: ConversationId) -> String {
  format!("history-jump/{conversation}")
}

/// Text that must not show up in logs (a share link password); `Debug` prints `[redacted]`.
#[derive(Clone, PartialEq, Eq)]
pub struct RedactedText(pub String);

impl std::fmt::Debug for RedactedText {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("[redacted]")
  }
}

pub struct CoreChatBackend<S> {
  store: S,
  me: UserId,
  clock: Box<dyn Clock>,
  commands: Rc<RefCell<Vec<CoreCommand>>>,
  /// Who types where; shared with the client when [`Self::with_typing`] was given its state.
  typing: crate::TypingState,
  /// When reads last asked the server for something, so the UI's repeated reads don't flood it.
  pub(crate) requests: RefCell<crate::ui_people::RequestLog>,
  #[cfg(not(target_arch = "wasm32"))]
  pub(crate) search: RefCell<crate::ui_search::UiSearchState>,
}

impl<S: LocalStore> CoreChatBackend<S> {
  /// `store` is shared with the sync engine (e.g. an `Arc`); `me` is the signed-in user.
  pub fn new(store: S, me: UserId, clock: Box<dyn Clock>) -> Self {
    Self {
      store,
      me,
      clock,
      commands: Rc::new(RefCell::new(Vec::new())),
      typing: crate::TypingState::default(),
      requests: RefCell::default(),
      #[cfg(not(target_arch = "wasm32"))]
      search: RefCell::new(Default::default()),
    }
  }

  /// Reads typing indicators from the client's state ([`crate::Client::typing_state`]).
  #[must_use]
  pub fn with_typing(mut self, typing: crate::TypingState) -> Self {
    self.typing = typing;
    self
  }

  /// Takes the queued commands (the sync task runs them).
  pub fn drain_commands(&self) -> Vec<CoreCommand> {
    std::mem::take(&mut *self.commands.borrow_mut())
  }

  pub(crate) fn me_id(&self) -> UserId {
    self.me
  }

  pub(crate) fn now_ms(&self) -> i64 {
    self.clock.now_ms()
  }

  pub(crate) fn store_ref(&self) -> &S {
    &self.store
  }

  #[cfg(not(target_arch = "wasm32"))]
  pub(crate) fn queue_command(&self, command: CoreCommand) {
    self.command(command);
  }

  pub(crate) fn message_view(&self, stored: &StoredMessage, all: &[StoredMessage]) -> Message {
    self.to_message(stored, all)
  }

  pub(crate) fn command(&self, command: CoreCommand) {
    self.commands.borrow_mut().push(command);
  }

  /// Reads that fail (corrupt store) are logged and show as empty: the UI has no error channel here.
  pub(crate) fn read<T: Default>(&self, result: Result<T, CoreError>) -> T {
    result.unwrap_or_else(|error| {
      tracing::error!(%error, "local store read failed");
      T::default()
    })
  }

  pub(crate) fn summaries(&self) -> BTreeMap<UserId, UserSummary> {
    let list: Vec<UserSummary> = self.read(records::list_json(&self.store, Table::Users));
    list.into_iter().map(|user| (user.id, user)).collect()
  }

  pub(crate) fn conversations_raw(&self) -> Vec<ConversationResponse> {
    self.read(records::list_json(&self.store, Table::Conversations))
  }

  fn members(&self, conversation: ConversationId) -> Vec<UserId> {
    let maybe: Option<ListConversationMembersResponse> = self.read(records::get_json(
      &self.store,
      Table::Members,
      &conversation.to_string(),
    ));
    maybe
      .map(|list| list.members.into_iter().map(|member| member.user.id).collect())
      .unwrap_or_default()
  }

  fn to_user(&self, summary: &UserSummary) -> User {
    let maybe_presence: Option<PresenceState> = self.read(records::get_json(
      &self.store,
      Table::Presence,
      &summary.id.to_string(),
    ));
    User {
      id: summary.id,
      username: summary.maybe_username.clone().unwrap_or_default(),
      display_name: summary.display_name.clone(),
      presence: maybe_presence.map_or(PresenceStatus::Offline, presence_status),
      maybe_custom_status: None,
      avatar_hue: hue(summary.id),
      is_guest: summary.kind == UserKind::Guest,
      is_bot: false,
      maybe_organization: summary.maybe_organization.clone(),
    }
  }

  pub(crate) fn to_conversation(
    &self,
    response: &ConversationResponse,
    users: &BTreeMap<UserId, UserSummary>,
    preferences: &MentionPreferences,
  ) -> Conversation {
    let member_ids = self.members(response.id);
    let kind = match (&response.kind, &response.visibility) {
      (ConversationKindProto::Dm, _) => ConversationKind::Direct,
      (ConversationKindProto::Group, _) => ConversationKind::Group,
      (_, _) if response.is_integration => ConversationKind::Integration,
      (_, ConversationVisibility::Public) => ConversationKind::Public,
      _ => ConversationKind::Private,
    };
    let is_favorite = response
      .maybe_my_membership
      .as_ref()
      .is_some_and(|membership| membership.is_favorite);
    let category = if is_favorite {
      SidebarCategory::Favorites
    } else if kind.is_direct_like() {
      SidebarCategory::DirectMessages
    } else {
      SidebarCategory::Channels
    };
    let name = response.maybe_name.clone().unwrap_or_else(|| {
      // DMs and group DMs are named after the other people.
      let mut others: Vec<String> = member_ids
        .iter()
        .filter(|user| **user != self.me)
        .filter_map(|user| users.get(user).map(|summary| summary.display_name.clone()))
        .collect();
      others.sort();
      others.join(", ")
    });
    let unread = self.unread_messages(response);
    Conversation {
      id: response.id,
      maybe_space: response.maybe_space_id,
      kind,
      category,
      name,
      topic: response.maybe_topic.clone().unwrap_or_default(),
      member_ids,
      unread_count: u32::try_from(unread.len()).unwrap_or(u32::MAX),
      mention_count: self.mention_count(&unread, users.get(&self.me), preferences),
      is_favorite,
      is_muted: self.is_muted(response),
      can_post: true,
      bookmarks: self.bookmarks_of(response.id),
    }
  }

  /// Whether notifications of the conversation are off now: its level is none or a mute time has
  /// not passed yet, with the space's setting standing in for a conversation the member never
  /// configured.
  fn is_muted(&self, response: &ConversationResponse) -> bool {
    let Some(membership) = &response.maybe_my_membership else {
      return false;
    };
    let own = ConversationNotificationSettings {
      level: membership.notification_level.clone(),
      maybe_muted_until: membership.maybe_muted_until,
      is_inherited: membership.is_notification_inherited,
    };
    let maybe_space: Option<SpaceResponse> = response
      .maybe_space_id
      .and_then(|space| self.read(records::get_json(&self.store, Table::Spaces, &space.to_string())));
    let effective = effective_notifications(
      &own,
      maybe_space
        .as_ref()
        .and_then(|space| space.maybe_my_notifications.as_ref()),
    );
    effective.level == NotificationLevel::None
      || effective
        .maybe_muted_until
        .is_some_and(|until| until.0 > self.now_ms())
  }

  /// The bookmarks bar of a conversation, as the newest applied `Bookmarks` message set it.
  fn bookmarks_of(&self, conversation: ConversationId) -> Vec<Bookmark> {
    let items: Vec<BookmarkItem> =
      records::get_json(&self.store, Table::Account, &bookmarks_key(conversation))
        .ok()
        .flatten()
        .unwrap_or_default();
    items
      .into_iter()
      .map(|item| {
        let maybe_message = item.maybe_message.and_then(|client_id| {
          self
            .store
            .message(conversation, client_id)
            .ok()
            .flatten()
            .map(|stored| ui_message_id(&stored))
        });
        Bookmark {
          emoji: item.emoji,
          title: item.title,
          kind: if maybe_message.is_some() {
            BookmarkKind::File
          } else {
            BookmarkKind::Link
          },
          url: item.url,
          maybe_message,
        }
      })
      .collect()
  }

  /// How many of the unread messages mention the user: an explicit mention, or, decided on the
  /// decrypted text, `@username`, `@channel`/`@here`/`@all`, the first name or a keyword
  /// (docs/notifications.md). Derived from the unread set, so a read marker resets it and a replayed
  /// event can't raise it twice. Muted conversations count too.
  fn mention_count(
    &self,
    unread: &[StoredMessage],
    maybe_me: Option<&UserSummary>,
    preferences: &MentionPreferences,
  ) -> u32 {
    let rules = MentionRules {
      username: maybe_me
        .and_then(|me| me.maybe_username.as_deref())
        .unwrap_or_default(),
      display_name: maybe_me.map_or("", |me| me.display_name.as_str()),
      first_name_is_mention: preferences.first_name_is_mention,
      channel_mentions_notify: preferences.channel_mentions_notify,
      keywords: &preferences.keywords,
    };
    let count = unread
      .iter()
      .filter(|message| message.sender_user_id != self.me)
      .filter(|message| match &message.content {
        MessageContent::Text { body, mentions, .. } => {
          mentions.contains(&self.me) || is_mentioned(body, &rules)
        },
        _ => false,
      })
      .count();
    u32::try_from(count).unwrap_or(u32::MAX)
  }

  /// The saved mention preferences, read once per list of conversations.
  pub(crate) fn mention_preferences(&self) -> MentionPreferences {
    load_mention_preferences(&self.store).unwrap_or_else(|error| {
      tracing::warn!(%error, "mention preferences unreadable, using defaults");
      MentionPreferences::default()
    })
  }

  /// Top-level messages the user hasn't read: others' messages after the server's read position,
  /// or everything from the user's own "mark unread" on.
  pub(crate) fn unread_messages(&self, response: &ConversationResponse) -> Vec<StoredMessage> {
    let maybe_mark = self
      .read(crate::client::stored_account_data(&self.store))
      .unread_mark(response.id);
    let maybe_last_read = response
      .maybe_my_membership
      .as_ref()
      .and_then(|membership| membership.maybe_last_read_message_id);
    let messages = self.read(
      self
        .store
        .messages(response.id, MessageFilter::TopLevel)
        .map_err(CoreError::from),
    );
    messages
      .into_iter()
      .filter(
        |message| match (message.maybe_server_id, maybe_mark, maybe_last_read) {
          (Some(id), Some(mark), _) => id >= mark,
          (Some(id), None, Some(last_read)) => message.sender_user_id != self.me && id > last_read,
          (Some(_), None, None) => message.sender_user_id != self.me,
          (None, _, _) => false,
        },
      )
      .collect()
  }

  pub(crate) fn to_message(&self, stored: &StoredMessage, all: &[StoredMessage]) -> Message {
    let id = ui_message_id(stored);
    let body = match &stored.content {
      // Text sent with files names them when it has no text of its own (search: `has:file`).
      MessageContent::Text { body, meta, .. } if body.is_empty() && !meta.attachments.is_empty() => meta
        .attachments
        .iter()
        .map(|attachment| format!("📎 {}", attachment.file_name))
        .collect::<Vec<_>>()
        .join(" "),
      MessageContent::Text { body, .. } => body.clone(),
      // The UI recognizes files by their name in the body (search filter `has:file`).
      MessageContent::File { attachment } => format!("📎 {}", attachment.file_name),
      MessageContent::Reaction { .. }
      | MessageContent::Edit { .. }
      | MessageContent::Delete { .. }
      | MessageContent::Pin { .. }
      | MessageContent::Bookmarks { .. }
      | MessageContent::Event { .. }
      | MessageContent::Deleted
      | MessageContent::IndexSegment { .. }
      | MessageContent::Unknown => String::new(),
    };
    let (kind, author) = match &stored.content {
      MessageContent::Event { event } => system_view(event, stored.sender_user_id),
      _ => (MessageKind::User, stored.sender_user_id),
    };
    let reply_count = stored.maybe_server_id.map_or(0, |server_id| {
      let thread = ThreadId::from_snowflake(server_id.snowflake());
      all
        .iter()
        .filter(|message| {
          message.maybe_thread_id == Some(thread) && !message.is_hidden() && !is_deleted(message)
        })
        .count()
    });
    Message {
      id,
      conversation: stored.conversation_id,
      author,
      kind,
      sent_at: stored.sent_at_ms,
      body,
      is_edited: stored.is_edited,
      reactions: reactions_of(stored.client_message_id, all, self.me),
      reply_count: u32::try_from(reply_count).unwrap_or(u32::MAX),
      maybe_thread: stored.maybe_thread_id,
    }
  }

  pub(crate) fn all_messages(&self, conversation: ConversationId) -> Vec<StoredMessage> {
    self.read(
      self
        .store
        .messages(conversation, MessageFilter::All)
        .map_err(CoreError::from),
    )
  }

  /// `@username` mentions of conversation members in a message body.
  pub(crate) fn mentions_in(&self, conversation: ConversationId, body: &str) -> Vec<UserId> {
    let users = self.summaries();
    let members = self.members(conversation);
    let mut mentions: Vec<UserId> = body
      .split(|character: char| {
        character.is_whitespace() || matches!(character, ',' | '.' | ':' | ';' | '!' | '?')
      })
      .filter_map(|word| word.strip_prefix('@'))
      .filter_map(|name| {
        members.iter().copied().find(|member| {
          users
            .get(member)
            .and_then(|summary| summary.maybe_username.as_deref())
            .is_some_and(|username| username.eq_ignore_ascii_case(name))
        })
      })
      .collect();
    mentions.sort();
    mentions.dedup();
    mentions
  }

  /// Writes a message to the outbox; returns its ID (or `None` if the store failed, logged).
  pub(crate) fn enqueue(
    &self,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
    content: MessageContent,
  ) -> Option<ClientMessageId> {
    let draft = Draft {
      conversation_id: conversation,
      maybe_thread_id: maybe_thread,
      content,
    };
    match records::enqueue_message(&self.store, self.me, self.clock.now_ms(), draft) {
      Ok(id) => {
        self.command(CoreCommand::FlushOutbox);
        Some(id)
      },
      Err(error) => {
        tracing::error!(%error, "could not write message to the outbox");
        None
      },
    }
  }
}

impl<S: LocalStore> ChatBackend for CoreChatBackend<S> {
  fn me(&self) -> User {
    let users = self.summaries();
    users
      .get(&self.me)
      .map(|summary| self.to_user(summary))
      .unwrap_or_else(|| User {
        id: self.me,
        username: String::new(),
        display_name: String::new(),
        presence: PresenceStatus::Online,
        maybe_custom_status: None,
        avatar_hue: hue(self.me),
        is_guest: false,
        is_bot: false,
        maybe_organization: None,
      })
  }

  fn user(&self, id: UserId) -> Option<User> {
    self.summaries().get(&id).map(|summary| self.to_user(summary))
  }

  fn users(&self) -> Vec<User> {
    self
      .summaries()
      .values()
      .map(|summary| self.to_user(summary))
      .collect()
  }

  fn spaces(&self) -> Vec<Space> {
    let spaces: Vec<SpaceResponse> = self.read(records::list_json(&self.store, Table::Spaces));
    // A channel moved into a space keeps its members, who may not belong to the space itself.
    let conversations = self.conversations_raw();
    spaces
      .into_iter()
      .filter(|space| space.maybe_my_role.is_some() || has_channel_in(&conversations, space.id))
      .map(|space| Space {
        id: space.id,
        name: space.name,
        hue: hue(space.id),
        unread_count: 0,
        mention_count: 0,
      })
      .collect()
  }

  fn space_conversations(&self, space: SpaceId) -> Vec<Conversation> {
    let users = self.summaries();
    let preferences = self.mention_preferences();
    self
      .conversations_raw()
      .iter()
      .filter(|conversation| conversation.maybe_space_id == Some(space))
      .map(|conversation| self.to_conversation(conversation, &users, &preferences))
      .collect()
  }

  fn direct_conversations(&self) -> Vec<Conversation> {
    let users = self.summaries();
    let preferences = self.mention_preferences();
    self
      .conversations_raw()
      .iter()
      .filter(|conversation| {
        matches!(
          conversation.kind,
          ConversationKindProto::Dm | ConversationKindProto::Group
        )
      })
      .map(|conversation| self.to_conversation(conversation, &users, &preferences))
      .collect()
  }

  fn conversation(&self, id: ConversationId) -> Option<Conversation> {
    let users = self.summaries();
    let preferences = self.mention_preferences();
    self
      .conversations_raw()
      .iter()
      .find(|conversation| conversation.id == id)
      .map(|conversation| self.to_conversation(conversation, &users, &preferences))
  }

  fn message_count(&self, conversation: ConversationId) -> usize {
    self
      .read(
        self
          .store
          .messages(conversation, MessageFilter::TopLevel)
          .map_err(CoreError::from),
      )
      .iter()
      .filter(|message| !is_deleted(message))
      .count()
  }

  fn last_activity(&self, conversation: ConversationId) -> Option<TimestampMs> {
    self
      .read(
        self
          .store
          .messages(conversation, MessageFilter::TopLevel)
          .map_err(CoreError::from),
      )
      .iter()
      .filter(|message| !is_deleted(message))
      .map(|message| message.sent_at_ms)
      .max()
  }

  fn messages(&self, conversation: ConversationId, start: usize, len: usize) -> Vec<Message> {
    let all = self.all_messages(conversation);
    let top_level: Vec<&StoredMessage> = all
      .iter()
      .filter(|message| {
        (message.maybe_thread_id.is_none() || message.is_also_in_channel())
          && !message.is_hidden()
          && !is_deleted(message)
      })
      .collect();
    let end = start.saturating_add(len).min(top_level.len());
    let start = start.min(end);
    top_level[start..end]
      .iter()
      .map(|message| self.to_message(message, &all))
      .collect()
  }

  fn thread(&self, conversation: ConversationId, thread: ThreadId) -> Vec<Message> {
    let all = self.all_messages(conversation);
    let root_id = MessageId::from_snowflake(thread.snowflake());
    let root = all
      .iter()
      .filter(|message| message.maybe_server_id == Some(root_id));
    let replies = all.iter().filter(|message| {
      message.maybe_thread_id == Some(thread) && !message.is_hidden() && !is_deleted(message)
    });
    root
      .chain(replies)
      .map(|message| self.to_message(message, &all))
      .collect()
  }

  fn followed_threads(&self) -> Vec<ThreadSummary> {
    // TODO(threads): following threads isn't tracked by the core yet.
    Vec::new()
  }

  fn post_message(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>, body: &str) {
    let mentions = self.mentions_in(conversation, body);
    self.enqueue(
      conversation,
      maybe_thread,
      MessageContent::Text {
        body: body.to_string(),
        mentions,
        meta: Default::default(),
      },
    );
  }

  fn toggle_reaction(&self, conversation: ConversationId, message: MessageId, emoji: &str) {
    let all = self.all_messages(conversation);
    let Some(target) = all.iter().find(|stored| ui_message_id(stored) == message) else {
      return;
    };
    let is_reacted = reactions_of(target.client_message_id, &all, self.me)
      .iter()
      .any(|reaction| reaction.emoji == emoji && reaction.reacted_by_me);
    let content = MessageContent::Reaction {
      target: target.client_message_id,
      emoji: emoji.to_string(),
      is_added: !is_reacted,
    };
    self.enqueue(conversation, None, content);
  }

  fn mark_read(&self, conversation: ConversationId) {
    self.command(CoreCommand::MarkRead { conversation });
  }

  fn set_favorite(&self, conversation: ConversationId, is_favorite: bool) {
    self.command(CoreCommand::SetFavorite {
      conversation,
      is_favorite,
    });
  }

  fn set_presence(&self, presence: PresenceStatus) {
    self.set_presence_until(presence, None);
  }

  fn set_presence_until(&self, presence: PresenceStatus, maybe_until: Option<TimestampMs>) {
    self.command(CoreCommand::SetPresence {
      presence,
      maybe_until,
    });
  }

  fn set_custom_status(&self, maybe_status: Option<CustomStatus>) {
    self.command(CoreCommand::SetCustomStatus { maybe_status });
  }

  fn set_saved(&self, conversation: ConversationId, message: MessageId, is_saved: bool) {
    self.ui_set_saved(conversation, message, is_saved);
  }

  fn saved_items(&self) -> Vec<SavedItem> {
    self.ui_saved_items()
  }

  fn background_images(&self) -> Vec<BackgroundImageInfo> {
    self
      .read(crate::client::stored_account_data(&self.store))
      .backgrounds()
      .into_iter()
      .map(|(id, entry)| BackgroundImageInfo {
        id,
        name: entry.name.clone(),
        size_bytes: entry.size_bytes,
      })
      .collect()
  }

  fn add_background_image(&self, name: &str, picture: Vec<u8>) {
    self.command(CoreCommand::AddBackground {
      name: name.to_string(),
      picture,
    });
  }

  fn delete_background_image(&self, id: BackgroundId) {
    self.command(CoreCommand::DeleteBackground { id });
  }

  fn org_backgrounds(&self) -> OrgBackgrounds {
    let state: crate::workspace::WorkspaceState = self.read(
      crate::records::get_json(
        &self.store,
        crate::store::Table::Workspace,
        crate::client::WORKSPACE_KEY,
      )
      .map(Option::unwrap_or_default),
    );
    let policy = state.policy.call_backgrounds;
    let is_guest = self.me().is_guest;
    OrgBackgrounds {
      applies_to_me: !(is_guest && policy.guests_exempt),
      mode: policy.mode,
      guests_exempt: policy.guests_exempt,
      images: policy
        .images
        .into_iter()
        .map(|image| OrgBackgroundInfo {
          id: image.id,
          name: image.name,
        })
        .collect(),
      maybe_default: policy.maybe_default,
    }
  }

  fn add_org_background(&self, name: &str, picture: Vec<u8>) {
    self.command(CoreCommand::AddOrgBackground {
      name: name.to_string(),
      picture,
    });
  }

  fn delete_org_background(&self, id: OrgBackgroundId) {
    self.command(CoreCommand::DeleteOrgBackground { id });
  }

  fn set_org_background_rule(&self, rule: OrgBackgroundRule) {
    self.command(CoreCommand::SetOrgBackgroundRule { rule });
  }

  fn draft(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>) -> Option<String> {
    self.ui_draft(conversation, maybe_thread)
  }

  fn set_draft(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>, body: &str) {
    self.command(CoreCommand::SetDraft {
      conversation,
      maybe_thread,
      body: body.to_string(),
    });
  }

  fn send_message(&self, message: OutgoingMessage) {
    self.ui_send_message(message);
  }

  fn message_extras(&self, conversation: ConversationId, messages: &[MessageId]) -> Vec<MessageExtras> {
    self.ui_message_extras(conversation, messages)
  }

  fn edit_message(&self, conversation: ConversationId, message: MessageId, body: &str) {
    self.ui_edit_message(conversation, message, body);
  }

  fn delete_message(&self, conversation: ConversationId, message: MessageId) {
    self.ui_delete_message(conversation, message);
  }

  fn forward_message(&self, from: ConversationId, message: MessageId, to: ConversationId, comment: &str) {
    self.ui_forward_message(from, message, to, comment);
  }

  fn set_pinned(&self, conversation: ConversationId, message: MessageId, is_pinned: bool) {
    self.ui_set_pinned(conversation, message, is_pinned);
  }

  fn pinned_messages(&self, conversation: ConversationId) -> Vec<Message> {
    self.ui_pinned_messages(conversation)
  }

  fn permalink(&self, conversation: ConversationId, message: MessageId) -> Option<String> {
    self.ui_permalink(conversation, message)
  }

  fn resolve_permalink(&self, link: &str) -> Option<PermalinkTarget> {
    self.ui_resolve_permalink(link)
  }

  fn activity(&self) -> teamcraft_client_api::activity::ActivityFeed {
    self.ui_activity()
  }

  fn mark_activity_read(&self) {
    self.ui_mark_activity_read();
  }

  fn last_seen(&self, user: UserId) -> Option<teamcraft_client_api::model::TimestampMs> {
    self.ui_last_seen(user)
  }

  fn privacy(&self) -> teamcraft_client_api::privacy::PrivacySettings {
    self.ui_privacy()
  }

  fn set_last_seen_visibility(&self, visibility: teamcraft_client_api::privacy::LastSeenVisibility) {
    self.command(CoreCommand::SetLastSeenVisibility { visibility });
  }

  fn call_history(&self) -> Vec<teamcraft_client_api::calls::CallHistoryItem> {
    self.ui_call_history()
  }

  fn refresh_call_history(&self) {
    self.ui_refresh_call_history();
  }

  fn own_devices(&self) -> Vec<teamcraft_client_api::admin::DeviceInfo> {
    self.ui_own_devices()
  }

  fn refresh_own_devices(&self) {
    self.ui_refresh_own_devices();
  }

  fn revoke_own_device(&self, device: teamcraft_proto::DeviceId) {
    self.command(CoreCommand::RevokeOwnDevice { device });
  }

  fn mark_unread(&self, conversation: ConversationId, from: MessageId) {
    self.command(CoreCommand::MarkUnread { conversation, from });
  }

  fn first_unread(&self, conversation: ConversationId) -> Option<MessageId> {
    self.ui_first_unread(conversation)
  }

  fn is_waiting_for_device(&self, conversation: ConversationId) -> bool {
    crate::client::is_waiting_for_device(&self.store, conversation).unwrap_or(false)
  }

  fn typing_users(&self, conversation: ConversationId) -> Vec<UserId> {
    self.typing.users(conversation, self.me, self.clock.now_ms())
  }

  fn notify_typing(&self, conversation: ConversationId) {
    self.command(CoreCommand::Typing { conversation });
  }

  fn retry_send(&self, conversation: ConversationId, message: MessageId) {
    self.ui_retry_send(conversation, message);
  }

  fn discard_unsent(&self, conversation: ConversationId, message: MessageId) {
    self.ui_discard_unsent(conversation, message);
  }

  fn acknowledge(&self, conversation: ConversationId, message: MessageId) {
    self.command(CoreCommand::Acknowledge {
      conversation,
      message,
    });
  }
  fn conversation_files(&self, conversation: ConversationId) -> Vec<FileInfo> {
    self.ui_conversation_files(conversation)
  }

  fn download_file(&self, conversation: ConversationId, message: MessageId) {
    self.ui_download_file(conversation, message);
  }

  fn create_share_link(&self, conversation: ConversationId, message: MessageId, options: ShareLinkOptions) {
    self.ui_create_share_link(conversation, message, options);
  }

  fn share_links(&self, conversation: ConversationId, message: MessageId) -> Vec<ShareLink> {
    self.ui_share_links(conversation, message)
  }

  fn max_upload_bytes(&self) -> Option<u64> {
    self.ui_max_upload_bytes()
  }

  fn create_space(&self, space: NewSpace) {
    self.ui_create_space(space);
  }

  fn update_space(&self, space: SpaceId, changes: SpaceChanges) {
    self.ui_update_space(space, changes);
  }

  fn create_channel_with(&self, channel: NewChannel) {
    self.ui_create_channel_with(channel);
  }

  fn update_channel(&self, conversation: ConversationId, changes: ChannelChanges) {
    self.ui_update_channel(conversation, changes);
  }

  fn browse_channels(&self, space: SpaceId) -> Vec<BrowsableChannel> {
    self.ui_browse_channels(space)
  }

  fn request_history_at(&self, conversation: ConversationId, at: i64) {
    self.command(CoreCommand::LoadHistoryAt {
      conversation,
      at_ms: at,
    });
  }

  fn is_history_loaded(&self, conversation: ConversationId, at: i64) -> bool {
    let key = history_jump_key(conversation);
    let answered: Option<i64> = self.read(records::get_json(&self.store, Table::SyncState, &key));
    answered == Some(at)
  }

  fn join_channel(&self, conversation: ConversationId) {
    self.command(CoreCommand::JoinChannel { conversation });
  }

  fn joinable_spaces(&self) -> Vec<JoinableSpace> {
    let spaces: Vec<SpaceResponse> = self.read(records::list_json(&self.store, Table::Spaces));
    let conversations = self.conversations_raw();
    spaces
      .into_iter()
      .filter(|space| {
        space.maybe_my_role.is_none()
          && space.join_policy == JoinPolicy::Open
          && !space.is_archived
          && !has_channel_in(&conversations, space.id)
      })
      .map(|space| JoinableSpace {
        id: space.id,
        name: space.name,
        maybe_description: space.maybe_description,
      })
      .collect()
  }

  fn join_space(&self, space: SpaceId) {
    self.command(CoreCommand::JoinSpace { space });
  }

  fn leave_channel(&self, conversation: ConversationId) {
    self.command(CoreCommand::LeaveChannel { conversation });
  }

  fn categories(&self, space: SpaceId) -> Vec<CategoryInfo> {
    self.ui_categories(space)
  }

  fn create_category(&self, space: SpaceId, name: &str) {
    self.command(CoreCommand::CreateCategory {
      space,
      name: name.to_string(),
    });
  }

  fn rename_category(&self, space: SpaceId, category: CategoryId, name: &str) {
    self.command(CoreCommand::RenameCategory {
      space,
      category,
      name: name.to_string(),
    });
  }

  fn delete_category(&self, space: SpaceId, category: CategoryId) {
    self.command(CoreCommand::DeleteCategory { space, category });
  }

  fn add_member_with_history(&self, conversation: ConversationId, user: UserId, history: HistoryPolicy) {
    self.command(CoreCommand::AddMemberWithHistory {
      conversation,
      user,
      history,
    });
  }

  fn set_bookmarks(&self, conversation: ConversationId, bookmarks: Vec<Bookmark>) {
    self.ui_set_bookmarks(conversation, bookmarks);
  }

  fn remove_member(&self, conversation: ConversationId, user: UserId) {
    self.command(CoreCommand::RemoveMember { conversation, user });
  }

  fn invite_to_space(&self, invite: SpaceInvite) {
    self.command(CoreCommand::InviteToSpace {
      space: invite.space,
      email: invite.email,
      is_guest: invite.is_guest,
      channels: invite.channels,
      maybe_expires_at: invite.maybe_expires_at,
    });
  }

  fn has_external_people(&self, conversation: ConversationId) -> bool {
    self
      .conversations_raw()
      .iter()
      .any(|response| response.id == conversation && response.contains_guests)
  }
}

/// How a conversation event shows: the UI's system events where one fits ("X joined" with the added
/// person as author); other events stay user messages without text, and the UI reads them from
/// `MessageExtras::maybe_workspace_event`.
fn system_view(event: &ConversationEvent, sender: UserId) -> (MessageKind, UserId) {
  match event {
    ConversationEvent::Joined => (MessageKind::System(SystemEvent::Joined), sender),
    ConversationEvent::Left => (MessageKind::System(SystemEvent::Left), sender),
    ConversationEvent::MemberAdded { user, .. } => (MessageKind::System(SystemEvent::Joined), *user),
    ConversationEvent::MemberRemoved { user } => (MessageKind::System(SystemEvent::Left), *user),
    ConversationEvent::Renamed { name } => (
      MessageKind::System(SystemEvent::Renamed { name: name.clone() }),
      sender,
    ),
    ConversationEvent::TopicChanged { topic } => (
      MessageKind::System(SystemEvent::TopicChanged { topic: topic.clone() }),
      sender,
    ),
    ConversationEvent::Archived
    | ConversationEvent::Unarchived
    | ConversationEvent::MadePrivate
    | ConversationEvent::MadePublic
    | ConversationEvent::Moved { .. }
    | ConversationEvent::Unknown => (MessageKind::User, sender),
  }
}

/// Whether the user is in a channel of the space.
fn has_channel_in(conversations: &[ConversationResponse], space: SpaceId) -> bool {
  conversations.iter().any(|conversation| {
    conversation.maybe_space_id == Some(space) && conversation.maybe_my_membership.is_some()
  })
}

fn is_deleted(message: &StoredMessage) -> bool {
  matches!(message.content, MessageContent::Deleted)
}

/// Aggregates reaction messages: the latest add/remove per (user, emoji) wins.
fn reactions_of(target: ClientMessageId, all: &[StoredMessage], me: UserId) -> Vec<Reaction> {
  let mut latest: HashMap<(UserId, String), bool> = HashMap::new();
  for message in all {
    if let MessageContent::Reaction {
      target: reacted,
      emoji,
      is_added,
    } = &message.content
      && *reacted == target
    {
      latest.insert((message.sender_user_id, emoji.clone()), *is_added);
    }
  }
  let mut by_emoji: BTreeMap<String, Reaction> = BTreeMap::new();
  for ((user, emoji), is_added) in latest {
    if !is_added {
      continue;
    }
    let entry = by_emoji.entry(emoji.clone()).or_insert(Reaction {
      emoji,
      count: 0,
      reacted_by_me: false,
    });
    entry.count += 1;
    entry.reacted_by_me |= user == me;
  }
  by_emoji.into_values().collect()
}

/// The ID the UI uses: the server's, or a stable stand-in for outbox messages.
pub(crate) fn ui_message_id(message: &StoredMessage) -> MessageId {
  if let Some(id) = message.maybe_server_id {
    return id;
  }
  let mut bytes = [0u8; 8];
  bytes.copy_from_slice(&message.client_message_id.as_bytes()[..8]);
  let raw = (i64::from_be_bytes(bytes) & (PENDING_ID_BIT - 1)) | PENDING_ID_BIT;
  // Invariant: `raw` is positive (bit 63 cleared above), so `Snowflake::new` accepts it.
  Snowflake::new(raw)
    .map(MessageId::from_snowflake)
    .unwrap_or_else(|_| unreachable!("positive by construction"))
}

fn presence_status(state: PresenceState) -> PresenceStatus {
  match state {
    PresenceState::Online => PresenceStatus::Online,
    PresenceState::Away => PresenceStatus::Away,
    PresenceState::DoNotDisturb => PresenceStatus::DoNotDisturb,
    PresenceState::Offline | PresenceState::Unknown(_) => PresenceStatus::Offline,
  }
}

/// Avatar colour derived from the ID, stable across devices.
fn hue(id: impl Id) -> u16 {
  u16::try_from(id.snowflake().get().rem_euclid(360)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::Snowflake;

  use super::*;
  use crate::clock::ManualClock;
  use crate::store::MemoryStore;

  #[test]
  fn posting_and_reacting_go_through_the_outbox() {
    let store = std::sync::Arc::new(MemoryStore::new());
    let me = id::<UserId>(1);
    let conversation = id::<ConversationId>(10);
    let backend = CoreChatBackend::new(store.clone(), me, Box::new(ManualClock::new(1_000)));

    backend.post_message(conversation, None, "hello");
    assert_eq!(backend.message_count(conversation), 1);
    let posted = backend.messages(conversation, 0, 10).remove(0);
    assert_eq!(posted.body, "hello");
    assert!(posted.id.snowflake().get() & PENDING_ID_BIT != 0);

    backend.toggle_reaction(conversation, posted.id, "+1");
    let reacted = backend.messages(conversation, 0, 10).remove(0);
    assert_eq!(
      reacted.reactions,
      vec![Reaction {
        emoji: "+1".into(),
        count: 1,
        reacted_by_me: true
      }]
    );
    backend.toggle_reaction(conversation, posted.id, "+1");
    assert!(backend.messages(conversation, 0, 10)[0].reactions.is_empty());

    assert_eq!(records::outbox(&store).unwrap().len(), 3);
    assert_eq!(
      backend.drain_commands(),
      vec![
        CoreCommand::FlushOutbox,
        CoreCommand::FlushOutbox,
        CoreCommand::FlushOutbox
      ]
    );
    backend.mark_read(conversation);
    assert_eq!(
      backend.drain_commands(),
      vec![CoreCommand::MarkRead { conversation }]
    );
  }

  #[test]
  fn typing_is_read_from_the_shared_state_and_announced_as_a_command() {
    let me = id::<UserId>(1);
    let colleague = id::<UserId>(2);
    let conversation = id::<ConversationId>(10);
    let typing = crate::TypingState::default();
    let backend = CoreChatBackend::new(MemoryStore::new(), me, Box::new(ManualClock::new(10_000)))
      .with_typing(typing.clone());

    // What the client records from the gateway; the backend shares the same state.
    typing.note(conversation, colleague, 9_000);
    typing.note(conversation, me, 9_000);
    assert_eq!(
      backend.typing_users(conversation),
      vec![colleague],
      "never the user themself"
    );
    assert!(backend.typing_users(id::<ConversationId>(11)).is_empty());
    typing.note(conversation, colleague, 10_000 - crate::client::TYPING_TIMEOUT_MS);
    assert!(
      backend.typing_users(conversation).is_empty(),
      "indicators run out"
    );

    backend.notify_typing(conversation);
    assert_eq!(
      backend.drain_commands(),
      vec![CoreCommand::Typing { conversation }]
    );
  }

  #[test]
  fn a_rejected_message_offers_retry_and_delete() {
    let store = std::sync::Arc::new(MemoryStore::new());
    let conversation = id::<ConversationId>(10);
    let backend = CoreChatBackend::new(store.clone(), id::<UserId>(1), Box::new(ManualClock::new(1_000)));
    backend.post_message(conversation, None, "hello");
    let posted = backend.messages(conversation, 0, 10).remove(0);
    let is_failed = || backend.message_extras(conversation, &[posted.id])[0].is_send_failed;
    assert!(!is_failed(), "a message on its way has not failed");

    // What the flush does when the server rejects the message for good.
    let mut entry = records::outbox(&store).unwrap().remove(0);
    let key = records::outbox_key(conversation, entry.client_message_id);
    entry.maybe_last_error = Some("rejected".into());
    records::put_json(store.as_ref(), Table::Outbox, &key, &entry).unwrap();
    assert!(is_failed());

    backend.drain_commands();
    backend.retry_send(conversation, posted.id);
    assert!(!is_failed());
    assert_eq!(backend.drain_commands(), vec![CoreCommand::FlushOutbox]);

    // Delete only takes back a failed message, never one that may still be on its way.
    backend.discard_unsent(conversation, posted.id);
    assert_eq!(records::outbox(&store).unwrap().len(), 1);
    records::put_json(store.as_ref(), Table::Outbox, &key, &entry).unwrap();
    backend.discard_unsent(conversation, posted.id);
    assert!(records::outbox(&store).unwrap().is_empty());
    assert!(backend.message_extras(conversation, &[posted.id])[0].is_deleted);
  }

  #[tokio::test]
  async fn commands_reach_the_server() {
    use crate::api::ServerApi;
    use crate::api::fake::FakeServer;

    let server = FakeServer::new();
    let api = server.add_user("Alice");
    let store = std::sync::Arc::new(MemoryStore::new());
    let options = crate::EnrollmentOptions {
      device_name: "test".into(),
      secret: crate::EnrollmentSecret::Passphrase(teamcraft_crypto::secret::SecretBytes::new(
        b"passphrase".to_vec(),
      )),
      argon2: teamcraft_crypto::backup::Argon2Settings {
        memory_kib: 64,
        iterations: 1,
        parallelism: 1,
      },
      maybe_attestation: None,
    };
    let mut client = crate::PendingEnrollment::new()
      .complete(
        api.clone(),
        store.clone(),
        Box::new(ManualClock::new(0)),
        Default::default(),
        options,
      )
      .await
      .unwrap();
    let space = api.list_spaces().await.unwrap().spaces[0].id;
    let channel = client.create_channel(space, "general").await.unwrap();
    let backend = CoreChatBackend::new(store.clone(), client.user_id(), Box::new(ManualClock::new(5)));

    backend.post_message(channel, None, "hi");
    backend.set_favorite(channel, true);
    for command in backend.drain_commands() {
      client.run_ui_command(command).await.unwrap();
    }
    backend.mark_read(channel);
    for command in backend.drain_commands() {
      client.run_ui_command(command).await.unwrap();
    }

    let conversation = backend.conversation(channel).unwrap();
    assert!(conversation.is_favorite);
    assert_eq!(conversation.category, SidebarCategory::Favorites);
    assert!(backend.messages(channel, 0, 1)[0].id.snowflake().get() & PENDING_ID_BIT == 0);
  }

  #[test]
  fn out_of_range_pages_are_clipped() {
    let backend = CoreChatBackend::new(MemoryStore::new(), id(1), Box::new(ManualClock::new(0)));
    assert!(backend.messages(id(10), 5, 10).is_empty());
  }

  mod mention_counts {
    use super::*;

    #[test]
    fn counts_explicit_mentions_channel_words_and_keywords_but_not_own_or_plain_messages() {
      let (backend, conversation, users) = mention_fixture(&[
        (2, "hello everyone", &[]),
        (2, "ping @alice", &[]),
        (2, "@here standup", &[]),
        (2, "no text match", &[1]),
        (1, "@alice my own message", &[1]),
      ]);
      let counted = backend.to_conversation(&conversation, &users, &backend.mention_preferences());
      assert_eq!(counted.unread_count, 4);
      assert_eq!(counted.mention_count, 3);
    }

    #[test]
    fn keywords_count_and_replayed_messages_do_not_raise_the_count() {
      let (backend, conversation, users) = mention_fixture(&[(2, "deploy failed", &[])]);
      let preferences = MentionPreferences {
        keywords: vec!["deploy".into()],
        ..MentionPreferences::default()
      };
      crate::notify_rules::save_mention_preferences(&backend.store, &preferences).unwrap();
      assert_eq!(
        backend
          .to_conversation(&conversation, &users, &backend.mention_preferences())
          .mention_count,
        1
      );
      // The same event arriving again is the same stored message.
      store_message(&backend, &conversation, 0, 2, "deploy failed", &[]);
      assert_eq!(
        backend
          .to_conversation(&conversation, &users, &backend.mention_preferences())
          .mention_count,
        1
      );
    }

    #[test]
    fn messages_at_or_before_the_read_position_do_not_count() {
      let (backend, mut conversation, users) =
        mention_fixture(&[(2, "@alice one", &[]), (2, "@alice two", &[])]);
      assert_eq!(
        backend
          .to_conversation(&conversation, &users, &backend.mention_preferences())
          .mention_count,
        2
      );
      let mut membership = test_membership();
      membership.maybe_last_read_message_id = Some(id(101));
      conversation.maybe_my_membership = Some(membership);
      assert_eq!(
        backend
          .to_conversation(&conversation, &users, &backend.mention_preferences())
          .mention_count,
        1
      );
      conversation
        .maybe_my_membership
        .as_mut()
        .unwrap()
        .maybe_last_read_message_id = Some(id(102));
      assert_eq!(
        backend
          .to_conversation(&conversation, &users, &backend.mention_preferences())
          .mention_count,
        0
      );
    }

    #[test]
    fn muted_conversations_still_show_mentions() {
      let (backend, mut conversation, users) = mention_fixture(&[(2, "@alice look", &[])]);
      let mut membership = test_membership();
      membership.notification_level = teamcraft_proto::NotificationLevel::None;
      conversation.maybe_my_membership = Some(membership);
      let counted = backend.to_conversation(&conversation, &users, &backend.mention_preferences());
      assert!(counted.is_muted);
      assert_eq!(counted.mention_count, 1);
    }

    fn test_membership() -> teamcraft_proto::MyMembership {
      teamcraft_proto::MyMembership {
        role: teamcraft_proto::ChannelRole::Member,
        notification_level: teamcraft_proto::NotificationLevel::All,
        is_favorite: false,
        maybe_last_read_message_id: None,
        maybe_muted_until: None,
        is_notification_inherited: false,
      }
    }

    /// Alice (1) is the user; messages are `(sender, body, explicit mentions)` with server IDs 101...
    fn mention_fixture(
      messages: &[(i64, &str, &[i64])],
    ) -> (
      CoreChatBackend<MemoryStore>,
      ConversationResponse,
      BTreeMap<UserId, UserSummary>,
    ) {
      let backend = CoreChatBackend::new(MemoryStore::new(), id(1), Box::new(ManualClock::new(0)));
      let channel = id::<ConversationId>(10);
      let conversation = ConversationResponse {
        id: channel,
        maybe_space_id: None,
        kind: ConversationKindProto::Channel,
        maybe_name: Some("general".into()),
        maybe_topic: None,
        maybe_description: None,
        visibility: ConversationVisibility::Public,
        history_policy: teamcraft_proto::HistoryPolicy::Full,
        maybe_category_id: None,
        is_archived: false,
        moderation: Default::default(),
        is_integration: false,
        contains_guests: false,
        maybe_my_membership: None,
        participants: Vec::new(),
      };
      for (index, (sender, body, mentions)) in messages.iter().enumerate() {
        store_message(&backend, &conversation, index, *sender, body, mentions);
      }
      let me = UserSummary {
        id: id(1),
        maybe_username: Some("alice".into()),
        display_name: "Alice Example".into(),
        kind: teamcraft_proto::UserKind::Member,
        maybe_email: None,
        maybe_organization: None,
      };
      (backend, conversation, BTreeMap::from([(me.id, me)]))
    }

    fn store_message(
      backend: &CoreChatBackend<MemoryStore>,
      conversation: &ConversationResponse,
      index: usize,
      sender: i64,
      body: &str,
      mentions: &[i64],
    ) {
      let number = u8::try_from(index).unwrap() + 1;
      backend
        .store
        .upsert_message(&StoredMessage {
          conversation_id: conversation.id,
          client_message_id: ClientMessageId::from_bytes([number; 16]),
          maybe_server_id: Some(id(100 + i64::from(number))),
          sender_user_id: id(sender),
          maybe_thread_id: None,
          sent_at_ms: 1_000 * i64::from(number),
          content: MessageContent::Text {
            body: body.into(),
            mentions: mentions.iter().map(|raw| id(*raw)).collect(),
            meta: Default::default(),
          },
          is_edited: false,
          edits: Vec::new(),
          is_pinned: false,
        })
        .unwrap();
    }
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
