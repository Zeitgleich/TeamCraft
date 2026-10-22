//! DAVE in a JavaScript host: group setup and a frame round trip on `wasm32-unknown-unknown`.
//!
//! Guards the vendored `davey` against `std::time::Instant`, which panics there (encryptor and
//! decryptor take timestamps on every frame). Runs in Node by default and in a browser with
//! `WASM_BINDGEN_USE_BROWSER=1`; see crates/vendor/davey/VENDORED.md for the commands.

#![cfg(all(target_arch = "wasm32", target_os = "unknown"))]

use teamcraft_media::dave::{
  DaveCallId, DaveExternalSender, DaveParticipant, DaveParticipantId, FrameCodec, MediaKind,
};
use wasm_bindgen_test::wasm_bindgen_test;

const CALL: DaveCallId = DaveCallId(7);
const ALICE: DaveParticipantId = DaveParticipantId(1);
const BOB: DaveParticipantId = DaveParticipantId(2);
const OPUS_FRAME: &[u8] = &[0x78, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

#[wasm_bindgen_test]
fn two_participants_exchange_frames() {
  let gateway = DaveExternalSender::new().unwrap();
  let mut alice = joined(&gateway, ALICE);
  let mut bob = joined(&gateway, BOB);

  let key_package = bob.create_key_package().unwrap();
  let epoch = alice.maybe_epoch().unwrap();
  let proposals = gateway.propose(CALL, epoch, &[(BOB, &key_package)], &[]).unwrap();
  let commit = alice
    .process_proposals(&proposals, &[ALICE, BOB])
    .unwrap()
    .unwrap();
  alice.process_commit(&commit.commit).unwrap();
  bob.process_welcome(&commit.maybe_welcome.unwrap()).unwrap();

  for _ in 0..3 {
    let encrypted = alice
      .encrypt_frame(MediaKind::Audio, FrameCodec::Opus, OPUS_FRAME)
      .unwrap()
      .into_owned();
    assert_eq!(
      bob.decrypt_frame(ALICE, MediaKind::Audio, &encrypted).unwrap(),
      OPUS_FRAME
    );
  }
  assert_eq!(
    alice.verification_code(BOB).unwrap(),
    bob.verification_code(ALICE).unwrap()
  );
}

fn joined(gateway: &DaveExternalSender, id: DaveParticipantId) -> DaveParticipant {
  let mut participant = DaveParticipant::new(id, CALL).unwrap();
  participant
    .set_external_sender(&gateway.package().unwrap())
    .unwrap();
  participant
}
