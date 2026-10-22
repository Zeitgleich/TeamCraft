//! The backup key and the ways it is wrapped (e2ee.md §6).
//!
//! The key bag is encrypted with a random backup key. The backup key itself is stored on the server
//! only in wrapped form, once per [`WrapMethod`]: by the device keystore (§6.1), by a passkey PRF
//! output (§6.2), by a recovery passphrase stretched with Argon2id (§6.2 fallback) and, later, by
//! the organization recovery key (§6.3). Every wrapper implements [`BackupKeyWrapper`].

use argon2::{Algorithm, Argon2, Params, Version};
use hkdf::Hkdf;
use rand_core::CryptoRngCore;
use sha2::Sha256;
use tls_codec::{DeserializeBytes as _, Serialize as _, TlsDeserializeBytes, TlsSerialize, TlsSize};
use zeroize::Zeroizing;

use crate::aead::{self, NONCE_LEN};
use crate::domain;
use crate::ids::UserId;
use crate::secret::{SYMMETRIC_KEY_LEN, SecretBytes, SymmetricKey};

const WRAPPED_BACKUP_KEY_VERSION: u8 = 1;
const PASSPHRASE_SALT_LEN: usize = 16;

/// Upper bounds for Argon2id parameters read from a wrapped key. The server supplies the wrapped
/// key, so these stop it from making a client allocate or compute without limit.
pub const MAX_ARGON2_MEMORY_KIB: u32 = 1024 * 1024;
pub const MAX_ARGON2_ITERATIONS: u32 = 16;
pub const MAX_ARGON2_PARALLELISM: u32 = 8;

/// The random 256-bit key that encrypts the key bag.
#[derive(Debug)]
pub struct BackupKey(SymmetricKey);

/// Wraps and unwraps the backup key with one recovery factor.
///
/// Implementations: [`PassphraseWrapper`], [`SymmetricKeyWrapper`] (device keystore, passkey PRF).
/// The organization recovery wrapper (§6.3, public-key encryption to a threshold-shared key) is
/// a later implementation of this trait.
pub trait BackupKeyWrapper {
  fn method(&self) -> WrapMethod;

  fn wrap_key(
    &self,
    user_id: UserId,
    backup_key: &BackupKey,
    rng: &mut dyn CryptoRngCore,
  ) -> Result<WrappedBackupKey, BackupError>;

  fn unwrap_key(&self, user_id: UserId, wrapped: &WrappedBackupKey) -> Result<BackupKey, BackupError>;
}

/// How a [`WrappedBackupKey`] was wrapped. Stored in plaintext so the client knows which factor
/// to ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, TlsSerialize, TlsDeserializeBytes, TlsSize)]
#[repr(u8)]
pub enum WrapMethod {
  DeviceKeystore = 1,
  PasskeyPrf = 2,
  Passphrase = 3,
  OrgRecovery = 4,
}

/// A backup key encrypted under one recovery factor, as stored on the server.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct WrappedBackupKey {
  version: u8,
  method: WrapMethod,
  /// Public, method-specific parameters (e.g. Argon2id salt and cost). Authenticated as AAD.
  parameters: Vec<u8>,
  nonce: [u8; NONCE_LEN],
  ciphertext: Vec<u8>,
}

/// Wraps with a recovery passphrase stretched by Argon2id (e2ee.md §6.2 fallback).
pub struct PassphraseWrapper {
  passphrase: SecretBytes,
  settings: Argon2Settings,
}

/// Argon2id cost parameters. Configurable; the default follows e2ee.md §6.2 (128 MiB, ~1 s).
#[derive(Clone, Copy, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct Argon2Settings {
  pub memory_kib: u32,
  pub iterations: u32,
  pub parallelism: u32,
}

/// Wraps with a 256-bit key-encryption key that comes from outside: the OS keystore (§6.1) or a
/// passkey PRF output (§6.2).
pub struct SymmetricKeyWrapper {
  method: WrapMethod,
  key_encryption_key: SymmetricKey,
}

/// Public parameters of a passphrase-wrapped key.
#[derive(TlsSerialize, TlsDeserializeBytes, TlsSize)]
struct PassphraseParameters {
  settings: Argon2Settings,
  salt: [u8; PASSPHRASE_SALT_LEN],
}

/// Associated data binding a wrapped key to its user, method and parameters.
#[derive(TlsSerialize, TlsSize)]
struct WrapAad<'a> {
  version: u8,
  user_id: UserId,
  method: WrapMethod,
  parameters: &'a [u8],
}

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
  #[error("wrapped key uses method {actual:?}, this wrapper handles {expected:?}")]
  WrongMethod {
    expected: WrapMethod,
    actual: WrapMethod,
  },
  #[error("unsupported wrapped key version {0}")]
  UnsupportedVersion(u8),
  #[error("Argon2id settings out of range: {0:?}")]
  InvalidArgon2Settings(Argon2Settings),
  #[error("key derivation failed")]
  KeyDerivation,
  #[error("passkey PRF output must be at least 32 bytes")]
  PrfOutputTooShort,
  #[error("encryption failed")]
  EncryptionFailed,
  #[error("backup key could not be unwrapped (wrong factor or tampered)")]
  UnwrapFailed,
  #[error("encoding failed: {0}")]
  Encoding(#[from] tls_codec::Error),
}

impl BackupKey {
  pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
    Self(SymmetricKey::generate(rng))
  }

  pub fn from_key(key: SymmetricKey) -> Self {
    Self(key)
  }

  /// The raw key, for keeping it in the device's encrypted local store after a recovery.
  pub fn into_key(self) -> SymmetricKey {
    self.0
  }

  pub(crate) fn key(&self) -> &SymmetricKey {
    &self.0
  }
}

impl WrappedBackupKey {
  pub fn method(&self) -> WrapMethod {
    self.method
  }

  pub fn to_bytes(&self) -> Result<Vec<u8>, BackupError> {
    Ok(self.tls_serialize_detached()?)
  }

  pub fn from_bytes(bytes: &[u8]) -> Result<Self, BackupError> {
    Ok(Self::tls_deserialize_exact_bytes(bytes)?)
  }
}

impl PassphraseWrapper {
  pub fn new(passphrase: SecretBytes, settings: Argon2Settings) -> Result<Self, BackupError> {
    settings.validate()?;
    Ok(Self { passphrase, settings })
  }

  fn derive_key(
    &self,
    settings: &Argon2Settings,
    salt: &[u8; PASSPHRASE_SALT_LEN],
  ) -> Result<SymmetricKey, BackupError> {
    settings.validate()?;
    let params = Params::new(
      settings.memory_kib,
      settings.iterations,
      settings.parallelism,
      Some(SYMMETRIC_KEY_LEN),
    )
    .map_err(|_| BackupError::InvalidArgon2Settings(*settings))?;
    let mut output = Zeroizing::new([0u8; SYMMETRIC_KEY_LEN]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
      .hash_password_into(self.passphrase.expose_secret(), salt, output.as_mut())
      .map_err(|_| BackupError::KeyDerivation)?;
    Ok(SymmetricKey::from_bytes(*output))
  }
}

impl BackupKeyWrapper for PassphraseWrapper {
  fn method(&self) -> WrapMethod {
    WrapMethod::Passphrase
  }

  fn wrap_key(
    &self,
    user_id: UserId,
    backup_key: &BackupKey,
    rng: &mut dyn CryptoRngCore,
  ) -> Result<WrappedBackupKey, BackupError> {
    let mut salt = [0u8; PASSPHRASE_SALT_LEN];
    rng.fill_bytes(&mut salt);
    let key_encryption_key = self.derive_key(&self.settings, &salt)?;
    let parameters = PassphraseParameters {
      settings: self.settings,
      salt,
    }
    .tls_serialize_detached()?;
    seal_backup_key(
      user_id,
      WrapMethod::Passphrase,
      parameters,
      &key_encryption_key,
      backup_key,
      rng,
    )
  }

  fn unwrap_key(&self, user_id: UserId, wrapped: &WrappedBackupKey) -> Result<BackupKey, BackupError> {
    ensure_method(WrapMethod::Passphrase, wrapped)?;
    let parameters = PassphraseParameters::tls_deserialize_exact_bytes(&wrapped.parameters)?;
    let key_encryption_key = self.derive_key(&parameters.settings, &parameters.salt)?;
    open_backup_key(user_id, &key_encryption_key, wrapped)
  }
}

impl std::fmt::Debug for PassphraseWrapper {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("PassphraseWrapper")
      .field("settings", &self.settings)
      .finish_non_exhaustive()
  }
}

impl Argon2Settings {
  pub fn validate(&self) -> Result<(), BackupError> {
    let memory_ok = (Params::MIN_M_COST..=MAX_ARGON2_MEMORY_KIB).contains(&self.memory_kib)
      && self.memory_kib >= 8 * self.parallelism;
    let iterations_ok = (Params::MIN_T_COST..=MAX_ARGON2_ITERATIONS).contains(&self.iterations);
    let parallelism_ok = (Params::MIN_P_COST..=MAX_ARGON2_PARALLELISM).contains(&self.parallelism);
    if memory_ok && iterations_ok && parallelism_ok {
      Ok(())
    } else {
      Err(BackupError::InvalidArgon2Settings(*self))
    }
  }
}

impl Default for Argon2Settings {
  fn default() -> Self {
    Self {
      memory_kib: 128 * 1024,
      iterations: 3,
      parallelism: 1,
    }
  }
}

impl SymmetricKeyWrapper {
  /// Uses a key held in the OS keystore (Keychain, Android Keystore, Secret Service).
  pub fn device_keystore(key: SymmetricKey) -> Self {
    Self {
      method: WrapMethod::DeviceKeystore,
      key_encryption_key: key,
    }
  }

  /// Derives the wrapping key from a WebAuthn PRF extension output via HKDF-SHA256.
  pub fn passkey_prf(prf_output: &SecretBytes) -> Result<Self, BackupError> {
    if prf_output.len() < SYMMETRIC_KEY_LEN {
      return Err(BackupError::PrfOutputTooShort);
    }
    let hkdf = Hkdf::<Sha256>::new(None, prf_output.expose_secret());
    let mut output = Zeroizing::new([0u8; SYMMETRIC_KEY_LEN]);
    hkdf
      .expand(domain::PASSKEY_PRF_KDF.as_bytes(), output.as_mut())
      .map_err(|_| BackupError::KeyDerivation)?;
    Ok(Self {
      method: WrapMethod::PasskeyPrf,
      key_encryption_key: SymmetricKey::from_bytes(*output),
    })
  }
}

impl BackupKeyWrapper for SymmetricKeyWrapper {
  fn method(&self) -> WrapMethod {
    self.method
  }

  fn wrap_key(
    &self,
    user_id: UserId,
    backup_key: &BackupKey,
    rng: &mut dyn CryptoRngCore,
  ) -> Result<WrappedBackupKey, BackupError> {
    seal_backup_key(
      user_id,
      self.method,
      Vec::new(),
      &self.key_encryption_key,
      backup_key,
      rng,
    )
  }

  fn unwrap_key(&self, user_id: UserId, wrapped: &WrappedBackupKey) -> Result<BackupKey, BackupError> {
    ensure_method(self.method, wrapped)?;
    open_backup_key(user_id, &self.key_encryption_key, wrapped)
  }
}

impl std::fmt::Debug for SymmetricKeyWrapper {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("SymmetricKeyWrapper")
      .field("method", &self.method)
      .finish_non_exhaustive()
  }
}

fn seal_backup_key(
  user_id: UserId,
  method: WrapMethod,
  parameters: Vec<u8>,
  key_encryption_key: &SymmetricKey,
  backup_key: &BackupKey,
  rng: &mut dyn CryptoRngCore,
) -> Result<WrappedBackupKey, BackupError> {
  let aad = wrap_aad(user_id, method, &parameters)?;
  let (nonce, ciphertext) = aead::seal(key_encryption_key, &aad, backup_key.0.expose_secret(), rng)
    .map_err(|_| BackupError::EncryptionFailed)?;
  Ok(WrappedBackupKey {
    version: WRAPPED_BACKUP_KEY_VERSION,
    method,
    parameters,
    nonce,
    ciphertext,
  })
}

fn open_backup_key(
  user_id: UserId,
  key_encryption_key: &SymmetricKey,
  wrapped: &WrappedBackupKey,
) -> Result<BackupKey, BackupError> {
  if wrapped.version != WRAPPED_BACKUP_KEY_VERSION {
    return Err(BackupError::UnsupportedVersion(wrapped.version));
  }
  let aad = wrap_aad(user_id, wrapped.method, &wrapped.parameters)?;
  let plaintext = aead::open(key_encryption_key, &wrapped.nonce, &aad, &wrapped.ciphertext)
    .map_err(|_| BackupError::UnwrapFailed)?;
  let key = SymmetricKey::from_slice(plaintext.expose_secret()).ok_or(BackupError::UnwrapFailed)?;
  Ok(BackupKey(key))
}

fn ensure_method(expected: WrapMethod, wrapped: &WrappedBackupKey) -> Result<(), BackupError> {
  if wrapped.method == expected {
    Ok(())
  } else {
    Err(BackupError::WrongMethod {
      expected,
      actual: wrapped.method,
    })
  }
}

fn wrap_aad(user_id: UserId, method: WrapMethod, parameters: &[u8]) -> Result<Vec<u8>, BackupError> {
  let aad = WrapAad {
    version: WRAPPED_BACKUP_KEY_VERSION,
    user_id,
    method,
    parameters,
  };
  Ok(domain::labeled(
    domain::BACKUP_KEY_WRAP_AAD,
    &aad.tls_serialize_detached()?,
  ))
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;
  use subtle::ConstantTimeEq;

  use super::*;

  const ALICE: UserId = UserId::new(1);
  /// Cheap settings so tests stay fast; production uses the default.
  const TEST_SETTINGS: Argon2Settings = Argon2Settings {
    memory_kib: 64,
    iterations: 1,
    parallelism: 1,
  };

  #[test]
  fn passphrase_round_trip_through_bytes() {
    let backup_key = BackupKey::generate(&mut OsRng);
    let wrapper = passphrase_wrapper("correct horse battery staple");
    let wrapped = wrapper.wrap_key(ALICE, &backup_key, &mut OsRng).unwrap();
    let decoded = WrappedBackupKey::from_bytes(&wrapped.to_bytes().unwrap()).unwrap();

    let unwrapped = wrapper.unwrap_key(ALICE, &decoded).unwrap();
    assert!(bool::from(unwrapped.key().ct_eq(backup_key.key())));
    assert_eq!(decoded.method(), WrapMethod::Passphrase);
  }

  #[test]
  fn keystore_and_passkey_round_trip() {
    let backup_key = BackupKey::generate(&mut OsRng);
    let prf_output = SecretBytes::new(vec![7u8; 32]);
    let wrappers = [
      SymmetricKeyWrapper::device_keystore(SymmetricKey::generate(&mut OsRng)),
      SymmetricKeyWrapper::passkey_prf(&prf_output).unwrap(),
    ];
    for wrapper in wrappers {
      let wrapped = wrapper.wrap_key(ALICE, &backup_key, &mut OsRng).unwrap();
      let unwrapped = wrapper.unwrap_key(ALICE, &wrapped).unwrap();
      assert!(bool::from(unwrapped.key().ct_eq(backup_key.key())));
    }
    // The same PRF output derives the same wrapping key on another device.
    let wrapped = SymmetricKeyWrapper::passkey_prf(&prf_output)
      .unwrap()
      .wrap_key(ALICE, &backup_key, &mut OsRng)
      .unwrap();
    SymmetricKeyWrapper::passkey_prf(&prf_output)
      .unwrap()
      .unwrap_key(ALICE, &wrapped)
      .unwrap();
  }

  #[test]
  fn default_settings_match_design() {
    let settings = Argon2Settings::default();
    assert_eq!(settings.memory_kib, 131_072);
    settings.validate().unwrap();
  }

  #[test]
  fn debug_hides_passphrase() {
    let wrapper = passphrase_wrapper("hunter2");
    assert!(!format!("{wrapper:?}").contains("hunter2"));
  }

  mod rejection {
    use super::*;

    #[test]
    fn rejects_wrong_passphrase() {
      let backup_key = BackupKey::generate(&mut OsRng);
      let wrapped = passphrase_wrapper("right")
        .wrap_key(ALICE, &backup_key, &mut OsRng)
        .unwrap();
      assert!(matches!(
        passphrase_wrapper("wrong").unwrap_key(ALICE, &wrapped),
        Err(BackupError::UnwrapFailed)
      ));
    }

    #[test]
    fn rejects_other_user_and_wrong_method() {
      let backup_key = BackupKey::generate(&mut OsRng);
      let keystore = SymmetricKeyWrapper::device_keystore(SymmetricKey::generate(&mut OsRng));
      let wrapped = keystore.wrap_key(ALICE, &backup_key, &mut OsRng).unwrap();

      assert!(matches!(
        keystore.unwrap_key(UserId::new(2), &wrapped),
        Err(BackupError::UnwrapFailed)
      ));
      assert!(matches!(
        passphrase_wrapper("x").unwrap_key(ALICE, &wrapped),
        Err(BackupError::WrongMethod {
          expected: WrapMethod::Passphrase,
          actual: WrapMethod::DeviceKeystore
        })
      ));
    }

    #[test]
    fn rejects_tampered_parameters_and_ciphertext() {
      let backup_key = BackupKey::generate(&mut OsRng);
      let wrapper = passphrase_wrapper("pass");
      let wrapped = wrapper.wrap_key(ALICE, &backup_key, &mut OsRng).unwrap();

      // Changing the salt (last parameter bytes) changes the derived key and the AAD.
      let mut salt_changed = wrapped.clone();
      let last = salt_changed.parameters.len() - 1;
      salt_changed.parameters[last] ^= 1;
      let mut body_changed = wrapped.clone();
      body_changed.ciphertext[0] ^= 1;
      let mut method_changed = wrapped.clone();
      method_changed.method = WrapMethod::PasskeyPrf;

      assert!(matches!(
        wrapper.unwrap_key(ALICE, &salt_changed),
        Err(BackupError::UnwrapFailed)
      ));
      assert!(matches!(
        wrapper.unwrap_key(ALICE, &body_changed),
        Err(BackupError::UnwrapFailed)
      ));
      assert!(matches!(
        wrapper.unwrap_key(ALICE, &method_changed),
        Err(BackupError::WrongMethod { .. })
      ));
    }

    #[test]
    fn rejects_hostile_argon2_settings_from_server() {
      let backup_key = BackupKey::generate(&mut OsRng);
      let wrapper = passphrase_wrapper("pass");
      let mut wrapped = wrapper.wrap_key(ALICE, &backup_key, &mut OsRng).unwrap();
      let mut parameters = PassphraseParameters::tls_deserialize_exact_bytes(&wrapped.parameters).unwrap();
      parameters.settings.memory_kib = u32::MAX;
      wrapped.parameters = parameters.tls_serialize_detached().unwrap();

      assert!(matches!(
        wrapper.unwrap_key(ALICE, &wrapped),
        Err(BackupError::InvalidArgon2Settings(_))
      ));
    }

    #[test]
    fn rejects_invalid_local_settings_and_short_prf() {
      let too_many = Argon2Settings {
        iterations: MAX_ARGON2_ITERATIONS + 1,
        ..TEST_SETTINGS
      };
      let no_memory = Argon2Settings {
        memory_kib: 0,
        ..TEST_SETTINGS
      };
      for settings in [too_many, no_memory] {
        assert!(PassphraseWrapper::new(SecretBytes::new(b"x".to_vec()), settings).is_err());
      }
      assert!(matches!(
        SymmetricKeyWrapper::passkey_prf(&SecretBytes::new(vec![1; 16])),
        Err(BackupError::PrfOutputTooShort)
      ));
    }

    #[test]
    fn rejects_malformed_bytes() {
      assert!(WrappedBackupKey::from_bytes(&[]).is_err());
      assert!(WrappedBackupKey::from_bytes(&[1, 9]).is_err());
    }
  }

  fn passphrase_wrapper(passphrase: &str) -> PassphraseWrapper {
    PassphraseWrapper::new(SecretBytes::new(passphrase.as_bytes().to_vec()), TEST_SETTINGS).unwrap()
  }
}
