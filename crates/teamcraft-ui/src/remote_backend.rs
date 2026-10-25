//! [`ChatBackend`] over the client core that runs in the desktop shell (`apps/desktop`).
//!
//! The trait is synchronous, the core is not reachable synchronously from a webview. So this backend
//! keeps a mirror: the sidebar data (a [`ChatSnapshot`]) and the pages of each conversation feed that
//! were read so far. A read that misses the mirror returns nothing and starts a load; when it
//! arrives the UI is told to re-read through the change notifier. The shell emits `core-changed`
//! whenever its store changes and the mirror then refreshes what it holds.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;
use teamcraft_client_api::calls::OrgBackgroundRule;
use teamcraft_proto::OrgBackgroundId;

use futures_util::future::{join_all, join3};
use leptos::prelude::set_timeout;
use leptos::task::spawn_local;
use serde_json::json;
use teamcraft_client_api::actions::{
  BrowsableChannel, CategoryInfo, ChannelChanges, FileInfo, LinkedMessage, MessageExtras, NewChannel,
  NewSpace, OutgoingMessage, PermalinkTarget, SavedItem, ShareLink, ShareLinkOptions, SpaceChanges,
  SpaceInvite,
};
use teamcraft_client_api::backend::{ChatBackend, SearchBackend};
use teamcraft_client_api::calls::BackgroundImageInfo;
use teamcraft_client_api::model::{
  Conversation, ConversationId, CustomStatus, InviteId, JoinableSpace, Message, MessageId, PendingInvite,
  PresenceStatus, Space, SpaceId, SpaceRole, SpaceRoster, ThreadId, ThreadSummary, User, UserId,
};
use teamcraft_client_api::remote::{
  CORE_CHANGED_EVENT, CORE_FAILED_EVENT, ChatAction, ChatAnswer, ChatQuery, ChatSnapshot, CoreConnection,
  CoreStatus, EnrollRequest, LinkStatus, MessageWindow, StoreHealth,
};
use teamcraft_client_api::search::{ParsedQuery, SavedSearch, SearchHit, SearchResults};
use teamcraft_client_api::snapshot_changes::{SnapshotChanges, SnapshotCursor};
use teamcraft_proto::{BackgroundId, CategoryId, HistoryPolicy, WebPushKeyResponse};

use crate::auth::Authenticator;
use crate::auth::tauri::{call, listen, listen_text};
use crate::feedback::{ActionFailure, ActionTicket, report_background_failure, report_failure};
use crate::route::Route;

/// Messages fetched per request. Feeds are read in 2-row windows by the list, so pages keep the
/// number of requests small while scrolling.
const PAGE: usize = 100;
/// Events arrive in bursts (a sync delivers many); refresh once after they settle.
const REFRESH_DELAY: Duration = Duration::from_millis(120);

thread_local! {
  /// The backend the core's events go to (#344). The page subscribes once; a new sign-in connects a
  /// new backend, which takes the events over, so listeners never stack and the old backend's
  /// mirror is not refreshed any more.
  static CORE_EVENTS: RefCell<EventRoute<RemoteChat>> = const { RefCell::new(EventRoute::new()) };
}

/// Where the local client stands, as the shell reports it.
pub async fn core_status(user: UserId) -> Result<CoreStatus, String> {
  call("core_start", &json!({ "user": user })).await
}

/// Where the core's gateway connection stands (answered without waiting for the core).
pub async fn connection_state() -> Result<CoreConnection, String> {
  call("core_connection", &json!({})).await
}

/// The network came back or the page became visible again: the core checks its connection now.
pub async fn network_changed() -> Result<(), String> {
  call("core_network_changed", &json!({})).await
}

/// Whether the browser saves the local data (e2ee.md §9.4). Browser only.
pub async fn store_health() -> Result<StoreHealth, String> {
  call("core_store_health", &json!({})).await
}

/// Creates (or recovers) the keys of this device.
pub async fn enroll(request: &EnrollRequest) -> Result<(), String> {
  call("core_enroll", &json!({ "request": request })).await
}

/// New device: shows a link code and waits for an existing device (e2ee.md §5).
pub async fn link_start(device_name: &str) -> Result<(), String> {
  call("core_link_start", &json!({ "device_name": device_name })).await
}

pub async fn link_status() -> Result<LinkStatus, String> {
  call("core_link_status", &json!({})).await
}

pub async fn link_cancel() -> Result<(), String> {
  call("core_link_cancel", &json!({})).await
}

/// Wraps the key backup with a new recovery passphrase.
pub async fn change_passphrase(passphrase: &str) -> Result<(), String> {
  call(
    "core_action",
    &json!({ "action": ChatAction::ChangePassphrase { passphrase: passphrase.to_owned() } }),
  )
  .await
}

/// Existing device: joins the link of a new device. The short code appears only on the new device.
pub async fn approval_start(code: &str) -> Result<(), String> {
  call("core_approval_start", &json!({ "code": code })).await
}

/// Existing device: the user typed the short code of the new device; the core checks it and then
/// hands the keys over.
pub async fn approval_confirm(short_code: &str) -> Result<(), String> {
  call("core_approval_confirm", &json!({ "short_code": short_code })).await
}

pub async fn approval_cancel() -> Result<(), String> {
  call("core_approval_cancel", &json!({})).await
}

/// Browser: subscribes this browser to Web Push with the server's key, so the service worker can show
/// notifications while no page is open.
pub async fn enable_web_push(auth: &dyn Authenticator) -> Result<(), String> {
  let reply = auth
    .api_get("/api/v1/push/web-key")
    .await
    .map_err(|error| error.to_string())?;
  if reply.status != 200 {
    return Err(format!("the server answered HTTP {}", reply.status));
  }
  let key: WebPushKeyResponse =
    serde_json::from_str(&reply.body).map_err(|error| format!("unreadable push key: {error}"))?;
  let Some(vapid_key) = key.maybe_public_key else {
    return Err("this server does not send Web Push".to_owned());
  };
  call("core_enable_push", &json!({ "vapid_key": vapid_key })).await
}

/// Sets the unreadable local database aside (desktop) or deletes it (browser) and opens a new one,
/// so the device can be set up again (issue #160).
pub async fn reset_store() -> Result<(), String> {
  call("core_reset_store", &json!({})).await
}

/// Signs the core out; the backend of this session gets no more events (#344).
pub async fn stop_core() -> Result<(), String> {
  // async-ok: a thread-local, not a signal.
  CORE_EVENTS.with(|route| route.borrow_mut().clear());
  call("core_stop", &json!({})).await
}

#[derive(Default)]
struct Feed {
  total: usize,
  rows: BTreeMap<usize, Message>,
  /// Read by the UI since it was last told about a change: a refresh reloads its pages.
  is_used: bool,
  /// The core changed while nobody read this feed: its pages are reloaded on the next read.
  is_stale: bool,
}

struct Mirror {
  snapshot: ChatSnapshot,
  /// How far the snapshot was read; the core answers the next refresh with what changed since.
  cursor: SnapshotCursor,
  feeds: HashMap<ConversationId, Feed>,
  threads: HashMap<(ConversationId, ThreadId), Vec<Message>>,
  /// Threads read since the UI was last told about a change; a refresh reloads only these.
  used_threads: HashSet<(ConversationId, ThreadId)>,
  loading: HashSet<LoadKey>,
  /// Loads asked for again while in flight: their answer may predate a change, so they are read
  /// once more when it arrives.
  outdated: HashSet<LoadKey>,
}

/// What a refresh reads again: only what the UI showed since its last change (issue #246).
#[derive(Default)]
struct RefreshPlan {
  is_snapshot_changed: bool,
  pages: Vec<(ConversationId, usize)>,
  threads: Vec<(ConversationId, ThreadId)>,
  queries: Vec<(String, ChatQuery)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum LoadKey {
  Page(ConversationId, usize),
  Thread(ConversationId, ThreadId),
}

type ChangeNotifier = Rc<dyn Fn()>;

/// The answer to one query, kept so the synchronous trait can read it. A change in the core marks
/// answers stale; they are shown as they are until the next read has fetched fresh ones.
#[derive(Default)]
struct Slot {
  maybe_answer: Option<ChatAnswer>,
  is_loading: bool,
  is_stale: bool,
  /// Read by the UI since it was last told about a change: a refresh asks again right away.
  is_used: bool,
  /// The core changed while an answer was in flight: it is asked again when that one arrives.
  is_outdated: bool,
}

/// The receiver of events the page subscribed to once (see `CORE_EVENTS`).
struct EventRoute<T> {
  is_subscribed: bool,
  maybe_target: Option<T>,
}

#[derive(Clone)]
pub struct RemoteChat {
  mirror: Rc<RefCell<Mirror>>,
  notify: Rc<RefCell<Option<ChangeNotifier>>>,
  /// The newest action failure, for the admin console's inline error (`last_action_error`).
  maybe_error: Rc<RefCell<Option<String>>>,
  refresh_queued: Rc<Cell<bool>>,
  answers: Rc<RefCell<HashMap<String, Slot>>>,
}

impl Feed {
  /// Takes a page read from the core; returns whether any row or the total changed.
  fn apply(&mut self, window: MessageWindow) -> bool {
    let mut is_changed = self.total != window.total;
    self.total = window.total;
    for (offset, message) in window.messages.into_iter().enumerate() {
      let index = window.start + offset;
      if self.rows.get(&index) != Some(&message) {
        self.rows.insert(index, message);
        is_changed = true;
      }
    }
    is_changed
  }

  /// The pages held, plus the last one (where new messages arrive).
  fn held_pages(&self) -> Vec<usize> {
    let last = self.total.saturating_sub(1) / PAGE;
    let pages: BTreeSet<usize> = self.rows.keys().map(|index| index / PAGE).chain([last]).collect();
    pages.into_iter().collect()
  }
}

impl Slot {
  /// Takes a fresh answer; returns whether it differs from the one held.
  fn settle(&mut self, answer: ChatAnswer) -> bool {
    self.is_loading = false;
    self.is_stale = false;
    if self.maybe_answer.as_ref() == Some(&answer) {
      return false;
    }
    self.maybe_answer = Some(answer);
    true
  }
}

impl<T: Clone> EventRoute<T> {
  const fn new() -> Self {
    Self {
      is_subscribed: false,
      maybe_target: None,
    }
  }

  /// Makes `target` the receiver; returns whether the caller still has to subscribe (the first
  /// time only).
  fn route_to(&mut self, target: T) -> bool {
    self.maybe_target = Some(target);
    !std::mem::replace(&mut self.is_subscribed, true)
  }

  fn target(&self) -> Option<T> {
    self.maybe_target.clone()
  }

  /// Drops the receiver (sign-out); the subscription stays for the next one.
  fn clear(&mut self) {
    self.maybe_target = None;
  }
}

impl RemoteChat {
  /// Reads the first snapshot and starts following the shell's change events.
  pub async fn connect() -> Result<Self, String> {
    let first: SnapshotChanges = call("core_snapshot_changes", &json!({ "since": null })).await?;
    let cursor = first.cursor;
    let snapshot = match first.maybe_full {
      Some(full) => *full,
      // Invariant: a reader without a cursor gets the whole snapshot; should a shell not send it,
      // the plain read does.
      None => call("core_snapshot", &json!({})).await?,
    };
    let backend = Self {
      mirror: Rc::new(RefCell::new(Mirror {
        snapshot,
        cursor,
        feeds: HashMap::new(),
        threads: HashMap::new(),
        used_threads: HashSet::new(),
        loading: HashSet::new(),
        outdated: HashSet::new(),
      })),
      notify: Rc::new(RefCell::new(None)),
      maybe_error: Rc::new(RefCell::new(None)),
      refresh_queued: Rc::new(Cell::new(false)),
      answers: Rc::new(RefCell::new(HashMap::new())),
    };
    // async-ok: a thread-local, not a signal.
    let must_subscribe = CORE_EVENTS.with(|route| route.borrow_mut().route_to(backend.clone()));
    if must_subscribe {
      listen(CORE_CHANGED_EVENT, || {
        // async-ok: a thread-local, not a signal.
        let maybe_target = CORE_EVENTS.with(|route| route.borrow().target());
        if let Some(target) = maybe_target {
          target.queue_refresh();
        }
      });
      // Desktop: failures of the core's background commands (the browser core reports its own).
      listen_text(CORE_FAILED_EVENT, |text| {
        report_background_failure(ActionFailure::from_wire(&text))
      });
    }
    Ok(backend)
  }

  /// The answer to a query, or `None` while it is being fetched (the UI re-reads when it arrives).
  /// A stale answer is returned as it is while a fresh one is fetched.
  fn ask(&self, query: ChatQuery) -> Option<ChatAnswer> {
    let key = serde_json::to_string(&query).ok()?;
    let (existing, must_load) = {
      let mut answers = self.answers.borrow_mut();
      let slot = answers.entry(key.clone()).or_default();
      slot.is_used = true;
      let must_load = !slot.is_loading && (slot.maybe_answer.is_none() || slot.is_stale);
      if must_load {
        slot.is_loading = true;
      }
      (slot.maybe_answer.clone(), must_load)
    };
    if must_load {
      let backend = self.clone();
      spawn_local(async move {
        if backend.fetch_answer(key, query).await {
          backend.changed();
        }
      });
    }
    existing
  }

  /// Everything fetched so far may be out of date.
  fn mark_answers_stale(&self) {
    for slot in self.answers.borrow_mut().values_mut() {
      slot.is_stale = true;
      // An answer in flight may predate the change: asked again when it arrives.
      slot.is_outdated |= slot.is_loading;
    }
  }

  /// Asks the core for one answer again; returns whether it differs from the one held. Asks once
  /// more when the core changed while the answer was in flight.
  async fn fetch_answer(&self, key: String, query: ChatQuery) -> bool {
    let mut is_changed = false;
    loop {
      let outcome = call::<ChatAnswer>("core_query", &json!({ "query": query })).await;
      let mut answers = self.answers.borrow_mut();
      let slot = answers.entry(key.clone()).or_default();
      match outcome {
        Ok(answer) => is_changed |= slot.settle(answer),
        Err(error) => {
          tracing::warn!(%error, "a query failed");
          // The old answer stays and nothing is asked again until something changes.
          slot.is_loading = false;
          slot.is_stale = false;
        },
      }
      if !std::mem::take(&mut slot.is_outdated) {
        return is_changed;
      }
      slot.is_loading = true;
    }
  }

  /// Tells the UI to read again. What it reads is marked used anew, so a later refresh reloads
  /// only what is still shown.
  fn changed(&self) {
    for slot in self.answers.borrow_mut().values_mut() {
      slot.is_used = false;
    }
    {
      let mut mirror = self.mirror.borrow_mut();
      mirror.used_threads.clear();
      for feed in mirror.feeds.values_mut() {
        feed.is_used = false;
      }
    }
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
        // async-ok: a Cell, not a signal.
        backend.refresh_queued.set(false);
        spawn_local(async move { backend.refresh().await });
      },
      REFRESH_DELAY,
    );
  }

  /// Takes what changed in the snapshot, re-reads what the UI showed of the conversations that
  /// changed, and tells the UI only when something differs: a change elsewhere in the core costs
  /// no re-render, and a change in one conversation no re-read of the others (issue #246).
  async fn refresh(&self) {
    let since = self.mirror.borrow().cursor;
    let changes = match call::<SnapshotChanges>("core_snapshot_changes", &json!({ "since": since })).await {
      Ok(changes) => changes,
      Err(error) => {
        tracing::warn!(%error, "could not refresh the chat snapshot");
        return;
      },
    };
    let plan = self.plan_refresh(changes);
    let pages = join_all(
      plan
        .pages
        .into_iter()
        .map(|(conversation, page)| self.fetch_page(conversation, page)),
    );
    let threads = join_all(
      plan
        .threads
        .into_iter()
        .map(|(conversation, thread)| self.fetch_thread(conversation, thread)),
    );
    let answers = join_all(
      plan
        .queries
        .into_iter()
        .map(|(key, query)| self.fetch_answer(key, query)),
    );
    let (pages, threads, answers) = join3(pages, threads, answers).await;
    let is_changed = plan.is_snapshot_changed
      || pages.into_iter().any(|is_changed| is_changed)
      || threads.into_iter().any(|is_changed| is_changed)
      || answers.into_iter().any(|is_changed| is_changed);
    if is_changed {
      self.changed();
    }
  }

  /// Takes what changed and picks what to reload: feeds and threads of the conversations that
  /// changed, if the UI read them since its last change; the others of those conversations are
  /// only marked stale and reloaded when read again. Queries are asked again whenever the core
  /// changed (they read more than the snapshot).
  fn plan_refresh(&self, changes: SnapshotChanges) -> RefreshPlan {
    let mut plan = RefreshPlan::default();
    let applied = {
      let mut mirror = self.mirror.borrow_mut();
      let mirror = &mut *mirror;
      mirror.cursor = changes.cursor;
      let applied = mirror.snapshot.apply(changes);
      plan.is_snapshot_changed = applied.is_changed;
      // The feeds the UI opened, not every conversation's count (issue #246).
      for (conversation, feed) in &mut mirror.feeds {
        if applied.touches(*conversation)
          && let Some(total) = mirror.snapshot.message_count(*conversation)
        {
          feed.total = total;
        }
      }
      let touched_threads: Vec<(ConversationId, ThreadId)> = mirror
        .used_threads
        .iter()
        .filter(|(conversation, _)| applied.touches(*conversation))
        .copied()
        .collect();
      let used_threads = &mirror.used_threads;
      mirror
        .threads
        .retain(|key, _| !applied.touches(key.0) || used_threads.contains(key));
      for key in &touched_threads {
        mirror.used_threads.remove(key);
      }
      plan.threads = touched_threads;
      for (conversation, feed) in &mut mirror.feeds {
        if !applied.touches(*conversation) {
          continue;
        }
        if feed.is_used {
          plan
            .pages
            .extend(feed.held_pages().into_iter().map(|page| (*conversation, page)));
        } else {
          feed.is_stale = true;
        }
      }
      applied
    };
    for (key, slot) in self.answers.borrow_mut().iter_mut() {
      let query = serde_json::from_str::<ChatQuery>(key);
      // A message link's preview only changes with its own conversation: other changes keep
      // the answer it has (#319, no rescans on every message elsewhere).
      if let Ok(ChatQuery::LinkedMessage { link }) = &query
        && slot.maybe_answer.is_some()
        && !slot.is_loading
        && Route::from_permalink(link)
          .and_then(|route| route.conversation())
          .is_some_and(|conversation| !applied.touches(conversation))
      {
        continue;
      }
      if slot.is_loading {
        slot.is_outdated = true;
        continue;
      }
      match (slot.is_used, query) {
        (true, Ok(query)) => {
          slot.is_loading = true;
          plan.queries.push((key.clone(), query));
        },
        _ => slot.is_stale = true,
      }
    }
    plan
  }

  fn load_page(&self, conversation: ConversationId, page: usize) {
    let backend = self.clone();
    spawn_local(async move {
      if backend.fetch_page(conversation, page).await {
        backend.changed();
      }
    });
  }

  /// Reads one page of a feed; returns whether it differs from the rows held.
  async fn fetch_page(&self, conversation: ConversationId, page: usize) -> bool {
    let key = LoadKey::Page(conversation, page);
    if !self.begin_load(key) {
      return false;
    }
    let mut is_changed = false;
    loop {
      let args = json!({ "conversation": conversation, "start": page * PAGE, "len": PAGE });
      let outcome = call::<MessageWindow>("core_messages", &args).await;
      let mut mirror = self.mirror.borrow_mut();
      match outcome {
        Ok(window) => is_changed |= mirror.feeds.entry(conversation).or_default().apply(window),
        Err(error) => tracing::warn!(%error, %conversation, "could not load messages"),
      }
      if !mirror.outdated.remove(&key) {
        mirror.loading.remove(&key);
        return is_changed;
      }
    }
  }

  /// Marks a load as in flight; `false` when it already is (it is then read once more, since the
  /// caller may know of a change the load in flight predates).
  fn begin_load(&self, key: LoadKey) -> bool {
    let mut mirror = self.mirror.borrow_mut();
    if mirror.loading.insert(key) {
      return true;
    }
    mirror.outdated.insert(key);
    false
  }

  fn load_thread(&self, conversation: ConversationId, thread: ThreadId) {
    let backend = self.clone();
    spawn_local(async move {
      if backend.fetch_thread(conversation, thread).await {
        backend.changed();
      }
    });
  }

  /// Reads one thread; returns whether it differs from the one held.
  async fn fetch_thread(&self, conversation: ConversationId, thread: ThreadId) -> bool {
    let key = LoadKey::Thread(conversation, thread);
    if !self.begin_load(key) {
      return false;
    }
    let mut is_changed = false;
    loop {
      let outcome = call::<Vec<Message>>(
        "core_thread",
        &json!({ "conversation": conversation, "thread": thread }),
      )
      .await;
      let mut mirror = self.mirror.borrow_mut();
      match outcome {
        Ok(messages) => {
          is_changed |= mirror.threads.get(&(conversation, thread)) != Some(&messages);
          mirror.threads.insert((conversation, thread), messages);
        },
        Err(error) => tracing::warn!(%error, "could not load a thread"),
      }
      if !mirror.outdated.remove(&key) {
        mirror.loading.remove(&key);
        return is_changed;
      }
    }
  }

  /// Hands an action to the core. A failure is shown to the user (issue #322); a button waiting
  /// for the action keeps its ticket until the core answered.
  fn act(&self, action: ChatAction) {
    let backend = self.clone();
    let maybe_ticket = ActionTicket::current();
    spawn_local(async move {
      let outcome = call::<()>("core_action", &json!({ "action": action })).await;
      *backend.maybe_error.borrow_mut() = outcome.as_ref().err().cloned();
      if let Err(error) = outcome {
        let failure = ActionFailure::from_wire(&error);
        if let Some(ticket) = &maybe_ticket {
          ticket.fail(failure.clone());
        }
        report_failure(failure);
      }
      backend.mark_answers_stale();
      backend.changed();
      // Dropped only now, so the button stays pending until the change is visible.
      drop(maybe_ticket);
    });
  }

  fn update_conversation(&self, conversation: ConversationId, change: impl FnOnce(&mut Conversation)) {
    let mut mirror = self.mirror.borrow_mut();
    if let Some(found) = mirror.snapshot.conversation_mut(conversation) {
      change(found);
    }
  }
}

impl ChatBackend for RemoteChat {
  fn can_sign_workspace(&self) -> bool {
    true
  }

  fn set_activity(&self, is_idle: bool) {
    self.act(ChatAction::SetActivity { is_idle });
  }

  fn set_attention(&self, is_attentive: bool) {
    self.act(ChatAction::SetAttention { is_attentive });
  }

  fn focus_conversation(&self, conversation: ConversationId) {
    spawn_local(async move {
      // A hint for the catch-up order; nothing for the user to act on if it is lost.
      if let Err(error) = call::<()>("core_focus", &json!({ "conversation": conversation })).await {
        tracing::debug!(%error, "the core did not take the focus");
      }
    });
  }

  fn search_backend(&self) -> Option<&dyn SearchBackend> {
    Some(self)
  }

  fn send_message(&self, message: OutgoingMessage) {
    self.act(ChatAction::SendMessage { message });
  }

  fn message_extras(&self, conversation: ConversationId, messages: &[MessageId]) -> Vec<MessageExtras> {
    match self.ask(ChatQuery::MessageExtras {
      conversation,
      messages: messages.to_vec(),
    }) {
      Some(ChatAnswer::MessageExtras { extras }) => extras,
      _ => Vec::new(),
    }
  }

  fn edit_message(&self, conversation: ConversationId, message: MessageId, body: &str) {
    self.act(ChatAction::EditMessage {
      conversation,
      message,
      body: body.to_owned(),
    });
  }

  fn delete_message(&self, conversation: ConversationId, message: MessageId) {
    self.act(ChatAction::DeleteMessage {
      conversation,
      message,
    });
  }

  fn forward_message(&self, from: ConversationId, message: MessageId, to: ConversationId, comment: &str) {
    self.act(ChatAction::ForwardMessage {
      from,
      message,
      to,
      comment: comment.to_owned(),
    });
  }

  fn set_pinned(&self, conversation: ConversationId, message: MessageId, is_pinned: bool) {
    self.act(ChatAction::SetPinned {
      conversation,
      message,
      is_pinned,
    });
  }

  fn pinned_messages(&self, conversation: ConversationId) -> Vec<Message> {
    match self.ask(ChatQuery::PinnedMessages { conversation }) {
      Some(ChatAnswer::Messages { messages }) => messages,
      _ => Vec::new(),
    }
  }

  fn set_saved(&self, conversation: ConversationId, message: MessageId, is_saved: bool) {
    self.act(ChatAction::SetSaved {
      conversation,
      message,
      is_saved,
    });
  }

  fn saved_items(&self) -> Vec<SavedItem> {
    match self.ask(ChatQuery::SavedItems) {
      Some(ChatAnswer::SavedItems { items }) => items,
      _ => Vec::new(),
    }
  }

  fn background_images(&self) -> Vec<BackgroundImageInfo> {
    match self.ask(ChatQuery::BackgroundImages) {
      Some(ChatAnswer::BackgroundImages { images }) => images,
      _ => Vec::new(),
    }
  }

  fn add_background_image(&self, name: &str, picture: Vec<u8>) {
    self.act(ChatAction::AddBackgroundImage {
      name: name.to_owned(),
      picture: picture.into(),
    });
  }

  fn add_org_background(&self, name: &str, picture: Vec<u8>) {
    self.act(ChatAction::AddOrgBackground {
      name: name.to_string(),
      picture: picture.into(),
    });
  }

  fn delete_background_image(&self, id: BackgroundId) {
    self.act(ChatAction::DeleteBackgroundImage { id });
  }

  fn delete_org_background(&self, id: OrgBackgroundId) {
    self.act(ChatAction::DeleteOrgBackground { id });
  }

  fn set_org_background_rule(&self, rule: OrgBackgroundRule) {
    self.act(ChatAction::SetOrgBackgroundRule { rule });
  }

  fn permalink(&self, conversation: ConversationId, message: MessageId) -> Option<String> {
    match self.ask(ChatQuery::Permalink {
      conversation,
      message,
    }) {
      Some(ChatAnswer::Link { maybe_link }) => maybe_link,
      _ => None,
    }
  }

  fn resolve_permalink(&self, link: &str) -> Option<PermalinkTarget> {
    match self.ask(ChatQuery::ResolvePermalink {
      link: link.to_owned(),
    }) {
      Some(ChatAnswer::PermalinkTarget { maybe_target }) => maybe_target,
      _ => None,
    }
  }

  fn linked_message(&self, link: &str) -> Option<LinkedMessage> {
    match self.ask(ChatQuery::LinkedMessage {
      link: link.to_owned(),
    }) {
      Some(ChatAnswer::LinkedMessage { maybe_linked }) => maybe_linked,
      _ => None,
    }
  }

  fn mark_unread(&self, conversation: ConversationId, from: MessageId) {
    self.act(ChatAction::MarkUnread { conversation, from });
  }

  fn is_waiting_for_device(&self, conversation: ConversationId) -> bool {
    matches!(
      self.ask(ChatQuery::IsWaitingForDevice { conversation }),
      Some(ChatAnswer::Flag { value: true })
    )
  }

  fn membership_issues(
    &self,
    conversation: ConversationId,
  ) -> Vec<teamcraft_client_api::model::MembershipIssue> {
    match self.ask(ChatQuery::MembershipIssues { conversation }) {
      Some(ChatAnswer::MembershipIssues { issues }) => issues,
      _ => Vec::new(),
    }
  }

  fn retry_removal(&self, conversation: ConversationId, user: UserId) {
    self.act(ChatAction::RetryRemoval { conversation, user });
  }

  fn retry_dm_setup(&self, conversation: ConversationId) {
    self.act(ChatAction::RetryDmSetup { conversation });
  }

  fn discard_removal(&self, conversation: ConversationId, user: UserId) {
    self.act(ChatAction::DiscardRemoval { conversation, user });
  }

  fn unconfirmed_removals(&self) -> Vec<(ConversationId, UserId)> {
    match self.ask(ChatQuery::UnconfirmedRemovals) {
      Some(ChatAnswer::UnconfirmedRemovals { removals }) => removals,
      _ => Vec::new(),
    }
  }

  fn first_unread(&self, conversation: ConversationId) -> Option<MessageId> {
    match self.ask(ChatQuery::FirstUnread { conversation }) {
      Some(ChatAnswer::Message { maybe_message }) => maybe_message,
      _ => None,
    }
  }

  fn draft(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>) -> Option<String> {
    match self.ask(ChatQuery::Draft {
      conversation,
      maybe_thread,
    }) {
      Some(ChatAnswer::Draft { maybe_body }) => maybe_body,
      _ => None,
    }
  }

  fn set_draft(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>, body: &str) {
    self.act(ChatAction::SetDraft {
      conversation,
      maybe_thread,
      body: body.to_owned(),
    });
  }

  fn acknowledge(&self, conversation: ConversationId, message: MessageId) {
    self.act(ChatAction::Acknowledge {
      conversation,
      message,
    });
  }

  fn retry_send(&self, conversation: ConversationId, message: MessageId) {
    self.act(ChatAction::RetrySend {
      conversation,
      message,
    });
  }

  fn discard_unsent(&self, conversation: ConversationId, message: MessageId) {
    self.act(ChatAction::DiscardUnsent {
      conversation,
      message,
    });
  }

  fn conversation_files(&self, conversation: ConversationId) -> Vec<FileInfo> {
    match self.ask(ChatQuery::ConversationFiles { conversation }) {
      Some(ChatAnswer::Files { files }) => files,
      _ => Vec::new(),
    }
  }

  fn conversation_images(
    &self,
    conversation: ConversationId,
    around: MessageId,
    max_messages: usize,
  ) -> Vec<FileInfo> {
    match self.ask(ChatQuery::ConversationImages {
      conversation,
      around,
      max_messages,
    }) {
      Some(ChatAnswer::Files { files }) => files,
      _ => Vec::new(),
    }
  }

  fn download_file(&self, conversation: ConversationId, message: MessageId) {
    self.act(ChatAction::DownloadFile {
      conversation,
      message,
    });
  }

  fn create_share_link(
    &self,
    conversation: ConversationId,
    message: MessageId,
    file_id: &str,
    options: ShareLinkOptions,
  ) {
    self.act(ChatAction::CreateShareLink {
      conversation,
      message,
      file_id: file_id.to_owned(),
      options,
    });
  }

  fn share_links(&self, conversation: ConversationId, message: MessageId, file_id: &str) -> Vec<ShareLink> {
    match self.ask(ChatQuery::ShareLinks {
      conversation,
      message,
      file_id: file_id.to_owned(),
    }) {
      Some(ChatAnswer::ShareLinks { links }) => links,
      _ => Vec::new(),
    }
  }

  fn max_upload_bytes(&self) -> Option<u64> {
    match self.ask(ChatQuery::MaxUploadBytes) {
      Some(ChatAnswer::Bytes { maybe_bytes }) => maybe_bytes,
      _ => None,
    }
  }

  fn create_space(&self, space: NewSpace) {
    self.act(ChatAction::CreateSpace { space });
  }

  fn update_space(&self, space: SpaceId, changes: SpaceChanges) {
    self.act(ChatAction::UpdateSpace { space, changes });
  }

  fn create_channel_with(&self, channel: NewChannel) {
    self.act(ChatAction::CreateChannelWith { channel });
  }

  fn update_channel(&self, conversation: ConversationId, changes: ChannelChanges) {
    self.act(ChatAction::UpdateChannel {
      conversation,
      changes,
    });
  }

  fn browse_channels(&self, space: SpaceId) -> Vec<BrowsableChannel> {
    match self.ask(ChatQuery::BrowseChannels { space }) {
      Some(ChatAnswer::Channels { channels }) => channels,
      _ => Vec::new(),
    }
  }

  fn join_channel(&self, conversation: ConversationId) {
    self.act(ChatAction::JoinChannel { conversation });
  }

  fn joinable_spaces(&self) -> Vec<JoinableSpace> {
    match self.ask(ChatQuery::JoinableSpaces) {
      Some(ChatAnswer::JoinableSpaces { spaces }) => spaces,
      _ => Vec::new(),
    }
  }

  fn join_space(&self, space: SpaceId) {
    self.act(ChatAction::JoinSpace { space });
  }

  fn leave_channel(&self, conversation: ConversationId) {
    self.act(ChatAction::LeaveChannel { conversation });
  }

  fn categories(&self, space: SpaceId) -> Vec<CategoryInfo> {
    match self.ask(ChatQuery::Categories { space }) {
      Some(ChatAnswer::Categories { categories }) => categories,
      _ => Vec::new(),
    }
  }

  fn create_category(&self, space: SpaceId, name: &str) {
    self.act(ChatAction::CreateCategory {
      space,
      name: name.to_owned(),
    });
  }

  fn rename_category(&self, space: SpaceId, category: CategoryId, name: &str) {
    self.act(ChatAction::RenameCategory {
      space,
      category,
      name: name.to_owned(),
    });
  }

  fn delete_category(&self, space: SpaceId, category: CategoryId) {
    self.act(ChatAction::DeleteCategory { space, category });
  }

  fn add_member_with_history(&self, conversation: ConversationId, user: UserId, history: HistoryPolicy) {
    self.act(ChatAction::AddMemberWithHistory {
      conversation,
      user,
      history,
    });
  }

  fn invite_to_space(&self, invite: SpaceInvite) {
    self.act(ChatAction::InviteToSpace { invite });
  }

  fn space_members(&self, space: SpaceId) -> SpaceRoster {
    match self.ask(ChatQuery::SpaceMembers { space }) {
      Some(ChatAnswer::SpaceMembers { roster }) => roster,
      _ => SpaceRoster::Loading,
    }
  }

  fn channel_roles(&self, conversation: ConversationId) -> Vec<teamcraft_client_api::model::ChannelMember> {
    match self.ask(ChatQuery::ChannelRoles { conversation }) {
      Some(ChatAnswer::ChannelRoles { members }) => members,
      _ => Vec::new(),
    }
  }

  fn set_channel_role(
    &self,
    conversation: ConversationId,
    user: UserId,
    role: teamcraft_client_api::model::ChannelRole,
  ) {
    self.act(ChatAction::SetChannelRole {
      conversation,
      user,
      role,
    });
  }

  fn space_invites(&self, space: SpaceId) -> Vec<PendingInvite> {
    match self.ask(ChatQuery::SpaceInvites { space }) {
      Some(ChatAnswer::SpaceInvites { invites }) => invites,
      _ => Vec::new(),
    }
  }

  fn add_people_to_channel(
    &self,
    conversation: ConversationId,
    people: Vec<(UserId, bool)>,
    history: HistoryPolicy,
    joins_space: bool,
  ) {
    self.act(ChatAction::AddPeopleToChannel {
      conversation,
      people,
      history,
      joins_space,
    });
  }

  fn refresh_space_roster(&self, space: SpaceId) {
    self.act(ChatAction::RefreshSpaceRoster { space });
  }

  fn set_space_role(&self, space: SpaceId, user: UserId, role: SpaceRole) {
    self.act(ChatAction::SetSpaceRole { space, user, role });
  }

  fn remove_space_member(&self, space: SpaceId, user: UserId) {
    self.act(ChatAction::RemoveSpaceMember { space, user });
  }

  fn revoke_space_invite(&self, space: SpaceId, invite: InviteId) {
    self.act(ChatAction::RevokeSpaceInvite { space, invite });
  }

  fn add_space_members(&self, space: SpaceId, users: Vec<UserId>, is_guest: bool) {
    self.act(ChatAction::AddSpaceMembers {
      space,
      users,
      is_guest,
    });
  }

  fn has_external_people(&self, conversation: ConversationId) -> bool {
    matches!(
      self.ask(ChatQuery::HasExternalPeople { conversation }),
      Some(ChatAnswer::Flag { value: true })
    )
  }

  fn activity(&self) -> teamcraft_client_api::activity::ActivityFeed {
    match self.ask(ChatQuery::Activity) {
      Some(ChatAnswer::Activity { feed }) => feed,
      _ => Default::default(),
    }
  }

  fn mark_activity_read(&self) {
    self.act(ChatAction::MarkActivityRead);
  }

  fn last_seen(&self, user: UserId) -> Option<teamcraft_client_api::model::TimestampMs> {
    match self.ask(ChatQuery::LastSeen { user }) {
      Some(ChatAnswer::LastSeen { maybe_at }) => maybe_at,
      _ => None,
    }
  }

  fn privacy(&self) -> teamcraft_client_api::privacy::PrivacySettings {
    match self.ask(ChatQuery::Privacy) {
      Some(ChatAnswer::Privacy { settings }) => settings,
      _ => Default::default(),
    }
  }

  fn set_last_seen_visibility(&self, visibility: teamcraft_client_api::privacy::LastSeenVisibility) {
    self.act(ChatAction::SetLastSeenVisibility { visibility });
  }

  fn call_history(&self) -> Vec<teamcraft_client_api::calls::CallHistoryItem> {
    match self.ask(ChatQuery::CallHistory) {
      Some(ChatAnswer::CallHistory { calls }) => calls,
      _ => Vec::new(),
    }
  }

  fn refresh_call_history(&self) {
    self.act(ChatAction::RefreshCallHistory);
  }

  fn own_devices(&self) -> Vec<teamcraft_client_api::admin::DeviceInfo> {
    match self.ask(ChatQuery::OwnDevices) {
      Some(ChatAnswer::Devices { devices }) => devices,
      _ => Vec::new(),
    }
  }

  fn refresh_own_devices(&self) {
    self.act(ChatAction::RefreshOwnDevices);
  }

  fn revoke_own_device(&self, device: teamcraft_proto::DeviceId) {
    self.act(ChatAction::RevokeOwnDevice { device });
  }

  fn security_keys(&self) -> teamcraft_client_api::remote::SecuritySnapshot {
    self.mirror.borrow().snapshot.security.clone()
  }

  fn owner_roster(&self) -> Option<teamcraft_client_api::admin::OwnerRosterStatus> {
    self.mirror.borrow().snapshot.security.maybe_owners.clone()
  }

  fn push_content_mode(&self) -> teamcraft_proto::PushContentMode {
    self
      .mirror
      .borrow()
      .snapshot
      .security
      .maybe_push_content
      .clone()
      .unwrap_or(teamcraft_proto::PushContentMode::Generic)
  }

  fn accept_key_change(&self, user: UserId) {
    self.act(ChatAction::AcceptKeyChange { user });
    // The pin changed in the core; read it back right away so the banner goes.
    self.queue_refresh();
  }

  fn publish_policy(
    &self,
    base: teamcraft_proto::WorkspacePolicy,
    policy: teamcraft_proto::WorkspacePolicy,
    owners: Vec<UserId>,
    admins: Vec<UserId>,
  ) {
    self.act(ChatAction::PublishPolicy {
      base,
      policy,
      owners,
      admins,
    });
  }

  fn publish_roster(&self, base_version: u64, members: Vec<teamcraft_client_api::admin::RosterEntry>) {
    self.act(ChatAction::PublishRoster {
      base_version,
      members,
    });
  }

  fn admin_add_self(&self, conversation: ConversationId) {
    self.act(ChatAction::AdminJoinChannel { conversation });
  }

  fn last_action_error(&self) -> Option<String> {
    self.maybe_error.borrow().clone()
  }

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

  fn search_people(&self, query: &str) -> Vec<User> {
    match self.ask(ChatQuery::SearchPeople {
      query: query.to_owned(),
    }) {
      Some(ChatAnswer::People { people }) => people,
      // Until the core answers: who the mirror knows.
      _ => teamcraft_client_api::backend::filter_people(self.users(), query),
    }
  }

  fn live_snapshot(&self) -> Option<teamcraft_client_api::live::LiveSnapshot> {
    self.mirror.borrow().snapshot.maybe_live.clone()
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
    self.mirror.borrow().snapshot.conversation(id).cloned()
  }

  fn message_count(&self, conversation: ConversationId) -> usize {
    let mirror = self.mirror.borrow();
    mirror
      .snapshot
      .message_count(conversation)
      .unwrap_or(0)
      .max(mirror.feeds.get(&conversation).map_or(0, |feed| feed.total))
  }

  fn last_activity(&self, conversation: ConversationId) -> Option<teamcraft_client_api::model::TimestampMs> {
    self.mirror.borrow().snapshot.last_activity_of(conversation)
  }

  fn messages(&self, conversation: ConversationId, start: usize, len: usize) -> Vec<Message> {
    let total = self.message_count(conversation);
    let end = total.min(start.saturating_add(len));
    if start >= end {
      return Vec::new();
    }
    let mut missing_pages = HashSet::new();
    let mut found = Vec::with_capacity(end - start);
    let mut stale_pages = Vec::new();
    {
      let mut mirror = self.mirror.borrow_mut();
      let feed = mirror.feeds.get_mut(&conversation).map(|feed| {
        feed.is_used = true;
        if std::mem::take(&mut feed.is_stale) {
          stale_pages = feed.held_pages();
        }
        &*feed
      });
      for index in start..end {
        match feed.and_then(|feed| feed.rows.get(&index)) {
          Some(message) => found.push(message.clone()),
          None => {
            missing_pages.insert(index / PAGE);
          },
        }
      }
    }
    for page in missing_pages.iter().chain(&stale_pages) {
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
    let cached = {
      let mut mirror = self.mirror.borrow_mut();
      mirror.used_threads.insert((conversation, thread));
      mirror.threads.get(&(conversation, thread)).cloned()
    };
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

  fn set_bookmarks(
    &self,
    conversation: ConversationId,
    bookmarks: Vec<teamcraft_client_api::model::Bookmark>,
  ) {
    self.update_conversation(conversation, |found| found.bookmarks = bookmarks.clone());
    self.act(ChatAction::SetBookmarks {
      conversation,
      bookmarks,
    });
  }

  fn remove_member(&self, conversation: ConversationId, user: UserId) {
    self.act(ChatAction::RemoveMember { conversation, user });
  }

  fn open_dm(&self, user: UserId) {
    self.act(ChatAction::OpenDm { user });
  }

  fn open_group_dm(&self, users: Vec<UserId>) {
    self.act(ChatAction::OpenGroupDm { users });
  }

  fn set_favorite(&self, conversation: ConversationId, is_favorite: bool) {
    self.update_conversation(conversation, |found| found.is_favorite = is_favorite);
    self.act(ChatAction::SetFavorite {
      conversation,
      is_favorite,
    });
  }

  fn set_presence(&self, presence: PresenceStatus) {
    self.set_presence_until(presence, None);
  }

  fn set_presence_until(
    &self,
    presence: PresenceStatus,
    maybe_until: Option<teamcraft_client_api::model::TimestampMs>,
  ) {
    self.mirror.borrow_mut().snapshot.me.presence = presence;
    // Choosing "Online" is activity on this device: it ends an away timeout the server applied
    // (the server can't tell which device a presence request came from).
    if presence == PresenceStatus::Online {
      self.act(ChatAction::SetActivity { is_idle: false });
    }
    self.act(ChatAction::SetPresence {
      presence,
      maybe_until,
    });
  }

  fn presence_policy(&self) -> teamcraft_proto::PresencePolicy {
    self
      .mirror
      .borrow()
      .snapshot
      .security
      .maybe_presence_policy
      .clone()
      .unwrap_or_default()
  }

  fn set_custom_status(&self, maybe_status: Option<CustomStatus>) {
    self.mirror.borrow_mut().snapshot.me.maybe_custom_status = maybe_status.clone();
    self.act(ChatAction::SetCustomStatus { maybe_status });
  }
}

impl SearchBackend for RemoteChat {
  fn search(&self, query: &ParsedQuery) -> SearchResults {
    let raw = query
      .tokens
      .iter()
      .map(|token| token.raw.as_str())
      .collect::<Vec<_>>()
      .join(" ");
    match self.ask(ChatQuery::Search { query: raw }) {
      Some(ChatAnswer::SearchResults { results }) => results,
      _ => SearchResults::default(),
    }
  }

  fn recent_mentions(&self) -> Vec<SearchHit> {
    match self.ask(ChatQuery::RecentMentions) {
      Some(ChatAnswer::SearchHits { hits }) => hits,
      _ => Vec::new(),
    }
  }

  fn saved_searches(&self) -> Vec<SavedSearch> {
    match self.ask(ChatQuery::SavedSearches) {
      Some(ChatAnswer::SavedSearches { searches }) => searches,
      _ => Vec::new(),
    }
  }

  fn save_search(&self, name: &str, query: &str) {
    self.act(ChatAction::SaveSearch {
      name: name.to_owned(),
      query: query.to_owned(),
    });
  }

  fn delete_saved_search(&self, id: u64) {
    self.act(ChatAction::DeleteSavedSearch { id });
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{MessageKind, id_from_raw};

  mod event_route {
    use super::*;

    /// #344: a reconnect (a new sign-in) must not stack a second listener on the shell's events.
    #[test]
    fn a_reconnect_takes_the_events_over_without_subscribing_again() {
      let mut route = EventRoute::new();
      assert!(route.route_to("first"), "the first connect subscribes");
      assert!(!route.route_to("second"), "a reconnect does not subscribe again");
      assert_eq!(
        route.target(),
        Some("second"),
        "events go to the newest backend only"
      );
    }

    #[test]
    fn after_sign_out_events_reach_no_backend_until_the_next_connect() {
      let mut route = EventRoute::new();
      route.route_to("first");
      route.clear();
      assert_eq!(route.target(), None);
      assert!(!route.route_to("second"), "still subscribed");
      assert_eq!(route.target(), Some("second"));
    }
  }

  mod feed {
    use super::*;

    #[test]
    fn the_same_page_again_is_no_change() {
      let mut feed = Feed::default();
      assert!(feed.apply(window(3, &["a", "b", "c"])));
      assert!(!feed.apply(window(3, &["a", "b", "c"])));
    }

    #[test]
    fn an_edited_row_or_a_new_total_is_a_change() {
      let mut feed = Feed::default();
      feed.apply(window(3, &["a", "b", "c"]));
      assert!(feed.apply(window(3, &["a", "B", "c"])));
      assert!(feed.apply(window(4, &["a", "B", "c"])));
    }

    #[test]
    fn held_pages_include_the_last_page() {
      let mut feed = Feed::default();
      feed.apply(window(PAGE * 3, &["a"]));
      assert_eq!(feed.held_pages(), vec![0, 2]);
    }
  }

  mod refresh_plan {
    use super::*;

    /// A thread reply that stays in its thread changes no conversation entry; the shown thread and
    /// feed (the root's reply count) are read again all the same.
    #[test]
    fn a_changed_feed_reloads_the_shown_thread_and_pages() {
      let (conversation, thread) = (id_from_raw(7), crate::model::thread_id_of(id_from_raw(70)));
      let backend = backend();
      {
        let mut mirror = backend.mirror.borrow_mut();
        mirror.threads.insert((conversation, thread), Vec::new());
        mirror.used_threads.insert((conversation, thread));
        let feed = mirror.feeds.entry(conversation).or_default();
        feed.apply(window(3, &["a", "b", "c"]));
        feed.is_used = true;
      }

      let plan = backend.plan_refresh(SnapshotChanges {
        feeds: vec![conversation],
        ..SnapshotChanges::default()
      });

      assert!(!plan.is_snapshot_changed, "the sidebar has nothing to re-render");
      assert_eq!(plan.threads, vec![(conversation, thread)]);
      assert_eq!(plan.pages, vec![(conversation, 0)]);
    }

    #[test]
    fn a_changed_feed_elsewhere_reloads_nothing_here() {
      let (conversation, thread) = (id_from_raw(7), crate::model::thread_id_of(id_from_raw(70)));
      let backend = backend();
      {
        let mut mirror = backend.mirror.borrow_mut();
        mirror.threads.insert((conversation, thread), Vec::new());
        mirror.used_threads.insert((conversation, thread));
      }

      let plan = backend.plan_refresh(SnapshotChanges {
        feeds: vec![id_from_raw(8)],
        ..SnapshotChanges::default()
      });

      assert!(plan.threads.is_empty() && plan.pages.is_empty());
    }

    fn backend() -> RemoteChat {
      let snapshot = ChatSnapshot::read(&crate::mock::MockBackend::new(0), Default::default());
      RemoteChat {
        mirror: Rc::new(RefCell::new(Mirror {
          snapshot,
          cursor: SnapshotCursor::default(),
          feeds: HashMap::new(),
          threads: HashMap::new(),
          used_threads: HashSet::new(),
          loading: HashSet::new(),
          outdated: HashSet::new(),
        })),
        notify: Rc::new(RefCell::new(None)),
        maybe_error: Rc::new(RefCell::new(None)),
        refresh_queued: Rc::new(Cell::new(false)),
        answers: Rc::new(RefCell::new(HashMap::new())),
      }
    }
  }

  mod slot {
    use super::*;

    #[test]
    fn an_equal_answer_is_no_change() {
      let mut slot = Slot::default();
      assert!(slot.settle(draft("x")));
      assert!(!slot.settle(draft("x")));
      assert!(slot.settle(draft("y")));
      assert!(!slot.is_loading && !slot.is_stale);
    }
  }

  fn draft(body: &str) -> ChatAnswer {
    ChatAnswer::Draft {
      maybe_body: Some(body.to_owned()),
    }
  }

  fn window(total: usize, bodies: &[&str]) -> MessageWindow {
    MessageWindow {
      conversation: id_from_raw(7),
      start: 0,
      total,
      undecrypted: 0,
      messages: bodies
        .iter()
        .zip(1_u64..)
        .map(|(body, id)| Message {
          id: id_from_raw(id),
          conversation: id_from_raw(7),
          author: id_from_raw(2),
          kind: MessageKind::User,
          sent_at: 0,
          body: (*body).to_owned(),
          is_edited: false,
          reactions: Vec::new(),
          reply_count: 0,
          maybe_thread: None,
        })
        .collect(),
    }
  }
}
