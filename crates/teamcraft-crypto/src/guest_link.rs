//! The key code of a meeting guest (e2ee.md §4.6, "Meeting guests"): the first 64 bits of the
//! guest's identity key fingerprint, shown on the guest's screen so a host can compare it out of
//! band. Meeting links carry no secret (#987); the guest's keys are sealed to the guest's own
//! device at admission.

use subtle::ConstantTimeEq as _;

use crate::identity::IdentityPublicKey;

/// Hex digits of the key code: 64 bits, so finding an identity key with a chosen code costs 2^64
/// key generations, not an afternoon.
const KEY_CODE_HEX_DIGITS: usize = 16;
/// Digits per group of the displayed code.
const KEY_CODE_GROUP: usize = 4;

/// The code of an identity key that a host compares with the guest's screen out of band: the first 64 bits of the key's fingerprint, in groups (`1a2b-3c4d-5e6f-7a8b`).
pub fn key_code(identity_key: &IdentityPublicKey) -> String {
  let digits: Vec<char> = identity_key
    .fingerprint()
    .to_string()
    .chars()
    .filter(char::is_ascii_hexdigit)
    .take(KEY_CODE_HEX_DIGITS)
    .collect();
  digits
    .chunks(KEY_CODE_GROUP)
    .map(|group| group.iter().collect::<String>())
    .collect::<Vec<_>>()
    .join("-")
}

/// Whether `typed` (any case, with or without the dashes and spaces) is the key code of the key.
pub fn key_code_matches(identity_key: &IdentityPublicKey, typed: &str) -> bool {
  let normalized = |code: &str| -> String {
    code
      .chars()
      .filter(char::is_ascii_hexdigit)
      .map(|c| c.to_ascii_lowercase())
      .collect()
  };
  let (expected, given) = (normalized(&key_code(identity_key)), normalized(typed));
  given.len() == KEY_CODE_HEX_DIGITS && expected.as_bytes().ct_eq(given.as_bytes()).into()
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;
  use crate::identity::IdentityKeyPair;

  #[test]
  fn the_key_code_is_sixteen_hex_digits_of_the_fingerprint_in_groups() {
    let key = IdentityKeyPair::generate(&mut OsRng).public_key();
    let code = key_code(&key);
    assert_eq!(code.len(), KEY_CODE_HEX_DIGITS + 3);
    assert_eq!(code.matches('-').count(), 3);
    let digits = code.replace('-', "");
    assert!(key.fingerprint().to_string().starts_with(&digits));
    assert!(key_code_matches(&key, &code.to_uppercase()));
    assert!(key_code_matches(&key, &digits));
    assert!(!key_code_matches(&key, &code[..code.len() - 1]));
    assert!(!key_code_matches(&key, "0000-0000-0000-0000"));
  }
}
