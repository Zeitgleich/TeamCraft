//! Share links for external download (e2ee.md §4.5.1): the `#fragment` of
//! `https://chat.example.com/f/<share-id>#<fragment>`, which browsers never send to the server.
//!
//! The fragment carries the file's own key (§4.5) and its name, encrypted with that key. With a
//! password, the file key is additionally wrapped with Argon2id(password); the downloader then needs
//! both the link and the password.
//!
//! Layout (base64url, no padding):
//! `version (1) || flags (1) || [salt (16) || m_kib (4) || t (4) || p (4) || nonce (24) ||
//! wrapped key (32 + 16)] or [key (32)] || name nonce (24) || encrypted name`.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand_core::CryptoRngCore;

use crate::aead::{self, NONCE_LEN, TAG_LEN};
use crate::attachment::AttachmentKey;
use crate::backup::{Argon2Settings, argon2id};
use crate::domain::{self, SHARE_LINK_KEY_WRAP, SHARE_LINK_NAME};
use crate::secret::{SYMMETRIC_KEY_LEN, SecretBytes, SymmetricKey};

const VERSION: u8 = 1;
const FLAG_PASSWORD: u8 = 1;
const SALT_LEN: usize = 16;
/// Shortest password a link may be wrapped with: anyone with the link can try passwords offline
/// (Argon2id slows that down, it does not stop it). Mirrored for the UI as
/// `teamcraft_client_api::actions::MIN_SHARE_PASSWORD_CHARS`.
pub const MIN_PASSWORD_CHARS: usize = 8;
/// Longest file name a link carries (bytes).
pub const MAX_FILE_NAME_BYTES: usize = 1024;

#[derive(Debug, thiserror::Error)]
pub enum ShareLinkError {
  #[error("the link fragment is malformed")]
  Malformed,
  #[error("this link needs a password")]
  PasswordRequired,
  #[error("wrong password or tampered link")]
  WrongPassword,
  #[error("the file name is too long for a link")]
  NameTooLong,
  #[error("the password needs at least {MIN_PASSWORD_CHARS} characters")]
  PasswordTooShort,
  #[error("password key derivation failed")]
  KeyDerivation,
  #[error("encryption failed")]
  Encrypt,
}

/// What the downloader gets out of a fragment.
pub struct OpenedShareLink {
  pub key: AttachmentKey,
  pub file_name: String,
}

/// Builds the fragment for a file. `maybe_password` adds the Argon2id wrap with `argon2`.
pub fn create_fragment(
  key: &AttachmentKey,
  file_name: &str,
  maybe_password: Option<&SecretBytes>,
  argon2: Argon2Settings,
  rng: &mut (impl CryptoRngCore + ?Sized),
) -> Result<String, ShareLinkError> {
  if file_name.len() > MAX_FILE_NAME_BYTES {
    return Err(ShareLinkError::NameTooLong);
  }
  let mut out = vec![VERSION];
  match maybe_password {
    None => {
      out.push(0);
      out.extend_from_slice(key.as_key().expose_secret());
    },
    Some(password) => {
      let is_long_enough = std::str::from_utf8(password.expose_secret())
        .map_or(password.expose_secret().len(), |text| text.chars().count())
        >= MIN_PASSWORD_CHARS;
      if !is_long_enough {
        return Err(ShareLinkError::PasswordTooShort);
      }
      out.push(FLAG_PASSWORD);
      let mut salt = [0u8; SALT_LEN];
      rng.fill_bytes(&mut salt);
      let parameters = encode_parameters(&salt, &argon2);
      let wrapping_key =
        argon2id(password.expose_secret(), &argon2, &salt).map_err(|_| ShareLinkError::KeyDerivation)?;
      let aad = domain::labeled(SHARE_LINK_KEY_WRAP, &parameters);
      let (nonce, wrapped) = aead::seal(&wrapping_key, &aad, key.as_key().expose_secret(), rng)
        .map_err(|_| ShareLinkError::Encrypt)?;
      out.extend_from_slice(&parameters);
      out.extend_from_slice(&nonce);
      out.extend_from_slice(&wrapped);
    },
  }
  let name_aad = domain::labeled(SHARE_LINK_NAME, &[]);
  let (nonce, name) =
    aead::seal(key.as_key(), &name_aad, file_name.as_bytes(), rng).map_err(|_| ShareLinkError::Encrypt)?;
  out.extend_from_slice(&nonce);
  out.extend_from_slice(&name);
  Ok(URL_SAFE_NO_PAD.encode(out))
}

/// Whether opening the fragment needs a password.
pub fn needs_password(fragment: &str) -> Result<bool, ShareLinkError> {
  let bytes = decode(fragment)?;
  Ok(bytes.get(1).ok_or(ShareLinkError::Malformed)? & FLAG_PASSWORD != 0)
}

/// Opens a fragment (what the download page does; also for tests and the apps).
pub fn open_fragment(
  fragment: &str,
  maybe_password: Option<&SecretBytes>,
) -> Result<OpenedShareLink, ShareLinkError> {
  let bytes = decode(fragment)?;
  let (&version, rest) = bytes.split_first().ok_or(ShareLinkError::Malformed)?;
  let (&flags, rest) = rest.split_first().ok_or(ShareLinkError::Malformed)?;
  if version != VERSION || flags & !FLAG_PASSWORD != 0 {
    return Err(ShareLinkError::Malformed);
  }
  let (key, rest) = if flags & FLAG_PASSWORD == 0 {
    let (key, rest) = split(rest, SYMMETRIC_KEY_LEN)?;
    (
      SymmetricKey::from_slice(key).ok_or(ShareLinkError::Malformed)?,
      rest,
    )
  } else {
    let password = maybe_password.ok_or(ShareLinkError::PasswordRequired)?;
    let (parameters, rest) = split(rest, SALT_LEN + 12)?;
    let (nonce, rest) = split(rest, NONCE_LEN)?;
    let (wrapped, rest) = split(rest, SYMMETRIC_KEY_LEN + TAG_LEN)?;
    let (salt, settings) = decode_parameters(parameters)?;
    let wrapping_key =
      argon2id(password.expose_secret(), &settings, salt).map_err(|_| ShareLinkError::KeyDerivation)?;
    let aad = domain::labeled(SHARE_LINK_KEY_WRAP, parameters);
    let nonce: &[u8; NONCE_LEN] = nonce.try_into().map_err(|_| ShareLinkError::Malformed)?;
    let key = aead::open(&wrapping_key, nonce, &aad, wrapped).map_err(|_| ShareLinkError::WrongPassword)?;
    (
      SymmetricKey::from_slice(key.expose_secret()).ok_or(ShareLinkError::Malformed)?,
      rest,
    )
  };
  let (nonce, name) = split(rest, NONCE_LEN)?;
  let nonce: &[u8; NONCE_LEN] = nonce.try_into().map_err(|_| ShareLinkError::Malformed)?;
  let name = aead::open(&key, nonce, &domain::labeled(SHARE_LINK_NAME, &[]), name)
    .map_err(|_| ShareLinkError::Malformed)?;
  let file_name = String::from_utf8(name.expose_secret().to_vec()).map_err(|_| ShareLinkError::Malformed)?;
  Ok(OpenedShareLink {
    key: AttachmentKey::from_key(key),
    file_name,
  })
}

fn encode_parameters(salt: &[u8; SALT_LEN], settings: &Argon2Settings) -> Vec<u8> {
  let mut out = salt.to_vec();
  out.extend_from_slice(&settings.memory_kib.to_be_bytes());
  out.extend_from_slice(&settings.iterations.to_be_bytes());
  out.extend_from_slice(&settings.parallelism.to_be_bytes());
  out
}

fn decode_parameters(bytes: &[u8]) -> Result<(&[u8], Argon2Settings), ShareLinkError> {
  let (salt, rest) = split(bytes, SALT_LEN)?;
  let number = |index: usize| -> Result<u32, ShareLinkError> {
    let field = rest
      .get(index * 4..index * 4 + 4)
      .ok_or(ShareLinkError::Malformed)?;
    Ok(u32::from_be_bytes(
      field.try_into().map_err(|_| ShareLinkError::Malformed)?,
    ))
  };
  let settings = Argon2Settings {
    memory_kib: number(0)?,
    iterations: number(1)?,
    parallelism: number(2)?,
  };
  // Hostile links must not make the downloader allocate absurd memory.
  settings.validate().map_err(|_| ShareLinkError::Malformed)?;
  Ok((salt, settings))
}

fn split(bytes: &[u8], at: usize) -> Result<(&[u8], &[u8]), ShareLinkError> {
  if bytes.len() < at {
    return Err(ShareLinkError::Malformed);
  }
  Ok(bytes.split_at(at))
}

fn decode(fragment: &str) -> Result<Vec<u8>, ShareLinkError> {
  URL_SAFE_NO_PAD
    .decode(fragment.trim_start_matches('#'))
    .map_err(|_| ShareLinkError::Malformed)
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;

  const CHEAP: Argon2Settings = Argon2Settings {
    memory_kib: 64,
    iterations: 1,
    parallelism: 1,
  };

  #[test]
  fn a_short_password_is_refused() {
    let key = AttachmentKey::generate(&mut OsRng);
    for short in ["", "1234567", "äöüäöüä"] {
      let password = SecretBytes::new(short.as_bytes().to_vec());
      assert!(
        matches!(
          create_fragment(&key, "a", Some(&password), CHEAP, &mut OsRng),
          Err(ShareLinkError::PasswordTooShort)
        ),
        "{short:?}"
      );
    }
    let eight = SecretBytes::new("äöüäöüäö".as_bytes().to_vec());
    assert!(create_fragment(&key, "a", Some(&eight), CHEAP, &mut OsRng).is_ok());
  }

  #[test]
  fn plain_links_carry_key_and_name() {
    let key = AttachmentKey::generate(&mut OsRng);
    let fragment = create_fragment(&key, "report.pdf", None, CHEAP, &mut OsRng).unwrap();
    assert!(!needs_password(&fragment).unwrap());
    let opened = open_fragment(&fragment, None).unwrap();
    assert_eq!(opened.file_name, "report.pdf");
    assert_eq!(opened.key.as_key().expose_secret(), key.as_key().expose_secret());
    assert!(!fragment.contains("report"), "the name is encrypted");
  }

  #[test]
  fn password_links_need_the_right_password() {
    let key = AttachmentKey::generate(&mut OsRng);
    let password = SecretBytes::new(b"correct horse".to_vec());
    let fragment = create_fragment(&key, "plan.txt", Some(&password), CHEAP, &mut OsRng).unwrap();
    assert!(needs_password(&fragment).unwrap());
    assert!(matches!(
      open_fragment(&fragment, None),
      Err(ShareLinkError::PasswordRequired)
    ));
    assert!(matches!(
      open_fragment(&fragment, Some(&SecretBytes::new(b"wrong".to_vec()))),
      Err(ShareLinkError::WrongPassword)
    ));
    let opened = open_fragment(&fragment, Some(&password)).unwrap();
    assert_eq!(opened.key.as_key().expose_secret(), key.as_key().expose_secret());
  }

  #[test]
  fn hostile_fragments_are_rejected() {
    let key = AttachmentKey::generate(&mut OsRng);
    let fragment = create_fragment(&key, "a", None, CHEAP, &mut OsRng).unwrap();
    let mut bytes = URL_SAFE_NO_PAD.decode(&fragment).unwrap();
    for garbage in ["", "!!", "AA"] {
      assert!(open_fragment(garbage, None).is_err());
    }
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    assert!(open_fragment(&URL_SAFE_NO_PAD.encode(&bytes), None).is_err());
    // A password link whose parameters ask for absurd memory.
    let password = SecretBytes::new(b"a long password".to_vec());
    let fragment = create_fragment(&key, "a", Some(&password), CHEAP, &mut OsRng).unwrap();
    let mut bytes = URL_SAFE_NO_PAD.decode(&fragment).unwrap();
    bytes[2 + SALT_LEN..2 + SALT_LEN + 4].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(matches!(
      open_fragment(&URL_SAFE_NO_PAD.encode(&bytes), Some(&password)),
      Err(ShareLinkError::Malformed)
    ));
    assert!(matches!(
      create_fragment(
        &key,
        &"x".repeat(MAX_FILE_NAME_BYTES + 1),
        None,
        CHEAP,
        &mut OsRng
      ),
      Err(ShareLinkError::NameTooLong)
    ));
  }
}
