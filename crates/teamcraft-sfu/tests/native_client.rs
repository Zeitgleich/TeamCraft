//! End to end with the real native client: three headless clients join through this SFU, form the
//! DAVE group with device-bound keys (call key bindings), send Opus test tones and decode each other.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
  collections::HashMap,
  net::{Ipv6Addr, SocketAddr},
  sync::Arc,
  time::Duration,
};

use rand_core::OsRng;
use teamcraft_crypto::{
  identity::{DeviceKeyPair, IdentityKeyPair},
  ids::{DeviceId as CryptoDeviceId, UserId as CryptoUserId},
};
use teamcraft_media::{
  bwe::BweConfig,
  call_auth::{CallKeyAuthenticator, DeviceCallKeyAuthenticator},
  call_engine::{CallEngineConfig, CallEngineEvent},
  call_native::{AudioSink, AudioSource, NativeCall, NativeCallConfig},
};
use teamcraft_proto::{DeviceId, MediaCapabilities};
use teamcraft_sfu::{
  auth::DevTokenVerifier,
  config::SfuConfig,
  server::{CALL_PATH, Sfu},
};
use wtransport::Identity;

const CALL_ID: u64 = 11;
const DEVICES: [u64; 3] = [1, 2, 3];
const CALL_DURATION: Duration = Duration::from_secs(3);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn headless_clients_hear_each_other_through_the_sfu() {
  let (port, certificate_hash) = start_sfu().await;
  let mut authenticators = authenticators();
  let mut calls = Vec::new();
  for (index, device) in DEVICES.into_iter().enumerate() {
    let roster: Vec<DeviceId> = DEVICES
      .iter()
      .filter(|other| **other != device)
      .map(|id| id_of(*id))
      .collect();
    let engine = CallEngineConfig {
      user_id: id_of(device),
      device_id: id_of(device),
      capabilities: MediaCapabilities::default(),
      bwe: BweConfig::default(),
    };
    let sfu_url = format!(
      "https://{}{CALL_PATH}",
      SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port)
    );
    let mut config = NativeCallConfig::new(sfu_url, format!("dev:{CALL_ID}:{device}:{device}"), engine);
    config.maybe_certificate_hash = Some(certificate_hash);
    config.source = AudioSource::Tone {
      frequency_hz: 300.0 + 200.0 * index as f32,
      amplitude: 0.3,
    };
    config.sink = AudioSink::Discard;
    config.maybe_trusted_roster = Some(roster);
    config.maybe_authenticator = authenticators.remove(&device);
    calls.push(NativeCall::join(config).await.unwrap());
  }

  let mut secured = vec![false; calls.len()];
  let deadline = tokio::time::Instant::now() + CALL_DURATION;
  while tokio::time::Instant::now() < deadline {
    for (index, call) in calls.iter_mut().enumerate() {
      if let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(10), call.next_event()).await {
        if matches!(event, CallEngineEvent::Secured { .. }) {
          secured[index] = true;
        }
        assert!(
          !matches!(
            event,
            CallEngineEvent::UntrustedRoster | CallEngineEvent::Error(_)
          ),
          "{event:?}"
        );
      }
    }
  }

  let mut codes = Vec::new();
  for call in calls {
    let stats = call.leave().await.unwrap();
    // Two others for most of the call: well above one second of frames.
    assert!(stats.frames_received > 100, "{stats:?}");
    assert!(stats.audible_frames_played > 50, "{stats:?}");
    assert!(stats.frames_sent > 100, "{stats:?}");
    codes.push(stats.maybe_privacy_code.unwrap());
  }
  assert!(secured.iter().all(|is_secured| *is_secured), "{secured:?}");
  assert!(codes.windows(2).all(|pair| pair[0] == pair[1]), "{codes:?}");
}

/// One identity per device; every client trusts all device certificates of the call.
fn authenticators() -> HashMap<u64, Arc<dyn CallKeyAuthenticator>> {
  let mut key_pairs = Vec::new();
  let mut verified = HashMap::new();
  for device in DEVICES {
    let identity = IdentityKeyPair::generate(&mut OsRng);
    let user = CryptoUserId::new(1000 + device);
    let key_pair = DeviceKeyPair::generate(user, CryptoDeviceId::new(device), &mut OsRng);
    let certificate = identity.certify_device(user, &key_pair.public_keys()).unwrap();
    verified.insert(
      id_of::<DeviceId>(device),
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

async fn start_sfu() -> (u16, [u8; 32]) {
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

fn id_of<T: std::str::FromStr>(raw: u64) -> T
where
  T::Err: std::fmt::Debug,
{
  raw.to_string().parse().unwrap()
}
