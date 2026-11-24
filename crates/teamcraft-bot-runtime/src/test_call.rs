//! For the bots' tests: a real SFU in the same process and call participants with device-bound
//! DAVE keys that verify each other, like two enrolled devices (feature `test-support`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;

use rand_core::OsRng;
use teamcraft_crypto::identity::{DeviceKeyPair, IdentityKeyPair};
use teamcraft_crypto::ids::{DeviceId as CryptoDeviceId, UserId as CryptoUserId};
use teamcraft_media::audio_io::{AudioSink, AudioSource};
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_auth::{CallKeyAuthenticator, DeviceCallKeyAuthenticator};
use teamcraft_media::call_engine::CallEngineConfig;
use teamcraft_media::call_native::NativeCallConfig;
use teamcraft_proto::{DeviceId, MediaCapabilities};
use teamcraft_sfu::auth::DevTokenVerifier;
use teamcraft_sfu::config::SfuConfig;
use teamcraft_sfu::server::{CALL_PATH, Sfu};
use wtransport::Identity;

/// An SFU on a free port of `::1`, with its certificate hash.
pub async fn start_sfu() -> (u16, [u8; 32]) {
  let config = SfuConfig {
    bind_addr: SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    ..SfuConfig::default()
  };
  let identity = Identity::self_signed(["localhost", "127.0.0.1", "::1"]).unwrap();
  let sfu = Sfu::bind(config, identity, Arc::new(DevTokenVerifier)).unwrap();
  let port = sfu.local_addr().unwrap().port();
  let certificate_hash = *sfu.certificate_hash().as_ref();
  tokio::spawn(sfu.run());
  (port, certificate_hash)
}

/// Device-bound DAVE keys of `devices` (user and device id the same number), each verifying the
/// others' certificates.
pub fn authenticators(devices: &[u64]) -> HashMap<u64, Arc<dyn CallKeyAuthenticator>> {
  let mut key_pairs = Vec::new();
  let mut verified = HashMap::new();
  for &device in devices {
    let identity = IdentityKeyPair::generate(&mut OsRng);
    let user = CryptoUserId::new(device);
    let key_pair = DeviceKeyPair::generate(user, CryptoDeviceId::new(device), &mut OsRng);
    let certificate = identity.certify_device(user, &key_pair.public_keys()).unwrap();
    verified.insert(
      id::<DeviceId>(device),
      certificate.verify(&identity.public_key()).unwrap(),
    );
    key_pairs.push((device, key_pair));
  }
  key_pairs
    .into_iter()
    .map(|(device, key_pair)| {
      let authenticator: Arc<dyn CallKeyAuthenticator> = Arc::new(DeviceCallKeyAuthenticator::new(
        Arc::new(key_pair),
        verified.clone(),
      ));
      (device, authenticator)
    })
    .collect()
}

/// The config of `device` in call `call` on the SFU at `port`, trusting `others`: it sends a
/// 220 Hz tone and discards what it hears. A bot's test sets the source and sink it needs.
pub fn call_config(
  port: u16,
  certificate_hash: [u8; 32],
  call: u64,
  device: u64,
  others: &[u64],
  authenticator: Arc<dyn CallKeyAuthenticator>,
) -> NativeCallConfig {
  let engine = CallEngineConfig {
    user_id: id(device),
    device_id: id(device),
    capabilities: MediaCapabilities::default(),
    bwe: BweConfig::default(),
  };
  let sfu_url = format!(
    "https://{}{CALL_PATH}",
    SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port)
  );
  let mut config = NativeCallConfig::new(sfu_url, format!("dev:{call}:{device}:{device}"), engine);
  config.maybe_certificate_hash = Some(certificate_hash);
  config.source = AudioSource::Tone {
    frequency_hz: 220.0,
    amplitude: 0.3,
  };
  config.sink = AudioSink::Discard;
  config.maybe_trusted_roster = Some(others.iter().map(|other| id(*other)).collect());
  config.maybe_authenticator = Some(authenticator);
  config
}

/// An id of any kind from a number.
pub fn id<T: std::str::FromStr>(raw: u64) -> T
where
  T::Err: std::fmt::Debug,
{
  raw.to_string().parse().unwrap()
}
