//! SHA-256 checksums of files a client writes for the user to keep (the data export, issue #211):
//! they let the user, or a tool, check that an archive arrived whole. Integrity only; no secret is
//! involved, and the files they cover are the user's own plaintext.

use sha2::{Digest, Sha256};

/// SHA-256 over a file written piece by piece.
#[derive(Clone, Default)]
pub struct FileChecksum(Sha256);

impl FileChecksum {
  pub fn new() -> Self {
    Self(Sha256::new())
  }

  pub fn update(&mut self, bytes: &[u8]) {
    self.0.update(bytes);
  }

  /// The checksum as lowercase hex, as `sha256sum` prints it.
  pub fn finish_hex(self) -> String {
    let digest: [u8; 32] = self.0.finalize().into();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn pieces_hash_like_the_whole_and_print_like_sha256sum() {
    let mut pieces = FileChecksum::new();
    pieces.update(b"ab");
    pieces.update(b"c");
    assert_eq!(
      pieces.finish_hex(),
      "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
  }
}
