//! Shared application state, provided through Leptos context.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use leptos::prelude::*;

use crate::admin::AdminSection;
use crate::backend::Backend;
use crate::calls::CallState;
use crate::model::{
  Conversation, ConversationId, MessageId, SpaceId, ThreadId, TimestampMs, User, UserId, id_from_raw,
};
use crate::navigation::{Step, divider_index, next_unread};
use crate::prefs::SidebarSort;
use crate::reactive::get_if_observed;
use crate::sections::{HomeFilter, apply_favorite_order, build_direct, build_sections};
use crate::security::DeviceApproval;
use crate::settings::Settings;
use teamcraft_client_api::actions::{NewChannel, NewSpace};

/// How many of the newest messages are searched for the first unread one.
const UNREAD_SCAN_LIMIT: usize = 500;

/// How long a message opened from search or a link stays highlighted.
const HIGHLIGHT_DURATION: Duration = Duration::from_millis(2_500);
/// How long a deleted message can be restored before it is deleted for everyone.
const UNDO_WINDOW: Duration = Duration::from_secs(6);

/// A short message over the app, optionally with an Undo button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
  pub id: u64,
  pub text: String,
  pub maybe_undo: Option<Undo>,
}

/// What the Undo button of a toast reverts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Undo {
  /// Keep a message whose deletion is still waiting.
  RestoreMessage(MessageId),
  /// Stay in a channel whose leaving is still waiting.
  StayInChannel(ConversationId),
}

/// A message the user is replying to with a quote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplyTarget {
  pub conversation: ConversationId,
  pub message: MessageId,
  pub author_name: String,
  pub excerpt: String,
}

/// A space or channel the user just created; it opens as soon as it shows up (the server answers
/// asynchronously and hands out the ID).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingCreation {
  Space {
    name: String,
    known: Vec<SpaceId>,
  },
  Channel {
    space: SpaceId,
    name: String,
    known: Vec<ConversationId>,
  },
}

/// Where the unread messages began when a conversation was opened: the feed shows a "New messages"
/// divider above this message index and starts there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnreadMarker {
  pub conversation: ConversationId,
  pub index: usize,
}

/// The entry of the space rail that is selected, and so which sidebar shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RailEntry {
  /// Home: the DMs and group DMs, which live outside spaces (Element's and Discord's home).
  Home,
  /// The space in [`AppState::space`] (or all spaces, in the grouped sidebar).
  Space,
}

/// What fills the main column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainView {
  Conversation(ConversationId),
  /// The threads inbox, optionally with one thread open in the side panel.
  Threads,
  /// The administration console (admins only).
  Admin(AdminSection),
  /// Message search; the query lives in [`AppState::search_query`].
  Search,
  /// Messages the user saved for later.
  Saved,
  /// Mentions, reactions to the user's messages and replies in their threads.
  Activity,
  /// The user's calls: missed, incoming and outgoing.
  Calls,
  /// Scheduled and instant meetings (signed-in builds).
  Meetings,
  /// People and user groups.
  Directory,
  /// First steps for an empty workspace or space: create a space or channel, invite colleagues.
  GettingStarted,
  /// Home with no conversation open.
  Home,
}

/// What the right-hand panel shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidePanel {
  Closed,
  Thread {
    conversation: ConversationId,
    thread: ThreadId,
  },
  Members,
  /// The pinned messages of the open conversation.
  Pins,
  /// Files shared in the open conversation.
  Files,
}

/// Modal and popover visibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overlay {
  None,
  QuickSwitcher,
  StatusMenu,
  CustomStatus,
  Settings,
  Shortcuts,
  /// Another device of the user asks for approval.
  DeviceApproval,
  /// QR code and code of this (new) device while it waits to be linked.
  LinkDevice,
  /// Compare the old and new identity key of someone in a conversation.
  VerifyKey(ConversationId),
  /// Compare the safety number with a contact (Settings > Devices and security).
  VerifyContact(UserId),
  /// The dialog that schedules a meeting.
  ScheduleMeeting,
  /// Pick a conversation to forward a message to.
  Forward(ConversationId, MessageId),
  NewSpace,
  /// Pick one or several people to write to (a DM or a group DM).
  NewMessage,
  /// Open spaces to join, and the way to create a new one.
  JoinSpace,
  NewChannel(SpaceId),
  BrowseChannels(SpaceId),
  ChannelSettings(ConversationId),
  SpaceSettings(SpaceId),
  /// The images of a conversation, starting at the one in this message.
  Gallery(ConversationId, MessageId),
  /// The versions of an edited message.
  EditHistory(ConversationId, MessageId),
  /// Edit the bookmarks bar of a conversation.
  Bookmarks(ConversationId),
  /// The signed-in user's profile form.
  EditProfile,
  /// Another person's profile card.
  UserProfile(UserId),
}

#[derive(Clone, Copy)]
pub struct AppState {
  backend: StoredValue<Rc<dyn Backend>, LocalStorage>,
  /// Bumped after every backend mutation; reading data through [`AppState::read`] subscribes to it.
  revision: RwSignal<u64>,
  pub space: RwSignal<SpaceId>,
  pub rail: RwSignal<RailEntry>,
  pub view: RwSignal<MainView>,
  pub panel: RwSignal<SidePanel>,
  pub overlay: RwSignal<Overlay>,
  /// Text of the search box, kept when leaving and re-entering the search page.
  pub search_query: RwSignal<String>,
  pub call: CallState,
  /// The login waiting for the user's approval, shown while the overlay is `DeviceApproval`.
  pub device_prompt: RwSignal<Option<DeviceApproval>>,
  /// Bumped when the composer sent a message, so the feed scrolls to it.
  /// The message the composer quotes in its next message.
  pub reply_target: RwSignal<Option<ReplyTarget>>,
  /// The people of a DM or group DM that was asked for and is not there yet.
  pub pending_dm: RwSignal<Option<Vec<UserId>>>,
  /// Short messages shown over the app until dismissed (reminders, undo).
  pub toasts: RwSignal<Vec<Toast>>,
  next_toast: StoredValue<u64>,
  /// Messages deleted with an undo window: hidden now, deleted when the window ends.
  pub hidden_messages: RwSignal<HashSet<MessageId>>,
  /// Channels left with an undo window: gone from the sidebar now, left when the window ends.
  pub hidden_conversations: RwSignal<HashSet<ConversationId>>,
  /// A conversation to open as soon as it appears in the lists (after joining it).
  pub pending_open: RwSignal<Option<ConversationId>>,
  /// A space or channel to open once it was created.
  pub pending_creation: RwSignal<Option<PendingCreation>>,
  /// Started on the getting-started page because nothing had loaded yet: the first conversation
  /// opens once the data arrives.
  awaits_first_conversation: RwSignal<bool>,
  pub jump_to_latest: RwSignal<u64>,
  /// A request to scroll a conversation's feed to a message index (jump to date).
  pub jump_to_index: RwSignal<Option<(ConversationId, usize)>>,
  /// The "New messages" position of the open conversation, taken before it was marked as read.
  pub unread_marker: RwSignal<Option<UnreadMarker>>,
  /// The first visible message of conversations the user left while scrolled up, to return there.
  pub scroll_anchors: StoredValue<HashMap<ConversationId, usize>>,
  /// A message just opened from search, a link or the activity list; its row lights up briefly.
  pub highlighted: RwSignal<Option<MessageId>>,
  /// A message the composer asked to edit (↑ in the empty composer); its row opens the editor.
  pub edit_request: RwSignal<Option<MessageId>>,
  pub settings: Settings,
}

impl AppState {
  /// `start` is the first space and conversation; without one the threads inbox opens.
  pub fn new(backend: Rc<dyn Backend>, settings: Settings, start: Option<(SpaceId, ConversationId)>) -> Self {
    let (space, view) = match start {
      Some((space, conversation)) => (space, MainView::Conversation(conversation)),
      None => (id_from_raw(0), MainView::GettingStarted),
    };
    Self {
      backend: StoredValue::new_local(backend),
      revision: RwSignal::new(0),
      space: RwSignal::new(space),
      rail: RwSignal::new(RailEntry::Space),
      view: RwSignal::new(view),
      panel: RwSignal::new(SidePanel::Closed),
      overlay: RwSignal::new(Overlay::None),
      search_query: RwSignal::new(String::new()),
      call: CallState::new(),
      device_prompt: RwSignal::new(None),
      reply_target: RwSignal::new(None),
      pending_dm: RwSignal::new(None),
      toasts: RwSignal::new(Vec::new()),
      next_toast: StoredValue::new(0),
      hidden_messages: RwSignal::new(HashSet::new()),
      hidden_conversations: RwSignal::new(HashSet::new()),
      pending_open: RwSignal::new(None),
      pending_creation: RwSignal::new(None),
      awaits_first_conversation: RwSignal::new(start.is_none()),
      jump_to_latest: RwSignal::new(0),
      jump_to_index: RwSignal::new(None),
      unread_marker: RwSignal::new(None),
      edit_request: RwSignal::new(None),
      highlighted: RwSignal::new(None),
      scroll_anchors: StoredValue::new(HashMap::new()),
      settings,
    }
  }

  /// Tells every reader that the backend changed on its own (data arrived over the network or from
  /// the desktop shell), so views re-read.
  pub fn backend_changed(&self) {
    self.revision.update(|revision| *revision += 1);
    self.open_pending_creation();
    self.open_first_loaded_conversation();
    if let Some(conversation) = self.pending_open.get_untracked()
      && self.read(|backend| backend.conversation(conversation)).is_some()
    {
      self.pending_open.set(None);
      self.open_conversation(conversation);
    }
    if let Some(users) = self.pending_dm.get_untracked()
      && let Some(conversation) = self.direct_with(&users)
    {
      self.pending_dm.set(None);
      self.open_conversation(conversation);
    }
  }

  /// Creates a space and switches to it once it exists.
  pub fn create_space(&self, space: NewSpace) {
    let known = self.read(|backend| backend.spaces().into_iter().map(|space| space.id).collect());
    self.pending_creation.set(Some(PendingCreation::Space {
      name: space.name.clone(),
      known,
    }));
    self.write(|backend| backend.create_space(space));
    self.open_pending_creation();
  }

  /// Joins an open space and switches to it once the server confirmed.
  pub fn join_space(&self, space: SpaceId, name: String) {
    let known = self.read(|backend| backend.spaces().into_iter().map(|space| space.id).collect());
    self
      .pending_creation
      .set(Some(PendingCreation::Space { name, known }));
    self.write(|backend| backend.join_space(space));
    self.open_pending_creation();
  }

  /// Creates a channel and opens it once it exists.
  pub fn create_channel(&self, channel: NewChannel) {
    let known = self.read(|backend| {
      backend
        .space_conversations(channel.space)
        .into_iter()
        .map(|conversation| conversation.id)
        .collect()
    });
    self.pending_creation.set(Some(PendingCreation::Channel {
      space: channel.space,
      name: channel.name.clone(),
      known,
    }));
    self.write(|backend| backend.create_channel_with(channel));
    self.open_pending_creation();
  }

  fn open_first_loaded_conversation(&self) {
    if !self.awaits_first_conversation.get_untracked()
      || self.view.get_untracked() != MainView::GettingStarted
    {
      return;
    }
    let first = self.backend.with_value(|backend| {
      backend
        .spaces()
        .into_iter()
        .find_map(|space| backend.space_conversations(space.id).into_iter().next())
        .or_else(|| backend.direct_conversations().into_iter().next())
    });
    if let Some(conversation) = first {
      self.open_conversation(conversation.id);
    }
  }

  fn open_pending_creation(&self) {
    let Some(pending) = self.pending_creation.get_untracked() else {
      return;
    };
    match pending {
      PendingCreation::Space { name, known } => {
        let found = self.backend.with_value(|backend| {
          backend
            .spaces()
            .into_iter()
            .find(|space| space.name == name && !known.contains(&space.id))
        });
        if let Some(space) = found {
          self.pending_creation.set(None);
          self.space.set(space.id);
          self.open_first_conversation_of(space.id);
        }
      },
      PendingCreation::Channel { space, name, known } => {
        let found = self.backend.with_value(|backend| {
          backend
            .space_conversations(space)
            .into_iter()
            .find(|conversation| conversation.name == name && !known.contains(&conversation.id))
        });
        if let Some(conversation) = found {
          self.pending_creation.set(None);
          self.open_conversation(conversation.id);
        }
      },
    }
  }

  /// Opens the first channel of a space, or shows its getting-started page when it has none.
  pub fn open_first_conversation_of(&self, space: SpaceId) {
    let first = self
      .backend
      .with_value(|backend| backend.space_conversations(space).into_iter().next());
    match first {
      Some(conversation) => self.open_conversation(conversation.id),
      None => {
        self.view.set(MainView::GettingStarted);
        self.panel.set(SidePanel::Closed);
      },
    }
  }

  /// Opens the direct conversation with a person, creating it first if there is none yet.
  pub fn open_direct(&self, user: UserId) {
    self.open_direct_with(vec![user]);
  }

  /// Opens the DM (one person) or group DM (several) with exactly these people, creating it first
  /// if there is none yet. The user themself may be in `users`.
  pub fn open_direct_with(&self, mut users: Vec<UserId>) {
    let me = self.me().id;
    users.retain(|user| *user != me);
    users.sort();
    users.dedup();
    if users.is_empty() {
      return;
    }
    if let Some(conversation) = self.direct_with(&users) {
      self.open_conversation(conversation);
      return;
    }
    self.pending_dm.set(Some(users.clone()));
    match users.as_slice() {
      [user] => {
        let user = *user;
        self.write(|backend| backend.open_dm(user));
      },
      _ => self.write(|backend| backend.open_group_dm(users)),
    }
  }

  fn direct_with(&self, users: &[UserId]) -> Option<ConversationId> {
    let me = self.me().id;
    let kind = if users.len() == 1 {
      crate::model::ConversationKind::Direct
    } else {
      crate::model::ConversationKind::Group
    };
    self.read(|backend| {
      backend
        .direct_conversations()
        .into_iter()
        .find(|conversation| conversation.kind == kind && has_exactly(&conversation.member_ids, users, me))
        .map(|conversation| conversation.id)
    })
  }

  /// Switches the sidebar to a space (all spaces in the grouped sidebar). Lands in its first channel
  /// unless the open page isn't tied to a space (threads, search, …) or already belongs to it.
  pub fn open_space(&self, space: SpaceId) {
    self.space.set(space);
    self.rail.set(RailEntry::Space);
    let is_grouped = self
      .settings
      .prefs
      .with_untracked(|prefs| prefs.sidebar.is_grouped_by_space);
    let belongs = match self.view.get_untracked() {
      MainView::Conversation(current) => {
        self
          .read(|backend| backend.conversation(current))
          .is_some_and(|conversation| {
            conversation.maybe_space.is_some() && (is_grouped || conversation.maybe_space == Some(space))
          })
      },
      MainView::Threads
      | MainView::Search
      | MainView::Meetings
      | MainView::Saved
      | MainView::Activity
      | MainView::Calls
      | MainView::Directory => true,
      MainView::Admin(_) | MainView::GettingStarted | MainView::Home => false,
    };
    if !belongs {
      self.open_first_conversation_of(space);
    }
  }

  /// Shows Home: the most recent DM, or the page's start when there is none.
  pub fn open_home(&self) {
    self.rail.set(RailEntry::Home);
    let is_direct_open = self
      .current_conversation_id()
      .and_then(|id| self.read(|backend| backend.conversation(id)))
      .is_some_and(|conversation| conversation.kind.is_direct_like());
    if is_direct_open {
      return;
    }
    let maybe_recent = self.direct_list().into_iter().next();
    match maybe_recent {
      Some(conversation) => self.open_conversation(conversation.id),
      None => {
        self.view.set(MainView::Home);
        self.panel.set(SidePanel::Closed);
      },
    }
  }

  /// DMs and group DMs as Home lists them (favorites first, then by recency
  /// or name), without the display limit.
  pub fn direct_list(&self) -> Vec<Conversation> {
    let sort = self.settings.prefs.with(|prefs| prefs.sidebar.dm_sort);
    build_direct(self.direct_entries(), sort, 0, "", HomeFilter::All)
      .sections
      .into_iter()
      .flat_map(|section| section.conversations)
      .collect()
  }

  /// DMs and group DMs (without the ones being left, mutes applied), each with the time of its
  /// newest message.
  pub fn direct_entries(&self) -> Vec<(Conversation, Option<TimestampMs>)> {
    let list = self.with_mutes(self.sidebar_list(self.read(|backend| backend.direct_conversations())));
    self.read(|backend| {
      list
        .into_iter()
        .map(|conversation| {
          let at = backend.last_activity(conversation.id);
          (conversation, at)
        })
        .collect()
    })
  }

  /// Reads from the backend and re-runs the calling reactive scope after any mutation.
  pub fn read<R>(&self, read: impl FnOnce(&dyn Backend) -> R) -> R {
    get_if_observed(self.revision);
    self.backend.with_value(|backend| read(backend.as_ref()))
  }

  /// Applies a mutation and notifies every reader.
  pub fn write<R>(&self, write: impl FnOnce(&dyn Backend) -> R) -> R {
    let result = self.backend.with_value(|backend| write(backend.as_ref()));
    self.revision.update(|revision| *revision += 1);
    result
  }

  pub fn me(&self) -> User {
    self.read(|backend| backend.me())
  }

  pub fn current_conversation_id(&self) -> Option<ConversationId> {
    match self.view.get() {
      MainView::Conversation(id) => Some(id),
      MainView::Threads
      | MainView::Admin(_)
      | MainView::Search
      | MainView::Meetings
      | MainView::Saved
      | MainView::Activity
      | MainView::Calls
      | MainView::Directory
      | MainView::GettingStarted
      | MainView::Home => None,
    }
  }

  /// Answers a ringing call on this device: opens its conversation and joins. The in-app card and
  /// the system notification's Accept share this.
  pub fn accept_call(&self, incoming: crate::calls::IncomingCall, with_video: bool) {
    let kind = match self
      .read(|backend| backend.conversation(incoming.conversation))
      .map(|conversation| conversation.kind)
    {
      Some(crate::model::ConversationKind::Direct) | None => crate::calls::CallKind::Direct,
      Some(_) => crate::calls::CallKind::Group,
    };
    self.call.incoming.set(None);
    self.call.is_camera_on.set(with_video);
    self.open_conversation(incoming.conversation);
    self.call.join(crate::calls::CallRequest {
      conversation: incoming.conversation,
      kind,
    });
  }

  pub fn open_conversation(&self, id: ConversationId) {
    self.awaits_first_conversation.set(false);
    let Some(conversation) = self.read(|backend| backend.conversation(id)) else {
      return;
    };
    if let Some(space) = conversation.maybe_space {
      self.space.set(space);
      self.rail.set(RailEntry::Space);
    } else if conversation.kind.is_direct_like() {
      self.rail.set(RailEntry::Home);
    }
    if let SidePanel::Thread {
      conversation: open, ..
    } = self.panel.get_untracked()
      && open != id
    {
      self.panel.set(SidePanel::Closed);
    }
    if self.view.get_untracked() != MainView::Conversation(id) {
      self.unread_marker.set(self.unread_marker_for(&conversation));
      self.settings.remember_conversation(id);
    }
    self.view.set(MainView::Conversation(id));
    self.overlay.set(Overlay::None);
    if conversation.unread_count > 0 || conversation.mention_count > 0 {
      self.write(|backend| backend.mark_read(id));
    }
  }

  /// Where the "New messages" divider of a conversation goes, before opening marks it as read.
  fn unread_marker_for(&self, conversation: &Conversation) -> Option<UnreadMarker> {
    let unread = conversation.unread_count as usize;
    if unread == 0 {
      return None;
    }
    let id = conversation.id;
    let (count, maybe_first, tail_start, tail_ids) = self.backend.with_value(|backend| {
      let count = backend.message_count(id);
      let tail_len = unread.saturating_mul(2).clamp(1, UNREAD_SCAN_LIMIT).min(count);
      let tail_start = count - tail_len;
      let tail_ids: Vec<MessageId> = backend
        .messages(id, tail_start, tail_len)
        .into_iter()
        .map(|message| message.id)
        .collect();
      (count, backend.first_unread(id), tail_start, tail_ids)
    });
    divider_index(count, unread, tail_start, &tail_ids, maybe_first).map(|index| UnreadMarker {
      conversation: id,
      index,
    })
  }

  /// Opens the previous or next conversation with unread messages in the sidebar (wrapping).
  pub fn step_unread(&self, step: Step) {
    let list: Vec<(ConversationId, bool)> = self
      .navigable_conversations()
      .into_iter()
      .map(|conversation| {
        let is_unread =
          conversation.mention_count > 0 || (conversation.unread_count > 0 && !conversation.is_muted);
        (conversation.id, is_unread)
      })
      .collect();
    if let Some(target) = next_unread(&list, self.current_conversation_id(), step) {
      self.open_conversation(target);
    }
  }

  /// Marks every conversation of the current list (the space's channels, or the DMs) as read.
  pub fn mark_all_read(&self) {
    let unread: Vec<ConversationId> = self
      .navigable_conversations()
      .into_iter()
      .filter(|conversation| conversation.unread_count > 0 || conversation.mention_count > 0)
      .map(|conversation| conversation.id)
      .collect();
    if unread.is_empty() {
      return;
    }
    self.write(|backend| {
      for conversation in unread {
        backend.mark_read(conversation);
      }
    });
  }

  /// Shows a toast until the user closes it.
  pub fn show_toast(&self, text: String) {
    self.push_toast(text, None);
  }

  pub fn dismiss_toast(&self, id: u64) {
    self.toasts.update(|list| list.retain(|toast| toast.id != id));
  }

  /// Hides an own message at once and deletes it after the undo window, unless Undo was pressed.
  pub fn delete_with_undo(&self, conversation: ConversationId, message: MessageId, text: String) {
    let state = *self;
    self.hidden_messages.update(|hidden| {
      hidden.insert(message);
    });
    let toast = self.push_toast(text, Some(Undo::RestoreMessage(message)));
    set_timeout(
      move || {
        state.dismiss_toast(toast);
        let mut is_still_deleted = false;
        state
          .hidden_messages
          .update(|hidden| is_still_deleted = hidden.remove(&message));
        if is_still_deleted {
          state.write(|backend| backend.delete_message(conversation, message));
        }
      },
      UNDO_WINDOW,
    );
  }

  /// Runs the Undo of a toast and closes it.
  pub fn undo(&self, toast: u64, undo: Undo) {
    match undo {
      Undo::RestoreMessage(message) => self.hidden_messages.update(|hidden| {
        hidden.remove(&message);
      }),
      Undo::StayInChannel(conversation) => {
        self.hidden_conversations.update(|hidden| {
          hidden.remove(&conversation);
        });
        self.open_conversation(conversation);
      },
    }
    self.dismiss_toast(toast);
  }

  /// Leaves a channel after the undo window; it disappears from the sidebar at once.
  pub fn leave_with_undo(&self, conversation: ConversationId, text: String) {
    let state = *self;
    if self.current_conversation_id() == Some(conversation) {
      let next = self
        .navigable_conversations()
        .into_iter()
        .find(|candidate| candidate.id != conversation);
      match next {
        Some(next) => self.open_conversation(next.id),
        None => self.open_threads(),
      }
    }
    self.hidden_conversations.update(|hidden| {
      hidden.insert(conversation);
    });
    let toast = self.push_toast(text, Some(Undo::StayInChannel(conversation)));
    set_timeout(
      move || {
        state.dismiss_toast(toast);
        let mut is_still_leaving = false;
        state
          .hidden_conversations
          .update(|hidden| is_still_leaving = hidden.remove(&conversation));
        if is_still_leaving {
          state.write(|backend| backend.leave_channel(conversation));
        }
      },
      UNDO_WINDOW,
    );
  }

  /// Conversations as the sidebar shows them: without the ones being left, favorites in the user's
  /// order.
  pub fn sidebar_list(&self, mut list: Vec<Conversation>) -> Vec<Conversation> {
    self
      .hidden_conversations
      .with(|hidden| list.retain(|conversation| !hidden.contains(&conversation.id)));
    if self.settings.prefs.with(|prefs| prefs.sidebar.sort) == SidebarSort::Default {
      self
        .settings
        .favorite_order
        .with(|order| apply_favorite_order(&mut list, order));
    }
    list
  }

  /// Conversations with the notification settings applied: one muted there (or through its space)
  /// looks muted in the lists too.
  pub fn with_mutes(&self, mut list: Vec<Conversation>) -> Vec<Conversation> {
    self.settings.prefs.with(|prefs| {
      for conversation in &mut list {
        let maybe_space = conversation.maybe_space.map(|space| space.to_string());
        conversation.is_muted |= prefs
          .notifications
          .is_muted_in(&conversation.id.to_string(), maybe_space.as_deref());
      }
    });
    list
  }

  fn push_toast(&self, text: String, maybe_undo: Option<Undo>) -> u64 {
    let id = self.next_toast.get_value();
    self.next_toast.set_value(id + 1);
    self
      .toasts
      .update(|list| list.push(Toast { id, text, maybe_undo }));
    id
  }

  pub fn open_admin(&self, section: AdminSection) {
    self.view.set(MainView::Admin(section));
    self.panel.set(SidePanel::Closed);
    self.overlay.set(Overlay::None);
  }

  /// Leaves the administration console and returns to the first conversation of the current space.
  pub fn exit_admin(&self) {
    let space = self.space.get_untracked();
    let first = self.read(|backend| {
      backend
        .space_conversations(space)
        .into_iter()
        .next()
        .map(|conv| conv.id)
    });
    match first {
      Some(id) => self.open_conversation(id),
      None => self.open_threads(),
    }
  }

  /// Opens the search page, optionally with a query already filled in.
  pub fn open_search(&self, maybe_query: Option<&str>) {
    if let Some(query) = maybe_query {
      self.search_query.set(query.to_owned());
    }
    self.view.set(MainView::Search);
    self.panel.set(SidePanel::Closed);
    self.overlay.set(Overlay::None);
  }

  pub fn open_directory(&self) {
    self.view.set(MainView::Directory);
    self.panel.set(SidePanel::Closed);
    self.overlay.set(Overlay::None);
  }

  pub fn open_saved(&self) {
    self.view.set(MainView::Saved);
    self.panel.set(SidePanel::Closed);
    self.overlay.set(Overlay::None);
  }

  pub fn open_activity(&self) {
    self.view.set(MainView::Activity);
    self.panel.set(SidePanel::Closed);
    self.overlay.set(Overlay::None);
  }

  pub fn open_calls(&self) {
    self.view.set(MainView::Calls);
    self.panel.set(SidePanel::Closed);
    self.overlay.set(Overlay::None);
    self.write(|backend| backend.refresh_call_history());
  }

  /// Shows a message: in its thread if it is a reply, else in the conversation, scrolled to it.
  pub fn open_message(
    &self,
    conversation: ConversationId,
    message: MessageId,
    maybe_thread: Option<ThreadId>,
  ) {
    self.open_conversation(conversation);
    self.highlight(message);
    if let Some(thread) = maybe_thread {
      self.open_thread(conversation, thread);
      return;
    }
    let index = self.read(|backend| {
      let count = backend.message_count(conversation);
      backend
        .messages(conversation, 0, count)
        .iter()
        .position(|candidate| candidate.id == message)
    });
    if let Some(index) = index {
      self.jump_to_index.set(Some((conversation, index)));
    }
  }

  /// Lights up a message for a moment so the eye finds it after a jump.
  fn highlight(&self, message: MessageId) {
    let state = *self;
    self.highlighted.set(Some(message));
    set_timeout(
      move || {
        if state.highlighted.get_untracked() == Some(message) {
          state.highlighted.set(None);
        }
      },
      HIGHLIGHT_DURATION,
    );
  }

  pub fn open_meetings(&self) {
    self.view.set(MainView::Meetings);
    self.panel.set(SidePanel::Closed);
    self.overlay.set(Overlay::None);
  }

  pub fn open_threads(&self) {
    self.view.set(MainView::Threads);
    self.panel.set(SidePanel::Closed);
    self.overlay.set(Overlay::None);
  }

  pub fn open_thread(&self, conversation: ConversationId, thread: ThreadId) {
    self.panel.set(SidePanel::Thread { conversation, thread });
  }

  pub fn close_overlay(&self) {
    self.overlay.set(Overlay::None);
  }

  /// All conversations of the current sidebar in its order: the DMs on Home,
  /// otherwise the channels of the current space (of every space in the grouped sidebar).
  pub fn navigable_conversations(&self) -> Vec<Conversation> {
    if self.rail.get() == RailEntry::Home {
      return self.direct_list();
    }
    let space = self.space.get();
    let is_grouped = self
      .settings
      .prefs
      .with(|prefs| prefs.sidebar.is_grouped_by_space);
    let options = self.settings.sidebar_options();
    let all = self.sidebar_list(self.read(|backend| {
      if is_grouped {
        backend
          .spaces()
          .into_iter()
          .flat_map(|space| backend.space_conversations(space.id))
          .collect()
      } else {
        backend.space_conversations(space)
      }
    }));
    build_sections(&all, &options)
      .into_iter()
      .flat_map(|section| section.conversations)
      .collect()
  }

  /// Moves to the previous (-1) or next (+1) conversation in the sidebar.
  pub fn step_conversation(&self, direction: i32) {
    let list = self.navigable_conversations();
    let Some(current) = self.current_conversation_id() else {
      return;
    };
    let Some(position) = list.iter().position(|conversation| conversation.id == current) else {
      return;
    };
    let target = position as i32 + direction;
    if let Some(next) = usize::try_from(target).ok().and_then(|index| list.get(index)) {
      self.open_conversation(next.id);
    }
  }
}

pub fn use_app_state() -> AppState {
  expect_context::<AppState>()
}

/// Whether `members` are exactly `users` plus (optionally) `me`.
fn has_exactly(members: &[UserId], users: &[UserId], me: UserId) -> bool {
  users.iter().all(|user| members.contains(user))
    && members
      .iter()
      .all(|member| *member == me || users.contains(member))
}
