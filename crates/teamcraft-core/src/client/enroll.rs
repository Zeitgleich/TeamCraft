//! First run on a device (e2ee.md §3, §6): create or recover the identity, register the device,
//! publish key packages and back up the key bag.
//!
//! Two steps, so the app can get the identity key attested by the identity provider in between
//! (e2ee.md §3.1): [`PendingEnrollment::fingerprint`] is written to the OIDC user attribute
//! (see [`crate::attestation`]), the refreshed ID token comes back as `maybe_attestation`.

use std::collections::HashMap;

use rand_core::OsRng;
use teamcraft_crypto::backup::{
  Argon2Settings, BackupKey, BackupKeyWrapper, PassphraseWrapper, WrappedBackupKey,
};
use teamcraft_crypto::identity::{DeviceKeyPair, IdentityKeyPair};
use teamcraft_crypto::key_bag::{EncryptedKeyBag, KeyBag};
use teamcraft_crypto::org_recovery::RecoveryRequestKey;
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use teamcraft_proto::{Base64Bytes, PutKeyBagRequest, RegisterDeviceRequest, UploadIdentityKeyRequest};

use super::{Client, ClientSettings};
use crate::api::ServerApi;
use crate::clock::Clock;
use crate::convert;
use crate::error::CoreError;
use crate::records::{self, Account, SyncState};
use crate::store::LocalStore;

/// Device ID used in the provisional certificate sent at registration, before the server has
/// assigned the real one (crate README: server gaps).
const PROVISIONAL_DEVICE_ID: u64 = 0;

pub struct EnrollmentOptions {
  /// Shown in the user's device list ("Firefox on Linux").
  pub device_name: String,
  /// Recovery passphrase (e2ee.md §6.2 fallback): wraps the backup key for a new identity, unwraps
  /// it when recovering an existing one.
  pub passphrase: SecretBytes,
  pub argon2: Argon2Settings,
  /// The ID token carrying the identity key fingerprint as claim `tc_identity_key`, if the
  /// identity provider supports it.
  pub maybe_attestation: Option<String>,
  /// Recovering through the organization (e2ee.md §6.3.1) instead of the passphrase.
  pub maybe_org_recovery: Option<OrgRecoveryUnlock>,
  /// Secrets received from an existing device by linking (e2ee.md §5).
  pub maybe_linked: Option<super::LinkedSecrets>,
}

/// What a recovering device received from the combining admin.
pub struct OrgRecoveryUnlock {
  /// The one-time key whose public part the admins sealed to.
  pub request: RecoveryRequestKey,
  /// The user's backup key, sealed to the request key.
  pub sealed_backup_key: Vec<u8>,
}

/// Identity material created (or recovered) before the device is registered.
pub struct PendingEnrollment {
  identity: IdentityKeyPair,
}

impl PendingEnrollment {
  /// Starts enrolling a new identity.
  pub fn new() -> Self {
    Self {
      identity: IdentityKeyPair::generate(&mut OsRng),
    }
  }

  /// Hex fingerprint of the identity key, for the identity provider's `tc_identity_key` attribute.
  pub fn fingerprint(&self) -> String {
    self.identity.public_key().fingerprint().to_string()
  }

  /// Registers this device and publishes everything. If the account already has an identity key
  /// on the server, the existing identity is recovered from the key bag with the passphrase
  /// instead (the pending identity is discarded).
  pub async fn complete<A: ServerApi, S: LocalStore>(
    self,
    api: A,
    store: S,
    clock: Box<dyn Clock>,
    settings: ClientSettings,
    options: EnrollmentOptions,
  ) -> Result<Client<A, S>, CoreError> {
    let me = api.me().await?;
    let existing = match api.identity_key(me.id).await {
      Ok(existing) => Some(existing),
      Err(error) if error.is_not_found() => None,
      Err(error) => return Err(error.into()),
    };
    let (key_bag, backup_secret, key_bag_version, is_new_identity) = match existing {
      None => {
        let backup_secret = SymmetricKey::generate(&mut OsRng);
        (
          KeyBag::new(convert::user(me.id), self.identity),
          backup_secret,
          0,
          true,
        )
      },
      Some(existing) => {
        let (key_bag, backup_secret, version) = recover_key_bag(&api, me.id, &options).await?;
        if key_bag.identity().public_key().as_bytes().as_slice() != existing.public_key.as_slice() {
          return Err(CoreError::IdentityMismatch);
        }
        (key_bag, backup_secret, version, false)
      },
    };

    let (device_id, device, certificate) = register_device(&api, me.id, key_bag.identity(), &options).await?;
    let account = Account {
      user_id: me.id,
      device_id,
      device,
      certificate,
      backup_secret,
      key_bag,
    };
    if is_new_identity {
      api
        .put_identity_key(&UploadIdentityKeyRequest {
          public_key: Base64Bytes::new(account.identity_public_key().as_bytes().to_vec()),
          // Proof of possession: the identity key's signature over this device's certificate.
          signature: Base64Bytes::new(account.certificate.to_bytes()?),
          maybe_attestation: options.maybe_attestation.clone(),
          replace: false,
        })
        .await
        .map_err(|error| match error.error_code() {
          // Another device of this user enrolled first, between our check and now: the caller
          // enrolls again, which then recovers that identity.
          Some(teamcraft_proto::ErrorCode::Conflict) => CoreError::AlreadyEnrolled,
          _ => CoreError::from(error),
        })?;
    }
    records::save_account(&store, &account)?;
    let sync = SyncState {
      key_bag_version,
      ..SyncState::default()
    };
    records::save_sync_state(&store, &sync)?;
    let maybe_preview_fetcher = super::previews::default_fetcher(&settings.link_previews);
    let mut client = Client {
      api,
      store,
      clock,
      settings,
      provider: Default::default(),
      account,
      groups: HashMap::new(),
      sync,
      typing: Default::default(),
      maybe_call_sink: None,
      maybe_preview_fetcher,
    };
    if is_new_identity {
      client
        .upload_key_bag_with(&options.passphrase, options.argon2)
        .await?;
    }
    client.top_up_key_packages().await?;
    Ok(client)
  }
}

impl Default for PendingEnrollment {
  fn default() -> Self {
    Self::new()
  }
}

impl std::fmt::Debug for PendingEnrollment {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("PendingEnrollment")
      .field("fingerprint", &self.fingerprint())
      .finish()
  }
}

impl std::fmt::Debug for EnrollmentOptions {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("EnrollmentOptions")
      .field("device_name", &self.device_name)
      .finish_non_exhaustive()
  }
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Encrypts the key bag and uploads it, wrapping the backup key with `passphrase`.
  pub async fn upload_key_bag_with(
    &mut self,
    passphrase: &SecretBytes,
    argon2: Argon2Settings,
  ) -> Result<(), CoreError> {
    let wrapper = PassphraseWrapper::new(SecretBytes::new(passphrase.expose_secret().to_vec()), argon2)?;
    let wrapped = wrapper.wrap_key(
      convert::user(self.account.user_id),
      &self.account.backup_key(),
      &mut OsRng,
    )?;
    let wrapped_backup_keys = encode_wrapped_keys(&[wrapped])?;
    records::put_json(
      &self.store,
      crate::store::Table::SyncState,
      WRAPPED_KEYS_KEY,
      &Base64Bytes::new(wrapped_backup_keys),
    )?;
    self.upload_key_bag().await
  }

  /// Uploads the current key bag (after content keys changed). Uses the wrapped backup keys stored
  /// at enrollment; another device's newer upload wins and is not overwritten blindly.
  pub async fn upload_key_bag(&mut self) -> Result<(), CoreError> {
    let Some(wrapped_backup_keys) =
      records::get_json::<Base64Bytes>(&self.store, crate::store::Table::SyncState, WRAPPED_KEYS_KEY)?
    else {
      // Recovered devices reuse the server's wrapped keys; fetch them on first upload.
      let current = self.api.key_bag().await?;
      records::put_json(
        &self.store,
        crate::store::Table::SyncState,
        WRAPPED_KEYS_KEY,
        &current.wrapped_backup_keys,
      )?;
      return Box::pin(self.upload_key_bag()).await;
    };
    let encrypted = self
      .account
      .key_bag
      .encrypt(&self.account.backup_key(), &mut OsRng)?;
    let request = PutKeyBagRequest {
      expected_version: self.sync.key_bag_version,
      encrypted_key_bag: Base64Bytes::new(encrypted.to_bytes()?),
      wrapped_backup_keys,
    };
    match self.api.put_key_bag(&request).await {
      Ok(stored) => {
        self.sync.key_bag_version = stored.version;
        self.sync.is_key_bag_dirty = false;
        self.persist_sync()
      },
      Err(error) if error.is_version_conflict() => {
        // Another device of the user uploaded meanwhile: merge its content keys, then retry once.
        self.merge_remote_key_bag().await?;
        let request = PutKeyBagRequest {
          expected_version: self.sync.key_bag_version,
          encrypted_key_bag: Base64Bytes::new(
            self
              .account
              .key_bag
              .encrypt(&self.account.backup_key(), &mut OsRng)?
              .to_bytes()?,
          ),
          wrapped_backup_keys: request.wrapped_backup_keys,
        };
        let stored = self.api.put_key_bag(&request).await?;
        self.sync.key_bag_version = stored.version;
        self.sync.is_key_bag_dirty = false;
        self.persist_sync()
      },
      Err(error) => Err(error.into()),
    }
  }

  async fn merge_remote_key_bag(&mut self) -> Result<(), CoreError> {
    let remote = self.api.key_bag().await?;
    self.merge_key_bag(remote.encrypted_key_bag.as_slice(), remote.version)
  }
}

pub(crate) const WRAPPED_KEYS_KEY: &str = "wrapped_backup_keys";

async fn recover_key_bag(
  api: &impl ServerApi,
  user: teamcraft_proto::UserId,
  options: &EnrollmentOptions,
) -> Result<(KeyBag, SymmetricKey, u64), CoreError> {
  let crypto_user = convert::user(user);
  if let Some(linked) = &options.maybe_linked {
    let backup_key = BackupKey::from_key(linked.backup_secret.duplicate());
    let key_bag =
      EncryptedKeyBag::from_bytes(&linked.encrypted_key_bag)?.decrypt(crypto_user, &backup_key)?;
    return Ok((key_bag, backup_key.into_key(), linked.key_bag_version));
  }
  let stored = api.key_bag().await?;
  if let Some(unlock) = &options.maybe_org_recovery {
    let backup_key = unlock
      .request
      .open_backup_key(crypto_user, &unlock.sealed_backup_key)
      .map_err(|error| CoreError::Corrupt(error.to_string()))?;
    let key_bag =
      EncryptedKeyBag::from_bytes(stored.encrypted_key_bag.as_slice())?.decrypt(crypto_user, &backup_key)?;
    return Ok((key_bag, backup_key.into_key(), stored.version));
  }
  let wrapper = PassphraseWrapper::new(
    SecretBytes::new(options.passphrase.expose_secret().to_vec()),
    options.argon2,
  )?;
  let mut last_error = None;
  for wrapped in decode_wrapped_keys(stored.wrapped_backup_keys.as_slice())? {
    if wrapped.method() != wrapper.method() {
      continue;
    }
    match wrapper.unwrap_key(crypto_user, &wrapped) {
      Ok(backup_key) => {
        let key_bag = EncryptedKeyBag::from_bytes(stored.encrypted_key_bag.as_slice())?
          .decrypt(crypto_user, &backup_key)?;
        return Ok((key_bag, backup_key.into_key(), stored.version));
      },
      Err(error) => last_error = Some(error),
    }
  }
  Err(last_error.map_or(CoreError::NotEnrolled, CoreError::from))
}

async fn register_device(
  api: &impl ServerApi,
  user: teamcraft_proto::UserId,
  identity: &IdentityKeyPair,
  options: &EnrollmentOptions,
) -> Result<
  (
    teamcraft_proto::DeviceId,
    DeviceKeyPair,
    teamcraft_crypto::identity::SignedDeviceCertificate,
  ),
  CoreError,
> {
  let crypto_user = convert::user(user);
  let provisional = DeviceKeyPair::generate(
    crypto_user,
    teamcraft_crypto::ids::DeviceId::new(PROVISIONAL_DEVICE_ID),
    &mut OsRng,
  );
  let public = provisional.public_keys();
  let mut public_key = public.signing_key.as_bytes().to_vec();
  public_key.extend_from_slice(public.agreement_key.as_bytes());
  // The server assigns the device ID, so the certificate sent at registration can't contain it
  // yet. The final certificate (with the ID) is what MLS credentials carry.
  let provisional_certificate = identity.certify_device(crypto_user, &public)?;
  let registered = api
    .register_device(&RegisterDeviceRequest {
      name: options.device_name.clone(),
      public_key: Base64Bytes::new(public_key),
      cross_signature: Base64Bytes::new(provisional_certificate.to_bytes()?),
    })
    .await?;
  let (signing, agreement) = provisional.to_secrets();
  let device = DeviceKeyPair::from_secrets(crypto_user, convert::device(registered.id), &signing, &agreement);
  let certificate = identity.certify_device(crypto_user, &device.public_keys())?;
  Ok((registered.id, device, certificate))
}

/// The server stores one opaque blob of wrapped backup keys; it holds a list (one per method).
pub(crate) fn encode_wrapped_keys(wrapped: &[WrappedBackupKey]) -> Result<Vec<u8>, CoreError> {
  let mut out =
    vec![u8::try_from(wrapped.len()).map_err(|_| CoreError::Corrupt("too many wrapped keys".into()))?];
  for key in wrapped {
    let bytes = key.to_bytes()?;
    let length =
      u32::try_from(bytes.len()).map_err(|_| CoreError::Corrupt("wrapped key too large".into()))?;
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(&bytes);
  }
  Ok(out)
}

pub(crate) fn decode_wrapped_keys(bytes: &[u8]) -> Result<Vec<WrappedBackupKey>, CoreError> {
  let corrupt = || CoreError::Corrupt("malformed wrapped backup keys".into());
  let (&count, mut rest) = bytes.split_first().ok_or_else(corrupt)?;
  let mut keys = Vec::with_capacity(usize::from(count));
  for _ in 0..count {
    let (length, tail) = rest.split_first_chunk::<4>().ok_or_else(corrupt)?;
    let length = u32::from_be_bytes(*length) as usize;
    let chunk = tail.get(..length).ok_or_else(corrupt)?;
    keys.push(WrappedBackupKey::from_bytes(chunk)?);
    rest = &tail[length..];
  }
  if !rest.is_empty() {
    return Err(corrupt());
  }
  Ok(keys)
}
