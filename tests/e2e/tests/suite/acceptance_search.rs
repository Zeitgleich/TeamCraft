//! v1 acceptance, PLAN.md §7 Search: full-text search over what a device decrypted (stemming,
//! filters). The server never sees the text; people search is in acceptance_people.
use teamcraft_core::search::{SearchQuery, SearchService};
use teamcraft_core::store::LocalStore;
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::{Id, ThreadId};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn search_finds_decrypted_messages_with_stemming_and_filters() {
  let Some(stack) = Stack::start().await else { return };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let space = space_with(&stack, &alice, "Project A", &[(&carol, "member")]).await;
  let channel = channel_with(&mut alice, space, "budget", &mut [&mut carol]).await;
  let other = channel_with(&mut alice, space, "lunch", &mut [&mut carol]).await;

  let root = alice
    .client
    .send_text(channel, None, "Budget planning starts Monday")
    .await
    .unwrap();
  let root_server = alice
    .store
    .message(channel, root)
    .unwrap()
    .and_then(|m| m.maybe_server_id)
    .unwrap();
  let thread = ThreadId::from_snowflake(root_server.snowflake());
  alice
    .client
    .send_text(other, None, "Lunch at noon")
    .await
    .unwrap();
  carol
    .wait_for_text(channel, "Budget planning starts Monday")
    .await;
  carol.wait_for_text(other, "Lunch at noon").await;
  carol
    .client
    .send_text(channel, None, "see https://docs.example.com/budget for the plans")
    .await
    .unwrap();
  carol
    .client
    .send_text(channel, Some(thread), "planned budgets are attached")
    .await
    .unwrap();
  alice
    .wait_for_text(channel, "see https://docs.example.com/budget for the plans")
    .await;
  alice
    .wait_until("the thread reply arrives", |peer| {
      !peer.thread_texts(channel, thread).is_empty()
    })
    .await;

  let mut service = SearchService::new();
  let mut search = |query: SearchQuery| service.search(alice.store.as_ref(), &query).unwrap().hits.len();
  let terms = |words: &[&str]| words.iter().map(|word| (*word).to_owned()).collect::<Vec<_>>();
  // Word forms (plan/planning/planned) and terms that must all match.
  assert_eq!(
    search(SearchQuery {
      terms: terms(&["planning"]),
      ..SearchQuery::default()
    }),
    3
  );
  assert_eq!(
    search(SearchQuery {
      terms: terms(&["budget", "monday"]),
      ..SearchQuery::default()
    }),
    1
  );
  assert_eq!(
    search(SearchQuery {
      terms: terms(&["lunch"]),
      ..SearchQuery::default()
    }),
    1
  );
  // Filters: conversation, sender, link, thread.
  assert_eq!(
    search(SearchQuery {
      terms: terms(&["budget"]),
      maybe_conversation: Some(other),
      ..SearchQuery::default()
    }),
    0
  );
  assert_eq!(
    search(SearchQuery {
      terms: terms(&["budget"]),
      maybe_sender: Some(carol.user_id()),
      ..SearchQuery::default()
    }),
    2
  );
  assert_eq!(
    search(SearchQuery {
      terms: terms(&["budget"]),
      has_link: true,
      ..SearchQuery::default()
    }),
    1
  );
  assert_eq!(
    search(SearchQuery {
      terms: terms(&["budgets"]),
      in_thread: true,
      ..SearchQuery::default()
    }),
    1
  );
  assert_eq!(
    search(SearchQuery {
      terms: terms(&["nothing-like-this"]),
      ..SearchQuery::default()
    }),
    0
  );

  // The server stores no plaintext that search could use.
  assert!(!stack.server_log().to_lowercase().contains("budget planning"));
  stack.stop().await;
}
