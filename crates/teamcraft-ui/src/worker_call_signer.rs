//! The call a browser's core worker signs for (e2ee.md §8, "Where the device key is", #2397): the
//! page of the worker runs the call and asks for each DAVE key binding, the device key stays in the
//! worker. The worker signs only for the one call its page prepared last (`core_prepare_call`), and
//! only while the page hasn't said the call ended (`core_forget_call_signer`); a request for
//! another conversation or another call is refused.

use teamcraft_proto::{CallId, ConversationId};

/// The prepared call and its signer, if any.
pub(crate) struct WorkerCallSigner<S> {
  maybe_prepared: Option<Prepared<S>>,
}

struct Prepared<S> {
  conversation: ConversationId,
  call_id: CallId,
  signer: S,
}

/// Why the worker didn't sign.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SignRefused {
  #[error("no call was prepared, or it ended")]
  NotPrepared,
  #[error("the binding is for another conversation or call than the one prepared")]
  OtherCall,
}

impl<S> WorkerCallSigner<S> {
  pub(crate) const fn new() -> Self {
    Self { maybe_prepared: None }
  }

  /// The page prepared `call_id` of `conversation`: only it is signed for from now on.
  pub(crate) fn prepare(&mut self, conversation: ConversationId, call_id: CallId, signer: S) {
    self.maybe_prepared = Some(Prepared {
      conversation,
      call_id,
      signer,
    });
  }

  /// The call ended (or the page let go of it): nothing is signed until the next preparation.
  pub(crate) fn forget(&mut self) {
    self.maybe_prepared = None;
  }

  /// The signer for a binding of `call_id` in `conversation`, if that is the prepared call.
  pub(crate) fn signer_for(&self, conversation: ConversationId, call_id: CallId) -> Result<&S, SignRefused> {
    let prepared = self.maybe_prepared.as_ref().ok_or(SignRefused::NotPrepared)?;
    if prepared.conversation != conversation || prepared.call_id != call_id {
      return Err(SignRefused::OtherCall);
    }
    Ok(&prepared.signer)
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn signs_only_for_the_prepared_call() {
    let mut signer = WorkerCallSigner::new();
    assert_eq!(
      signer.signer_for(conversation(1), call(10)),
      Err(SignRefused::NotPrepared)
    );
    signer.prepare(conversation(1), call(10), "key");
    assert_eq!(signer.signer_for(conversation(1), call(10)), Ok(&"key"));
    assert_eq!(
      signer.signer_for(conversation(2), call(10)),
      Err(SignRefused::OtherCall)
    );
    assert_eq!(
      signer.signer_for(conversation(1), call(11)),
      Err(SignRefused::OtherCall)
    );
  }

  #[test]
  fn a_new_preparation_replaces_the_last_and_an_ended_call_signs_nothing() {
    let mut signer = WorkerCallSigner::new();
    signer.prepare(conversation(1), call(10), "key");
    signer.prepare(conversation(2), call(20), "key");
    assert_eq!(
      signer.signer_for(conversation(1), call(10)),
      Err(SignRefused::OtherCall)
    );
    assert_eq!(signer.signer_for(conversation(2), call(20)), Ok(&"key"));
    signer.forget();
    assert_eq!(
      signer.signer_for(conversation(2), call(20)),
      Err(SignRefused::NotPrepared)
    );
  }

  fn conversation(raw: i64) -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn call(raw: i64) -> CallId {
    CallId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
