//! Whether a list is loading, ready or failed as the UI reads it (issue #297): the empty state only
//! ever follows an answer, cached rows stay, a failure is told and can be retried.
#![cfg(feature = "ui")]
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use crate::common;

use common::{Peer, backend_of, run_commands};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::list_load::{ListLoad, ListPhase, LoadFailure, LoadingList};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::store::{LocalStore, MemoryStore, Table};
use teamcraft_core::ui_backend::{CoreChatBackend, CoreCommand};

const FAILED: ListLoad = ListLoad::Failed {
  failure: LoadFailure::Network,
};

mod call_history {
  use super::*;

  #[tokio::test]
  async fn an_empty_history_shows_placeholders_first_and_the_empty_state_after_the_answer() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let ui = backend_of(&alice);
    let list = LoadingList::CallHistory;

    assert!(ui.call_history().is_empty());
    assert_eq!(phase(&ui, &list), ListPhase::Skeleton, "no empty flash");

    run_commands(&mut alice, &ui).await;
    assert!(ui.call_history().is_empty());
    assert_eq!(phase(&ui, &list), ListPhase::Empty);
  }

  #[tokio::test]
  async fn a_failed_read_is_told_and_a_retry_loads_again() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let ui = backend_of(&alice);
    let list = LoadingList::CallHistory;

    server.set_offline(alice.client.user_id(), true);
    ui.call_history();
    run_commands(&mut alice, &ui).await;
    assert_eq!(ui.list_load(&list), FAILED);

    // Reads while it stays failed may ask again now and then, but don't go back to placeholders.
    ui.call_history();
    assert_eq!(ui.list_load(&list), FAILED);

    server.set_offline(alice.client.user_id(), false);
    ui.retry_list(&list);
    assert_eq!(ui.list_load(&list), ListLoad::Loading);
    run_commands(&mut alice, &ui).await;
    assert_eq!(ui.list_load(&list), ListLoad::Ready);
  }

  #[tokio::test]
  async fn the_answer_kept_from_earlier_stays_when_a_refresh_fails() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let ui = backend_of(&alice);
    let list = LoadingList::CallHistory;
    ui.call_history();
    run_commands(&mut alice, &ui).await;

    server.set_offline(alice.client.user_id(), true);
    ui.retry_list(&list);
    assert_eq!(
      ui.list_load(&list),
      ListLoad::Ready,
      "no skeleton over what is cached"
    );
    run_commands(&mut alice, &ui).await;
    assert_eq!(ui.list_load(&list), ListLoad::Ready);
  }
}

mod directory {
  use super::*;

  #[tokio::test]
  async fn nobody_found_only_shows_once_the_server_answered_that_search() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let ui = backend_of(&alice);
    let list = LoadingList::directory(" NoSuchName ");

    assert!(ui.search_people("NoSuchName").is_empty());
    assert_eq!(phase(&ui, &list), ListPhase::Skeleton, "no empty flash");

    run_commands(&mut alice, &ui).await;
    assert!(ui.search_people("NoSuchName").is_empty());
    assert_eq!(phase(&ui, &list), ListPhase::Empty);
    assert_eq!(
      ui.list_load(&LoadingList::directory("another name")),
      ListLoad::Loading,
      "every search waits for its own answer"
    );
  }

  #[tokio::test]
  async fn a_failed_search_keeps_the_people_it_knows() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let ui = backend_of(&alice);
    let list = LoadingList::directory("");

    server.set_offline(alice.client.user_id(), true);
    let known = ui.search_people("");
    run_commands(&mut alice, &ui).await;
    assert_eq!(ui.list_load(&list), FAILED);
    assert!(!known.is_empty(), "the user knows themselves");
    assert_eq!(
      ListPhase::of(ui.list_load(&list), known.is_empty()),
      ListPhase::Rows
    );
  }
}

mod members {
  use super::*;

  #[tokio::test]
  async fn members_not_read_yet_are_asked_for_once_and_show_after_the_answer() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let conversation = alice
      .client
      .create_channel(alice.space().await, "team")
      .await
      .unwrap();
    let ui = backend_of(&alice);
    let list = LoadingList::Members { conversation };
    assert_eq!(phase(&ui, &list), ListPhase::Skeleton, "no empty flash");
    assert_eq!(ui.list_load(&list), ListLoad::Loading);
    assert_eq!(
      ui.drain_commands(),
      [CoreCommand::RefreshMembers { conversation }],
      "reads don't repeat the request"
    );

    ui.retry_list(&list);
    run_commands(&mut alice, &ui).await;
    assert_eq!(ui.list_load(&list), ListLoad::Ready);
    assert!(
      alice
        .store
        .get(Table::Members, &conversation.to_string())
        .unwrap()
        .is_some()
    );
  }
}

/// What a list that holds no rows shows.
fn phase(ui: &CoreChatBackend<Arc<MemoryStore>>, list: &LoadingList) -> ListPhase {
  ListPhase::of(ui.list_load(list), true)
}
