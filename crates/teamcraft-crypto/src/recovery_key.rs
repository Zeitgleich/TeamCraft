//! The recovery key of a recovery kit (e2ee.md §6.5.1): 128 random bits the client generates and
//! the user keeps (printed, in a password manager), instead of a passphrase they invent.
//!
//! Shown as 32 Crockford Base32 characters in 8 groups of 4: the secret and a 32-bit checksum
//! that catches typos locally. The machine form (`tcrecovery1:` and the 32 characters) goes into
//! the kit's QR code and file, where [`RecoveryKey::find_in`] reads it back.

use rand_core::CryptoRngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::domain;

/// Random bytes of a recovery key (128 bit).
pub const RECOVERY_KEY_SECRET_LEN: usize = 16;
/// Characters of the shown and the machine form, without separators.
pub const RECOVERY_KEY_CHARS: usize = 32;
/// Characters per group of the shown form.
pub const RECOVERY_KEY_GROUP_LEN: usize = 4;
/// Groups of the shown form.
pub const RECOVERY_KEY_GROUPS: usize = RECOVERY_KEY_CHARS / RECOVERY_KEY_GROUP_LEN;
/// Prefix of the machine form; the digit is the format version.
pub const MACHINE_PREFIX: &str = "tcrecovery1:";

const FORMAT_VERSION: u8 = 1;
const CHECKSUM_LEN: usize = 4;
const KEY_BYTES_LEN: usize = RECOVERY_KEY_SECRET_LEN + CHECKSUM_LEN;
/// Crockford's Base32 alphabet: no I, L, O, U, so nothing looks alike in print.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// Machine forms of every version start with this; the version digits and `:` follow.
const MACHINE_PREFIX_STEM: &str = "tcrecovery";
const GROUP_SEPARATOR: char = '-';

/// A recovery key. Zeroized on drop, redacted in `Debug`, no `Display`, not `Clone`.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct RecoveryKey([u8; RECOVERY_KEY_SECRET_LEN]);

/// Why a text is not a recovery key. Carries no part of the text, which may be most of a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RecoveryKeyError {
  #[error("a recovery key has {RECOVERY_KEY_CHARS} characters, this has {0}")]
  WrongLength(usize),
  #[error("character {position} can't be part of a recovery key")]
  InvalidCharacter { position: usize },
  /// The text has the shape of a key but one or more characters are wrong (a typo).
  #[error("the recovery key's checksum doesn't match: a character is wrong")]
  ChecksumMismatch,
  /// A machine form of another format version (`tcrecovery2:`).
  #[error("recovery key format version {0} is not supported")]
  UnsupportedVersion(u32),
}

impl RecoveryKey {
  /// A fresh key from `rng` (the OS RNG in the apps).
  pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
    let mut secret = [0u8; RECOVERY_KEY_SECRET_LEN];
    rng.fill_bytes(&mut secret);
    Self(secret)
  }

  /// Reads the shown form (any case, with or without spaces and hyphens, `O` read as `0`, `I` and
  /// `L` as `1`) or the machine form.
  pub fn parse(text: &str) -> Result<Self, RecoveryKeyError> {
    let body = strip_machine_prefix(text.trim())?;
    let mut values = Zeroizing::new([0u8; RECOVERY_KEY_CHARS]);
    let mut count = 0;
    for character in body.chars() {
      if character.is_whitespace() || character == GROUP_SEPARATOR {
        continue;
      }
      let Some(value) = crockford_value(character) else {
        return Err(RecoveryKeyError::InvalidCharacter { position: count });
      };
      if let Some(slot) = values.get_mut(count) {
        *slot = value;
      }
      count += 1;
    }
    if count != RECOVERY_KEY_CHARS {
      return Err(RecoveryKeyError::WrongLength(count));
    }
    let bytes = unpack(&values);
    let (secret, checksum) = bytes.split_at(RECOVERY_KEY_SECRET_LEN);
    let mut key = Self([0u8; RECOVERY_KEY_SECRET_LEN]);
    key.0.copy_from_slice(secret);
    if bool::from(key.checksum().ct_eq(checksum)) {
      Ok(key)
    } else {
      Err(RecoveryKeyError::ChecksumMismatch)
    }
  }

  /// The first valid key in machine form inside `document` (a kit file), if any.
  pub fn find_in(document: &str) -> Option<Self> {
    let lowered = document.to_ascii_lowercase();
    lowered.match_indices(MACHINE_PREFIX).find_map(|(start, _)| {
      let body_start = start + MACHINE_PREFIX.len();
      let body: String = document
        .get(body_start..)?
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .take(RECOVERY_KEY_CHARS + 1)
        .collect();
      let body = Zeroizing::new(body);
      Self::parse(&body).ok()
    })
  }

  /// The shown form: `XXXX-XXXX-…`, 8 groups of 4.
  pub fn to_display(&self) -> Zeroizing<String> {
    let characters = self.to_characters();
    let mut out = Zeroizing::new(String::with_capacity(
      RECOVERY_KEY_CHARS + RECOVERY_KEY_GROUPS - 1,
    ));
    for (index, character) in characters.chars().enumerate() {
      if index > 0 && index % RECOVERY_KEY_GROUP_LEN == 0 {
        out.push(GROUP_SEPARATOR);
      }
      out.push(character);
    }
    out
  }

  /// The groups of the shown form, for the kit and the confirmation step.
  pub fn groups(&self) -> Zeroizing<Vec<String>> {
    let characters = self.to_characters();
    let groups = characters
      .as_bytes()
      .chunks(RECOVERY_KEY_GROUP_LEN)
      .map(|chunk| chunk.iter().map(|&byte| char::from(byte)).collect())
      .collect();
    Zeroizing::new(groups)
  }

  /// The machine form for the QR code and the kit file: `tcrecovery1:` and the 32 characters.
  pub fn to_machine(&self) -> Zeroizing<String> {
    let mut out = Zeroizing::new(String::with_capacity(MACHINE_PREFIX.len() + RECOVERY_KEY_CHARS));
    out.push_str(MACHINE_PREFIX);
    out.push_str(&self.to_characters());
    out
  }

  /// Explicit copy. Not `Clone`, so copies of keys are visible in the code.
  pub fn duplicate(&self) -> Self {
    Self(self.0)
  }

  /// Equality in constant time, for crates without `subtle`.
  pub fn is_same_key(&self, other: &Self) -> bool {
    bool::from(self.ct_eq(other))
  }

  /// The 128-bit secret, for the key derivation of the wrap (e2ee.md §6.5.2).
  pub(crate) fn expose_secret(&self) -> &[u8; RECOVERY_KEY_SECRET_LEN] {
    &self.0
  }

  fn to_characters(&self) -> Zeroizing<String> {
    let mut bytes = Zeroizing::new([0u8; KEY_BYTES_LEN]);
    bytes[..RECOVERY_KEY_SECRET_LEN].copy_from_slice(&self.0);
    bytes[RECOVERY_KEY_SECRET_LEN..].copy_from_slice(&self.checksum());
    pack(&bytes)
  }

  fn checksum(&self) -> [u8; CHECKSUM_LEN] {
    let mut input = Zeroizing::new(Vec::with_capacity(1 + RECOVERY_KEY_SECRET_LEN));
    input.push(FORMAT_VERSION);
    input.extend_from_slice(&self.0);
    let labeled = Zeroizing::new(domain::labeled(domain::RECOVERY_KEY_CHECKSUM, &input));
    let digest = Sha256::digest(labeled.as_slice());
    let mut checksum = [0u8; CHECKSUM_LEN];
    checksum.copy_from_slice(&digest[..CHECKSUM_LEN]);
    checksum
  }
}

impl ConstantTimeEq for RecoveryKey {
  fn ct_eq(&self, other: &Self) -> subtle::Choice {
    self.0.ct_eq(&other.0)
  }
}

impl std::fmt::Debug for RecoveryKey {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("RecoveryKey([redacted])")
  }
}

/// The text after a machine prefix of version 1, or the text itself when it has none.
fn strip_machine_prefix(text: &str) -> Result<&str, RecoveryKeyError> {
  let Some(stem) = text.get(..MACHINE_PREFIX_STEM.len()) else {
    return Ok(text);
  };
  if !stem.eq_ignore_ascii_case(MACHINE_PREFIX_STEM) {
    return Ok(text);
  }
  let rest = &text[MACHINE_PREFIX_STEM.len()..];
  let Some((version, body)) = rest.split_once(':') else {
    return Ok(text);
  };
  match version.parse::<u32>() {
    Ok(1) => Ok(body),
    Ok(other) => Err(RecoveryKeyError::UnsupportedVersion(other)),
    Err(_) => Ok(text),
  }
}

/// The 5-bit value of a Crockford character, reading `O` as `0` and `I`/`L` as `1`.
fn crockford_value(character: char) -> Option<u8> {
  let upper = match character.to_ascii_uppercase() {
    'O' => '0',
    'I' | 'L' => '1',
    other => other,
  };
  let byte = u8::try_from(upper).ok()?;
  ALPHABET
    .iter()
    .position(|&candidate| candidate == byte)
    .and_then(|index| u8::try_from(index).ok())
}

/// 20 bytes to 32 characters, most significant bit first.
fn pack(bytes: &[u8; KEY_BYTES_LEN]) -> Zeroizing<String> {
  let mut out = Zeroizing::new(String::with_capacity(RECOVERY_KEY_CHARS));
  let mut buffer: u32 = 0;
  let mut bits = 0u32;
  for &byte in bytes {
    buffer = (buffer << 8) | u32::from(byte);
    bits += 8;
    while bits >= 5 {
      bits -= 5;
      let index = (buffer >> bits) & 0x1f;
      out.push(char::from(ALPHABET[index as usize]));
    }
    buffer &= (1 << bits) - 1;
  }
  // 160 bits split into 32 groups of 5 exactly: nothing is left over.
  buffer.zeroize();
  out
}

/// 32 five-bit values back to 20 bytes.
fn unpack(values: &[u8; RECOVERY_KEY_CHARS]) -> Zeroizing<[u8; KEY_BYTES_LEN]> {
  let mut out = Zeroizing::new([0u8; KEY_BYTES_LEN]);
  let mut buffer: u32 = 0;
  let mut bits = 0u32;
  let mut position = 0;
  for &value in values {
    buffer = (buffer << 5) | u32::from(value);
    bits += 5;
    if bits >= 8 {
      bits -= 8;
      if let Some(slot) = out.get_mut(position) {
        *slot = ((buffer >> bits) & 0xff) as u8;
      }
      position += 1;
    }
    buffer &= (1 << bits) - 1;
  }
  buffer.zeroize();
  out
}

#[cfg(test)]
mod tests {
  use rand_core::{OsRng, RngCore};

  use super::*;

  const ZERO_SECRET: [u8; RECOVERY_KEY_SECRET_LEN] = [0; RECOVERY_KEY_SECRET_LEN];

  #[test]
  fn shown_form_has_eight_groups_of_four_from_the_alphabet() {
    let key = RecoveryKey::generate(&mut OsRng);
    let shown = key.to_display();
    let groups: Vec<&str> = shown.split('-').collect();
    assert_eq!(groups.len(), RECOVERY_KEY_GROUPS);
    assert!(groups.iter().all(|group| group.len() == RECOVERY_KEY_GROUP_LEN));
    assert!(
      shown
        .bytes()
        .filter(|&byte| byte != b'-')
        .all(|byte| ALPHABET.contains(&byte))
    );
    assert_eq!(key.groups().join("-"), shown.as_str());
  }

  /// Computed independently (Python's hashlib and the alphabet above), so the format can't drift.
  #[test]
  fn known_answers() {
    let counting: [u8; RECOVERY_KEY_SECRET_LEN] = std::array::from_fn(|index| index as u8);
    for (secret, shown) in [
      (ZERO_SECRET, "0000-0000-0000-0000-0000-0000-02CT-AZJ0"),
      (counting, "000G-40R4-0M30-E209-185G-R38E-1W1J-JG7V"),
    ] {
      let key = RecoveryKey(secret);
      assert_eq!(key.to_display().as_str(), shown);
      assert_eq!(
        key.to_machine().as_str(),
        format!("tcrecovery1:{}", shown.replace('-', ""))
      );
      assert_eq!(RecoveryKey::parse(shown).unwrap().0, secret);
    }
  }

  #[test]
  fn packing_round_trips_every_bit() {
    for _ in 0..64 {
      let mut bytes = [0u8; KEY_BYTES_LEN];
      OsRng.fill_bytes(&mut bytes);
      let text = pack(&bytes);
      let values: Vec<u8> = text.chars().map(|c| crockford_value(c).unwrap()).collect();
      let values: [u8; RECOVERY_KEY_CHARS] = values.try_into().unwrap();
      assert_eq!(*unpack(&values), bytes);
    }
  }

  mod parsing {
    use super::*;

    #[test]
    fn shown_and_machine_forms_round_trip() {
      let key = RecoveryKey::generate(&mut OsRng);
      for text in [key.to_display(), key.to_machine()] {
        let parsed = RecoveryKey::parse(&text).unwrap();
        assert!(bool::from(parsed.ct_eq(&key)));
      }
    }

    #[test]
    fn reading_is_forgiving_about_case_spaces_and_look_alikes() {
      let key = RecoveryKey::generate(&mut OsRng);
      let shown = key.to_display();
      let sloppy: String = format!("  {}  ", shown.replace('-', " "))
        .to_lowercase()
        .replace('0', "o")
        .replace('1', "l");
      let parsed = RecoveryKey::parse(&sloppy).unwrap();
      assert!(bool::from(parsed.ct_eq(&key)));
      let machine_upper = key.to_machine().to_uppercase();
      assert!(RecoveryKey::parse(&machine_upper).is_ok());
    }

    #[test]
    fn a_typo_is_a_checksum_mismatch() {
      let key = RecoveryKey::generate(&mut OsRng);
      let shown = key.to_display();
      let mut characters: Vec<char> = shown.chars().collect();
      characters[5] = if characters[5] == 'A' { 'B' } else { 'A' };
      let typo: String = characters.into_iter().collect();
      assert_eq!(
        RecoveryKey::parse(&typo).unwrap_err(),
        RecoveryKeyError::ChecksumMismatch
      );
      // Two groups swapped: same characters, still caught.
      let groups = key.groups();
      if groups[0] != groups[1] {
        let mut swapped = groups.to_vec();
        swapped.swap(0, 1);
        assert_eq!(
          RecoveryKey::parse(&swapped.join("-")).unwrap_err(),
          RecoveryKeyError::ChecksumMismatch
        );
      }
    }

    #[test]
    fn wrong_lengths_characters_and_versions_are_refused() {
      let key = RecoveryKey::generate(&mut OsRng);
      let characters = key.to_display().replace('-', "");
      assert_eq!(
        RecoveryKey::parse(&characters[..31]).unwrap_err(),
        RecoveryKeyError::WrongLength(31)
      );
      assert_eq!(
        RecoveryKey::parse(&format!("{characters}0")).unwrap_err(),
        RecoveryKeyError::WrongLength(33)
      );
      assert_eq!(
        RecoveryKey::parse("").unwrap_err(),
        RecoveryKeyError::WrongLength(0)
      );
      assert_eq!(
        RecoveryKey::parse("correct horse battery staple").unwrap_err(),
        RecoveryKeyError::WrongLength(25)
      );
      assert_eq!(
        RecoveryKey::parse("hunter2 hunter2").unwrap_err(),
        RecoveryKeyError::InvalidCharacter { position: 1 }
      );
      let with_u = format!("U{}", &characters[1..]);
      assert_eq!(
        RecoveryKey::parse(&with_u).unwrap_err(),
        RecoveryKeyError::InvalidCharacter { position: 0 }
      );
      assert_eq!(
        RecoveryKey::parse(&format!("tcrecovery2:{characters}")).unwrap_err(),
        RecoveryKeyError::UnsupportedVersion(2)
      );
    }

    #[test]
    fn errors_never_contain_the_text() {
      let key = RecoveryKey::generate(&mut OsRng);
      let shown = key.to_display();
      let truncated = &shown[..shown.len() - 1];
      let message = RecoveryKey::parse(truncated).unwrap_err().to_string();
      assert!(!message.contains(&shown[..4]));
    }
  }

  mod kit_files {
    use super::*;

    #[test]
    fn finds_the_machine_form_in_a_document() {
      let key = RecoveryKey::generate(&mut OsRng);
      let document = format!(
        "<html><body><p>Old: tcrecovery1:not-a-key</p><code>{}</code>\n</body></html>",
        key.to_machine().as_str()
      );
      let found = RecoveryKey::find_in(&document).unwrap();
      assert!(bool::from(found.ct_eq(&key)));
      let shouting = document.replace("tcrecovery1:", "TCRECOVERY1:");
      assert!(RecoveryKey::find_in(&shouting).is_some());
    }

    #[test]
    fn a_document_without_a_valid_key_has_none() {
      let key = RecoveryKey::generate(&mut OsRng);
      let machine = key.to_machine();
      let last = if machine.ends_with('X') { 'Y' } else { 'X' };
      let broken = format!("{}{last}", &machine[..machine.len() - 1]);
      assert!(RecoveryKey::find_in(&broken).is_none());
      assert!(RecoveryKey::find_in("<p>no key here</p>").is_none());
      // The shown form alone is not searched for: only the machine form marks a key.
      assert!(RecoveryKey::find_in(&key.to_display()).is_none());
    }
  }

  #[test]
  fn debug_is_redacted() {
    let key = RecoveryKey::generate(&mut OsRng);
    let debug = format!("{key:?}");
    assert_eq!(debug, "RecoveryKey([redacted])");
    assert!(!debug.contains(&key.to_display()[..4]));
  }
}
