//! Push notification crypto (e2ee.md §7).
//!
//! * **Wake-up payload:** XChaCha20-Poly1305 under the device's 256-bit push key, so Apple, Google
//!   and push relays see only ciphertext. The server seals, the app or its notification extension
//!   opens. Layout: `version (1) || nonce (24) || ciphertext || tag (16)`. The plaintext is opaque
//!   bytes to this crate (the server uses JSON naming the conversation and message).
//! * **Web Push:** RFC 8291 `aes128gcm` content encryption (single record) for a browser
//!   subscription, and the RFC 8292 VAPID `Authorization` header (ES256 JWT).
//! * **Subscription change:** the service worker proves that a rotated Web Push subscription comes
//!   from the device that registered the old one, with an HMAC-SHA-256 under a key derived from the
//!   push key, over the device, the old and the new subscription (e2ee.md §7). It can only replace
//!   the subscription it names: once replaced, the same proof no longer matches.
//!
//! No custom primitives: ECDH P-256, HKDF-SHA-256, AES-128-GCM and ECDSA P-256 compose as the RFCs
//! prescribe. The RFC 8291 appendix A test vector is a known-answer test.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hkdf::Hkdf;
use hmac::{Hmac, KeyInit as _, Mac as _};
use p256::ecdh::diffie_hellman;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{PublicKey, SecretKey};
use rand_core::CryptoRngCore;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// The user agent's private key; exposed so tests can play the browser.
pub use p256::SecretKey as UserAgentSecretKey;

use crate::aead::{self, NONCE_LEN, TAG_LEN};
use crate::domain::{self, PUSH_SUBSCRIPTION_CHANGE, PUSH_SUBSCRIPTION_CHANGE_KEY, PUSH_WAKEUP_AAD};
use crate::ids::DeviceId;
use crate::secret::{SecretBytes, SymmetricKey};

const WAKEUP_VERSION: u8 = 1;

/// Record size announced in the Web Push header; the whole payload is one record (RFC 8291 §4).
const RECORD_SIZE: u32 = 4096;
const SALT_LEN: usize = 16;
/// Length of an uncompressed P-256 public key.
pub const P256_PUBLIC_LEN: usize = 65;
/// Length of the Web Push authentication secret.
pub const AUTH_SECRET_LEN: usize = 16;
const GCM_TAG_LEN: usize = 16;
/// Largest plaintext that fits one record: record size minus tag minus the delimiter byte.
pub const MAX_WEB_PUSH_PLAINTEXT: usize = RECORD_SIZE as usize - GCM_TAG_LEN - 1;
/// VAPID tokens may live at most 24 hours (RFC 8292 §2); 12 leaves room for clock differences.
const VAPID_LIFETIME_SECS: u64 = 12 * 60 * 60;
/// Length of the proof of a subscription change (HMAC-SHA-256).
pub const SUBSCRIPTION_CHANGE_MAC_LEN: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum PushCryptoError {
  #[error("the encrypted wake-up is malformed or was tampered with")]
  BadWakeUp,
  #[error("subscription key material is invalid: {0}")]
  BadSubscription(&'static str),
  #[error("the payload is too large for one Web Push record")]
  TooLarge,
  #[error("the VAPID key is invalid")]
  BadVapidKey,
  #[error("a push key must be {} bytes", crate::secret::SYMMETRIC_KEY_LEN)]
  BadPushKey,
  #[error("encryption failed")]
  Encrypt,
  #[error("the subscription change is not authenticated by this push key")]
  BadSubscriptionChange,
}

/// Seals a wake-up for a device with its push key. A fresh random nonce is drawn per call.
pub fn seal_wakeup(
  push_key: &SymmetricKey,
  plaintext: &[u8],
  rng: &mut (impl CryptoRngCore + ?Sized),
) -> Result<Vec<u8>, PushCryptoError> {
  let aad = domain::labeled(PUSH_WAKEUP_AAD, &[]);
  let (nonce, ciphertext) =
    aead::seal(push_key, &aad, plaintext, rng).map_err(|_| PushCryptoError::Encrypt)?;
  let mut sealed = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
  sealed.push(WAKEUP_VERSION);
  sealed.extend_from_slice(&nonce);
  sealed.extend_from_slice(&ciphertext);
  Ok(sealed)
}

/// Opens a sealed wake-up (apps and notification extensions do this).
pub fn open_wakeup(push_key: &SymmetricKey, sealed: &[u8]) -> Result<SecretBytes, PushCryptoError> {
  let (version, rest) = sealed.split_first().ok_or(PushCryptoError::BadWakeUp)?;
  if *version != WAKEUP_VERSION || rest.len() < NONCE_LEN + TAG_LEN {
    return Err(PushCryptoError::BadWakeUp);
  }
  let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
  let nonce = <[u8; NONCE_LEN]>::try_from(nonce).map_err(|_| PushCryptoError::BadWakeUp)?;
  let aad = domain::labeled(PUSH_WAKEUP_AAD, &[]);
  aead::open(push_key, &nonce, &aad, ciphertext).map_err(|_| PushCryptoError::BadWakeUp)
}

/// The proof that `device`, holding `push_key`, replaces its Web Push subscription `previous_token`
/// with `token` (both the subscription JSON the server stores).
pub fn subscription_change_mac(
  push_key: &SymmetricKey,
  device: DeviceId,
  previous_token: &str,
  token: &str,
) -> [u8; SUBSCRIPTION_CHANGE_MAC_LEN] {
  subscription_change_hmac(push_key, device, previous_token, token)
    .finalize()
    .into_bytes()
    .into()
}

/// Checks a proof made by [`subscription_change_mac`], in constant time.
pub fn verify_subscription_change(
  push_key: &SymmetricKey,
  device: DeviceId,
  previous_token: &str,
  token: &str,
  mac: &[u8],
) -> Result<(), PushCryptoError> {
  subscription_change_hmac(push_key, device, previous_token, token)
    .verify_slice(mac)
    .map_err(|_| PushCryptoError::BadSubscriptionChange)
}

/// HMAC under a key of its own, so the push key's other use (sealing wake-ups) never meets it. The
/// body has fixed-length fields only: the device ID and the SHA-256 of both subscriptions.
fn subscription_change_hmac(
  push_key: &SymmetricKey,
  device: DeviceId,
  previous_token: &str,
  token: &str,
) -> Hmac<Sha256> {
  let hkdf = Hkdf::<Sha256>::new(None, push_key.expose_secret());
  let mut key = Zeroizing::new([0u8; 32]);
  // Invariant: 32 bytes is far below HKDF-SHA-256's output limit of 255 * 32 bytes.
  let _ = hkdf.expand(PUSH_SUBSCRIPTION_CHANGE_KEY.as_bytes(), key.as_mut());
  let mut body = Vec::with_capacity(8 + 2 * 32);
  body.extend_from_slice(&device.get().to_be_bytes());
  body.extend_from_slice(&Sha256::digest(previous_token.as_bytes()));
  body.extend_from_slice(&Sha256::digest(token.as_bytes()));
  // Invariant: HMAC takes keys of any length.
  let mut mac = match Hmac::<Sha256>::new_from_slice(key.as_ref()) {
    Ok(mac) => mac,
    Err(_) => unreachable!("HMAC-SHA-256 rejects a 32-byte key"),
  };
  mac.update(&domain::labeled(PUSH_SUBSCRIPTION_CHANGE, &body));
  mac
}

/// A browser's push subscription keys (`p256dh` and `auth`).
#[derive(Debug, Clone)]
pub struct SubscriptionKeys {
  ua_public: PublicKey,
  ua_public_bytes: Vec<u8>,
  auth_secret: [u8; AUTH_SECRET_LEN],
}

impl SubscriptionKeys {
  /// From raw bytes: the 65-byte uncompressed P-256 point and the 16-byte auth secret.
  pub fn from_bytes(ua_public: &[u8], auth_secret: &[u8]) -> Result<Self, PushCryptoError> {
    let auth_secret = <[u8; AUTH_SECRET_LEN]>::try_from(auth_secret)
      .map_err(|_| PushCryptoError::BadSubscription("auth size"))?;
    if ua_public.len() != P256_PUBLIC_LEN {
      return Err(PushCryptoError::BadSubscription("p256dh size"));
    }
    let key = PublicKey::from_sec1_bytes(ua_public)
      .map_err(|_| PushCryptoError::BadSubscription("p256dh is not a P-256 point"))?;
    Ok(Self {
      ua_public: key,
      ua_public_bytes: ua_public.to_vec(),
      auth_secret,
    })
  }

  /// From the base64url strings of a `PushSubscription`.
  pub fn from_base64url(p256dh: &str, auth: &str) -> Result<Self, PushCryptoError> {
    let decode = |text: &str, what| {
      URL_SAFE_NO_PAD
        .decode(text.trim_end_matches('='))
        .map_err(|_| PushCryptoError::BadSubscription(what))
    };
    Self::from_bytes(&decode(p256dh, "p256dh")?, &decode(auth, "auth")?)
  }
}

/// Encrypts `plaintext` for a subscription (RFC 8291, `aes128gcm`, one record). The result is the
/// complete request body, including the coding header with salt and the sender's public key.
pub fn encrypt_web_push(
  keys: &SubscriptionKeys,
  plaintext: &[u8],
  rng: &mut impl CryptoRngCore,
) -> Result<Vec<u8>, PushCryptoError> {
  let sender = SecretKey::random(&mut *rng);
  let mut salt = [0u8; SALT_LEN];
  rng.fill_bytes(&mut salt);
  encrypt_with(&sender, &salt, keys, plaintext)
}

fn encrypt_with(
  sender: &SecretKey,
  salt: &[u8; SALT_LEN],
  keys: &SubscriptionKeys,
  plaintext: &[u8],
) -> Result<Vec<u8>, PushCryptoError> {
  if plaintext.len() > MAX_WEB_PUSH_PLAINTEXT {
    return Err(PushCryptoError::TooLarge);
  }
  let as_public = sender.public_key().to_encoded_point(false);
  let as_public = as_public.as_bytes();
  let shared = diffie_hellman(sender.to_nonzero_scalar(), keys.ua_public.as_affine());
  let (cek, nonce) = derive_content_key(
    shared.raw_secret_bytes(),
    &keys.auth_secret,
    &keys.ua_public_bytes,
    as_public,
    salt,
  )?;

  // Single, last record: data followed by the 0x02 delimiter.
  let mut record = Zeroizing::new(Vec::with_capacity(plaintext.len() + 1));
  record.extend_from_slice(plaintext);
  record.push(0x02);
  let ciphertext = Aes128Gcm::new_from_slice(cek.as_slice())
    .map_err(|_| PushCryptoError::Encrypt)?
    .encrypt(Nonce::from_slice(nonce.as_slice()), record.as_slice())
    .map_err(|_| PushCryptoError::Encrypt)?;

  let key_id_len = u8::try_from(as_public.len()).map_err(|_| PushCryptoError::Encrypt)?;
  let mut body = Vec::with_capacity(SALT_LEN + 5 + as_public.len() + ciphertext.len());
  body.extend_from_slice(salt);
  body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
  body.push(key_id_len);
  body.extend_from_slice(as_public);
  body.extend_from_slice(&ciphertext);
  Ok(body)
}

/// RFC 8291 §3.4 (IKM from the ECDH secret and auth secret) and RFC 8188 §2.2/2.3 (CEK and nonce).
#[allow(clippy::type_complexity)] // two small zeroizing buffers
fn derive_content_key(
  ecdh_secret: &[u8],
  auth_secret: &[u8],
  ua_public: &[u8],
  as_public: &[u8],
  salt: &[u8],
) -> Result<(Zeroizing<[u8; 16]>, Zeroizing<[u8; 12]>), PushCryptoError> {
  let mut key_info = Vec::with_capacity(14 + ua_public.len() + as_public.len());
  key_info.extend_from_slice(b"WebPush: info\0");
  key_info.extend_from_slice(ua_public);
  key_info.extend_from_slice(as_public);
  let mut ikm = Zeroizing::new([0u8; 32]);
  Hkdf::<Sha256>::new(Some(auth_secret), ecdh_secret)
    .expand(&key_info, ikm.as_mut_slice())
    .map_err(|_| PushCryptoError::Encrypt)?;

  let content = Hkdf::<Sha256>::new(Some(salt), ikm.as_slice());
  let mut cek = Zeroizing::new([0u8; 16]);
  let mut nonce = Zeroizing::new([0u8; 12]);
  content
    .expand(b"Content-Encoding: aes128gcm\0", cek.as_mut_slice())
    .map_err(|_| PushCryptoError::Encrypt)?;
  content
    .expand(b"Content-Encoding: nonce\0", nonce.as_mut_slice())
    .map_err(|_| PushCryptoError::Encrypt)?;
  Ok((cek, nonce))
}

/// Decrypts a Web Push body as a user agent would. Browsers do this themselves; the server and the
/// tests use it to check what was sent.
pub fn decrypt_web_push(
  ua_private: &UserAgentSecretKey,
  auth_secret: &[u8],
  body: &[u8],
) -> Result<SecretBytes, PushCryptoError> {
  let malformed = PushCryptoError::BadSubscription("malformed body");
  if body.len() < SALT_LEN + 5 {
    return Err(malformed);
  }
  let (salt, rest) = body.split_at(SALT_LEN);
  let key_id_len = usize::from(rest[4]);
  let rest = &rest[5..];
  if rest.len() < key_id_len + GCM_TAG_LEN {
    return Err(PushCryptoError::BadSubscription("malformed body"));
  }
  let (as_public, ciphertext) = rest.split_at(key_id_len);
  let sender_public =
    PublicKey::from_sec1_bytes(as_public).map_err(|_| PushCryptoError::BadSubscription("sender key"))?;
  let ua_public = ua_private.public_key().to_encoded_point(false);

  let shared = diffie_hellman(ua_private.to_nonzero_scalar(), sender_public.as_affine());
  let (cek, nonce) = derive_content_key(
    shared.raw_secret_bytes(),
    auth_secret,
    ua_public.as_bytes(),
    as_public,
    salt,
  )?;
  let mut record = Aes128Gcm::new_from_slice(cek.as_slice())
    .map_err(|_| PushCryptoError::Encrypt)?
    .decrypt(Nonce::from_slice(nonce.as_slice()), ciphertext)
    .map_err(|_| PushCryptoError::BadWakeUp)?;
  // Strip the padding: zeros, then the delimiter.
  while record.last() == Some(&0) {
    record.pop();
  }
  if record.pop() != Some(0x02) {
    return Err(PushCryptoError::BadWakeUp);
  }
  Ok(SecretBytes::new(record))
}

/// The application server's VAPID identity (RFC 8292).
pub struct VapidKey {
  signing_key: SigningKey,
  public_key: Vec<u8>,
}

impl VapidKey {
  /// From the raw 32-byte P-256 private scalar.
  pub fn from_private_bytes(private_key: &[u8]) -> Result<Self, PushCryptoError> {
    if private_key.len() != 32 {
      return Err(PushCryptoError::BadVapidKey);
    }
    let secret = SecretKey::from_slice(private_key).map_err(|_| PushCryptoError::BadVapidKey)?;
    let public_key = secret.public_key().to_encoded_point(false).as_bytes().to_vec();
    Ok(Self {
      signing_key: SigningKey::from(&secret),
      public_key,
    })
  }

  /// From the base64url form `web-push` tools print.
  pub fn from_base64url(private_key: &str) -> Result<Self, PushCryptoError> {
    let raw = Zeroizing::new(
      URL_SAFE_NO_PAD
        .decode(private_key.trim().trim_end_matches('='))
        .map_err(|_| PushCryptoError::BadVapidKey)?,
    );
    Self::from_private_bytes(&raw)
  }

  /// The public key as base64url: what browsers pass as `applicationServerKey` when subscribing.
  pub fn public_key_base64url(&self) -> String {
    URL_SAFE_NO_PAD.encode(&self.public_key)
  }

  /// The `Authorization` header value (`vapid t=<jwt>, k=<public key>`) for a push service whose
  /// origin (`scheme://host[:port]`) is `audience`.
  pub fn authorization(
    &self,
    audience: &str,
    subject: &str,
    now_unix: u64,
  ) -> Result<String, PushCryptoError> {
    let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
    let claims = serde_json::json!({"aud": audience, "exp": now_unix + VAPID_LIFETIME_SECS, "sub": subject});
    let claims = serde_json::to_vec(&claims).map_err(|_| PushCryptoError::Encrypt)?;
    let signing_input = format!("{header}.{}", URL_SAFE_NO_PAD.encode(claims));
    let signature: Signature = self.signing_key.sign(signing_input.as_bytes());
    Ok(format!(
      "vapid t={signing_input}.{}, k={}",
      URL_SAFE_NO_PAD.encode(signature.to_bytes()),
      self.public_key_base64url()
    ))
  }
}

#[cfg(test)]
mod tests {
  use p256::ecdsa::VerifyingKey;
  use p256::ecdsa::signature::Verifier;
  use rand_core::OsRng;

  use super::*;

  fn b64(text: &str) -> Vec<u8> {
    URL_SAFE_NO_PAD.decode(text).unwrap()
  }

  fn key(byte: u8) -> SymmetricKey {
    SymmetricKey::from_bytes([byte; 32])
  }

  fn random_subscription() -> (SecretKey, [u8; AUTH_SECRET_LEN], SubscriptionKeys) {
    let ua_private = SecretKey::random(&mut OsRng);
    let mut auth = [0u8; AUTH_SECRET_LEN];
    rand_core::RngCore::fill_bytes(&mut OsRng, &mut auth);
    let keys =
      SubscriptionKeys::from_bytes(ua_private.public_key().to_encoded_point(false).as_bytes(), &auth)
        .unwrap();
    (ua_private, auth, keys)
  }

  mod subscription_change {
    use super::*;

    const OLD: &str = r#"{"endpoint":"https://push.example.com/old"}"#;
    const NEW: &str = r#"{"endpoint":"https://push.example.com/new"}"#;

    #[test]
    fn verifies_for_the_same_device_key_and_subscriptions() {
      let mac = subscription_change_mac(&key(1), DeviceId::new(42), OLD, NEW);
      verify_subscription_change(&key(1), DeviceId::new(42), OLD, NEW, &mac).unwrap();
    }

    #[test]
    fn rejects_another_key_device_subscription_or_a_tampered_mac() {
      let mac = subscription_change_mac(&key(1), DeviceId::new(42), OLD, NEW);
      let device = DeviceId::new(42);
      for (push_key, device, previous, token) in [
        (key(2), device, OLD, NEW),
        (key(1), DeviceId::new(43), OLD, NEW),
        (key(1), device, NEW, NEW),
        (key(1), device, OLD, OLD),
        // Swapping old and new must not verify: the proof can't undo a change.
        (key(1), device, NEW, OLD),
      ] {
        assert!(verify_subscription_change(&push_key, device, previous, token, &mac).is_err());
      }
      let mut tampered = mac;
      tampered[0] ^= 1;
      assert!(verify_subscription_change(&key(1), device, OLD, NEW, &tampered).is_err());
      assert!(verify_subscription_change(&key(1), device, OLD, NEW, &mac[..31]).is_err());
      assert!(verify_subscription_change(&key(1), device, OLD, NEW, &[]).is_err());
    }

    #[test]
    fn is_stable_across_versions() {
      let mac = subscription_change_mac(&key(1), DeviceId::new(42), OLD, NEW);
      assert_eq!(hex(&mac), KNOWN_MAC);
    }

    /// Fixed so a server and an older service worker keep agreeing; checked against an independent
    /// HKDF/HMAC implementation (Python's `hmac` and `hashlib`).
    const KNOWN_MAC: &str = "b55f79e587ad4fc553861d31e577236263740d3ba74dcc191e89ef50c931550d";

    fn hex(bytes: &[u8]) -> String {
      bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
  }

  mod wakeup {
    use super::*;

    #[test]
    fn round_trips_with_the_push_key_only() {
      let sealed = seal_wakeup(&key(7), b"{\"conversation\":\"1\"}", &mut OsRng).unwrap();
      assert_eq!(
        open_wakeup(&key(7), &sealed).unwrap().expose_secret(),
        b"{\"conversation\":\"1\"}"
      );
      assert_eq!(sealed[0], WAKEUP_VERSION);
      assert!(!String::from_utf8_lossy(&sealed).contains("conversation"));
    }

    #[test]
    fn every_seal_uses_a_fresh_nonce() {
      assert_ne!(
        seal_wakeup(&key(7), b"x", &mut OsRng).unwrap(),
        seal_wakeup(&key(7), b"x", &mut OsRng).unwrap()
      );
    }

    #[test]
    fn tampering_wrong_keys_and_garbage_are_rejected() {
      let sealed = seal_wakeup(&key(7), b"payload", &mut OsRng).unwrap();
      let mut flipped = sealed.clone();
      *flipped.last_mut().unwrap() ^= 1;
      assert!(open_wakeup(&key(7), &flipped).is_err());
      assert!(open_wakeup(&key(8), &sealed).is_err());
      let mut wrong_version = sealed.clone();
      wrong_version[0] = 2;
      assert!(open_wakeup(&key(7), &wrong_version).is_err());
      for bad in [&[][..], &[1][..], &sealed[..20]] {
        assert!(open_wakeup(&key(7), bad).is_err());
      }
    }

    #[test]
    fn the_payload_is_bound_to_this_purpose() {
      // A message sealed with the push key under another label must not open as a wake-up.
      let (nonce, ciphertext) = aead::seal(
        &key(7),
        &domain::labeled(domain::MESSAGE_AAD, &[]),
        b"x",
        &mut OsRng,
      )
      .unwrap();
      let mut forged = vec![WAKEUP_VERSION];
      forged.extend_from_slice(&nonce);
      forged.extend_from_slice(&ciphertext);
      assert!(open_wakeup(&key(7), &forged).is_err());
    }
  }

  mod web_push {
    use super::*;

    /// RFC 8291 appendix A.
    #[test]
    fn matches_the_rfc_8291_test_vector() {
      let plaintext = b64("V2hlbiBJIGdyb3cgdXAsIEkgd2FudCB0byBiZSBhIHdhdGVybWVsb24");
      let sender = SecretKey::from_slice(&b64("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw")).unwrap();
      let keys = SubscriptionKeys::from_base64url(
        "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
        "BTBZMqHH6r4Tts7J_aSIgg",
      )
      .unwrap();
      let salt: [u8; SALT_LEN] = b64("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();

      let body = encrypt_with(&sender, &salt, &keys, &plaintext).unwrap();
      let expected = b64(
        "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN",
      );
      assert_eq!(body, expected);

      // And the user agent side opens it again.
      let ua_private = SecretKey::from_slice(&b64("q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94")).unwrap();
      let opened = decrypt_web_push(&ua_private, &b64("BTBZMqHH6r4Tts7J_aSIgg"), &body).unwrap();
      assert_eq!(opened.expose_secret(), plaintext.as_slice());
    }

    #[test]
    fn the_user_agent_can_decrypt_what_was_encrypted_for_it() {
      let (ua_private, auth, keys) = random_subscription();
      for message in [&b"x"[..], &[7u8; 1000][..], &[1u8; MAX_WEB_PUSH_PLAINTEXT][..]] {
        let body = encrypt_web_push(&keys, message, &mut OsRng).unwrap();
        assert_eq!(
          decrypt_web_push(&ua_private, &auth, &body)
            .unwrap()
            .expose_secret(),
          message
        );
      }
    }

    #[test]
    fn wrong_secrets_tampering_and_oversized_payloads_fail() {
      let (ua_private, auth, keys) = random_subscription();
      let body = encrypt_web_push(&keys, b"hello", &mut OsRng).unwrap();
      assert!(
        decrypt_web_push(&ua_private, &[4u8; AUTH_SECRET_LEN], &body).is_err(),
        "wrong auth secret"
      );
      assert!(
        decrypt_web_push(&SecretKey::random(&mut OsRng), &auth, &body).is_err(),
        "wrong private key"
      );
      let mut flipped = body.clone();
      *flipped.last_mut().unwrap() ^= 1;
      assert!(decrypt_web_push(&ua_private, &auth, &flipped).is_err());
      assert!(decrypt_web_push(&ua_private, &auth, &body[..20]).is_err());
      assert!(matches!(
        encrypt_web_push(&keys, &vec![0u8; MAX_WEB_PUSH_PLAINTEXT + 1], &mut OsRng),
        Err(PushCryptoError::TooLarge)
      ));
    }

    #[test]
    fn every_message_uses_a_fresh_sender_key_and_salt() {
      let (_, _, keys) = random_subscription();
      let (a, b) = (
        encrypt_web_push(&keys, b"same", &mut OsRng).unwrap(),
        encrypt_web_push(&keys, b"same", &mut OsRng).unwrap(),
      );
      assert_ne!(a[..SALT_LEN], b[..SALT_LEN]);
      let key_range = SALT_LEN + 5..SALT_LEN + 5 + P256_PUBLIC_LEN;
      assert_ne!(a[key_range.clone()], b[key_range]);
    }

    #[test]
    fn subscription_keys_are_validated() {
      assert!(SubscriptionKeys::from_base64url("AAAA", "BTBZMqHH6r4Tts7J_aSIgg").is_err());
      assert!(SubscriptionKeys::from_base64url("!!!", "BTBZMqHH6r4Tts7J_aSIgg").is_err());
      let not_on_curve = URL_SAFE_NO_PAD.encode([4u8; P256_PUBLIC_LEN]);
      assert!(SubscriptionKeys::from_base64url(&not_on_curve, "BTBZMqHH6r4Tts7J_aSIgg").is_err());
      assert!(SubscriptionKeys::from_bytes(&[0u8; 65], &[0u8; 15]).is_err());
    }
  }

  mod vapid {
    use super::*;

    const PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";

    #[test]
    fn the_authorization_header_carries_a_verifiable_token_and_the_public_key() {
      let key = VapidKey::from_base64url(PRIVATE).unwrap();
      let header = key
        .authorization(
          "https://push.example.com:8443",
          "mailto:ops@example.com",
          1_800_000_000,
        )
        .unwrap();
      let (token, public) = header
        .strip_prefix("vapid t=")
        .unwrap()
        .split_once(", k=")
        .unwrap();
      assert_eq!(public, key.public_key_base64url());
      assert_eq!(b64(public).len(), P256_PUBLIC_LEN);

      let (signing_input, signature) = token.rsplit_once('.').unwrap();
      let verifying = VerifyingKey::from_sec1_bytes(&b64(public)).unwrap();
      let signature = Signature::from_slice(&b64(signature)).unwrap();
      verifying.verify(signing_input.as_bytes(), &signature).unwrap();

      let (head, claims) = signing_input.split_once('.').unwrap();
      assert_eq!(b64(head), br#"{"typ":"JWT","alg":"ES256"}"#);
      let claims: serde_json::Value = serde_json::from_slice(&b64(claims)).unwrap();
      assert_eq!(claims["aud"], "https://push.example.com:8443");
      assert_eq!(claims["sub"], "mailto:ops@example.com");
      assert_eq!(claims["exp"], 1_800_000_000 + VAPID_LIFETIME_SECS);
    }

    #[test]
    fn a_token_does_not_verify_after_tampering() {
      let key = VapidKey::from_base64url(PRIVATE).unwrap();
      let header = key
        .authorization("https://a.example.com", "mailto:a@example.com", 1)
        .unwrap();
      let (token, public) = header
        .strip_prefix("vapid t=")
        .unwrap()
        .split_once(", k=")
        .unwrap();
      let (input, signature) = token.rsplit_once('.').unwrap();
      let verifying = VerifyingKey::from_sec1_bytes(&b64(public)).unwrap();
      let signature = Signature::from_slice(&b64(signature)).unwrap();
      assert!(
        verifying
          .verify(format!("{input}x").as_bytes(), &signature)
          .is_err()
      );
    }

    #[test]
    fn invalid_keys_are_rejected() {
      assert!(VapidKey::from_base64url("not-a-key").is_err());
      assert!(
        VapidKey::from_base64url(&URL_SAFE_NO_PAD.encode([0u8; 32])).is_err(),
        "zero is no valid scalar"
      );
      assert!(VapidKey::from_private_bytes(&[1u8; 31]).is_err());
    }
  }
}
