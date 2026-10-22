//! Who may be in a call's DAVE group: call key bindings (e2ee.md §8).
//!
//! The engine asks a [`CallKeyAuthenticator`] to sign its own per-call DAVE key and to verify the
//! bindings of others; [`DeviceCallKeyAuthenticator`] does both with the device key and the verified
//! device certificates from `teamcraft-crypto`. Apps that don't have device keys (development tools)
//! run without one and get [`crate::call_engine::CallEngineEvent::UnauthenticatedKeys`].

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use teamcraft_crypto::call_binding::{CallBindingError, CallId as CryptoCallId, SignedCallKeyBinding};
use teamcraft_crypto::identity::{DeviceKeyPair, VerifiedDevice};
use teamcraft_crypto::ids::DeviceId as CryptoDeviceId;
use teamcraft_proto::{CallId, DeviceId, Id};
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

/// [`CallKeyAuthenticator`] backed by the device key and verified device certificates.
pub struct DeviceCallKeyAuthenticator {
  device: Arc<DeviceKeyPair>,
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
}

impl DeviceCallKeyAuthenticator {
  /// `devices`: the verified certificates of the devices that may be in the call (the trusted
  /// roster), keyed by device id.
  pub fn new(device: Arc<DeviceKeyPair>, devices: HashMap<DeviceId, VerifiedDevice>) -> Self {
    Self { device, devices }
  }
}

impl CallKeyAuthenticator for DeviceCallKeyAuthenticator {
  fn sign(&self, call_id: CallId, dave_public_key: &[u8]) -> Result<Vec<u8>, CallKeyError> {
    let not_after = now_unix_secs()? + BINDING_VALIDITY.as_secs();
    let signed = self
      .device
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
