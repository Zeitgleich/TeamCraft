//! Fixed wire-format vectors: serialized keys, signatures, the rendezvous transcript, HPKE seals and
//! the encrypted key bag.
//!
//! Captured from `main` before the crypto dependency majors were upgraded (issue #124), so a
//! dependency bump that changes a byte on the wire fails here. Deterministic outputs are compared
//! exactly; HPKE seals draw their own randomness, so captured seals must still open instead.
//! Never update these values to make a test pass: a mismatch is a wire format break.

#![allow(clippy::unwrap_used)]

use openmls_traits::signatures::Signer as _;
use rand_core::{CryptoRng, RngCore, impls};
use teamcraft_crypto::backup::{BackupKey, BackupKeyWrapper as _, WrappedBackupKey};
use teamcraft_crypto::identity::{DeviceKeyPair, IdentityKeyPair};
use teamcraft_crypto::ids::{DeviceId, UserId};
use teamcraft_crypto::key_bag::{EncryptedKeyBag, KeyBag, KeyBagBinding};
use teamcraft_crypto::org_recovery::{
  OrgRecoveryShare, OrgRecoveryWrapper, RecoveryRequest, RecoveryRequestKey, combine_shares,
  create_org_recovery,
};
use teamcraft_crypto::rendezvous::{Initiator, RendezvousChannel, RendezvousKey, Responder};
use teamcraft_crypto::secret::SymmetricKey;

const USER: UserId = UserId::new(1_001);
const DEVICE: DeviceId = DeviceId::new(2_002);
const PEER_DEVICE: DeviceId = DeviceId::new(3_003);
const IDENTITY_SECRET: [u8; 32] = [0x11; 32];
const SIGNING_SECRET: [u8; 32] = [0x22; 32];
const AGREEMENT_SECRET: [u8; 32] = [0x33; 32];
const PEER_SIGNING_SECRET: [u8; 32] = [0x44; 32];
const PEER_AGREEMENT_SECRET: [u8; 32] = [0x55; 32];
const RENDEZVOUS_KEY: [u8; 32] = [0x66; 32];
const BACKUP_KEY: [u8; 32] = [0x77; 32];
const MLS_PAYLOAD: &[u8] = b"mls handshake payload";

const IDENTITY_PUBLIC_KEY: &str = "d04ab232742bb4ab3a1368bd4615e4e6d0224ab71a016baf8520a332c9778737";
const IDENTITY_FINGERPRINT: &str = "fe25b99040110293c4a4ff083abd3666a5b91cb248577a450bc9e39fa44b991d";
const DEVICE_SIGNING_PUBLIC_KEY: &str = "a09aa5f47a6759802ff955f8dc2d2a14a5c99d23be97f864127ff9383455a4f0";
const DEVICE_AGREEMENT_PUBLIC_KEY: &str = "7b0d47d93427f8311160781c7c733fd89f88970aef490d8aa0ee19a4cb8a1b14";
const SIGNED_DEVICE_CERTIFICATE: &str = concat!(
  "0100000000000003e900000000000007d2d04ab232742bb4ab3a1368bd4615e4e6d0224ab71a016baf8520a332c97787",
  "37a09aa5f47a6759802ff955f8dc2d2a14a5c99d23be97f864127ff9383455a4f07b0d47d93427f8311160781c7c733f",
  "d89f88970aef490d8aa0ee19a4cb8a1b14c98cc2e9f66f01e5b686fc2036fc6319c31968bf769d8ce08d9026869df252",
  "c3eeb4c72e7e931d24ec4d294d2e038c956848fc0bf363ff16b253c9ca2642b907",
);
const MLS_SIGNATURE: &str = concat!(
  "eaf088bed31c28c7867ceccfd3204c05babc101483ffc71d399c92b451d953c2eee846e0a3df894afc201c08d90a9dd4",
  "37f71ba479fdaaaaf640cd2a3afdf401",
);
const DEVICE_AGREEMENT: &str = "7b5c0ae9cc1e35635f73a4819f8f818b623d2584052f2b22a60221498c3bf548";

const RENDEZVOUS_HELLO: &str = concat!(
  "bc92ad9cc99b108b38ab9ea523a4e4d82bd2c9c2ed86cb9d109004d602a837b301aa95ab85dc5cc33100ae72f3d19098",
  "4fce1a650cfa8a016ba7e4965e234e8d064cdb6f27b9cbb07d487daf",
);
const RENDEZVOUS_AUTH_HELLO: &str = concat!(
  "29b68575fddae9d95193cd3518a2717d54ac75364897b6fe25ce12373bf9c34d74f4a36f879d2aac5c2f5c12d63f74e5",
  "4b76306c6571b7d11dbee4e4477789090f1dc336a0f8133e3d76e19eb68bfe3f6840f428ed670cfb7a80ef2e",
);
const RENDEZVOUS_AUTH: &str = concat!(
  "3b99c3a1264f9c9be490b59c25623849d7ff0330252fc1b16dec100fcd29630d42ec44a767021301fb29508853db6a67",
  "0b400c1683bd557a6698f1ff0f931ec365a77300ae833a952a193539",
);
const RENDEZVOUS_SHORT_CODE: &str = "023443";
const RENDEZVOUS_FRAME_TO_INITIATOR: &str =
  "088869e3b479b4829b80d8ad88fc87d8a87c289d81b73777b2ac793f35c74b57406fae5ab7594687";
const RENDEZVOUS_FRAME_TO_RESPONDER: &str = "94275da37a9cb4b4a951103da4f62e8bd98b738cd90b";

const RECOVERY_REQUEST_PUBLIC_KEY: &str = "40e913b735c3f408e7c85ca9bfbb5af8196911ea249fe96d4e96abafcbaa1d38";
const ORG_RECOVERY_PUBLIC_KEY: &str = "af9661f66607a0325657ed122caa87837064f4fdf1b81bbf9a26ef5154fefb12";
const ORG_RECOVERY_SHARES: [&str; 3] = [
  "01f27d9d8c6e73a61df949a5c57ccd052c6022c1e0990b3969c44def22f6820b8f",
  "02b6008327a5b9bf982fb04e8cc84b4982b1b11ba7f3c9ff613f9bf75e79821b5b",
  "038a2b89b715ff411294e71742a4c08411fec0a49ad57ebd909f20ff83f582e217",
];
const SEALED_SHARE_FOR_DEVICE: &str = concat!(
  "6b60dc2c388716b55cbf53b39a92a6a62c29b66e446b0b68b3a18e4085cc4a2c64940fe1fe857bd9412e94df7e82dcfc",
  "234481884bd776b0ab8b826230f4b738938ae5352309cc7aeb70a1c8124ff464ad",
);
const SEALED_BACKUP_KEY_FOR_REQUEST: &str = concat!(
  "fd6f5577ea7c2804f954ac81345d81ddbaaf99b7cd5f166938ae89030bcd4c28f73f0c04889e7f6d1c509b8eac21cccc",
  "f6de54d642564ae9f6b75a0ca93368b7408a46b72b0e1fbe828b3f36ff993126",
);
const ORG_WRAPPED_BACKUP_KEY: &str = concat!(
  "010420af9661f66607a0325657ed122caa87837064f4fdf1b81bbf9a26ef5154fefb1200000000000000000000000000",
  "0000000000000000000000405081345b51ded9da899a46fe6d45a5c4b1fa4a3b19809202cec882c437f17ef86d9c8d28",
  "8d6a789d19a8b38b155ce849847b8531f154b05616543a2d9c6c27d5171ed9529fe52ccc53338520ffa8244398",
);

const ENCRYPTED_KEY_BAG: &str = concat!(
  "020000000000000007ed8f01dbe4140b1d89a9817b8a6f46b301dd50d0a6e8eb9c40473b25f6b093e1e314d0694a9de9",
  "d10bf188a5a5ce101035870b6ee876ae987b93621f35a461e39a27a2edb2b8f8de7bedc039fc488cf2bdb9a20a62c551",
  "6d11ba3e42123b5ee7a0",
);

mod identity {
  use super::*;

  #[test]
  fn public_keys_and_fingerprint_are_stable() {
    let identity = identity();
    assert_eq!(hex(identity.public_key().as_bytes()), IDENTITY_PUBLIC_KEY);
    assert_eq!(
      identity.public_key().fingerprint().to_string(),
      IDENTITY_FINGERPRINT
    );
    let keys = device().public_keys();
    assert_eq!(hex(keys.signing_key.as_bytes()), DEVICE_SIGNING_PUBLIC_KEY);
    assert_eq!(hex(keys.agreement_key.as_bytes()), DEVICE_AGREEMENT_PUBLIC_KEY);
  }

  #[test]
  fn device_certificate_signature_is_stable() {
    let signed = identity().certify_device(USER, &device().public_keys()).unwrap();
    assert_eq!(hex(&signed.to_bytes().unwrap()), SIGNED_DEVICE_CERTIFICATE);
  }

  #[test]
  fn captured_device_certificate_still_verifies() {
    let signed =
      teamcraft_crypto::identity::SignedDeviceCertificate::from_bytes(&unhex(SIGNED_DEVICE_CERTIFICATE))
        .unwrap();
    let verified = signed.verify(&identity().public_key()).unwrap();
    assert_eq!(verified.device_id(), DEVICE);
  }

  #[test]
  fn mls_signature_is_stable() {
    assert_eq!(hex(&device().sign(MLS_PAYLOAD).unwrap()), MLS_SIGNATURE);
  }

  #[test]
  fn device_agreement_is_stable_and_symmetric() {
    let device = device();
    let peer = peer_device();
    let ours = device.agree(&peer.public_keys().agreement_key);
    let theirs = peer.agree(&device.public_keys().agreement_key);
    assert_eq!(hex(ours.expose_secret()), DEVICE_AGREEMENT);
    assert_eq!(ours.expose_secret(), theirs.expose_secret());
  }
}

mod rendezvous {
  use super::*;

  #[test]
  fn handshake_transcript_is_stable() {
    let transcript = run_handshake();
    assert_eq!(hex(&transcript.hello), RENDEZVOUS_HELLO);
    assert_eq!(hex(&transcript.auth_hello), RENDEZVOUS_AUTH_HELLO);
    assert_eq!(hex(&transcript.auth), RENDEZVOUS_AUTH);
    assert_eq!(transcript.initiator.short_code(), RENDEZVOUS_SHORT_CODE);
    assert_eq!(transcript.responder.short_code(), RENDEZVOUS_SHORT_CODE);
  }

  #[test]
  fn channel_frames_are_stable() {
    let mut transcript = run_handshake();
    let to_initiator = transcript.responder.seal(b"identity key and key bag").unwrap();
    let to_responder = transcript.initiator.seal(b"thanks").unwrap();
    assert_eq!(hex(&to_initiator), RENDEZVOUS_FRAME_TO_INITIATOR);
    assert_eq!(hex(&to_responder), RENDEZVOUS_FRAME_TO_RESPONDER);
    assert_eq!(
      transcript.initiator.open(&to_initiator).unwrap().expose_secret(),
      b"identity key and key bag"
    );
  }
}

mod org_recovery {
  use super::*;

  #[test]
  fn request_key_is_stable() {
    let key = RecoveryRequestKey::generate(&mut TestRng::new(1));
    assert_eq!(hex(key.public_key().as_bytes()), RECOVERY_REQUEST_PUBLIC_KEY);
  }

  #[test]
  fn org_key_and_shares_are_stable() {
    let setup = create_org_recovery(2, 3, &mut TestRng::new(2)).unwrap();
    assert_eq!(hex(setup.public_key.as_bytes()), ORG_RECOVERY_PUBLIC_KEY);
    let shares: Vec<String> = setup
      .shares
      .iter()
      .map(|share| hex(share.expose_secret()))
      .collect();
    assert_eq!(shares, ORG_RECOVERY_SHARES);
  }

  #[test]
  fn captured_share_seal_still_opens() {
    let opened = OrgRecoveryShare::open_on_device(&device(), &unhex(SEALED_SHARE_FOR_DEVICE)).unwrap();
    assert_eq!(hex(opened.expose_secret()), ORG_RECOVERY_SHARES[0]);
  }

  #[test]
  fn captured_backup_key_seal_still_opens() {
    let key = RecoveryRequestKey::generate(&mut TestRng::new(1));
    let opened = key
      .open_backup_key(&request(&key), &unhex(SEALED_BACKUP_KEY_FOR_REQUEST))
      .unwrap();
    assert_eq!(opened.into_key().expose_secret(), &BACKUP_KEY);
  }

  #[test]
  fn captured_org_wrap_still_unwraps() {
    let wrapped = WrappedBackupKey::from_bytes(&unhex(ORG_WRAPPED_BACKUP_KEY)).unwrap();
    let unwrapped = org_unwrapper().unwrap_key(USER, &wrapped).unwrap();
    assert_eq!(unwrapped.into_key().expose_secret(), &BACKUP_KEY);
  }

  #[test]
  fn fresh_seals_round_trip() {
    let share = OrgRecoveryShare::from_bytes(unhex(ORG_RECOVERY_SHARES[0]));
    let sealed = share
      .seal_for_device(USER, &device().public_keys().agreement_key)
      .unwrap();
    let opened = OrgRecoveryShare::open_on_device(&device(), &sealed).unwrap();
    assert_eq!(opened.expose_secret(), share.expose_secret());

    let key = RecoveryRequestKey::generate(&mut TestRng::new(1));
    let pending = request(&key);
    let code = pending.verification_code();
    let sealed = pending
      .confirm(&code)
      .unwrap()
      .seal_backup_key(&backup_key())
      .unwrap();
    let opened = key.open_backup_key(&request(&key), &sealed).unwrap();
    assert_eq!(opened.into_key().expose_secret(), &BACKUP_KEY);
  }

  /// The combining admin's wrapper, from two of the fixed shares.
  pub(super) fn org_unwrapper() -> OrgRecoveryWrapper {
    let shares: Vec<OrgRecoveryShare> = ORG_RECOVERY_SHARES[..2]
      .iter()
      .map(|share| OrgRecoveryShare::from_bytes(unhex(share)))
      .collect();
    OrgRecoveryWrapper::for_unwrapping(combine_shares(&shares).unwrap()).unwrap()
  }

  pub(super) fn request(key: &RecoveryRequestKey) -> RecoveryRequest {
    RecoveryRequest::for_server_id(42, USER, key.public_key())
  }
}

mod key_bag {
  use super::*;

  #[test]
  fn encrypted_key_bag_is_stable() {
    let encrypted = key_bag()
      .encrypt(&backup_key(), binding(), &mut TestRng::new(3))
      .unwrap();
    assert_eq!(hex(&encrypted.to_bytes().unwrap()), ENCRYPTED_KEY_BAG);
  }

  #[test]
  fn captured_key_bag_still_decrypts() {
    let encrypted = EncryptedKeyBag::from_bytes(&unhex(ENCRYPTED_KEY_BAG)).unwrap();
    let decrypted = encrypted.decrypt(USER, &backup_key(), b"wrapped keys").unwrap();
    assert_eq!(decrypted.identity().public_key(), identity().public_key());
    assert_eq!(decrypted.account_data(), b"account data");
  }

  fn key_bag() -> KeyBag {
    let mut key_bag = KeyBag::new(USER, identity());
    key_bag.set_account_data(b"account data".to_vec());
    key_bag
  }

  fn binding() -> KeyBagBinding<'static> {
    KeyBagBinding {
      sequence: 7,
      wrapped_keys: b"wrapped keys",
    }
  }
}

/// Output of the three handshake messages, all drawn from one seeded RNG in a fixed order.
struct Transcript {
  hello: Vec<u8>,
  auth_hello: Vec<u8>,
  auth: Vec<u8>,
  initiator: RendezvousChannel,
  responder: RendezvousChannel,
}

fn run_handshake() -> Transcript {
  let key = RendezvousKey::from_bytes(RENDEZVOUS_KEY);
  let mut rng = TestRng::new(4);
  let initiator = Initiator::new(&key, &mut rng);
  let (responder, hello) = Responder::new(&key, &mut rng).unwrap();
  let (initiator, auth_hello) = initiator.receive_hello(&hello, &mut rng).unwrap();
  let (responder, auth) = responder.receive_auth_hello(&auth_hello, &mut rng).unwrap();
  let initiator = initiator.receive_auth(&auth).unwrap();
  Transcript {
    hello,
    auth_hello,
    auth,
    initiator,
    responder,
  }
}

fn identity() -> IdentityKeyPair {
  IdentityKeyPair::from_secret(&SymmetricKey::from_bytes(IDENTITY_SECRET))
}

fn device() -> DeviceKeyPair {
  DeviceKeyPair::from_secrets(
    USER,
    DEVICE,
    &SymmetricKey::from_bytes(SIGNING_SECRET),
    &SymmetricKey::from_bytes(AGREEMENT_SECRET),
  )
}

fn peer_device() -> DeviceKeyPair {
  DeviceKeyPair::from_secrets(
    USER,
    PEER_DEVICE,
    &SymmetricKey::from_bytes(PEER_SIGNING_SECRET),
    &SymmetricKey::from_bytes(PEER_AGREEMENT_SECRET),
  )
}

fn backup_key() -> BackupKey {
  BackupKey::from_key(SymmetricKey::from_bytes(BACKUP_KEY))
}

fn hex(bytes: &[u8]) -> String {
  bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
  (0..text.len())
    .step_by(2)
    .map(|index| u8::from_str_radix(&text[index..index + 2], 16).unwrap())
    .collect()
}

/// A seeded, reproducible stand-in for the OS RNG (SplitMix64). Test-only: not a CSPRNG.
struct TestRng(u64);

impl TestRng {
  fn new(seed: u64) -> Self {
    Self(seed)
  }
}

impl RngCore for TestRng {
  fn next_u32(&mut self) -> u32 {
    // Truncation intended: the low half of the next 64-bit output.
    self.next_u64() as u32
  }

  fn next_u64(&mut self) -> u64 {
    self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = self.0;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
  }

  fn fill_bytes(&mut self, dest: &mut [u8]) {
    impls::fill_bytes_via_next(self, dest);
  }

  fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
    self.fill_bytes(dest);
    Ok(())
  }
}

// Test-only: lets the seeded RNG stand in where the API asks for a cryptographic RNG.
impl CryptoRng for TestRng {}
