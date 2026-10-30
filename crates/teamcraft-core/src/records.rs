//! Typed records on top of [`LocalStore`]: the account, sync cursors, the outbox and pinned
//! identities. The openmls provider storage has its own module, [`crate::mls_storage`].

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use teamcraft_crypto::backup::BackupKey;
use teamcraft_crypto::identity::{DeviceKeyPair, IdentityPublicKey, SignedDeviceCertificate};
use teamcraft_crypto::key_bag::{EncryptedKeyBag, KeyBag, KeyBagBinding};
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{
  Base64Bytes, ClientMessageId, ConversationId, DeviceId, MlsMessageId, ThreadId, UserId,
};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::account_data::SenderAbsence;
use crate::convert;
use crate::error::CoreError;
use crate::store::{LocalStore, MessageContent, StoredMessage, Table, write_group};

const ACCOUNT_KEY: &str = "account";
const KEY_BAG_KEY: &str = "key_bag";
const SYNC_KEY: &str = "sync";
const PENDING_DEVICE_KEY: &str = "pending_device";
const PENDING_LINK_KEY: &str = "pending_link";
const ABANDONED_DEVICES_KEY: &str = "abandoned_devices";
/// At most this many registrations of dropped device key pairs wait to be revoked; older ones are
/// given up (they stay in the user's device list, where the user can remove them).
const MAX_ABANDONED_DEVICES: usize = 16;
/// The device ID in a pending key pair: the server assigns the real one at registration. It plays
/// no part in the public keys.
pub(crate) const PENDING_DEVICE_ID: u64 = 0;

/// This device's identity: the user's keys (inside the key bag) and the device's own keys.
pub struct Account {
  pub user_id: UserId,
  pub device_id: DeviceId,
  pub device: DeviceKeyPair,
  pub certificate: SignedDeviceCertificate,
  pub backup_secret: SymmetricKey,
  pub key_bag: KeyBag,
}

/// Cursors and flags of the sync engine.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncState {
  #[serde(default)]
  pub maybe_gateway_session: Option<GatewaySession>,
  /// Last MLS entry processed per conversation.
  #[serde(default)]
  pub mls_cursors: BTreeMap<ConversationId, MlsMessageId>,
  #[serde(default)]
  pub maybe_welcome_cursor: Option<MlsMessageId>,
  /// Version of the key bag on the server (compare-and-swap).
  #[serde(default)]
  pub key_bag_version: u64,
  /// Content keys changed since the last key bag upload.
  #[serde(default)]
  pub is_key_bag_dirty: bool,
  /// Highest key bag sequence this device uploaded or accepted; older bags are refused (B11).
  #[serde(default)]
  pub key_bag_sequence: u64,
  /// The account is stored, but enrollment hasn't published the device list and key packages yet;
  /// the next sync finishes it (e2ee.md §3.2).
  #[serde(default)]
  pub is_enrollment_unfinished: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewaySession {
  pub session_id: String,
  pub last_seq: u64,
}

/// A message waiting to be sent; the content is the stored message with the same IDs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEntry {
  pub conversation_id: ConversationId,
  pub client_message_id: ClientMessageId,
  #[serde(rename = "thread_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_thread_id: Option<ThreadId>,
  pub attempts: u32,
  #[serde(rename = "last_error", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_error: Option<String>,
}

/// A user's pinned identity key, and a different key the server offered later, if any.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedIdentity {
  pub public_key: [u8; 32],
  #[serde(rename = "changed_to", default, skip_serializing_if = "Option::is_none")]
  pub maybe_changed_to: Option<[u8; 32]>,
  /// The identity provider attested the key when it was pinned (otherwise trust on first use).
  #[serde(default)]
  pub is_attested: bool,
}

/// A device key pair an unfinished enrollment registers (or is about to): stored before the
/// registration request, so a retry reuses the registration or revokes it instead of leaving an
/// orphan device on the server.
pub struct PendingDevice {
  pub user_id: UserId,
  /// The identity key that certified the device at registration.
  pub identity_public_key: [u8; 32],
  /// Carries the provisional device ID; the server assigns the real one.
  pub device: DeviceKeyPair,
}

/// Registrations of device key pairs that were dropped before their enrollment finished (issue
/// #415), kept until the server revoked them. No secret is in here; the key pairs themselves are
/// gone.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbandonedDevices {
  /// Oldest first.
  pub devices: Vec<AbandonedDevice>,
  /// After a refusal that won't pass by itself (not the network), the next try waits until then.
  #[serde(rename = "retry_at_ms", default, skip_serializing_if = "Option::is_none")]
  pub maybe_retry_at_ms: Option<i64>,
  /// Such refusals in a row; the wait doubles with each.
  #[serde(default)]
  pub failures: u32,
}

/// One registration to revoke.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbandonedDevice {
  pub user_id: UserId,
  /// As the server stores it: signing key, then agreement key.
  pub public_key: Base64Bytes,
  /// The identity key that certified the registration: only a device list it signed can name the
  /// device as in use.
  pub identity_public_key: [u8; 32],
}

/// Secrets an existing device handed over by linking, kept until the account is stored (issue
/// #168): a new device that stops after confirming the transfer finishes enrolling on its next
/// start instead of asking for another link.
pub struct PendingLink {
  pub user_id: UserId,
  pub device_name: String,
  pub secrets: crate::client::LinkedSecrets,
}

/// Secret part of [`Account`] as stored (SQLCipher protects it at rest).
#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct AccountRecord {
  #[zeroize(skip)]
  user_id: UserId,
  #[zeroize(skip)]
  device_id: DeviceId,
  device_signing_secret: [u8; 32],
  device_agreement_secret: [u8; 32],
  certificate: Vec<u8>,
  backup_secret: [u8; 32],
}

/// [`PendingDevice`] as stored.
#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct PendingDeviceRecord {
  #[zeroize(skip)]
  user_id: UserId,
  identity_public_key: [u8; 32],
  device_signing_secret: [u8; 32],
  device_agreement_secret: [u8; 32],
  /// When the key pair was stored (issue #415); missing in records of v1.0.8 and earlier.
  #[zeroize(skip)]
  #[serde(rename = "created_at_ms", default, skip_serializing_if = "Option::is_none")]
  maybe_created_at_ms: Option<i64>,
}

/// [`PendingLink`] as stored, protected like [`AccountRecord`].
#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct PendingLinkRecord {
  #[zeroize(skip)]
  user_id: UserId,
  #[zeroize(skip)]
  device_name: String,
  backup_secret: [u8; 32],
  encrypted_key_bag: Vec<u8>,
  #[zeroize(skip)]
  key_bag_version: u64,
  wrapped_keys: Vec<u8>,
}

impl Account {
  pub fn backup_key(&self) -> BackupKey {
    BackupKey::from_key(self.backup_secret.duplicate())
  }

  pub fn identity_public_key(&self) -> IdentityPublicKey {
    self.key_bag.identity().public_key()
  }
}

impl std::fmt::Debug for Account {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Account")
      .field("user_id", &self.user_id)
      .field("device_id", &self.device_id)
      .finish_non_exhaustive()
  }
}

pub fn save_account(store: &impl LocalStore, account: &Account) -> Result<(), CoreError> {
  let (signing, agreement) = account.device.to_secrets();
  let record = AccountRecord {
    user_id: account.user_id,
    device_id: account.device_id,
    device_signing_secret: *signing.expose_secret(),
    device_agreement_secret: *agreement.expose_secret(),
    certificate: account.certificate.to_bytes()?,
    backup_secret: *account.backup_secret.expose_secret(),
  };
  write_group(store, || {
    put_json(store, Table::Account, ACCOUNT_KEY, &record)?;
    save_key_bag(store, account)
  })
}

/// The local copy is never uploaded, so it isn't bound to a sequence or the wrapped keys.
const LOCAL_BINDING: KeyBagBinding<'static> = KeyBagBinding {
  sequence: 0,
  wrapped_keys: &[],
};

/// The key bag (identity key and all content keys) is stored encrypted with the backup key.
pub fn save_key_bag(store: &impl LocalStore, account: &Account) -> Result<(), CoreError> {
  let encrypted = account
    .key_bag
    .encrypt(&account.backup_key(), LOCAL_BINDING, &mut rand_core::OsRng)?;
  store.put(Table::Account, KEY_BAG_KEY, &encrypted.to_bytes()?)?;
  Ok(())
}

/// This device's ID alone, for helpers that run next to the app (the service worker) and need no
/// keys: only the ID field of the account record is read.
pub fn stored_device_id(store: &impl LocalStore) -> Result<Option<DeviceId>, CoreError> {
  #[derive(Deserialize)]
  struct DeviceOnly {
    device_id: DeviceId,
  }
  Ok(get_json::<DeviceOnly>(store, Table::Account, ACCOUNT_KEY)?.map(|record| record.device_id))
}

/// The account's user ID alone, like [`stored_device_id`]: the service worker finds the account's
/// media cache by it.
pub fn account_user_id(store: &impl LocalStore) -> Result<Option<UserId>, CoreError> {
  #[derive(Deserialize)]
  struct UserOnly {
    user_id: UserId,
  }
  Ok(get_json::<UserOnly>(store, Table::Account, ACCOUNT_KEY)?.map(|record| record.user_id))
}

pub fn load_account(store: &impl LocalStore) -> Result<Option<Account>, CoreError> {
  let Some(record) = get_json::<AccountRecord>(store, Table::Account, ACCOUNT_KEY)? else {
    return Ok(None);
  };
  let key_bag_bytes = store
    .get(Table::Account, KEY_BAG_KEY)?
    .ok_or_else(|| CoreError::Corrupt("account without key bag".into()))?;
  let backup_secret = SymmetricKey::from_bytes(record.backup_secret);
  let key_bag = EncryptedKeyBag::from_bytes(&key_bag_bytes)?.decrypt(
    convert::user(record.user_id),
    &BackupKey::from_key(backup_secret.duplicate()),
    LOCAL_BINDING.wrapped_keys,
  )?;
  let device = DeviceKeyPair::from_secrets(
    convert::user(record.user_id),
    convert::device(record.device_id),
    &SymmetricKey::from_bytes(record.device_signing_secret),
    &SymmetricKey::from_bytes(record.device_agreement_secret),
  );
  Ok(Some(Account {
    user_id: record.user_id,
    device_id: record.device_id,
    device,
    certificate: SignedDeviceCertificate::from_bytes(&record.certificate)?,
    backup_secret,
    key_bag,
  }))
}

/// Stores the key pair an enrollment is about to register, at `created_at_ms`: the expiry of an
/// enrollment nobody finishes counts from then (issue #415).
pub fn save_pending_device(
  store: &impl LocalStore,
  pending: &PendingDevice,
  created_at_ms: i64,
) -> Result<(), CoreError> {
  let (signing, agreement) = pending.device.to_secrets();
  let record = PendingDeviceRecord {
    user_id: pending.user_id,
    identity_public_key: pending.identity_public_key,
    device_signing_secret: *signing.expose_secret(),
    device_agreement_secret: *agreement.expose_secret(),
    maybe_created_at_ms: Some(created_at_ms),
  };
  put_json(store, Table::Account, PENDING_DEVICE_KEY, &record)
}

/// When the pending device key pair was stored, if there is one (issue #415). A record of an older
/// version has no time, and a time after `now_ms` comes from a clock that was wrong: both are set to
/// `now_ms` (and keep it), so the expiry counts from now instead of at once or from the future.
pub fn pending_device_created_at(store: &impl LocalStore, now_ms: i64) -> Result<Option<i64>, CoreError> {
  let Some(mut record) = get_secret_json::<PendingDeviceRecord>(store, Table::Account, PENDING_DEVICE_KEY)?
  else {
    return Ok(None);
  };
  match record.maybe_created_at_ms {
    Some(created_at_ms) if created_at_ms <= now_ms => Ok(Some(created_at_ms)),
    _ => {
      record.maybe_created_at_ms = Some(now_ms);
      put_json(store, Table::Account, PENDING_DEVICE_KEY, &record)?;
      Ok(Some(now_ms))
    },
  }
}

/// The pending device, with the provisional device ID `provisional`.
pub fn load_pending_device(
  store: &impl LocalStore,
  provisional: teamcraft_crypto::ids::DeviceId,
) -> Result<Option<PendingDevice>, CoreError> {
  let Some(record) = get_secret_json::<PendingDeviceRecord>(store, Table::Account, PENDING_DEVICE_KEY)?
  else {
    return Ok(None);
  };
  Ok(Some(PendingDevice {
    user_id: record.user_id,
    identity_public_key: record.identity_public_key,
    device: DeviceKeyPair::from_secrets(
      convert::user(record.user_id),
      provisional,
      &SymmetricKey::from_bytes(record.device_signing_secret),
      &SymmetricKey::from_bytes(record.device_agreement_secret),
    ),
  }))
}

pub fn delete_pending_device(store: &impl LocalStore) -> Result<(), CoreError> {
  Ok(store.delete(Table::Account, PENDING_DEVICE_KEY)?)
}

pub fn save_pending_link(store: &impl LocalStore, pending: &PendingLink) -> Result<(), CoreError> {
  let record = PendingLinkRecord {
    user_id: pending.user_id,
    device_name: pending.device_name.clone(),
    backup_secret: *pending.secrets.backup_secret.expose_secret(),
    encrypted_key_bag: pending.secrets.encrypted_key_bag.clone(),
    key_bag_version: pending.secrets.key_bag_version,
    wrapped_keys: pending.secrets.wrapped_keys.clone(),
  };
  put_json(store, Table::Account, PENDING_LINK_KEY, &record)
}

pub fn load_pending_link(store: &impl LocalStore) -> Result<Option<PendingLink>, CoreError> {
  let Some(record) = get_secret_json::<PendingLinkRecord>(store, Table::Account, PENDING_LINK_KEY)? else {
    return Ok(None);
  };
  Ok(Some(PendingLink {
    user_id: record.user_id,
    device_name: record.device_name.clone(),
    secrets: crate::client::LinkedSecrets {
      backup_secret: SymmetricKey::from_bytes(record.backup_secret),
      encrypted_key_bag: record.encrypted_key_bag.clone(),
      key_bag_version: record.key_bag_version,
      wrapped_keys: record.wrapped_keys.clone(),
    },
  }))
}

/// Whether the keys of a link wait to be enrolled, without decoding them; the plaintext the store
/// returned is zeroized at once.
pub fn has_pending_link(store: &impl LocalStore) -> Result<bool, CoreError> {
  Ok(
    store
      .get(Table::Account, PENDING_LINK_KEY)?
      .map(Zeroizing::new)
      .is_some(),
  )
}

pub fn delete_pending_link(store: &impl LocalStore) -> Result<(), CoreError> {
  Ok(store.delete(Table::Account, PENDING_LINK_KEY)?)
}

/// Drops what an unfinished enrollment kept, the secrets of a link and the pending device key pair:
/// the user signed out instead of finishing it (issues #168, #415). Does nothing once the account is
/// stored. The secrets are gone at once, and the store's free pages are purged so they can't be read
/// back from its files; the key pair's public key stays ([`AbandonedDevices`]) until the server
/// revoked the device it may have registered (`client::abandoned`).
pub fn forget_unfinished_enrollment(store: &impl LocalStore) -> Result<(), CoreError> {
  if is_account_stored(store)? {
    return Ok(());
  }
  write_group(store, || {
    delete_pending_link(store)?;
    move_pending_device_to_abandoned(store)
  })?;
  Ok(store.purge_freed()?)
}

/// Drops the pending device key pair of an enrollment nobody finishes (issue #415). Its public key
/// goes to the [`AbandonedDevices`] in the same group of writes, so a crash leaves either the key
/// pair or the registration to revoke, never neither; the store's free pages are purged afterwards.
/// Returns whether there was a key pair.
pub fn abandon_pending_device(store: &impl LocalStore) -> Result<bool, CoreError> {
  let is_abandoned = write_group(store, || move_pending_device_to_abandoned(store))?;
  if is_abandoned {
    store.purge_freed()?;
  }
  Ok(is_abandoned)
}

/// The registrations still to revoke, if any. A record that doesn't decode is dropped: it holds
/// public keys only, and it must not keep a key pair from being dropped for good.
pub fn load_abandoned_devices(store: &impl LocalStore) -> Result<Option<AbandonedDevices>, CoreError> {
  match get_json(store, Table::Account, ABANDONED_DEVICES_KEY) {
    Err(CoreError::Corrupt(error)) => {
      tracing::warn!(%error, "dropping an unreadable list of device registrations to revoke");
      store.delete(Table::Account, ABANDONED_DEVICES_KEY)?;
      Ok(None)
    },
    other => other,
  }
}

/// Keeps what is still to revoke; the record goes once nothing is.
pub fn save_abandoned_devices(
  store: &impl LocalStore,
  abandoned: &AbandonedDevices,
) -> Result<(), CoreError> {
  if abandoned.devices.is_empty() {
    return Ok(store.delete(Table::Account, ABANDONED_DEVICES_KEY)?);
  }
  put_json(store, Table::Account, ABANDONED_DEVICES_KEY, abandoned)
}

/// Whether this store holds an enrolled account.
pub fn is_account_stored(store: &impl LocalStore) -> Result<bool, CoreError> {
  Ok(store.get(Table::Account, ACCOUNT_KEY)?.is_some())
}

/// What the server stores as a device's public key: signing key, then agreement key.
pub(crate) fn device_public_key(device: &DeviceKeyPair) -> Vec<u8> {
  let public = device.public_keys();
  let mut public_key = public.signing_key.as_bytes().to_vec();
  public_key.extend_from_slice(public.agreement_key.as_bytes());
  public_key
}

/// [`abandon_pending_device`] without its group of writes and the purge.
fn move_pending_device_to_abandoned(store: &impl LocalStore) -> Result<bool, CoreError> {
  let Some(record) = get_secret_json::<PendingDeviceRecord>(store, Table::Account, PENDING_DEVICE_KEY)?
  else {
    return Ok(false);
  };
  let public_key = {
    let device = DeviceKeyPair::from_secrets(
      convert::user(record.user_id),
      teamcraft_crypto::ids::DeviceId::new(PENDING_DEVICE_ID),
      &SymmetricKey::from_bytes(record.device_signing_secret),
      &SymmetricKey::from_bytes(record.device_agreement_secret),
    );
    Base64Bytes::new(device_public_key(&device))
  };
  let entry = AbandonedDevice {
    user_id: record.user_id,
    public_key,
    identity_public_key: record.identity_public_key,
  };
  let mut abandoned = load_abandoned_devices(store)?.unwrap_or_default();
  if !abandoned.devices.contains(&entry) {
    abandoned.devices.push(entry);
  }
  let excess = abandoned.devices.len().saturating_sub(MAX_ABANDONED_DEVICES);
  if excess > 0 {
    tracing::warn!(
      excess,
      "giving up revoking the oldest device registrations of dropped enrollments"
    );
    abandoned.devices.drain(..excess);
  }
  save_abandoned_devices(store, &abandoned)?;
  delete_pending_device(store)?;
  Ok(true)
}

pub fn load_sync_state(store: &impl LocalStore) -> Result<SyncState, CoreError> {
  Ok(get_json(store, Table::SyncState, SYNC_KEY)?.unwrap_or_default())
}

pub fn save_sync_state(store: &impl LocalStore, state: &SyncState) -> Result<(), CoreError> {
  put_json(store, Table::SyncState, SYNC_KEY, state)
}

pub fn outbox_key(conversation: ConversationId, client_message_id: ClientMessageId) -> String {
  format!("{conversation}/{}", client_message_id.to_hex())
}

/// A message the user wrote, before it gets IDs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
  pub conversation_id: ConversationId,
  pub maybe_thread_id: Option<ThreadId>,
  pub content: MessageContent,
}

/// Stores a new own message and puts it into the outbox; the engine sends it on its next flush.
/// Usable without the engine (UI adapters write here, the sync task sends).
pub fn enqueue_message(
  store: &impl LocalStore,
  sender: UserId,
  now_ms: i64,
  draft: Draft,
) -> Result<ClientMessageId, CoreError> {
  let client_message_id = new_client_message_id();
  enqueue_message_as(store, sender, now_ms, draft, client_message_id)?;
  Ok(client_message_id)
}

/// A fresh random ID for an own message.
pub(crate) fn new_client_message_id() -> ClientMessageId {
  let mut bytes = [0u8; 16];
  rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut bytes);
  ClientMessageId::from_bytes(bytes)
}

/// [`enqueue_message`] with an ID chosen before: writing the same message again replaces it, and
/// the server deduplicates on the ID, so a retry after a crash never sends it twice.
pub(crate) fn enqueue_message_as(
  store: &impl LocalStore,
  sender: UserId,
  now_ms: i64,
  draft: Draft,
  client_message_id: ClientMessageId,
) -> Result<(), CoreError> {
  // Outbox messages are ordered by time written; keep that order strict even within a millisecond
  // (a reaction toggled twice quickly must apply in order).
  // Only the outbox is looked at: reading the whole conversation made every send cost as much as
  // the conversation's history.
  let mut latest_pending = None;
  for entry in list_json::<OutboxEntry>(store, Table::Outbox)?
    .into_iter()
    .filter(|entry| entry.conversation_id == draft.conversation_id)
  {
    if let Some(message) = store.message(entry.conversation_id, entry.client_message_id)? {
      latest_pending = latest_pending.max(Some(message.sent_at_ms));
    }
  }
  let now_ms = latest_pending.map_or(now_ms, |latest| now_ms.max(latest + 1));
  let message = StoredMessage {
    conversation_id: draft.conversation_id,
    client_message_id,
    maybe_server_id: None,
    sender_user_id: sender,
    maybe_thread_id: draft.maybe_thread_id,
    sent_at_ms: now_ms,
    content: draft.content,
    is_edited: false,
    edits: Vec::new(),
    maybe_edit_id: None,
    is_pinned: false,
    maybe_verified_commit: None,
    maybe_server_change: None,
  };
  let entry = OutboxEntry {
    conversation_id: draft.conversation_id,
    client_message_id,
    maybe_thread_id: draft.maybe_thread_id,
    attempts: 0,
    maybe_last_error: None,
  };
  // Together: a message without its outbox entry would show "sending" forever (issue #160).
  write_group(store, || {
    store.upsert_message(&message)?;
    put_json(
      store,
      Table::Outbox,
      &outbox_key(draft.conversation_id, client_message_id),
      &entry,
    )
  })
}

pub fn outbox(store: &impl LocalStore) -> Result<Vec<OutboxEntry>, CoreError> {
  list_json(store, Table::Outbox)
}

/// Whether the server rejected an own message for good; it then waits for "retry" or "delete".
pub fn is_send_failed(
  store: &impl LocalStore,
  conversation: ConversationId,
  client_message_id: ClientMessageId,
) -> Result<bool, CoreError> {
  let key = outbox_key(conversation, client_message_id);
  Ok(
    get_json::<OutboxEntry>(store, Table::Outbox, &key)?
      .is_some_and(|entry| entry.maybe_last_error.is_some()),
  )
}

/// Puts a failed outbox message back in line (the user tapped "retry"); the next flush sends it.
pub fn retry_failed(
  store: &impl LocalStore,
  conversation: ConversationId,
  client_message_id: ClientMessageId,
) -> Result<(), CoreError> {
  let key = outbox_key(conversation, client_message_id);
  if let Some(mut entry) = get_json::<OutboxEntry>(store, Table::Outbox, &key)? {
    entry.maybe_last_error = None;
    put_json(store, Table::Outbox, &key, &entry)?;
  }
  Ok(())
}

/// Drops a failed outbox message (the user chose "delete"): it leaves the outbox and shows as
/// deleted. Only failed ones: a message that may still be on its way can't be taken back here.
pub fn discard_failed(
  store: &impl LocalStore,
  conversation: ConversationId,
  client_message_id: ClientMessageId,
) -> Result<(), CoreError> {
  if !is_send_failed(store, conversation, client_message_id)? {
    return Ok(());
  }
  write_group(store, || {
    store.delete(Table::Outbox, &outbox_key(conversation, client_message_id))?;
    // Never sent: nobody else has it, so nothing of it is left in the conversation.
    store.delete_message(conversation, client_message_id)?;
    Ok(())
  })
}

pub fn pinned_identities(store: &impl LocalStore) -> Result<BTreeMap<UserId, PinnedIdentity>, CoreError> {
  let mut pinned = BTreeMap::new();
  for (key, value) in store.list(Table::PinnedIdentities)? {
    let user: UserId = serde_json::from_value(serde_json::Value::String(key))
      .map_err(|error| CoreError::Corrupt(error.to_string()))?;
    pinned.insert(user, decode(&value)?);
  }
  Ok(pinned)
}

pub fn pin_identity(store: &impl LocalStore, user: UserId, pinned: &PinnedIdentity) -> Result<(), CoreError> {
  put_json(store, Table::PinnedIdentities, &user.to_string(), pinned)
}

pub fn get_json<T: DeserializeOwned>(
  store: &impl LocalStore,
  table: Table,
  key: &str,
) -> Result<Option<T>, CoreError> {
  store.get(table, key)?.map(|bytes| decode(&bytes)).transpose()
}

/// [`get_json`] for a record that holds secrets: the plaintext the store returned is zeroized once
/// it is decoded (the record type zeroizes itself).
fn get_secret_json<T: DeserializeOwned>(
  store: &impl LocalStore,
  table: Table,
  key: &str,
) -> Result<Option<T>, CoreError> {
  store
    .get(table, key)?
    .map(|bytes| decode(&Zeroizing::new(bytes)))
    .transpose()
}

pub fn put_json<T: Serialize>(
  store: &impl LocalStore,
  table: Table,
  key: &str,
  value: &T,
) -> Result<(), CoreError> {
  let bytes = zeroize::Zeroizing::new(
    serde_json::to_vec(value).map_err(|error| CoreError::Corrupt(error.to_string()))?,
  );
  store.put(table, key, &bytes)?;
  Ok(())
}

pub fn list_json<T: DeserializeOwned>(store: &impl LocalStore, table: Table) -> Result<Vec<T>, CoreError> {
  store
    .list(table)?
    .iter()
    .map(|(_, value)| decode(value))
    .collect()
}

/// The entries of a table whose keys start with `prefix`, with their keys (a table that holds
/// records of several kinds under different prefixes).
pub fn list_json_with_prefix<T: DeserializeOwned>(
  store: &impl LocalStore,
  table: Table,
  prefix: &str,
) -> Result<Vec<(String, T)>, CoreError> {
  store
    .list_prefix(table, prefix)?
    .into_iter()
    .map(|(key, value)| decode(&value).map(|decoded| (key, decoded)))
    .collect()
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CoreError> {
  serde_json::from_slice(bytes).map_err(|error| CoreError::Corrupt(error.to_string()))
}

/// The verified signing key of a device, as recorded from its certificate (security review A10).
pub fn device_signing_key(
  store: &impl LocalStore,
  user: UserId,
  device: DeviceId,
) -> Result<Option<teamcraft_crypto::identity::DeviceSigningPublicKey>, CoreError> {
  let maybe: Option<Base64Bytes> = get_json(store, Table::DeviceKeys, &format!("{user}/{device}"))?;
  Ok(
    maybe
      .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
      .map(teamcraft_crypto::identity::DeviceSigningPublicKey::from_bytes),
  )
}

/// Where and when a device was out of a conversation, as this device checks its messages
/// (`conversation/user/device`). Rows from older versions hold just the time it left.
pub fn removed_sender(store: &impl LocalStore, key: &str) -> Result<Option<SenderAbsence>, CoreError> {
  let Some(bytes) = store.get(Table::RemovedSenders, key)? else {
    return Ok(None);
  };
  if let Ok(left_at_ms) = serde_json::from_slice::<i64>(&bytes) {
    return Ok(Some(SenderAbsence {
      left_at_ms,
      maybe_back_at_ms: None,
    }));
  }
  serde_json::from_slice(&bytes)
    .map(Some)
    .map_err(|error| CoreError::Corrupt(error.to_string()))
}

/// Merges `absence` into the stored row (the later time of each); returns the merged row.
pub fn merge_removed_sender(
  store: &impl LocalStore,
  key: &str,
  absence: &SenderAbsence,
) -> Result<SenderAbsence, CoreError> {
  let merged = match removed_sender(store, key)? {
    Some(mut stored) => {
      if !stored.merge(absence) {
        return Ok(stored);
      }
      stored
    },
    None => *absence,
  };
  put_json(store, Table::RemovedSenders, key, &merged)?;
  Ok(merged)
}

pub fn put_device_signing_key(
  store: &impl LocalStore,
  user: UserId,
  device: DeviceId,
  key: &teamcraft_crypto::identity::DeviceSigningPublicKey,
) -> Result<(), CoreError> {
  put_json(
    store,
    Table::DeviceKeys,
    &format!("{user}/{device}"),
    &Base64Bytes::new(key.as_bytes().to_vec()),
  )
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;
  use teamcraft_crypto::identity::IdentityKeyPair;
  use teamcraft_proto::{Id, Snowflake};

  use super::*;
  use crate::store::MemoryStore;

  #[test]
  fn account_round_trips_through_the_store() {
    let store = MemoryStore::new();
    let account = sample_account();
    save_account(&store, &account).unwrap();
    let loaded = load_account(&store).unwrap().unwrap();
    assert_eq!(loaded.user_id, account.user_id);
    assert_eq!(account_user_id(&store).unwrap(), Some(account.user_id));
    assert_eq!(
      account_user_id(&MemoryStore::new()).unwrap(),
      None,
      "no account yet"
    );
    assert_eq!(loaded.device.public_keys(), account.device.public_keys());
    assert_eq!(loaded.identity_public_key(), account.identity_public_key());
    assert_eq!(loaded.certificate, account.certificate);
  }

  #[test]
  fn signing_out_of_an_unfinished_enrollment_forgets_its_secrets_only() {
    let store = MemoryStore::new();
    let account = sample_account();
    let pending = sample_pending(account.user_id);
    save_pending_device(&store, &pending, 0).unwrap();
    store.put(Table::Account, PENDING_LINK_KEY, b"{}").unwrap();
    forget_unfinished_enrollment(&store).unwrap();
    assert_eq!(store.get(Table::Account, PENDING_DEVICE_KEY).unwrap(), None);
    assert_eq!(store.get(Table::Account, PENDING_LINK_KEY).unwrap(), None);
    // The public keys alone stay, until the server revoked the registration (issue #415).
    let abandoned = load_abandoned_devices(&store).unwrap().unwrap();
    assert_eq!(abandoned.devices, vec![entry_of(&pending)]);

    // An enrolled device keeps everything.
    save_account(&store, &account).unwrap();
    save_pending_device(&store, &pending, 0).unwrap();
    forget_unfinished_enrollment(&store).unwrap();
    assert!(store.get(Table::Account, PENDING_DEVICE_KEY).unwrap().is_some());
  }

  mod abandoned_devices {
    use super::*;

    const YEAR_MS: i64 = 365 * 24 * 60 * 60 * 1000;

    #[test]
    fn a_pending_record_of_an_older_version_gets_its_time_when_first_seen() {
      let store = MemoryStore::new();
      save_pending_device(&store, &sample_pending(sample_user()), 7).unwrap();
      // As v1.0.8 wrote it: no time.
      let mut json: serde_json::Value =
        serde_json::from_slice(&store.get(Table::Account, PENDING_DEVICE_KEY).unwrap().unwrap()).unwrap();
      json.as_object_mut().unwrap().remove("created_at_ms");
      store
        .put(
          Table::Account,
          PENDING_DEVICE_KEY,
          &serde_json::to_vec(&json).unwrap(),
        )
        .unwrap();

      assert_eq!(pending_device_created_at(&store, 1_000).unwrap(), Some(1_000));
      assert_eq!(pending_device_created_at(&store, 9_000).unwrap(), Some(1_000));
      assert_eq!(
        pending_device_created_at(&MemoryStore::new(), 1_000).unwrap(),
        None
      );
    }

    #[test]
    fn a_time_in_the_future_counts_from_now() {
      let store = MemoryStore::new();
      // Stored while the clock was a year ahead.
      save_pending_device(&store, &sample_pending(sample_user()), 1_000 + YEAR_MS).unwrap();

      assert_eq!(pending_device_created_at(&store, 1_000).unwrap(), Some(1_000));
      assert_eq!(
        pending_device_created_at(&store, 5_000).unwrap(),
        Some(1_000),
        "kept"
      );
    }

    #[test]
    fn only_the_newest_registrations_wait_to_be_revoked() {
      let store = MemoryStore::new();
      let user = sample_user();
      let mut entries = Vec::new();
      for _ in 0..=MAX_ABANDONED_DEVICES {
        let pending = sample_pending(user);
        entries.push(entry_of(&pending));
        save_pending_device(&store, &pending, 0).unwrap();
        assert!(abandon_pending_device(&store).unwrap());
      }
      assert!(!abandon_pending_device(&store).unwrap(), "nothing left to drop");
      let abandoned = load_abandoned_devices(&store).unwrap().unwrap();
      assert_eq!(abandoned.devices, entries[1..]);

      // Nothing left to revoke: the record goes.
      save_abandoned_devices(&store, &AbandonedDevices::default()).unwrap();
      assert_eq!(store.get(Table::Account, ABANDONED_DEVICES_KEY).unwrap(), None);
    }

    #[test]
    fn an_unreadable_list_never_keeps_a_key_pair_from_being_dropped() {
      let store = MemoryStore::new();
      store
        .put(Table::Account, ABANDONED_DEVICES_KEY, b"not json")
        .unwrap();
      assert_eq!(load_abandoned_devices(&store).unwrap(), None);
      assert_eq!(store.get(Table::Account, ABANDONED_DEVICES_KEY).unwrap(), None);

      store
        .put(Table::Account, ABANDONED_DEVICES_KEY, b"not json")
        .unwrap();
      let pending = sample_pending(sample_user());
      save_pending_device(&store, &pending, 0).unwrap();
      assert!(abandon_pending_device(&store).unwrap());
      assert_eq!(
        load_abandoned_devices(&store).unwrap().unwrap().devices,
        vec![entry_of(&pending)]
      );
    }

    /// The dropped key pair can't be read back from the files, also not with the database key.
    #[cfg(all(feature = "sealed-sqlite", not(target_arch = "wasm32")))]
    #[test]
    fn a_dropped_key_pair_leaves_no_ciphertext_in_the_files() {
      let gone = sealed_records_gone(
        |store| save_pending_device(store, &sample_pending(sample_user()), 0).unwrap(),
        |store| assert!(abandon_pending_device(store).unwrap()),
      );
      assert_eq!(gone, 1, "the key pair's record");
    }

    /// Signing out drops the keys of a link and the key pair; neither stays in the files.
    #[cfg(all(feature = "sealed-sqlite", not(target_arch = "wasm32")))]
    #[test]
    fn signing_out_leaves_no_ciphertext_of_the_secrets_in_the_files() {
      let gone = sealed_records_gone(
        |store| {
          let user = sample_user();
          save_pending_device(store, &sample_pending(user), 0).unwrap();
          let pending_link = PendingLink {
            user_id: user,
            device_name: "Laptop".into(),
            secrets: crate::client::LinkedSecrets {
              backup_secret: SymmetricKey::generate(&mut OsRng),
              encrypted_key_bag: vec![9; 512],
              key_bag_version: 3,
              wrapped_keys: vec![8; 128],
            },
          };
          save_pending_link(store, &pending_link).unwrap();
        },
        |store| forget_unfinished_enrollment(store).unwrap(),
      );
      assert_eq!(gone, 2, "the key pair's and the link's records");
    }

    /// Runs `before`, then `operation` on a sealed SQLite store, and checks that no ciphertext of
    /// the records `operation` deleted is left in the database or its log. Returns how many it
    /// deleted.
    #[cfg(all(feature = "sealed-sqlite", not(target_arch = "wasm32")))]
    fn sealed_records_gone(
      before: impl FnOnce(&crate::store::SealedSqliteStore),
      operation: impl FnOnce(&crate::store::SealedSqliteStore),
    ) -> usize {
      use rand_core::RngCore;

      use crate::store::{SealedSqliteStore, StaticKeySource};

      let directory = std::env::temp_dir().join(format!("teamcraft-records-{}", OsRng.next_u64()));
      std::fs::create_dir_all(&directory).unwrap();
      let path = directory.join("store.db");
      let store =
        SealedSqliteStore::open(&path, &StaticKeySource::new(SymmetricKey::generate(&mut OsRng))).unwrap();
      before(&store);
      let values_before = sealed_values(&path);

      operation(&store);

      let values_after = sealed_values(&path);
      let gone: Vec<Vec<u8>> = values_before
        .into_iter()
        .filter(|value| !values_after.contains(value))
        .collect();
      let raw = std::fs::read(&path).unwrap();
      let wal = std::fs::read(path.with_extension("db-wal")).unwrap_or_default();
      for record in &gone {
        let needle = &record[record.len() / 2..][..32];
        for bytes in [&raw, &wal] {
          assert!(!bytes.windows(needle.len()).any(|window| window == needle));
        }
      }
      drop(store);
      std::fs::remove_dir_all(&directory).unwrap();
      gone.len()
    }

    #[cfg(all(feature = "sealed-sqlite", not(target_arch = "wasm32")))]
    fn sealed_values(path: &std::path::Path) -> Vec<Vec<u8>> {
      let connection = rusqlite::Connection::open(path).unwrap();
      let mut statement = connection.prepare("SELECT value FROM sealed_records").unwrap();
      statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }
  }

  fn sample_account() -> Account {
    let user_id = UserId::from_snowflake(Snowflake::new(5).unwrap());
    let device_id = DeviceId::from_snowflake(Snowflake::new(6).unwrap());
    let identity = IdentityKeyPair::generate(&mut OsRng);
    let device = DeviceKeyPair::generate(convert::user(user_id), convert::device(device_id), &mut OsRng);
    let certificate = identity
      .certify_device(convert::user(user_id), &device.public_keys())
      .unwrap();
    Account {
      user_id,
      device_id,
      device,
      certificate,
      backup_secret: SymmetricKey::generate(&mut OsRng),
      key_bag: KeyBag::new(convert::user(user_id), identity),
    }
  }

  fn sample_user() -> UserId {
    UserId::from_snowflake(Snowflake::new(5).unwrap())
  }

  fn sample_pending(user_id: UserId) -> PendingDevice {
    PendingDevice {
      user_id,
      identity_public_key: [1; 32],
      device: DeviceKeyPair::generate(
        convert::user(user_id),
        teamcraft_crypto::ids::DeviceId::new(PENDING_DEVICE_ID),
        &mut OsRng,
      ),
    }
  }

  /// What dropping `pending` leaves to revoke.
  fn entry_of(pending: &PendingDevice) -> AbandonedDevice {
    AbandonedDevice {
      user_id: pending.user_id,
      public_key: Base64Bytes::new(device_public_key(&pending.device)),
      identity_public_key: pending.identity_public_key,
    }
  }
}
