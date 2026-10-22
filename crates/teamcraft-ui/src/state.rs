//! Shared application state, provided through Leptos context.

use std::rc::Rc;

use leptos::prelude::*;

use crate::admin::AdminSection;
use crate::backend::Backend;
use crate::calls::CallState;
use crate::model::{Conversation, ConversationId, MessageId, SpaceId, ThreadId, User, UserId, id_from_raw};
use crate::reactive::get_if_observed;
use crate::sections::build_sections;
use crate::security::DeviceApproval;
use crate::settings::Settings;

/// A message the user is replying to with a quote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplyTarget {
  pub conversation: ConversationId,
  pub message: MessageId,
  pub author_name: String,
  pub excerpt: String,
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
  /// Scheduled and instant meetings (signed-in builds).
  Meetings,
  /// People and user groups.
  Directory,
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
  NewChannel(SpaceId),
  BrowseChannels(SpaceId),
  ChannelSettings(ConversationId),
  SpaceSettings(SpaceId),
  /// The images of a conversation, starting at the one in this message.
  Gallery(ConversationId, MessageId),
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
  /// The person whose direct conversation was asked for and is not there yet.
  pub pending_dm: RwSignal<Option<UserId>>,
  /// Short messages shown over the app until dismissed (reminders).
  pub toasts: RwSignal<Vec<String>>,
  /// A conversation to open as soon as it appears in the lists (after joining it).
  pub pending_open: RwSignal<Option<ConversationId>>,
  pub jump_to_latest: RwSignal<u64>,
  /// A request to scroll a conversation's feed to a message index (jump to date).
  pub jump_to_index: RwSignal<Option<(ConversationId, usize)>>,
  pub settings: Settings,
}

impl AppState {
  /// `start` is the first space and conversation; without one the threads inbox opens.
  pub fn new(backend: Rc<dyn Backend>, settings: Settings, start: Option<(SpaceId, ConversationId)>) -> Self {
    let (space, view) = match start {
      Some((space, conversation)) => (space, MainView::Conversation(conversation)),
      None => (id_from_raw(0), MainView::Threads),
    };
    Self {
      backend: StoredValue::new_local(backend),
      revision: RwSignal::new(0),
      space: RwSignal::new(space),
      view: RwSignal::new(view),
      panel: RwSignal::new(SidePanel::Closed),
      overlay: RwSignal::new(Overlay::None),
      search_query: RwSignal::new(String::new()),
      call: CallState::new(),
      device_prompt: RwSignal::new(None),
      reply_target: RwSignal::new(None),
      pending_dm: RwSignal::new(None),
      toasts: RwSignal::new(Vec::new()),
      pending_open: RwSignal::new(None),
      jump_to_latest: RwSignal::new(0),
      jump_to_index: RwSignal::new(None),
      settings,
    }
  }

  /// Tells every reader that the backend changed on its own (data arrived over the network or from
  /// the desktop shell), so views re-read.
  pub fn backend_changed(&self) {
    self.revision.update(|revision| *revision += 1);
    if let Some(conversation) = self.pending_open.get_untracked()
      && self.read(|backend| backend.conversation(conversation)).is_some()
    {
      self.pending_open.set(None);
      self.open_conversation(conversation);
    }
    if let Some(user) = self.pending_dm.get_untracked()
      && let Some(conversation) = self.direct_with(user)
    {
      self.pending_dm.set(None);
      self.open_conversation(conversation);
    }
  }

  /// Opens the direct conversation with a person, creating it first if there is none yet.
  pub fn open_direct(&self, user: UserId) {
    if let Some(conversation) = self.direct_with(user) {
      self.open_conversation(conversation);
    } else {
      self.pending_dm.set(Some(user));
      self.write(|backend| backend.open_dm(user));
    }
  }

  fn direct_with(&self, user: UserId) -> Option<ConversationId> {
    let me = self.me().id;
    self.read(|backend| {
      backend
        .direct_conversations()
        .into_iter()
        .find(|conversation| {
          conversation.kind == crate::model::ConversationKind::Direct
            && conversation.member_ids.contains(&user)
            && conversation
              .member_ids
              .iter()
              .all(|member| *member == user || *member == me)
        })
        .map(|conversation| conversation.id)
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
      | MainView::Directory => None,
    }
  }

  pub fn open_conversation(&self, id: ConversationId) {
    let Some(conversation) = self.read(|backend| backend.conversation(id)) else {
      return;
    };
    if let Some(space) = conversation.maybe_space {
      self.space.set(space);
    }
    if let SidePanel::Thread {
      conversation: open, ..
    } = self.panel.get_untracked()
      && open != id
    {
      self.panel.set(SidePanel::Closed);
    }
    self.view.set(MainView::Conversation(id));
    self.overlay.set(Overlay::None);
    if conversation.unread_count > 0 || conversation.mention_count > 0 {
      self.write(|backend| backend.mark_read(id));
    }
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

  /// All conversations listed in the sidebar of the current space, in sidebar order.
  pub fn navigable_conversations(&self) -> Vec<Conversation> {
    let space = self.space.get();
    let options = self.settings.sidebar_options(usize::MAX);
    let all = self.read(|backend| {
      let mut list = backend.space_conversations(space);
      list.extend(backend.direct_conversations());
      list
    });
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
