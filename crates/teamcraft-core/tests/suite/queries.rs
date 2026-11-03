//! The reads a UI in another process (the desktop webview) asks for over IPC: client-api's
//! `ChatQuery` answered by the core's backend, through JSON both ways like the desktop shell.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, backend_of, run_commands, settle};
use teamcraft_client_api::actions::LinkedMessage;
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::list_load::{ListLoad, LoadFailure};
use teamcraft_client_api::remote::{ChatAction, ChatAnswer, ChatQuery};
use teamcraft_core::api::fake::FakeServer;

#[tokio::test]
async fn search_mentions_and_saved_searches_answer_over_ipc() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "budget")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  let (alice_ui, bob_ui) = (backend_of(&alice), backend_of(&bob));
  alice_ui.post_message(channel, None, "the budget review moved to Friday");
  alice_ui.post_message(channel, None, "@bob please check the budgets");
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;

  let answer = ask(&bob_ui, r#"{"kind":"search","query":"budget in:budget"}"#);
  let ChatAnswer::SearchResults { results } = answer else {
    panic!("unexpected answer {answer:?}");
  };
  assert_eq!(results.total, 2, "word forms match (budgets)");

  let ChatAnswer::SearchHits { hits } = ask(&bob_ui, r#"{"kind":"recent_mentions"}"#) else {
    panic!("no hits");
  };
  assert_eq!(hits.len(), 1);

  // Saving a search is an action; the engine stores it in the synced account data.
  let action: ChatAction =
    serde_json::from_str(r#"{"action":"save_search","name":"Budget","query":"budget"}"#).unwrap();
  assert!(action.apply_to(&bob_ui).is_none());
  run_commands(&mut bob, &bob_ui).await;
  let ChatAnswer::SavedSearches { searches } = ask(&bob_ui, r#"{"kind":"saved_searches"}"#) else {
    panic!("no saved searches");
  };
  assert_eq!(searches[0].name, "Budget");

  // A message link's preview, resolved from the asking device's own store.
  let budget = bob_ui
    .messages(channel, 0, 10)
    .into_iter()
    .find(|message| message.body.contains("Friday"))
    .unwrap();
  let link = bob_ui.permalink(channel, budget.id).unwrap();
  let answer = ask(
    &bob_ui,
    &format!(r#"{{"kind":"linked_message","link":"{link}"}}"#),
  );
  let ChatAnswer::LinkedMessage {
    maybe_linked: Some(LinkedMessage::Visible { message, .. }),
  } = answer
  else {
    panic!("unexpected answer {answer:?}");
  };
  assert_eq!(message.id, budget.id);

  // Unknown queries from a newer UI are answered as unsupported, not as errors.
  assert_eq!(ask(&bob_ui, r#"{"kind":"time_travel"}"#), ChatAnswer::Unsupported);
}

/// What the desktop shell does with a query from the webview.
#[tokio::test]
async fn a_list_status_and_its_retry_go_over_ipc() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let alice_ui = backend_of(&alice);
  let query = r#"{"kind":"list_load","list":{"name":"call_history"}}"#;
  assert_eq!(load(&alice_ui, query), ListLoad::Loading);

  // A failed read is answered as failed; retrying asks again and shows as loading until it ends.
  server.set_offline(alice.client.user_id(), true);
  alice_ui.call_history();
  run_commands(&mut alice, &alice_ui).await;
  let failed = ListLoad::Failed {
    failure: LoadFailure::Network,
  };
  assert_eq!(load(&alice_ui, query), failed);

  server.set_offline(alice.client.user_id(), false);
  let action: ChatAction =
    serde_json::from_str(r#"{"action":"retry_list","list":{"name":"call_history"}}"#).unwrap();
  action.apply_to(&alice_ui);
  assert_eq!(load(&alice_ui, query), ListLoad::Loading);
  run_commands(&mut alice, &alice_ui).await;
  assert_eq!(load(&alice_ui, query), ListLoad::Ready);
}

fn load(
  backend: &teamcraft_core::ui_backend::CoreChatBackend<std::sync::Arc<teamcraft_core::store::MemoryStore>>,
  json: &str,
) -> ListLoad {
  let answer = ask(backend, json);
  let ChatAnswer::ListLoad { load } = answer else {
    panic!("unexpected answer {answer:?}");
  };
  load
}

fn ask(
  backend: &teamcraft_core::ui_backend::CoreChatBackend<std::sync::Arc<teamcraft_core::store::MemoryStore>>,
  json: &str,
) -> ChatAnswer {
  let query: ChatQuery = serde_json::from_str(json).unwrap();
  let answer = query.answer(backend);
  serde_json::from_str(&serde_json::to_string(&answer).unwrap()).unwrap()
}
