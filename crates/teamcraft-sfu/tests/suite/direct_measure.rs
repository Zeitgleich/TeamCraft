//! Measurements of direct media (#127) on a shaped network, by hand: not part of the test run.
//!
//! One process runs the SFU, two more run one client each, every client in a network namespace of
//! its own whose link is shaped (docs/performance.md, "Direct media"). Roles and settings come from
//! the environment:
//!
//! - `MEASURE_ROLE`: `sfu`, `1` or `2`
//! - `MEASURE_DIR`: where the SFU writes its port and certificate hash, and everybody its numbers
//! - `MEASURE_SFU_HOST`: the address the clients reach the SFU at
//! - `MEASURE_DIRECT`: `1` lets the clients go direct, `0` keeps them on the SFU
//! - `MEASURE_SECS`: how long the clients stay in the call
//! - `MEASURE_REFLECT`: `1` gives the SFU its second port that only reflects (hard NATs show)
//! - `MEASURE_FALLBACK`: `1` gives the SFU its WebSocket fallback, for networks that block UDP
//!
//! The NAT matrix (docs/performance.md, "Direct media through NATs") runs it in namespaces behind
//! routers of each NAT type: `scripts/nat-matrix.sh`.
//!
//! `cargo test -p teamcraft-sfu --test suite -- --ignored direct_measure::run --nocapture`

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
  collections::HashMap,
  fmt::Write as _,
  net::{Ipv6Addr, SocketAddr},
  path::PathBuf,
  sync::Arc,
  time::{Duration, Instant},
};

use teamcraft_client_api::video_stats::MediaPath;
use teamcraft_crypto::{
  identity::{DeviceKeyPair, IdentityKeyPair},
  ids::{DeviceId as CryptoDeviceId, UserId as CryptoUserId},
  secret::SymmetricKey,
};
use teamcraft_media::{
  audio_io::{AudioSink, AudioSource},
  bwe::BweConfig,
  call_auth::{CallKeyAuthenticator, DeviceCallKeyAuthenticator},
  call_engine::{CallEngineConfig, CallEngineEvent},
  call_native::{DirectPathConfig, NativeCall, NativeCallConfig},
};
use teamcraft_proto::{CallPeerStanding, DeviceId, DirectMediaPolicy, MediaCapabilities};
use teamcraft_sfu::{auth::DevTokenVerifier, config::SfuConfig, metrics::SfuMetrics, server::Sfu};
use wtransport::Identity;

const CALL_ID: u64 = 1270;
const DEVICES: [u64; 2] = [1, 2];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "a measurement on a shaped network, run by hand"]
async fn run() {
  let dir = PathBuf::from(std::env::var("MEASURE_DIR").unwrap());
  let secs: u64 = std::env::var("MEASURE_SECS").map_or(30, |secs| secs.parse().unwrap());
  match std::env::var("MEASURE_ROLE").unwrap().as_str() {
    "sfu" => run_sfu(&dir, secs).await,
    role => run_client(&dir, role.parse().unwrap(), secs).await,
  }
}

/// The SFU, and the bytes it forwarded, once a second.
async fn run_sfu(dir: &std::path::Path, secs: u64) {
  let is_on = |name: &str| std::env::var(name).as_deref() == Ok("1");
  let mut config = SfuConfig {
    bind_addr: SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    maybe_reflect_bind_addr: is_on("MEASURE_REFLECT").then_some(SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))),
    ..SfuConfig::default()
  };
  if is_on("MEASURE_FALLBACK") {
    config.fallback.maybe_bind_addr = Some(SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)));
  }
  let host = std::env::var("MEASURE_SFU_HOST").unwrap();
  let identity = Identity::self_signed(["localhost", host.as_str()]).unwrap();
  let sfu = Sfu::bind(config, identity, Arc::new(DevTokenVerifier)).unwrap();
  let port = sfu.local_addr().unwrap().port();
  let fallback_port = sfu.fallback_addr().unwrap().map_or(0, |addr| addr.port());
  let hash = hex(sfu.certificate_hash().as_ref());
  let control = sfu.control();
  let metrics = SfuMetrics::new(control.quality()).unwrap();
  tokio::spawn(sfu.run());
  std::fs::write(dir.join("sfu.info"), format!("{port} {hash} {fallback_port}")).unwrap();
  let start = Instant::now();
  let mut lines = String::new();
  let extra_secs: u64 = std::env::var("MEASURE_SFU_EXTRA_SECS").map_or(30, |secs| secs.parse().unwrap());
  while start.elapsed() < Duration::from_secs(secs + extra_secs) {
    tokio::time::sleep(Duration::from_secs(1)).await;
    let text = metrics
      .render(control.room_stats(), control.fallback_connections())
      .unwrap();
    let forwarded = text
      .lines()
      .find_map(|line| line.strip_prefix("teamcraft_sfu_forwarded_bytes_total "))
      .unwrap_or("0");
    writeln!(lines, "{} {forwarded}", start.elapsed().as_secs()).unwrap();
    std::fs::write(dir.join("sfu.bytes"), &lines).unwrap();
  }
}

/// One client: the call's path, both round trips and the audio heard, once a second.
async fn run_client(dir: &std::path::Path, device: u64, secs: u64) {
  let info = std::fs::read_to_string(dir.join("sfu.info")).unwrap();
  let mut fields = info.split_whitespace();
  let (port, hash) = (fields.next().unwrap(), fields.next().unwrap());
  let fallback_port = fields.next().unwrap_or("0");
  let host = std::env::var("MEASURE_SFU_HOST").unwrap();
  let is_direct_allowed = std::env::var("MEASURE_DIRECT").as_deref() == Ok("1");
  let engine = CallEngineConfig {
    user_id: id_of(device),
    device_id: id_of(device),
    capabilities: MediaCapabilities::default(),
    bwe: BweConfig::default(),
  };
  let mut config = NativeCallConfig::new(
    format!("https://{host}:{port}/call"),
    format!("dev:{CALL_ID}:{device}:{device}"),
    engine,
  );
  config.maybe_certificate_hash = Some(unhex(hash));
  if fallback_port != "0" {
    config.maybe_fallback_url = Some(format!(
      "ws://{host}:{fallback_port}{}",
      teamcraft_proto::CALL_FALLBACK_PATH
    ));
  }
  config.source = AudioSource::Tone {
    frequency_hz: 300.0 + 200.0 * device as f32,
    amplitude: 0.3,
  };
  config.sink = AudioSink::Frames;
  config.maybe_trusted_roster = Some(
    DEVICES
      .iter()
      .filter(|other| **other != device)
      .map(|id| id_of(*id))
      .collect(),
  );
  config.maybe_authenticator = authenticators().remove(&device);
  config.maybe_direct = Some(DirectPathConfig {
    policy: DirectMediaPolicy::default(),
    does_user_allow: is_direct_allowed,
    own: CallPeerStanding::Member,
    standings: DEVICES
      .iter()
      .map(|n| (id_of(*n), CallPeerStanding::Member))
      .collect(),
  });
  let mut call = NativeCall::join(config).await.unwrap();
  let start = Instant::now();
  let mut lines = String::new();
  while start.elapsed() < Duration::from_secs(secs) {
    let second_end = tokio::time::Instant::now() + Duration::from_secs(1);
    let mut heard = 0;
    while let Ok(Some(event)) = tokio::time::timeout_at(second_end, call.next_event()).await {
      if matches!(event, CallEngineEvent::AudioFrame { .. }) {
        heard += 1;
      }
    }
    let report = call.video_stats().await.unwrap_or_default();
    let (path, direct_rtt) = match report.maybe_media_path {
      Some(MediaPath::Direct { maybe_rtt_ms }) => ("direct", maybe_rtt_ms),
      _ => ("relay", None),
    };
    writeln!(
      lines,
      "{} {path} sfu_rtt={} direct_rtt={} heard={heard}",
      start.elapsed().as_secs(),
      report.maybe_rtt_ms.map_or("-".into(), |rtt| rtt.to_string()),
      direct_rtt.map_or("-".into(), |rtt| rtt.to_string()),
    )
    .unwrap();
    std::fs::write(dir.join(format!("client{device}.log")), &lines).unwrap();
  }
  call.leave().await.unwrap();
}

/// The same identities in every process: fixed secrets, test only.
fn authenticators() -> HashMap<u64, Arc<dyn CallKeyAuthenticator>> {
  let mut key_pairs = Vec::new();
  let mut verified = HashMap::new();
  for device in DEVICES {
    let seed = u8::try_from(device).unwrap();
    let identity = IdentityKeyPair::from_secret(&SymmetricKey::from_bytes([10 + seed; 32]));
    let user = CryptoUserId::new(1000 + device);
    let key_pair = DeviceKeyPair::from_secrets(
      user,
      CryptoDeviceId::new(device),
      &SymmetricKey::from_bytes([20 + seed; 32]),
      &SymmetricKey::from_bytes([30 + seed; 32]),
    );
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

fn hex(bytes: &[u8]) -> String {
  bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> [u8; 32] {
  let bytes: Vec<u8> = (0..text.len())
    .step_by(2)
    .map(|index| u8::from_str_radix(&text[index..index + 2], 16).unwrap())
    .collect();
  bytes.try_into().unwrap()
}

fn id_of<T: std::str::FromStr>(raw: u64) -> T
where
  T::Err: std::fmt::Debug,
{
  raw.to_string().parse().unwrap()
}
