//! The import device's DPoP key (RFC 9449; e2ee.md §3.1.2): one P-256 key in the state directory,
//! readable by this user only like the refresh token it is bound to. A new sign-in makes a new key;
//! a refresh uses the one the sign-in was bound with and no other.
//!
//! A software key: the import runs on a server without a hardware key store, and whoever can read the
//! state directory can read the store's key and the refresh token as well. What the binding still
//! adds is that a copy of the access token alone, from a log or a proxy on the way, is worth nothing.

use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use teamcraft_crypto::dpop::DpopSigningKey;

use crate::files::write_private;

const KEY_FILE: &str = "dpop.key";

#[derive(Debug, thiserror::Error)]
pub enum ProofKeyError {
  #[error("{path}: {source}")]
  File { path: PathBuf, source: std::io::Error },
  #[error("{0}: not a DPoP key")]
  Unreadable(PathBuf),
}

/// Makes a new key and keeps it, replacing the one before.
pub fn create(state: &Path) -> Result<DpopSigningKey, ProofKeyError> {
  let key = DpopSigningKey::generate();
  let path = state.join(KEY_FILE);
  let encoded = zeroize::Zeroizing::new(STANDARD.encode(key.to_secret_bytes()));
  write_private(&path, encoded.as_bytes()).map_err(|source| ProofKeyError::File { path, source })?;
  Ok(key)
}

/// The kept key, `None` if there is none.
pub fn load(state: &Path) -> Result<Option<DpopSigningKey>, ProofKeyError> {
  let path = state.join(KEY_FILE);
  let text = match std::fs::read_to_string(&path) {
    Ok(text) => zeroize::Zeroizing::new(text),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
    Err(source) => return Err(ProofKeyError::File { path, source }),
  };
  let bytes = STANDARD
    .decode(text.trim())
    .map(zeroize::Zeroizing::new)
    .map_err(|_| ProofKeyError::Unreadable(path.clone()))?;
  DpopSigningKey::from_secret_bytes(&bytes)
    .map(Some)
    .map_err(|_| ProofKeyError::Unreadable(path))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_key_is_kept_private_and_read_back() {
    let dir = temporary_dir("kept");
    assert!(load(&dir).unwrap().is_none());
    let key = create(&dir).unwrap();
    let read = load(&dir).unwrap().unwrap();
    assert_eq!(read.thumbprint(), key.thumbprint());
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;
      let mode = std::fs::metadata(dir.join(KEY_FILE))
        .unwrap()
        .permissions()
        .mode();
      assert_eq!(mode & 0o777, 0o600);
    }
    drop(std::fs::remove_dir_all(&dir));
  }

  #[test]
  fn a_new_sign_in_replaces_the_key() {
    let dir = temporary_dir("replaced");
    let first = create(&dir).unwrap();
    let second = create(&dir).unwrap();
    assert_ne!(first.thumbprint(), second.thumbprint());
    assert_eq!(load(&dir).unwrap().unwrap().thumbprint(), second.thumbprint());
    drop(std::fs::remove_dir_all(&dir));
  }

  #[test]
  fn a_damaged_key_is_an_error_not_a_missing_one() {
    let dir = temporary_dir("damaged");
    for content in ["not base64 at all", "AAAA"] {
      std::fs::write(dir.join(KEY_FILE), content).unwrap();
      assert!(matches!(load(&dir), Err(ProofKeyError::Unreadable(_))));
    }
    drop(std::fs::remove_dir_all(&dir));
  }

  fn temporary_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
      "teamcraft-import-proof-key-{name}-{}",
      std::process::id()
    ));
    drop(std::fs::remove_dir_all(&dir));
    crate::files::make_state_dir(&dir).unwrap();
    dir
  }
}
