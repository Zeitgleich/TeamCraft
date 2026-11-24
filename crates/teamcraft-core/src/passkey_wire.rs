//! Passkeys between the UI and the core (e2ee.md §6.2.1): credential IDs, PRF inputs and outputs
//! travel as base64url without padding, the form WebAuthn's JSON uses.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use teamcraft_client_api::remote::PasskeyWire;
use teamcraft_crypto::backup::{PASSKEY_PRF_SALT_LEN, PasskeyCredential};
use teamcraft_crypto::secret::SecretBytes;
use zeroize::Zeroizing;

use crate::error::CoreError;

/// A credential as the UI names it.
pub fn to_wire(credential: &PasskeyCredential) -> PasskeyWire {
  PasskeyWire {
    credential_id: URL_SAFE_NO_PAD.encode(credential.credential_id()),
    prf_salt: URL_SAFE_NO_PAD.encode(credential.prf_salt()),
  }
}

/// A credential the UI names, checked like one read from a wrap.
pub fn from_wire(wire: &PasskeyWire) -> Result<PasskeyCredential, CoreError> {
  let prf_salt = <[u8; PASSKEY_PRF_SALT_LEN]>::try_from(decode(&wire.prf_salt)?.as_slice())
    .map_err(|_| CoreError::Corrupt("a passkey's PRF input has the wrong length".into()))?;
  Ok(PasskeyCredential::new(
    credential_id(&wire.credential_id)?,
    prf_salt,
  )?)
}

/// A credential ID in base64url.
pub fn credential_id(text: &str) -> Result<Vec<u8>, CoreError> {
  decode(text)
}

/// The PRF output the authenticator returned, in base64url; the decoded copy is the only one kept.
pub fn prf_output(text: &str) -> Result<SecretBytes, CoreError> {
  let bytes = Zeroizing::new(decode(text)?);
  Ok(SecretBytes::new(bytes.to_vec()))
}

fn decode(text: &str) -> Result<Vec<u8>, CoreError> {
  URL_SAFE_NO_PAD
    .decode(text.trim_end_matches('='))
    .map_err(|_| CoreError::Corrupt("passkey data is not base64url".into()))
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;

  #[test]
  fn credentials_round_trip_and_bad_input_is_refused() {
    let credential = PasskeyCredential::generate(b"credential".to_vec(), &mut OsRng).unwrap();
    assert_eq!(from_wire(&to_wire(&credential)).unwrap(), credential);
    let short_salt = PasskeyWire {
      prf_salt: "AAAA".into(),
      ..to_wire(&credential)
    };
    assert!(from_wire(&short_salt).is_err());
    assert!(prf_output("not base64!").is_err());
  }
}
