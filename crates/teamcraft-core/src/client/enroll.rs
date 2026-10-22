//! First run on a device (e2ee.md §3, §6): create or recover the identity, register the device,
//! publish key packages and back up the key bag.
//!
//! Two steps, so the app can get the identity key attested by the identity provider in between
//! (e2ee.md §3.1): [`PendingEnrollment::fingerprint`] is written to the OIDC user attribute
//! (see [`crate::attestation`]), the refreshed ID token comes back as `maybe_attestation`.

use std::collections::HashMap;

use rand_core::OsRng;
use teamcraft_crypto::backup::{
  Argon2Settings, BackupKey, BackupKeyWrapper, PassphraseWrapper, WrapMethod, WrappedBackupKey,
};
use teamcraft_crypto::identity::{DeviceKeyPair, IdentityKeyPair};
use teamcraft_crypto::key_bag::{EncryptedKeyBag, KeyBag, KeyBagBinding};
use teamcraft_crypto::org_recovery::{RecoveryRequest, RecoveryRequestKey};
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use teamcraft_proto::{
  Base64Bytes, PutKeyBagRequest, RegisterDeviceRequest, UploadIdentityKeyRequest, UserId,
};

use super::{Client, ClientSettings};
use crate::api::ServerApi;
use crate::clock::Clock;
use crate::convert;
use crate::error::CoreError;
use crate::records::{self, Account, SyncState};
use crate::store::{LocalStore, Table};

/// Device ID used in the provisional certificate sent at registration, before the server has
/// assigned the real one (crate README: server gaps).
const PROVISIONAL_DEVICE_ID: u64 = 0;

pub struct EnrollmentOptions {
  /// Shown in the user's device list ("Firefox on Linux").
  pub device_name: String,
  /// How this device gets (or creates) the account's keys.
  pub secret: EnrollmentSecret,
  pub argon2: Argon2Settings,
  /// The ID token carrying the identity key fingerprint as claim `tc_identity_key`, if the
  /// identity provider supports it.
  pub maybe_attestation: Option<String>,
}

/// Where the account's keys come from. Only a passphrase enrollment may create a new identity:
/// linked and recovering devices fail closed when the server claims the account has none
/// (security review B1).
pub enum EnrollmentSecret {
  /// Recovery passphrase (e2ee.md §6.2 fallback): wraps the backup key for a new identity, unwraps
  /// it when recovering an existing one.
  Passphrase(SecretBytes),
  /// Secrets received from an existing device by linking (e2ee.md §5).
  Linked(super::LinkedSecrets),
  /// Recovering through the organization (e2ee.md §6.3.1); `new_passphrase` wraps the fresh backup
  /// key the device switches to, since the admins saw the old one.
  OrgRecovery {
    unlock: OrgRecoveryUnlock,
    new_passphrase: SecretBytes,
  },
}

/// What a recovering device received from the combining admin.
pub struct OrgRecoveryUnlock {
  /// The one-time key whose public part the admins sealed to.
  pub key: RecoveryRequestKey,
  /// The request this device made (its code was compared with the admin).
  pub request: RecoveryRequest,
  /// The user's backup key, sealed to the request.
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

  /// Hex fingerprint of the identity key.
  pub fn fingerprint(&self) -> String {
    self.identity.public_key().fingerprint().to_string()
  }

  /// The value for the identity provider's `tc_identity_key` attribute: binds this user's ID and the
  /// identity key (security review A1).
  pub fn attestation_claim(&self, user: UserId) -> String {
    teamcraft_crypto::attestation::attestation_claim(convert::user(user), &self.identity.public_key())
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
    let (recovered, is_new_identity) = match existing {
      None if !matches!(options.secret, EnrollmentSecret::Passphrase(_)) => {
        return Err(CoreError::IdentityMissing);
      },
      None => (
        Recovered {
          key_bag: KeyBag::new(convert::user(me.id), self.identity),
          backup_secret: SymmetricKey::generate(&mut OsRng),
          version: 0,
          sequence: 0,
          wrapped_keys: Vec::new(),
        },
        true,
      ),
      Some(existing) => {
        let recovered = recover_key_bag(&api, me.id, &options).await?;
        if recovered.key_bag.identity().public_key().as_bytes().as_slice() != existing.public_key.as_slice() {
          return Err(CoreError::IdentityMismatch);
        }
        (recovered, false)
      },
    };
    let Recovered {
      key_bag,
      backup_secret,
      version: key_bag_version,
      sequence: key_bag_sequence,
      wrapped_keys,
    } = recovered;

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
      key_bag_sequence,
      ..SyncState::default()
    };
    records::save_sync_state(&store, &sync)?;
    if !is_new_identity {
      records::put_json(
        &store,
        Table::SyncState,
        WRAPPED_KEYS_KEY,
        &Base64Bytes::new(wrapped_keys),
      )?;
    }
    let maybe_preview_fetcher = super::previews::default_fetcher(&settings.link_previews);
    let maybe_jwks_source = super::attested::default_jwks_source(settings.maybe_attestation.as_ref());
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
      maybe_jwks_source,
    };
    match &options.secret {
      EnrollmentSecret::Passphrase(passphrase) if is_new_identity => {
        client.upload_key_bag_with(passphrase, options.argon2).await?;
      },
      // The admins who recovered the account saw its backup key (security review B5).
      EnrollmentSecret::OrgRecovery { new_passphrase, .. } => {
        client.rotate_backup_key(new_passphrase, options.argon2).await?;
      },
      _ => {},
    }
    client.top_up_key_packages().await?;
    client.publish_device_list(&[]).await?;
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
  /// Encrypts the key bag and uploads it, wrapping the backup key with `passphrase` (replacing an
  /// earlier passphrase wrap; the other recovery factors stay).
  pub async fn upload_key_bag_with(
    &mut self,
    passphrase: &SecretBytes,
    argon2: Argon2Settings,
  ) -> Result<(), CoreError> {
    let wrapped = self.passphrase_wrap(passphrase, argon2)?;
    let mut keys: Vec<WrappedBackupKey> = self
      .current_wrapped_keys()
      .await?
      .into_iter()
      .filter(|existing| existing.method() != WrapMethod::Passphrase)
      .collect();
    keys.push(wrapped);
    self.store_wrapped_keys(&keys)?;
    self.upload_key_bag_keeping(WrapsOnMerge::KeepOwn).await
  }

  /// Switches the account to a fresh backup key, after a recovery in which others saw the old one
  /// (e2ee.md §6.3.1). The key bag is re-encrypted and wrapped with `passphrase` and, if the
  /// workspace has one, the current org recovery key; other factors (passkeys) must be enrolled
  /// again, and the user's other devices link again.
  pub async fn rotate_backup_key(
    &mut self,
    passphrase: &SecretBytes,
    argon2: Argon2Settings,
  ) -> Result<(), CoreError> {
    self.account.backup_secret = SymmetricKey::generate(&mut OsRng);
    records::save_account(&self.store, &self.account)?;
    self.sync.is_key_bag_dirty = true;
    self.persist_sync()?;
    let mut keys = vec![self.passphrase_wrap(passphrase, argon2)?];
    self.sync_workspace().await?;
    if let Some(org_wrap) = self.org_recovery_wrap()? {
      keys.push(org_wrap);
    }
    self.store_wrapped_keys(&keys)?;
    self.upload_key_bag_keeping(WrapsOnMerge::KeepOwn).await
  }

  fn passphrase_wrap(
    &self,
    passphrase: &SecretBytes,
    argon2: Argon2Settings,
  ) -> Result<WrappedBackupKey, CoreError> {
    let wrapper = PassphraseWrapper::new(SecretBytes::new(passphrase.expose_secret().to_vec()), argon2)?;
    Ok(wrapper.wrap_key(
      convert::user(self.account.user_id),
      &self.account.backup_key(),
      &mut OsRng,
    )?)
  }

  /// The wrapped backup keys this device uploads, decoded.
  pub(crate) async fn current_wrapped_keys(&mut self) -> Result<Vec<WrappedBackupKey>, CoreError> {
    let blob = self.wrapped_keys_blob().await?;
    decode_wrapped_keys(&blob)
  }

  /// The wrapped backup keys this device uploads next to the key bag: the stored ones, else the
  /// server's, taken only after the bag stored with them opened (they are bound to it, B11), else
  /// none (new identity).
  pub(crate) async fn wrapped_keys_blob(&mut self) -> Result<Vec<u8>, CoreError> {
    if let Some(stored) = records::get_json::<Base64Bytes>(&self.store, Table::SyncState, WRAPPED_KEYS_KEY)? {
      return Ok(stored.into_vec());
    }
    match self.api.key_bag().await {
      Ok(current) => {
        self.merge_key_bag(
          current.encrypted_key_bag.as_slice(),
          current.version,
          current.wrapped_backup_keys.as_slice(),
          WrapsOnMerge::Adopt,
        )?;
        Ok(current.wrapped_backup_keys.into_vec())
      },
      Err(error) if error.is_not_found() => encode_wrapped_keys(&[]),
      Err(error) => Err(error.into()),
    }
  }

  pub(crate) fn store_wrapped_keys(&self, keys: &[WrappedBackupKey]) -> Result<(), CoreError> {
    self.store_wrapped_keys_blob(&encode_wrapped_keys(keys)?)
  }

  pub(crate) fn store_wrapped_keys_blob(&self, blob: &[u8]) -> Result<(), CoreError> {
    records::put_json(
      &self.store,
      Table::SyncState,
      WRAPPED_KEYS_KEY,
      &Base64Bytes::new(blob.to_vec()),
    )
  }

  /// Uploads the current key bag (after content keys changed). Another device's newer upload wins
  /// and is merged first, including its wrapped keys (it may have changed the passphrase).
  pub async fn upload_key_bag(&mut self) -> Result<(), CoreError> {
    self.upload_key_bag_keeping(WrapsOnMerge::Adopt).await
  }

  /// Uploads with this device's wraps (after it changed them) or the newest ones on a conflict.
  pub(crate) async fn upload_key_bag_keeping(&mut self, on_conflict: WrapsOnMerge) -> Result<(), CoreError> {
    let request = self.key_bag_request().await?;
    let stored = match self.api.put_key_bag(&request.put).await {
      Ok(stored) => stored,
      Err(error) if error.is_version_conflict() => {
        // Another device of the user uploaded meanwhile: merge its content keys, then retry once.
        let remote = self.api.key_bag().await?;
        self.merge_key_bag(
          remote.encrypted_key_bag.as_slice(),
          remote.version,
          remote.wrapped_backup_keys.as_slice(),
          on_conflict,
        )?;
        let retry = self.key_bag_request().await?;
        let stored = self.api.put_key_bag(&retry.put).await?;
        self.sync.key_bag_sequence = retry.sequence;
        stored
      },
      Err(error) => return Err(error.into()),
    };
    if self.sync.key_bag_sequence < request.sequence {
      self.sync.key_bag_sequence = request.sequence;
    }
    self.sync.key_bag_version = stored.version;
    self.sync.is_key_bag_dirty = false;
    self.persist_sync()
  }

  /// The next upload: a sequence above every bag this device saw, bound to the wraps it sends.
  async fn key_bag_request(&mut self) -> Result<KeyBagUpload, CoreError> {
    let wrapped_keys = self.wrapped_keys_blob().await?;
    let sequence = self.sync.key_bag_sequence + 1;
    let encrypted = self.account.key_bag.encrypt(
      &self.account.backup_key(),
      KeyBagBinding {
        sequence,
        wrapped_keys: &wrapped_keys,
      },
      &mut OsRng,
    )?;
    Ok(KeyBagUpload {
      sequence,
      put: PutKeyBagRequest {
        expected_version: self.sync.key_bag_version,
        encrypted_key_bag: Base64Bytes::new(encrypted.to_bytes()?),
        wrapped_backup_keys: Base64Bytes::new(wrapped_keys),
      },
    })
  }
}

/// Whether merging another device's key bag takes over the wrapped keys stored with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WrapsOnMerge {
  Adopt,
  /// This device just changed its wraps and is uploading them.
  KeepOwn,
}

struct KeyBagUpload {
  sequence: u64,
  put: PutKeyBagRequest,
}

pub(crate) const WRAPPED_KEYS_KEY: &str = "wrapped_backup_keys";

/// A key bag opened during enrollment, with what it is bound to.
struct Recovered {
  key_bag: KeyBag,
  backup_secret: SymmetricKey,
  version: u64,
  sequence: u64,
  /// The wrapped backup keys the bag opened with (so they are the uploader's).
  wrapped_keys: Vec<u8>,
}

async fn recover_key_bag(
  api: &impl ServerApi,
  user: teamcraft_proto::UserId,
  options: &EnrollmentOptions,
) -> Result<Recovered, CoreError> {
  let crypto_user = convert::user(user);
  let passphrase = match &options.secret {
    EnrollmentSecret::Linked(linked) => {
      let backup_key = BackupKey::from_key(linked.backup_secret.duplicate());
      let encrypted = EncryptedKeyBag::from_bytes(&linked.encrypted_key_bag)?;
      let key_bag = encrypted.decrypt(crypto_user, &backup_key, &linked.wrapped_keys)?;
      return Ok(Recovered {
        key_bag,
        backup_secret: backup_key.into_key(),
        version: linked.key_bag_version,
        sequence: encrypted.sequence(),
        wrapped_keys: linked.wrapped_keys.clone(),
      });
    },
    EnrollmentSecret::OrgRecovery { unlock, .. } => {
      if unlock.request.user_id() != crypto_user {
        return Err(CoreError::Corrupt(
          "the recovery request is for another user".into(),
        ));
      }
      let backup_key = unlock
        .key
        .open_backup_key(&unlock.request, &unlock.sealed_backup_key)
        .map_err(|error| CoreError::Corrupt(error.to_string()))?;
      return open_stored(api, crypto_user, backup_key).await;
    },
    EnrollmentSecret::Passphrase(passphrase) => passphrase,
  };
  let stored = api.key_bag().await?;
  let wrapper = PassphraseWrapper::new(
    SecretBytes::new(passphrase.expose_secret().to_vec()),
    options.argon2,
  )?;
  let mut last_error = None;
  for wrapped in decode_wrapped_keys(stored.wrapped_backup_keys.as_slice())? {
    if wrapped.method() != wrapper.method() {
      continue;
    }
    match wrapper.unwrap_key(crypto_user, &wrapped) {
      Ok(backup_key) => return open_stored(api, crypto_user, backup_key).await,
      Err(error) => last_error = Some(error),
    }
  }
  Err(last_error.map_or(CoreError::NotEnrolled, CoreError::from))
}

/// Opens the key bag the server stores, checking it against the wraps stored next to it.
async fn open_stored(
  api: &impl ServerApi,
  user: teamcraft_crypto::ids::UserId,
  backup_key: BackupKey,
) -> Result<Recovered, CoreError> {
  let stored = api.key_bag().await?;
  let encrypted = EncryptedKeyBag::from_bytes(stored.encrypted_key_bag.as_slice())?;
  let key_bag = encrypted.decrypt(user, &backup_key, stored.wrapped_backup_keys.as_slice())?;
  Ok(Recovered {
    key_bag,
    backup_secret: backup_key.into_key(),
    version: stored.version,
    sequence: encrypted.sequence(),
    wrapped_keys: stored.wrapped_backup_keys.into_vec(),
  })
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
