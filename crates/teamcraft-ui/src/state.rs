//! Shared application state, provided through Leptos context.

use std::rc::Rc;

use leptos::prelude::*;

use crate::admin::AdminSection;
use crate::backend::Backend;
use crate::calls::CallState;
use crate::model::{Conversation, ConversationId, SpaceId, ThreadId, User, UserId, id_from_raw};
use crate::reactive::get_if_observed;
use crate::sections::build_sections;
use crate::security::DeviceApproval;
use crate::settings::Settings;

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
  pub jump_to_latest: RwSignal<u64>,
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
      jump_to_latest: RwSignal::new(0),
      settings,
    }
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
      MainView::Threads | MainView::Admin(_) | MainView::Search => None,
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
