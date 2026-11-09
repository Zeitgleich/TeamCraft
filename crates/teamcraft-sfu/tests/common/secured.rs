//! Waiting for a call's clients to share one DAVE epoch.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use teamcraft_media::call_engine::CallEngineEvent;
use teamcraft_media::call_native::NativeCall;

/// Waits until both clients were secured on the same DAVE epoch and checks that they show the same
/// privacy code for it; returns the events Bob had meanwhile.
///
/// Compared here, with both still in the call: a leave re-keys the group, so a code read from the
/// stats of a `leave` can belong to the epoch after the other side left.
pub async fn wait_for_same_epoch(alice: &mut NativeCall, bob: &mut NativeCall) -> Vec<CallEngineEvent> {
  let wait = async {
    let (mut maybe_alice, mut maybe_bob) = (None, None);
    let mut bob_events = Vec::new();
    loop {
      tokio::select! {
        Some(event) = alice.next_event() => {
          if let CallEngineEvent::Secured { epoch, privacy_code } = event {
            maybe_alice = Some((epoch, privacy_code));
          }
        },
        Some(event) = bob.next_event() => {
          if let CallEngineEvent::Secured { epoch, privacy_code } = &event {
            maybe_bob = Some((*epoch, privacy_code.clone()));
          }
          bob_events.push(event);
        },
      }
      if let (Some((alice_epoch, alice_code)), Some((bob_epoch, bob_code))) = (&maybe_alice, &maybe_bob)
        && alice_epoch == bob_epoch
      {
        assert_eq!(alice_code, bob_code, "epoch {alice_epoch}");
        return bob_events;
      }
    }
  };
  tokio::time::timeout(Duration::from_secs(15), wait)
    .await
    .expect("the two clients were not secured on the same epoch")
}
