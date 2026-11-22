//! The SFU reports joins, leaves, its roster and recordings to the server's internal endpoints
//! with its registration token, in the order they happened.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
  collections::HashMap,
  net::{Ipv6Addr, SocketAddr},
  sync::Arc,
  time::Duration,
};

use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};
use rand_core::OsRng;
use serde_json::Value;
use teamcraft_crypto::{
  identity::{DeviceKeyPair, IdentityKeyPair},
  ids::{DeviceId as CryptoDeviceId, UserId as CryptoUserId},
};
use teamcraft_media::{
  audio_io::{AudioSink, AudioSource},
  bwe::BweConfig,
  call_auth::{CallKeyAuthenticator, DeviceCallKeyAuthenticator},
  call_engine::{CallEngineConfig, CallEngineEvent},
  call_native::{NativeCall, NativeCallConfig},
  call_recording::CallRecorder,
};
use teamcraft_proto::{
  CallRosterEntry, DeviceId, MediaCapabilities, SfuCallRoster, SfuParticipantReport, SfuRecordingReport,
  SfuRecordingState, SfuRosterReport,
};
use teamcraft_sfu::{
  auth::{DevTokenVerifier, JoinGrant},
  config::SfuConfig,
  reporting::{HttpReporter, ParticipantReporter},
  server::{CALL_PATH, Sfu},
};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use wtransport::Identity;

const REGISTRATION_TOKEN: &str = "sfu-registration-test-token";

/// What the fake server received: the kind (`joined`, `left`, `roster`, `recording`), the `Authorization`
/// header and the body.
type Recorded = (String, Option<String>, Value);
type Reports = mpsc::UnboundedReceiver<Recorded>;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn joins_and_leaves_are_reported_with_the_registration_token() {
  let (server_url, mut reports) = start_fake_server().await;
  let (port, certificate_hash) = start_sfu(&server_url).await;

  let engine = CallEngineConfig {
    user_id: "7".parse().unwrap(),
    device_id: "70".parse().unwrap(),
    capabilities: MediaCapabilities::default(),
    bwe: BweConfig::default(),
  };
  let sfu_url = format!(
    "https://{}{CALL_PATH}",
    SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port)
  );
  let mut config = NativeCallConfig::new(sfu_url, "dev:5:7:70".into(), engine);
  config.maybe_certificate_hash = Some(certificate_hash);
  let mut call = NativeCall::join(config).await.unwrap();
  // Joined means the SFU accepted (and reported) us.
  let joined = tokio::time::timeout(Duration::from_secs(5), async {
    while let Some(event) = call.next_event().await {
      if matches!(event, CallEngineEvent::Joined { .. }) {
        return true;
      }
    }
    false
  })
  .await
  .unwrap();
  assert!(joined);
  call.leave().await.unwrap();

  let mut recorded = Vec::new();
  while recorded.len() < 2 {
    let (kind, authorization, body) = next(&mut reports).await;
    assert_eq!(
      authorization.as_deref(),
      Some(format!("Bearer {REGISTRATION_TOKEN}").as_str())
    );
    // The roster the SFU reports at start comes in between too.
    if kind == "roster" {
      continue;
    }
    let report: SfuParticipantReport = serde_json::from_value(body).unwrap();
    assert_eq!(report.call_id, "5".parse().unwrap());
    assert_eq!(report.user_id, "7".parse().unwrap());
    assert_eq!(report.device_id, "70".parse().unwrap());
    recorded.push(kind);
  }
  assert_eq!(recorded, vec!["joined", "left"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sfu_reports_its_roster_when_it_starts() {
  let (server_url, mut reports) = start_fake_server().await;
  start_sfu(&server_url).await;

  let (kind, authorization, body) = next(&mut reports).await;

  assert_eq!(kind, "roster");
  assert_eq!(
    authorization.as_deref(),
    Some(format!("Bearer {REGISTRATION_TOKEN}").as_str())
  );
  let roster: SfuRosterReport = serde_json::from_value(body).unwrap();
  assert!(roster.calls.is_empty(), "a fresh SFU has no calls: {roster:?}");
}

#[tokio::test]
async fn reports_reach_the_server_in_the_order_they_were_made() {
  let (server_url, mut reports) = start_fake_server().await;
  let reporter = HttpReporter::new(server_url, REGISTRATION_TOKEN);
  let (laptop, phone) = (grant(70), grant(71));
  let roster = SfuRosterReport {
    uptime_secs: 60,
    calls: vec![SfuCallRoster {
      call_id: "5".parse().unwrap(),
      participants: vec![CallRosterEntry {
        user_id: laptop.user_id,
        device_id: laptop.device_id,
      }],
      maybe_conversation_id: None,
      maybe_started_by: None,
      maybe_started_at: None,
    }],
    maybe_quality: None,
  };

  // A quick move to another device: the old device's leave comes after its join, and the
  // roster between them shows only what was true at that point.
  reporter.joined(laptop);
  reporter.roster(roster.clone());
  reporter.joined(phone);
  reporter.left(laptop);
  reporter.left(phone);

  let mut kinds = Vec::new();
  for _ in 0..5 {
    let (kind, _, body) = next(&mut reports).await;
    let label = match kind.as_str() {
      "roster" => {
        assert_eq!(serde_json::from_value::<SfuRosterReport>(body).unwrap(), roster);
        "roster".to_owned()
      },
      _ => {
        let report: SfuParticipantReport = serde_json::from_value(body).unwrap();
        format!("{kind} {}", report.device_id)
      },
    };
    kinds.push(label);
  }
  assert_eq!(
    kinds,
    vec!["joined 70", "roster", "joined 71", "left 70", "left 71"]
  );
}

/// Issue #2117: a device's signed recording notice, sent by the real native client over DAVE,
/// reaches the server as a start with the devices in the call, and its stop as a stop.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_signed_recording_notice_is_reported_as_a_start_and_a_stop() {
  const CALL: u64 = 41;
  const RECORDER: u64 = 1;
  const LISTENER: u64 = 2;
  let (server_url, mut reports) = start_fake_server().await;
  let (port, certificate_hash) = start_sfu(&server_url).await;
  let mut authenticators = authenticators(&[RECORDER, LISTENER]);
  let mut calls = HashMap::new();
  for (device, other) in [(RECORDER, LISTENER), (LISTENER, RECORDER)] {
    let engine = CallEngineConfig {
      user_id: device.to_string().parse().unwrap(),
      device_id: device.to_string().parse().unwrap(),
      capabilities: MediaCapabilities::default(),
      bwe: BweConfig::default(),
    };
    let sfu_url = format!(
      "https://{}{CALL_PATH}",
      SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port)
    );
    let mut config = NativeCallConfig::new(sfu_url, format!("dev:{CALL}:{device}:{device}"), engine);
    config.maybe_certificate_hash = Some(certificate_hash);
    config.source = AudioSource::Silence;
    config.sink = AudioSink::Discard;
    config.maybe_trusted_roster = Some(vec![other.to_string().parse::<DeviceId>().unwrap()]);
    config.maybe_authenticator = authenticators.remove(&device);
    calls.insert(device, NativeCall::join(config).await.unwrap());
  }
  // Both in the call (their joins reported) before the recording starts.
  let mut joined = 0;
  while joined < 2 {
    let (kind, _, _) = next(&mut reports).await;
    joined += usize::from(kind == "joined");
  }

  let folder = std::env::temp_dir().join(format!("teamcraft-sfu-recording-{}", std::process::id()));
  std::fs::create_dir_all(&folder).unwrap();
  let recorder = CallRecorder::start(&folder.join("take"), Duration::from_secs(60), None).unwrap();
  let recording = &calls[&RECORDER];
  // The notice goes out once the call has its DAVE session; until then the engine holds it.
  recording.set_recording(Some(recorder.sink()));
  let started = next_recording(&mut reports).await;
  assert_eq!(started.state, SfuRecordingState::Recording);
  assert_eq!(started.call_id, CALL.to_string().parse().unwrap());
  assert_eq!(started.device_id, RECORDER.to_string().parse().unwrap());
  assert_eq!(started.participants, 2);

  recording.set_recording(None);
  let stopped = next_recording(&mut reports).await;
  assert_eq!(stopped.state, SfuRecordingState::Stopped);
  assert_eq!(stopped.device_id, started.device_id);
  recorder.cancel();
  let _ = std::fs::remove_dir_all(&folder);
  for (_, call) in calls {
    call.leave().await.unwrap();
  }
}

/// The next recording report, skipping the rest (the roster at start, joins).
async fn next_recording(reports: &mut Reports) -> SfuRecordingReport {
  loop {
    let (kind, _, body) = tokio::time::timeout(Duration::from_secs(30), reports.recv())
      .await
      .expect("no recording report arrived")
      .expect("the fake server stopped");
    if kind == "recording" {
      return serde_json::from_value(body).unwrap();
    }
  }
}

/// Device-bound DAVE keys of every device, each verifying the others' certificates.
fn authenticators(devices: &[u64]) -> HashMap<u64, Arc<dyn CallKeyAuthenticator>> {
  let mut key_pairs = Vec::new();
  let mut verified = HashMap::new();
  for &device in devices {
    let identity = IdentityKeyPair::generate(&mut OsRng);
    let user = CryptoUserId::new(device);
    let key_pair = DeviceKeyPair::generate(user, CryptoDeviceId::new(device), &mut OsRng);
    let certificate = identity.certify_device(user, &key_pair.public_keys()).unwrap();
    verified.insert(
      device.to_string().parse::<DeviceId>().unwrap(),
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

/// The next report the fake server received, within five seconds.
async fn next(reports: &mut Reports) -> Recorded {
  tokio::time::timeout(Duration::from_secs(5), reports.recv())
    .await
    .expect("no report arrived")
    .expect("the fake server stopped")
}

/// A stand-in for the server's internal call endpoints that hands on what it receives.
async fn start_fake_server() -> (String, Reports) {
  async fn record(
    kind: &'static str,
    State(reports): State<mpsc::UnboundedSender<Recorded>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
  ) {
    let authorization = headers
      .get("authorization")
      .and_then(|value| value.to_str().ok())
      .map(str::to_owned);
    // The test may be done listening already.
    let _ = reports.send((kind.to_owned(), authorization, body));
  }
  let (sender, receiver) = mpsc::unbounded_channel();
  let router = Router::new()
    .route(
      "/api/internal/v1/calls/participants/joined",
      post(|state, headers, body| record("joined", state, headers, body)),
    )
    .route(
      "/api/internal/v1/calls/participants/left",
      post(|state, headers, body| record("left", state, headers, body)),
    )
    .route(
      "/api/internal/v1/calls/roster",
      post(|state, headers, body| record("roster", state, headers, body)),
    )
    .route(
      "/api/internal/v1/calls/recording",
      post(|state, headers, body| record("recording", state, headers, body)),
    )
    .with_state(sender);
  let listener = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap();
  let address = listener.local_addr().unwrap();
  tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
  (format!("http://{address}"), receiver)
}

async fn start_sfu(server_url: &str) -> (u16, [u8; 32]) {
  let config = SfuConfig {
    bind_addr: SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    // Only the roster at start: the tests don't wait for a second one.
    roster_interval: Duration::from_secs(3600),
    ..SfuConfig::default()
  };
  let identity = Identity::self_signed(["localhost", "127.0.0.1", "::1"]).unwrap();
  let sfu = Sfu::bind(config, identity, Arc::new(DevTokenVerifier))
    .unwrap()
    .with_reporter(Arc::new(HttpReporter::new(server_url, REGISTRATION_TOKEN)));
  let port = sfu.local_addr().unwrap().port();
  let certificate_hash = *sfu.certificate_hash().as_ref();
  tokio::spawn(sfu.run());
  (port, certificate_hash)
}

fn grant(device: i64) -> JoinGrant {
  JoinGrant {
    call_id: "5".parse().unwrap(),
    user_id: "7".parse().unwrap(),
    device_id: device.to_string().parse().unwrap(),
    maybe_conversation_id: None,
  }
}
