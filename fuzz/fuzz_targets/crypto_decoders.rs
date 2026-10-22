//! The binary decoders of `teamcraft-crypto` for data that comes from the network or the server:
//! encrypted messages, key bags, wrapped backup keys, signed membership changes, device
//! certificates and call key bindings. Garbage must produce an error, never a panic.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft_crypto::backup::WrappedBackupKey;
use teamcraft_crypto::call_binding::SignedCallKeyBinding;
use teamcraft_crypto::identity::SignedDeviceCertificate;
use teamcraft_crypto::key_bag::EncryptedKeyBag;
use teamcraft_crypto::membership::SignedMembershipChange;
use teamcraft_crypto::message::EncryptedMessage;

fuzz_target!(|data: &[u8]| {
  let _ = EncryptedMessage::from_bytes(data);
  let _ = EncryptedKeyBag::from_bytes(data);
  let _ = WrappedBackupKey::from_bytes(data);
  let _ = SignedMembershipChange::from_bytes(data);
  let _ = SignedDeviceCertificate::from_bytes(data);
  let _ = SignedCallKeyBinding::from_bytes(data);
});
