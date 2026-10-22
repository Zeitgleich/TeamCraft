//! OIDC attestation of the identity key (e2ee.md §3.1), Keycloak flavour.
//!
//! The client writes the attestation claim (`"<user id>:<fingerprint>"`,
//! [`crate::PendingEnrollment::attestation_claim`]) into the user attribute `tc_identity_key` via the
//! Keycloak account API, authenticated by the user's own access token. After the app refreshes its
//! tokens, the ID token carries it as claim `tc_identity_key`; that token is the attestation passed
//! to enrollment ([`crate::EnrollmentOptions::maybe_attestation`]), and colleagues' clients verify
//! it against the identity provider's keys before pinning (`ClientSettings::maybe_attestation`).
//! The realm must sign ID tokens with ES256 or EdDSA.
//!
//! Optional by design: Keycloak only stores the attribute if the realm's user profile declares it
//! (or allows unmanaged attributes). Without that, enrollment proceeds with trust on first use.

use serde_json::Value;

/// The user attribute and claim name (dev realm mapper: dev/keycloak/teamcraft-dev-realm.json).
pub const IDENTITY_KEY_ATTRIBUTE: &str = "tc_identity_key";

#[derive(Debug, thiserror::Error)]
pub enum AttestationError {
  #[error("identity provider unreachable: {0}")]
  Network(String),
  #[error("account API answered {0}")]
  Status(u16),
  #[error("unreadable account API response: {0}")]
  Decode(String),
  /// The access token may not use the account API (it needs the `account` audience and the
  /// `manage-account` role); attestation is unavailable with this identity provider setup.
  #[error("the access token may not use the identity provider's account API")]
  NotPermitted,
  /// The realm's user profile doesn't allow the attribute; attestation is unavailable.
  #[error("the identity provider did not store the {IDENTITY_KEY_ATTRIBUTE} attribute")]
  AttributeNotStored,
}

/// Writes `claim` into the user's `tc_identity_key` attribute through the Keycloak account API of
/// `issuer` (e.g. `https://id.example.com/realms/company`).
pub async fn write_identity_attribute(
  http: &reqwest::Client,
  issuer: &str,
  access_token: &str,
  claim: &str,
) -> Result<(), AttestationError> {
  let url = format!("{}/account", issuer.trim_end_matches('/'));
  let mut account = get_account(http, &url, access_token).await?;
  let attributes = account
    .as_object_mut()
    .ok_or_else(|| AttestationError::Decode("account is not an object".into()))?
    .entry("attributes")
    .or_insert_with(|| Value::Object(Default::default()));
  if let Some(map) = attributes.as_object_mut() {
    map.insert(
      IDENTITY_KEY_ATTRIBUTE.into(),
      Value::Array(vec![Value::String(claim.into())]),
    );
  }
  let response = http
    .post(&url)
    .bearer_auth(access_token)
    .json(&account)
    .send()
    .await
    .map_err(|error| AttestationError::Network(error.to_string()))?;
  if !response.status().is_success() {
    return Err(match response.status().as_u16() {
      // Keycloak rejects unknown attributes when unmanaged attributes are disabled.
      400 => AttestationError::AttributeNotStored,
      status => AttestationError::Status(status),
    });
  }
  // Some realm settings accept the request but drop the attribute silently.
  let stored = get_account(http, &url, access_token).await?;
  let is_stored = stored
    .pointer(&format!("/attributes/{IDENTITY_KEY_ATTRIBUTE}/0"))
    .and_then(Value::as_str)
    .is_some_and(|value| value == claim);
  if is_stored {
    Ok(())
  } else {
    Err(AttestationError::AttributeNotStored)
  }
}

async fn get_account(
  http: &reqwest::Client,
  url: &str,
  access_token: &str,
) -> Result<Value, AttestationError> {
  let response = http
    .get(url)
    .bearer_auth(access_token)
    .header("Accept", "application/json")
    .send()
    .await
    .map_err(|error| AttestationError::Network(error.to_string()))?;
  match response.status().as_u16() {
    401 | 403 => return Err(AttestationError::NotPermitted),
    status if !(200..300).contains(&status) => return Err(AttestationError::Status(status)),
    _ => {},
  }
  response
    .json()
    .await
    .map_err(|error| AttestationError::Decode(error.to_string()))
}
