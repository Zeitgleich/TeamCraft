//! Catching up after being away (issue #247): every conversation arrives, the marks the UI shows as
//! "catching up" are gone afterwards, also after a catch-up that failed half way.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use crate::common::{Peer, link_device, settings, settle};
use teamcraft_core::ClientSettings;
use teamcraft_core::api::fake::{FakeServer, NetworkCut};
use teamcraft_core::client::is_catching_up;
use teamcraft_core::store::{LocalStore, MessageContent, MessageFilter, Table};
use teamcraft_proto::{ClientMessageId, ConversationId, UserId};

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

/// Alice's channel with Carol, "before" in it, read by Carol; Carol reads pages of five and two of
/// them in order before the newest.
async fn busy_channel() -> (FakeServer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let capped = ClientSettings {
    history_page_size: 5,
    catch_up_message_pages: Some(2),
    ..settings()
  };
  let mut carol = Peer::enroll_with_settings(&server, "Carol", capped).await;
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
