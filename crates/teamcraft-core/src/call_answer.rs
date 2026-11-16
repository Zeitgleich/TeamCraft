//! Whether answering a ring may join (docs/notifications.md, "Mobile push delivery").
//!
//! Joining a conversation's call starts one when none runs: the server hands out a join token
//! either way. That is right for the person who calls, and wrong for the person who answers. A ring
//! can outlive its call: the caller hung up, their app or connection dropped, or the device learnt
//! of the call late (a push that arrived after the call ended, a catch-up after a restart). Answering
//! such a ring started a call of its own that rang the caller back instead of reaching them. So an
//! answer first asks the server which call runs, and joins only the call that rang.

use teamcraft_proto::CallId;

/// What answering a ring does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingAnswer {
  /// The call that rang still runs: join it.
  Join,
  /// The call that rang is over (none runs, or another one does): tell the user, start nothing.
  Ended,
}

/// `maybe_running`: the call the server says runs in the conversation now. `maybe_rung`: the call the
/// ring named, when it named one (a push names it; the gateway's ring names the conversation only).
pub fn ring_answer(maybe_running: Option<CallId>, maybe_rung: Option<CallId>) -> RingAnswer {
  match (maybe_running, maybe_rung) {
    (None, _) => RingAnswer::Ended,
    (Some(running), Some(rung)) if running != rung => RingAnswer::Ended,
    (Some(_), _) => RingAnswer::Join,
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id as _, Snowflake};

  use super::*;

  #[test]
  fn the_running_call_that_rang_is_joined() {
    assert_eq!(ring_answer(Some(call(1)), Some(call(1))), RingAnswer::Join);
  }

  #[test]
  fn a_ring_without_a_call_id_joins_whatever_runs() {
    assert_eq!(ring_answer(Some(call(1)), None), RingAnswer::Join);
  }

  #[test]
  fn a_ring_whose_call_is_over_starts_nothing() {
    assert_eq!(ring_answer(None, Some(call(1))), RingAnswer::Ended);
    assert_eq!(ring_answer(None, None), RingAnswer::Ended);
  }

  /// The caller hung up and called again: the old ring must not join the new call by accident,
  /// its own ring comes.
  #[test]
  fn a_ring_of_an_earlier_call_does_not_join_a_later_one() {
    assert_eq!(ring_answer(Some(call(2)), Some(call(1))), RingAnswer::Ended);
  }

  fn call(raw: i64) -> CallId {
    CallId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
