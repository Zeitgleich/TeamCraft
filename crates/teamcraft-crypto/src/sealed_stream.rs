//! A file on this device, encrypted as it is written under a key held only in memory (e2ee.md §8.3,
//! issue #2129): a call recording waits in the app's cache folder until the user posts or deletes
//! it, and nobody who reads the disk meanwhile (a backup, another user, a stolen laptop) gets the
//! audio.
//!
//! - The format is the attachment format (§4.5, [`crate::attachment`]): STREAM over
//!   XChaCha20-Poly1305 in chunks of [`CHUNK_SIZE`] with a random nonce prefix, every chunk with its
//!   own tag, the last one marked. Reordered, dropped, tampered or appended chunks and a file cut
//!   short fail to decrypt.
//! - The key is random per file and never leaves the process: it is not written to disk, not put
//!   in the OS keystore, and zeroized when dropped. A file whose key is gone (the app quit or
//!   crashed) can't be read by anybody, the app included; the app deletes it at its next start.
//! - [`SealedStreamWriter`] holds at most one chunk of plaintext (zeroized), so the file grows on
//!   disk chunk by chunk and memory stays bounded however long the file gets.
//! - [`SealedStreamReader`] decrypts chunk by chunk as it is read. A chunk is handed out only once
//!   its tag verified, but truncation shows only at the end: what was read is trustworthy only once
//!   the reader returned end of file without an error.

use std::fmt;
use std::io::{self, Read, Write};

use rand_core::CryptoRngCore;
use zeroize::Zeroizing;

use crate::aead::TAG_LEN;
use crate::attachment::{
  AttachmentDecryptor, AttachmentEncryptor, AttachmentError, AttachmentKey, HEADER_LEN,
};
use crate::secret::SecretBytes;

/// Plaintext per chunk: 64 KiB, about 16 seconds of a call recording (32 kbit/s).
pub const CHUNK_SIZE: u32 = 64 * 1024;

/// The key of one sealed file. Only in memory; zeroized on drop; no `Clone`.
pub struct SealedStreamKey(AttachmentKey);

/// Encrypts what is written to it into `inner`. [`Self::finish`] writes the final chunk; a writer
/// dropped without it leaves a file that never decrypts (it has no final chunk).
pub struct SealedStreamWriter<W: Write> {
  inner: W,
  maybe_encryptor: Option<AttachmentEncryptor>,
  /// Plaintext not yet encrypted: at most one chunk, never reallocated (so no copy is left
  /// behind), zeroized on drop.
  pending: Zeroizing<Vec<u8>>,
  chunk_size: usize,
  plaintext_len: u64,
}

/// Decrypts a file [`SealedStreamWriter`] wrote, as it is read.
pub struct SealedStreamReader<R: Read> {
  inner: R,
  maybe_decryptor: Option<AttachmentDecryptor>,
  /// The decrypted chunk being handed out, and how much of it was.
  plaintext: SecretBytes,
  position: usize,
  /// The next encrypted chunk, plus one byte read ahead to tell whether it is the last.
  ciphertext: Vec<u8>,
}

impl SealedStreamKey {
  pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
    Self(AttachmentKey::generate(rng))
  }
}

impl fmt::Debug for SealedStreamKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("SealedStreamKey([redacted])")
  }
}

impl<W: Write> SealedStreamWriter<W> {
  /// Starts the file: writes its header to `inner` right away.
  pub fn new(key: &SealedStreamKey, mut inner: W, rng: &mut impl CryptoRngCore) -> io::Result<Self> {
    let encryptor = AttachmentEncryptor::new(&key.0, CHUNK_SIZE, rng).map_err(invalid)?;
    inner.write_all(encryptor.header())?;
    let chunk_size = CHUNK_SIZE as usize;
    Ok(Self {
      inner,
      maybe_encryptor: Some(encryptor),
      pending: Zeroizing::new(Vec::with_capacity(chunk_size)),
      chunk_size,
      plaintext_len: 0,
    })
  }

  /// Encrypts what is pending as the final chunk and flushes. Returns `inner` and the plaintext
  /// length.
  pub fn finish(mut self) -> io::Result<(W, u64)> {
    let encryptor = self.maybe_encryptor.take().ok_or_else(finished)?;
    let last = encryptor.finish(&self.pending).map_err(invalid)?;
    self.pending.clear();
    self.inner.write_all(&last)?;
    self.inner.flush()?;
    Ok((self.inner, self.plaintext_len))
  }

  /// Encrypts the full pending chunk, which is not the last since more follows.
  fn write_pending_chunk(&mut self) -> io::Result<()> {
    let encryptor = self.maybe_encryptor.as_mut().ok_or_else(finished)?;
    let chunk = encryptor.encrypt_chunk(&self.pending).map_err(invalid)?;
    self.pending.clear();
    self.inner.write_all(&chunk)
  }
}

impl<W: Write> Write for SealedStreamWriter<W> {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    if buf.is_empty() {
      return Ok(0);
    }
    // A full chunk is written only once more data shows it isn't the last.
    if self.pending.len() == self.chunk_size {
      self.write_pending_chunk()?;
    }
    let taken = buf.len().min(self.chunk_size - self.pending.len());
    self.pending.extend_from_slice(&buf[..taken]);
    self.plaintext_len += taken as u64;
    Ok(taken)
  }

  /// Flushes `inner`. Up to one chunk stays pending: only whole chunks are written before
  /// [`Self::finish`].
  fn flush(&mut self) -> io::Result<()> {
    self.inner.flush()
  }
}

impl<W: Write> fmt::Debug for SealedStreamWriter<W> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("SealedStreamWriter")
      .field("plaintext_len", &self.plaintext_len)
      .finish_non_exhaustive()
  }
}

impl<R: Read> SealedStreamReader<R> {
  /// Reads the header from `inner`.
  pub fn new(key: &SealedStreamKey, mut inner: R) -> io::Result<Self> {
    let mut header = [0u8; HEADER_LEN];
    inner.read_exact(&mut header)?;
    let decryptor = AttachmentDecryptor::new(&key.0, &header).map_err(invalid)?;
    let capacity = decryptor.encrypted_chunk_len() + 1;
    Ok(Self {
      inner,
      maybe_decryptor: Some(decryptor),
      plaintext: SecretBytes::new(Vec::new()),
      position: 0,
      ciphertext: Vec::with_capacity(capacity),
    })
  }

  /// Decrypts the next chunk into `plaintext`; at the final one, the decryptor is done.
  fn next_chunk(&mut self) -> io::Result<()> {
    let Some(decryptor) = self.maybe_decryptor.as_mut() else {
      return Ok(());
    };
    let chunk_len = decryptor.encrypted_chunk_len();
    let wanted = chunk_len + 1;
    while self.ciphertext.len() < wanted {
      let start = self.ciphertext.len();
      self.ciphertext.resize(wanted, 0);
      match self.inner.read(&mut self.ciphertext[start..]) {
        Ok(0) => {
          self.ciphertext.truncate(start);
          break;
        },
        Ok(read) => self.ciphertext.truncate(start + read),
        Err(error) if error.kind() == io::ErrorKind::Interrupted => self.ciphertext.truncate(start),
        Err(error) => {
          self.ciphertext.truncate(start);
          return Err(error);
        },
      }
    }
    self.position = 0;
    if self.ciphertext.len() == wanted {
      // More follows: this chunk is not the last. Keep the byte read ahead.
      self.plaintext = decryptor
        .decrypt_chunk(&self.ciphertext[..chunk_len])
        .map_err(invalid)?;
      self.ciphertext.drain(..chunk_len);
      return Ok(());
    }
    // End of the file: what is left must be the final chunk, else it was cut short.
    let decryptor = self.maybe_decryptor.take().ok_or_else(finished)?;
    if self.ciphertext.len() < TAG_LEN {
      return Err(invalid(AttachmentError::DecryptionFailed));
    }
    self.plaintext = decryptor.finish(&self.ciphertext).map_err(invalid)?;
    self.ciphertext.clear();
    Ok(())
  }
}

impl<R: Read> Read for SealedStreamReader<R> {
  fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
    while self.position == self.plaintext.len() {
      if self.maybe_decryptor.is_none() {
        return Ok(0);
      }
      self.next_chunk()?;
    }
    let available = &self.plaintext.expose_secret()[self.position..];
    let copied = available.len().min(buf.len());
    buf[..copied].copy_from_slice(&available[..copied]);
    self.position += copied;
    Ok(copied)
  }
}

impl<R: Read> fmt::Debug for SealedStreamReader<R> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("SealedStreamReader").finish_non_exhaustive()
  }
}

/// A file that doesn't decrypt (wrong key, tampered, reordered or cut short) reads as invalid data.
fn invalid(error: AttachmentError) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, error)
}

fn finished() -> io::Error {
  io::Error::other("the sealed stream is already finished")
}

#[cfg(test)]
mod tests {
  use std::io::Cursor;

  use rand_core::OsRng;

  use super::*;

  const CHUNK: usize = CHUNK_SIZE as usize;
  const ENCRYPTED_CHUNK: usize = CHUNK + TAG_LEN;

  #[test]
  fn round_trips_any_length_written_in_any_pieces() {
    let key = SealedStreamKey::generate(&mut OsRng);
    for len in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK, 3 * CHUNK + 17] {
      let plaintext = sample(len);
      let sealed = seal_in_pieces(&key, &plaintext, 997);
      assert_eq!(
        sealed.len() as u64,
        crate::attachment::encrypted_len(len as u64, CHUNK_SIZE)
      );
      assert_eq!(open(&key, &sealed).unwrap(), plaintext, "length {len}");
    }
  }

  #[test]
  fn the_file_holds_no_plaintext() {
    let key = SealedStreamKey::generate(&mut OsRng);
    let plaintext = vec![b'A'; 2 * CHUNK];
    let sealed = seal_in_pieces(&key, &plaintext, CHUNK);
    assert!(
      !sealed
        .windows(64)
        .any(|window| window.iter().all(|byte| *byte == b'A'))
    );
  }

  #[test]
  fn the_writer_reports_the_plaintext_length() {
    let key = SealedStreamKey::generate(&mut OsRng);
    let mut writer = SealedStreamWriter::new(&key, Vec::new(), &mut OsRng).unwrap();
    writer.write_all(&sample(CHUNK + 5)).unwrap();
    let (_, len) = writer.finish().unwrap();
    assert_eq!(len, CHUNK as u64 + 5);
  }

  #[test]
  fn a_tampered_chunk_fails() {
    let key = SealedStreamKey::generate(&mut OsRng);
    let mut sealed = seal_in_pieces(&key, &sample(2 * CHUNK + 10), 4096);
    sealed[HEADER_LEN + ENCRYPTED_CHUNK + 3] ^= 1;
    assert_eq!(
      open(&key, &sealed).unwrap_err().kind(),
      io::ErrorKind::InvalidData
    );
  }

  #[test]
  fn a_tampered_header_fails() {
    let key = SealedStreamKey::generate(&mut OsRng);
    let mut sealed = seal_in_pieces(&key, &sample(100), 100);
    sealed[HEADER_LEN - 1] ^= 1;
    assert!(open(&key, &sealed).is_err());
  }

  #[test]
  fn a_truncated_file_fails() {
    let key = SealedStreamKey::generate(&mut OsRng);
    let sealed = seal_in_pieces(&key, &sample(3 * CHUNK + 10), 4096);
    // Cut at a chunk boundary (the final chunk dropped), inside a chunk, inside the last tag,
    // inside the header, and the empty file.
    for len in [
      HEADER_LEN + 3 * ENCRYPTED_CHUNK,
      HEADER_LEN + 2 * ENCRYPTED_CHUNK,
      HEADER_LEN + ENCRYPTED_CHUNK + 100,
      sealed.len() - 1,
      HEADER_LEN + 5,
      HEADER_LEN,
      HEADER_LEN - 1,
      0,
    ] {
      assert!(open(&key, &sealed[..len]).is_err(), "cut at {len}");
    }
  }

  #[test]
  fn an_extended_file_fails() {
    let key = SealedStreamKey::generate(&mut OsRng);
    let mut sealed = seal_in_pieces(&key, &sample(CHUNK + 10), 4096);
    sealed.extend_from_slice(&[0; TAG_LEN + 4]);
    assert!(open(&key, &sealed).is_err());
  }

  #[test]
  fn reordered_chunks_fail() {
    let key = SealedStreamKey::generate(&mut OsRng);
    let sealed = seal_in_pieces(&key, &sample(3 * CHUNK + 10), 4096);
    let (header, body) = sealed.split_at(HEADER_LEN);
    let mut swapped = header.to_vec();
    swapped.extend_from_slice(&body[ENCRYPTED_CHUNK..2 * ENCRYPTED_CHUNK]);
    swapped.extend_from_slice(&body[..ENCRYPTED_CHUNK]);
    swapped.extend_from_slice(&body[2 * ENCRYPTED_CHUNK..]);
    assert_eq!(swapped.len(), sealed.len());
    assert_eq!(
      open(&key, &swapped).unwrap_err().kind(),
      io::ErrorKind::InvalidData
    );
  }

  #[test]
  fn a_chunk_from_another_file_fails() {
    let key = SealedStreamKey::generate(&mut OsRng);
    let first = seal_in_pieces(&key, &sample(2 * CHUNK), 4096);
    let second = seal_in_pieces(&key, &sample(2 * CHUNK), 4096);
    let mut mixed = first[..HEADER_LEN + ENCRYPTED_CHUNK].to_vec();
    mixed.extend_from_slice(&second[HEADER_LEN + ENCRYPTED_CHUNK..]);
    assert!(open(&key, &mixed).is_err());
  }

  #[test]
  fn the_wrong_key_fails() {
    let key = SealedStreamKey::generate(&mut OsRng);
    let sealed = seal_in_pieces(&key, &sample(CHUNK + 10), 4096);
    let other = SealedStreamKey::generate(&mut OsRng);
    assert_eq!(
      open(&other, &sealed).unwrap_err().kind(),
      io::ErrorKind::InvalidData
    );
  }

  #[test]
  fn an_unfinished_file_fails() {
    let key = SealedStreamKey::generate(&mut OsRng);
    let mut sealed = Vec::new();
    {
      let mut writer = SealedStreamWriter::new(&key, &mut sealed, &mut OsRng).unwrap();
      writer.write_all(&sample(2 * CHUNK + 10)).unwrap();
      // Dropped without `finish`: a crash while recording.
    }
    assert!(open(&key, &sealed).is_err());
  }

  #[test]
  fn the_key_never_prints() {
    let key = SealedStreamKey::generate(&mut OsRng);
    assert_eq!(format!("{key:?}"), "SealedStreamKey([redacted])");
  }

  fn seal_in_pieces(key: &SealedStreamKey, plaintext: &[u8], piece: usize) -> Vec<u8> {
    let mut writer = SealedStreamWriter::new(key, Vec::new(), &mut OsRng).unwrap();
    for part in plaintext.chunks(piece) {
      writer.write_all(part).unwrap();
    }
    writer.finish().unwrap().0
  }

  fn open(key: &SealedStreamKey, sealed: &[u8]) -> io::Result<Vec<u8>> {
    let mut reader = SealedStreamReader::new(key, Cursor::new(sealed))?;
    let mut plaintext = Vec::new();
    // Small reads, so chunks are handed out in pieces too.
    let mut buf = [0u8; 1000];
    loop {
      let read = reader.read(&mut buf)?;
      if read == 0 {
        return Ok(plaintext);
      }
      plaintext.extend_from_slice(&buf[..read]);
    }
  }

  fn sample(len: usize) -> Vec<u8> {
    (0..len).map(|index| (index % 251) as u8).collect()
  }
}
