//! A user with two devices, as the mobile apps meet it every day: the desktop is open, so the
//! server sends the phone no notifications, and many messages arrive meanwhile. Then the user picks
//! up the phone; the app comes out of standby and the chat the user opens must show what is new at
//! once, the chat list must fill in as the rest arrives, and the connection must say it is there.
//!
//! The phone runs the [`SyncRunner`] as the apps do, its UI hears only `on_change` (the mobile
//! `EventListener`), and each call is checked against what the store holds at that moment: what the
//! UI would read. Real MLS clients on the fake server, tokio's paused clock, and member lists that
//! take as long as on a mobile network after a long absence.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used, clippy::expect_used)]
// The mobile apps run the core with its UI adapter; the desktop marks a chat read through it.
#![cfg(feature = "ui")]

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::common::{Peer, TestClient, TokioClock, link_device_with_store, settings, settle};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::is_catching_up;
use teamcraft_core::connection::{ConnectionState, ConnectionStatus, NetworkChanges};
use teamcraft_core::read_state;
use teamcraft_core::records::{Draft, enqueue_message};
use teamcraft_core::runner::{RunnerCommand, RunnerSettings, SyncRunner};
use teamcraft_core::store::{LocalStore, MemoryStore, MessageContent, MessageFilter, StoredMessage};
use teamcraft_core::ui_backend::CoreCommand;
use teamcraft_proto::{ClientMessageId, ConversationId, Id, MessageId, Snowflake, UserId};
use tokio::time::Instant;

/// Channels the user shares with Ann; each gets news while the phone sleeps.
const CHANNELS: usize = 24;
/// Messages Ann writes in each channel while the phone sleeps.
const NEWS_PER_CHANNEL: usize = 4;
/// Messages Ann writes to the user directly while the phone sleeps.
const DIRECT_NEWS: usize = 30;
/// What a member list takes after a long absence on a mobile network (several round trips on a
/// busy server); the catch-up reads one per changed conversation.
const MEMBER_LIST_DELAY: Duration = Duration::from_millis(400);
/// What the direct chat holds in the end: the morning's message, Ann's news, the desktop's answer.
const DIRECT_COMPLETE: usize = DIRECT_NEWS + 2;
/// Older messages of the direct chat on the phone, imported from another chat system: a long
/// history is what makes work per message on every change show.
const IMPORTED: usize = 70_000;
/// How long the server keeps a session after its connection ended (its default).
const RESUME_WINDOW: Duration = Duration::from_secs(120);
/// How long the phone stays in its pocket: far past the server's resume window.
const IN_THE_POCKET: Duration = Duration::from_secs(45 * 60);

/// The core's part of "at once": it answers within the network's own time. The fake server
/// answers without delay except for the member lists, so anything above this is the core's
/// waiting, not the network's.
const AT_ONCE: Duration = Duration::from_millis(500);

#[tokio::test(start_paused = true)]
async fn back_from_the_pocket_the_open_chat_shows_its_news_at_once_and_the_list_fills_in() {
  let mut day = Day::begin().await;
  day.phone.awake_until(None, Told::has_the_morning).await;
  day.phone.import_history(day.ann.client.user_id(), IMPORTED);

  day.desktop_afternoon().await;
  day.server.delay_member_lists(day.me, MEMBER_LIST_DELAY);
  // Picked up from the pocket: the app shows its cached list, the user taps the direct chat with
  // Ann at the top, and its reads name it to the core.
  let connects_before = day.server.gateway_connect_count();
  let woke = day
    .phone
    .awake_until(Some(day.direct), Told::has_the_afternoon)
    .await;
  // Away far longer than the server keeps sessions: no resume that can only be refused, one
  // connection, not two.
  assert_eq!(day.server.gateway_connect_count() - connects_before, 1);

  let told_connected = woke
    .first(|told| told.is_connected)
    .expect("never told it is connected");
  assert!(
    told_connected.at <= AT_ONCE,
    "the app heard of the connection {:?} after waking",
    told_connected.at
  );
  let open_chat = woke
    .first(|told| told.direct_texts == DIRECT_COMPLETE)
    .expect("the open chat never showed its news");
  let caught_up = woke.caught_up_at();
  // The open chat's own member list and its messages; the others' member lists come after it.
  assert!(
    open_chat.at <= MEMBER_LIST_DELAY + AT_ONCE,
    "the open chat showed its news {:?} after waking (whole catch-up: {caught_up:?})",
    open_chat.at
  );
  assert!(
    open_chat.at * 4 < caught_up,
    "the open chat waited for the others: {:?} of {caught_up:?}",
    open_chat.at
  );
  // The list fills in as conversations arrive, each marked while it waits, not all at the end.
  let steps = woke.distinct_channel_counts();
  assert!(
    steps.len() >= 4,
    "the list changed in {} steps only: {steps:?}",
    steps.len()
  );
  assert!(
    woke
      .first(|told| told.channels_complete < CHANNELS && told.channels_marked > 0)
      .is_some(),
    "the conversations still to come were never marked as loading"
  );
  day.phone.assert_everything_once(&day);
  // Read on the desktop: no badge on the phone.
  assert_eq!(
    day.phone.unread(day.direct),
    0,
    "read on the desktop, unread on the phone"
  );
}

/// The server's silent sync wake-up reached the phone in the pocket (notifications.md, "Sync
/// wake-ups"): it caught up in the background, so the chat the user opens later shows everything in
/// its first frame, before any connection, and so does the list.
#[tokio::test(start_paused = true)]
async fn after_a_sync_wake_up_everything_is_there_before_any_connection() {
  let mut day = Day::begin().await;
  day.phone.awake_until(None, Told::has_the_morning).await;
  day.phone.import_history(day.ann.client.user_id(), IMPORTED);

  day.desktop_afternoon().await;
  day.server.delay_member_lists(day.me, MEMBER_LIST_DELAY);
  day.phone.catch_up_in_background().await;
  // Still in the pocket for a while; then the session is gone again.
  tokio::time::advance(Duration::from_secs(10 * 60)).await;
  day.server.expire_sessions();

  let first_frame = day.phone.read_now();
  assert_eq!(first_frame.direct_texts, DIRECT_COMPLETE, "the open chat");
  assert_eq!(first_frame.channels_complete, CHANNELS, "the list");
  assert_eq!(first_frame.channels_marked, 0);
  day.phone.assert_everything_once(&day);
  assert_eq!(day.phone.unread(day.direct), 0, "read on the desktop");
}

/// The user answers in the chat they opened while the rest is still caught up: the reply goes out
/// once that chat is current, not after every other conversation.
#[tokio::test(start_paused = true)]
async fn a_reply_right_after_opening_goes_out_before_the_catch_up_ends() {
  let mut day = Day::begin().await;
  day.phone.awake_until(None, Told::has_the_morning).await;

  day.desktop_afternoon().await;
  day.server.delay_member_lists(day.me, MEMBER_LIST_DELAY);
  let (store, direct, me) = (day.phone.store.clone(), day.direct, day.me);
  let sent = Arc::new(Mutex::new(None));
  let sent_at = sent.clone();
  day.phone.client().catch_up_focus().set(direct);
  let woke = day
    .phone
    .awake_while(
      async move {
        let woke_at = Instant::now();
        // Typed as soon as the chat shows Ann's news.
        while texts(store.as_ref(), direct).len() < DIRECT_COMPLETE {
          tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let draft = Draft {
          conversation_id: direct,
          maybe_thread_id: None,
          content: MessageContent::text("on it", Vec::new()),
        };
        let reply = enqueue_message(store.as_ref(), me, 1_800_000_000_000, draft).unwrap();
        let typed_at = woke_at.elapsed();
        while store
          .message(direct, reply)
          .unwrap()
          .and_then(|message| message.maybe_server_id)
          .is_none()
        {
          tokio::time::sleep(Duration::from_millis(20)).await;
          assert!(
            woke_at.elapsed() < Duration::from_secs(60),
            "the reply never went out"
          );
        }
        *sent_at.lock().unwrap() = Some((typed_at, woke_at.elapsed()));
      },
      Told::has_the_afternoon_and_a_reply,
    )
    .await;

  let (typed_at, sent_at) = sent.lock().unwrap().expect("sent");
  let caught_up = woke.caught_up_at();
  assert!(
    sent_at < caught_up,
    "typed at {typed_at:?}, sent at {sent_at:?}, the catch-up ended at {caught_up:?}"
  );
  assert!(
    sent_at - typed_at <= MEMBER_LIST_DELAY * 2 + AT_ONCE,
    "typed at {typed_at:?}, sent at {sent_at:?}"
  );
}

/// Away for less than the resume window (a look at another app): the server replays what was
/// missed, and the open chat has it within the network's time, without a catch-up.
#[tokio::test(start_paused = true)]
async fn back_within_the_resume_window_the_replay_shows_at_once() {
  let mut day = Day::begin().await;
  day.phone.awake_until(None, Told::has_the_morning).await;

  for index in 0..DIRECT_NEWS {
    day
      .ann
      .client
      .send_text(day.direct, None, &format!("quick {index}"))
      .await
      .unwrap();
  }
  tokio::time::advance(Duration::from_secs(60)).await;
  let woke = day
    .phone
    .awake_until(Some(day.direct), |told| told.direct_texts == DIRECT_NEWS + 1)
    .await;

  let open_chat = woke
    .first(|told| told.direct_texts == DIRECT_NEWS + 1)
    .expect("the open chat never showed the replay");
  assert!(
    open_chat.at <= AT_ONCE,
    "the replay showed {:?} after waking",
    open_chat.at
  );
}

/// The user opens another chat while the catch-up still runs: that chat is taken next, after the
/// round trip already on its way, and shows its news before the channels ahead of it.
#[tokio::test(start_paused = true)]
async fn a_chat_opened_during_the_catch_up_is_taken_next() {
  let mut day = Day::begin().await;
  day.phone.awake_until(None, Told::has_the_morning).await;

  day.desktop_afternoon().await;
  day.server.delay_member_lists(day.me, MEMBER_LIST_DELAY);
  let later = day.channels[CHANNELS - 1];
  let focus = day.phone.client().catch_up_focus();
  focus.set(day.direct);
  let switch = MEMBER_LIST_DELAY * 2;
  let woke = day
    .phone
    .awake_while(
      async move {
        tokio::time::sleep(switch).await;
        focus.set(later);
      },
      Told::has_the_afternoon,
    )
    .await;

  let later_index = CHANNELS - 1;
  let opened_later = woke
    .first(|told| told.channel_texts[later_index] == NEWS_PER_CHANNEL + 1)
    .expect("the chat opened later never showed its news");
  let caught_up = woke.caught_up_at();
  assert!(
    opened_later.at <= switch + MEMBER_LIST_DELAY * 2 + AT_ONCE,
    "the chat opened at {switch:?} showed its news at {:?} (whole catch-up: {caught_up:?})",
    opened_later.at
  );
  // It is the last channel in the catch-up's order: everything else came first without the tap.
  assert!(
    opened_later.channels_complete < CHANNELS - 1,
    "it waited for the others: {} channels were through before it",
    opened_later.channels_complete
  );
  day.phone.assert_everything_once(&day);
}

/// The people and conversations of the day: Ann, the user's desktop and the user's phone.
struct Day {
  server: FakeServer,
  ann: Peer,
  desktop: Peer,
  me: teamcraft_proto::UserId,
  channels: Vec<ConversationId>,
  direct: ConversationId,
  phone: Phone,
}

impl Day {
  /// Ann's channels with the user and a direct chat, read on the desktop; the phone linked to it.
  async fn begin() -> Self {
    let server = FakeServer::new();
    server.announce_resume_window(RESUME_WINDOW);
    let mut ann = Peer::enroll(&server, "Ann").await;
    let mut desktop = Peer::enroll(&server, "Me").await;
    let me = desktop.client.user_id();
    let space = ann.space().await;
    let mut channels = Vec::new();
    for index in 0..CHANNELS {
      let channel = ann
        .client
        .create_channel(space, &format!("channel {index}"))
        .await
        .unwrap();
      ann.client.add_member(channel, me).await.unwrap();
      ann
        .client
        .send_text(channel, None, &format!("hello {index}"))
        .await
        .unwrap();
      channels.push(channel);
    }
    let direct = ann.client.open_dm(me).await.unwrap();
    ann.client.send_text(direct, None, "morning").await.unwrap();
    settle(&mut [&mut ann, &mut desktop]).await;
    let (client, store) =
      link_device_with_store(&mut desktop, settings(), Box::new(TokioClock::new().0)).await;
    let phone = Phone::new(client, store, channels.clone(), direct);
    Self {
      server,
      ann,
      desktop,
      me,
      channels,
      direct,
      phone,
    }
  }

  /// The phone in the pocket, the desktop open: Ann writes a lot, the user answers her on the
  /// desktop and reads the direct chat there. The phone hears nothing of it (no notifications
  /// while the desktop is active), and its gateway session expires.
  async fn desktop_afternoon(&mut self) {
    tokio::time::advance(IN_THE_POCKET).await;
    for (index, channel) in self.channels.iter().enumerate() {
      for news in 0..NEWS_PER_CHANNEL {
        self
          .ann
          .client
          .send_text(*channel, None, &format!("news {news} in {index}"))
          .await
          .unwrap();
      }
    }
    for index in 0..DIRECT_NEWS {
      self
        .ann
        .client
        .send_text(self.direct, None, &format!("direct {index}"))
        .await
        .unwrap();
    }
    settle(&mut [&mut self.ann, &mut self.desktop]).await;
    self
      .desktop
      .client
      .send_text(self.direct, None, "on my way")
      .await
      .unwrap();
    self
      .desktop
      .client
      .run_ui_command(CoreCommand::MarkRead {
        conversation: self.direct,
      })
      .await
      .unwrap();
    self.desktop.client.flush_pending_actions().await.unwrap();
    settle(&mut [&mut self.ann, &mut self.desktop]).await;
    // Far past the resume window: the phone's session is gone (this closes every connection).
    self.server.expire_sessions();
  }
}

/// The user's phone: its core between foregrounds, and what its UI was told.
struct Phone {
  maybe_client: Option<TestClient>,
  store: Arc<MemoryStore>,
  network: NetworkChanges,
  status: ConnectionStatus,
  channels: Vec<ConversationId>,
  direct: ConversationId,
  /// Messages of the direct chat imported before the day ([`Self::import_history`]).
  imported: usize,
}

/// One `on_change` and what the UI would have read then.
#[derive(Clone, Debug)]
struct Told {
  /// Since the app came to the foreground.
  at: Duration,
  is_connected: bool,
  direct_texts: usize,
  channel_texts: Vec<usize>,
  /// Channels holding everything Ann wrote.
  channels_complete: usize,
  /// Channels the UI shows as still loading.
  channels_marked: usize,
}

/// Everything the UI was told during one foreground.
struct Woke {
  told: Vec<Told>,
}

impl Phone {
  fn new(
    client: TestClient,
    store: Arc<MemoryStore>,
    channels: Vec<ConversationId>,
    direct: ConversationId,
  ) -> Self {
    Self {
      maybe_client: Some(client),
      store,
      network: NetworkChanges::new(),
      status: ConnectionStatus::new(),
      channels,
      direct,
      imported: 0,
    }
  }

  fn client(&self) -> &TestClient {
    self.maybe_client.as_ref().unwrap()
  }

  /// `count` messages of `sender` before everything the direct chat holds, as an import leaves them.
  fn import_history(&mut self, sender: UserId, count: usize) {
    let oldest = texts_with_ids(self.store.as_ref(), self.direct)
      .first()
      .map(|(id, _)| *id)
      .expect("the morning's message");
    let count_i64 = i64::try_from(count).unwrap();
    for index in 0..count {
      let index_i64 = i64::try_from(index).unwrap();
      let server_id = oldest - count_i64 + index_i64;
      let mut client_message_id = [0x1d; 16];
      client_message_id[8..].copy_from_slice(&server_id.to_be_bytes());
      self
        .store
        .upsert_message(&StoredMessage {
          conversation_id: self.direct,
          client_message_id: ClientMessageId::from_bytes(client_message_id),
          maybe_server_id: Some(MessageId::from_snowflake(Snowflake::new(server_id).unwrap())),
          sender_user_id: sender,
          maybe_thread_id: None,
          sent_at_ms: 1_700_000_000_000 + index_i64,
          content: MessageContent::text(format!("imported {index}"), Vec::new()),
          is_edited: false,
          edits: Vec::new(),
          maybe_edit_id: None,
          is_pinned: false,
          maybe_verified_commit: None,
          maybe_server_change: None,
          maybe_unreadable: None,
        })
        .unwrap();
    }
    self.imported = count;
  }

  /// A `sync` wake-up in the pocket: the sync loop runs until the connection is up and caught up,
  /// then stops, as `TeamCraft::catch_up_in_background` does it. Nothing is on screen.
  async fn catch_up_in_background(&mut self) {
    let client = self.maybe_client.take().unwrap();
    let (mut runner, handle) = SyncRunner::new(client, RunnerSettings::default(), Box::new(|| {}));
    runner.share_connection_status(self.status.clone());
    let (caught_up, is_caught_up) = futures_channel::oneshot::channel();
    assert!(handle.send(RunnerCommand::CatchUp(caught_up)));
    let woken_at = Instant::now();
    let stop = async move {
      is_caught_up.await.unwrap();
      assert!(
        woken_at.elapsed() < Duration::from_secs(60),
        "the background catch-up took too long"
      );
      assert!(handle.send(RunnerCommand::Stop));
    };
    let (client, ()) = tokio::join!(runner.run(), stop);
    self.maybe_client = Some(client);
  }

  /// What the UI reads from the store right now, before any connection (the app's first frame).
  fn read_now(&self) -> Told {
    Told::read(
      Duration::ZERO,
      &self.status,
      self.store.as_ref(),
      &self.channels,
      (self.direct, self.imported),
    )
  }

  /// The app in the foreground, `maybe_open` the chat on screen, until the UI would read what
  /// `is_done` asks for; then a few more seconds, and back to the pocket.
  async fn awake_until(&mut self, maybe_open: Option<ConversationId>, is_done: fn(&Told) -> bool) -> Woke {
    if let Some(open) = maybe_open {
      self.client().catch_up_focus().set(open);
    }
    self.awake_while(std::future::ready(()), is_done).await
  }

  /// [`Self::awake_until`], with `steer` running beside it (the user's taps).
  async fn awake_while(&mut self, steer: impl Future<Output = ()>, is_done: fn(&Told) -> bool) -> Woke {
    let told = Arc::new(Mutex::new(Vec::new()));
    let woke_at = Instant::now();
    let on_change = {
      let (told, store, status) = (told.clone(), self.store.clone(), self.status.clone());
      let (channels, direct, imported) = (self.channels.clone(), self.direct, self.imported);
      Box::new(move || {
        let seen = Told::read(
          woke_at.elapsed(),
          &status,
          store.as_ref(),
          &channels,
          (direct, imported),
        );
        told.lock().unwrap().push(seen);
      })
    };
    let client = self.maybe_client.take().unwrap();
    let (mut runner, handle) = SyncRunner::new(client, RunnerSettings::default(), on_change);
    runner.share_connection_status(self.status.clone());
    runner.watch_network(&self.network);
    // The app reports the network on every return to the foreground.
    self.network.notify();
    let (store, status) = (self.store.clone(), self.status.clone());
    let (channels, direct) = (self.channels.clone(), (self.direct, self.imported));
    let user = async move {
      steer.await;
      while !is_done(&Told::read(
        woke_at.elapsed(),
        &status,
        store.as_ref(),
        &channels,
        direct,
      )) {
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(woke_at.elapsed() < Duration::from_secs(120), "never caught up");
      }
      tokio::time::sleep(Duration::from_secs(2)).await;
      assert!(handle.send(RunnerCommand::Stop));
    };
    let (client, ()) = tokio::join!(runner.run(), user);
    self.maybe_client = Some(client);
    let told = told.lock().unwrap().clone();
    Woke { told }
  }

  /// Every message of the day once, in order, and nothing marked as loading.
  fn assert_everything_once(&self, day: &Day) {
    let mut direct: Vec<String> = (0..self.imported)
      .map(|index| format!("imported {index}"))
      .collect();
    direct.push("morning".into());
    direct.extend((0..DIRECT_NEWS).map(|index| format!("direct {index}")));
    direct.push("on my way".into());
    assert_eq!(texts(self.store.as_ref(), day.direct), direct);
    for (index, channel) in day.channels.iter().enumerate() {
      let mut expected = vec![format!("hello {index}")];
      expected.extend((0..NEWS_PER_CHANNEL).map(|news| format!("news {news} in {index}")));
      assert_eq!(texts(self.store.as_ref(), *channel), expected, "channel {index}");
      assert!(!is_catching_up(self.store.as_ref(), *channel).unwrap());
    }
  }

  /// The unread count the phone shows for `conversation`.
  fn unread(&self, conversation: ConversationId) -> u32 {
    read_state::unread(self.store.as_ref())
      .unwrap()
      .and_then(|unread| {
        unread
          .conversations
          .into_iter()
          .find(|entry| entry.conversation_id == conversation)
      })
      .map_or(0, |entry| entry.unread_count)
  }
}

impl Told {
  fn read(
    at: Duration,
    status: &ConnectionStatus,
    store: &MemoryStore,
    channels: &[ConversationId],
    (direct, imported): (ConversationId, usize),
  ) -> Self {
    let channel_texts: Vec<usize> = channels
      .iter()
      .map(|channel| texts(store, *channel).len())
      .collect();
    Self {
      at,
      is_connected: status.get() == ConnectionState::Connected,
      direct_texts: texts(store, direct).len() - imported,
      channels_complete: channel_texts
        .iter()
        .filter(|count| **count == NEWS_PER_CHANNEL + 1)
        .count(),
      channels_marked: channels
        .iter()
        .filter(|channel| is_catching_up(store, **channel).unwrap())
        .count(),
      channel_texts,
    }
  }

  /// Connected, and the morning's messages are there.
  fn has_the_morning(&self) -> bool {
    self.is_connected
      && self.channels_marked == 0
      && self.direct_texts == 1
      && self.channel_texts.iter().all(|count| *count == 1)
  }

  /// [`Self::has_the_afternoon`], with the phone's own reply after it.
  fn has_the_afternoon_and_a_reply(&self) -> bool {
    self.is_connected
      && self.channels_marked == 0
      && self.direct_texts == DIRECT_COMPLETE + 1
      && self.channels_complete == CHANNELS
  }

  /// Connected, and every message of the afternoon is there.
  fn has_the_afternoon(&self) -> bool {
    self.is_connected
      && self.channels_marked == 0
      && self.direct_texts == DIRECT_COMPLETE
      && self.channels_complete == CHANNELS
  }
}

impl Woke {
  fn first(&self, matches: impl Fn(&Told) -> bool) -> Option<&Told> {
    self.told.iter().find(|told| matches(told))
  }

  /// When the UI was told that the last channel holds everything.
  fn caught_up_at(&self) -> Duration {
    self
      .first(|told| told.channels_complete == CHANNELS)
      .expect("never caught up")
      .at
  }

  fn distinct_channel_counts(&self) -> Vec<usize> {
    let mut counts: Vec<usize> = self.told.iter().map(|told| told.channels_complete).collect();
    counts.dedup();
    counts
  }
}

/// The texts of `conversation` with their server IDs, oldest first.
fn texts_with_ids(store: &impl LocalStore, conversation: ConversationId) -> Vec<(i64, String)> {
  store
    .messages(conversation, MessageFilter::TopLevel)
    .unwrap()
    .into_iter()
    .filter_map(|message| match (message.maybe_server_id, message.content) {
      (Some(id), MessageContent::Text { body, .. }) => Some((id.snowflake().get(), body)),
      _ => None,
    })
    .collect()
}

fn texts(store: &impl LocalStore, conversation: ConversationId) -> Vec<String> {
  store
    .messages(conversation, MessageFilter::TopLevel)
    .unwrap()
    .into_iter()
    .filter_map(|message| match message.content {
      MessageContent::Text { body, .. } => Some(body),
      _ => None,
    })
    .collect()
}
