//! The server's settings key (`[secrets]`): inline, or a file created on first start. It encrypts
//! the passwords admins enter in the console before they reach the database.

use std::io::Write;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use rand_core::OsRng;
use secrecy::ExposeSecret;
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_crypto::server_secret::SettingsSealingKey;
use zeroize::Zeroizing;

use crate::config::SecretsConfig;

#[derive(Debug, thiserror::Error)]
pub enum SettingsKeyError {
  #[error("secrets.key is not 32 bytes of base64")]
  Malformed,
  #[error("secrets.key_path: {0}")]
  File(#[from] std::io::Error),
}

/// The sealing key, if `[secrets]` names one.
pub fn load(config: &SecretsConfig) -> Result<Option<SettingsSealingKey>, SettingsKeyError> {
  let master = if let Some(inline) = &config.maybe_key {
    parse(inline.expose_secret())?
  } else if let Some(path) = &config.maybe_key_path {
    load_or_create(path)?
  } else {
    return Ok(None);
  };
  Ok(Some(SettingsSealingKey::derive(&master)))
}

/// Reads the key file, or creates it (owner-only) with a fresh random key.
fn load_or_create(path: &Path) -> Result<SymmetricKey, SettingsKeyError> {
  match std::fs::read_to_string(path) {
    Ok(text) => return parse(&Zeroizing::new(text)),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
    Err(error) => return Err(error.into()),
  }
  let key = SymmetricKey::generate(&mut OsRng);
  let encoded = Zeroizing::new(URL_SAFE_NO_PAD.encode(key.expose_secret()));
  let mut options = std::fs::OpenOptions::new();
  options.write(true).create_new(true);
  #[cfg(unix)]
  std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
  options.open(path)?.write_all(encoded.as_bytes())?;
  Ok(key)
}

/// Standard or URL-safe base64, padded or not.
fn parse(text: &str) -> Result<SymmetricKey, SettingsKeyError> {
  let trimmed = text.trim();
  let bytes = Zeroizing::new(
    URL_SAFE_NO_PAD
      .decode(trimmed.trim_end_matches('='))
      .or_else(|_| STANDARD.decode(trimmed))
      .map_err(|_| SettingsKeyError::Malformed)?,
  );
  SymmetricKey::from_slice(&bytes).ok_or(SettingsKeyError::Malformed)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn inline_keys_must_be_32_bytes() {
    let good = SecretsConfig {
      maybe_key: Some(STANDARD.encode([3u8; 32]).into()),
      maybe_key_path: None,
    };
    assert!(load(&good).unwrap().is_some());
    let short = SecretsConfig {
      maybe_key: Some(STANDARD.encode([3u8; 16]).into()),
      maybe_key_path: None,
    };
    assert!(load(&short).is_err());
    assert!(load(&SecretsConfig::default()).unwrap().is_none());
  }

  #[test]
  fn a_key_file_is_created_once_and_read_back() {
    let dir = std::env::temp_dir().join(format!("teamcraft-settings-key-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("settings.key");
    let config = SecretsConfig {
      maybe_key: None,
      maybe_key_path: Some(path.clone()),
    };
    let first = load(&config).unwrap().unwrap();
    let sealed = first.seal("smtp", b"pw", &mut OsRng).unwrap();
    let second = load(&config).unwrap().unwrap();
    assert_eq!(second.open("smtp", &sealed).unwrap().expose_secret(), b"pw");
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;
      assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
      );
    }
    std::fs::remove_dir_all(&dir).unwrap();
  }
}
