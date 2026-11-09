//! Signatures of outgoing webhooks (docs/webhooks.md), following the Standard Webhooks
//! specification so receivers can use its libraries: HMAC-SHA-256 over `{id}.{timestamp}.{body}`
//! with a random per-webhook secret, sent as `webhook-signature: v1,<base64>`. The timestamp is
//! part of the signed content, so a receiver that rejects old timestamps rejects replays; the ID
//! lets it drop duplicates within its tolerance window.
//!
//! The secret authenticates the server to the receiver only. It protects no content: webhooks
//! carry metadata the server already knows.

use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, KeyInit as _, Mac as _};
use rand_core::CryptoRngCore;
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

/// Prefix of a secret's text form, as in the Standard Webhooks specification.
pub const WEBHOOK_SECRET_PREFIX: &str = "whsec_";
/// Length of a generated secret (256 bit).
pub const WEBHOOK_SECRET_LEN: usize = 32;
/// Version tag of the signature scheme in the `webhook-signature` header.
const SIGNATURE_VERSION: &str = "v1";
/// Shortest secret accepted when parsing (Standard Webhooks allows 24 to 64 bytes).
const MIN_SECRET_LEN: usize = 24;
const MAX_SECRET_LEN: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WebhookSignatureError {
  #[error("the webhook secret is malformed")]
  MalformedSecret,
  #[error("the timestamp is outside the tolerance")]
  StaleTimestamp,
  #[error("no signature matches")]
  NoMatch,
}

/// A webhook's signing secret. Zeroized on drop, never printed.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct WebhookSecret(Vec<u8>);

/// What a signature covers.
#[derive(Debug, Clone, Copy)]
pub struct WebhookMessage<'a> {
  /// The event's ID (`webhook-id`), the same for every attempt.
  pub id: &'a str,
  /// Seconds since the Unix epoch at sending (`webhook-timestamp`).
  pub timestamp_secs: i64,
  /// The exact request body.
  pub body: &'a [u8],
}

impl WebhookSecret {
  /// A fresh random secret.
  pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
    let mut bytes = vec![0u8; WEBHOOK_SECRET_LEN];
    rng.fill_bytes(&mut bytes);
    Self(bytes)
  }

  /// Takes raw secret bytes, e.g. after unsealing them from the database.
  pub fn from_bytes(bytes: &[u8]) -> Result<Self, WebhookSignatureError> {
    if !(MIN_SECRET_LEN..=MAX_SECRET_LEN).contains(&bytes.len()) {
      return Err(WebhookSignatureError::MalformedSecret);
    }
    Ok(Self(bytes.to_vec()))
  }

  /// Parses the text form `whsec_<base64>`.
  pub fn from_text(text: &str) -> Result<Self, WebhookSignatureError> {
    let encoded = text
      .strip_prefix(WEBHOOK_SECRET_PREFIX)
      .ok_or(WebhookSignatureError::MalformedSecret)?;
    let bytes = Zeroizing::new(
      STANDARD
        .decode(encoded)
        .map_err(|_| WebhookSignatureError::MalformedSecret)?,
    );
    Self::from_bytes(&bytes)
  }

  /// The text form shown to the admin once, `whsec_<base64>`.
  pub fn to_text(&self) -> Zeroizing<String> {
    Zeroizing::new(format!("{WEBHOOK_SECRET_PREFIX}{}", STANDARD.encode(&self.0)))
  }

  /// Raw bytes, for sealing. Never log or store them unsealed.
  pub fn expose_secret(&self) -> &[u8] {
    &self.0
  }

  /// The `webhook-signature` header value: `v1,<base64 of the HMAC>`.
  pub fn sign(&self, message: &WebhookMessage<'_>) -> String {
    let tag = self.mac(message).finalize().into_bytes();
    format!("{SIGNATURE_VERSION},{}", STANDARD.encode(tag))
  }

  /// Checks a `webhook-signature` header (space-separated `v1,<base64>` entries, any of which may
  /// match) and that the timestamp is within `tolerance_secs` of `now_secs`. What a receiver does;
  /// the server uses it in tests and the documentation shows it.
  pub fn verify(
    &self,
    message: &WebhookMessage<'_>,
    header: &str,
    now_secs: i64,
    tolerance_secs: i64,
  ) -> Result<(), WebhookSignatureError> {
    if now_secs.saturating_sub(message.timestamp_secs).saturating_abs() > tolerance_secs {
      return Err(WebhookSignatureError::StaleTimestamp);
    }
    let matches = header
      .split(' ')
      .filter_map(|entry| entry.split_once(','))
      .filter(|(version, _)| *version == SIGNATURE_VERSION)
      .filter_map(|(_, encoded)| STANDARD.decode(encoded).ok())
      // `verify_slice` compares in constant time.
      .any(|tag| self.mac(message).verify_slice(&tag).is_ok());
    if matches {
      Ok(())
    } else {
      Err(WebhookSignatureError::NoMatch)
    }
  }

  fn mac(&self, message: &WebhookMessage<'_>) -> Hmac<Sha256> {
    // Invariant: HMAC takes keys of any length.
    let mut mac = match Hmac::<Sha256>::new_from_slice(&self.0) {
      Ok(mac) => mac,
      Err(_) => unreachable!("HMAC-SHA-256 rejects no key length"),
    };
    mac.update(message.id.as_bytes());
    mac.update(b".");
    mac.update(message.timestamp_secs.to_string().as_bytes());
    mac.update(b".");
    mac.update(message.body);
    mac
  }
}

impl fmt::Debug for WebhookSecret {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("WebhookSecret([redacted])")
  }
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;

  // Example of the Standard Webhooks specification.
  const SPEC_SECRET: &str = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw";
  const SPEC_ID: &str = "msg_p5jXN8AQM9LWM0D4loKWxJek";
  const SPEC_TIMESTAMP: i64 = 1_614_265_330;
  const SPEC_BODY: &[u8] = br#"{"test": 2432232314}"#;
  const SPEC_SIGNATURE: &str = "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=";
  const TOLERANCE_SECS: i64 = 300;

  fn spec_message() -> WebhookMessage<'static> {
    WebhookMessage {
      id: SPEC_ID,
      timestamp_secs: SPEC_TIMESTAMP,
      body: SPEC_BODY,
    }
  }

  #[test]
  fn matches_the_specification_example() {
    let secret = WebhookSecret::from_text(SPEC_SECRET).unwrap();
    assert_eq!(secret.sign(&spec_message()), SPEC_SIGNATURE);
  }

  #[test]
  fn a_signature_verifies_within_the_tolerance() {
    let secret = WebhookSecret::generate(&mut OsRng);
    let header = secret.sign(&spec_message());
    assert_eq!(
      secret.verify(&spec_message(), &header, SPEC_TIMESTAMP + 10, TOLERANCE_SECS),
      Ok(())
    );
    // Several signatures (a rotation): any one may match.
    let several = format!("v1,AAAA {header}");
    assert_eq!(
      secret.verify(&spec_message(), &several, SPEC_TIMESTAMP, TOLERANCE_SECS),
      Ok(())
    );
  }

  #[test]
  fn a_replayed_old_request_is_refused() {
    let secret = WebhookSecret::generate(&mut OsRng);
    let header = secret.sign(&spec_message());
    assert_eq!(
      secret.verify(&spec_message(), &header, SPEC_TIMESTAMP + 3600, TOLERANCE_SECS),
      Err(WebhookSignatureError::StaleTimestamp)
    );
  }

  #[test]
  fn a_changed_body_timestamp_or_key_does_not_verify() {
    let secret = WebhookSecret::generate(&mut OsRng);
    let header = secret.sign(&spec_message());
    let changed_body = WebhookMessage {
      body: br#"{"test": 1}"#,
      ..spec_message()
    };
    let changed_time = WebhookMessage {
      timestamp_secs: SPEC_TIMESTAMP + 1,
      ..spec_message()
    };
    for message in [changed_body, changed_time] {
      assert_eq!(
        secret.verify(&message, &header, SPEC_TIMESTAMP, TOLERANCE_SECS),
        Err(WebhookSignatureError::NoMatch)
      );
    }
    let other = WebhookSecret::generate(&mut OsRng);
    assert_eq!(
      other.verify(&spec_message(), &header, SPEC_TIMESTAMP, TOLERANCE_SECS),
      Err(WebhookSignatureError::NoMatch)
    );
    assert_eq!(
      secret.verify(&spec_message(), "v2,abc garbage", SPEC_TIMESTAMP, TOLERANCE_SECS),
      Err(WebhookSignatureError::NoMatch)
    );
  }

  #[test]
  fn the_text_form_round_trips_and_bad_secrets_are_refused() {
    let secret = WebhookSecret::generate(&mut OsRng);
    let text = secret.to_text();
    assert!(text.starts_with(WEBHOOK_SECRET_PREFIX));
    let parsed = WebhookSecret::from_text(&text).unwrap();
    assert_eq!(parsed.expose_secret(), secret.expose_secret());
    for bad in ["MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw", "whsec_!!!", "whsec_AAAA"] {
      assert_eq!(
        WebhookSecret::from_text(bad).err(),
        Some(WebhookSignatureError::MalformedSecret),
        "{bad}"
      );
    }
    assert!(!format!("{secret:?}").contains(&text[6..]));
  }
}
