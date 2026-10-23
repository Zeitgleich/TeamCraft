use crate::aes_gcm::{AesGcm, KeyInit, aead::AeadMutInPlace, aes::Aes128};
use sha2::digest::{
  InvalidLength,
  consts::{U8, U12},
};

type Aes128GcmModified = AesGcm<Aes128, U12, U8>;

pub struct AeadCipher {
  key: Aes128GcmModified,
}

impl AeadCipher {
  pub fn new(key: &[u8]) -> Result<Self, InvalidLength> {
    Ok(Self {
      key: Aes128GcmModified::new_from_slice(key)?,
    })
  }

  pub fn encrypt(
    &mut self,
    buffer: &mut [u8],
    nonce: &[u8],
    aad: &[u8],
  ) -> crate::aes_gcm::aead::Result<[u8; 8]> {
    self
      .key
      .encrypt_in_place_detached(nonce.into(), aad, buffer)
      .map(|tag| tag.into())
  }

  pub fn decrypt(
    &mut self,
    buffer: &mut [u8],
    nonce: &[u8],
    aad: &[u8],
    tag: &[u8],
  ) -> crate::aes_gcm::aead::Result<()> {
    self
      .key
      .decrypt_in_place_detached(nonce.into(), aad, buffer, tag.into())
  }
}

// TeamCraft: known-answer test for the truncated-tag AES-128-GCM (vendored change 5). Values match
// a standard AES-128-GCM (Python `cryptography`) with the tag cut to its first 8 bytes.
#[cfg(test)]
mod tests {
  use super::*;

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
  fn known_answer() {
    let mut cipher = AeadCipher::new(&KEY).expect("16-byte key");
    let mut buffer = *PLAINTEXT;
    let tag = cipher.encrypt(&mut buffer, &NONCE, AAD).expect("encrypt");
    assert_eq!(buffer, CIPHERTEXT);
    assert_eq!(tag, TAG);
    cipher.decrypt(&mut buffer, &NONCE, AAD, &TAG).expect("decrypt");
    assert_eq!(&buffer, PLAINTEXT);
  }

  #[test]
  fn tampered_tag_is_rejected() {
    let mut cipher = AeadCipher::new(&KEY).expect("16-byte key");
    let mut buffer = CIPHERTEXT;
    let mut tag = TAG;
    tag[0] ^= 1;
    assert!(cipher.decrypt(&mut buffer, &NONCE, AAD, &tag).is_err());
  }
}
