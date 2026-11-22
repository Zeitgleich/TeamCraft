//! A file on this device, encrypted as it is written under a key held only in memory (e2ee.md §8.3,
//! issue #2129): a call recording waits in the app's cache folder until the user posts or deletes
//! it, and nobody who reads the disk meanwhile (a backup, another user, a stolen laptop) gets the
//! audio.
//!
//! - The format is the attachment format (§4.5, [`crate::attachment`]): STREAM over
//!   XChaCha20-Poly1305 in chunks of [`CHUNK_SIZE`] with a random nonce prefix, every chunk with its
//!   own tag, the last one marked. Reordered, dropped, tampered or appended chunks and a file cut
//!   short fail to decrypt.
//! - The key is random per file and zeroized when dropped. It is not written to disk in the clear
//!   and not put in the OS keystore. An app that wants to read the file after a crash keeps it
//!   sealed under a key of the device's local secrets ([`SealedStreamKey::wrap`], issue #2116);
//!   without that, a file whose key is gone can't be read by anybody, the app included.
//! - [`SealedStreamWriter`] holds at most one chunk of plaintext, so the file grows on disk chunk
//!   by chunk and memory stays bounded however long the file gets. Its buffer is overwritten once
//!   a chunk is encrypted and when the writer is dropped; [`SealedStreamReader`] overwrites each
//!   decrypted chunk when it is replaced or dropped. Copies the caller makes on either side (the
//!   audio pipeline, `io::copy`, file buffers) are not wiped: what protects here is the disk, the
//!   process's memory is out of scope (e2ee.md §8.3).
//! - [`SealedStreamReader`] decrypts chunk by chunk as it is read. A chunk is handed out only once
//!   its tag verified, but truncation shows only at the end: what was read is trustworthy only once
//!   the reader returned end of file without an error.
//! - A file a crash left unfinished has no final chunk: [`SealedStreamReader::recovering`] reads
//!   the chunks that verify, in order, up to the first that doesn't (a cut, a chunk half written),
//!   and then ends; [`SealedStreamReader::is_complete`] says whether the final chunk was there.
//!   Every chunk it hands out still verified and sits at its place, so a file can be cut short
//!   unnoticed this way, never changed: the app tells its user that such a file is unfinished.

use std::fmt;
use std::io::{self, Read, Write};

use rand_core::CryptoRngCore;
use zeroize::{Zeroize, Zeroizing};

use crate::aead::{self, NONCE_LEN, TAG_LEN};
use crate::attachment::{
  AttachmentDecryptor, AttachmentEncryptor, AttachmentError, AttachmentKey, HEADER_LEN,
};
use crate::domain::{self, SEALED_STREAM_KEY_WRAP};
use crate::secret::{SYMMETRIC_KEY_LEN, SecretBytes, SymmetricKey};

/// Plaintext per chunk: 64 KiB, about 16 seconds of a call recording (32 kbit/s).
pub const CHUNK_SIZE: u32 = 64 * 1024;
/// Version byte of a wrapped key ([`SealedStreamKey::wrap`]).
const WRAP_VERSION: u8 = 1;
/// The longest note sealed with a wrapped key: what the app needs to offer the file again.
pub const MAX_WRAP_NOTE_LEN: usize = 1024;

/// The key of one sealed file. Only in memory; zeroized on drop; no `Clone`.
pub struct SealedStreamKey(AttachmentKey);

/// Encrypts what is written to it into `inner`. [`Self::finish`] writes the final chunk; a writer
/// dropped without it leaves a file that never decrypts (it has no final chunk).
pub struct SealedStreamWriter<W: Write> {
  inner: W,
  maybe_encryptor: Option<AttachmentEncryptor>,
  /// Plaintext not yet encrypted: at most one chunk, never reallocated (so no copy is left
  /// behind), overwritten after each chunk and on drop.
  pending: Zeroizing<Vec<u8>>,
  chunk_size: usize,
  plaintext_len: u64,
}

/// Decrypts a file [`SealedStreamWriter`] wrote, as it is read.
pub struct SealedStreamReader<R: Read> {
  inner: R,
  maybe_decryptor: Option<AttachmentDecryptor>,
  /// Reads what verifies of an unfinished file instead of failing at its end
  /// ([`Self::recovering`]).
  is_recovering: bool,
  /// The final chunk was read and verified.
  is_complete: bool,
  /// The decrypted chunk being handed out, and how much of it was.
  plaintext: SecretBytes,
  position: usize,
  /// The next encrypted chunk, plus one byte read ahead to tell whether it is the last.
  ciphertext: Vec<u8>,
}

/// A wrapped key that doesn't open: another device key, changed on disk, or not a wrapped key.
#[derive(Debug, thiserror::Error)]
pub enum KeyWrapError {
  #[error("the kept key doesn't open: it was changed, or belongs to another device key")]
  BadWrap,
  #[error("the note kept with the key is too long")]
  NoteTooLong,
  #[error("encryption failed")]
  Encrypt,
}

impl SealedStreamKey {
  pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
    Self(AttachmentKey::generate(rng))
  }

  /// Seals this key and `note` (at most [`MAX_WRAP_NOTE_LEN`] bytes: what the app needs to offer
  /// the file again) under `device_key`, a key of the device's local secrets, for a file that must
  /// stay readable after a crash (e2ee.md §8.3, #2116).
  ///
  /// Layout: `version (1) || nonce (24) || XChaCha20-Poly1305(key (32) || note) || tag (16)`, a
  /// fresh random nonce per call, the version bound in as associated data under its own label.
  /// The note is sealed too: the disk shows neither the key nor what the note says.
  pub fn wrap(
    &self,
    device_key: &SymmetricKey,
    note: &[u8],
    rng: &mut (impl CryptoRngCore + ?Sized),
  ) -> Result<Vec<u8>, KeyWrapError> {
    if note.len() > MAX_WRAP_NOTE_LEN {
      return Err(KeyWrapError::NoteTooLong);
    }
    let mut plaintext = Zeroizing::new(Vec::with_capacity(SYMMETRIC_KEY_LEN + note.len()));
    plaintext.extend_from_slice(self.0.as_key().expose_secret());
    plaintext.extend_from_slice(note);
    let (nonce, ciphertext) =
      aead::seal(device_key, &wrap_aad(), &plaintext, rng).map_err(|_| KeyWrapError::Encrypt)?;
    let mut wrapped = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    wrapped.push(WRAP_VERSION);
    wrapped.extend_from_slice(&nonce);
    wrapped.extend_from_slice(&ciphertext);
    Ok(wrapped)
  }

  /// Opens what [`Self::wrap`] sealed: the key and the note.
  pub fn unwrap(device_key: &SymmetricKey, wrapped: &[u8]) -> Result<(Self, SecretBytes), KeyWrapError> {
    let (&version, rest) = wrapped.split_first().ok_or(KeyWrapError::BadWrap)?;
    if version != WRAP_VERSION
      || rest.len() < NONCE_LEN + SYMMETRIC_KEY_LEN + TAG_LEN
      || rest.len() > NONCE_LEN + SYMMETRIC_KEY_LEN + MAX_WRAP_NOTE_LEN + TAG_LEN
    {
      return Err(KeyWrapError::BadWrap);
    }
    let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
    let nonce: &[u8; NONCE_LEN] = nonce.try_into().map_err(|_| KeyWrapError::BadWrap)?;
    let plaintext =
      aead::open(device_key, nonce, &wrap_aad(), ciphertext).map_err(|_| KeyWrapError::BadWrap)?;
    let (key, note) = plaintext.expose_secret().split_at(SYMMETRIC_KEY_LEN);
    let key = SymmetricKey::from_slice(key).ok_or(KeyWrapError::BadWrap)?;
    Ok((
      Self(AttachmentKey::from_key(key)),
      SecretBytes::new(note.to_vec()),
    ))
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
    self.pending.zeroize();
    self.inner.write_all(&last)?;
    self.inner.flush()?;
    Ok((self.inner, self.plaintext_len))
  }

  /// Encrypts the full pending chunk, which is not the last since more follows.
  fn write_pending_chunk(&mut self) -> io::Result<()> {
    let encryptor = self.maybe_encryptor.as_mut().ok_or_else(finished)?;
    let chunk = encryptor.encrypt_chunk(&self.pending).map_err(invalid)?;
    self.pending.zeroize();
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
  pub fn new(key: &SealedStreamKey, inner: R) -> io::Result<Self> {
    Self::open(key, inner, false)
  }

  /// Reads a file a crash may have left unfinished (e2ee.md §8.3, #2116): the chunks that verify,
  /// in order, up to the first that doesn't or the end of the file, then end of file without an
  /// error. A chunk cut short or half written ends it; so does a chunk changed on disk, which
  /// can't be told apart. [`Self::is_complete`] says afterwards whether the file was finished.
  /// The header must still verify: a file without one has nothing to recover.
  pub fn recovering(key: &SealedStreamKey, inner: R) -> io::Result<Self> {
    Self::open(key, inner, true)
  }

  /// Whether the final chunk was read and verified: the whole file, as its writer finished it.
  pub fn is_complete(&self) -> bool {
    self.is_complete
  }

  fn open(key: &SealedStreamKey, mut inner: R, is_recovering: bool) -> io::Result<Self> {
    let mut header = [0u8; HEADER_LEN];
    inner.read_exact(&mut header)?;
    let decryptor = AttachmentDecryptor::new(&key.0, &header).map_err(invalid)?;
    let capacity = decryptor.encrypted_chunk_len() + 1;
    Ok(Self {
      inner,
      maybe_decryptor: Some(decryptor),
      is_recovering,
      is_complete: false,
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
      match decryptor.decrypt_chunk(&self.ciphertext[..chunk_len]) {
        Ok(plaintext) => self.plaintext = plaintext,
        Err(_) if self.is_recovering => {
          self.end_recovery();
          return Ok(());
        },
        Err(error) => return Err(invalid(error)),
      }
      self.ciphertext.drain(..chunk_len);
      return Ok(());
    }
    if self.is_recovering {
      self.recover_end();
      return Ok(());
    }
    // End of the file: what is left must be the final chunk, else it was cut short.
    let decryptor = self.maybe_decryptor.take().ok_or_else(finished)?;
    if self.ciphertext.len() < TAG_LEN {
      return Err(invalid(AttachmentError::DecryptionFailed));
    }
    self.plaintext = decryptor.finish(&self.ciphertext).map_err(invalid)?;
    self.is_complete = true;
    self.ciphertext.clear();
    Ok(())
  }

  /// The end of a file read with [`Self::recovering`]: what is left is the final chunk, a full
  /// chunk the writer wrote just before a crash (it writes one only once more follows, so an
  /// unfinished file mostly ends in one), or a chunk cut short, which is dropped.
  fn recover_end(&mut self) {
    self.plaintext = SecretBytes::new(Vec::new());
    let Some(mut decryptor) = self.maybe_decryptor.take() else {
      return;
    };
    // A full chunk is tried as a middle chunk first: a failed try leaves the decryptor's position
    // as it was, so it can still be the final one (a file whose last chunk is exactly full).
    if self.ciphertext.len() == decryptor.encrypted_chunk_len()
      && let Ok(plaintext) = decryptor.decrypt_chunk(&self.ciphertext)
    {
      self.plaintext = plaintext;
    } else if self.ciphertext.len() >= TAG_LEN
      && let Ok(plaintext) = decryptor.finish(&self.ciphertext)
    {
      self.plaintext = plaintext;
      self.is_complete = true;
    }
    self.ciphertext.clear();
  }

  /// Ends a file read with [`Self::recovering`] at a chunk that didn't verify: nothing after it
  /// is read.
  fn end_recovery(&mut self) {
    self.plaintext = SecretBytes::new(Vec::new());
    self.maybe_decryptor = None;
    self.ciphertext.clear();
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

fn wrap_aad() -> Vec<u8> {
  domain::labeled(SEALED_STREAM_KEY_WRAP, &[WRAP_VERSION])
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
  fn a_duplicated_chunk_fails() {
    let key = SealedStreamKey::generate(&mut OsRng);
    let sealed = seal_in_pieces(&key, &sample(2 * CHUNK + 10), 4096);
    let (header, body) = sealed.split_at(HEADER_LEN);
    let mut duplicated = header.to_vec();
    duplicated.extend_from_slice(&body[..ENCRYPTED_CHUNK]);
    duplicated.extend_from_slice(body);
    assert_eq!(
      open(&key, &duplicated).unwrap_err().kind(),
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
  fn recovering_reads_an_unfinished_file_up_to_its_last_whole_chunk() {
    let key = SealedStreamKey::generate(&mut OsRng);
    let plaintext = sample(3 * CHUNK + 10);
    let mut sealed = Vec::new();
    {
      let mut writer = SealedStreamWriter::new(&key, &mut sealed, &mut OsRng).unwrap();
      writer.write_all(&plaintext).unwrap();
      // Dropped without `finish`: a crash while recording. The last 10 bytes and one full chunk
      // were still pending: the writer writes a chunk only once more follows.
    }
    assert_eq!(sealed.len(), HEADER_LEN + 3 * ENCRYPTED_CHUNK);
    let (read, is_complete) = recover(&key, &sealed).unwrap();
    assert_eq!(read, plaintext[..3 * CHUNK]);
    assert!(!is_complete);
  }

  #[test]
  fn recovering_drops_a_chunk_cut_short_or_damaged_and_everything_after_it() {
    let key = SealedStreamKey::generate(&mut OsRng);
    let plaintext = sample(3 * CHUNK + 10);
    let sealed = seal_in_pieces(&key, &plaintext, 4096);
    // Cut inside the second chunk, inside the final chunk's tag, at a chunk boundary.
    for (len, kept) in [
      (HEADER_LEN + ENCRYPTED_CHUNK + 100, CHUNK),
      (sealed.len() - 1, 3 * CHUNK),
      (HEADER_LEN + 2 * ENCRYPTED_CHUNK, 2 * CHUNK),
      (HEADER_LEN, 0),
    ] {
      let (read, is_complete) = recover(&key, &sealed[..len]).unwrap();
      assert_eq!(read, plaintext[..kept], "cut at {len}");
      assert!(!is_complete);
    }
    // A damaged middle chunk (a hole a power loss left) ends what is read.
    let mut damaged = sealed.clone();
    damaged[HEADER_LEN + ENCRYPTED_CHUNK + 7] ^= 1;
    let (read, is_complete) = recover(&key, &damaged).unwrap();
    assert_eq!(read, plaintext[..CHUNK]);
    assert!(!is_complete);
  }

  #[test]
  fn recovering_a_finished_file_reads_all_of_it() {
    let key = SealedStreamKey::generate(&mut OsRng);
    for len in [0, 10, CHUNK, 2 * CHUNK, 2 * CHUNK + 3] {
      let plaintext = sample(len);
      let sealed = seal_in_pieces(&key, &plaintext, 4096);
      let (read, is_complete) = recover(&key, &sealed).unwrap();
      assert_eq!(read, plaintext, "length {len}");
      assert!(is_complete, "length {len}");
    }
  }

  #[test]
  fn recovering_still_needs_the_key_and_the_header() {
    let key = SealedStreamKey::generate(&mut OsRng);
    let sealed = seal_in_pieces(&key, &sample(CHUNK + 10), 4096);
    let other = SealedStreamKey::generate(&mut OsRng);
    let (read, is_complete) = recover(&other, &sealed).unwrap();
    assert!(read.is_empty() && !is_complete, "another key reads nothing");
    assert!(SealedStreamReader::recovering(&key, Cursor::new(&sealed[..HEADER_LEN - 1])).is_err());
  }

  #[test]
  fn a_wrapped_key_opens_with_its_note_under_the_same_device_key_only() {
    let key = SealedStreamKey::generate(&mut OsRng);
    let sealed = seal_in_pieces(&key, &sample(100), 100);
    let device_key = SymmetricKey::generate(&mut OsRng);
    let wrapped = key.wrap(&device_key, b"note", &mut OsRng).unwrap();
    assert!(!wrapped.windows(4).any(|window| window == b"note"));
    let (unwrapped, note) = SealedStreamKey::unwrap(&device_key, &wrapped).unwrap();
    assert_eq!(note.expose_secret(), b"note");
    assert_eq!(open(&unwrapped, &sealed).unwrap(), sample(100));
    let other = SymmetricKey::generate(&mut OsRng);
    assert!(SealedStreamKey::unwrap(&other, &wrapped).is_err());
    for index in 0..wrapped.len() {
      let mut changed = wrapped.clone();
      changed[index] ^= 1;
      assert!(
        SealedStreamKey::unwrap(&device_key, &changed).is_err(),
        "byte {index}"
      );
    }
    assert!(SealedStreamKey::unwrap(&device_key, &wrapped[..wrapped.len() - 1]).is_err());
    assert!(SealedStreamKey::unwrap(&device_key, &[]).is_err());
    assert!(matches!(
      key.wrap(&device_key, &[0; MAX_WRAP_NOTE_LEN + 1], &mut OsRng),
      Err(KeyWrapError::NoteTooLong)
    ));
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

  /// What [`SealedStreamReader::recovering`] reads, and whether the file was complete.
  fn recover(key: &SealedStreamKey, sealed: &[u8]) -> io::Result<(Vec<u8>, bool)> {
    let mut reader = SealedStreamReader::recovering(key, Cursor::new(sealed))?;
    let mut plaintext = Vec::new();
    reader.read_to_end(&mut plaintext)?;
    Ok((plaintext, reader.is_complete()))
  }

  fn sample(len: usize) -> Vec<u8> {
    (0..len).map(|index| (index % 251) as u8).collect()
  }
}
