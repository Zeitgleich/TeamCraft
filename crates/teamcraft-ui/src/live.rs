//! Server-side state that lives next to the end-to-end encrypted chat: unread and mention counts,
//! followed threads, presence with custom status, and read receipts.
//!
//! None of this is message content, so the server owns it and the UI reads it over REST. The client
//! core keeps the decrypted messages; [`crate::hybrid::Hybrid`] lays this state over the chat
//! backend. Refreshes are pulled: once at start, when the chat changes (coalesced) and on a slow
//! timer, because a missed event must not leave a stale badge.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use leptos::prelude::set_timeout;
use leptos::task::spawn_local;
use teamcraft_proto::{
  CustomStatus as CustomStatusWire, ListPresenceResponse, ListThreadsResponse, PresenceState,
  ReadReceiptsResponse, ThreadSummary as ThreadWire, UnreadResponse, UserPresence,
};

use crate::auth::{ApiMethod, Authenticator};
use crate::clock::{end_of_local_day, end_of_local_week};
use crate::model::{
  Conversation, ConversationId, CustomStatus, Message, MessageId, PresenceStatus, StatusExpiry,
  ThreadSummary as ThreadView, TimestampMs, UserId,
};

/// Wait before a refresh that a change triggered, so a burst of sync steps costs one request round.
const REFRESH_DELAY: Duration = Duration::from_millis(400);
/// Safety net for events the UI never heard about.
pub const POLL_INTERVAL_MS: u64 = 30_000;
/// The presence endpoint takes a comma separated ID list; keep the URL short.
const PRESENCE_BATCH: usize = 50;
const THREAD_PAGE: u32 = 100;

/// What the chat currently shows, so a refresh asks only for relevant data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LiveScope {
  pub users: Vec<UserId>,
  /// DMs and group DMs: the only conversations with read receipts.
  pub direct: Vec<ConversationId>,
}

type ScopeFn = Rc<dyn Fn() -> LiveScope>;
type ChangeNotifier = Rc<dyn Fn()>;

#[derive(Clone)]
pub struct LiveServer {
  auth: Rc<dyn Authenticator>,
  scope: ScopeFn,
  state: Rc<RefCell<LiveState>>,
  notify: Rc<RefCell<Option<ChangeNotifier>>>,
  refresh_queued: Rc<Cell<bool>>,
}

/// The last answers of the server. Empty until the first refresh finishes.
#[derive(Debug, Default)]
pub struct LiveState {
  is_loaded: bool,
  unread: HashMap<ConversationId, (u32, u32)>,
  threads: Vec<ThreadWire>,
  presence: HashMap<UserId, UserPresence>,
  receipts: HashMap<ConversationId, Vec<(UserId, MessageId)>>,
}

impl LiveServer {
  pub fn new(auth: Rc<dyn Authenticator>, scope: ScopeFn) -> Self {
    Self {
      auth,
      scope,
      state: Rc::new(RefCell::new(LiveState::default())),
      notify: Rc::new(RefCell::new(None)),
      refresh_queued: Rc::new(Cell::new(false)),
    }
  }

  /// Called after every refresh that changed something.
  pub fn set_change_notifier(&self, notify: ChangeNotifier) {
    *self.notify.borrow_mut() = Some(notify);
  }

  /// Schedules a refresh soon; calls while one is scheduled collapse into it.
  pub fn queue_refresh(&self) {
    if self.refresh_queued.replace(true) {
      return;
    }
    let live = self.clone();
    set_timeout(
      move || {
        live.refresh_queued.set(false);
        spawn_local(async move { live.refresh().await });
      },
      REFRESH_DELAY,
    );
  }

  /// Fetches everything now. Failed parts keep their previous values.
  pub async fn refresh(&self) {
    let scope = (self.scope)();
    let unread = self.get::<UnreadResponse>("/api/v1/unread").await;
    let threads = self
      .get::<ListThreadsResponse>(&format!("/api/v1/threads?limit={THREAD_PAGE}"))
      .await;
    let presence = self.presence(&scope.users).await;
    let mut receipts = HashMap::new();
    for conversation in &scope.direct {
      let path = format!("/api/v1/conversations/{conversation}/read-receipts");
      if let Some(response) = self.get::<ReadReceiptsResponse>(&path).await {
        receipts.insert(
          *conversation,
          response
            .receipts
            .into_iter()
            .map(|receipt| (receipt.user_id, receipt.message_id))
            .collect(),
        );
      }
    }
    {
      let mut state = self.state.borrow_mut();
      state.is_loaded = true;
      if let Some(unread) = unread {
        state.unread = unread
          .conversations
          .into_iter()
          .map(|entry| (entry.conversation_id, (entry.unread_count, entry.mention_count)))
          .collect();
      }
      if let Some(threads) = threads {
        state.threads = threads.threads;
      }
      state
        .presence
        .extend(presence.into_iter().map(|entry| (entry.user_id, entry)));
      state.receipts.extend(receipts);
    }
    self.changed();
  }

  /// Unread and mention counts of the conversation, once the server has answered.
  pub fn with_counts(&self, conversation: Conversation) -> Conversation {
    let state = self.state.borrow();
    apply_counts(conversation, &state.unread, state.is_loaded)
  }

  pub fn thread_entries(&self) -> Vec<ThreadWire> {
    self.state.borrow().threads.clone()
  }

  pub fn presence_of(&self, user: UserId) -> Option<(PresenceStatus, Option<CustomStatus>)> {
    self.state.borrow().presence.get(&user).map(|entry| {
      (
        presence_status(&entry.state),
        entry.maybe_status.as_ref().map(custom_status_from_wire),
      )
    })
  }

  /// Users who have read up to (or past) the message, in a DM or group DM.
  pub fn seen_by(&self, conversation: ConversationId, message: MessageId) -> Vec<UserId> {
    let state = self.state.borrow();
    state
      .receipts
      .get(&conversation)
      .map_or_else(Vec::new, |receipts| {
        receipts
          .iter()
          .filter(|(_, read_up_to)| *read_up_to >= message)
          .map(|(user, _)| *user)
          .collect()
      })
  }

  /// Marks the conversation read in the local counts right away; the server confirms on refresh.
  pub fn clear_unread(&self, conversation: ConversationId) {
    self.state.borrow_mut().unread.insert(conversation, (0, 0));
    self.changed();
  }

  /// Reports how far the thread was read; the server resets the thread's unread counters.
  pub fn mark_thread_read(
    &self,
    conversation: ConversationId,
    thread: teamcraft_proto::ThreadId,
    latest: MessageId,
  ) {
    let path = format!("/api/v1/conversations/{conversation}/threads/{thread}/read");
    let body = serde_json::json!({ "message_id": latest });
    self.send(ApiMethod::Put, path, Some(body.to_string()));
  }

  pub fn set_thread_followed(
    &self,
    conversation: ConversationId,
    thread: teamcraft_proto::ThreadId,
    is_followed: bool,
  ) {
    let path = format!("/api/v1/conversations/{conversation}/threads/{thread}/follow");
    let method = if is_followed {
      ApiMethod::Put
    } else {
      ApiMethod::Delete
    };
    self.send(method, path, None);
  }

  /// Stores (or clears) the custom status of the signed-in user and shows it right away.
  pub fn set_custom_status(&self, me: UserId, maybe_status: Option<&CustomStatus>, now: TimestampMs) {
    match maybe_status {
      Some(status) => {
        let wire = custom_status_to_wire(status, now);
        let body = serde_json::to_string(&wire).ok();
        self.send(ApiMethod::Put, "/api/v1/me/status".to_owned(), body);
        let mut state = self.state.borrow_mut();
        let entry = state.presence.entry(me).or_insert(UserPresence {
          user_id: me,
          state: PresenceState::Online,
          maybe_status: None,
        });
        entry.maybe_status = Some(wire);
      },
      None => {
        self.send(ApiMethod::Delete, "/api/v1/me/status".to_owned(), None);
        if let Some(entry) = self.state.borrow_mut().presence.get_mut(&me) {
          entry.maybe_status = None;
        }
      },
    }
    self.changed();
  }

  fn changed(&self) {
    let notify = self.notify.borrow().clone();
    if let Some(notify) = notify {
      notify();
    }
  }

  async fn presence(&self, users: &[UserId]) -> Vec<UserPresence> {
    let mut all = Vec::new();
    for batch in users.chunks(PRESENCE_BATCH) {
      let ids: Vec<String> = batch.iter().map(ToString::to_string).collect();
      let path = format!("/api/v1/presence?user_ids={}", ids.join(","));
      if let Some(response) = self.get::<ListPresenceResponse>(&path).await {
        all.extend(response.presences);
      }
    }
    all
  }

  async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Option<T> {
    let response = match self.auth.api_get(path).await {
      Ok(response) => response,
      Err(error) => {
        tracing::warn!(path, %error, "live refresh failed");
        return None;
      },
    };
    if response.status != 200 {
      tracing::warn!(path, status = response.status, "live refresh was refused");
      return None;
    }
    match serde_json::from_str(&response.body) {
      Ok(value) => Some(value),
      Err(error) => {
        tracing::warn!(path, %error, "live refresh returned something unreadable");
        None
      },
    }
  }

  /// Fire and forget; the next refresh shows the outcome (or the unchanged truth after a refusal).
  fn send(&self, method: ApiMethod, path: String, maybe_body: Option<String>) {
    let live = self.clone();
    spawn_local(async move {
      match live.auth.api_request(method, &path, maybe_body).await {
        Ok(response) if response.status < 300 => {},
        Ok(response) => tracing::warn!(path, status = response.status, "the server refused the change"),
        Err(error) => tracing::warn!(path, %error, "could not reach the server"),
      }
      live.queue_refresh();
    });
  }
}

/// Replaces the locally derived counts with the server's, once it has answered. A conversation the
/// server does not list has nothing unread.
pub fn apply_counts(
  mut conversation: Conversation,
  unread: &HashMap<ConversationId, (u32, u32)>,
  is_loaded: bool,
) -> Conversation {
  if is_loaded {
    let (unread_count, mention_count) = unread.get(&conversation.id).copied().unwrap_or((0, 0));
    conversation.unread_count = unread_count;
    conversation.mention_count = mention_count;
  }
  conversation
}

/// A thread inbox row from the server's counters and the decrypted thread (root first). `None`
/// while the thread has not been read from the core yet.
pub fn thread_summary(entry: &ThreadWire, messages: &[Message]) -> Option<ThreadView> {
  let (root, replies) = messages.split_first()?;
  let mut participant_ids = vec![root.author];
  for reply in replies {
    if !participant_ids.contains(&reply.author) {
      participant_ids.push(reply.author);
    }
  }
  Some(ThreadView {
    conversation: entry.conversation_id,
    root: root.clone(),
    reply_count: entry.reply_count,
    unread_replies: entry.unread_count,
    last_reply_at: replies.last().map_or(root.sent_at, |reply| reply.sent_at),
    participant_ids,
    has_mention: entry.mention_count > 0,
  })
}

pub fn presence_status(state: &PresenceState) -> PresenceStatus {
  match state {
    PresenceState::Online => PresenceStatus::Online,
    PresenceState::Away => PresenceStatus::Away,
    PresenceState::DoNotDisturb => PresenceStatus::DoNotDisturb,
    PresenceState::Offline | PresenceState::Unknown(_) => PresenceStatus::Offline,
  }
}

/// The wire form of a status; the expiry choice becomes a point in time in the user's time zone.
pub fn custom_status_to_wire(status: &CustomStatus, now: TimestampMs) -> CustomStatusWire {
  let maybe_expires_at = match status.expiry {
    StatusExpiry::Never => None,
    StatusExpiry::ThirtyMinutes => Some(now + 30 * 60_000),
    StatusExpiry::OneHour => Some(now + 60 * 60_000),
    StatusExpiry::Today => Some(end_of_local_day(now)),
    StatusExpiry::ThisWeek => Some(end_of_local_week(now)),
  };
  CustomStatusWire {
    maybe_emoji: Some(status.emoji.clone()).filter(|emoji| !emoji.is_empty()),
    maybe_text: Some(status.text.clone()).filter(|text| !text.is_empty()),
    maybe_expires_at: maybe_expires_at.map(unix_millis),
  }
}

pub fn custom_status_from_wire(status: &CustomStatusWire) -> CustomStatus {
  CustomStatus {
    emoji: status.maybe_emoji.clone().unwrap_or_default(),
    text: status.maybe_text.clone().unwrap_or_default(),
    expiry: if status.maybe_expires_at.is_some() {
      StatusExpiry::Today
    } else {
      StatusExpiry::Never
    },
  }
}

fn unix_millis(value: TimestampMs) -> teamcraft_proto::UnixMillis {
  teamcraft_proto::UnixMillis(value)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{ConversationKind, SidebarCategory, id_from_raw};

  const NOW: TimestampMs = 1_792_540_800_000;

  #[test]
  fn counts_come_from_the_server_once_loaded() {
    let id: ConversationId = id_from_raw(7);
    let unread = HashMap::from([(id, (3, 1))]);
    assert_eq!(apply_counts(conversation(id), &unread, true).unread_count, 3);
    assert_eq!(apply_counts(conversation(id), &unread, true).mention_count, 1);
  }

  #[test]
  fn counts_stay_local_before_the_first_answer() {
    let id: ConversationId = id_from_raw(7);
    let result = apply_counts(conversation(id), &HashMap::new(), false);
    assert_eq!(result.unread_count, 9);
  }

  #[test]
  fn unlisted_conversations_have_nothing_unread() {
    let id: ConversationId = id_from_raw(7);
    let result = apply_counts(conversation(id), &HashMap::new(), true);
    assert_eq!((result.unread_count, result.mention_count), (0, 0));
  }

  #[test]
  fn custom_status_round_trips_and_expiry_becomes_a_time() {
    let status = CustomStatus {
      emoji: "🌴".into(),
      text: "On vacation".into(),
      expiry: StatusExpiry::OneHour,
    };
    let wire = custom_status_to_wire(&status, NOW);
    assert_eq!(wire.maybe_expires_at, Some(unix_millis(NOW + 3_600_000)));
    let back = custom_status_from_wire(&wire);
    assert_eq!((back.emoji.as_str(), back.text.as_str()), ("🌴", "On vacation"));
  }

  #[test]
  fn a_status_without_expiry_stays_until_cleared() {
    let status = CustomStatus {
      emoji: String::new(),
      text: "Heads down".into(),
      expiry: StatusExpiry::Never,
    };
    let wire = custom_status_to_wire(&status, NOW);
    assert_eq!(wire.maybe_expires_at, None);
    assert_eq!(wire.maybe_emoji, None);
  }

  #[test]
  fn a_thread_row_needs_the_decrypted_root() {
    let wire = thread_wire(2, 1);
    assert!(thread_summary(&wire, &[]).is_none());
    let root = message(1, 10, 1_000);
    let replies = [message(2, 11, 2_000), message(3, 10, 3_000)];
    let summary =
      thread_summary(&wire, &[root.clone(), replies[0].clone(), replies[1].clone()]).expect("summary");
    assert_eq!(summary.last_reply_at, 3_000);
    assert_eq!(summary.participant_ids, vec![root.author, replies[0].author]);
    assert_eq!((summary.reply_count, summary.unread_replies), (2, 1));
    assert!(summary.has_mention);
  }

  #[test]
  fn unknown_presence_is_offline() {
    assert_eq!(
      presence_status(&PresenceState::Unknown("x".into())),
      PresenceStatus::Offline
    );
    assert_eq!(
      presence_status(&PresenceState::DoNotDisturb),
      PresenceStatus::DoNotDisturb
    );
  }

  fn thread_wire(replies: u32, unread: u32) -> ThreadWire {
    ThreadWire {
      conversation_id: id_from_raw(7),
      thread_id: id_from_raw(1),
      follow_reason: teamcraft_proto::ThreadFollowReason::Replied,
      reply_count: replies,
      unread_count: unread,
      mention_count: 1,
      maybe_last_reply: None,
    }
  }

  fn message(id: u64, author: u64, sent_at: TimestampMs) -> Message {
    Message {
      id: id_from_raw(id),
      conversation: id_from_raw(7),
      author: id_from_raw(author),
      kind: crate::model::MessageKind::User,
      sent_at,
      body: String::new(),
      is_edited: false,
      reactions: Vec::new(),
      reply_count: 0,
      maybe_thread: None,
    }
  }

  fn conversation(id: ConversationId) -> Conversation {
    Conversation {
      id,
      maybe_space: None,
      kind: ConversationKind::Public,
      category: SidebarCategory::Channels,
      name: "x".into(),
      topic: String::new(),
      member_ids: Vec::new(),
      unread_count: 9,
      mention_count: 0,
      is_favorite: false,
      is_muted: false,
      can_post: true,
      bookmarks: Vec::new(),
    }
  }
}
