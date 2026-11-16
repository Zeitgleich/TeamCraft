//! Calls in the timeline (docs/notifications.md, "Calls in the timeline"): every call of a conversation
//! shows as a line at its start, saying whether it goes on and how long it took.
//!
//! The server keeps a record of each call (call history: who started it, when it started and
//! ended); it learns all of that anyway, since it hands out the call tokens. No member sends
//! anything for the line: each device derives it from that record, like the lines about changes
//! the server reports (issue #818, [`StoredMessage::maybe_server_change`]), so nobody's message
//! can make up or hide a call. The line comes from:
//!
//! - a gateway `call_start`: at once, as a call that goes on;
//! - a gateway `call_end`: the line ends at once, and the server's record is read again soon
//!   (the upkeep, [`Client::take_call_lines_stale`]) for when it ended;
//! - the call history, read once per gateway connection ([`Client::sync_call_lines`]): calls this
//!   device missed while offline, and their ends. A record without an end whose call isn't running
//!   any more (lost in a server restart) is shown as ended, without a duration.
//!
//! A line never goes back from ended to ongoing, so reads in any order converge.

use teamcraft_proto::{
  CallHistoryEntry, CallId, ClientMessageId, ConversationId, Id, ListCallHistoryQuery, MessageId, UnixMillis,
  UserId,
};

use super::Client;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::store::{
  CallLine, CallLineState, ChangeActor, ConversationEvent, LocalStore, MessageContent, ServerChange,
  StoredMessage, nobody,
};

/// The second half of a call line's ID (the first is the call's ID). Its top bit is set, so it
/// is never a user ID, and it differs from the tag of the lines about server changes.
const LINE_ID_TAG: u64 = 0x8000_0000_0000_0ca1;

/// Calls read per request of the history.
const HISTORY_PAGE: u32 = 50;

/// At most this many pages per read: a device new to the account backfills the latest
/// `HISTORY_PAGE * MAX_HISTORY_PAGES` calls; older calls show no line there.
const MAX_HISTORY_PAGES: usize = 4;

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// A call started in `conversation` (gateway `call_start`): its line, as a call that goes on.
  pub(crate) fn note_call_start(
    &self,
    conversation: ConversationId,
    call: CallId,
    started_by: UserId,
    started_at: UnixMillis,
  ) -> Result<(), CoreError> {
    let line = CallLine {
      call,
      maybe_started_by: Some(started_by),
      started_at_ms: started_at.0,
      state: CallLineState::Ongoing,
      is_missed: false,
    };
    put_call_line(&self.store, conversation, line)?;
    Ok(())
  }

  /// The call `call` in `conversation` ended (gateway `call_end`): its line ends now, and the
  /// server's record is read again soon for when it ended.
  pub(crate) fn note_call_end(
    &mut self,
    conversation: ConversationId,
    call: CallId,
  ) -> Result<(), CoreError> {
    self.are_call_lines_stale = true;
    end_call_line(&self.store, conversation, call)?;
    Ok(())
  }

  /// Whether a call ended since the last call: the upkeep reads the history again then.
  pub fn take_call_lines_stale(&mut self) -> bool {
    std::mem::take(&mut self.are_call_lines_stale)
  }

  /// The call history is to be read again (a read that failed).
  pub fn mark_call_lines_stale(&mut self) {
    self.are_call_lines_stale = true;
  }

  /// Brings the call lines up to the server's call history: its newest page, and older pages
  /// while they bring calls this device didn't know (at most [`MAX_HISTORY_PAGES`]). Returns how
  /// many lines changed.
  pub async fn sync_call_lines(&mut self) -> Result<usize, CoreError> {
    let mut changed = 0;
    let mut open: Vec<(ConversationId, CallId)> = Vec::new();
    let mut maybe_before = None;
    for _ in 0..MAX_HISTORY_PAGES {
      let page = self
        .api
        .list_call_history(&ListCallHistoryQuery {
          maybe_before,
          maybe_limit: Some(HISTORY_PAGE),
        })
        .await?;
      let mut page_changed = 0;
      for entry in &page.calls {
        if entry.maybe_ended_at.is_none() {
          open.push((entry.conversation_id, entry.call_id));
        }
        if put_call_line(&self.store, entry.conversation_id, line_of(entry))? {
          page_changed += 1;
        }
      }
      changed += page_changed;
      // A page with nothing new: what is older is here already.
      if !page.has_more || page_changed == 0 {
        break;
      }
      maybe_before = page.calls.last().map(|entry| entry.call_id);
    }
    changed += self.end_lost_calls(&open).await?;
    if changed > 0 {
      tracing::debug!(lines = changed, "call lines brought up to the call history");
    }
    Ok(changed)
  }

  /// Records without an end whose call isn't running any more: the server lost them (a restart)
  /// and will never record their end. A server without the list of running calls leaves them.
  async fn end_lost_calls(&self, open: &[(ConversationId, CallId)]) -> Result<usize, CoreError> {
    if open.is_empty() {
      return Ok(0);
    }
    let Some(running) = self.api.running_calls().await? else {
      return Ok(0);
    };
    let mut ended = 0;
    for (conversation, call) in open {
      if running.iter().any(|running| running.call_id == *call) {
        continue;
      }
      if end_call_line(&self.store, *conversation, *call)? {
        ended += 1;
      }
    }
    Ok(ended)
  }
}

/// The ID of `call`'s line: the same on every read, so a line is written once and then updated.
pub(crate) fn call_line_id(call: CallId) -> ClientMessageId {
  let mut bytes = [0u8; 16];
  bytes[..8].copy_from_slice(&call.snowflake().get().to_be_bytes());
  bytes[8..].copy_from_slice(&LINE_ID_TAG.to_be_bytes());
  ClientMessageId::from_bytes(bytes)
}

/// The line of a call as the server's history records it.
fn line_of(entry: &CallHistoryEntry) -> CallLine {
  CallLine {
    call: entry.call_id,
    maybe_started_by: entry.maybe_started_by,
    started_at_ms: entry.started_at.0,
    state: match entry.maybe_ended_at {
      Some(ended_at) => CallLineState::Ended {
        maybe_ended_at_ms: Some(ended_at.0),
      },
      None => CallLineState::Ongoing,
    },
    is_missed: entry.is_missed,
  }
}

/// Writes `line` into `conversation`, merged with the line held ([`merged`]). Returns whether
/// anything changed; an unchanged line costs no write.
fn put_call_line(
  store: &impl LocalStore,
  conversation: ConversationId,
  line: CallLine,
) -> Result<bool, CoreError> {
  let id = call_line_id(line.call);
  let maybe_held = store.message(conversation, id)?;
  let maybe_held_line = maybe_held.as_ref().and_then(StoredMessage::maybe_call_line);
  let line = match maybe_held_line {
    Some(held) => merged(held, line),
    None => line,
  };
  if maybe_held_line == Some(&line) {
    return Ok(false);
  }
  store.upsert_message(&stored_line(conversation, line))?;
  Ok(true)
}

/// Marks `call`'s line in `conversation` as ended, if there is one that goes on. Returns whether
/// it changed.
fn end_call_line(
  store: &impl LocalStore,
  conversation: ConversationId,
  call: CallId,
) -> Result<bool, CoreError> {
  let Some(held) = store.message(conversation, call_line_id(call))? else {
    return Ok(false);
  };
  let Some(line) = held.maybe_call_line() else {
    return Ok(false);
  };
  if line.state != CallLineState::Ongoing {
    return Ok(false);
  }
  let ended = CallLine {
    state: CallLineState::Ended {
      maybe_ended_at_ms: None,
    },
    ..line.clone()
  };
  store.upsert_message(&stored_line(conversation, ended))?;
  Ok(true)
}

/// `new` over `held`: a call that ended doesn't go on again, and a known end stays known. A missed
/// call stays missed; the starter stays named.
fn merged(held: &CallLine, new: CallLine) -> CallLine {
  let state = match (held.state, new.state) {
    (
      _,
      CallLineState::Ended {
        maybe_ended_at_ms: Some(at),
      },
    ) => CallLineState::Ended {
      maybe_ended_at_ms: Some(at),
    },
    (CallLineState::Ended { maybe_ended_at_ms }, _) => CallLineState::Ended { maybe_ended_at_ms },
    (CallLineState::Ongoing, state) => state,
  };
  CallLine {
    state,
    is_missed: held.is_missed || new.is_missed,
    maybe_started_by: new.maybe_started_by.or(held.maybe_started_by),
    ..new
  }
}

/// The stored row of a call line: at the call's place among the messages (call IDs are the
/// server's snowflakes, time-ordered like message IDs), sent by nobody (the call names its starter).
fn stored_line(conversation: ConversationId, line: CallLine) -> StoredMessage {
  let at = MessageId::from_snowflake(line.call.snowflake());
  StoredMessage {
    conversation_id: conversation,
    client_message_id: call_line_id(line.call),
    maybe_server_id: None,
    sender_user_id: nobody(),
    maybe_thread_id: None,
    sent_at_ms: line.started_at_ms,
    content: MessageContent::Event {
      event: ConversationEvent::Call(line),
    },
    is_edited: false,
    edits: Vec::new(),
    maybe_edit_id: None,
    is_pinned: false,
    maybe_verified_commit: None,
    maybe_server_change: Some(ServerChange {
      at,
      seen_after: at,
      seen_until: at,
      actor: ChangeActor::Nobody,
    }),
    maybe_unreadable: None,
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::Snowflake;

  use super::*;
  use crate::store::{MemoryStore, MessageFilter};

  const STARTED_MS: i64 = 1_800_000_000_000;

  #[test]
  fn a_line_is_written_once_and_sits_at_the_call() {
    let store = MemoryStore::default();
    assert!(put_call_line(&store, conversation(), ongoing(7)).unwrap());
    assert!(!put_call_line(&store, conversation(), ongoing(7)).unwrap());
    let rows = store.messages(conversation(), MessageFilter::All).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].line_position(), Some(id::<MessageId>(7)));
    assert!(rows[0].is_feed_row());
    assert_eq!(rows[0].maybe_call_line(), Some(&ongoing(7)));
  }

  #[test]
  fn an_ended_call_never_goes_on_again() {
    let store = MemoryStore::default();
    put_call_line(&store, conversation(), ended(7, Some(STARTED_MS + 60_000))).unwrap();
    assert!(!put_call_line(&store, conversation(), ongoing(7)).unwrap());
    assert_eq!(state_of(&store, 7), ended(7, Some(STARTED_MS + 60_000)).state);
  }

  #[test]
  fn an_end_without_time_takes_the_time_when_it_comes() {
    let store = MemoryStore::default();
    put_call_line(&store, conversation(), ongoing(7)).unwrap();
    assert!(end_call_line(&store, conversation(), id::<CallId>(7)).unwrap());
    assert_eq!(
      state_of(&store, 7),
      CallLineState::Ended {
        maybe_ended_at_ms: None
      }
    );
    // Ending it twice changes nothing.
    assert!(!end_call_line(&store, conversation(), id::<CallId>(7)).unwrap());
    put_call_line(&store, conversation(), ended(7, Some(STARTED_MS + 5_000))).unwrap();
    assert_eq!(
      state_of(&store, 7),
      CallLineState::Ended {
        maybe_ended_at_ms: Some(STARTED_MS + 5_000)
      }
    );
    // A later read without the end keeps it.
    put_call_line(&store, conversation(), ended(7, None)).unwrap();
    assert_eq!(
      state_of(&store, 7),
      CallLineState::Ended {
        maybe_ended_at_ms: Some(STARTED_MS + 5_000)
      }
    );
  }

  #[test]
  fn ending_a_call_without_a_line_writes_nothing() {
    let store = MemoryStore::default();
    assert!(!end_call_line(&store, conversation(), id::<CallId>(7)).unwrap());
    assert!(
      store
        .messages(conversation(), MessageFilter::All)
        .unwrap()
        .is_empty()
    );
  }

  #[test]
  fn a_member_cannot_send_a_call_line() {
    let mut sent = stored_line(conversation(), ongoing(7));
    sent.maybe_server_change = None;
    sent.maybe_server_id = Some(id::<MessageId>(8));
    assert!(sent.is_hidden());
    assert_eq!(sent.maybe_call_line(), None);
  }

  #[test]
  fn line_ids_differ_from_server_change_lines() {
    let line_id = call_line_id(id::<CallId>(7));
    assert_eq!(line_id, call_line_id(id::<CallId>(7)));
    assert_ne!(line_id, call_line_id(id::<CallId>(8)));
    assert!(
      line_id.as_bytes()[8] & 0x80 != 0,
      "no user ID has the top bit set"
    );
  }

  #[test]
  fn the_line_round_trips_through_json() {
    let line = stored_line(conversation(), ended(7, Some(STARTED_MS + 1)));
    let json = serde_json::to_value(&line.content).unwrap();
    assert_eq!(
      json,
      serde_json::json!({
        "type": "event",
        "event": {
          "event": "call",
          "call": "7",
          "started_by": "3",
          "started_at_ms": STARTED_MS,
          "state": { "state": "ended", "ended_at_ms": STARTED_MS + 1 },
        },
      })
    );
    let back: MessageContent = serde_json::from_value(json).unwrap();
    assert_eq!(back, line.content);
  }

  fn conversation() -> ConversationId {
    id::<ConversationId>(1)
  }

  fn ongoing(call: i64) -> CallLine {
    CallLine {
      call: id::<CallId>(call),
      maybe_started_by: Some(id::<UserId>(3)),
      started_at_ms: STARTED_MS,
      state: CallLineState::Ongoing,
      is_missed: false,
    }
  }

  fn ended(call: i64, maybe_ended_at_ms: Option<i64>) -> CallLine {
    CallLine {
      state: CallLineState::Ended { maybe_ended_at_ms },
      ..ongoing(call)
    }
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn state_of(store: &MemoryStore, call: i64) -> CallLineState {
    store
      .message(conversation(), call_line_id(id::<CallId>(call)))
      .unwrap()
      .unwrap()
      .maybe_call_line()
      .unwrap()
      .state
  }
}
