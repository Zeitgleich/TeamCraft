//! Passkeys with the WebAuthn PRF extension, in the browser (e2ee.md §6.2.1, issue #1965).
//!
//! The page runs the ceremonies, the core does the cryptography: creating a passkey and evaluating
//! its PRF with a fresh random input gives a 32-byte secret the core wraps the backup key with;
//! recovering evaluates the passkey again with the input stored in its wrap. The relying party is
//! the web client's own host (the browser's default), so a passkey made here works wherever the
//! user's passkey provider syncs it and the same web client is opened. The desktop app's webview has
//! no origin a passkey could belong to, so it doesn't offer passkeys.
//!
//! The WebAuthn challenge is random and checked by nobody: no server verifies an assertion, only
//! the PRF output matters, and an authenticator without the credential can't produce it.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use js_sys::{Array, ArrayBuffer, Object, Reflect, Uint8Array};
use teamcraft_client_api::remote::{PasskeyWire, SecretText};
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::JsFuture;
use zeroize::Zeroizing;

/// Bytes of the PRF input stored with a wrap (`teamcraft_crypto::backup::PASSKEY_PRF_SALT_LEN`).
const PRF_SALT_LEN: usize = 32;
/// How long the browser waits for the user at a passkey prompt.
const CEREMONY_TIMEOUT_MS: u32 = 120_000;
/// ES256 and RS256, the algorithms every passkey provider supports.
const ALGORITHMS: [i32; 2] = [-7, -257];

/// Why a passkey step didn't give a PRF output. `Cancelled` is the user's choice and isn't shown as
/// an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasskeyFailure {
  /// The browser has no WebAuthn, or this is the desktop app.
  Unsupported,
  /// The passkey or its provider can't do PRF (some security keys, older providers).
  NoPrf,
  /// The user closed the prompt or it timed out.
  Cancelled,
  /// None of the passkeys the backup knows was used.
  Unknown,
}

/// A passkey just made and evaluated, ready for the core to wrap the backup key with.
pub struct NewPasskey {
  pub passkey: PasskeyWire,
  pub prf_output: SecretText,
}

/// Whether this page can offer passkeys: a browser with WebAuthn, not the desktop app.
pub fn is_supported() -> bool {
  !crate::auth::tauri::is_available() && credentials().is_some() && public_key_credential().is_some()
}

/// Creates a passkey for the key backup and evaluates its PRF with a fresh input. Two prompts on
/// most platforms: one to create, one to use it.
pub async fn create(user_handle: &[u8], user_name: &str) -> Result<NewPasskey, PasskeyFailure> {
  let credentials = credentials().ok_or(PasskeyFailure::Unsupported)?;
  let created = call_credentials(&credentials, "create", &creation_options(user_handle, user_name)?).await?;
  let credential_id = raw_id(&created)?;
  if prf_extension(&created)
    .and_then(|prf| Reflect::get(&prf, &"enabled".into()).ok())
    .is_some_and(|enabled| enabled == JsValue::FALSE)
  {
    return Err(PasskeyFailure::NoPrf);
  }
  let prf_salt = random_bytes(PRF_SALT_LEN)?;
  let passkey = PasskeyWire {
    credential_id: URL_SAFE_NO_PAD.encode(&credential_id),
    prf_salt: URL_SAFE_NO_PAD.encode(&prf_salt),
  };
  let (used, prf_output) = evaluate(std::slice::from_ref(&passkey)).await?;
  if used != passkey {
    return Err(PasskeyFailure::Unknown);
  }
  Ok(NewPasskey { passkey, prf_output })
}

/// Asks the user for one of `passkeys` (the account's passkey wraps) and evaluates its PRF with the
/// input stored for it. Returns which passkey was used and its output (base64url).
pub async fn evaluate(passkeys: &[PasskeyWire]) -> Result<(PasskeyWire, SecretText), PasskeyFailure> {
  let credentials = credentials().ok_or(PasskeyFailure::Unsupported)?;
  let asserted = call_credentials(&credentials, "get", &request_options(passkeys)?).await?;
  let used_id = URL_SAFE_NO_PAD.encode(raw_id(&asserted)?);
  let used = passkeys
    .iter()
    .find(|passkey| passkey.credential_id.trim_end_matches('=') == used_id)
    .ok_or(PasskeyFailure::Unknown)?
    .clone();
  let first = prf_extension(&asserted)
    .and_then(|prf| Reflect::get(&prf, &"results".into()).ok())
    .filter(|results| results.is_object())
    .and_then(|results| Reflect::get(&results, &"first".into()).ok())
    .and_then(|first| first.dyn_into::<ArrayBuffer>().ok())
    .ok_or(PasskeyFailure::NoPrf)?;
  let bytes = Zeroizing::new(Uint8Array::new(&first).to_vec());
  Ok((used, SecretText(URL_SAFE_NO_PAD.encode(bytes.as_slice()))))
}

/// `{ publicKey: { rp, user, challenge, pubKeyCredParams, authenticatorSelection, extensions } }`.
fn creation_options(user_handle: &[u8], user_name: &str) -> Result<Object, PasskeyFailure> {
  let rp = object(&[("name", JsValue::from_str("TeamCraft"))])?;
  let user = object(&[
    ("id", bytes_value(user_handle)),
    ("name", JsValue::from_str(user_name)),
    ("displayName", JsValue::from_str(user_name)),
  ])?;
  let parameters = Array::new();
  for algorithm in ALGORITHMS {
    let parameter = object(&[
      ("type", JsValue::from_str("public-key")),
      ("alg", JsValue::from(algorithm)),
    ])?;
    parameters.push(&parameter);
  }
  let selection = object(&[
    ("residentKey", JsValue::from_str("preferred")),
    ("userVerification", JsValue::from_str("required")),
  ])?;
  let extensions = object(&[("prf", Object::new().into())])?;
  let public_key = object(&[
    ("rp", rp.into()),
    ("user", user.into()),
    ("challenge", bytes_value(&random_bytes(32)?)),
    ("pubKeyCredParams", parameters.into()),
    ("authenticatorSelection", selection.into()),
    ("timeout", JsValue::from(CEREMONY_TIMEOUT_MS)),
    ("extensions", extensions.into()),
  ])?;
  object(&[("publicKey", public_key.into())])
}

/// `{ publicKey: { challenge, allowCredentials, userVerification, extensions: { prf: {
/// evalByCredential } } } }`: each allowed passkey is evaluated with its own stored input.
fn request_options(passkeys: &[PasskeyWire]) -> Result<Object, PasskeyFailure> {
  let allowed = Array::new();
  let by_credential = Object::new();
  for passkey in passkeys {
    let id = decode(&passkey.credential_id)?;
    let salt = decode(&passkey.prf_salt)?;
    let descriptor = object(&[
      ("type", JsValue::from_str("public-key")),
      ("id", bytes_value(&id)),
    ])?;
    allowed.push(&descriptor);
    set(
      &by_credential,
      passkey.credential_id.trim_end_matches('='),
      object(&[("first", bytes_value(&salt))])?.into(),
    )?;
  }
  let prf = object(&[("evalByCredential", by_credential.into())])?;
  let public_key = object(&[
    ("challenge", bytes_value(&random_bytes(32)?)),
    ("allowCredentials", allowed.into()),
    ("userVerification", JsValue::from_str("required")),
    ("timeout", JsValue::from(CEREMONY_TIMEOUT_MS)),
    ("extensions", object(&[("prf", prf.into())])?.into()),
  ])?;
  object(&[("publicKey", public_key.into())])
}

async fn call_credentials(
  credentials: &JsValue,
  method: &str,
  options: &Object,
) -> Result<JsValue, PasskeyFailure> {
  let function = Reflect::get(credentials, &method.into())
    .ok()
    .and_then(|value| value.dyn_into::<js_sys::Function>().ok())
    .ok_or(PasskeyFailure::Unsupported)?;
  let promise = function
    .call1(credentials, options)
    .ok()
    .and_then(|value| value.dyn_into::<js_sys::Promise>().ok())
    .ok_or(PasskeyFailure::Unsupported)?;
  match JsFuture::from(promise).await {
    Ok(value) if value.is_object() => Ok(value),
    Ok(_) => Err(PasskeyFailure::Cancelled),
    Err(error) => Err(failure_of(&error)),
  }
}

/// `NotAllowedError` is what browsers report for a cancelled or timed-out prompt; anything else
/// means the platform can't do it.
fn failure_of(error: &JsValue) -> PasskeyFailure {
  let name = Reflect::get(error, &"name".into())
    .ok()
    .and_then(|name| name.as_string())
    .unwrap_or_default();
  tracing::info!(error = %name, "the passkey prompt ended without a passkey");
  match name.as_str() {
    "NotAllowedError" | "AbortError" => PasskeyFailure::Cancelled,
    _ => PasskeyFailure::Unsupported,
  }
}

fn raw_id(credential: &JsValue) -> Result<Vec<u8>, PasskeyFailure> {
  Reflect::get(credential, &"rawId".into())
    .ok()
    .and_then(|value| value.dyn_into::<ArrayBuffer>().ok())
    .map(|buffer| Uint8Array::new(&buffer).to_vec())
    .ok_or(PasskeyFailure::Unsupported)
}

/// `credential.getClientExtensionResults().prf`, if the browser reported one.
fn prf_extension(credential: &JsValue) -> Option<JsValue> {
  let results = Reflect::get(credential, &"getClientExtensionResults".into())
    .ok()?
    .dyn_into::<js_sys::Function>()
    .ok()?
    .call0(credential)
    .ok()?;
  Reflect::get(&results, &"prf".into())
    .ok()
    .filter(JsValue::is_object)
}

fn credentials() -> Option<JsValue> {
  let navigator = web_sys::window()?.navigator();
  Reflect::get(&navigator, &"credentials".into())
    .ok()
    .filter(JsValue::is_object)
}

fn public_key_credential() -> Option<JsValue> {
  let window: JsValue = web_sys::window()?.into();
  Reflect::get(&window, &"PublicKeyCredential".into())
    .ok()
    .filter(|value| !value.is_undefined())
}

fn random_bytes(len: usize) -> Result<Vec<u8>, PasskeyFailure> {
  let mut bytes = vec![0u8; len];
  getrandom::getrandom(&mut bytes).map_err(|_| PasskeyFailure::Unsupported)?;
  Ok(bytes)
}

fn decode(text: &str) -> Result<Vec<u8>, PasskeyFailure> {
  URL_SAFE_NO_PAD
    .decode(text.trim_end_matches('='))
    .map_err(|_| PasskeyFailure::Unknown)
}

fn bytes_value(bytes: &[u8]) -> JsValue {
  Uint8Array::from(bytes).buffer().into()
}

fn object(fields: &[(&str, JsValue)]) -> Result<Object, PasskeyFailure> {
  let object = Object::new();
  for (key, value) in fields {
    set(&object, key, value.clone())?;
  }
  Ok(object)
}

fn set(object: &Object, key: &str, value: JsValue) -> Result<(), PasskeyFailure> {
  Reflect::set(object, &key.into(), &value)
    .map(|_| ())
    .map_err(|_| PasskeyFailure::Unsupported)
}
