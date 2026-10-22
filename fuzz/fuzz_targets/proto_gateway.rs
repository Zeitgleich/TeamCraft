//! Gateway frames in both directions, events, and the other wire enums: they arrive over a
//! WebSocket from untrusted peers. Unknown variants must be tolerated (forward compatibility),
//! never panic, and survive a round trip.
#![no_main]

use libfuzzer_sys::fuzz_target;
use serde::{Serialize, de::DeserializeOwned};
use teamcraft_proto::*;

fn check<T: DeserializeOwned + Serialize>(data: &[u8]) {
  if let Ok(value) = serde_json::from_slice::<T>(data) {
    let encoded = serde_json::to_vec(&value).expect("a decoded value encodes");
    serde_json::from_slice::<T>(&encoded).expect("what we encode decodes again");
  }
}

fuzz_target!(|data: &[u8]| {
  check::<GatewayClientMessage>(data);
  check::<GatewayServerMessage>(data);
  check::<GatewayEvent>(data);
  check::<RendezvousClientMessage>(data);
  check::<ErrorBody>(data);
  check::<MessageResponse>(data);
  check::<MlsMessageResponse>(data);
  check::<ConversationResponse>(data);
  check::<MeetingResponse>(data);
  check::<CallJoinClaims>(data);
  check::<PushWakeUp>(data);
});
