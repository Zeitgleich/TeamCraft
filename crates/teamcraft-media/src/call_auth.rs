//! Who may be in a call's DAVE group: call key bindings (e2ee.md §8).
//!
//! The engine asks a [`CallKeyAuthenticator`] to sign its own per-call DAVE key and to verify the
//! bindings of others; [`DeviceCallKeyAuthenticator`] does both with a [`CallBindingSigner`] (the
//! device key, or the client core holding it) and the verified device certificates from
//! `teamcraft-crypto`. Apps that don't have device keys (development tools)
//! run without one and get [`crate::call_engine::CallEngineEvent::UnauthenticatedKeys`].

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use teamcraft_crypto::call_binding::{CallBindingError, CallId as CryptoCallId, SignedCallKeyBinding};
use teamcraft_crypto::identity::{DeviceKeyPair, VerifiedDevice};
use teamcraft_crypto::ids::DeviceId as CryptoDeviceId;
use teamcraft_proto::{CallId, DeviceId, Id, Snowflake};
use thiserror::Error;
use web_time::{SystemTime, UNIX_EPOCH};

/// How long this device's binding is valid; it is only needed while joining.
const BINDING_VALIDITY: Duration = Duration::from_secs(600);

/// Signs this device's DAVE key and verifies other devices' bindings.
pub trait CallKeyAuthenticator: fmt::Debug + Send + Sync {
  /// A signed binding of `dave_public_key` to this device for `call_id`.
  fn sign(&self, call_id: CallId, dave_public_key: &[u8]) -> Result<Vec<u8>, CallKeyError>;
  /// Verifies `device_id`'s binding for `call_id`; returns the bound DAVE public key.
  fn verify(&self, call_id: CallId, device_id: DeviceId, binding: &[u8]) -> Result<Vec<u8>, CallKeyError>;
}

/// Signs call key bindings with this device's key. The client core implements it so the private
/// key never leaves it; tests use a [`DeviceKeyPair`] directly.
pub trait CallBindingSigner: Send + Sync {
  fn sign_call_key_binding(
    &self,
    call_id: CryptoCallId,
    dave_public_key: &[u8],
    not_after: u64,
  ) -> Result<SignedCallKeyBinding, CallBindingError>;
}

/// [`CallKeyAuthenticator`] backed by the device key and verified device certificates.
pub struct DeviceCallKeyAuthenticator {
  signer: Arc<dyn CallBindingSigner>,
  devices: HashMap<DeviceId, VerifiedDevice>,
}

#[derive(Debug, Error)]
pub enum CallKeyError {
  #[error("no verified certificate for device {0}")]
  UnknownDevice(DeviceId),
  #[error("the binding is for device {actual}, not {expected}")]
  DeviceMismatch { expected: DeviceId, actual: u64 },
  #[error(transparent)]
  Binding(#[from] CallBindingError),
  #[error("the system clock is before 1970")]
  Clock,
  #[error("device id {0} is not a valid snowflake")]
  InvalidDeviceId(u64),
  /// Signing or verifying happens elsewhere (the web client's page) and failed there.
  #[error("{0}")]
  Remote(String),
}

impl DeviceCallKeyAuthenticator {
  /// `devices`: the verified certificates of the devices that may be in the call (the trusted
  /// roster), keyed by device id.
  pub fn new(signer: Arc<dyn CallBindingSigner>, devices: HashMap<DeviceId, VerifiedDevice>) -> Self {
    Self { signer, devices }
  }

  /// From the verified certificates as the client core lists them (any order).
  pub fn from_verified(
    signer: Arc<dyn CallBindingSigner>,
    devices: Vec<VerifiedDevice>,
  ) -> Result<Self, CallKeyError> {
    let devices = devices
      .into_iter()
      .map(|device| Ok((proto_device_id(device.device_id())?, device)))
      .collect::<Result<_, CallKeyError>>()?;
    Ok(Self::new(signer, devices))
  }

  /// The devices that may be in the call.
  pub fn device_ids(&self) -> impl Iterator<Item = DeviceId> + '_ {
    self.devices.keys().copied()
  }
}

impl CallBindingSigner for DeviceKeyPair {
  fn sign_call_key_binding(
    &self,
    call_id: CryptoCallId,
    dave_public_key: &[u8],
    not_after: u64,
  ) -> Result<SignedCallKeyBinding, CallBindingError> {
    DeviceKeyPair::sign_call_key_binding(self, call_id, dave_public_key, not_after)
  }
}

impl CallKeyAuthenticator for DeviceCallKeyAuthenticator {
  fn sign(&self, call_id: CallId, dave_public_key: &[u8]) -> Result<Vec<u8>, CallKeyError> {
    let not_after = now_unix_secs()? + BINDING_VALIDITY.as_secs();
    let signed = self
      .signer
      .sign_call_key_binding(crypto_call_id(call_id), dave_public_key, not_after)?;
    Ok(signed.to_bytes()?)
  }

  fn verify(&self, call_id: CallId, device_id: DeviceId, binding: &[u8]) -> Result<Vec<u8>, CallKeyError> {
    let device = self
      .devices
      .get(&device_id)
      .ok_or(CallKeyError::UnknownDevice(device_id))?;
    if device.device_id() != crypto_device_id(device_id) {
      return Err(CallKeyError::DeviceMismatch {
        expected: device_id,
        actual: device.device_id().get(),
      });
    }
    let key =
      SignedCallKeyBinding::from_bytes(binding)?.verify(device, crypto_call_id(call_id), now_unix_secs()?)?;
    Ok(key.dave_public_key().to_vec())
  }
}

impl fmt::Debug for DeviceCallKeyAuthenticator {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("DeviceCallKeyAuthenticator")
      .field("devices", &self.devices.len())
      .finish_non_exhaustive()
  }
}

/// Converts the shared snowflake ids into `teamcraft-crypto`'s ids (same `u64` value).
pub fn crypto_device_id(device_id: DeviceId) -> CryptoDeviceId {
  CryptoDeviceId::new(snowflake_u64(device_id.snowflake().get()))
}

fn proto_device_id(device_id: CryptoDeviceId) -> Result<DeviceId, CallKeyError> {
  let raw = device_id.get();
  i64::try_from(raw)
    .ok()
    .and_then(|raw| Snowflake::new(raw).ok())
    .map(DeviceId::from_snowflake)
    .ok_or(CallKeyError::InvalidDeviceId(raw))
}

fn crypto_call_id(call_id: CallId) -> CryptoCallId {
  CryptoCallId::new(snowflake_u64(call_id.snowflake().get()))
}

fn snowflake_u64(raw: i64) -> u64 {
  // Invariant: snowflakes are non-negative.
  u64::try_from(raw).unwrap_or_default()
}

fn now_unix_secs() -> Result<u64, CallKeyError> {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|elapsed| elapsed.as_secs())
    .map_err(|_| CallKeyError::Clock)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn crypto_device_ids_map_to_wire_ids() {
    let device = proto_device_id(CryptoDeviceId::new(7)).unwrap();
    assert_eq!(device.snowflake().get(), 7);
    assert_eq!(crypto_device_id(device).get(), 7);
    assert!(matches!(
      proto_device_id(CryptoDeviceId::new(u64::MAX)),
      Err(CallKeyError::InvalidDeviceId(u64::MAX))
    ));
  }
}
