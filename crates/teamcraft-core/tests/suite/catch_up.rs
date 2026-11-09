//! Catching up after being away (issue #247): every conversation arrives, the marks the UI shows as
//! "catching up" are gone afterwards, also after a catch-up that failed half way.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use crate::common::{Peer, clock, link_device, settings, settle};
use teamcraft_core::ClientSettings;
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::{FakeServer, NetworkCut};
use teamcraft_core::client::is_catching_up;
use teamcraft_core::clock::{Clock, ManualClock};
use teamcraft_core::records;
use teamcraft_core::runner::{RunnerCommand, RunnerSettings, SyncRunner};
use teamcraft_core::store::{LocalStore, MessageContent, MessageFilter, StoredMessage, Table, Unreadable};
use teamcraft_proto::{ClientMessageId, ConversationId, ListMessagesQuery, UserId};

/// How long each member list takes in the tests about the focused conversation.
const MEMBER_LIST_DELAY: Duration = Duration::from_secs(1);

#[tokio::test]
async fn a_new_device_catches_up_every_conversation_and_drops_the_marks() {
  let (_server, mut carol, channels) = busy_workspace().await;
  let mut laptop = link_device(&mut carol).await;
  // Another conversation is in focus than the first one: all of them still arrive.
  laptop.catch_up_focus().set(channels[3]);

  laptop.connect_gateway().await.unwrap();

  for (index, channel) in channels.iter().enumerate() {
    assert_eq!(
      texts(laptop.store(), *channel),
      vec![format!("news {index}")],
      "channel {index}"
    );
    assert!(!is_catching_up(laptop.store(), *channel).unwrap());
  }
}

#[tokio::test]
async fn a_catch_up_cut_short_leaves_no_marks() {
  let (server, mut carol, channels) = busy_workspace().await;
  let mut laptop = link_device(&mut carol).await;
  // Enough calls to get into the per-conversation catch-up, not through it.
  server.cut_network_after(laptop.user_id(), 30, NetworkCut::BeforeRequest);

  assert!(laptop.connect_gateway().await.is_err());

  for channel in &channels {
    assert!(!is_catching_up(laptop.store(), *channel).unwrap());
  }
}

#[tokio::test]
async fn messages_missed_in_a_conversation_that_was_empty_arrive() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let space = alice.space().await;
  let channel = alice.client.create_channel(space, "quiet").await.unwrap();
  alice
    .client
    .add_member(channel, carol.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut carol]).await;
  // Carol's history of the channel is loaded, and none of it is stored (all of it expired, say).
  carol.client.full_sync().await.unwrap();
  carol.store.delete_messages(channel).unwrap();

  // Written while Carol is away; her catch-up reads the newest page ahead of the queue.
  alice
    .client
    .send_text(channel, None, "while you were away")
    .await
    .unwrap();
  carol.client.full_sync().await.unwrap();

  assert_eq!(carol.texts(channel), vec!["while you were away".to_string()]);
}

/// The conversation the user looks at is readable before the catch-up has read the other
/// conversations' member lists and welcomes (issue #247: 30 s on a large workspace before).
#[tokio::test(start_paused = true)]
async fn the_focused_conversation_is_readable_before_the_others_are_caught_up() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  // Carol is away: welcomes and messages wait for her catch-up. Three batches of member lists,
  // and joining a group reads its member list too.
  let channels = channels_with(&mut alice, carol.client.user_id(), 13).await;
  server.delay_member_lists(carol.client.user_id(), MEMBER_LIST_DELAY);
  carol.client.catch_up_focus().set(channels[7]);

  {
    let sync = std::pin::pin!(carol.client.full_sync());
    tokio::select! {
      _ = sync => panic!("the other member lists should still be on their way"),
      () = tokio::time::sleep(MEMBER_LIST_DELAY * 6) => {},
    }
  }
  assert_eq!(carol.texts(channels[7]), vec!["news 7".to_string()]);
  assert!(carol.texts(channels[0]).is_empty());
  assert_eq!(
    carol.client.catch_up_focus().caught_up_count(),
    1,
    "the host is told"
  );

  // The rest arrives with the next sync; the welcome taken ahead didn't skip the others.
  carol.client.full_sync().await.unwrap();
  for (index, channel) in channels.iter().enumerate() {
    assert_eq!(
      carol.texts(*channel),
      vec![format!("news {index}")],
      "channel {index}"
    );
    assert!(!is_catching_up(&carol.store, *channel).unwrap());
  }
}

/// The focus moves while the catch-up runs: each conversation it pointed to is taken ahead, and
/// all of them arrive.
#[tokio::test(start_paused = true)]
async fn a_focus_set_during_the_catch_up_is_taken_next() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let channels = channels_with(&mut alice, carol.client.user_id(), 13).await;
  server.delay_member_lists(carol.client.user_id(), MEMBER_LIST_DELAY);
  let focus = carol.client.catch_up_focus();
  let late = channels[11];

  {
    let sync = std::pin::pin!(carol.client.full_sync());
    let steer = async {
      // During the first batch of member lists.
      tokio::time::sleep(MEMBER_LIST_DELAY / 2).await;
      focus.set(late);
      tokio::time::sleep(MEMBER_LIST_DELAY * 7).await;
    };
    tokio::select! {
      _ = sync => panic!("the other member lists should still be on their way"),
      () = steer => {},
    }
  }
  assert_eq!(carol.texts(late), vec!["news 11".to_string()]);
  assert!(carol.texts(channels[12]).is_empty());

  carol.client.full_sync().await.unwrap();
  for (index, channel) in channels.iter().enumerate() {
    assert_eq!(
      carol.texts(*channel),
      vec![format!("news {index}")],
      "channel {index}"
    );
  }
}

/// Carol is away while Alice writes 30 messages to their channel, more than Carol's catch-up reads
/// in order (issue #1423). It reads ten, then the newest five, then the rest: cut short after one
/// page of the rest, she has the newest messages; the next catch-up reads what is left, and every
/// message is there once, in order, nothing left marked.
#[tokio::test]
async fn a_busy_channel_shows_its_newest_messages_first_and_the_rest_by_the_end() {
  let (server, mut alice, mut carol, channel) = busy_channel().await;
  let busy: Vec<String> = (0..30).map(|index| format!("busy {index}")).collect();
  for text in &busy {
    alice.client.send_text(channel, None, text).await.unwrap();
  }
  // Two pages in order, the newest page, one page of the rest; then the network goes.
  server.cut_message_lists_after(carol.client.user_id(), Some(4));

  assert!(carol.client.full_sync().await.is_err());

  let mut expected = vec!["before".to_owned()];
  expected.extend(busy[..15].iter().cloned());
  expected.extend(busy[25..].iter().cloned());
  assert_eq!(
    carol.texts(channel),
    expected,
    "in order up to busy 14, and the newest page"
  );

  server.cut_message_lists_after(carol.client.user_id(), None);
  // Nothing new since: only the backlog makes the channel due.
  carol.client.full_sync().await.unwrap();

  let mut everything = vec!["before".to_owned()];
  everything.extend(busy);
  assert_eq!(carol.texts(channel), everything);
  assert!(!is_catching_up(carol.client.store(), channel).unwrap());
  assert!(
    carol
      .store
      .get(Table::SyncState, &format!("history_gaps/{channel}"))
      .unwrap()
      .is_none(),
    "a hole is left"
  );
  settle(&mut [&mut alice, &mut carol]).await;
  assert_eq!(
    carol.texts(channel).len(),
    31,
    "the live events add nothing twice"
  );
}

/// Alice pins a message Carol holds and unpins it later, both while Carol is away: the pin lies in
/// the backlog, which Carol reads after the newest page with the unpin. The order counts, not the
/// order read: the message ends up unpinned.
#[tokio::test]
async fn a_pin_in_the_backlog_doesnt_undo_a_newer_unpin() {
  let (_server, mut alice, mut carol, channel) = busy_channel().await;
  let target = alice.client.send_text(channel, None, "pin me").await.unwrap();
  settle(&mut [&mut alice, &mut carol]).await;
  carol.client.full_sync().await.unwrap();
  assert!(!is_pinned(&carol, channel, target));

  // Two pages in order (ten of the twelve), the pin among the rest, the unpin in the newest page.
  send_many(&mut alice, channel, "a", 12).await;
  alice.client.set_pinned(channel, target, true).await.unwrap();
  send_many(&mut alice, channel, "b", 12).await;
  alice.client.set_pinned(channel, target, false).await.unwrap();
  send_many(&mut alice, channel, "c", 2).await;
  carol.client.full_sync().await.unwrap();

  assert!(!is_pinned(&carol, channel, target), "the older pin won");
  assert_eq!(carol.texts(channel).len(), 2 + 26);
  assert!(!is_catching_up(carol.client.store(), channel).unwrap());
}

/// Issue #1448: Alice pins a message and unpins it later; Carol couldn't read the pin when it came
/// (parked for its key, #888) and decrypts it once the unpin is there. The newer unpin stays.
#[tokio::test]
async fn a_pin_decrypted_after_a_newer_unpin_doesnt_undo_it() {
  let (_server, mut alice, mut carol, channel) = busy_channel().await;
  let target = alice.client.send_text(channel, None, "pin me").await.unwrap();
  alice.client.set_pinned(channel, target, true).await.unwrap();
  alice.client.set_pinned(channel, target, false).await.unwrap();
  settle(&mut [&mut alice, &mut carol]).await;
  assert!(!is_pinned(&carol, channel, target));

  // As if the pin had waited for its key: a placeholder at its place, its envelope parked.
  let pin = carol
    .store
    .messages(channel, MessageFilter::All)
    .unwrap()
    .into_iter()
    .find(|message| matches!(message.content, MessageContent::Pin { is_pinned: true, .. }))
    .unwrap();
  let envelope = carol
    .api
    .list_messages(channel, &ListMessagesQuery::default())
    .await
    .unwrap()
    .messages
    .into_iter()
    .find(|message| message.client_message_id == pin.client_message_id)
    .unwrap();
  carol
    .store
    .upsert_message(&StoredMessage {
      content: MessageContent::Unknown,
      maybe_unreadable: Some(Unreadable::Waiting { since_ms: 0 }),
      ..pin.clone()
    })
    .unwrap();
  let parked = format!("{channel}/{}", pin.client_message_id.to_hex());
  records::put_json(&*carol.store, Table::Undecrypted, &parked, &envelope).unwrap();

  // The catch-up tries it again, and now it opens.
  carol.client.full_sync().await.unwrap();

  let decrypted = carol
    .store
    .message(channel, pin.client_message_id)
    .unwrap()
    .unwrap();
  assert!(decrypted.maybe_unreadable.is_none(), "decrypted");
  assert!(!is_pinned(&carol, channel, target), "the older pin won");
}

/// Issue #1448: Alice unpins a message while Carol, who hasn't read that yet, pins it: Carol's pin
/// comes after the unpin on the server, so it stays pinned everywhere, also once Carol reads the
/// unpin.
#[tokio::test]
async fn an_own_pin_sent_after_an_unpin_not_read_yet_stays() {
  let (_server, mut alice, mut carol, channel) = busy_channel().await;
  let target = alice.client.send_text(channel, None, "pin me").await.unwrap();
  alice.client.set_pinned(channel, target, true).await.unwrap();
  settle(&mut [&mut alice, &mut carol]).await;
  assert!(is_pinned(&carol, channel, target));

  alice.client.set_pinned(channel, target, false).await.unwrap();
  carol.client.set_pinned(channel, target, true).await.unwrap();
  settle(&mut [&mut alice, &mut carol]).await;

  assert!(is_pinned(&alice, channel, target));
  assert!(is_pinned(&carol, channel, target), "the older unpin won");
}

/// Issue #1572: a page of a backlog read in the background wakes the UI only for the conversation on
/// screen; for any other the UI hears of it once, when its backlog is read full. A wake per page had
/// the web UI read everything again for every page of a long absence.
#[tokio::test]
async fn backlog_pages_wake_the_ui_only_for_the_conversation_on_screen() {
  for is_on_screen in [false, true] {
    let (_server, mut alice, mut carol, channel) = busy_channel().await;
    carol.client.read_backlogs_in_background().unwrap();
    send_many(&mut alice, channel, "a", 30).await;
    carol.client.full_sync().await.unwrap();
    if is_on_screen {
      carol.client.catch_up_focus().set(channel);
    }
    let mut wakes = Vec::new();
    while carol.client.is_background_history_due() {
      wakes.push(carol.client.load_background_history_page().await);
    }
    assert!(wakes.len() > 2, "several pages: {wakes:?}");
    let (last, pages) = wakes.split_last().unwrap();
    assert!(*last, "the end of the backlog wakes the UI");
    assert_eq!(
      pages.iter().all(|wake| *wake),
      is_on_screen,
      "on screen: {is_on_screen}, wakes: {wakes:?}"
    );
    assert!(pages.iter().all(|wake| *wake == is_on_screen));
  }
}

/// Issue #1448: with the host reading backlogs in the background, the sync ends after the newest
/// page with the hole still marked, and the host's loop reads the rest, a page per turn, in order.
#[tokio::test]
async fn a_host_reads_the_backlog_after_the_sync() {
  let (_server, mut alice, mut carol, channel) = busy_channel().await;
  carol.client.read_backlogs_in_background().unwrap();
  let target = alice.client.send_text(channel, None, "pin me").await.unwrap();
  settle(&mut [&mut alice, &mut carol]).await;
  // The pin among the rest, the unpin in the newest page.
  send_many(&mut alice, channel, "a", 12).await;
  alice.client.set_pinned(channel, target, true).await.unwrap();
  send_many(&mut alice, channel, "b", 12).await;
  alice.client.set_pinned(channel, target, false).await.unwrap();
  send_many(&mut alice, channel, "c", 2).await;

  carol.client.full_sync().await.unwrap();

  assert_eq!(
    carol.texts(channel).len(),
    2 + 10 + 4,
    "in order up to a 9, and the newest page from b 10 on"
  );
  assert!(
    is_catching_up(carol.client.store(), channel).unwrap(),
    "the hole is still to read"
  );
  let mut turns = 0;
  while carol.client.is_background_history_due() {
    carol.client.load_background_history_page().await;
    turns += 1;
  }
  assert!(turns > 1, "a page per turn, not all at once");
  let mut everything = vec!["before".to_owned(), "pin me".to_owned()];
  everything.extend((0..12).map(|index| format!("a {index}")));
  everything.extend((0..12).map(|index| format!("b {index}")));
  everything.extend((0..2).map(|index| format!("c {index}")));
  assert_eq!(carol.texts(channel), everything);
  assert!(!is_pinned(&carol, channel, target), "the older pin won");
  assert!(!is_catching_up(carol.client.store(), channel).unwrap());
  assert!(
    carol
      .store
      .get(Table::SyncState, &format!("history_gaps/{channel}"))
      .unwrap()
      .is_none()
  );
}

/// Issue #1448: a backlog a catch-up cut short left is read by the host's loop from where it stood,
/// without another catch-up; a page that fails waits for its retry time.
#[tokio::test]
async fn a_host_reads_on_where_a_backlog_was_left_and_retries_a_failed_page_later() {
  let clock = Arc::new(ManualClock::new(1_800_000_000_000));
  let (server, mut alice, mut carol, channel) = busy_channel_on(Box::new(clock.clone())).await;
  let busy: Vec<String> = (0..30).map(|index| format!("busy {index}")).collect();
  for text in &busy {
    alice.client.send_text(channel, None, text).await.unwrap();
  }
  // Two pages in order, the newest page, one page of the rest; then the network goes.
  server.cut_message_lists_after(carol.client.user_id(), Some(4));
  assert!(carol.client.full_sync().await.is_err());

  // The app starts again (the loop finds the backlog), the network is still gone.
  carol.client.read_backlogs_in_background().unwrap();
  assert!(carol.client.is_background_history_due());
  carol.client.load_background_history_page().await;
  assert!(
    !carol.client.is_background_history_due(),
    "waits for its retry time"
  );

  server.cut_message_lists_after(carol.client.user_id(), None);
  clock.advance(30_000);
  while carol.client.is_background_history_due() {
    carol.client.load_background_history_page().await;
  }
  let mut everything = vec!["before".to_owned()];
  everything.extend(busy);
  assert_eq!(carol.texts(channel), everything);
}

/// Issue #1448: the apps' sync runner reads the backlog after the sync, and a background wake-up's
/// "caught up" waits for it.
#[tokio::test]
async fn the_runner_reads_the_backlog_before_it_says_caught_up() {
  let (server, mut alice, carol, channel) = busy_channel().await;
  send_many(&mut alice, channel, "busy", 30).await;
  // A fresh session, so the runner's connection catches up.
  server.expire_sessions();
  let Peer { client, store, .. } = carol;
  let (runner, handle) = SyncRunner::new(client, unpaced(), Box::new(|| {}));
  let (caught_up, is_caught_up) = futures_channel::oneshot::channel();
  assert!(handle.send(RunnerCommand::CatchUp(caught_up)));
  let stop = async {
    tokio::time::timeout(Duration::from_secs(60), is_caught_up)
      .await
      .expect("never caught up")
      .unwrap();
    assert_eq!(texts(store.as_ref(), channel).len(), 31, "the whole backlog");
    assert!(!is_catching_up(store.as_ref(), channel).unwrap());
    assert!(handle.send(RunnerCommand::Stop));
  };
  tokio::join!(runner.run(), stop);
}

/// Alice's channel with Carol, "before" in it, read by Carol; Carol reads pages of five and two of
/// them in order before the newest.
/// Issue #1575: a host that runs the client on the UI's thread (web) asks for turns. A catch-up of
/// several conversations then hands the thread back between them and within a page of messages, so
/// the UI reads half-caught-up state meanwhile instead of waiting for the end; without turns the
/// same catch-up (against a server that answers at once) never lets it in.
#[tokio::test]
async fn a_catch_up_hands_the_thread_back_between_conversations_and_within_a_page() {
  for is_taking_turns in [false, true] {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let turns = ClientSettings {
      maybe_thread_turn: is_taking_turns.then_some(Duration::ZERO),
      ..settings()
    };
    let mut carol = Peer::enroll_with(server.add_user("Carol"), "Carol", turns).await;
    let channels = channels_with(&mut alice, carol.client.user_id(), 4).await;
    settle(&mut [&mut alice, &mut carol]).await;
    carol.client.full_sync().await.unwrap();
    for channel in &channels {
      send_many(&mut alice, *channel, "away", 10).await;
    }
    let store = carol.store.clone();
    let seen = std::cell::RefCell::new(Vec::new());
    let ui = async {
      #[allow(clippy::infinite_loop, reason = "the UI reads until the sync is done")]
      loop {
        let counts: Vec<usize> = channels
          .iter()
          .map(|channel| texts(&*store, *channel).len())
          .collect();
        seen.borrow_mut().push(counts);
        tokio::task::yield_now().await;
      }
    };
    {
      let sync = std::pin::pin!(carol.client.full_sync());
      let ui = std::pin::pin!(ui);
      match futures_util::future::select(sync, ui).await {
        futures_util::future::Either::Left((synced, _)) => synced.unwrap(),
        futures_util::future::Either::Right((never, _)) => match never {},
      }
    }
    for channel in &channels {
      assert_eq!(texts(&*store, *channel).len(), 11, "every message arrived");
    }
    let seen = seen.into_inner();
    // A conversation through and the next one not yet begun.
    let between = seen
      .iter()
      .any(|counts| counts.contains(&11) && counts.contains(&1));
    // A conversation part way through its page.
    let within = seen
      .iter()
      .any(|counts| counts.iter().any(|count| (2..11).contains(count)));
    assert_eq!(
      (between, within),
      (is_taking_turns, is_taking_turns),
      "taking turns: {is_taking_turns}, the UI saw {} states",
      seen.len()
    );
  }
}

/// Issue #1575: the native runner reads a backlog's pages paced, with a pause of at least
/// [`RunnerSettings::background_page_gap`] between two, instead of back to back: the app's reads
/// on their own thread queued up behind the engine's pages for the whole backlog.
#[tokio::test(start_paused = true)]
async fn the_runner_reads_a_backlog_paced() {
  let (clock, _) = crate::common::TokioClock::new();
  let (server, mut alice, carol, channel) = busy_channel_on(Box::new(clock)).await;
  send_many(&mut alice, channel, "busy", 30).await;
  server.expire_sessions();
  let Peer { client, store, .. } = carol;
  let settings = RunnerSettings::default();
  let gap = settings.background_page_gap;
  let (runner, handle) = SyncRunner::new(client, settings, Box::new(|| {}));
  let (caught_up, is_caught_up) = futures_channel::oneshot::channel();
  assert!(handle.send(RunnerCommand::CatchUp(caught_up)));
  let watch = async {
    let mut arrivals = Vec::new();
    let mut held = texts(store.as_ref(), channel).len();
    let started = tokio::time::Instant::now();
    while held < 31 {
      assert!(
        started.elapsed() < Duration::from_secs(60),
        "the backlog never arrived"
      );
      tokio::time::sleep(Duration::from_millis(1)).await;
      let now_held = texts(store.as_ref(), channel).len();
      if now_held != held {
        arrivals.push(tokio::time::Instant::now());
        held = now_held;
      }
    }
    is_caught_up.await.unwrap();
    assert!(handle.send(RunnerCommand::Stop));
    arrivals
  };
  let (arrivals, _) = tokio::join!(watch, runner.run());
  // The catch-up's own pages come at once; the backlog's after it, a gap apart.
  let backlog_pages = &arrivals[arrivals.len().saturating_sub(3)..];
  assert!(backlog_pages.len() >= 2, "arrivals: {arrivals:?}");
  for pair in backlog_pages.windows(2) {
    assert!(pair[1] - pair[0] >= gap, "pages {:?} apart", pair[1] - pair[0]);
  }
}

/// The runner's settings for a test whose clock stands still: backlog pages paced by it would never
/// come due.
fn unpaced() -> RunnerSettings {
  RunnerSettings {
    background_page_gap: Duration::ZERO,
    ..RunnerSettings::default()
  }
}

async fn busy_channel() -> (FakeServer, Peer, Peer, ConversationId) {
  busy_channel_on(clock()).await
}

/// [`busy_channel`] with Carol on `carol_clock`.
async fn busy_channel_on(carol_clock: Box<dyn Clock>) -> (FakeServer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let capped = ClientSettings {
    history_page_size: 5,
    catch_up_message_pages: Some(2),
    ..settings()
  };
  let mut carol = Peer::enroll_on_clock(server.add_user("Carol"), "Carol", capped, carol_clock).await;
  let space = alice.space().await;
  let channel = alice.client.create_channel(space, "busy").await.unwrap();
  alice
    .client
    .add_member(channel, carol.client.user_id())
    .await
    .unwrap();
  alice.client.send_text(channel, None, "before").await.unwrap();
  settle(&mut [&mut alice, &mut carol]).await;
  carol.client.full_sync().await.unwrap();
  assert_eq!(carol.texts(channel), vec!["before".to_owned()]);
  (server, alice, carol, channel)
}

async fn send_many(peer: &mut Peer, channel: ConversationId, prefix: &str, count: usize) {
  for index in 0..count {
    peer
      .client
      .send_text(channel, None, &format!("{prefix} {index}"))
      .await
      .unwrap();
  }
}

fn is_pinned(peer: &Peer, channel: ConversationId, target: ClientMessageId) -> bool {
  peer.store.message(channel, target).unwrap().unwrap().is_pinned
}

/// `count` new channels of Alice with `member` added and a message in each; Alice's side settled.
async fn channels_with(alice: &mut Peer, member: UserId, count: usize) -> Vec<ConversationId> {
  let space = alice.space().await;
  let mut channels = Vec::new();
  for index in 0..count {
    let channel = alice
      .client
      .create_channel(space, &format!("channel {index}"))
      .await
      .unwrap();
    alice.client.add_member(channel, member).await.unwrap();
    alice
      .client
      .send_text(channel, None, &format!("news {index}"))
      .await
      .unwrap();
    channels.push(channel);
  }
  channels
}

/// Alice's five channels with Carol in them and a message in each.
async fn busy_workspace() -> (FakeServer, Peer, Vec<ConversationId>) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let space = alice.space().await;
  let mut channels = Vec::new();
  for index in 0..5 {
    let channel = alice
      .client
      .create_channel(space, &format!("channel {index}"))
      .await
      .unwrap();
    alice
      .client
      .add_member(channel, carol.client.user_id())
      .await
      .unwrap();
    alice
      .client
      .send_text(channel, None, &format!("news {index}"))
      .await
      .unwrap();
    channels.push(channel);
  }
  settle(&mut [&mut alice, &mut carol]).await;
  (server, carol, channels)
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
