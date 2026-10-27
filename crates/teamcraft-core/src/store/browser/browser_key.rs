//! The database key in the browser (e2ee.md §9): a random 256-bit key, kept in IndexedDB wrapped
//! by a non-extractable Web Crypto AES-GCM key.
//!
//! This is the browser's stand-in for the OS keystore ([`super::super::KeyringKeySource`]): the
//! wrapping key's bytes can't be read by any script, so a copy of the IndexedDB files alone does not
//! reveal the database key. It does **not** keep the key from script running on the origin: pages
//! and the service worker unwrap it without a user gesture (push notifications need that), and one
//! `decrypt` call returns the raw database key, which an injected script could copy away for offline
//! use (docs/e2ee.md §9.1, docs/security-review.md C2). The key itself is not cryptography of our
//! own, just storage: Web Crypto encrypts, nothing is combined here.

use rand_core::{OsRng, RngCore};
use teamcraft_crypto::secret::{SYMMETRIC_KEY_LEN, SymmetricKey};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{AesGcmParams, AesKeyGenParams, CryptoKey, IdbTransactionMode, SubtleCrypto};
use zeroize::Zeroizing;

use super::idb::{self, KEYS};
use crate::store::StoreError;

pub(super) const WRAPPING_KEY: &str = "wrapping_key";
const WRAPPED_DATABASE_KEY: &str = "wrapped_database_key";
const AES_GCM: &str = "AES-GCM";
const IV_LEN: usize = 12;

/// Returns the database key of the IndexedDB database `name`, creating and storing it on first
/// use.
pub async fn browser_database_key(name: &str) -> Result<SymmetricKey, StoreError> {
  let database = idb::open_database(name).await?;
  let result = load_or_create(&database).await;
  database.close();
  result
}

async fn load_or_create(database: &web_sys::IdbDatabase) -> Result<SymmetricKey, StoreError> {
  let subtle = subtle()?;
  let maybe_wrapping_key = read(database, WRAPPING_KEY).await?;
  let maybe_wrapped = read(database, WRAPPED_DATABASE_KEY).await?;
  if let (Some(wrapping_key), Some(wrapped)) = (maybe_wrapping_key, maybe_wrapped) {
    let wrapping_key: CryptoKey = wrapping_key
      .dyn_into()
      .map_err(|_| StoreError::Corrupt("stored wrapping key".into()))?;
    let wrapped = wrapped
      .dyn_into::<js_sys::Uint8Array>()
      .map_err(|_| StoreError::Corrupt("stored database key".into()))?
      .to_vec();
    let plaintext = decrypt(&subtle, &wrapping_key, &wrapped)
      .await?
      .ok_or(StoreError::WrongKey)?;
    if plaintext.len() != SYMMETRIC_KEY_LEN {
      return Err(StoreError::Corrupt("wrapped database key length".into()));
    }
    return SymmetricKey::from_slice(&plaintext).ok_or_else(|| StoreError::Corrupt("database key".into()));
  }
  let wrapping_key = generate_wrapping_key(&subtle).await?;
  let database_key = SymmetricKey::generate(&mut OsRng);
  let wrapped = encrypt(&subtle, &wrapping_key, database_key.expose_secret()).await?;
  write(database, &wrapping_key, &wrapped).await?;
  Ok(database_key)
}

pub(super) async fn generate_wrapping_key(subtle: &SubtleCrypto) -> Result<CryptoKey, StoreError> {
  let algorithm = AesKeyGenParams::new(AES_GCM, 256);
  let usages = js_sys::Array::of2(&JsValue::from_str("encrypt"), &JsValue::from_str("decrypt"));
  // `extractable = false`: the key's bytes never leave the browser's crypto implementation.
  let promise = subtle
    .generate_key_with_object(&algorithm, false, &usages)
    .map_err(|error| crypto_error(&error))?;
  JsFuture::from(promise)
    .await
    .map_err(|error| crypto_error(&error))?
    .dyn_into()
    .map_err(|_| StoreError::Database("generateKey returned no key".into()))
}

/// `iv (12) || AES-GCM ciphertext and tag`.
pub(super) async fn encrypt(
  subtle: &SubtleCrypto,
  wrapping_key: &CryptoKey,
  plaintext: &[u8],
) -> Result<Vec<u8>, StoreError> {
  let mut iv = [0u8; IV_LEN];
  OsRng.fill_bytes(&mut iv);
  let params = AesGcmParams::new(AES_GCM, &js_sys::Uint8Array::from(iv.as_slice()));
  let promise = subtle
    .encrypt_with_object_and_u8_array(&params, wrapping_key, plaintext)
    .map_err(|error| crypto_error(&error))?;
  let ciphertext = JsFuture::from(promise)
    .await
    .map_err(|error| crypto_error(&error))?;
  let mut wrapped = iv.to_vec();
  wrapped.extend(js_sys::Uint8Array::new(&ciphertext).to_vec());
  Ok(wrapped)
}

/// The plaintext of [`encrypt`]'s output; `None` if it was not sealed with `wrapping_key` or was
/// changed.
pub(super) async fn decrypt(
  subtle: &SubtleCrypto,
  wrapping_key: &CryptoKey,
  sealed: &[u8],
) -> Result<Option<Zeroizing<Vec<u8>>>, StoreError> {
  if sealed.len() <= IV_LEN {
    return Ok(None);
  }
  let (iv, ciphertext) = sealed.split_at(IV_LEN);
  let params = AesGcmParams::new(AES_GCM, &js_sys::Uint8Array::from(iv));
  let promise = subtle
    .decrypt_with_object_and_u8_array(&params, wrapping_key, ciphertext)
    .map_err(|error| crypto_error(&error))?;
  let Ok(plaintext) = JsFuture::from(promise).await else {
    return Ok(None);
  };
  Ok(Some(Zeroizing::new(js_sys::Uint8Array::new(&plaintext).to_vec())))
}

async fn read(database: &web_sys::IdbDatabase, key: &str) -> Result<Option<JsValue>, StoreError> {
  let transaction = database
    .transaction_with_str(KEYS)
    .map_err(|error| idb::js_error("read key", &error))?;
  let request = transaction
    .object_store(KEYS)
    .and_then(|keys| keys.get(&JsValue::from_str(key)))
    .map_err(|error| idb::js_error("read key", &error))?;
  let value = idb::request_result(&request).await?;
  Ok((!value.is_undefined()).then_some(value))
}

async fn write(
  database: &web_sys::IdbDatabase,
  wrapping_key: &CryptoKey,
  wrapped: &[u8],
) -> Result<(), StoreError> {
  let transaction = database
    .transaction_with_str_and_mode(KEYS, IdbTransactionMode::Readwrite)
    .map_err(|error| idb::js_error("write key", &error))?;
  let keys = transaction
    .object_store(KEYS)
    .map_err(|error| idb::js_error("write key", &error))?;
  // Structured clone keeps the CryptoKey non-extractable in storage.
  keys
    .put_with_key(wrapping_key, &JsValue::from_str(WRAPPING_KEY))
    .and_then(|_| {
      keys.put_with_key(
        &js_sys::Uint8Array::from(wrapped),
        &JsValue::from_str(WRAPPED_DATABASE_KEY),
      )
    })
    .map_err(|error| idb::js_error("write key", &error))?;
  idb::transaction_done(&transaction).await
}

/// `crypto.subtle` of the window or worker.
pub(super) fn subtle() -> Result<SubtleCrypto, StoreError> {
  let crypto = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("crypto"))
    .ok()
    .and_then(|crypto| crypto.dyn_into::<web_sys::Crypto>().ok())
    .ok_or_else(|| StoreError::Database("Web Crypto is not available".into()))?;
  Ok(crypto.subtle())
}

pub(super) fn crypto_error(error: &JsValue) -> StoreError {
  StoreError::Database(format!(
    "Web Crypto: {}",
    crate::platform::describe_js_error(error)
  ))
}
