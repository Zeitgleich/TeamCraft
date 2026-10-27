//! Reads a client core answers from its local store while a long sync holds the client (issue
//! #247): the UI shows what is stored so far instead of waiting minutes for the whole catch-up.

use teamcraft_client_api::backend::{ChatBackend, SearchBackend};
use teamcraft_client_api::model::{ConversationId, Message, ThreadId};
use teamcraft_client_api::remote::{ChatAnswer, ChatQuery, ChatSnapshot, MessageWindow, SecuritySnapshot};

/// A read of the local store.
#[derive(Clone, Debug, PartialEq)]
pub enum SyncRead {
  Snapshot,
  Messages {
    conversation: ConversationId,
    start: usize,
    len: usize,
  },
  Thread {
    conversation: ConversationId,
    thread: ThreadId,
  },
  Query(ChatQuery),
}

/// The answer to a [`SyncRead`], same variant names.
#[derive(Clone, Debug, PartialEq)]
pub enum SyncReadAnswer {
  Snapshot(Box<ChatSnapshot>),
  Messages(MessageWindow),
  Thread(Vec<Message>),
  Query(ChatAnswer),
}

/// What the store held when the sync started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoredWorkspace {
  /// Nothing yet (first start, or the browser deleted the data): the UI waits for the sync, so it
  /// shows "connecting" rather than an empty workspace.
  Empty,
  /// Spaces or conversations from an earlier session: shown while the sync catches up.
  Known,
}

impl StoredWorkspace {
  pub fn of(backend: &impl ChatBackend) -> Self {
    if backend.spaces().is_empty() && backend.direct_conversations().is_empty() {
      Self::Empty
    } else {
      Self::Known
    }
  }
}

/// Answers a read during a sync, or `None` when it has to wait for the sync to end: the workspace
/// was not known yet, or no security snapshot was taken yet (it needs the client).
pub fn answer_during_sync(
  read: SyncRead,
  backend: &(impl ChatBackend + SearchBackend),
  stored: StoredWorkspace,
  maybe_security: Option<&SecuritySnapshot>,
) -> Option<SyncReadAnswer> {
  if stored == StoredWorkspace::Empty {
    return None;
  }
  Some(match read {
    SyncRead::Snapshot => {
      SyncReadAnswer::Snapshot(Box::new(ChatSnapshot::read(backend, maybe_security?.clone())))
    },
    SyncRead::Messages {
      conversation,
      start,
      len,
    } => SyncReadAnswer::Messages(MessageWindow::read(backend, conversation, start, len)),
    SyncRead::Thread { conversation, thread } => SyncReadAnswer::Thread(backend.thread(conversation, thread)),
    SyncRead::Query(query) => SyncReadAnswer::Query(query.answer(backend)),
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::mock::MockBackend;

  const NOW: i64 = 1_792_540_800_000;

  #[test]
  fn a_known_workspace_is_read_from_the_store() {
    let backend = MockBackend::new(NOW);
    let stored = StoredWorkspace::of(&backend);
    assert_eq!(stored, StoredWorkspace::Known);
    let Some(SyncReadAnswer::Snapshot(snapshot)) =
      answer_during_sync(SyncRead::Snapshot, &backend, stored, Some(&security()))
    else {
      panic!("the snapshot was not answered");
    };
    assert_eq!(snapshot.spaces, backend.spaces());
    assert!(!snapshot.conversations.is_empty());
  }

  #[test]
  fn messages_and_queries_are_answered() {
    let backend = MockBackend::new(NOW);
    let conversation = backend.direct_conversations()[0].id;
    let messages = SyncRead::Messages {
      conversation,
      start: 0,
      len: 10,
    };
    assert_eq!(
      answer_during_sync(messages, &backend, StoredWorkspace::Known, None),
      Some(SyncReadAnswer::Messages(MessageWindow::read(
        &backend,
        conversation,
        0,
        10
      )))
    );
    let query = SyncRead::Query(ChatQuery::Draft {
      conversation,
      maybe_thread: None,
    });
    assert!(matches!(
      answer_during_sync(query, &backend, StoredWorkspace::Known, None),
      Some(SyncReadAnswer::Query(ChatAnswer::Draft { .. }))
    ));
  }

  #[test]
  fn an_empty_store_waits_for_the_sync() {
    let backend = MockBackend::empty_workspace(NOW);
    let stored = StoredWorkspace::of(&backend);
    assert_eq!(stored, StoredWorkspace::Empty);
    assert_eq!(
      answer_during_sync(SyncRead::Snapshot, &backend, stored, Some(&security())),
      None
    );
  }

  #[test]
  fn a_snapshot_without_a_security_part_waits() {
    let backend = MockBackend::new(NOW);
    assert_eq!(
      answer_during_sync(SyncRead::Snapshot, &backend, StoredWorkspace::Known, None),
      None
    );
  }

  fn security() -> SecuritySnapshot {
    SecuritySnapshot {
      my_fingerprint: "fingerprint".to_owned(),
      contacts: Vec::new(),
      maybe_push_content: None,
      maybe_owners: None,
      maybe_presence_policy: None,
    }
  }
}
