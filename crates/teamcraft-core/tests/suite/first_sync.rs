//! The first sync of a newly set up device: progress in real numbers, going on where it stood after
//! a network loss or a kill, what it keeps, and nothing read again at later starts.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use crate::common::{Peer, TestClient, clock, link_device_on_clock, link_device_with, settings, settle};
use teamcraft_core::Client;
use teamcraft_core::ClientSettings;
use teamcraft_core::api::fake::{FakeServer, NetworkCut};
use teamcraft_core::client::{FirstSyncStage, HistoryBackfill, first_sync_progress};
use teamcraft_core::clock::ManualClock;
use teamcraft_core::runner::{RunnerCommand, RunnerSettings, SyncRunner};
use teamcraft_core::store::{LocalStore, MessageContent, MessageFilter};
use teamcraft_proto::ConversationId;

const MESSAGES_PER_CHANNEL: usize = 25;
const PAGE: u32 = 10;
/// A window that holds everything the fake server has.
const ALL: HistoryBackfill = HistoryBackfill {
  window: Duration::from_secs(10 * 365 * 24 * 60 * 60),
  max_per_conversation: 100_000,
  max_messages: 100_000,
};

#[tokio::test]
async fn progress_counts_conversations_through_both_stages_and_ends() {
  let (_server, mut carol, channels) = busy_workspace(3).await;
  let mut laptop = link_device_with(&mut carol, backfilling(ALL)).await;
  // Set up, nothing read: no record yet, then the sync says where it stands.
  assert_eq!(first_sync_progress(laptop.store()).unwrap(), None);

  laptop.connect_gateway().await.unwrap();

  let after_newest = progress(&laptop);
  assert_eq!(after_newest.stage, FirstSyncStage::Older);
  assert_eq!((after_newest.done, after_newest.total), (0, channels.len()));
  assert_eq!(after_newest.maybe_fraction, Some(0.5));
  // Newest pages only so far, older history follows page by page.
  assert_eq!(stored(&laptop, channels[0]), PAGE as usize);

  let mut last_fraction = 0.5;
  let mut pages = 0;
  while laptop.is_older_history_due() {
    laptop.load_older_history_page().await;
    pages += 1;
    if let Some(now) = first_sync_progress(laptop.store()).unwrap() {
      let fraction = now.maybe_fraction.unwrap();
      assert!(fraction >= last_fraction, "the bar never moves back");
      last_fraction = fraction;
    }
    assert!(pages < 100, "older history must end");
  }

  assert_eq!(first_sync_progress(laptop.store()).unwrap(), None);
  for channel in &channels {
    assert_eq!(stored(&laptop, *channel), MESSAGES_PER_CHANNEL);
  }
}

#[tokio::test]
async fn nothing_finished_yet_means_an_indeterminate_bar() {
  let (server, mut carol, _channels) = busy_workspace(3).await;
  let mut laptop = link_device_with(&mut carol, backfilling(ALL)).await;
  // Through the first reads, not to the end of the catch-up.
  server.cut_network_after(laptop.user_id(), 20, NetworkCut::BeforeRequest);

  assert!(laptop.connect_gateway().await.is_err());

  let cut = progress(&laptop);
  assert_eq!(cut.stage, FirstSyncStage::Newest);
  assert_eq!(cut.done, 0);
  assert_eq!(cut.maybe_fraction, None);
}

/// The activity feed and the threads inbox wait for the newest messages: no "all caught up" on a
/// device that hasn't read anything yet (issue #297).
#[cfg(feature = "ui")]
#[tokio::test]
async fn the_activity_lists_load_until_the_newest_messages_are_here() {
  use teamcraft_client_api::backend::ChatBackend;
  use teamcraft_client_api::list_load::{ListLoad, LoadingList};
  use teamcraft_core::ui_backend::CoreChatBackend;

  let (server, mut carol, _channels) = busy_workspace(3).await;
  let mut laptop = link_device_with(&mut carol, backfilling(ALL)).await;
  server.cut_network_after(laptop.user_id(), 20, NetworkCut::BeforeRequest);
  assert!(laptop.connect_gateway().await.is_err());
  assert_eq!(progress(&laptop).stage, FirstSyncStage::Newest);
  let ui = CoreChatBackend::new(laptop.store().clone(), laptop.user_id(), clock());
  assert_eq!(ui.list_load(&LoadingList::Activity), ListLoad::Loading);

  server.set_offline(laptop.user_id(), false);
  laptop.connect_gateway().await.unwrap();
  assert_eq!(progress(&laptop).stage, FirstSyncStage::Older);
  assert_eq!(ui.list_load(&LoadingList::Activity), ListLoad::Ready);
}

#[tokio::test]
async fn a_catch_up_cut_short_goes_on_with_the_conversations_left() {
  let (server, mut carol, channels) = busy_workspace(4).await;
  let mut laptop = link_device_with(&mut carol, backfilling(ALL)).await;
  server.cut_network_after(laptop.user_id(), 30, NetworkCut::BeforeRequest);
  assert!(laptop.connect_gateway().await.is_err());
  let cut = progress(&laptop);
  assert_eq!(cut.stage, FirstSyncStage::Newest);
  assert!(!laptop.is_older_history_due());

  server.set_offline(laptop.user_id(), false);
  laptop.connect_gateway().await.unwrap();

  let resumed = progress(&laptop);
  assert_eq!(resumed.stage, FirstSyncStage::Older);
  for channel in &channels {
    assert_eq!(stored(&laptop, *channel), PAGE as usize);
  }
}

#[tokio::test]
async fn a_failed_page_waits_and_the_read_goes_on_where_it_stood() {
  let (server, mut carol, channels) = busy_workspace(1).await;
  let time = Arc::new(ManualClock::new(real_now_ms()));
  let mut laptop = link_device_on_clock(&mut carol, backfilling(ALL), Box::new(time.clone())).await;
  laptop.connect_gateway().await.unwrap();
  while progress(&laptop).done == 0 && stored(&laptop, channels[0]) < 2 * PAGE as usize {
    laptop.load_older_history_page().await;
  }
  let before = stored(&laptop, channels[0]);

  server.set_offline(laptop.user_id(), true);
  laptop.load_older_history_page().await;

  assert!(
    !laptop.is_older_history_due(),
    "a failed page waits for its retry"
  );
  assert_eq!(progress(&laptop).stage, FirstSyncStage::Older);
  time.advance(31_000);
  assert!(laptop.is_older_history_due());

  server.set_offline(laptop.user_id(), false);
  let calls_before = server.rest_call_count(laptop.user_id());
  laptop.load_older_history_page().await;

  assert_eq!(server.rest_call_count(laptop.user_id()), calls_before + 1);
  assert!(stored(&laptop, channels[0]) > before);
}

#[tokio::test]
async fn a_killed_app_goes_on_where_it_stood_and_reads_no_page_twice() {
  let (server, mut carol, channels) = busy_workspace(1).await;
  let mut laptop = link_device_with(&mut carol, backfilling(ALL)).await;
  laptop.connect_gateway().await.unwrap();
  laptop.load_older_history_page().await;
  let held = stored(&laptop, channels[0]);
  let stage = progress(&laptop).stage;
  assert_eq!(stage, FirstSyncStage::Older);

  // Killed: the same store opens in a new client.
  let store = laptop.store().clone();
  let api = laptop.api().clone();
  drop(laptop);
  let mut reopened = Client::open(api, store, clock(), backfilling(ALL))
    .unwrap()
    .unwrap();

  assert!(reopened.is_older_history_due());
  assert_eq!(stored(&reopened, channels[0]), held);
  let mut pages = 0;
  while reopened.is_older_history_due() {
    let calls_before = server.rest_call_count(reopened.user_id());
    reopened.load_older_history_page().await;
    assert!(server.rest_call_count(reopened.user_id()) <= calls_before + 2);
    pages += 1;
    assert!(pages < 20);
  }
  assert_eq!(stored(&reopened, channels[0]), MESSAGES_PER_CHANNEL);
  // Pages left: what was missing, not the whole channel again (25 messages, 10 a page, 20 held).
  assert!(pages <= 3, "read {pages} pages");
}

#[tokio::test]
async fn a_later_start_reads_no_message_again() {
  let (server, mut carol, channels) = busy_workspace(2).await;
  let mut laptop = link_device_with(&mut carol, backfilling(ALL)).await;
  laptop.connect_gateway().await.unwrap();
  while laptop.is_older_history_due() {
    laptop.load_older_history_page().await;
  }
  let store = laptop.store().clone();
  let api = laptop.api().clone();
  drop(laptop);

  let mut reopened = Client::open(api, store, clock(), backfilling(ALL))
    .unwrap()
    .unwrap();
  let reads_before = server.message_list_count(reopened.user_id());
  reopened.connect_gateway().await.unwrap();

  assert_eq!(server.message_list_count(reopened.user_id()), reads_before);
  assert!(!reopened.is_older_history_due());
  assert_eq!(first_sync_progress(reopened.store()).unwrap(), None);
  assert_eq!(stored(&reopened, channels[0]), MESSAGES_PER_CHANNEL);
}

#[tokio::test]
async fn it_keeps_the_window_and_no_more() {
  let (_server, mut carol, channels) = busy_workspace(1).await;
  // Every message lies before the window when the device's clock is a year ahead.
  let ahead = Box::new(ManualClock::new(real_now_ms() + 365 * 24 * 60 * 60 * 1000));
  let mut laptop = link_device_on_clock(&mut carol, backfilling(HistoryBackfill::KEEP_RECENT), ahead).await;
  laptop.connect_gateway().await.unwrap();

  while laptop.is_older_history_due() {
    laptop.load_older_history_page().await;
  }

  assert_eq!(stored(&laptop, channels[0]), PAGE as usize);
  assert_eq!(first_sync_progress(laptop.store()).unwrap(), None);
}

#[tokio::test]
async fn a_busy_conversation_stops_at_its_limit_and_the_budget_ends_it_all() {
  let (_server, mut carol, channels) = busy_workspace(1).await;
  let limited = HistoryBackfill {
    max_per_conversation: 5,
    ..ALL
  };
  let mut laptop = link_device_with(&mut carol, backfilling(limited)).await;
  laptop.connect_gateway().await.unwrap();
  while laptop.is_older_history_due() {
    laptop.load_older_history_page().await;
  }
  // The newest page, and one page before the limit was reached.
  assert_eq!(stored(&laptop, channels[0]), 2 * PAGE as usize);

  let (_server, mut carol, channels) = busy_workspace(3).await;
  let budget = HistoryBackfill {
    max_messages: 5,
    ..ALL
  };
  let mut laptop = link_device_with(&mut carol, backfilling(budget)).await;
  laptop.connect_gateway().await.unwrap();
  while laptop.is_older_history_due() {
    laptop.load_older_history_page().await;
  }
  assert_eq!(first_sync_progress(laptop.store()).unwrap(), None);
  let total: usize = channels.iter().map(|channel| stored(&laptop, *channel)).sum();
  assert_eq!(total, 3 * PAGE as usize + PAGE as usize);
}

#[tokio::test]
async fn the_conversation_the_user_looks_at_goes_first() {
  let (_server, mut carol, channels) = busy_workspace(3).await;
  let mut laptop = link_device_with(&mut carol, backfilling(ALL)).await;
  laptop.connect_gateway().await.unwrap();
  let looked_at = *channels.first().unwrap();
  laptop.catch_up_focus().set(looked_at);

  laptop.load_older_history_page().await;

  assert_eq!(stored(&laptop, looked_at), 2 * PAGE as usize);
}

#[tokio::test]
async fn without_a_backfill_the_bar_ends_with_the_newest_pages() {
  let (_server, mut carol, channels) = busy_workspace(2).await;
  let mut laptop = link_device_with(&mut carol, page_settings()).await;

  laptop.connect_gateway().await.unwrap();

  assert_eq!(first_sync_progress(laptop.store()).unwrap(), None);
  assert!(!laptop.is_older_history_due());
  assert_eq!(stored(&laptop, channels[0]), PAGE as usize);
}

#[tokio::test]
async fn a_device_that_holds_history_never_starts_it() {
  let (_server, carol, channels) = busy_workspace(1).await;
  // Carol's own device read its history under a version without this (`settings()` has no backfill)
  // and is updated: its record says finished after its next sync, with a backfill on from now on.
  let store = carol.store.clone();
  let api = carol.api.clone();
  let mut updated = Client::open(api, store, clock(), backfilling(ALL))
    .unwrap()
    .unwrap();
  updated.full_sync().await.unwrap();
  assert_eq!(first_sync_progress(updated.store()).unwrap(), None);
  assert!(!updated.is_older_history_due());
  assert!(stored(&updated, channels[0]) > 0);
}

#[tokio::test]
async fn sending_goes_on_between_pages() {
  let (_server, mut carol, channels) = busy_workspace(1).await;
  let mut laptop = link_device_with(&mut carol, backfilling(ALL)).await;
  laptop.connect_gateway().await.unwrap();
  laptop.load_older_history_page().await;

  laptop.send_text(channels[0], None, "still works").await.unwrap();
  laptop.load_older_history_page().await;

  assert!(
    laptop
      .store()
      .messages(channels[0], MessageFilter::TopLevel)
      .unwrap()
      .iter()
      .any(|message| matches!(&message.content, MessageContent::Text { body, .. } if body == "still works"))
  );
}

/// The whole path of an app: the UI hears of the progress during the connection, older history
/// comes without a timer, and a stopped runner leaves the rest for the next start.
#[tokio::test]
async fn the_runner_tells_the_ui_and_reads_older_history_until_it_is_done() {
  let (server, mut carol, channels) = busy_workspace(2).await;
  let laptop = link_device_with(&mut carol, backfilling(ALL)).await;
  let changes = Rc::new(Cell::new(0_u32));
  let notified = changes.clone();
  let store = laptop.store().clone();
  let (runner, handle) = SyncRunner::new(
    laptop,
    RunnerSettings::default(),
    Box::new(move || notified.set(notified.get() + 1)),
  );
  let watcher = async {
    while first_sync_progress(&store).unwrap().is_none() || server.gateway_connect_count() == 0 {
      tokio::task::yield_now().await;
    }
    while first_sync_progress(&store).unwrap().is_some() {
      tokio::task::yield_now().await;
    }
    assert!(handle.send(RunnerCommand::Stop));
  };

  let (client, ()) = tokio::join!(runner.run(), watcher);

  for channel in &channels {
    assert_eq!(stored(&client, *channel), MESSAGES_PER_CHANNEL);
  }
  // The list, each conversation in both stages and the end, not once per page.
  assert!(changes.get() >= 4, "told {} times", changes.get());
}

/// Alice's channels with Carol in them and [`MESSAGES_PER_CHANNEL`] messages in each.
async fn busy_workspace(channels: usize) -> (FakeServer, Peer, Vec<ConversationId>) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let space = alice.space().await;
  let mut ids = Vec::new();
  for index in 0..channels {
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
    for message in 0..MESSAGES_PER_CHANNEL {
      alice
        .client
        .send_text(channel, None, &format!("message {message}"))
        .await
        .unwrap();
    }
    ids.push(channel);
  }
  settle(&mut [&mut alice, &mut carol]).await;
  (server, carol, ids)
}

fn page_settings() -> ClientSettings {
  ClientSettings {
    history_page_size: PAGE,
    ..settings()
  }
}

fn backfilling(history_backfill: HistoryBackfill) -> ClientSettings {
  ClientSettings {
    history_backfill,
    ..page_settings()
  }
}

fn progress(client: &TestClient) -> teamcraft_core::client::FirstSyncProgress {
  first_sync_progress(client.store()).unwrap().unwrap()
}

fn stored(client: &TestClient, conversation: ConversationId) -> usize {
  client
    .store()
    .messages(conversation, MessageFilter::TopLevel)
    .unwrap()
    .iter()
    .filter(|message| matches!(message.content, MessageContent::Text { .. }))
    .count()
}

fn real_now_ms() -> i64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0, |elapsed| {
      i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
    })
}
