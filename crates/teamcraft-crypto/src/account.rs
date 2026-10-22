//! Credentials of local accounts (guests that do not come from the identity provider): password
//! hashes (Argon2id), TOTP second factors (RFC 6238), one-time tokens and sealing of stored secrets.
//!
//! Local accounts are the exception, not the rule: members sign in with the identity provider.
//! This module only handles secrets; sessions and lockout are the server's business.

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use hkdf::Hkdf;
use hmac::{Hmac, KeyInit as _, Mac as _};
use rand_core::CryptoRngCore;
use sha1::Sha1;
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;
use zeroize::Zeroizing;

use crate::aead::{self, NONCE_LEN};
use crate::backup::{MAX_ARGON2_ITERATIONS, MAX_ARGON2_MEMORY_KIB, MAX_ARGON2_PARALLELISM};
use crate::domain;
use crate::secret::{SYMMETRIC_KEY_LEN, SecretBytes, SymmetricKey};

const PASSWORD_SCHEME: &str = "argon2id";
const SALT_LEN: usize = 16;
const HASH_LEN: usize = 32;
const TOTP_SECRET_LEN: usize = 20;
const TOTP_STEP_SECS: u64 = 30;
const ONE_TIME_TOKEN_BYTES: usize = 32;
/// A fixed hash to compare against when the account does not exist, so unknown and known accounts
/// take the same time.
const DUMMY_SALT: [u8; SALT_LEN] = [7; SALT_LEN];

/// Argon2id cost of password hashes. Configurable; the default follows the OWASP minimum
/// (19 MiB, 2 iterations).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasswordCost {
  pub memory_kib: u32,
  pub iterations: u32,
  pub parallelism: u32,
}

impl Default for PasswordCost {
  fn default() -> Self {
    Self {
      memory_kib: 19 * 1024,
      iterations: 2,
      parallelism: 1,
    }
  }
}

#[derive(Debug, thiserror::Error)]
pub enum AccountCryptoError {
  #[error("invalid password hash parameters")]
  InvalidCost,
  #[error("the stored password hash is malformed")]
  MalformedHash,
  #[error("key derivation failed")]
  KeyDerivation,
  #[error("the sealed secret is malformed or was tampered with")]
  Unseal,
}

/// Hashes a password into `argon2id$m=..,t=..,p=..$salt$hash` (unpadded standard base64).
pub fn hash_password(
  password: &str,
  cost: &PasswordCost,
  rng: &mut impl CryptoRngCore,
) -> Result<String, AccountCryptoError> {
  let mut salt = [0u8; SALT_LEN];
  rng.fill_bytes(&mut salt);
  let hash = derive(password, cost, &salt)?;
  Ok(format!(
    "{PASSWORD_SCHEME}$m={},t={},p={}${}${}",
    cost.memory_kib,
    cost.iterations,
    cost.parallelism,
    STANDARD_NO_PAD.encode(salt),
    STANDARD_NO_PAD.encode(hash.as_slice()),
  ))
}

/// Whether `password` matches a hash made by [`hash_password`]. The cost in the hash is bounded so
/// a corrupted row cannot make the server allocate without limit.
pub fn verify_password(password: &str, stored: &str) -> Result<bool, AccountCryptoError> {
  let parsed = parse_hash(stored)?;
  let actual = derive(password, &parsed.cost, &parsed.salt)?;
  Ok(bool::from(actual.as_slice().ct_eq(parsed.hash.as_slice())))
}

/// Spends the time of a password check without an account. Always `false`.
pub fn verify_password_for_unknown_account(password: &str, cost: &PasswordCost) -> bool {
  let _ = derive(password, cost, &DUMMY_SALT);
  false
}

/// Whether a stored hash was made with a different cost than `cost` and should be recomputed at
/// the next successful sign-in.
pub fn password_needs_rehash(stored: &str, cost: &PasswordCost) -> bool {
  parse_hash(stored)
    .map(|parsed| &parsed.cost != cost)
    .unwrap_or(true)
}

/// A stored password hash taken apart.
struct ParsedHash {
  cost: PasswordCost,
  salt: [u8; SALT_LEN],
  hash: Zeroizing<Vec<u8>>,
}

fn parse_hash(stored: &str) -> Result<ParsedHash, AccountCryptoError> {
  let mut parts = stored.split('$');
  let (Some(scheme), Some(params), Some(salt), Some(hash), None) = (
    parts.next(),
    parts.next(),
    parts.next(),
    parts.next(),
    parts.next(),
  ) else {
    return Err(AccountCryptoError::MalformedHash);
  };
  if scheme != PASSWORD_SCHEME {
    return Err(AccountCryptoError::MalformedHash);
  }
  let mut numbers = params
    .split(',')
    .map(|part| part.split_once('=').map(|(_, value)| value.parse::<u32>()));
  let (Some(Some(Ok(memory_kib))), Some(Some(Ok(iterations))), Some(Some(Ok(parallelism))), None) =
    (numbers.next(), numbers.next(), numbers.next(), numbers.next())
  else {
    return Err(AccountCryptoError::MalformedHash);
  };
  let salt = STANDARD_NO_PAD
    .decode(salt)
    .map_err(|_| AccountCryptoError::MalformedHash)?;
  let salt = <[u8; SALT_LEN]>::try_from(salt.as_slice()).map_err(|_| AccountCryptoError::MalformedHash)?;
  let hash = STANDARD_NO_PAD
    .decode(hash)
    .map_err(|_| AccountCryptoError::MalformedHash)?;
  if hash.len() != HASH_LEN {
    return Err(AccountCryptoError::MalformedHash);
  }
  Ok(ParsedHash {
    cost: PasswordCost {
      memory_kib,
      iterations,
      parallelism,
    },
    salt,
    hash: Zeroizing::new(hash),
  })
}

fn derive(
  password: &str,
  cost: &PasswordCost,
  salt: &[u8; SALT_LEN],
) -> Result<Zeroizing<[u8; HASH_LEN]>, AccountCryptoError> {
  if cost.memory_kib > MAX_ARGON2_MEMORY_KIB
    || cost.iterations > MAX_ARGON2_ITERATIONS
    || cost.parallelism > MAX_ARGON2_PARALLELISM
  {
    return Err(AccountCryptoError::InvalidCost);
  }
  let params = Params::new(cost.memory_kib, cost.iterations, cost.parallelism, Some(HASH_LEN))
    .map_err(|_| AccountCryptoError::InvalidCost)?;
  let mut output = Zeroizing::new([0u8; HASH_LEN]);
  Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
    .hash_password_into(password.as_bytes(), salt, output.as_mut())
    .map_err(|_| AccountCryptoError::KeyDerivation)?;
  Ok(output)
}

/// The Subresource Integrity value (`sha384-<base64>`) of a file, for `integrity` attributes.
pub fn subresource_integrity(bytes: &[u8]) -> String {
  format!("sha384-{}", STANDARD.encode(sha2::Sha384::digest(bytes)))
}

/// A random token for activation links and the like (256 bits, URL-safe). The server stores only
/// [`digest_token`] of it.
pub fn new_one_time_token(rng: &mut impl CryptoRngCore) -> String {
  let mut bytes = Zeroizing::new([0u8; ONE_TIME_TOKEN_BYTES]);
  rng.fill_bytes(bytes.as_mut());
  URL_SAFE_NO_PAD.encode(bytes.as_slice())
}

/// SHA-256 of a one-time token; high-entropy tokens need no slow hash.
pub fn digest_token(token: &str) -> [u8; 32] {
  Sha256::digest(token.as_bytes()).into()
}

/// The shared secret of a TOTP second factor.
pub struct TotpSecret(Zeroizing<Vec<u8>>);

impl TotpSecret {
  pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
    let mut bytes = Zeroizing::new(vec![0u8; TOTP_SECRET_LEN]);
    rng.fill_bytes(&mut bytes);
    Self(bytes)
  }

  pub fn from_bytes(bytes: &[u8]) -> Self {
    Self(Zeroizing::new(bytes.to_vec()))
  }

  pub fn as_bytes(&self) -> &[u8] {
    &self.0
  }

  /// The secret as authenticator apps expect it (RFC 4648 base32, no padding).
  pub fn to_base32(&self) -> String {
    base32(&self.0)
  }

  /// The `otpauth://` URI authenticator apps read from a QR code.
  pub fn provisioning_uri(&self, issuer: &str, account: &str) -> String {
    format!(
      "otpauth://totp/{}:{}?secret={}&issuer={}&algorithm=SHA1&digits=6&period={TOTP_STEP_SECS}",
      uri_escape(issuer),
      uri_escape(account),
      self.to_base32(),
      uri_escape(issuer),
    )
  }

  /// The code of the time step `step` with `digits` digits (RFC 6238 with HMAC-SHA1).
  pub fn code(&self, step: u64, digits: u32) -> u32 {
    // Invariant: HMAC accepts keys of any length.
    let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(&self.0) else {
      return 0;
    };
    mac.update(&step.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = usize::from(digest[digest.len() - 1] & 0x0f);
    let truncated = u32::from_be_bytes([
      digest[offset] & 0x7f,
      digest[offset + 1],
      digest[offset + 2],
      digest[offset + 3],
    ]);
    truncated % 10u32.pow(digits)
  }

  /// Checks a 6-digit code at `unix_secs`, accepting `window` steps before and after (clock drift).
  /// Returns the matching time step so the caller can refuse to accept it twice.
  pub fn verify(&self, code: &str, unix_secs: u64, window: u64) -> Option<u64> {
    if code.len() != 6 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
      return None;
    }
    let now = unix_secs / TOTP_STEP_SECS;
    let mut matched = None;
    // Every candidate is compared, so the time does not tell which step matched.
    for step in now.saturating_sub(window)..=now.saturating_add(window) {
      let expected = format!("{:06}", self.code(step, 6));
      if bool::from(expected.as_bytes().ct_eq(code.as_bytes())) {
        matched = Some(step);
      }
    }
    matched
  }
}

impl std::fmt::Debug for TotpSecret {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("TotpSecret([redacted])")
  }
}

/// Derives the key that seals secrets stored in the database from the server's secret.
pub fn derive_account_key(server_secret: &[u8]) -> Result<SymmetricKey, AccountCryptoError> {
  let hkdf = Hkdf::<Sha256>::new(None, server_secret);
  let mut key = Zeroizing::new([0u8; SYMMETRIC_KEY_LEN]);
  hkdf
    .expand(domain::ACCOUNT_SECRET_KEY.as_bytes(), key.as_mut())
    .map_err(|_| AccountCryptoError::KeyDerivation)?;
  Ok(SymmetricKey::from_bytes(*key))
}

/// Encrypts a secret for the database. `context` (e.g. the user ID) is authenticated, so a sealed
/// secret cannot be moved to another row.
pub fn seal_secret(
  key: &SymmetricKey,
  context: &[u8],
  secret: &[u8],
  rng: &mut impl CryptoRngCore,
) -> Result<Vec<u8>, AccountCryptoError> {
  let aad = domain::labeled(domain::ACCOUNT_SECRET_AAD, context);
  let (nonce, ciphertext) = aead::seal(key, &aad, secret, rng).map_err(|_| AccountCryptoError::Unseal)?;
  let mut out = nonce.to_vec();
  out.extend_from_slice(&ciphertext);
  Ok(out)
}

pub fn open_secret(
  key: &SymmetricKey,
  context: &[u8],
  sealed: &[u8],
) -> Result<SecretBytes, AccountCryptoError> {
  if sealed.len() < NONCE_LEN {
    return Err(AccountCryptoError::Unseal);
  }
  let (nonce, ciphertext) = sealed.split_at(NONCE_LEN);
  let nonce = <[u8; NONCE_LEN]>::try_from(nonce).map_err(|_| AccountCryptoError::Unseal)?;
  let aad = domain::labeled(domain::ACCOUNT_SECRET_AAD, context);
  aead::open(key, &nonce, &aad, ciphertext).map_err(|_| AccountCryptoError::Unseal)
}

fn base32(bytes: &[u8]) -> String {
  const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
  let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
  let (mut buffer, mut bits) = (0u32, 0u32);
  for byte in bytes {
    buffer = (buffer << 8) | u32::from(*byte);
    bits += 8;
    while bits >= 5 {
      bits -= 5;
      out.push(char::from(ALPHABET[((buffer >> bits) & 0x1f) as usize]));
    }
  }
  if bits > 0 {
    out.push(char::from(ALPHABET[((buffer << (5 - bits)) & 0x1f) as usize]));
  }
  out
}

fn uri_escape(text: &str) -> String {
  let mut out = String::new();
  for byte in text.bytes() {
    if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
      out.push(char::from(byte));
    } else {
      out.push_str(&format!("%{byte:02X}"));
    }
  }
  out
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;

  const CHEAP: PasswordCost = PasswordCost {
    memory_kib: 64,
    iterations: 1,
    parallelism: 1,
  };

  mod passwords {
    use super::*;

    #[test]
    fn the_right_password_verifies_and_others_do_not() {
      let stored = hash_password("correct horse", &CHEAP, &mut OsRng).unwrap();
      assert!(stored.starts_with("argon2id$m=64,t=1,p=1$"));
      assert!(verify_password("correct horse", &stored).unwrap());
      assert!(!verify_password("correct horse ", &stored).unwrap());
      assert!(!verify_password("", &stored).unwrap());
    }

    #[test]
    fn equal_passwords_get_different_salts() {
      let a = hash_password("same", &CHEAP, &mut OsRng).unwrap();
      let b = hash_password("same", &CHEAP, &mut OsRng).unwrap();
      assert_ne!(a, b);
    }

    #[test]
    fn malformed_or_oversized_hashes_are_rejected() {
      for stored in [
        "",
        "argon2id",
        "argon2id$m=64,t=1,p=1$AAAA$AAAA",
        "bcrypt$m=64,t=1,p=1$AAAAAAAAAAAAAAAAAAAAAA$AAAA",
        "argon2id$m=x,t=1,p=1$AAAAAAAAAAAAAAAAAAAAAA$AAAA",
        "argon2id$m=64,t=1,p=1$AAAAAAAAAAAAAAAAAAAAAA$AAAA$extra",
      ] {
        assert!(verify_password("x", stored).is_err(), "{stored}");
      }
      let salt = STANDARD_NO_PAD.encode([1u8; SALT_LEN]);
      let hash = STANDARD_NO_PAD.encode([2u8; HASH_LEN]);
      let huge = format!("argon2id$m={},t=1,p=1${salt}${hash}", MAX_ARGON2_MEMORY_KIB + 1);
      assert!(matches!(
        verify_password("x", &huge),
        Err(AccountCryptoError::InvalidCost)
      ));
    }

    #[test]
    fn a_changed_cost_asks_for_a_rehash() {
      let stored = hash_password("pw", &CHEAP, &mut OsRng).unwrap();
      assert!(!password_needs_rehash(&stored, &CHEAP));
      assert!(password_needs_rehash(&stored, &PasswordCost::default()));
      assert!(password_needs_rehash("garbage", &CHEAP));
    }

    #[test]
    fn unknown_accounts_never_verify() {
      assert!(!verify_password_for_unknown_account("anything", &CHEAP));
    }
  }

  mod totp {
    use super::*;

    // RFC 6238 appendix B, SHA-1 secret "12345678901234567890", 8 digits.
    #[test]
    fn matches_the_rfc_6238_test_vectors() {
      let secret = TotpSecret::from_bytes(b"12345678901234567890");
      for (time, expected) in [
        (59u64, 94_287_082u32),
        (1_111_111_109, 7_081_804),
        (1_111_111_111, 14_050_471),
        (1_234_567_890, 89_005_924),
        (2_000_000_000, 69_279_037),
      ] {
        assert_eq!(secret.code(time / 30, 8), expected, "t={time}");
      }
    }

    #[test]
    fn codes_verify_inside_the_window_only() {
      let secret = TotpSecret::generate(&mut OsRng);
      let now = 1_700_000_000u64;
      let code = format!("{:06}", secret.code(now / 30, 6));
      assert_eq!(secret.verify(&code, now, 1), Some(now / 30));
      assert_eq!(secret.verify(&code, now + 30, 1), Some(now / 30));
      assert_eq!(secret.verify(&code, now + 90, 1), None);
      assert_eq!(secret.verify(&code, now - 90, 1), None);
    }

    #[test]
    fn malformed_codes_are_rejected() {
      let secret = TotpSecret::generate(&mut OsRng);
      for code in ["", "12345", "1234567", "12345a", "１２３４５６", " 12345"] {
        assert_eq!(secret.verify(code, 1_700_000_000, 1), None, "{code:?}");
      }
    }

    #[test]
    fn base32_matches_rfc_4648_and_the_uri_is_escaped() {
      assert_eq!(base32(b""), "");
      assert_eq!(base32(b"f"), "MY");
      assert_eq!(base32(b"fo"), "MZXQ");
      assert_eq!(base32(b"foobar"), "MZXW6YTBOI");
      let secret = TotpSecret::from_bytes(b"foobar");
      let uri = secret.provisioning_uri("Team Craft", "a@b.example");
      assert_eq!(
        uri,
        "otpauth://totp/Team%20Craft:a%40b.example?secret=MZXW6YTBOI&issuer=Team%20Craft&algorithm=SHA1&digits=6&period=30"
      );
    }

    #[test]
    fn the_secret_is_not_printed() {
      let secret = TotpSecret::from_bytes(b"foobar");
      assert!(!format!("{secret:?}").contains("MZXW"));
    }
  }

  mod sealing {
    use super::*;

    #[test]
    fn sealed_secrets_round_trip_and_stick_to_their_context() {
      let key = derive_account_key(b"server secret of at least 32 bytes!!").unwrap();
      let sealed = seal_secret(&key, b"user 1", b"totp-secret", &mut OsRng).unwrap();
      assert_eq!(
        open_secret(&key, b"user 1", &sealed).unwrap().expose_secret(),
        b"totp-secret"
      );
      assert!(open_secret(&key, b"user 2", &sealed).is_err());
      let other = derive_account_key(b"another server secret of 32 bytes!").unwrap();
      assert!(open_secret(&other, b"user 1", &sealed).is_err());
      let mut tampered = sealed.clone();
      *tampered.last_mut().unwrap() ^= 1;
      assert!(open_secret(&key, b"user 1", &tampered).is_err());
      assert!(open_secret(&key, b"user 1", &[0u8; 5]).is_err());
    }

    #[test]
    fn subresource_integrity_matches_the_known_sha384_of_the_empty_input() {
      assert_eq!(
        subresource_integrity(b""),
        "sha384-OLBgp1GsljhM2TJ+sbHjaiH9txEUvgdDTAzHv2P24donTt6/529l+9Ua0vFImLlb"
      );
    }

    #[test]
    fn one_time_tokens_are_unique_and_digest_stably() {
      let a = new_one_time_token(&mut OsRng);
      let b = new_one_time_token(&mut OsRng);
      assert_ne!(a, b);
      assert_eq!(digest_token(&a), digest_token(&a));
      assert_ne!(digest_token(&a), digest_token(&b));
    }
  }
}
