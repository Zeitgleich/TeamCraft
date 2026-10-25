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
use teamcraft_crypto::identity::{DeviceKeyPair, IdentityKeyPair, SignedDeviceCertificate};
use teamcraft_crypto::key_bag::{EncryptedKeyBag, KeyBag, KeyBagBinding};
use teamcraft_crypto::org_recovery::{RecoveryRequest, RecoveryRequestKey};
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use teamcraft_proto::{
  Base64Bytes, DeviceId, InitialKeyBag, PutKeyBagRequest, RegisterDeviceRequest, UploadIdentityKeyRequest,
  UserId,
};

use super::{Client, ClientSettings};
use crate::api::ServerApi;
use crate::clock::Clock;
use crate::convert;
use crate::error::CoreError;
use crate::records::{self, Account, PendingDevice, SyncState};
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
  ///
  /// Safe to run again after any failure, also after a restart (e2ee.md §3.2): a new identity
  /// reaches the server only together with its passphrase-wrapped key bag, the device key pair is
  /// stored before it is registered so a retry reuses (or revokes) that registration, and the
  /// account is stored only once the server holds everything needed to recover it. What remains
  /// after that (key packages, device list) [`Client::finish_enrollment`] completes on the next
  /// sync.
  pub async fn complete<A: ServerApi, S: LocalStore>(
    self,
    api: A,
    store: S,
    clock: Box<dyn Clock>,
    settings: ClientSettings,
    options: EnrollmentOptions,
  ) -> Result<Client<A, S>, CoreError> {
    let me = api.me().await?;
    let existing = api.identity_key(me.id).await?;
    let is_new_identity = existing.is_none();
    let mut recovered = match existing {
      None => match &options.secret {
        EnrollmentSecret::Passphrase(passphrase) => {
          new_identity(me.id, self.identity, passphrase, options.argon2)?
        },
        _ => return Err(CoreError::IdentityMissing),
      },
      Some(existing) => {
        let recovered = recover_key_bag(&api, me.id, &options).await?;
        if recovered.key_bag.identity().public_key().as_bytes().as_slice() != existing.public_key.as_slice() {
          return Err(CoreError::IdentityMismatch);
        }
        recovered
      },
    };

    let (device_id, device, certificate) =
      ensure_device(&api, &store, me.id, recovered.key_bag.identity(), &options).await?;
    if is_new_identity {
      publish_identity(&api, &mut recovered, &certificate, &options).await?;
    }
    let Recovered {
      key_bag,
      backup_secret,
      version: key_bag_version,
      sequence: key_bag_sequence,
      wrapped_keys,
    } = recovered;
    let account = Account {
      user_id: me.id,
      device_id,
      device,
      certificate,
      backup_secret,
      key_bag,
    };
    let sync = SyncState {
      key_bag_version,
      key_bag_sequence,
      is_enrollment_unfinished: true,
      ..SyncState::default()
    };
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
      background_failures: Vec::new(),
      maybe_call_sink: None,
      maybe_preview_fetcher,
      maybe_jwks_source,
      last_key_package_check_ms: i64::MIN,
      mls_current: Default::default(),
      maybe_replay_left: None,
      member_join_attempts: HashMap::new(),
      device_add_requests_handled: HashMap::new(),
      are_welcomes_synced: false,
      maybe_waiting_checked_at_ms: None,
      catch_up_focus: super::CatchUpFocus::default(),
      is_welcome_cursor_held: false,
    };
    client.persist_sync()?;
    client.store_wrapped_keys_blob(&wrapped_keys)?;
    // The admins who recovered the account saw its backup key (security review B5). Before the
    // account is stored: interrupted, the device recovers again (with the new passphrase if the
    // new bag reached the server).
    if let EnrollmentSecret::OrgRecovery { new_passphrase, .. } = &options.secret {
      client.rotate_backup_key(new_passphrase, options.argon2).await?;
    }
    // A key bag from the link is as fresh as the user's other device; one the server served may not
    // be (e2ee.md §4.6, joining by external commit).
    let is_linked = matches!(options.secret, EnrollmentSecret::Linked(_));
    // One group of writes: the account and its key bag replace the pending device and the
    // secrets of a link together (issue #160).
    crate::store::write_group(&client.store, || {
      records::put_json(
        &client.store,
        Table::Account,
        super::member_device::KEY_BAG_FROM_LINK_KEY,
        &is_linked,
      )?;
      records::save_account(&client.store, &client.account)?;
      records::delete_pending_device(&client.store)?;
      records::delete_pending_link(&client.store)
    })?;
    // Removal records from the key bag count before the first message is decrypted.
    client.apply_shared_removals()?;
    client.finish_enrollment().await?;
    Ok(client)
  }
}

impl PendingEnrollment {
  /// Finishes an enrollment by linking that stopped after the existing device handed over the keys
  /// (issue #168): the secrets were stored when they arrived. `Ok(None)` if there is none.
  ///
  /// A failure that will repeat (the key bag no longer opens, the account's identity changed, the
  /// secrets belong to another account) drops the stored secrets, so the device asks to be set up
  /// again; anything else (the network) keeps them for the next try.
  pub async fn resume_link<A: ServerApi, S: LocalStore + Clone>(
    api: A,
    store: S,
    clock: Box<dyn Clock>,
    settings: ClientSettings,
    argon2: Argon2Settings,
  ) -> Result<Option<Client<A, S>>, CoreError> {
    let pending = match records::load_pending_link(&store) {
      Ok(Some(pending)) => pending,
      Ok(None) => return Ok(None),
      // An unreadable record would fail the same way on every start.
      Err(CoreError::Corrupt(error)) => {
        tracing::warn!(%error, "dropping the unreadable keys of an interrupted device link");
        records::delete_pending_link(&store)?;
        return Ok(None);
      },
      Err(error) => return Err(error),
    };
    let me = api.me().await?;
    if me.id != pending.user_id {
      tracing::warn!("dropping the keys of a device link for another account");
      records::delete_pending_link(&store)?;
      return Ok(None);
    }
    tracing::info!("finishing the enrollment of a device link that was interrupted");
    let options = EnrollmentOptions {
      device_name: pending.device_name,
      secret: EnrollmentSecret::Linked(pending.secrets),
      argon2,
      maybe_attestation: None,
    };
    match Self::new()
      .complete(api, store.clone(), clock, settings, options)
      .await
    {
      Ok(client) => Ok(Some(client)),
      Err(error) => {
        if is_link_failure_final(&error) {
          tracing::warn!(%error, "the keys of an interrupted device link no longer work; dropping them");
          records::delete_pending_link(&store)?;
        }
        Err(error)
      },
    }
  }
}

/// Whether resuming a link enrollment would fail the same way every time.
fn is_link_failure_final(error: &CoreError) -> bool {
  matches!(
    error,
    CoreError::KeyBag(_)
      | CoreError::Corrupt(_)
      | CoreError::Backup(_)
      | CoreError::IdentityMismatch
      | CoreError::IdentityMissing
      | CoreError::KeyBagRollback { .. }
  )
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
  /// Publishes what an enrollment that stopped after storing the account left out: key packages
  /// and this device in the signed device list. Does nothing once done; [`Client::full_sync`] calls
  /// it first.
  pub async fn finish_enrollment(&mut self) -> Result<(), CoreError> {
    if !self.sync.is_enrollment_unfinished {
      return Ok(());
    }
    self.top_up_key_packages().await?;
    self.publish_device_list(&[]).await?;
    self.sync.is_enrollment_unfinished = false;
    self.persist_sync()
  }

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
  ///
  /// Only during enrollment, before the account is stored: the new backup key is stored with the
  /// account once the bag encrypted with it is on the server.
  pub(crate) async fn rotate_backup_key(
    &mut self,
    passphrase: &SecretBytes,
    argon2: Argon2Settings,
  ) -> Result<(), CoreError> {
    self.account.backup_secret = SymmetricKey::generate(&mut OsRng);
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
    passphrase_wrap(
      self.account.user_id,
      &self.account.backup_key(),
      passphrase,
      argon2,
    )
  }

  /// The wrapped backup keys this device uploads, decoded.
  pub(crate) async fn current_wrapped_keys(&mut self) -> Result<Vec<WrappedBackupKey>, CoreError> {
    let blob = self.wrapped_keys_blob().await?;
    decode_wrapped_keys(&blob)
  }

  /// The wrapped backup keys this device uploads next to the key bag: the stored ones, else the
  /// server's, taken only after the bag stored with them opened (they are bound to it, B11).
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
      // Enrollment uploads the first bag with the identity: an enrolled account without one lost
      // it (or enrolled with an older client). A bag without wraps could never be recovered.
      Err(error) if error.is_not_found() => Err(CoreError::KeyBagMissing),
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
  user: UserId,
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

/// A new identity with a fresh backup key, wrapped with the passphrase (the only way to create one).
fn new_identity(
  user: UserId,
  identity: IdentityKeyPair,
  passphrase: &SecretBytes,
  argon2: Argon2Settings,
) -> Result<Recovered, CoreError> {
  let backup_secret = SymmetricKey::generate(&mut OsRng);
  let wrapped = passphrase_wrap(
    user,
    &BackupKey::from_key(backup_secret.duplicate()),
    passphrase,
    argon2,
  )?;
  Ok(Recovered {
    key_bag: KeyBag::new(convert::user(user), identity),
    backup_secret,
    version: 0,
    sequence: 0,
    wrapped_keys: encode_wrapped_keys(&[wrapped])?,
  })
}

/// Uploads the new identity key together with its first key bag, in one request (e2ee.md §3.2):
/// the server never holds the identity without a way to recover it.
async fn publish_identity(
  api: &impl ServerApi,
  recovered: &mut Recovered,
  certificate: &SignedDeviceCertificate,
  options: &EnrollmentOptions,
) -> Result<(), CoreError> {
  let sequence = recovered.sequence + 1;
  let encrypted = recovered.key_bag.encrypt(
    &BackupKey::from_key(recovered.backup_secret.duplicate()),
    KeyBagBinding {
      sequence,
      wrapped_keys: &recovered.wrapped_keys,
    },
    &mut OsRng,
  )?;
  let encrypted_key_bag = Base64Bytes::new(encrypted.to_bytes()?);
  let wrapped_backup_keys = Base64Bytes::new(recovered.wrapped_keys.clone());
  let stored = api
    .put_identity_key(&UploadIdentityKeyRequest {
      public_key: Base64Bytes::new(recovered.key_bag.identity().public_key().as_bytes().to_vec()),
      // Proof of possession: the identity key's signature over this device's certificate.
      signature: Base64Bytes::new(certificate.to_bytes()?),
      maybe_attestation: options.maybe_attestation.clone(),
      replace: false,
      maybe_key_bag: Some(InitialKeyBag {
        encrypted_key_bag: encrypted_key_bag.clone(),
        wrapped_backup_keys: wrapped_backup_keys.clone(),
      }),
    })
    .await
    .map_err(|error| match error.error_code() {
      // Another device of this user enrolled first, between our check and now: the caller
      // enrolls again, which then recovers that identity.
      Some(teamcraft_proto::ErrorCode::Conflict) => CoreError::AlreadyEnrolled,
      _ => CoreError::from(error),
    })?;
  recovered.sequence = sequence;
  recovered.version = match stored.maybe_key_bag_version {
    Some(version) => version,
    // A server older than the combined upload ignored the bag: store it right away (the
    // window in which the identity has no bag is what the combined upload closes).
    None => {
      api
        .put_key_bag(&PutKeyBagRequest {
          expected_version: 0,
          encrypted_key_bag,
          wrapped_backup_keys,
        })
        .await?
        .version
    },
  };
  Ok(())
}

/// Registers this device's keys, or picks up the registration an interrupted attempt made with the
/// same identity. A registration certified by another identity (one that never reached the
/// server) is revoked, so no orphan device stays behind.
async fn ensure_device(
  api: &impl ServerApi,
  store: &impl LocalStore,
  user: UserId,
  identity: &IdentityKeyPair,
  options: &EnrollmentOptions,
) -> Result<(DeviceId, DeviceKeyPair, SignedDeviceCertificate), CoreError> {
  let provisional = teamcraft_crypto::ids::DeviceId::new(PROVISIONAL_DEVICE_ID);
  let identity_public_key = *identity.public_key().as_bytes();
  let maybe_pending =
    records::load_pending_device(store, provisional)?.filter(|pending| pending.user_id == user);
  let device = match maybe_pending {
    Some(pending) => {
      let maybe_registered = find_registered(api, user, &pending.device).await?;
      match maybe_registered {
        Some(id) if pending.identity_public_key == identity_public_key => {
          return certify(user, identity, &pending.device, id);
        },
        Some(orphan) => {
          revoke_orphan(api, orphan).await?;
          new_pending_device(store, user, identity_public_key)?
        },
        // The registration never reached the server: register the same keys.
        None if pending.identity_public_key == identity_public_key => pending.device,
        None => new_pending_device(store, user, identity_public_key)?,
      }
    },
    None => new_pending_device(store, user, identity_public_key)?,
  };
  let id = register_device(api, user, identity, &device, options).await?;
  certify(user, identity, &device, id)
}

/// A fresh device key pair, stored before it is registered.
fn new_pending_device(
  store: &impl LocalStore,
  user: UserId,
  identity_public_key: [u8; 32],
) -> Result<DeviceKeyPair, CoreError> {
  let pending = PendingDevice {
    user_id: user,
    identity_public_key,
    device: DeviceKeyPair::generate(
      convert::user(user),
      teamcraft_crypto::ids::DeviceId::new(PROVISIONAL_DEVICE_ID),
      &mut OsRng,
    ),
  };
  records::save_pending_device(store, &pending)?;
  Ok(pending.device)
}

/// The server's ID for `device`, if it is registered and not revoked.
async fn find_registered(
  api: &impl ServerApi,
  user: UserId,
  device: &DeviceKeyPair,
) -> Result<Option<DeviceId>, CoreError> {
  let public_key = device_public_key(device);
  let devices = api.list_user_devices(user).await?.devices;
  Ok(
    devices
      .iter()
      .find(|registered| registered.public_key.as_slice() == public_key.as_slice())
      .map(|registered| registered.id),
  )
}

async fn revoke_orphan(api: &impl ServerApi, orphan: DeviceId) -> Result<(), CoreError> {
  tracing::info!(device_id = %orphan, "revoking the device an interrupted enrollment registered");
  match api.revoke_device(orphan).await {
    Ok(()) => Ok(()),
    Err(error) if error.is_not_found() => Ok(()),
    Err(error) => Err(error.into()),
  }
}

async fn register_device(
  api: &impl ServerApi,
  user: UserId,
  identity: &IdentityKeyPair,
  device: &DeviceKeyPair,
  options: &EnrollmentOptions,
) -> Result<DeviceId, CoreError> {
  // The server assigns the device ID, so the certificate sent at registration can't contain it
  // yet. The final certificate (with the ID) is what MLS credentials carry.
  let provisional_certificate = identity.certify_device(convert::user(user), &device.public_keys())?;
  let registered = api
    .register_device(&RegisterDeviceRequest {
      name: options.device_name.clone(),
      public_key: Base64Bytes::new(device_public_key(device)),
      cross_signature: Base64Bytes::new(provisional_certificate.to_bytes()?),
    })
    .await?;
  Ok(registered.id)
}

/// The device's keys under the ID the server assigned, with the final certificate.
fn certify(
  user: UserId,
  identity: &IdentityKeyPair,
  provisional: &DeviceKeyPair,
  id: DeviceId,
) -> Result<(DeviceId, DeviceKeyPair, SignedDeviceCertificate), CoreError> {
  let crypto_user = convert::user(user);
  let (signing, agreement) = provisional.to_secrets();
  let device = DeviceKeyPair::from_secrets(crypto_user, convert::device(id), &signing, &agreement);
  let certificate = identity.certify_device(crypto_user, &device.public_keys())?;
  Ok((id, device, certificate))
}

/// What the server stores as the device's public key: signing key, then agreement key.
fn device_public_key(device: &DeviceKeyPair) -> Vec<u8> {
  let public = device.public_keys();
  let mut public_key = public.signing_key.as_bytes().to_vec();
  public_key.extend_from_slice(public.agreement_key.as_bytes());
  public_key
}

fn passphrase_wrap(
  user: UserId,
  backup_key: &BackupKey,
  passphrase: &SecretBytes,
  argon2: Argon2Settings,
) -> Result<WrappedBackupKey, CoreError> {
  let wrapper = PassphraseWrapper::new(SecretBytes::new(passphrase.expose_secret().to_vec()), argon2)?;
  Ok(wrapper.wrap_key(convert::user(user), backup_key, &mut OsRng)?)
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
