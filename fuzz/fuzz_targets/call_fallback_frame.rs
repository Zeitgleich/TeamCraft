//! Frames of a call's WebSocket fallback (ARCHITECTURE.md §6.6): they come from untrusted clients to
//! the SFU, and from an untrusted server or anything in between to the clients. Decoding must never
//! panic, what decodes must encode to something that decodes the same, and a signaling frame's body
//! goes through the same JSON both sides read it with.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft_proto::{CallClientMessage, CallServerMessage, FallbackFrame};

fuzz_target!(|data: &[u8]| {
  let Ok(frame) = FallbackFrame::decode(data) else {
    return;
  };
  let encoded = frame.encode();
  assert_eq!(FallbackFrame::decode(&encoded), Ok(frame));
  if let FallbackFrame::Signal(body) = frame {
    let _ = serde_json::from_slice::<CallClientMessage>(body);
    let _ = serde_json::from_slice::<CallServerMessage>(body);
  }
});
