//! One-time codes of authenticator apps (RFC 6238), for local account sign-in.

use teamcraft_crypto::account::TotpSecret;

const STEP_SECS: u64 = 30;

/// The six-digit code of the secret (RFC 4648 base32, as the server shows it) `steps_from_now`
/// time steps from now. The server accepts one step either side and refuses to accept a step twice,
/// so consecutive sign-ins within a test use consecutive steps (-1, 0, +1).
pub fn code(base32_secret: &str, steps_from_now: i64) -> String {
  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .expect("clock after 1970")
    .as_secs();
  let step = (now / STEP_SECS).saturating_add_signed(steps_from_now);
  let secret = TotpSecret::from_bytes(&decode_base32(base32_secret));
  format!("{:06}", secret.code(step, 6))
}

fn decode_base32(text: &str) -> Vec<u8> {
  let mut bits = 0u32;
  let mut buffer = 0u32;
  let mut bytes = Vec::new();
  for character in text.trim_end_matches('=').bytes() {
    let value = match character {
      b'A'..=b'Z' => character - b'A',
      b'a'..=b'z' => character - b'a',
      b'2'..=b'7' => character - b'2' + 26,
      other => panic!("{:?} is not base32", other as char),
    };
    buffer = (buffer << 5) | u32::from(value);
    bits += 5;
    if bits >= 8 {
      bits -= 8;
      bytes.push((buffer >> bits) as u8);
      buffer &= (1 << bits) - 1;
    }
  }
  bytes
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn base32_decodes_the_rfc_4648_vectors() {
    assert_eq!(decode_base32("MZXW6YTBOI======"), b"foobar");
    assert_eq!(decode_base32("MZXW6"), b"foo");
  }
}
