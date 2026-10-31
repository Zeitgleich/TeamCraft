//! The state directory (docs/import-synology.md §8): the device's encrypted store, its key, the
//! sign-in and the progress, all readable by this user only.

use std::io::Write;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rand_core::{OsRng, RngCore};

/// Writes `bytes` to `path` readable by this user only, replacing it whole (a crash leaves the
/// old file or the new one).
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
  let temporary = path.with_extension("new");
  {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
      use std::os::unix::fs::OpenOptionsExt;
      options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
  }
  std::fs::rename(&temporary, path)
}

/// Makes the state directory, readable by this user only.
pub fn make_state_dir(path: &Path) -> std::io::Result<()> {
  std::fs::create_dir_all(path)?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
  }
  Ok(())
}

/// A secret kept in the state directory (the store's key, the identity's passphrase): read, or
/// made from 32 random bytes the first time.
pub fn secret_in(path: &Path) -> std::io::Result<String> {
  match std::fs::read_to_string(path) {
    Ok(text) => Ok(text.trim().to_owned()),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
      let mut bytes = [0_u8; 32];
      OsRng.fill_bytes(&mut bytes);
      let encoded = STANDARD.encode(bytes);
      write_private(path, encoded.as_bytes())?;
      Ok(encoded)
    },
    Err(error) => Err(error),
  }
}

/// Tells the operator something on standard output (the tool's own output, apart from the log).
pub fn say(text: &str) {
  let mut out = std::io::stdout().lock();
  // Nothing to do when nobody reads it (a closed pipe).
  drop(writeln!(out, "{text}"));
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_secret_is_made_once_and_kept() {
    let dir = std::env::temp_dir().join(format!("teamcraft-import-files-{}", std::process::id()));
    drop(std::fs::remove_dir_all(&dir));
    make_state_dir(&dir).unwrap();
    let first = secret_in(&dir.join("store.key")).unwrap();
    assert_eq!(first, secret_in(&dir.join("store.key")).unwrap());
    assert_eq!(STANDARD.decode(&first).unwrap().len(), 32);
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;
      let mode = std::fs::metadata(dir.join("store.key"))
        .unwrap()
        .permissions()
        .mode();
      assert_eq!(mode & 0o777, 0o600);
    }
    drop(std::fs::remove_dir_all(&dir));
  }
}
