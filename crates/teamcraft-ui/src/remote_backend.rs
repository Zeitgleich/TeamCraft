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
use teamcraft_client_api::calls::OrgBackgroundRule;
use teamcraft_proto::OrgBackgroundId;

use leptos::prelude::set_timeout;
use leptos::task::spawn_local;
use serde_json::json;
use teamcraft_client_api::actions::{
  BrowsableChannel, CategoryInfo, ChannelChanges, FileInfo, MessageExtras, NewChannel, NewSpace,
  OutgoingMessage, PermalinkTarget, SavedItem, ShareLink, ShareLinkOptions, SpaceChanges, SpaceInvite,
};
use teamcraft_client_api::backend::{ChatBackend, SearchBackend};
use teamcraft_client_api::calls::BackgroundImageInfo;
use teamcraft_client_api::model::{
  Conversation, ConversationId, CustomStatus, JoinableSpace, Message, MessageId, PresenceStatus, Space,
  SpaceId, ThreadId, ThreadSummary, User, UserId,
};
use teamcraft_client_api::remote::{
  CORE_CHANGED_EVENT, ChatAction, ChatAnswer, ChatQuery, ChatSnapshot, CoreConnection, CoreStatus,
  EnrollRequest, LinkStatus, MessageWindow, StoreHealth,
};
use teamcraft_client_api::search::{ParsedQuery, SavedSearch, SearchHit, SearchResults};
use teamcraft_proto::{BackgroundId, CategoryId, HistoryPolicy, WebPushKeyResponse};

use crate::auth::Authenticator;
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

/// The answer to one query, kept so the synchronous trait can read it. A change in the core marks
/// answers stale; they are shown as they are until the next read has fetched fresh ones.
#[derive(Default)]
struct Slot {
  maybe_answer: Option<ChatAnswer>,
  is_loading: bool,
  is_stale: bool,
}

#[derive(Clone)]
pub struct RemoteChat {
  mirror: Rc<RefCell<Mirror>>,
  notify: Rc<RefCell<Option<ChangeNotifier>>>,
  refresh_queued: Rc<Cell<bool>>,
  maybe_error: Rc<RefCell<Option<String>>>,
  answers: Rc<RefCell<HashMap<String, Slot>>>,
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
      maybe_error: Rc::new(RefCell::new(None)),
      answers: Rc::new(RefCell::new(HashMap::new())),
    };
    let listener = backend.clone();
    listen(CORE_CHANGED_EVENT, move || listener.queue_refresh());
    Ok(backend)
  }

  /// The answer to a query, or `None` while it is being fetched (the UI re-reads when it arrives).
  /// A stale answer is returned as it is while a fresh one is fetched.
  fn ask(&self, query: ChatQuery) -> Option<ChatAnswer> {
    let key = serde_json::to_string(&query).ok()?;
    let (existing, must_load) = {
      let mut answers = self.answers.borrow_mut();
      let slot = answers.entry(key.clone()).or_default();
      let must_load = !slot.is_loading && (slot.maybe_answer.is_none() || slot.is_stale);
      if must_load {
        slot.is_loading = true;
      }
      (slot.maybe_answer.clone(), must_load)
    };
    if must_load {
      let backend = self.clone();
      spawn_local(async move {
        let outcome = call::<ChatAnswer>("core_query", &json!({ "query": query })).await;
        {
          let mut answers = backend.answers.borrow_mut();
          let slot = answers.entry(key).or_default();
          slot.is_loading = false;
          // After a failure the old answer stays and nothing is asked again until something changes.
          slot.is_stale = false;
          match outcome {
            Ok(answer) => slot.maybe_answer = Some(answer),
            Err(error) => tracing::warn!(%error, "a query failed"),
          }
        }
        backend.changed();
      });
    }
    existing
  }

  /// Everything fetched so far may be out of date.
  fn mark_answers_stale(&self) {
    for slot in self.answers.borrow_mut().values_mut() {
      slot.is_stale = true;
    }
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
        self.mark_answers_stale();
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
    let backend = self.clone();
    spawn_local(async move {
      match call::<()>("core_action", &json!({ "action": action })).await {
        Ok(()) => *backend.maybe_error.borrow_mut() = None,
        Err(error) => {
          tracing::warn!(%error, "action failed");
          *backend.maybe_error.borrow_mut() = Some(error);
        },
      }
      backend.mark_answers_stale();
      backend.changed();
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
  fn can_sign_workspace(&self) -> bool {
    true
  }

  fn set_activity(&self, is_idle: bool) {
    self.act(ChatAction::SetActivity { is_idle });
  }

  fn set_attention(&self, is_attentive: bool) {
    self.act(ChatAction::SetAttention { is_attentive });
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

  fn download_file(&self, conversation: ConversationId, message: MessageId) {
    self.act(ChatAction::DownloadFile {
      conversation,
      message,
    });
  }

  fn create_share_link(&self, conversation: ConversationId, message: MessageId, options: ShareLinkOptions) {
    self.act(ChatAction::CreateShareLink {
      conversation,
      message,
      options,
    });
  }

  fn share_links(&self, conversation: ConversationId, message: MessageId) -> Vec<ShareLink> {
    match self.ask(ChatQuery::ShareLinks {
      conversation,
      message,
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

  fn last_activity(&self, conversation: ConversationId) -> Option<teamcraft_client_api::model::TimestampMs> {
    self
      .mirror
      .borrow()
      .snapshot
      .last_activity
      .iter()
      .find(|(id, _)| *id == conversation)
      .map(|(_, at)| *at)
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
    self.act(ChatAction::SetPresence {
      presence,
      maybe_until,
    });
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
