//! Shared encrypted search index segments (e2ee.md §10.1) through the fake server.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]
#![cfg(not(target_arch = "wasm32"))]

mod common;

use common::{Peer, settle};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::search::{SearchQuery, SearchService};
use teamcraft_core::store::LocalStore;

#[tokio::test]
async fn a_new_member_searches_history_through_published_segments() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "budget")
    .await
    .unwrap();
  alice
    .client
    .send_text(channel, None, "Budget planning starts Monday")
    .await
    .unwrap();
  alice
    .client
    .send_text(channel, None, "Lunch at noon")
    .await
    .unwrap();

  // The month is closed (the clock is a year later); Alice's client publishes its segment once.
  assert_eq!(alice.client.publish_index_segments().await.unwrap(), 1);
  assert_eq!(alice.client.publish_index_segments().await.unwrap(), 0);
  for blob in server.stored_blobs() {
    assert!(
      !blob
        .windows(6)
        .any(|window| window.eq_ignore_ascii_case(b"budget"))
    );
  }

  // Carol joins with full history but has no messages locally yet, only the announcement.
  alice
    .client
    .add_member(channel, carol.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut carol]).await;
  carol.client.load_older_messages(channel).await.unwrap();
  assert_eq!(carol.client.fetch_index_segments().await.unwrap(), 1);

  // A phone that keeps little history: the messages are gone locally, the segment stays.
  carol.store.delete_messages(channel).unwrap();

  let query = SearchQuery {
    terms: vec!["budgets".into()],
    ..SearchQuery::default()
  };
  let mut search = SearchService::new();
  let results = search.search(carol.store.as_ref(), &query).unwrap();
  // Found through the segment; shown only after the message itself was fetched and decrypted.
  assert!(results.hits.is_empty());
  assert_eq!(results.to_fetch.len(), 1);
  assert_eq!(
    carol.client.fetch_search_hits(&results.to_fetch).await.unwrap(),
    1
  );
  let results = search.search(carol.store.as_ref(), &query).unwrap();
  assert_eq!(results.hits.len(), 1);
  assert!(results.to_fetch.is_empty());
}
