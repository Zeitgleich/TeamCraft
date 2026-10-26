//! Server-side state that lives next to the end-to-end encrypted chat: unread and mention counts,
//! followed threads, presence with custom status, and read receipts.
//!
//! None of this is message content, so the server owns it. With a client core (desktop, and the
//! browser with its in-page core) the core keeps it current from gateway events and hands it over
//! in its snapshot ([`LiveSnapshot`]), presence included (it reads everyone's after each
//! connection). The UI reads over REST only what the core doesn't keep: the announcement, and the
//! receipts of the DM that is open, once at start and once after each gateway connection, never
//! on a timer and never one request per DM (issues #183, #252). Without a core there is no
//! gateway: then everything is read over REST, with a slow poll while the page is visible.
//! [`crate::hybrid::Hybrid`] lays this state over the chat backend.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use leptos::prelude::set_timeout;
use leptos::task::spawn_local;
use teamcraft_client_api::custom_status::set_request;
use teamcraft_client_api::live::{LiveMarks, LiveReceipt, LiveSnapshot};
use teamcraft_proto::{
  AnnouncementId, AnnouncementResponse, CustomStatus as CustomStatusWire, GetAnnouncementResponse,
  ListPresenceResponse, ListThreadsResponse, PresenceState, ReadReceiptsResponse,
  ThreadSummary as ThreadWire, UnreadResponse, UserPresence,
};

use crate::auth::{ApiMethod, Authenticator};
use crate::clock::{end_of_local_day, end_of_local_week};
use crate::feedback::{ActionTicket, report_failure};
use crate::model::{
  Conversation, ConversationId, CustomStatus, Message, MessageId, PresenceStatus, StatusExpiry,
  ThreadSummary as ThreadView, TimestampMs, UserId,
};
use crate::rest::RestClient;

/// Wait before a refresh that a change triggered, so a burst of sync steps costs one request round.
const REFRESH_DELAY: Duration = Duration::from_millis(400);
/// Only without a client core, which has no gateway to hear changes on: every 5 minutes, and only
/// while the page is visible, the counters are read again so a badge doesn't stay stale forever.
pub const REST_ONLY_POLL_INTERVAL_MS: u64 = 300_000;
/// The presence endpoint takes a comma separated ID list; keep the URL short.
const PRESENCE_BATCH: usize = 50;
const THREAD_PAGE: u32 = 100;

/// What the chat currently shows, so a refresh asks only for relevant data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LiveScope {
  /// Whose presence a UI without a core reads.
  pub users: Vec<UserId>,
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
  /// Counts and threads come from the core's snapshot, not from REST.
  is_core_fed: bool,
  unread: HashMap<ConversationId, (u32, u32)>,
  threads: Vec<ThreadWire>,
  presence: HashMap<UserId, UserPresence>,
  receipts: HashMap<ConversationId, Vec<(UserId, MessageId)>>,
  announcement: Option<AnnouncementResponse>,
  /// The marks of the core's last snapshot; `None` before the first.
  maybe_last_marks: Option<LiveMarks>,
  /// The newest presence announcement taken from the core, so only newer ones are laid over the
  /// REST answers.
  presence_seen: u64,
  /// The receipts of the last snapshot: unchanged ones are not applied again.
  last_receipts: Vec<LiveReceipt>,
  /// The conversation whose receipts the UI showed last (the open DM): the only one whose
  /// receipts are read over REST, once per connection.
  maybe_receipts_shown: Option<ConversationId>,
  /// Conversations whose receipts were read since the last connection, or are being read.
  receipts_read: HashSet<ConversationId>,
}

/// What the UI still has to read from the server after a core snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiveFollowUp {
  /// The gateway (re)connected: events in between may be lost, so read once.
  pub needs_resync: bool,
  pub needs_announcement: bool,
}

/// One refresh's answers from the server; `None` parts were not asked for or failed.
#[derive(Debug, Default)]
struct Fetched {
  /// Counts and threads came over REST (no core).
  is_rest: bool,
  maybe_unread: Option<HashMap<ConversationId, (u32, u32)>>,
  maybe_threads: Option<Vec<ThreadWire>>,
  presence: Vec<UserPresence>,
  receipts: HashMap<ConversationId, Vec<(UserId, MessageId)>>,
  maybe_announcement: Option<GetAnnouncementResponse>,
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

  /// Whether counts and threads come from a client core's snapshot (then nothing needs polling).
  pub fn is_core_fed(&self) -> bool {
    self.state.borrow().is_core_fed
  }

  /// Takes the core's snapshot after a change it reported; reads from the server only what the
  /// snapshot says is due. `None` (no core) changes nothing.
  pub fn observe(&self, maybe_live: Option<LiveSnapshot>) {
    let Some(live) = maybe_live else {
      return;
    };
    let follow_up = self.state.borrow_mut().apply_snapshot(live);
    if follow_up.needs_resync {
      self.queue_refresh();
    } else if follow_up.needs_announcement {
      let live = self.clone();
      spawn_local(async move { live.refresh_announcement().await });
    }
  }

  /// Schedules a refresh soon; calls while one is scheduled collapse into it.
  pub fn queue_refresh(&self) {
    if self.refresh_queued.replace(true) {
      return;
    }
    let live = self.clone();
    set_timeout(
      move || {
        // async-ok: a Cell, not a signal.
        live.refresh_queued.set(false);
        spawn_local(async move { live.refresh().await });
      },
      REFRESH_DELAY,
    );
  }

  /// Fetches what the core doesn't keep (everything without a core): at most the announcement and
  /// the open DM's receipts with a core, whatever the workspace's size. Failed parts keep their
  /// previous values.
  pub async fn refresh(&self) {
    let (is_core_fed, maybe_shown) = {
      let mut state = self.state.borrow_mut();
      // Receipts may have been missed since they were read: read again when shown.
      state.receipts_read.clear();
      (state.is_core_fed, state.maybe_receipts_shown)
    };
    let (unread, threads, presence) = if is_core_fed {
      (None, None, Vec::new())
    } else {
      let scope = (self.scope)();
      (
        self.get::<UnreadResponse>("/api/v1/unread").await,
        self
          .get::<ListThreadsResponse>(&format!("/api/v1/threads?limit={THREAD_PAGE}"))
          .await,
        self.presence(&scope.users).await,
      )
    };
    let announcement = self.get::<GetAnnouncementResponse>("/api/v1/announcement").await;
    let mut receipts = HashMap::new();
    if let Some(conversation) = maybe_shown {
      self.state.borrow_mut().receipts_read.insert(conversation);
      if let Some(read) = self.receipts(conversation).await {
        receipts.insert(conversation, read);
      }
    }
    let fetched = Fetched {
      is_rest: !is_core_fed,
      maybe_unread: unread.map(|unread| unread_map(&unread)),
      maybe_threads: threads.map(|threads| threads.threads),
      presence,
      receipts,
      maybe_announcement: announcement,
    };
    // Issue #246: an unchanged answer must not make the whole UI read the workspace again.
    if self.state.borrow_mut().merge(fetched) {
      self.changed();
    }
  }

  async fn refresh_receipts(&self, conversation: ConversationId) {
    let Some(receipts) = self.receipts(conversation).await else {
      return;
    };
    let fetched = Fetched {
      receipts: HashMap::from([(conversation, receipts)]),
      ..Fetched::default()
    };
    if self.state.borrow_mut().merge(fetched) {
      self.changed();
    }
  }

  async fn receipts(&self, conversation: ConversationId) -> Option<Vec<(UserId, MessageId)>> {
    let path = format!("/api/v1/conversations/{conversation}/read-receipts");
    let response = self.get::<ReadReceiptsResponse>(&path).await?;
    Some(
      response
        .receipts
        .into_iter()
        .map(|receipt| (receipt.user_id, receipt.message_id))
        .collect(),
    )
  }

  async fn refresh_announcement(&self) {
    if let Some(announcement) = self.get::<GetAnnouncementResponse>("/api/v1/announcement").await {
      let is_changed = replace_if_changed(
        &mut self.state.borrow_mut().announcement,
        announcement.maybe_announcement,
      );
      if is_changed {
        self.changed();
      }
    }
  }

  /// The instance announcement, unless this user dismissed it.
  pub fn announcement(&self) -> Option<AnnouncementResponse> {
    self
      .state
      .borrow()
      .announcement
      .clone()
      .filter(|banner| !banner.is_dismissed)
  }

  pub fn dismiss_announcement(&self, id: AnnouncementId) {
    if let Some(banner) = self.state.borrow_mut().announcement.as_mut() {
      banner.is_dismissed = true;
    }
    self.changed();
    self.send(
      ApiMethod::Post,
      format!("/api/v1/announcement/{id}/dismiss"),
      Some("{}".to_owned()),
    );
  }

  /// "Leave on all devices": the server takes all of the user's devices out of the conversation's
  /// call and tells everyone (a `call_update` or `call_end` event). A button waiting for it stays
  /// pending until the server answered; a failure is shown (#322).
  pub fn leave_call_everywhere(&self, conversation: ConversationId) {
    let client = RestClient::new(self.auth.clone());
    let path = format!("/api/v1/conversations/{conversation}/call/leave");
    let maybe_ticket = ActionTicket::current();
    spawn_local(async move {
      if let Err(error) = client.send(ApiMethod::Post, &path, Some("{}".to_owned())).await {
        let failure = error.to_failure();
        if let Some(ticket) = &maybe_ticket {
          ticket.fail(failure.clone());
        }
        report_failure(failure);
      }
      // Dropped only now, so the button stays pending until the answer is in.
      drop(maybe_ticket);
    });
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
        entry.maybe_status.as_ref().and_then(CustomStatus::from_wire),
      )
    })
  }

  /// Users who have read up to (or past) the message, in a DM or group DM. The first time a
  /// conversation is shown after a connection its receipts are read from the server once (events
  /// may have been missed); the UI reads again when they arrived.
  pub fn seen_by(&self, conversation: ConversationId, message: MessageId) -> Vec<UserId> {
    let is_unread = {
      let mut state = self.state.borrow_mut();
      state.maybe_receipts_shown = Some(conversation);
      state.receipts_read.insert(conversation)
    };
    if is_unread {
      let live = self.clone();
      spawn_local(async move { live.refresh_receipts(conversation).await });
    }
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

  /// Stores (or clears) the custom status of the signed-in user over REST, for a UI without a
  /// client core (the core queues it otherwise), and shows it right away.
  pub fn set_custom_status(&self, me: UserId, maybe_status: Option<&CustomStatus>, now: TimestampMs) {
    let maybe_wire = maybe_status.and_then(|status| status.to_wire(now));
    match maybe_wire.as_ref().and_then(|wire| set_request(wire, now)) {
      Some(request) => {
        let body = serde_json::to_string(&request).ok();
        self.send(ApiMethod::Put, "/api/v1/me/status".to_owned(), body);
      },
      None => self.send(ApiMethod::Delete, "/api/v1/me/status".to_owned(), None),
    }
    self.show_own_status(me, maybe_wire);
  }

  /// Shows the user's own status change until the server's answer (or the core's next snapshot)
  /// confirms it.
  pub fn show_own_status(&self, me: UserId, maybe_wire: Option<CustomStatusWire>) {
    {
      let mut state = self.state.borrow_mut();
      let entry = state.presence.entry(me).or_insert(UserPresence {
        user_id: me,
        state: PresenceState::Online,
        maybe_status: None,
        maybe_last_seen: None,
      });
      entry.maybe_status = maybe_wire;
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

  /// Fire and forget; a refresh shows the outcome (or the unchanged truth after a refusal). With a
  /// core, a change that went through comes back as a gateway event instead.
  fn send(&self, method: ApiMethod, path: String, maybe_body: Option<String>) {
    let live = self.clone();
    spawn_local(async move {
      let is_accepted = match live.auth.api_request(method, &path, maybe_body).await {
        Ok(response) if response.status < 300 => true,
        Ok(response) => {
          tracing::warn!(path, status = response.status, "the server refused the change");
          false
        },
        Err(error) => {
          tracing::warn!(path, %error, "could not reach the server");
          false
        },
      };
      if !is_accepted || !live.state.borrow().is_core_fed {
        live.queue_refresh();
      }
    });
  }
}

impl LiveState {
  /// Lays the core's snapshot over the state: counts and threads are the core's, presence and
  /// receipts only where they changed since the last snapshot (a REST answer read after an older
  /// event must not be undone by it). Proportional to what changed, not to everything the core
  /// keeps: presence by revision, receipts skipped when unchanged.
  /// Takes a refresh's answers; failed parts keep their previous values. Returns whether anything
  /// the UI shows changed.
  fn merge(&mut self, fetched: Fetched) -> bool {
    let mut is_changed = false;
    if fetched.is_rest && !self.is_loaded {
      self.is_loaded = true;
      is_changed = true;
    }
    if let Some(unread) = fetched.maybe_unread {
      is_changed |= replace_if_changed(&mut self.unread, unread);
    }
    if let Some(threads) = fetched.maybe_threads {
      is_changed |= replace_if_changed(&mut self.threads, threads);
    }
    for entry in fetched.presence {
      if self.presence.get(&entry.user_id) != Some(&entry) {
        self.presence.insert(entry.user_id, entry);
        is_changed = true;
      }
    }
    for (conversation, receipts) in fetched.receipts {
      if self.receipts.get(&conversation) != Some(&receipts) {
        self.receipts.insert(conversation, receipts);
        is_changed = true;
      }
    }
    if let Some(announcement) = fetched.maybe_announcement {
      is_changed |= replace_if_changed(&mut self.announcement, announcement.maybe_announcement);
    }
    is_changed
  }

  pub fn apply_snapshot(&mut self, live: LiveSnapshot) -> LiveFollowUp {
    let maybe_last_marks = self.maybe_last_marks.replace(live.marks);
    self.is_core_fed = true;
    if let Some(unread) = &live.maybe_unread {
      self.is_loaded = true;
      self.unread = unread_map(unread);
    }
    self.threads = live.threads;
    // The core announces everyone it knows after each connection; a reader that fell behind the
    // log's bound keeps the older presence of the people dropped meanwhile until their next
    // announcement (there is nothing better to read: the UI no longer asks the server).
    for entry in live.presence.since(self.presence_seen) {
      self
        .presence
        .insert(entry.presence.user_id, entry.presence.clone());
    }
    self.presence_seen = self.presence_seen.max(live.presence.revision);
    if live.receipts != self.last_receipts {
      self.apply_receipts(&live.receipts);
      self.last_receipts = live.receipts;
    }
    follow_up(maybe_last_marks, live.marks)
  }

  fn apply_receipts(&mut self, live: &[LiveReceipt]) {
    for receipt in live {
      let receipts = self.receipts.entry(receipt.conversation).or_default();
      match receipts.iter_mut().find(|(user, _)| *user == receipt.user) {
        Some((_, read_up_to)) => *read_up_to = (*read_up_to).max(receipt.message),
        None => receipts.push((receipt.user, receipt.message)),
      }
    }
  }
}

fn replace_if_changed<T: PartialEq>(current: &mut T, new: T) -> bool {
  if *current == new {
    return false;
  }
  *current = new;
  true
}

/// The first snapshot needs no resync of its own: the start reads everything anyway.
fn follow_up(maybe_last: Option<LiveMarks>, marks: LiveMarks) -> LiveFollowUp {
  match maybe_last {
    None => LiveFollowUp::default(),
    Some(last) => LiveFollowUp {
      needs_resync: marks.connections != last.connections,
      needs_announcement: marks.announcements != last.announcements,
    },
  }
}

fn unread_map(unread: &UnreadResponse) -> HashMap<ConversationId, (u32, u32)> {
  unread
    .conversations
    .iter()
    .map(|entry| (entry.conversation_id, (entry.unread_count, entry.mention_count)))
    .collect()
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

/// The status with its end resolved in the user's time zone ("today" ends at local midnight), on
/// this device's clock. It reaches the server as the time left from when it is sent
/// (`SetCustomStatusRequest::for_status`), so a clock that is off doesn't matter. An end the
/// caller already chose (a preset's duration, the unchanged end of the current status) stays.
pub fn with_local_expiry(mut status: CustomStatus, now: TimestampMs) -> CustomStatus {
  if status.maybe_expires_at.is_some() {
    return status;
  }
  status.maybe_expires_at = match status.expiry {
    StatusExpiry::Never => None,
    StatusExpiry::ThirtyMinutes => Some(now + 30 * 60_000),
    StatusExpiry::OneHour => Some(now + 60 * 60_000),
    StatusExpiry::Today => Some(end_of_local_day(now)),
    StatusExpiry::ThisWeek => Some(end_of_local_week(now)),
  };
  status
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{ConversationKind, SidebarCategory, id_from_raw};
  use teamcraft_client_api::live::LiveReceipt;

  const NOW: TimestampMs = 1_792_540_800_000;

  mod refresh_merge {
    use super::*;

    #[test]
    fn the_same_answers_again_change_nothing() {
      let mut state = LiveState::default();
      assert!(state.merge(fetched(Some(&[(7, 3, 1)]))));
      assert!(!state.merge(fetched(Some(&[(7, 3, 1)]))));
    }

    #[test]
    fn a_new_count_is_a_change() {
      let mut state = LiveState::default();
      state.merge(fetched(Some(&[(7, 3, 1)])));
      assert!(state.merge(fetched(Some(&[(7, 4, 1)]))));
      assert_eq!(state.unread.get(&id_from_raw(7)), Some(&(4, 1)));
    }

    #[test]
    fn a_failed_part_keeps_the_previous_answer_and_is_no_change() {
      let mut state = LiveState::default();
      state.merge(fetched(Some(&[(7, 3, 1)])));
      assert!(!state.merge(fetched(None)));
      assert_eq!(state.unread.get(&id_from_raw(7)), Some(&(3, 1)));
    }

    #[test]
    fn a_new_receipt_is_a_change_and_an_old_one_is_not() {
      let mut state = LiveState::default();
      let conversation: ConversationId = id_from_raw(7);
      let receipt = (id_from_raw::<UserId>(2), id_from_raw::<MessageId>(9));
      let with_receipt = || Fetched {
        receipts: HashMap::from([(conversation, vec![receipt])]),
        ..Fetched::default()
      };
      assert!(state.merge(with_receipt()));
      assert!(!state.merge(with_receipt()));
    }

    fn fetched(maybe_unread: Option<&[(u64, u32, u32)]>) -> Fetched {
      Fetched {
        is_rest: true,
        maybe_unread: maybe_unread.map(|rows| {
          rows
            .iter()
            .map(|(id, unread, mentions)| (id_from_raw(*id), (*unread, *mentions)))
            .collect()
        }),
        ..Fetched::default()
      }
    }
  }

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

  mod core_snapshot {
    use super::*;

    #[test]
    fn counts_come_from_the_snapshot_without_asking_the_server() {
      let id: ConversationId = id_from_raw(7);
      let mut state = LiveState::default();
      state.apply_snapshot(live(Some(&[(7, 4, 2)]), LiveMarks::default()));
      assert!(state.is_core_fed);
      let counted = apply_counts(conversation(id), &state.unread, state.is_loaded);
      assert_eq!((counted.unread_count, counted.mention_count), (4, 2));

      // An `unread_update` (mark read on another device) reached the core: the badge follows.
      state.apply_snapshot(live(Some(&[]), LiveMarks::default()));
      let counted = apply_counts(conversation(id), &state.unread, state.is_loaded);
      assert_eq!((counted.unread_count, counted.mention_count), (0, 0));
    }

    #[test]
    fn counts_stay_local_until_the_core_has_synced() {
      let mut state = LiveState::default();
      state.apply_snapshot(live(None, LiveMarks::default()));
      assert!(!state.is_loaded);
    }

    #[test]
    fn a_new_gateway_connection_asks_for_one_resync() {
      let mut state = LiveState::default();
      let first = LiveMarks::default();
      assert_eq!(
        state.apply_snapshot(live(Some(&[]), first)),
        LiveFollowUp::default()
      );
      assert_eq!(
        state.apply_snapshot(live(Some(&[]), first)),
        LiveFollowUp::default()
      );
      let reconnected = LiveMarks {
        connections: 1,
        ..first
      };
      assert_eq!(
        state.apply_snapshot(live(Some(&[]), reconnected)),
        LiveFollowUp {
          needs_resync: true,
          needs_announcement: false
        }
      );
      assert_eq!(
        state.apply_snapshot(live(Some(&[]), reconnected)),
        LiveFollowUp::default()
      );
      let announced = LiveMarks {
        announcements: 1,
        ..reconnected
      };
      assert!(
        state
          .apply_snapshot(live(Some(&[]), announced))
          .needs_announcement
      );
    }

    #[test]
    fn only_presence_that_changed_overrides_the_rest_answer() {
      let user: UserId = id_from_raw(3);
      let mut state = LiveState::default();
      let mut snapshot = live(Some(&[]), LiveMarks::default());
      snapshot.presence.note(presence(user, PresenceState::Away), 10);
      state.apply_snapshot(snapshot.clone());
      // A REST resync answered later with a newer state; the same old event must not undo it.
      state.presence.insert(user, presence(user, PresenceState::Online));
      state.apply_snapshot(snapshot.clone());
      assert_eq!(state.presence[&user].state, PresenceState::Online);

      snapshot
        .presence
        .note(presence(user, PresenceState::DoNotDisturb), 10);
      state.apply_snapshot(snapshot);
      assert_eq!(state.presence[&user].state, PresenceState::DoNotDisturb);
    }

    #[test]
    fn announcements_dropped_before_the_ui_saw_them_ask_the_server_nothing() {
      let mut state = LiveState::default();
      let mut snapshot = live(Some(&[]), LiveMarks::default());
      state.apply_snapshot(snapshot.clone());
      for user in 1..=4 {
        snapshot
          .presence
          .note(presence(id_from_raw(user), PresenceState::Online), 2);
      }
      // Issue #252: the core owns presence; the UI takes what the log still holds.
      assert_eq!(state.apply_snapshot(snapshot.clone()), LiveFollowUp::default());
      assert_eq!(state.presence.len(), 2, "the entries still kept are applied");
    }

    #[test]
    fn receipts_from_events_only_move_forward() {
      let (dm, reader): (ConversationId, UserId) = (id_from_raw(5), id_from_raw(3));
      let mut state = LiveState::default();
      state.receipts.insert(dm, vec![(reader, id_from_raw(30))]);
      let mut snapshot = live(Some(&[]), LiveMarks::default());
      snapshot.receipts = vec![LiveReceipt {
        conversation: dm,
        user: reader,
        message: id_from_raw(20),
      }];
      state.apply_snapshot(snapshot);
      assert_eq!(state.receipts[&dm], vec![(reader, id_from_raw(30))]);
    }

    fn live(maybe_counts: Option<&[(u64, u32, u32)]>, marks: LiveMarks) -> LiveSnapshot {
      LiveSnapshot {
        maybe_unread: maybe_counts.map(|counts| UnreadResponse {
          conversations: counts
            .iter()
            .map(
              |(id, unread_count, mention_count)| teamcraft_proto::ConversationUnread {
                conversation_id: id_from_raw(*id),
                unread_count: *unread_count,
                mention_count: *mention_count,
              },
            )
            .collect(),
          unread_thread_count: 0,
          thread_mention_count: 0,
          maybe_max_counted: None,
          maybe_counted_through: None,
        }),
        marks,
        ..LiveSnapshot::default()
      }
    }

    fn presence(user: UserId, state: PresenceState) -> UserPresence {
      UserPresence {
        user_id: user,
        state,
        maybe_status: None,
        maybe_last_seen: None,
      }
    }
  }

  #[test]
  fn custom_status_round_trips_and_expiry_becomes_a_time() {
    let status = with_local_expiry(
      CustomStatus {
        emoji: "🌴".into(),
        text: "On vacation".into(),
        expiry: StatusExpiry::OneHour,
        maybe_expires_at: None,
      },
      NOW,
    );
    let wire = status.to_wire(NOW).unwrap();
    assert_eq!(
      wire.maybe_expires_at,
      Some(teamcraft_proto::UnixMillis(NOW + 3_600_000))
    );
    let back = CustomStatus::from_wire(&wire).unwrap();
    assert_eq!((back.emoji.as_str(), back.text.as_str()), ("🌴", "On vacation"));
    assert_eq!(back.maybe_expires_at, Some(NOW + 3_600_000));
  }

  #[test]
  fn a_status_without_expiry_stays_until_cleared() {
    let status = with_local_expiry(
      CustomStatus {
        emoji: String::new(),
        text: "Heads down".into(),
        expiry: StatusExpiry::Never,
        maybe_expires_at: None,
      },
      NOW,
    );
    let wire = status.to_wire(NOW).unwrap();
    assert_eq!(wire.maybe_expires_at, None);
    assert_eq!(wire.maybe_emoji, None);
  }

  #[test]
  fn today_ends_at_local_midnight() {
    let status = with_local_expiry(
      CustomStatus {
        emoji: "🌴".into(),
        text: "Out".into(),
        expiry: StatusExpiry::Today,
        maybe_expires_at: None,
      },
      NOW,
    );
    assert_eq!(status.maybe_expires_at, Some(end_of_local_day(NOW)));
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
      is_catching_up: false,
      is_notes_to_self: false,
      maybe_permissions: None,
      has_only_deactivated_others: false,
      maybe_image_version: None,
    }
  }
}
