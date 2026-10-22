//! [`ChatBackend`] over the client core that runs in the desktop shell (`apps/desktop`).
//!
//! The trait is synchronous, the core is not reachable synchronously from a webview. So this backend
//! keeps a mirror: the sidebar data (a [`ChatSnapshot`]) and the pages of each conversation feed that
//! were read so far. A read that misses the mirror returns nothing and starts a load; when it
//! arrives the UI is told to re-read through the change notifier. The shell emits `core-changed`
//! whenever its store changes and the mirror then refreshes what it holds.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use leptos::prelude::set_timeout;
use leptos::task::spawn_local;
use serde_json::json;
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::{
  Conversation, ConversationId, CustomStatus, Message, MessageId, PresenceStatus, Space, SpaceId, ThreadId,
  ThreadSummary, User, UserId,
};
use teamcraft_client_api::remote::{
  CORE_CHANGED_EVENT, ChatAction, ChatSnapshot, CoreStatus, EnrollRequest, MessageWindow,
};

use crate::auth::tauri::{call, listen};

/// Messages fetched per request. Feeds are read in 2-row windows by the list, so pages keep the
/// number of requests small while scrolling.
const PAGE: usize = 100;
/// Events arrive in bursts (a sync delivers many); refresh once after they settle.
const REFRESH_DELAY: Duration = Duration::from_millis(120);

/// Where the local client stands, as the shell reports it.
pub async fn core_status(user: UserId) -> Result<CoreStatus, String> {
  call("core_start", &json!({ "user": user })).await
}

/// Creates (or recovers) the keys of this device.
pub async fn enroll(request: &EnrollRequest) -> Result<(), String> {
  call("core_enroll", &json!({ "request": request })).await
}

pub async fn stop_core() -> Result<(), String> {
  call("core_stop", &json!({})).await
}

#[derive(Default)]
struct Feed {
  total: usize,
  rows: BTreeMap<usize, Message>,
}

struct Mirror {
  snapshot: ChatSnapshot,
  feeds: HashMap<ConversationId, Feed>,
  threads: HashMap<(ConversationId, ThreadId), Vec<Message>>,
  loading: HashSet<LoadKey>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum LoadKey {
  Page(ConversationId, usize),
  Thread(ConversationId, ThreadId),
}

type ChangeNotifier = Rc<dyn Fn()>;

#[derive(Clone)]
pub struct RemoteChat {
  mirror: Rc<RefCell<Mirror>>,
  notify: Rc<RefCell<Option<ChangeNotifier>>>,
  refresh_queued: Rc<Cell<bool>>,
}

impl RemoteChat {
  /// Reads the first snapshot and starts following the shell's change events.
  pub async fn connect() -> Result<Self, String> {
    let snapshot: ChatSnapshot = call("core_snapshot", &json!({})).await?;
    let backend = Self {
      mirror: Rc::new(RefCell::new(Mirror {
        snapshot,
        feeds: HashMap::new(),
        threads: HashMap::new(),
        loading: HashSet::new(),
      })),
      notify: Rc::new(RefCell::new(None)),
      refresh_queued: Rc::new(Cell::new(false)),
    };
    let listener = backend.clone();
    listen(CORE_CHANGED_EVENT, move || listener.queue_refresh());
    Ok(backend)
  }

  fn changed(&self) {
    let notify = self.notify.borrow().clone();
    if let Some(notify) = notify {
      notify();
    }
  }

  fn queue_refresh(&self) {
    if self.refresh_queued.replace(true) {
      return;
    }
    let backend = self.clone();
    set_timeout(
      move || {
        backend.refresh_queued.set(false);
        spawn_local(async move { backend.refresh().await });
      },
      REFRESH_DELAY,
    );
  }

  /// Re-reads the snapshot and every page held in the mirror, then tells the UI.
  async fn refresh(&self) {
    match call::<ChatSnapshot>("core_snapshot", &json!({})).await {
      Ok(snapshot) => {
        let pages: Vec<(ConversationId, usize)> = {
          let mut mirror = self.mirror.borrow_mut();
          for (conversation, total) in &snapshot.message_counts {
            if let Some(feed) = mirror.feeds.get_mut(conversation) {
              feed.total = *total;
            }
          }
          mirror.snapshot = snapshot;
          // Threads are re-read on demand; cached pages are overwritten in place so rows do not flicker.
          mirror.threads.clear();
          mirror
            .feeds
            .iter()
            .flat_map(|(conversation, feed)| {
              let last = feed.total.saturating_sub(1) / PAGE;
              feed
                .rows
                .keys()
                .map(|index| index / PAGE)
                .chain([last])
                .collect::<HashSet<_>>()
                .into_iter()
                .map(|page| (*conversation, page))
                .collect::<Vec<_>>()
            })
            .collect()
        };
        for (conversation, page) in pages {
          self.load_page(conversation, page);
        }
        self.changed();
      },
      Err(error) => tracing::warn!(%error, "could not refresh the chat snapshot"),
    }
  }

  fn load_page(&self, conversation: ConversationId, page: usize) {
    let key = LoadKey::Page(conversation, page);
    if !self.mirror.borrow_mut().loading.insert(key) {
      return;
    }
    let backend = self.clone();
    spawn_local(async move {
      let args = json!({ "conversation": conversation, "start": page * PAGE, "len": PAGE });
      let outcome = call::<MessageWindow>("core_messages", &args).await;
      {
        let mut mirror = backend.mirror.borrow_mut();
        mirror.loading.remove(&key);
        match outcome {
          Ok(window) => {
            let feed = mirror.feeds.entry(conversation).or_default();
            feed.total = window.total;
            for (offset, message) in window.messages.into_iter().enumerate() {
              feed.rows.insert(window.start + offset, message);
            }
          },
          Err(error) => {
            tracing::warn!(%error, %conversation, "could not load messages");
            return;
          },
        }
      }
      backend.changed();
    });
  }

  fn load_thread(&self, conversation: ConversationId, thread: ThreadId) {
    let key = LoadKey::Thread(conversation, thread);
    if !self.mirror.borrow_mut().loading.insert(key) {
      return;
    }
    let backend = self.clone();
    spawn_local(async move {
      let outcome = call::<Vec<Message>>(
        "core_thread",
        &json!({ "conversation": conversation, "thread": thread }),
      )
      .await;
      {
        let mut mirror = backend.mirror.borrow_mut();
        mirror.loading.remove(&key);
        match outcome {
          Ok(messages) => {
            mirror.threads.insert((conversation, thread), messages);
          },
          Err(error) => {
            tracing::warn!(%error, "could not load a thread");
            return;
          },
        }
      }
      backend.changed();
    });
  }

  fn act(&self, action: ChatAction) {
    spawn_local(async move {
      if let Err(error) = call::<()>("core_action", &json!({ "action": action })).await {
        tracing::warn!(%error, "action failed");
      }
    });
  }

  fn update_conversation(&self, conversation: ConversationId, change: impl FnOnce(&mut Conversation)) {
    let mut mirror = self.mirror.borrow_mut();
    if let Some(found) = mirror
      .snapshot
      .conversations
      .iter_mut()
      .find(|candidate| candidate.id == conversation)
    {
      change(found);
    }
  }
}

impl ChatBackend for RemoteChat {
  fn set_change_notifier(&self, notify: Rc<dyn Fn()>) {
    *self.notify.borrow_mut() = Some(notify);
  }

  fn me(&self) -> User {
    self.mirror.borrow().snapshot.me.clone()
  }

  fn user(&self, id: UserId) -> Option<User> {
    self
      .mirror
      .borrow()
      .snapshot
      .users
      .iter()
      .find(|user| user.id == id)
      .cloned()
  }

  fn users(&self) -> Vec<User> {
    self.mirror.borrow().snapshot.users.clone()
  }

  fn spaces(&self) -> Vec<Space> {
    self.mirror.borrow().snapshot.spaces.clone()
  }

  fn space_conversations(&self, space: SpaceId) -> Vec<Conversation> {
    let mirror = self.mirror.borrow();
    mirror
      .snapshot
      .conversations
      .iter()
      .filter(|conversation| conversation.maybe_space == Some(space))
      .cloned()
      .collect()
  }

  fn direct_conversations(&self) -> Vec<Conversation> {
    let mirror = self.mirror.borrow();
    mirror
      .snapshot
      .conversations
      .iter()
      .filter(|conversation| conversation.kind.is_direct_like())
      .cloned()
      .collect()
  }

  fn conversation(&self, id: ConversationId) -> Option<Conversation> {
    self
      .mirror
      .borrow()
      .snapshot
      .conversations
      .iter()
      .find(|conversation| conversation.id == id)
      .cloned()
  }

  fn message_count(&self, conversation: ConversationId) -> usize {
    let mirror = self.mirror.borrow();
    mirror
      .snapshot
      .message_counts
      .iter()
      .find(|(id, _)| *id == conversation)
      .map_or(0, |(_, total)| *total)
      .max(mirror.feeds.get(&conversation).map_or(0, |feed| feed.total))
  }

  fn messages(&self, conversation: ConversationId, start: usize, len: usize) -> Vec<Message> {
    let total = self.message_count(conversation);
    let end = total.min(start.saturating_add(len));
    if start >= end {
      return Vec::new();
    }
    let mut missing_pages = HashSet::new();
    let mut found = Vec::with_capacity(end - start);
    {
      let mirror = self.mirror.borrow();
      let feed = mirror.feeds.get(&conversation);
      for index in start..end {
        match feed.and_then(|feed| feed.rows.get(&index)) {
          Some(message) => found.push(message.clone()),
          None => {
            missing_pages.insert(index / PAGE);
          },
        }
      }
    }
    for page in &missing_pages {
      self.load_page(conversation, *page);
    }
    // All or nothing: a window with holes would pair messages with the wrong neighbours.
    if missing_pages.is_empty() {
      found
    } else {
      Vec::new()
    }
  }

  fn thread(&self, conversation: ConversationId, thread: ThreadId) -> Vec<Message> {
    let cached = self.mirror.borrow().threads.get(&(conversation, thread)).cloned();
    match cached {
      Some(messages) => messages,
      None => {
        self.load_thread(conversation, thread);
        Vec::new()
      },
    }
  }

  fn followed_threads(&self) -> Vec<ThreadSummary> {
    Vec::new()
  }

  fn post_message(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>, body: &str) {
    self.act(ChatAction::PostMessage {
      conversation,
      maybe_thread,
      body: body.to_owned(),
    });
  }

  fn toggle_reaction(&self, conversation: ConversationId, message: MessageId, emoji: &str) {
    self.act(ChatAction::ToggleReaction {
      conversation,
      message,
      emoji: emoji.to_owned(),
    });
  }

  fn mark_read(&self, conversation: ConversationId) {
    self.update_conversation(conversation, |found| {
      found.unread_count = 0;
      found.mention_count = 0;
    });
    self.act(ChatAction::MarkRead { conversation });
  }

  fn set_favorite(&self, conversation: ConversationId, is_favorite: bool) {
    self.update_conversation(conversation, |found| found.is_favorite = is_favorite);
    self.act(ChatAction::SetFavorite {
      conversation,
      is_favorite,
    });
  }

  fn set_presence(&self, presence: PresenceStatus) {
    self.mirror.borrow_mut().snapshot.me.presence = presence;
    self.act(ChatAction::SetPresence { presence });
  }

  fn set_custom_status(&self, maybe_status: Option<CustomStatus>) {
    self.mirror.borrow_mut().snapshot.me.maybe_custom_status = maybe_status.clone();
    self.act(ChatAction::SetCustomStatus { maybe_status });
  }
}
