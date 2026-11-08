//! TeamCraft: known-answer tests that pin the bytes of the frame cipher and the sender key
//! ratchet, so a dependency upgrade (sha2/hmac, aes-gcm) can't change them unnoticed
//! (VENDORED.md, issue #124). The values were captured with sha2 0.10 / hmac 0.12 before that
//! upgrade; the AEAD values match a standard AES-128-GCM (Python `cryptography`) with the tag cut
//! to its first 8 bytes.

use crate::cryptor::{aead_cipher::AeadCipher, hash_ratchet::HashRatchet};

const KEY: [u8; 16] = [0x42; 16];
const NONCE: [u8; 12] = [0x07; 12];
const AAD: &[u8] = b"unencrypted header";
const PLAINTEXT: &[u8; 18] = b"opus frame payload";
const CIPHERTEXT: [u8; 18] = [
  0x56, 0x23, 0x7c, 0xb1, 0x57, 0xa8, 0x5d, 0xb1, 0x4b, 0xfa, 0x6b, 0xe0, 0x80, 0xe5, 0xe5, 0x57,
  0xf6, 0x5e,
];
const TAG: [u8; 8] = [0x34, 0x09, 0x58, 0x0f, 0x7f, 0x7e, 0xed, 0x09];

#[test]
fn aead_known_answer() {
  let mut cipher = AeadCipher::new(&KEY).expect("16-byte key");
  let mut buffer = *PLAINTEXT;
  let tag = cipher.encrypt(&mut buffer, &NONCE, AAD).expect("encrypt");
  assert_eq!(buffer, CIPHERTEXT);
  assert_eq!(tag, TAG);
  cipher.decrypt(&mut buffer, &NONCE, AAD, &TAG).expect("decrypt");
  assert_eq!(&buffer, PLAINTEXT);
}

#[test]
fn aead_tampered_tag_is_rejected() {
  let mut cipher = AeadCipher::new(&KEY).expect("16-byte key");
  let mut buffer = CIPHERTEXT;
  let mut tag = TAG;
  tag[0] ^= 1;
  assert!(cipher.decrypt(&mut buffer, &NONCE, AAD, &tag).is_err());
}

/// Generation 1 runs the 32-byte secret derivation of `mlspp_crypto.rs` as well; libdave's
/// vector (`hash_ratchet.rs`, `expected_result`) only checks generation 0.
#[test]
fn hash_ratchet_generation_one() {
  let mut ratchet = HashRatchet::new(vec![
    206, 221, 97, 177, 184, 161, 202, 105, 4, 101, 84, 40, 44, 247, 11, 123,
  ]);
  let (key, nonce) = ratchet.get(1).expect("Expected success from ratchet");
  assert_eq!(
    *key,
    vec![205, 221, 6, 224, 243, 67, 117, 150, 186, 203, 156, 6, 160, 50, 156, 73]
  );
  assert_eq!(*nonce, vec![150, 253, 18, 131, 76, 61, 231, 55, 136, 236, 52, 25]);
}
