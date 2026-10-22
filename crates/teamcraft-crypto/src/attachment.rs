//! Chunked, streaming attachment encryption (e2ee.md §4.5).
//!
//! Every file gets a fresh random key, so a share link (§4.5.1) exposes exactly one file. The file
//! is split into fixed-size chunks and encrypted with the STREAM construction (Hoang, Reyhanitabar,
//! Rogaway, Vizár 2015) over XChaCha20-Poly1305 as implemented in `aead::stream` (`StreamBE32`):
//! each chunk has its own tag, chunks can't be reordered or dropped, and truncation is detected
//! because the final chunk is marked. Uploads and downloads never need the whole file in memory.
//!
//! Format: `header || chunk_0 || … || chunk_n`, where the header is
//! `version (1) || chunk_size (u32 BE) || nonce_prefix (19)` and every chunk except the last is
//! exactly `chunk_size + 16` bytes. The header is the associated data of every chunk.

use std::fmt;

use chacha20poly1305::XChaCha20Poly1305;
use chacha20poly1305::aead::stream::{DecryptorBE32, EncryptorBE32};
use chacha20poly1305::aead::{KeyInit, Payload};
use rand_core::CryptoRngCore;
use sha2::{Digest, Sha256};

use crate::aead::TAG_LEN;
use crate::domain;
use crate::secret::{SecretBytes, SymmetricKey};

const ATTACHMENT_VERSION: u8 = 1;
/// XChaCha20 nonce (24 bytes) minus STREAM's 4-byte counter and 1-byte last-chunk flag.
const NONCE_PREFIX_LEN: usize = 19;
pub const HEADER_LEN: usize = 1 + 4 + NONCE_PREFIX_LEN;
/// Default plaintext chunk size: 64 KiB.
pub const DEFAULT_CHUNK_SIZE: u32 = 64 * 1024;
pub const MIN_CHUNK_SIZE: u32 = 1024;
/// Upper bound, also enforced when decrypting, so a hostile header can't force huge buffers.
pub const MAX_CHUNK_SIZE: u32 = 16 * 1024 * 1024;

/// The per-file key. Goes into the encrypted message (or a share link fragment), never to the server.
#[derive(Debug)]
pub struct AttachmentKey(SymmetricKey);

/// Encrypts one file chunk by chunk.
pub struct AttachmentEncryptor {
  encryptor: EncryptorBE32<XChaCha20Poly1305>,
  header: [u8; HEADER_LEN],
  chunk_size: usize,
}

/// Decrypts one file chunk by chunk.
pub struct AttachmentDecryptor {
  decryptor: DecryptorBE32<XChaCha20Poly1305>,
  header: [u8; HEADER_LEN],
  chunk_size: usize,
}

/// Result of [`encrypt_attachment`]: the key for the message and the blob for S3.
#[derive(Debug)]
pub struct EncryptedAttachment {
  pub key: AttachmentKey,
  pub ciphertext: Vec<u8>,
  /// SHA-256 of `ciphertext`, sent inside the encrypted message so the recipient can check the
  /// blob before decrypting and the client can deduplicate uploads.
  pub ciphertext_sha256: [u8; 32],
}

#[derive(Debug, thiserror::Error)]
pub enum AttachmentError {
  #[error("chunk size {0} is outside {MIN_CHUNK_SIZE}..={MAX_CHUNK_SIZE}")]
  InvalidChunkSize(u32),
  #[error("chunk has {actual} bytes, expected {expected}")]
  WrongChunkLength { expected: usize, actual: usize },
  #[error("attachment header is malformed")]
  MalformedHeader,
  #[error("unsupported attachment version {0}")]
  UnsupportedVersion(u8),
  #[error("too many chunks for one attachment")]
  TooManyChunks,
  #[error("attachment could not be decrypted (wrong key, tampered, reordered or truncated)")]
  DecryptionFailed,
}

/// Encrypts a whole file in memory with a fresh key. For large files use [`AttachmentEncryptor`].
pub fn encrypt_attachment(
  plaintext: &[u8],
  chunk_size: u32,
  rng: &mut impl CryptoRngCore,
) -> Result<EncryptedAttachment, AttachmentError> {
  let key = AttachmentKey::generate(rng);
  let mut encryptor = AttachmentEncryptor::new(&key, chunk_size, rng)?;
  let mut ciphertext = encryptor.header().to_vec();
  let chunk_len = encryptor.chunk_size;
  // The last chunk may be full-sized or empty; it always exists so truncation is detectable.
  let full_chunks = if plaintext.is_empty() {
    0
  } else {
    (plaintext.len() - 1) / chunk_len
  };
  let (body, last) = plaintext.split_at(full_chunks * chunk_len);
  for chunk in body.chunks(chunk_len) {
    ciphertext.extend_from_slice(&encryptor.encrypt_chunk(chunk)?);
  }
  ciphertext.extend_from_slice(&encryptor.finish(last)?);
  let ciphertext_sha256 = Sha256::digest(&ciphertext).into();
  Ok(EncryptedAttachment {
    key,
    ciphertext,
    ciphertext_sha256,
  })
}

/// SHA-256 of a public object whose hash a signed document vouches for (e.g. an organization
/// background in the workspace policy).
pub fn content_sha256(content: &[u8]) -> [u8; 32] {
  Sha256::digest(content).into()
}

/// Whether a downloaded blob is the one the message announced (SHA-256 from
/// [`EncryptedAttachment::ciphertext_sha256`]). Checked before decrypting, so a swapped blob fails
/// early with a clear error.
pub fn ciphertext_matches(ciphertext: &[u8], expected_sha256: &[u8]) -> bool {
  Sha256::digest(ciphertext).as_slice() == expected_sha256
}

/// Decrypts a whole file in memory. For large files use [`AttachmentDecryptor`].
pub fn decrypt_attachment(key: &AttachmentKey, ciphertext: &[u8]) -> Result<SecretBytes, AttachmentError> {
  let header = ciphertext
    .get(..HEADER_LEN)
    .ok_or(AttachmentError::MalformedHeader)?;
  let mut decryptor = AttachmentDecryptor::new(key, header)?;
  let encrypted_chunk_len = decryptor.encrypted_chunk_len();
  let body = &ciphertext[HEADER_LEN..];
  // Mirror of the encryption split: the last (possibly empty) chunk is at most one full chunk.
  let full_chunks = if body.is_empty() {
    0
  } else {
    (body.len() - 1) / encrypted_chunk_len
  };
  let (chunks, last) = body.split_at(full_chunks * encrypted_chunk_len);
  let mut plaintext = Vec::with_capacity(body.len());
  for chunk in chunks.chunks(encrypted_chunk_len) {
    plaintext.extend_from_slice(decryptor.decrypt_chunk(chunk)?.expose_secret());
  }
  plaintext.extend_from_slice(decryptor.finish(last)?.expose_secret());
  Ok(SecretBytes::new(plaintext))
}

impl AttachmentKey {
  pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
    Self(SymmetricKey::generate(rng))
  }

  pub fn from_key(key: SymmetricKey) -> Self {
    Self(key)
  }

  pub fn as_key(&self) -> &SymmetricKey {
    &self.0
  }
}

impl AttachmentEncryptor {
  /// Starts a new encryption. Write [`Self::header`] first, then every chunk.
  pub fn new(
    key: &AttachmentKey,
    chunk_size: u32,
    rng: &mut impl CryptoRngCore,
  ) -> Result<Self, AttachmentError> {
    validate_chunk_size(chunk_size)?;
    let mut nonce_prefix = [0u8; NONCE_PREFIX_LEN];
    rng.fill_bytes(&mut nonce_prefix);
    let header = encode_header(chunk_size, &nonce_prefix);
    let cipher = XChaCha20Poly1305::new(key.0.expose_secret().into());
    let encryptor = EncryptorBE32::from_aead(cipher, nonce_prefix.as_slice().into());
    Ok(Self {
      encryptor,
      header,
      chunk_size: chunk_size as usize,
    })
  }

  pub fn header(&self) -> &[u8; HEADER_LEN] {
    &self.header
  }

  /// Encrypts one full chunk of exactly `chunk_size` bytes.
  pub fn encrypt_chunk(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, AttachmentError> {
    if plaintext.len() != self.chunk_size {
      return Err(AttachmentError::WrongChunkLength {
        expected: self.chunk_size,
        actual: plaintext.len(),
      });
    }
    let aad = chunk_aad(&self.header);
    self
      .encryptor
      .encrypt_next(Payload {
        msg: plaintext,
        aad: &aad,
      })
      .map_err(|_| AttachmentError::TooManyChunks)
  }

  /// Encrypts the final chunk (0 to `chunk_size` bytes) and ends the stream.
  pub fn finish(self, plaintext: &[u8]) -> Result<Vec<u8>, AttachmentError> {
    if plaintext.len() > self.chunk_size {
      return Err(AttachmentError::WrongChunkLength {
        expected: self.chunk_size,
        actual: plaintext.len(),
      });
    }
    let aad = chunk_aad(&self.header);
    self
      .encryptor
      .encrypt_last(Payload {
        msg: plaintext,
        aad: &aad,
      })
      .map_err(|_| AttachmentError::TooManyChunks)
  }
}

impl fmt::Debug for AttachmentEncryptor {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("AttachmentEncryptor")
      .field("chunk_size", &self.chunk_size)
      .finish_non_exhaustive()
  }
}

impl AttachmentDecryptor {
  /// Starts decryption from the first [`HEADER_LEN`] bytes of the blob.
  pub fn new(key: &AttachmentKey, header: &[u8]) -> Result<Self, AttachmentError> {
    let (header, chunk_size, nonce_prefix) = decode_header(header)?;
    let cipher = XChaCha20Poly1305::new(key.0.expose_secret().into());
    let decryptor = DecryptorBE32::from_aead(cipher, nonce_prefix.as_slice().into());
    Ok(Self {
      decryptor,
      header,
      chunk_size: chunk_size as usize,
    })
  }

  /// Size of every encrypted chunk except the last.
  pub fn encrypted_chunk_len(&self) -> usize {
    self.chunk_size + TAG_LEN
  }

  /// Decrypts one full chunk of exactly [`Self::encrypted_chunk_len`] bytes.
  pub fn decrypt_chunk(&mut self, ciphertext: &[u8]) -> Result<SecretBytes, AttachmentError> {
    if ciphertext.len() != self.encrypted_chunk_len() {
      return Err(AttachmentError::WrongChunkLength {
        expected: self.encrypted_chunk_len(),
        actual: ciphertext.len(),
      });
    }
    let aad = chunk_aad(&self.header);
    self
      .decryptor
      .decrypt_next(Payload {
        msg: ciphertext,
        aad: &aad,
      })
      .map(SecretBytes::new)
      .map_err(|_| AttachmentError::DecryptionFailed)
  }

  /// Decrypts the final chunk. Fails if the stream was truncated or extended.
  pub fn finish(self, ciphertext: &[u8]) -> Result<SecretBytes, AttachmentError> {
    if ciphertext.len() < TAG_LEN || ciphertext.len() > self.encrypted_chunk_len() {
      return Err(AttachmentError::DecryptionFailed);
    }
    let aad = chunk_aad(&self.header);
    self
      .decryptor
      .decrypt_last(Payload {
        msg: ciphertext,
        aad: &aad,
      })
      .map(SecretBytes::new)
      .map_err(|_| AttachmentError::DecryptionFailed)
  }
}

impl fmt::Debug for AttachmentDecryptor {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("AttachmentDecryptor")
      .field("chunk_size", &self.chunk_size)
      .finish_non_exhaustive()
  }
}

fn validate_chunk_size(chunk_size: u32) -> Result<(), AttachmentError> {
  if (MIN_CHUNK_SIZE..=MAX_CHUNK_SIZE).contains(&chunk_size) {
    Ok(())
  } else {
    Err(AttachmentError::InvalidChunkSize(chunk_size))
  }
}

fn encode_header(chunk_size: u32, nonce_prefix: &[u8; NONCE_PREFIX_LEN]) -> [u8; HEADER_LEN] {
  let mut header = [0u8; HEADER_LEN];
  header[0] = ATTACHMENT_VERSION;
  header[1..5].copy_from_slice(&chunk_size.to_be_bytes());
  header[5..].copy_from_slice(nonce_prefix);
  header
}

fn decode_header(bytes: &[u8]) -> Result<([u8; HEADER_LEN], u32, [u8; NONCE_PREFIX_LEN]), AttachmentError> {
  let header = <[u8; HEADER_LEN]>::try_from(bytes).map_err(|_| AttachmentError::MalformedHeader)?;
  if header[0] != ATTACHMENT_VERSION {
    return Err(AttachmentError::UnsupportedVersion(header[0]));
  }
  let chunk_size = u32::from_be_bytes([header[1], header[2], header[3], header[4]]);
  validate_chunk_size(chunk_size)?;
  let mut nonce_prefix = [0u8; NONCE_PREFIX_LEN];
  nonce_prefix.copy_from_slice(&header[5..]);
  Ok((header, chunk_size, nonce_prefix))
}

fn chunk_aad(header: &[u8; HEADER_LEN]) -> Vec<u8> {
  domain::labeled(domain::ATTACHMENT_AAD, header)
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;

  const CHUNK: u32 = MIN_CHUNK_SIZE;

  #[test]
  fn round_trip_for_boundary_sizes() {
    let chunk = CHUNK as usize;
    for len in [0, 1, chunk - 1, chunk, chunk + 1, 3 * chunk, 3 * chunk + 7] {
      let plaintext = sample(len);
      let encrypted = encrypt_attachment(&plaintext, CHUNK, &mut OsRng).unwrap();
      let decrypted = decrypt_attachment(&encrypted.key, &encrypted.ciphertext).unwrap();
      assert_eq!(decrypted.expose_secret(), plaintext.as_slice(), "length {len}");
    }
  }

  #[test]
  fn streaming_api_matches_one_shot() {
    let plaintext = sample(2 * CHUNK as usize + 10);
    let key = AttachmentKey::generate(&mut OsRng);
    let mut encryptor = AttachmentEncryptor::new(&key, CHUNK, &mut OsRng).unwrap();
    let mut blob = encryptor.header().to_vec();
    let (full, last) = plaintext.split_at(2 * CHUNK as usize);
    for chunk in full.chunks(CHUNK as usize) {
      blob.extend(encryptor.encrypt_chunk(chunk).unwrap());
    }
    blob.extend(encryptor.finish(last).unwrap());

    assert_eq!(
      decrypt_attachment(&key, &blob).unwrap().expose_secret(),
      plaintext.as_slice()
    );
  }

  #[test]
  fn ciphertext_matches_its_announced_hash_only() {
    let encrypted = encrypt_attachment(b"report", CHUNK, &mut OsRng).unwrap();
    assert!(ciphertext_matches(
      &encrypted.ciphertext,
      &encrypted.ciphertext_sha256
    ));
    let mut swapped = encrypted.ciphertext.clone();
    swapped[HEADER_LEN] ^= 1;
    assert!(!ciphertext_matches(&swapped, &encrypted.ciphertext_sha256));
  }

  #[test]
  fn hash_covers_ciphertext() {
    let encrypted = encrypt_attachment(b"report", CHUNK, &mut OsRng).unwrap();
    assert_eq!(
      encrypted.ciphertext_sha256,
      <[u8; 32]>::from(Sha256::digest(&encrypted.ciphertext))
    );
  }

  mod rejection {
    use super::*;

    #[test]
    fn rejects_wrong_key() {
      let encrypted = encrypt_attachment(&sample(100), CHUNK, &mut OsRng).unwrap();
      let wrong = AttachmentKey::generate(&mut OsRng);
      assert!(matches!(
        decrypt_attachment(&wrong, &encrypted.ciphertext),
        Err(AttachmentError::DecryptionFailed)
      ));
    }

    #[test]
    fn rejects_tampered_bytes_anywhere() {
      let encrypted = encrypt_attachment(&sample(2 * CHUNK as usize + 5), CHUNK, &mut OsRng).unwrap();
      // Nonce prefix, first chunk body, first chunk tag, last chunk.
      let positions = [
        10,
        HEADER_LEN + 3,
        HEADER_LEN + CHUNK as usize + 2,
        encrypted.ciphertext.len() - 1,
      ];
      for position in positions {
        let mut tampered = encrypted.ciphertext.clone();
        tampered[position] ^= 1;
        assert!(
          decrypt_attachment(&encrypted.key, &tampered).is_err(),
          "position {position}"
        );
      }
    }

    #[test]
    fn rejects_truncation_reordering_and_extension() {
      let chunk_len = CHUNK as usize + TAG_LEN;
      let encrypted = encrypt_attachment(&sample(3 * CHUNK as usize), CHUNK, &mut OsRng).unwrap();
      let blob = &encrypted.ciphertext;
      let header = &blob[..HEADER_LEN];
      let chunks: Vec<&[u8]> = blob[HEADER_LEN..].chunks(chunk_len).collect();
      assert_eq!(chunks.len(), 3);

      let dropped_last = [header, chunks[0], chunks[1]].concat();
      let swapped = [header, chunks[1], chunks[0], chunks[2]].concat();
      let duplicated = [header, chunks[0], chunks[0], chunks[1], chunks[2]].concat();
      let extended = [blob.as_slice(), &[0u8; TAG_LEN]].concat();
      for tampered in [dropped_last, swapped, duplicated, extended] {
        assert!(decrypt_attachment(&encrypted.key, &tampered).is_err());
      }
    }

    #[test]
    fn rejects_changed_chunk_size_in_header() {
      let encrypted = encrypt_attachment(&sample(5000), CHUNK, &mut OsRng).unwrap();
      let mut tampered = encrypted.ciphertext.clone();
      tampered[1..5].copy_from_slice(&(2 * CHUNK).to_be_bytes());
      assert!(decrypt_attachment(&encrypted.key, &tampered).is_err());
    }

    #[test]
    fn rejects_malformed_headers() {
      let key = AttachmentKey::generate(&mut OsRng);
      assert!(matches!(
        decrypt_attachment(&key, &[]),
        Err(AttachmentError::MalformedHeader)
      ));
      assert!(matches!(
        decrypt_attachment(&key, &[1; 10]),
        Err(AttachmentError::MalformedHeader)
      ));
      let mut bad_version = encode_header(CHUNK, &[0; NONCE_PREFIX_LEN]);
      bad_version[0] = 2;
      assert!(matches!(
        AttachmentDecryptor::new(&key, &bad_version),
        Err(AttachmentError::UnsupportedVersion(2))
      ));
      for size in [0, MIN_CHUNK_SIZE - 1, MAX_CHUNK_SIZE + 1, u32::MAX] {
        let header = encode_header(size, &[0; NONCE_PREFIX_LEN]);
        assert!(matches!(
          AttachmentDecryptor::new(&key, &header),
          Err(AttachmentError::InvalidChunkSize(_))
        ));
      }
      // Header only, without the mandatory final chunk.
      let encrypted = encrypt_attachment(b"x", CHUNK, &mut OsRng).unwrap();
      assert!(decrypt_attachment(&encrypted.key, &encrypted.ciphertext[..HEADER_LEN]).is_err());
    }

    #[test]
    fn rejects_wrong_chunk_lengths_in_streaming_api() {
      let key = AttachmentKey::generate(&mut OsRng);
      let mut encryptor = AttachmentEncryptor::new(&key, CHUNK, &mut OsRng).unwrap();
      assert!(matches!(
        encryptor.encrypt_chunk(&[0; 10]),
        Err(AttachmentError::WrongChunkLength { .. })
      ));
      assert!(matches!(
        encryptor.finish(&sample(CHUNK as usize + 1)),
        Err(AttachmentError::WrongChunkLength { .. })
      ));
      assert!(matches!(
        AttachmentEncryptor::new(&key, MAX_CHUNK_SIZE + 1, &mut OsRng),
        Err(AttachmentError::InvalidChunkSize(_))
      ));
    }
  }

  fn sample(len: usize) -> Vec<u8> {
    (0..len).map(|index| (index % 251) as u8).collect()
  }
}
