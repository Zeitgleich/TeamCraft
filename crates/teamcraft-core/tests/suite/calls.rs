//! What calls take from the client: join tokens, call key bindings and verified device certificates
//! (e2ee.md §8).
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, settle};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_crypto::call_binding::{CallId, SignedCallKeyBinding};

const NOW_SECS: u64 = 1_800_000_000;
const DAVE_KEY: [u8; 65] = [4; 65];

#[tokio::test]
async fn dm_peers_verify_each_others_call_keys() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let dm = alice.client.open_dm(bob.client.user_id()).await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  let token = alice.client.call_join_token(dm).await.unwrap();
  assert!(!token.token.is_empty());

  let devices = bob.client.verified_call_devices(dm).await.unwrap();
  assert_eq!(devices.len(), 2, "both devices of the DM");
  let alice_device = devices
    .iter()
    .find(|device| device.device_id().get() == alice_device_id(&alice))
    .unwrap();

  let call = CallId::new(77);
  let signed = alice
    .client
    .call_key_signer()
    .sign_call_key_binding(call, &DAVE_KEY, NOW_SECS + 300)
    .unwrap();
  let decoded = SignedCallKeyBinding::from_bytes(&signed.to_bytes().unwrap()).unwrap();
  let key = decoded.verify(alice_device, call, NOW_SECS).unwrap();
  assert_eq!(key.dave_public_key(), DAVE_KEY);
  // Bob's certificate doesn't verify Alice's binding.
  let bob_device = devices
    .iter()
    .find(|device| device.device_id().get() != alice_device_id(&alice))
    .unwrap();
  assert!(decoded.verify(bob_device, call, NOW_SECS).is_err());
}

fn alice_device_id(alice: &Peer) -> u64 {
  u64::try_from(alice.client.device_id().to_string().parse::<i64>().unwrap()).unwrap()
}
