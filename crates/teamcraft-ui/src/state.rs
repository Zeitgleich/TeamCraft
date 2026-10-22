//! Shared application state, provided through Leptos context.

use std::rc::Rc;

use leptos::prelude::*;

use crate::backend::ChatBackend;
use crate::model::{Conversation, ConversationId, MessageId, SpaceId, User};
use crate::reactive::get_if_observed;
use crate::sections::build_sections;
use crate::settings::Settings;

/// What fills the main column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainView {
  Conversation(ConversationId),
  /// The threads inbox, optionally with one thread open in the side panel.
  Threads,
}

/// What the right-hand panel shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidePanel {
  Closed,
  Thread {
    conversation: ConversationId,
    root: MessageId,
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
}

#[derive(Clone, Copy)]
pub struct AppState {
  backend: StoredValue<Rc<dyn ChatBackend>, LocalStorage>,
  /// Bumped after every backend mutation; reading data through [`AppState::read`] subscribes to it.
  revision: RwSignal<u64>,
  pub space: RwSignal<SpaceId>,
  pub view: RwSignal<MainView>,
  pub panel: RwSignal<SidePanel>,
  pub overlay: RwSignal<Overlay>,
  /// Bumped when the composer sent a message, so the feed scrolls to it.
  pub jump_to_latest: RwSignal<u64>,
  pub settings: Settings,
}

impl AppState {
  pub fn new(
    backend: Rc<dyn ChatBackend>,
    settings: Settings,
    first_space: SpaceId,
    first: ConversationId,
  ) -> Self {
    Self {
      backend: StoredValue::new_local(backend),
      revision: RwSignal::new(0),
      space: RwSignal::new(first_space),
      view: RwSignal::new(MainView::Conversation(first)),
      panel: RwSignal::new(SidePanel::Closed),
      overlay: RwSignal::new(Overlay::None),
      jump_to_latest: RwSignal::new(0),
      settings,
    }
  }

  /// Reads from the backend and re-runs the calling reactive scope after any mutation.
  pub fn read<R>(&self, read: impl FnOnce(&dyn ChatBackend) -> R) -> R {
    get_if_observed(self.revision);
    self.backend.with_value(|backend| read(backend.as_ref()))
  }

  /// Applies a mutation and notifies every reader.
  pub fn write<R>(&self, write: impl FnOnce(&dyn ChatBackend) -> R) -> R {
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
      MainView::Threads => None,
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

  pub fn open_threads(&self) {
    self.view.set(MainView::Threads);
    self.panel.set(SidePanel::Closed);
    self.overlay.set(Overlay::None);
  }

  pub fn open_thread(&self, conversation: ConversationId, root: MessageId) {
    self.panel.set(SidePanel::Thread { conversation, root });
  }

  pub fn close_overlay(&self) {
    self.overlay.set(Overlay::None);
  }

  /// All conversations listed in the sidebar of the current space, in sidebar order.
  pub fn navigable_conversations(&self) -> Vec<Conversation> {
    let space = self.space.get();
    let show_unreads_group = self.settings.show_unreads_group.get();
    let all = self.read(|backend| {
      let mut list = backend.space_conversations(space);
      list.extend(backend.direct_conversations());
      list
    });
    build_sections(&all, show_unreads_group)
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
