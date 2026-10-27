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
  FileInfo, LinkedMessage, MediaAnchor, MediaPage, MessageExtras, OutgoingMessage, PermalinkTarget,
  SavedItem, ShareLink, ShareLinkOptions,
};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::calls::{
  BackgroundImageInfo, OrgBackgroundInfo, OrgBackgroundRule, OrgBackgrounds,
};
use teamcraft_client_api::live::LiveSnapshot;
use teamcraft_client_api::model::{
  Bookmark, BookmarkKind, Conversation, ConversationId, ConversationKind, CustomStatus, DeviceSetup,
  JoinableSpace, MembershipIssue, Message, MessageKind, PENDING_ID_BIT, PendingInvite, PresenceStatus,
  Reaction, SidebarCategory, Space, SpaceId, SpaceMember, SpaceRoster, SystemEvent, ThreadId, ThreadSummary,
  TimestampMs, User, UserId, hue_from_hex,
};
use teamcraft_proto::{
  AttachmentId, ClientMessageId, ConversationKind as ConversationKindProto, ConversationResponse,
  ConversationVisibility, Id, InviteId, JoinPolicy, ListConversationMembersResponse, ListInvitesResponse,
  MessageId, NotificationLevel, PresenceState, Snowflake, SpaceResponse, SpaceRole, UserKind, UserSummary,
};
use teamcraft_proto::{BackgroundId, OrgBackgroundId};
use teamcraft_proto::{
  CategoryId, ConversationNotificationSettings, CreateSpaceRequest, EffectiveNotifications, HistoryPolicy,
  MoveChannelRequest, UpdateConversationRequest, UpdateSpaceRequest, effective_notifications,
};

use teamcraft_client_api::notify::{MentionRules, is_mentioned};
use teamcraft_client_api::receipts::{ReadIndicator, seen_by};

use crate::client::bookmarks_key;
use crate::client::workspace_admin::{ChannelOptions, StoredRoster};
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
  /// The conversation was read up to this message (an activity entry that was opened), not to its
  /// end. The server keeps the position monotonic.
  MarkReadThrough {
    conversation: ConversationId,
    message: MessageId,
  },
  /// The thread was read up to this reply.
  MarkThreadRead {
    conversation: ConversationId,
    thread: ThreadId,
    message: MessageId,
  },
  /// Presence and last seen of these people.
  RefreshPresence {
    users: Vec<UserId>,
  },
  /// How far the other members of a DM or group DM read.
  RefreshReadReceipts {
    conversation: ConversationId,
  },
  /// Ask the server's directory for people matching the query (people page, quick switcher).
  SearchDirectory {
    query: String,
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
    maybe_expires_in_seconds: Option<u64>,
    maybe_max_downloads: Option<u32>,
    maybe_password: Option<RedactedText>,
  },
  RevokeShareLink {
    conversation: ConversationId,
    attachment: AttachmentId,
    share_id: String,
  },
  RefreshShareLinks {
    attachment: AttachmentId,
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
  /// An admin asks the owners of a private channel to let them in (admin.md §2.4).
  RequestAdminJoin {
    conversation: ConversationId,
  },
  WithdrawAdminJoin {
    conversation: ConversationId,
  },
  /// A channel owner lets an admin in who asked: the server membership and the own MLS add, with
  /// the history the owner chose.
  ApproveAdminJoin {
    conversation: ConversationId,
    user: UserId,
    history: HistoryPolicy,
  },
  DeclineAdminJoin {
    conversation: ConversationId,
    user: UserId,
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
  /// "Retry" on a failed removal or leave, or "remove" on somebody left in the group.
  RetryRemoval {
    conversation: ConversationId,
    user: UserId,
  },
  /// "Keep" on an unconfirmed or failed removal.
  DiscardRemoval {
    conversation: ConversationId,
    user: UserId,
  },
  /// "Retry" on a DM whose encryption setup waits for a welcome or failed (#278).
  RetryDmSetup {
    conversation: ConversationId,
  },
  /// A meeting chat the user scheduled or started, with the users they invited.
  SetUpMeetingChat {
    conversation: ConversationId,
    invitees: Vec<UserId>,
  },
  /// The organizer saved a changed invitee list of a meeting.
  UpdateMeetingChatMembers {
    conversation: ConversationId,
    invitees: Vec<UserId>,
    removed: Vec<UserId>,
  },
  /// The user added people to their DM call: it moves into the meeting chat they just started.
  AddPeopleToCall {
    from: ConversationId,
    in_call: Vec<UserId>,
    added: Vec<UserId>,
    maybe_title: Option<String>,
    key: teamcraft_proto::IdempotencyKey,
  },
  /// The user follows an announced move of their call into a meeting room.
  AcceptCallMove {
    from: ConversationId,
    to: ConversationId,
  },
  /// A host admitted a user from a meeting's lobby.
  LetIntoMeetingChat {
    conversation: ConversationId,
    user: UserId,
  },
  /// A host's app is about to hand out a guest link: make its secret (`guest_link_fragment`).
  PrepareGuestLink {
    conversation: ConversationId,
    meeting: teamcraft_proto::MeetingId,
  },
  /// A host admitted a guest: their device joins the chat's group if the link's proof checks out.
  LetGuestIntoMeetingChat {
    conversation: ConversationId,
    meeting: teamcraft_proto::MeetingId,
    guest: UserId,
    display_name: String,
    maybe_proof: Option<Vec<u8>>,
    access_ends_at_ms: i64,
  },
  InviteToSpace {
    space: SpaceId,
    email: String,
    is_guest: bool,
    channels: Vec<ConversationId>,
    maybe_expires_at: Option<i64>,
    maybe_message: Option<String>,
  },
  /// People join a channel, first its space when asked (the add-people dialog).
  AddPeopleToChannel {
    conversation: ConversationId,
    people: Vec<(UserId, bool)>,
    history: HistoryPolicy,
    joins_space: bool,
  },
  /// The members and pending invitations of a space, for its settings.
  RefreshSpaceRoster {
    space: SpaceId,
  },
  SetSpaceRole {
    space: SpaceId,
    user: UserId,
    role: SpaceRole,
  },
  SetChannelRole {
    conversation: ConversationId,
    user: UserId,
    role: teamcraft_proto::ChannelRole,
  },
  RemoveSpaceMember {
    space: SpaceId,
    user: UserId,
  },
  RevokeSpaceInvite {
    space: SpaceId,
    invite: InviteId,
  },
  /// People picked from the directory join a space (those already in it are skipped).
  AddSpaceMembers {
    space: SpaceId,
    users: Vec<UserId>,
    is_guest: bool,
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

impl Drop for RedactedText {
  fn drop(&mut self) {
    zeroize::Zeroize::zeroize(&mut self.0);
  }
}

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
  /// File transfers in flight; shared with the client when [`Self::with_transfers`] was given them.
  transfers: crate::transfers::Transfers,
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
      transfers: crate::transfers::Transfers::new(),
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

  /// Reads the progress of file transfers from the client ([`crate::Client::transfers`]).
  #[must_use]
  pub fn with_transfers(mut self, transfers: crate::transfers::Transfers) -> Self {
    self.transfers = transfers;
    self
  }

  pub(crate) fn transfers(&self) -> &crate::transfers::Transfers {
    &self.transfers
  }

  /// Takes the queued commands (the sync task runs them).
  pub fn drain_commands(&self) -> Vec<CoreCommand> {
    std::mem::take(&mut *self.commands.borrow_mut())
  }

  /// The deactivated and deleted accounts this device knows: [`ChatBackend::users`] leaves them out
  /// of lists and pickers (issue #654), but they still name their old messages. The mobile apps
  /// read them next to the snapshot (issue #751).
  pub fn former_users(&self) -> Vec<User> {
    self
      .summaries()
      .values()
      .filter(|summary| summary.is_deactivated)
      .map(|summary| self.to_user(summary))
      .collect()
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

  pub(crate) fn members(&self, conversation: ConversationId) -> Vec<UserId> {
    let maybe: Option<ListConversationMembersResponse> = self.read(records::get_json(
      &self.store,
      Table::Members,
      &conversation.to_string(),
    ));
    maybe
      .map(|list| list.members.into_iter().map(|member| member.user.id).collect())
      .unwrap_or_default()
  }

  /// The members of a conversation whose receipts show as ticks (a DM, #635); `None` elsewhere.
  pub(crate) fn tick_members(&self, conversation: ConversationId) -> Option<Vec<UserId>> {
    let response = self
      .conversations_raw()
      .into_iter()
      .find(|response| response.id == conversation)?;
    (ReadIndicator::of(view_kind(&response)) == ReadIndicator::Ticks).then(|| self.members(conversation))
  }

  /// How far each other member read, as far as this device heard (gateway events and
  /// [`CoreCommand::RefreshReadReceipts`]).
  pub(crate) fn read_receipts(&self, conversation: ConversationId) -> Vec<(UserId, MessageId)> {
    self
      .read(crate::read_state::read_receipts(&self.store, conversation))
      .into_iter()
      .collect()
  }

  pub(crate) fn to_user(&self, summary: &UserSummary) -> User {
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
      maybe_custom_status: self.custom_status_of(summary.id),
      avatar_hue: hue(summary.id),
      is_guest: summary.kind == UserKind::Guest,
      is_bot: summary.kind == UserKind::Bot,
      maybe_organization: summary.maybe_organization.clone(),
      is_deactivated: summary.is_deactivated,
      is_deleted: summary.is_deleted,
      device_setup: match summary.maybe_has_devices {
        Some(true) => DeviceSetup::HasDevices,
        Some(false) => DeviceSetup::NoDevices,
        None => DeviceSetup::Unknown,
      },
      maybe_email: summary.maybe_email.clone(),
    }
  }

  /// The person's custom status from the presence log (what the server last announced, or the
  /// user's own change while it waits for the server). It keeps its end: the UI stops showing it
  /// once that passed, before the server's announcement that it ended arrives.
  fn custom_status_of(&self, user: UserId) -> Option<CustomStatus> {
    let maybe_entry: Option<teamcraft_proto::live::RevisedPresence> = self.read(records::get_json(
      &self.store,
      Table::AnnouncedPresence,
      &user.to_string(),
    ));
    maybe_entry
      .and_then(|entry| entry.presence.maybe_status)
      .and_then(|status| CustomStatus::from_wire(&status))
  }

  pub(crate) fn to_conversation(
    &self,
    response: &ConversationResponse,
    users: &BTreeMap<UserId, UserSummary>,
    preferences: &MentionPreferences,
  ) -> Conversation {
    let unread = self.unread_messages(response);
    self.conversation_view(response, users, preferences, &unread)
  }

  /// A conversation's entry with its unread messages already picked.
  pub(crate) fn conversation_view(
    &self,
    response: &ConversationResponse,
    users: &BTreeMap<UserId, UserSummary>,
    preferences: &MentionPreferences,
    unread: &[StoredMessage],
  ) -> Conversation {
    let mut member_ids = self.members(response.id);
    let is_notes_to_self = response.is_notes_to_self(self.me);
    // Notes to self (a DM of one): known from the conversation itself before any member list.
    if member_ids.is_empty() && is_notes_to_self {
      member_ids = vec![self.me];
    }
    let kind = view_kind(response);
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
      if others.is_empty() {
        // The member list is not loaded yet (a sync catching up, issue #247): the conversation
        // list names the other people too.
        others = response
          .participants
          .iter()
          .filter(|user| user.id != self.me)
          .map(|user| user.display_name.clone())
          .collect();
      }
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
      unread_count: u32::try_from(unread.len()).unwrap_or(u32::MAX),
      mention_count: self.mention_count(unread, users.get(&self.me), preferences),
      is_favorite,
      is_muted: self.is_muted(response),
      can_post: response.may_post(),
      bookmarks: self.bookmarks_of(response.id),
      is_catching_up: self.read(crate::client::is_catching_up(&self.store, response.id)),
      is_notes_to_self,
      maybe_permissions: response.maybe_my_permissions.clone(),
      has_only_deactivated_others: response.has_only_deactivated_others(self.me, |participant| {
        users
          .get(&participant.id)
          .map_or(participant.is_deactivated, |user| user.is_deactivated)
      }),
      maybe_image_version: response.maybe_image.as_ref().map(|image| image.version.clone()),
    }
  }

  /// Whether notifications of the conversation are off now: its level is none or a mute time has
  /// not passed yet, with the space's setting standing in for a conversation the member never
  /// configured.
  fn is_muted(&self, response: &ConversationResponse) -> bool {
    let Some(effective) = self.effective_notifications(response) else {
      return false;
    };
    effective.level == NotificationLevel::None
      || effective
        .maybe_muted_until
        .is_some_and(|until| until.0 > self.now_ms())
  }

  /// When a mute that is on now ends by itself (Unix milliseconds): the conversation's entry
  /// changes then without any write (issue #246, the snapshot cache reads it again).
  pub(crate) fn mute_end(&self, response: &ConversationResponse) -> Option<i64> {
    let until = self.effective_notifications(response)?.maybe_muted_until?.0;
    (until > self.now_ms()).then_some(until)
  }

  fn effective_notifications(&self, response: &ConversationResponse) -> Option<EffectiveNotifications> {
    let membership = response.maybe_my_membership.as_ref()?;
    let own = ConversationNotificationSettings {
      level: membership.notification_level.clone(),
      maybe_muted_until: membership.maybe_muted_until,
      is_inherited: membership.is_notification_inherited,
    };
    let maybe_space: Option<SpaceResponse> = response
      .maybe_space_id
      .and_then(|space| self.read(records::get_json(&self.store, Table::Spaces, &space.to_string())));
    Some(effective_notifications(
      &own,
      maybe_space
        .as_ref()
        .and_then(|space| space.maybe_my_notifications.as_ref()),
    ))
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
    self.unread_among(response, maybe_mark, self.top_level(response.id))
  }

  /// A conversation's top-level messages, deleted ones included.
  pub(crate) fn top_level(&self, conversation: ConversationId) -> Vec<StoredMessage> {
    self.read(
      self
        .store
        .messages(conversation, MessageFilter::TopLevel)
        .map_err(CoreError::from),
    )
  }

  /// The unread ones of a conversation's top-level `messages`, given the user's "mark unread".
  pub(crate) fn unread_among(
    &self,
    response: &ConversationResponse,
    maybe_mark: Option<MessageId>,
    messages: Vec<StoredMessage>,
  ) -> Vec<StoredMessage> {
    let maybe_last_read = response
      .maybe_my_membership
      .as_ref()
      .and_then(|membership| membership.maybe_last_read_message_id);
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
        .map(|attachment| attachment.file_name.clone())
        .collect::<Vec<_>>()
        .join(" "),
      MessageContent::Text { body, .. } => body.clone(),
      // The UI recognizes files by their name in the body (search filter `has:file`).
      MessageContent::File { attachment } => attachment.file_name.clone(),
      MessageContent::Reaction { .. }
      | MessageContent::Edit { .. }
      | MessageContent::Delete { .. }
      | MessageContent::Pin { .. }
      | MessageContent::Bookmarks { .. }
      | MessageContent::Event { .. }
      | MessageContent::Deleted
      | MessageContent::IndexSegment { .. }
      | MessageContent::IntegrationSource { .. }
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
        maybe_custom_status: self.custom_status_of(self.me),
        avatar_hue: hue(self.me),
        is_guest: false,
        is_bot: false,
        maybe_organization: None,
        is_deactivated: false,
        is_deleted: false,
        device_setup: DeviceSetup::HasDevices,
        maybe_email: None,
      })
  }

  fn user(&self, id: UserId) -> Option<User> {
    self.summaries().get(&id).map(|summary| self.to_user(summary))
  }

  fn users(&self) -> Vec<User> {
    self
      .summaries()
      .values()
      .filter(|summary| !summary.is_deactivated)
      .map(|summary| self.to_user(summary))
      .collect()
  }

  fn search_people(&self, query: &str) -> Vec<User> {
    self.ui_search_people(query)
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
        hue: space
          .maybe_color
          .as_deref()
          .and_then(hue_from_hex)
          .unwrap_or_else(|| hue(space.id)),
        maybe_color: space.maybe_color,
        maybe_image_version: space.maybe_image.map(|image| image.version),
        unread_count: 0,
        mention_count: 0,
        maybe_description: space.maybe_description,
        member_count: space.member_count,
        join_policy: space.join_policy,
        can_add_members: space.can_add_members,
        can_invite_guests: space.can_invite_guests,
        can_manage_space: space.can_manage_space,
        can_create_channels: space.can_create_channels,
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

  fn undecrypted_count(&self, conversation: ConversationId) -> usize {
    // Parked under "<conversation>/<client message ID>" (`client::messaging`); the keys suffice.
    let prefix = format!("{conversation}/");
    self
      .read(self.store.list(Table::Undecrypted).map_err(CoreError::from))
      .iter()
      .filter(|(key, _)| key.starts_with(&prefix))
      .count()
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

  fn mark_read_through(&self, conversation: ConversationId, message: MessageId) {
    self.command(CoreCommand::MarkReadThrough {
      conversation,
      message,
    });
  }

  fn mark_thread_read(&self, conversation: ConversationId, thread: ThreadId) {
    // The server takes a reply of the thread, not the root; the newest one stored is the furthest
    // this device has seen.
    let maybe_latest = self
      .stored(conversation)
      .into_iter()
      .filter(|stored| {
        stored.maybe_thread_id == Some(thread) && matches!(stored.content, MessageContent::Text { .. })
      })
      .filter_map(|stored| stored.maybe_server_id)
      .max();
    if let Some(message) = maybe_latest {
      self.command(CoreCommand::MarkThreadRead {
        conversation,
        thread,
        message,
      });
    }
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

  fn presence_policy(&self) -> teamcraft_proto::PresencePolicy {
    self.verified_policy().presence
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

  fn linked_message(&self, link: &str) -> Option<LinkedMessage> {
    Some(self.ui_linked_message(link))
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

  fn is_loaded(&self, list: teamcraft_client_api::backend::LoadingList) -> bool {
    self.ui_is_loaded(list)
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

  fn newest_message(&self, conversation: ConversationId) -> Option<MessageId> {
    // The same message `CoreCommand::MarkRead` sends as the read position.
    newest_server_id(&self.all_messages(conversation))
  }

  fn is_waiting_for_device(&self, conversation: ConversationId) -> bool {
    crate::client::is_waiting_for_device(&self.store, conversation).unwrap_or(false)
  }

  fn membership_issues(&self, conversation: ConversationId) -> Vec<MembershipIssue> {
    let mut issues: Vec<MembershipIssue> = self
      .read(crate::client::pending_removals(&self.store, conversation))
      .into_iter()
      .map(|(user, status)| match status {
        crate::client::RemovalStatus::Pending => MembershipIssue::RemovalPending { user },
        crate::client::RemovalStatus::Failed => MembershipIssue::RemovalFailed { user },
        crate::client::RemovalStatus::Unconfirmed => MembershipIssue::RemovalUnconfirmed { user },
      })
      .collect();
    // Users left in the group are never shown here (#661): the admin console lists them.
    if self.read(crate::client::is_queue_refused(&self.store, conversation)) {
      issues.push(MembershipIssue::GroupRefused);
    }
    match self.read(crate::client::dm_setup_status(
      &self.store,
      conversation,
      self.now_ms(),
    )) {
      Some(crate::client::DmSetupStatus::WaitingForWelcome { remaining }) => {
        issues.push(MembershipIssue::EncryptionSetup {
          remaining_ms: u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX),
        });
      },
      Some(crate::client::DmSetupStatus::Failed) => issues.push(MembershipIssue::EncryptionSetupFailed),
      None => {},
    }
    issues.extend(
      self
        .read(crate::client::pending_user_additions(&self.store, conversation))
        .into_iter()
        .map(|(user, wait)| match wait {
          crate::client::UserAddWait::NoDevice => MembershipIssue::WaitingForFirstDevice { user },
          crate::client::UserAddWait::NoKeyPackage => MembershipIssue::NoKeyPackage { user },
          crate::client::UserAddWait::Unverifiable => MembershipIssue::DevicesUnverified { user },
          crate::client::UserAddWait::NeverSetUp => MembershipIssue::NeverSetUp { user },
        }),
    );
    issues
  }

  fn retry_removal(&self, conversation: ConversationId, user: UserId) {
    self.command(CoreCommand::RetryRemoval { conversation, user });
  }

  fn discard_removal(&self, conversation: ConversationId, user: UserId) {
    self.command(CoreCommand::DiscardRemoval { conversation, user });
  }

  fn retry_dm_setup(&self, conversation: ConversationId) {
    self.command(CoreCommand::RetryDmSetup { conversation });
  }

  fn set_up_meeting_chat(&self, conversation: ConversationId, invitees: Vec<UserId>) {
    self.command(CoreCommand::SetUpMeetingChat {
      conversation,
      invitees,
    });
  }

  fn update_meeting_chat_members(
    &self,
    conversation: ConversationId,
    invitees: Vec<UserId>,
    removed: Vec<UserId>,
  ) {
    self.command(CoreCommand::UpdateMeetingChatMembers {
      conversation,
      invitees,
      removed,
    });
  }

  fn add_people_to_call(
    &self,
    from: ConversationId,
    in_call: Vec<UserId>,
    added: Vec<UserId>,
    maybe_title: Option<String>,
    key: teamcraft_proto::IdempotencyKey,
  ) {
    self.command(CoreCommand::AddPeopleToCall {
      from,
      in_call,
      added,
      maybe_title,
      key,
    });
  }

  fn call_move_offer(
    &self,
    from: ConversationId,
    to: ConversationId,
  ) -> Option<teamcraft_client_api::calls::CallMoveOffer> {
    self
      .read(crate::client::call_move_announcement(&self.store, from, to))
      .map(|record| teamcraft_client_api::calls::CallMoveOffer {
        by: record.by,
        added: record.added,
        is_requested_here: record.is_requested_here,
      })
  }

  fn accept_call_move(&self, from: ConversationId, to: ConversationId) {
    self.command(CoreCommand::AcceptCallMove { from, to });
  }

  fn let_into_meeting_chat(&self, conversation: ConversationId, user: UserId) {
    self.command(CoreCommand::LetIntoMeetingChat { conversation, user });
  }

  fn prepare_guest_link(&self, conversation: ConversationId, meeting: teamcraft_proto::MeetingId) {
    self.command(CoreCommand::PrepareGuestLink {
      conversation,
      meeting,
    });
  }

  fn guest_link_fragment(&self, meeting: teamcraft_proto::MeetingId) -> Option<String> {
    crate::client::guest_link_fragment(self.store_ref(), meeting)
  }

  fn let_guest_into_meeting_chat(
    &self,
    conversation: ConversationId,
    meeting: teamcraft_proto::MeetingId,
    guest: UserId,
    display_name: String,
    maybe_proof: Option<Vec<u8>>,
    access_ends_at_ms: i64,
  ) {
    self.command(CoreCommand::LetGuestIntoMeetingChat {
      conversation,
      meeting,
      guest,
      display_name,
      maybe_proof,
      access_ends_at_ms,
    });
  }

  fn unconfirmed_removals(&self) -> Vec<(ConversationId, UserId)> {
    self.read(crate::client::unconfirmed_removals(&self.store))
  }

  fn group_leftovers(&self) -> Vec<(ConversationId, UserId)> {
    self.read(crate::client::all_group_leftovers(&self.store))
  }

  fn typing_users(&self, conversation: ConversationId) -> Vec<UserId> {
    self.typing.users(conversation, self.me, self.clock.now_ms())
  }

  fn seen_by(&self, conversation: ConversationId, message: MessageId) -> Vec<UserId> {
    seen_by(&self.read_receipts(conversation), message)
  }

  fn refresh_read_receipts(&self, conversation: ConversationId) {
    // Receipts exist only in DMs and group DMs: a channel's answer would always be empty.
    let has_receipts = self.conversations_raw().iter().any(|response| {
      response.id == conversation
        && matches!(
          response.kind,
          ConversationKindProto::Dm | ConversationKindProto::Group
        )
    });
    if has_receipts {
      self.command(CoreCommand::RefreshReadReceipts { conversation });
    }
  }

  fn live_snapshot(&self) -> Option<LiveSnapshot> {
    // Each part is one record, except the receipts (one per member of each DM).
    Some(LiveSnapshot {
      maybe_unread: self.read(crate::read_state::unread(&self.store)),
      threads: self.read(crate::read_state::threads(&self.store)),
      presence: self.read(crate::read_state::announced_presence(&self.store)),
      receipts: self.read(crate::read_state::all_read_receipts(&self.store)),
      marks: self.read(crate::read_state::marks(&self.store)),
    })
  }

  fn meetings_revision(&self) -> u64 {
    self.read(crate::read_state::marks(&self.store)).meetings
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

  fn cancel_upload(&self, conversation: ConversationId, message: MessageId, file_id: &str) {
    self.ui_cancel_upload(conversation, message, file_id);
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

  fn conversation_media(
    &self,
    conversation: ConversationId,
    anchor: MediaAnchor,
    max_messages: usize,
  ) -> Option<MediaPage> {
    Some(self.ui_conversation_media(conversation, anchor, max_messages))
  }

  fn download_file(&self, conversation: ConversationId, message: MessageId) {
    self.ui_download_file(conversation, message);
  }

  fn download_attachment(&self, conversation: ConversationId, message: MessageId, file_id: &str) {
    self.ui_download_attachment(conversation, message, file_id);
  }

  fn create_share_link(
    &self,
    conversation: ConversationId,
    message: MessageId,
    file_id: &str,
    options: ShareLinkOptions,
  ) {
    self.ui_create_share_link(conversation, message, file_id, options);
  }

  fn share_links(&self, conversation: ConversationId, message: MessageId, file_id: &str) -> Vec<ShareLink> {
    self.ui_share_links(conversation, message, file_id)
  }
  fn revoke_share_link(
    &self,
    conversation: ConversationId,
    message: MessageId,
    file_id: &str,
    share_id: &str,
  ) {
    self.ui_revoke_share_link(conversation, message, file_id, share_id);
  }
  fn refresh_share_links(&self, conversation: ConversationId, message: MessageId, file_id: &str) {
    self.ui_refresh_share_links(conversation, message, file_id);
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

  fn join_requests(&self) -> Vec<teamcraft_client_api::activity::JoinRequest> {
    self.ui_join_requests()
  }

  fn request_admin_join(&self, conversation: ConversationId) {
    self.command(CoreCommand::RequestAdminJoin { conversation });
  }

  fn withdraw_admin_join(&self, conversation: ConversationId) {
    self.command(CoreCommand::WithdrawAdminJoin { conversation });
  }

  fn approve_admin_join(&self, conversation: ConversationId, user: UserId, history: HistoryPolicy) {
    self.command(CoreCommand::ApproveAdminJoin {
      conversation,
      user,
      history,
    });
  }

  fn decline_admin_join(&self, conversation: ConversationId, user: UserId) {
    self.command(CoreCommand::DeclineAdminJoin { conversation, user });
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
        hue: space
          .maybe_color
          .as_deref()
          .and_then(hue_from_hex)
          .unwrap_or_else(|| hue(space.id)),
        name: space.name,
        maybe_description: space.maybe_description,
        maybe_image_version: space.maybe_image.map(|image| image.version),
        member_count: space.member_count,
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
      maybe_message: invite.maybe_message,
    });
  }

  fn space_members(&self, space: SpaceId) -> SpaceRoster {
    let stored: Option<StoredRoster> = self.read(records::get_json(
      &self.store,
      Table::SpaceRosters,
      &space.to_string(),
    ));
    let roster = match stored {
      None => return SpaceRoster::Loading,
      Some(StoredRoster::Hidden) => return SpaceRoster::Hidden,
      Some(StoredRoster::Visible(roster)) => roster,
    };
    let mut members: Vec<SpaceMember> = roster
      .members
      .into_iter()
      .map(|member| SpaceMember {
        user: member.user.id,
        is_guest: member.user.kind == UserKind::Guest,
        is_bot: member.user.kind == UserKind::Bot,
        display_name: member.user.display_name,
        role: member.role,
      })
      .collect();
    members.sort_by_key(|member| member.display_name.to_lowercase());
    SpaceRoster::Loaded(members)
  }

  fn space_invites(&self, space: SpaceId) -> Vec<PendingInvite> {
    let invites: Option<ListInvitesResponse> = self.read(records::get_json(
      &self.store,
      Table::SpaceInvites,
      &space.to_string(),
    ));
    invites
      .map(|list| list.invites)
      .unwrap_or_default()
      .into_iter()
      .map(|invite| PendingInvite {
        id: invite.id,
        email: invite.email,
        role: invite.role,
      })
      .collect()
  }

  fn add_people_to_channel(
    &self,
    conversation: ConversationId,
    people: Vec<(UserId, bool)>,
    history: HistoryPolicy,
    joins_space: bool,
  ) {
    self.command(CoreCommand::AddPeopleToChannel {
      conversation,
      people,
      history,
      joins_space,
    });
  }

  fn refresh_space_roster(&self, space: SpaceId) {
    self.command(CoreCommand::RefreshSpaceRoster { space });
  }

  fn integration_sources(
    &self,
    conversation: ConversationId,
  ) -> Vec<teamcraft_client_api::model::IntegrationSource> {
    self.ui_integration_sources(conversation)
  }

  fn can_manage_integration_sources(&self) -> bool {
    self.ui_can_manage_integration_sources()
  }

  fn add_integration_source(
    &self,
    conversation: ConversationId,
    source: teamcraft_client_api::model::NewIntegrationSource,
  ) {
    self.ui_add_integration_source(conversation, source);
  }

  fn revoke_integration_source(&self, conversation: ConversationId, source: teamcraft_proto::SourceId) {
    self.ui_revoke_integration_source(conversation, source);
  }

  fn channel_roles(&self, conversation: ConversationId) -> Vec<teamcraft_client_api::model::ChannelMember> {
    let stored: Option<teamcraft_proto::ListConversationMembersResponse> = self.read(records::get_json(
      &self.store,
      Table::Members,
      &conversation.to_string(),
    ));
    stored
      .into_iter()
      .flat_map(|list| list.members)
      .map(|member| teamcraft_client_api::model::ChannelMember {
        user: member.user.id,
        role: member.role,
      })
      .collect()
  }

  fn set_channel_role(&self, conversation: ConversationId, user: UserId, role: teamcraft_proto::ChannelRole) {
    self.command(CoreCommand::SetChannelRole {
      conversation,
      user,
      role,
    });
  }

  fn set_space_role(&self, space: SpaceId, user: UserId, role: SpaceRole) {
    self.command(CoreCommand::SetSpaceRole { space, user, role });
  }

  fn remove_space_member(&self, space: SpaceId, user: UserId) {
    self.command(CoreCommand::RemoveSpaceMember { space, user });
  }

  fn revoke_space_invite(&self, space: SpaceId, invite: InviteId) {
    self.command(CoreCommand::RevokeSpaceInvite { space, invite });
  }

  fn add_space_members(&self, space: SpaceId, users: Vec<UserId>, is_guest: bool) {
    self.command(CoreCommand::AddSpaceMembers {
      space,
      users,
      is_guest,
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
    | ConversationEvent::CallMoved { .. }
    | ConversationEvent::AdminJoined { .. }
    | ConversationEvent::Unknown => (MessageKind::User, sender),
  }
}

/// The UI's kind of a conversation.
fn view_kind(response: &ConversationResponse) -> ConversationKind {
  match (&response.kind, &response.visibility) {
    (ConversationKindProto::Dm, _) => ConversationKind::Direct,
    (ConversationKindProto::Group, _) => ConversationKind::Group,
    (_, _) if response.is_integration => ConversationKind::Integration,
    (_, ConversationVisibility::Public) => ConversationKind::Public,
    _ => ConversationKind::Private,
  }
}

/// Whether the user is in a channel of the space.
fn has_channel_in(conversations: &[ConversationResponse], space: SpaceId) -> bool {
  conversations.iter().any(|conversation| {
    conversation.maybe_space_id == Some(space) && conversation.maybe_my_membership.is_some()
  })
}

pub(crate) fn is_deleted(message: &StoredMessage) -> bool {
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

/// The newest of `messages` the server stored (outbox messages have no server ID yet): the read
/// position that covers everything, reactions included.
pub(crate) fn newest_server_id(messages: &[StoredMessage]) -> Option<MessageId> {
  messages
    .iter()
    .filter_map(|message| message.maybe_server_id)
    .max()
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
  fn the_viewer_cap_the_ui_checks_is_the_one_the_core_enforces() {
    assert_eq!(
      teamcraft_client_api::actions::MAX_VIEWER_BYTES,
      crate::MAX_VIEWER_BYTES
    );
  }

  #[test]
  fn a_reaction_is_the_newest_message_though_not_a_row() {
    let store = std::sync::Arc::new(MemoryStore::new());
    let conversation = id::<ConversationId>(10);
    let backend = CoreChatBackend::new(store.clone(), id(1), Box::new(ManualClock::new(1_000)));
    assert_eq!(backend.newest_message(conversation), None);
    let text = ClientMessageId::from_bytes([1; 16]);
    let stored = |client_message_id, server_id, content| StoredMessage {
      conversation_id: conversation,
      client_message_id,
      maybe_server_id: Some(id(server_id)),
      sender_user_id: id(2),
      maybe_thread_id: None,
      sent_at_ms: 1_000,
      content,
      is_edited: false,
      edits: Vec::new(),
      maybe_edit_id: None,
      is_pinned: false,
    };
    let body = MessageContent::Text {
      body: "hello".into(),
      mentions: Vec::new(),
      meta: Default::default(),
    };
    store.upsert_message(&stored(text, 101, body)).unwrap();
    let reaction = MessageContent::Reaction {
      target: text,
      emoji: "+1".into(),
      is_added: true,
    };
    store
      .upsert_message(&stored(ClientMessageId::from_bytes([2; 16]), 102, reaction))
      .unwrap();

    // #641: the reaction raises the unread count, so the open conversation must see it to read it.
    assert_eq!(backend.messages(conversation, 0, 10).len(), 1);
    assert_eq!(backend.newest_message(conversation), Some(id(102)));
  }

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
  fn a_roster_admin_adds_and_revokes_sources_and_the_announcements_stay_out_of_the_feed() {
    use teamcraft_client_api::model::NewIntegrationSource;
    use teamcraft_proto::{SourceChange, SourceId, SourceKind, SourceOptions, SourceTokenHash};

    use crate::workspace::{RosterMember, RosterRole, WorkspaceState};

    let store = std::sync::Arc::new(MemoryStore::new());
    let (me, colleague) = (id::<UserId>(1), id::<UserId>(2));
    let conversation = id::<ConversationId>(10);
    let backend = CoreChatBackend::new(store.clone(), me, Box::new(ManualClock::new(1_000)));
    assert!(!backend.can_manage_integration_sources(), "no roster, no sources");
    let mut workspace = WorkspaceState::default();
    workspace.roster.members.push(RosterMember {
      user_id: me,
      role: RosterRole::Admin,
      identity_key: teamcraft_proto::Base64Bytes::new(vec![0; 32]),
    });
    records::put_json(
      &*store,
      Table::Workspace,
      crate::client::WORKSPACE_KEY,
      &workspace,
    )
    .unwrap();
    assert!(backend.can_manage_integration_sources());

    let source = SourceId::random();
    backend.add_integration_source(
      conversation,
      NewIntegrationSource {
        id: source,
        name: "ci".into(),
        kind: SourceKind::Webhook,
        token_sha256: SourceTokenHash::from_bytes([3; 32]),
        options: SourceOptions::default(),
      },
    );
    let listed = backend.integration_sources(conversation);
    assert_eq!(listed.len(), 1);
    assert_eq!(
      (
        listed[0].name.as_str(),
        listed[0].created_by,
        listed[0].created_at_ms
      ),
      ("ci", me, 1_000)
    );
    assert_eq!(backend.message_count(conversation), 0, "hidden from the feed");
    assert_eq!(
      records::outbox(&store).unwrap().len(),
      1,
      "on its way to the members"
    );

    // A member outside the roster announces one and revokes the admin's: neither counts.
    for change in [
      SourceChange::Created {
        source: SourceId::random(),
        name: "sneaky".into(),
        kind: SourceKind::Log,
        token_sha256: SourceTokenHash::from_bytes([4; 32]),
        options: SourceOptions::default(),
      },
      SourceChange::Revoked { source },
    ] {
      crate::integration_sources::apply_source_change(&*store, conversation, colleague, &change, 2_000, None)
        .unwrap();
    }
    assert_eq!(backend.integration_sources(conversation).len(), 1);

    backend.revoke_integration_source(conversation, source);
    assert!(backend.integration_sources(conversation).is_empty());
    assert_eq!(records::outbox(&store).unwrap().len(), 2);
  }

  #[test]
  fn messages_waiting_for_keys_are_counted_per_conversation() {
    let store = MemoryStore::new();
    let conversation: ConversationId = id(7);
    for key in [
      format!("{conversation}/aa"),
      format!("{conversation}/bb"),
      "70/cc".to_string(),
    ] {
      store.put(Table::Undecrypted, &key, b"{}").unwrap();
    }
    let backend = CoreChatBackend::new(store, id(1), Box::new(ManualClock::new(0)));
    assert_eq!(backend.undecrypted_count(conversation), 2);
    assert_eq!(backend.undecrypted_count(id(70)), 1);
    assert_eq!(backend.undecrypted_count(id(8)), 0);
    // Not part of the feed, but the window says they exist (an otherwise empty conversation).
    let window = teamcraft_client_api::remote::MessageWindow::read(&backend, conversation, 0, 50);
    assert_eq!((window.total, window.undecrypted), (0, 2));
  }

  #[test]
  fn a_direct_message_is_named_after_its_participants_before_its_members_are_loaded() {
    let backend = CoreChatBackend::new(MemoryStore::new(), id(1), Box::new(ManualClock::new(0)));
    let participant = |raw: i64, name: &str| UserSummary {
      id: id(raw),
      maybe_username: None,
      display_name: name.into(),
      kind: teamcraft_proto::UserKind::Member,
      maybe_email: None,
      maybe_organization: None,
      maybe_has_devices: None,
      is_deactivated: false,
      is_deleted: false,
    };
    let direct = ConversationResponse {
      id: id(10),
      maybe_space_id: None,
      kind: ConversationKindProto::Dm,
      maybe_name: None,
      maybe_topic: None,
      maybe_description: None,
      visibility: ConversationVisibility::Private,
      history_policy: teamcraft_proto::HistoryPolicy::Full,
      maybe_category_id: None,
      is_archived: false,
      moderation: Default::default(),
      is_integration: false,
      contains_guests: false,
      maybe_organizer_user_id: None,
      maybe_my_permissions: None,
      maybe_my_membership: None,
      participants: vec![participant(1, "Alice Example"), participant(2, "Bob Example")],
      maybe_image: None,
    };
    let conversation = backend.to_conversation(&direct, &BTreeMap::new(), &MentionPreferences::default());
    assert_eq!(conversation.name, "Bob Example");
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
    assert!(
      backend.messages(conversation, 0, 10).is_empty(),
      "never sent, so nothing of it is left"
    );
  }

  #[cfg(not(target_arch = "wasm32"))]
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
        maybe_organizer_user_id: None,
        maybe_my_permissions: None,
        maybe_my_membership: None,
        participants: Vec::new(),
        maybe_image: None,
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
        maybe_has_devices: None,
        is_deactivated: false,
        is_deleted: false,
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
          maybe_edit_id: None,
          is_pinned: false,
        })
        .unwrap();
    }
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
