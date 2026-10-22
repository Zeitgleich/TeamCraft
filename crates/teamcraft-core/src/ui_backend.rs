//! [`ChatBackend`] of `teamcraft-client-api` over the local store (feature `ui`).
//!
//! The UI's trait is synchronous and reads the *already decrypted* store (crates/teamcraft-client-api
//! backend.rs), which is exactly what the store holds. Writes don't go to the network here:
//! messages and reactions are put into the outbox, everything else becomes a [`CoreCommand`] the
//! app's sync task drains and runs against the server. After either, the sync task flushes.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::{
  Conversation, ConversationId, ConversationKind, CustomStatus, Message, MessageKind, PresenceStatus,
  Reaction, SidebarCategory, Space, SpaceId, ThreadId, ThreadSummary, User, UserId,
};
use teamcraft_proto::{
  ClientMessageId, ConversationKind as ConversationKindProto, ConversationResponse, ConversationVisibility,
  Id, ListConversationMembersResponse, MessageId, NotificationLevel, PresenceState, Snowflake, SpaceResponse,
  UserKind, UserSummary,
};

use crate::clock::Clock;
use crate::error::CoreError;
use crate::records::{self, Draft};
use crate::store::{LocalStore, MessageContent, MessageFilter, StoredMessage, Table};

/// Bit set on the IDs the UI gets for messages still in the outbox. Real snowflakes stay below it
/// for decades (41 bits of milliseconds since 2026 shifted by 22).
const PENDING_ID_BIT: i64 = 1 << 62;

/// Requests from the UI that need the server; the sync task drains and runs them.
#[derive(Clone, Debug, PartialEq)]
pub enum CoreCommand {
  /// New messages or reactions are in the outbox.
  FlushOutbox,
  MarkRead {
    conversation: ConversationId,
  },
  SetFavorite {
    conversation: ConversationId,
    is_favorite: bool,
  },
  SetPresence {
    presence: PresenceStatus,
  },
  SetCustomStatus {
    maybe_status: Option<CustomStatus>,
  },
  /// Messages a search found only in shared index segments: fetch and decrypt them.
  #[cfg(not(target_arch = "wasm32"))]
  FetchSearchHits {
    hits: Vec<crate::search::SearchMatch>,
  },
}

pub struct CoreChatBackend<S> {
  store: S,
  me: UserId,
  clock: Box<dyn Clock>,
  commands: Rc<RefCell<Vec<CoreCommand>>>,
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
      #[cfg(not(target_arch = "wasm32"))]
      search: RefCell::new(Default::default()),
    }
  }

  /// Takes the queued commands (the sync task runs them).
  pub fn drain_commands(&self) -> Vec<CoreCommand> {
    std::mem::take(&mut *self.commands.borrow_mut())
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

  fn command(&self, command: CoreCommand) {
    self.commands.borrow_mut().push(command);
  }

  /// Reads that fail (corrupt store) are logged and show as empty: the UI has no error channel here.
  fn read<T: Default>(&self, result: Result<T, CoreError>) -> T {
    result.unwrap_or_else(|error| {
      tracing::error!(%error, "local store read failed");
      T::default()
    })
  }

  fn summaries(&self) -> BTreeMap<UserId, UserSummary> {
    let list: Vec<UserSummary> = self.read(records::list_json(&self.store, Table::Users));
    list.into_iter().map(|user| (user.id, user)).collect()
  }

  fn conversations_raw(&self) -> Vec<ConversationResponse> {
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
    }
  }

  fn to_conversation(
    &self,
    response: &ConversationResponse,
    users: &BTreeMap<UserId, UserSummary>,
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
    Conversation {
      id: response.id,
      maybe_space: response.maybe_space_id,
      kind,
      category,
      name,
      topic: response.maybe_topic.clone().unwrap_or_default(),
      member_ids,
      unread_count: self.unread_count(response),
      mention_count: 0,
      is_favorite,
      is_muted: response
        .maybe_my_membership
        .as_ref()
        .is_some_and(|membership| membership.notification_level == NotificationLevel::None),
      can_post: true,
      bookmarks: Vec::new(),
    }
  }

  fn unread_count(&self, response: &ConversationResponse) -> u32 {
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
    let unread = messages
      .iter()
      .filter(|message| message.sender_user_id != self.me)
      .filter(|message| match (message.maybe_server_id, maybe_last_read) {
        (Some(id), Some(last_read)) => id > last_read,
        (Some(_), None) => true,
        (None, _) => false,
      })
      .count();
    u32::try_from(unread).unwrap_or(u32::MAX)
  }

  fn to_message(&self, stored: &StoredMessage, all: &[StoredMessage]) -> Message {
    let id = ui_message_id(stored);
    let body = match &stored.content {
      MessageContent::Text { body, .. } => body.clone(),
      // The UI recognizes files by their name in the body (search filter `has:file`).
      MessageContent::File { attachment } => format!("📎 {}", attachment.file_name),
      MessageContent::Reaction { .. }
      | MessageContent::Edit { .. }
      | MessageContent::Delete { .. }
      | MessageContent::Deleted
      | MessageContent::IndexSegment { .. }
      | MessageContent::Unknown => String::new(),
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
      author: stored.sender_user_id,
      kind: MessageKind::User,
      sent_at: stored.sent_at_ms,
      body,
      is_edited: stored.is_edited,
      reactions: reactions_of(stored.client_message_id, all, self.me),
      reply_count: u32::try_from(reply_count).unwrap_or(u32::MAX),
      maybe_thread: stored.maybe_thread_id,
    }
  }

  fn all_messages(&self, conversation: ConversationId) -> Vec<StoredMessage> {
    self.read(
      self
        .store
        .messages(conversation, MessageFilter::All)
        .map_err(CoreError::from),
    )
  }

  /// `@username` mentions of conversation members in a message body.
  fn mentions_in(&self, conversation: ConversationId, body: &str) -> Vec<UserId> {
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

  fn enqueue(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>, content: MessageContent) {
    let draft = Draft {
      conversation_id: conversation,
      maybe_thread_id: maybe_thread,
      content,
    };
    match records::enqueue_message(&self.store, self.me, self.clock.now_ms(), draft) {
      Ok(_) => self.command(CoreCommand::FlushOutbox),
      Err(error) => tracing::error!(%error, "could not write message to the outbox"),
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
    spaces
      .into_iter()
      .filter(|space| space.maybe_my_role.is_some())
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
    self
      .conversations_raw()
      .iter()
      .filter(|conversation| conversation.maybe_space_id == Some(space))
      .map(|conversation| self.to_conversation(conversation, &users))
      .collect()
  }

  fn direct_conversations(&self) -> Vec<Conversation> {
    let users = self.summaries();
    self
      .conversations_raw()
      .iter()
      .filter(|conversation| {
        matches!(
          conversation.kind,
          ConversationKindProto::Dm | ConversationKindProto::Group
        )
      })
      .map(|conversation| self.to_conversation(conversation, &users))
      .collect()
  }

  fn conversation(&self, id: ConversationId) -> Option<Conversation> {
    let users = self.summaries();
    self
      .conversations_raw()
      .iter()
      .find(|conversation| conversation.id == id)
      .map(|conversation| self.to_conversation(conversation, &users))
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

  fn messages(&self, conversation: ConversationId, start: usize, len: usize) -> Vec<Message> {
    let all = self.all_messages(conversation);
    let top_level: Vec<&StoredMessage> = all
      .iter()
      .filter(|message| message.maybe_thread_id.is_none() && !message.is_hidden() && !is_deleted(message))
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
    self.command(CoreCommand::SetPresence { presence });
  }

  fn set_custom_status(&self, maybe_status: Option<CustomStatus>) {
    self.command(CoreCommand::SetCustomStatus { maybe_status });
  }
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
fn ui_message_id(message: &StoredMessage) -> MessageId {
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

  #[tokio::test]
  async fn commands_reach_the_server() {
    use crate::api::ServerApi;
    use crate::api::fake::FakeServer;

    let server = FakeServer::new();
    let api = server.add_user("Alice");
    let store = std::sync::Arc::new(MemoryStore::new());
    let options = crate::EnrollmentOptions {
      device_name: "test".into(),
      passphrase: teamcraft_crypto::secret::SecretBytes::new(b"pass".to_vec()),
      argon2: teamcraft_crypto::backup::Argon2Settings {
        memory_kib: 64,
        iterations: 1,
        parallelism: 1,
      },
      maybe_attestation: None,
      maybe_org_recovery: None,
      maybe_linked: None,
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

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
