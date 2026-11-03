//! The DPoP key in the browser (e2ee.md §3.1.2): a non-extractable Web Crypto ES256 key pair, kept in
//! IndexedDB. Its private half can't be read by any script, so a token and a copy of the browser's
//! files don't make a proof, and neither does script that ran on the origin once and is gone. Script
//! running on the origin can still ask it to sign, as long as it runs. The origin is the server here,
//! so this is one key per device and server. It stays after a sign-out: tabs that are open keep using
//! it, and it says nothing without a token. Web Crypto signs, nothing is combined here.

use std::sync::Arc;

use teamcraft_crypto::dpop::{DPOP_SIGNATURE_BYTES, DpopPublicKey};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{CryptoKey, EcKeyGenParams, EcdsaParams, IdbDatabase, IdbTransactionMode};

use super::browser_key::{crypto_error, subtle};
use super::idb::{self, KEYS, OpenMode};
use crate::api::dpop::{DpopProofError, DpopProver, DpopSignature, DpopSigner, SignFuture};
use crate::store::StoreError;

/// Its own database, so that nothing that clears the session or an account's store takes it along
/// while another tab still signs with it.
const DATABASE: &str = "teamcraft-dpop";
/// Fixed, apart from the account store's schema: a bump there must not make an older bundle or the
/// service worker unable to read the key, since a key that can't be read looks like a lost one.
const DATABASE_VERSION: u32 = 1;
const KEY_PAIR: &str = "key_pair";
const ECDSA: &str = "ECDSA";
const CURVE: &str = "P-256";
const HASH: &str = "SHA-256";

/// A proof key in Web Crypto.
struct BrowserDpopSigner {
  private_key: CryptoKey,
  public: DpopPublicKey,
}

impl DpopSigner for BrowserDpopSigner {
  fn public_key(&self) -> &DpopPublicKey {
    &self.public
  }

  fn sign<'a>(&'a self, message: &'a [u8]) -> SignFuture<'a> {
    Box::pin(async move {
      let subtle = subtle().map_err(signer_error)?;
      let params = EcdsaParams::new(ECDSA, &JsValue::from_str(HASH));
      let promise = subtle
        .sign_with_object_and_u8_array(&params, &self.private_key, message)
        .map_err(|error| signer_error(crypto_error(&error)))?;
      let signature = JsFuture::from(promise)
        .await
        .map_err(|error| signer_error(crypto_error(&error)))?;
      // Web Crypto's ECDSA signature is `r || s`, the form a JWS carries.
      let signature = js_sys::Uint8Array::new(&signature).to_vec();
      let signature: DpopSignature = <[u8; DPOP_SIGNATURE_BYTES]>::try_from(signature.as_slice())
        .map_err(|_| DpopProofError::Signer("Web Crypto signed in another form".to_owned()))?;
      Ok(signature)
    })
  }
}

/// What signs this browser's proofs. `Ok(None)` only if the store was read and has no key; a store that
/// can't be read is an error, which says nothing about the sign-in that needs the key.
pub async fn browser_dpop_prover() -> Result<Option<Arc<DpopProver>>, StoreError> {
  let database = idb::open_database_with(DATABASE, OpenMode::At(DATABASE_VERSION)).await?;
  let result = stored_prover(&database).await;
  database.close();
  result
}

/// [`browser_dpop_prover`], creating the key on first use. Two tabs creating it at once agree on one.
pub async fn browser_dpop_prover_or_new() -> Result<Arc<DpopProver>, StoreError> {
  let database = idb::open_database_with(DATABASE, OpenMode::At(DATABASE_VERSION)).await?;
  let result = stored_or_new(&database).await;
  database.close();
  result
}

async fn stored_or_new(database: &IdbDatabase) -> Result<Arc<DpopProver>, StoreError> {
  if let Some(prover) = stored_prover(database).await? {
    return Ok(prover);
  }
  let key_pair = generate_key_pair().await?;
  let transaction = database
    .transaction_with_str_and_mode(KEYS, IdbTransactionMode::Readwrite)
    .map_err(|error| idb::js_error("write DPoP key", &error))?;
  // Structured clone keeps the private key non-extractable in storage. `add` refuses an existing
  // key, and the one stored is the one everybody uses.
  transaction
    .object_store(KEYS)
    .and_then(|keys| keys.add_with_key(&key_pair, &JsValue::from_str(KEY_PAIR)))
    .map_err(|error| idb::js_error("write DPoP key", &error))?;
  if idb::transaction_done(&transaction).await.is_err() {
    tracing::debug!("another page stored the DPoP key first");
  }
  stored_prover(database)
    .await?
    .ok_or_else(|| StoreError::Database("the DPoP key was not stored".into()))
}

async fn stored_prover(database: &IdbDatabase) -> Result<Option<Arc<DpopProver>>, StoreError> {
  let transaction = database
    .transaction_with_str(KEYS)
    .map_err(|error| idb::js_error("read DPoP key", &error))?;
  let request = transaction
    .object_store(KEYS)
    .and_then(|keys| keys.get(&JsValue::from_str(KEY_PAIR)))
    .map_err(|error| idb::js_error("read DPoP key", &error))?;
  let key_pair = idb::request_result(&request).await?;
  if key_pair.is_undefined() {
    return Ok(None);
  }
  let private_key = key_of(&key_pair, "privateKey")?;
  let public = public_key_of(&key_of(&key_pair, "publicKey")?).await?;
  // Invariant of the browser build: Arc is only what the shared interface asks for, on one thread.
  #[allow(clippy::arc_with_non_send_sync)]
  let signer = Arc::new(BrowserDpopSigner { private_key, public });
  #[allow(clippy::arc_with_non_send_sync)]
  Ok(Some(Arc::new(DpopProver::new(signer))))
}

async fn generate_key_pair() -> Result<JsValue, StoreError> {
  let params = EcKeyGenParams::new(ECDSA, CURVE);
  let usages = js_sys::Array::of1(&JsValue::from_str("sign"));
  // `extractable = false`: the private key's bytes never leave the browser's crypto implementation.
  // A key pair's public half can always be read.
  let promise = subtle()?
    .generate_key_with_object(&params, false, &usages)
    .map_err(|error| crypto_error(&error))?;
  JsFuture::from(promise)
    .await
    .map_err(|error| crypto_error(&error))
}

fn key_of(key_pair: &JsValue, name: &str) -> Result<CryptoKey, StoreError> {
  js_sys::Reflect::get(key_pair, &JsValue::from_str(name))
    .ok()
    .and_then(|key| key.dyn_into().ok())
    .ok_or_else(|| StoreError::Corrupt("stored DPoP key".into()))
}

async fn public_key_of(key: &CryptoKey) -> Result<DpopPublicKey, StoreError> {
  let promise = subtle()?
    .export_key("jwk", key)
    .map_err(|error| crypto_error(&error))?;
  let jwk = JsFuture::from(promise)
    .await
    .map_err(|error| crypto_error(&error))?;
  let coordinate = |name: &str| {
    js_sys::Reflect::get(&jwk, &JsValue::from_str(name))
      .ok()
      .and_then(|value| value.as_string())
      .ok_or_else(|| StoreError::Corrupt("stored DPoP key".into()))
  };
  DpopPublicKey::from_coordinates(&coordinate("x")?, &coordinate("y")?)
    .map_err(|_| StoreError::Corrupt("stored DPoP key".into()))
}

fn signer_error(error: StoreError) -> DpopProofError {
  DpopProofError::Signer(error.to_string())
}
