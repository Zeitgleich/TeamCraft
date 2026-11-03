//! Fixed wire-format vectors: serialized keys, signatures, the rendezvous transcript, HPKE seals and
//! the encrypted key bag, plus what existing devices and the server already hold: stored key seeds,
//! every backup wrap, the local stores, messages and attachments.
//!
//! Captured from `main` before the crypto dependency majors were upgraded (issue #124), so a
//! dependency bump that changes a byte on the wire fails here. Deterministic outputs are compared
//! exactly; HPKE seals draw their own randomness, so captured seals must still open instead.
//! Never update these values to make a test pass: a mismatch is a wire format break.

#![allow(clippy::unwrap_used)]

use openmls_traits::signatures::Signer as _;
use rand_core::{CryptoRng, RngCore, impls};
use teamcraft_crypto::attachment::{self, AttachmentKey};
use teamcraft_crypto::backup::{
  Argon2Settings, BackupKey, BackupKeyWrapper, PassphraseWrapper, RecoveryKeyWrapper, SymmetricKeyWrapper,
  WrappedBackupKey, decode_wrapped_key_list, encode_wrapped_key_list,
};
use teamcraft_crypto::content_key::{ContentKey, ContentKeyId, ContentKeyRing};
use teamcraft_crypto::identity::{DeviceKeyPair, IdentityKeyPair};
use teamcraft_crypto::ids::{ClientMessageId, ConversationId, DeviceId, UserId};
use teamcraft_crypto::key_bag::{EncryptedKeyBag, KeyBag, KeyBagBinding};
use teamcraft_crypto::local_store::LocalStoreKeys;
use teamcraft_crypto::message::{EncryptedMessage, MessageContext, decrypt_message, encrypt_message};
use teamcraft_crypto::org_recovery::{
  OrgRecoveryShare, OrgRecoveryWrapper, RecoveryRequest, RecoveryRequestKey, combine_shares,
  create_org_recovery,
};
use teamcraft_crypto::recovery_key::RecoveryKey;
use teamcraft_crypto::rendezvous::{Initiator, RendezvousChannel, RendezvousKey, Responder};
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use teamcraft_crypto::secret_file;

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

// Data an existing device or the server already holds, captured from `main` with the same
// seeded RNG: backup wraps, the local stores, messages and attachments.
const KEYSTORE_KEY: [u8; 32] = [0x88; 32];
const PASSKEY_PRF_OUTPUT: [u8; 32] = [0x99; 32];
const PASSPHRASE: &[u8] = b"correct horse battery staple";
const RECOVERY_KEY: &str = "000G-40R4-0M30-E209-185G-R38E-1W1J-JG7V";
const DATABASE_KEY: [u8; 32] = [0xaa; 32];
const SECRETS_FILE_KEY: [u8; 32] = [0xbb; 32];
const CONTENT_KEY: [u8; 32] = [0xcc; 32];
const CONTENT_KEY_ID: [u8; 16] = [0xdd; 16];
const CONVERSATION: ConversationId = ConversationId::new(4_004);
const STORED_PLAINTEXT: &[u8] = b"stored on an existing device";
/// Bytes of an attachment in the vector: one more than a minimum-size chunk, so it has two chunks.
const ATTACHMENT_LEN: usize = attachment::MIN_CHUNK_SIZE as usize + 1;

const WRAP_DEVICE_KEYSTORE: &str = concat!(
  "010100ca2f588abe12870846ce266e597cffbb3d4aec2a7a6e87213015b9e43ed715626532fba2ceaf080c587ccd5ad7",
  "e3d86c99d7236c7f9450823972e3007b35ceec282e2ba071be630f2b",
);
const WRAP_PASSKEY_PRF: &str = concat!(
  "0102009d3080237d64f550a1136b7ad25c2a436d129b6e30be56a330ac76cc3cf44be4a3cb3538141cbb1b877514abbb",
  "7417002bf669a3f365620ac25d5a161a7297ab9d9868954fa6be0dba",
);
const WRAP_PASSPHRASE: &str = concat!(
  "01031c000000400000000100000001038fde99fcf93f9457a4a89d26aa80f0ee767f2fd7d7173cc969e6900288a3e76e",
  "ffb8f2e78727da3096b58eaa76238ec2209ecbbab275824cd9a06902d359d16056b3ba878b6025172af821b6b0f3abc4",
  "c453531f8ff98f2f",
);
const WRAP_RECOVERY_KEY: &str = concat!(
  "01051101ff8aadf8b737cac471a036afdee32454c8f4257a25a202a27338ef2ec578a54a63f7f010972c0bd1302d6288",
  "01eccc9876bc084a78c4d087f40ba1c7e808c13b62e854a8aabddde0c798e0149d7ad368f59f517b9a701ad330",
);
const SECRETS_FILE: &str = concat!(
  "013ee6db3514d6a96a7ac2acf5f81941120a935f9c629dd2035d44e615b66ed10c8f116fc6bb6911708bf520c453169b",
  "1d9b58b83c6069e4b22af8068bf03a8a01e28052f1",
);
const LOCAL_STORE_LOOKUP_ID: &str = "2e53c405ed4bab082d9a4c45a1e3ac793c572f08fcb7b003d43b4174832cbbac";
const LOCAL_STORE_RECORD: &str = concat!(
  "010550f5ab07935b87501fa8bb7717b8c7334e76191cb27d8ffa69b4aadcf4704b8c06bddb8308f092000a3f86df72b8",
  "c3dfef2d3dcb13dd8045c9799dbb5401c4914a4208",
);
const MEDIA_CACHE_LOOKUP_ID: &str = "d35509cb651dff6a021d4ff669c698189333974a6ce50ddb0553336915c9b3da";
const MEDIA_CACHE_RECORD: &str = concat!(
  "01079277badc86e15d4b6485c9ca115a147446e8a4122d33d9f280f522f9d00aec2a084d79be1fc658f8388f7acbdf62",
  "8c7fa077dd2593ac139c43e2293340201cd64746cb",
);
const ENCRYPTED_MESSAGE: &str = concat!(
  "02dddddddddddddddddddddddddddddddd32f05509d0b3201118c05722246ad4b66268b0ac8322376e2c9a8172663718",
  "4b28e2b0f198705f73887a0c1a2cbe4e58f32dfb52ea076e50ef280fd259db3afb62386d790896b3f5767e417aa226c8",
  "531f88eaad31e35584ee2fbc20e005246b86fd008d5cafa52e644f8c979aa20c86a685adbea1291554ba2e72ab5e19d5",
  "c9e4905e1708",
);
const ATTACHMENT_SHA256: &str = "280c6d4ee04f7321f2be6730583dda2aa9fa6aab712310c472d9fad7b78f1f6a";

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

mod stored_keys {
  use super::*;

  /// Devices store these seeds as they are. The agreement seed is not a clamped scalar, so a
  /// dependency that returned the clamped form would change what is written back to the store.
  #[test]
  fn device_secrets_come_back_unchanged() {
    let (signing, agreement) = device().to_secrets();
    assert_eq!(signing.expose_secret(), &SIGNING_SECRET);
    assert_eq!(agreement.expose_secret(), &AGREEMENT_SECRET);
  }

  #[test]
  fn identity_secret_comes_back_unchanged() {
    assert_eq!(identity().to_secret().expose_secret(), &IDENTITY_SECRET);
  }
}

mod backup_wraps {
  use super::*;

  #[test]
  fn wraps_are_stable() {
    for (wrapper, seed, captured) in wrappers() {
      let wrapped = wrapper
        .wrap_key(USER, &backup_key(), &mut TestRng::new(seed))
        .unwrap();
      assert_eq!(
        hex(&wrapped.to_bytes().unwrap()),
        captured,
        "{:?}",
        wrapper.method()
      );
    }
  }

  #[test]
  fn captured_wraps_still_unwrap() {
    for (wrapper, _, captured) in wrappers() {
      let wrapped = WrappedBackupKey::from_bytes(&unhex(captured)).unwrap();
      let unwrapped = wrapper.unwrap_key(USER, &wrapped).unwrap();
      assert_eq!(unwrapped.into_key().expose_secret(), &BACKUP_KEY);
    }
  }

  #[test]
  fn captured_wrap_list_still_decodes() {
    let wraps: Vec<WrappedBackupKey> = wrappers()
      .iter()
      .map(|(_, _, captured)| WrappedBackupKey::from_bytes(&unhex(captured)).unwrap())
      .collect();
    let list = encode_wrapped_key_list(&wraps).unwrap();
    let mut expected = vec![4];
    for (_, _, captured) in wrappers() {
      let bytes = unhex(captured);
      expected.extend_from_slice(&u32::try_from(bytes.len()).unwrap().to_be_bytes());
      expected.extend_from_slice(&bytes);
    }
    assert_eq!(list, expected);
    assert_eq!(decode_wrapped_key_list(&list).unwrap(), wraps);
  }

  /// Every wrapper a device can have used, with the RNG seed and the wrap captured with it.
  fn wrappers() -> [(Box<dyn BackupKeyWrapper>, u64, &'static str); 4] {
    let settings = Argon2Settings {
      memory_kib: 64,
      iterations: 1,
      parallelism: 1,
    };
    [
      (
        Box::new(SymmetricKeyWrapper::device_keystore(SymmetricKey::from_bytes(
          KEYSTORE_KEY,
        ))),
        10,
        WRAP_DEVICE_KEYSTORE,
      ),
      (
        Box::new(SymmetricKeyWrapper::passkey_prf(&SecretBytes::new(PASSKEY_PRF_OUTPUT.to_vec())).unwrap()),
        11,
        WRAP_PASSKEY_PRF,
      ),
      (
        Box::new(PassphraseWrapper::new(SecretBytes::new(PASSPHRASE.to_vec()), settings).unwrap()),
        12,
        WRAP_PASSPHRASE,
      ),
      (
        Box::new(RecoveryKeyWrapper::new(
          &RecoveryKey::parse(RECOVERY_KEY).unwrap(),
        )),
        13,
        WRAP_RECOVERY_KEY,
      ),
    ]
  }
}

mod local_data {
  use super::*;

  #[test]
  fn secrets_file_is_stable_and_still_opens() {
    let key = SymmetricKey::from_bytes(SECRETS_FILE_KEY);
    let sealed = secret_file::seal(&key, STORED_PLAINTEXT, &mut TestRng::new(14)).unwrap();
    assert_eq!(hex(&sealed), SECRETS_FILE);
    let opened = secret_file::open(&key, &unhex(SECRETS_FILE)).unwrap();
    assert_eq!(opened.expose_secret(), STORED_PLAINTEXT);
  }

  #[test]
  fn local_store_record_is_stable_and_still_opens() {
    let keys = LocalStoreKeys::derive(&SymmetricKey::from_bytes(DATABASE_KEY));
    check_record(&keys, 15, LOCAL_STORE_LOOKUP_ID, LOCAL_STORE_RECORD);
  }

  #[test]
  fn media_cache_record_is_stable_and_still_opens() {
    let keys = LocalStoreKeys::derive_media_cache(&SymmetricKey::from_bytes(DATABASE_KEY));
    check_record(&keys, 16, MEDIA_CACHE_LOOKUP_ID, MEDIA_CACHE_RECORD);
  }

  fn check_record(keys: &LocalStoreKeys, seed: u64, captured_id: &str, captured: &str) {
    let id = keys.lookup_id("vectors", b"record");
    assert_eq!(hex(&id.0), captured_id);
    let sealed = keys.seal(&id, STORED_PLAINTEXT, &mut TestRng::new(seed)).unwrap();
    assert_eq!(hex(&sealed), captured);
    let opened = keys.open(&id, &unhex(captured)).unwrap();
    assert_eq!(opened.expose_secret(), STORED_PLAINTEXT);
  }
}

mod content {
  use super::*;

  #[test]
  fn message_is_stable() {
    let context = message_context();
    let encrypted = encrypt_message(
      &content_keys(),
      &context,
      STORED_PLAINTEXT,
      &device(),
      &mut TestRng::new(18),
    )
    .unwrap();
    assert_eq!(hex(&encrypted.to_bytes().unwrap()), ENCRYPTED_MESSAGE);
  }

  #[test]
  fn captured_message_still_decrypts() {
    let encrypted = EncryptedMessage::from_bytes(&unhex(ENCRYPTED_MESSAGE)).unwrap();
    let decrypted = decrypt_message(
      &content_keys(),
      &message_context(),
      &encrypted,
      &device().public_keys().signing_key,
    )
    .unwrap();
    assert_eq!(decrypted.expose_secret(), STORED_PLAINTEXT);
  }

  /// The blob is pinned through its hash: it is over a kilobyte. Same hash, same bytes, so opening
  /// it here is opening the captured blob.
  #[test]
  fn attachment_is_stable_and_still_decrypts() {
    let plaintext = attachment_plaintext();
    let encrypted =
      attachment::encrypt_attachment(&plaintext, attachment::MIN_CHUNK_SIZE, &mut TestRng::new(19)).unwrap();
    assert_eq!(hex(&encrypted.ciphertext_sha256), ATTACHMENT_SHA256);
    // The key is the first thing `encrypt_attachment` draws.
    let key = AttachmentKey::generate(&mut TestRng::new(19));
    let opened = attachment::decrypt_attachment(&key, &encrypted.ciphertext).unwrap();
    assert_eq!(opened.expose_secret(), plaintext);
  }

  pub(super) fn attachment_plaintext() -> Vec<u8> {
    (0..ATTACHMENT_LEN).map(|index| (index % 251) as u8).collect()
  }

  pub(super) fn content_keys() -> ContentKeyRing {
    let id = ContentKeyId::from_bytes(CONTENT_KEY_ID);
    let mut ring = ContentKeyRing::new(CONVERSATION);
    ring
      .insert(ContentKey::from_parts(id, SymmetricKey::from_bytes(CONTENT_KEY)))
      .unwrap();
    ring.set_current(id).unwrap();
    ring
  }

  pub(super) fn message_context() -> MessageContext {
    MessageContext {
      conversation_id: CONVERSATION,
      sender_user_id: USER,
      sender_device_id: DEVICE,
      client_message_id: ClientMessageId::generate(&mut TestRng::new(17)),
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
