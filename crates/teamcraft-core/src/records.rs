//! Typed records on top of [`LocalStore`]: the account, sync cursors, the outbox, pinned identities
//! and the openmls provider storage.

use std::collections::{BTreeMap, HashMap};

use openmls_rust_crypto::OpenMlsRustCrypto;
use openmls_traits::OpenMlsProvider;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use teamcraft_crypto::backup::BackupKey;
use teamcraft_crypto::identity::{DeviceKeyPair, IdentityPublicKey, SignedDeviceCertificate};
use teamcraft_crypto::key_bag::{EncryptedKeyBag, KeyBag, KeyBagBinding};
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{
  Base64Bytes, ClientMessageId, ConversationId, DeviceId, MlsMessageId, ThreadId, UserId,
};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::convert;
use crate::error::CoreError;
use crate::store::{LocalStore, MessageContent, StoredMessage, Table};

const ACCOUNT_KEY: &str = "account";
const KEY_BAG_KEY: &str = "key_bag";
const SYNC_KEY: &str = "sync";
const PENDING_DEVICE_KEY: &str = "pending_device";
const MLS_STORAGE_KEY: &str = "provider";

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
  put_json(store, Table::Account, ACCOUNT_KEY, &record)?;
  save_key_bag(store, account)
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

pub fn save_pending_device(store: &impl LocalStore, pending: &PendingDevice) -> Result<(), CoreError> {
  let (signing, agreement) = pending.device.to_secrets();
  let record = PendingDeviceRecord {
    user_id: pending.user_id,
    identity_public_key: pending.identity_public_key,
    device_signing_secret: *signing.expose_secret(),
    device_agreement_secret: *agreement.expose_secret(),
  };
  put_json(store, Table::Account, PENDING_DEVICE_KEY, &record)
}

/// The pending device, with the provisional device ID `provisional`.
pub fn load_pending_device(
  store: &impl LocalStore,
  provisional: teamcraft_crypto::ids::DeviceId,
) -> Result<Option<PendingDevice>, CoreError> {
  let Some(record) = get_json::<PendingDeviceRecord>(store, Table::Account, PENDING_DEVICE_KEY)? else {
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
  let mut bytes = [0u8; 16];
  rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut bytes);
  let client_message_id = ClientMessageId::from_bytes(bytes);
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
  store.upsert_message(&StoredMessage {
    conversation_id: draft.conversation_id,
    client_message_id,
    maybe_server_id: None,
    sender_user_id: sender,
    maybe_thread_id: draft.maybe_thread_id,
    sent_at_ms: now_ms,
    content: draft.content,
    is_edited: false,
    edits: Vec::new(),
    is_pinned: false,
  })?;
  let entry = OutboxEntry {
    conversation_id: draft.conversation_id,
    client_message_id,
    maybe_thread_id: draft.maybe_thread_id,
    attempts: 0,
    maybe_last_error: None,
  };
  put_json(
    store,
    Table::Outbox,
    &outbox_key(draft.conversation_id, client_message_id),
    &entry,
  )?;
  Ok(client_message_id)
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
  store.delete(Table::Outbox, &outbox_key(conversation, client_message_id))?;
  if let Some(mut message) = store.message(conversation, client_message_id)? {
    message.content = MessageContent::Deleted;
    store.upsert_message(&message)?;
  }
  Ok(())
}

/// The openmls provider storage holds group state and key package private keys. It is kept as one
/// snapshot (all entries) and written after every MLS operation.
pub fn save_mls_storage(store: &impl LocalStore, provider: &OpenMlsRustCrypto) -> Result<(), CoreError> {
  let values = provider
    .storage()
    .values
    .read()
    .map_err(|_| CoreError::Corrupt("MLS storage lock poisoned".into()))?;
  let mut encoded = zeroize::Zeroizing::new(Vec::new());
  for (key, value) in values.iter() {
    push_chunk(&mut encoded, key);
    push_chunk(&mut encoded, value);
  }
  store.put(Table::MlsStorage, MLS_STORAGE_KEY, &encoded)?;
  Ok(())
}

pub fn load_mls_storage(store: &impl LocalStore) -> Result<OpenMlsRustCrypto, CoreError> {
  let provider = OpenMlsRustCrypto::default();
  let Some(encoded) = store.get(Table::MlsStorage, MLS_STORAGE_KEY)? else {
    return Ok(provider);
  };
  let encoded = zeroize::Zeroizing::new(encoded);
  let mut entries = HashMap::new();
  let mut rest = encoded.as_slice();
  while !rest.is_empty() {
    let key = take_chunk(&mut rest)?;
    let value = take_chunk(&mut rest)?;
    entries.insert(key, value);
  }
  *provider
    .storage()
    .values
    .write()
    .map_err(|_| CoreError::Corrupt("MLS storage lock poisoned".into()))? = entries;
  Ok(provider)
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

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CoreError> {
  serde_json::from_slice(bytes).map_err(|error| CoreError::Corrupt(error.to_string()))
}

fn push_chunk(out: &mut Vec<u8>, chunk: &[u8]) {
  // Invariant: storage entries are far below 4 GiB.
  let length = u32::try_from(chunk.len()).unwrap_or(u32::MAX);
  out.extend_from_slice(&length.to_be_bytes());
  out.extend_from_slice(chunk);
}

fn take_chunk(rest: &mut &[u8]) -> Result<Vec<u8>, CoreError> {
  let corrupt = || CoreError::Corrupt("truncated MLS storage snapshot".into());
  let (length, tail) = rest.split_first_chunk::<4>().ok_or_else(corrupt)?;
  let length = u32::from_be_bytes(*length) as usize;
  let chunk = tail.get(..length).ok_or_else(corrupt)?;
  *rest = &tail[length..];
  Ok(chunk.to_vec())
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
    assert_eq!(loaded.device.public_keys(), account.device.public_keys());
    assert_eq!(loaded.identity_public_key(), account.identity_public_key());
    assert_eq!(loaded.certificate, account.certificate);
  }

  #[test]
  fn mls_storage_snapshot_round_trips() {
    let store = MemoryStore::new();
    let provider = OpenMlsRustCrypto::default();
    provider
      .storage()
      .values
      .write()
      .unwrap()
      .insert(b"key".to_vec(), b"value".to_vec());
    save_mls_storage(&store, &provider).unwrap();
    let loaded = load_mls_storage(&store).unwrap();
    assert_eq!(
      loaded.storage().values.read().unwrap().get(b"key".as_slice()),
      Some(&b"value".to_vec())
    );
    store
      .put(Table::MlsStorage, MLS_STORAGE_KEY, &[0, 0, 0, 9, 1])
      .unwrap();
    assert!(load_mls_storage(&store).is_err());
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
}
