//! A call through the whole stack (`--features integration`, needs `DATABASE_URL`): the server
//! issues join tokens and rings, the SFU verifies the tokens and reports joins, and two headless
//! native clients with device-bound DAVE keys hear each other's test tones.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;
use std::net::{Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use rand_core::OsRng;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_crypto::identity::{DeviceKeyPair, IdentityKeyPair};
use teamcraft_crypto::ids::{DeviceId as CryptoDeviceId, UserId as CryptoUserId};
use teamcraft_media::audio_io::AudioSource;
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_auth::{CallKeyAuthenticator, DeviceCallKeyAuthenticator};
use teamcraft_media::call_engine::CallStats;
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_native::{NativeCall, NativeCallConfig};
use teamcraft_proto::{DeviceId, GatewayEvent, MediaCapabilities, UserId};
use teamcraft_server::config::CallsConfig;
use teamcraft_server::testing::{TEST_ISSUER, TestApp, TestUser};
use teamcraft_sfu::auth::{JwtJoinTokenConfig, JwtJoinTokenVerifier};
use teamcraft_sfu::config::SfuConfig;
use teamcraft_sfu::reporting::HttpReporter;
use teamcraft_sfu::server::{CALL_PATH, Sfu};
use wtransport::Identity;

/// Throwaway Ed25519 key pair used only by these tests. It protects nothing.
const SIGNING_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MC4CAQAwBQYDK2VwBCIEIAxxS2yuWa9SUUAfZhI0772UQ48VXZLz45+L2A5IfcWA
-----END PRIVATE KEY-----
";
const PUBLIC_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEA8YVjY2Zv9XPhv003tFAgzW7J6gfDn+ICj5xCB2LXMVg=
-----END PUBLIC KEY-----
";
const SFU_TOKEN: &str = "sfu-registration-token-for-the-e2e-test";
const CALL_DURATION: Duration = Duration::from_secs(3);
const QUIET: Duration = Duration::from_millis(400);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dm_call_rings_and_both_sides_hear_each_other() {
  // The SFU binds first so the server knows its URL; it learns the server's URL afterwards.
  let sfu = bind_sfu();
  let port = sfu.local_addr().unwrap().port();
  let certificate_hash = *sfu.certificate_hash().as_ref();
  let sfu_url = format!(
    "https://{}{CALL_PATH}",
    SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port)
  );
  let Some(app) = TestApp::start_with_calls_config(&calls_config(&sfu_url), SIGNING_KEY_PEM).await else {
    return;
  };
  tokio::spawn(
    sfu
      .with_reporter(Arc::new(HttpReporter::new(app.base_url.clone(), SFU_TOKEN)))
      .run(),
  );

  let (alice, bob) = (owner(), member("bob"));
  let (alice_id, bob_id) = (user_id(&app, &alice).await, user_id(&app, &bob).await);
  let (alice_device, bob_device) = (
    register_device(&app, &alice).await,
    register_device(&app, &bob).await,
  );
  let dm = app.post(&alice, "/dms", json!({ "user_id": bob_id })).await.id();
  let mut bob_socket = ws::connect(&app, &bob, &bob_device).await;
  ws::events(&mut bob_socket, QUIET).await;
  let mut authenticators = authenticators(&[(&alice_id, &alice_device), (&bob_id, &bob_device)]);

  // Alice starts the call; Bob's devices ring once the SFU reports her join.
  let alice_call = join(
    &app,
    &alice,
    &alice_id,
    &alice_device,
    &dm,
    &bob_device,
    &mut authenticators,
    certificate_hash,
    330.0,
  )
  .await;
  let events = ws::events(&mut bob_socket, Duration::from_secs(2)).await;
  assert!(
    events
      .iter()
      .any(|event| matches!(event, GatewayEvent::CallRing { .. })),
    "Bob was not rung: {events:?}"
  );

  // Bob accepts.
  let bob_call = join(
    &app,
    &bob,
    &bob_id,
    &bob_device,
    &dm,
    &alice_device,
    &mut authenticators,
    certificate_hash,
    550.0,
  )
  .await;
  let (alice_stats, bob_stats) = tokio::join!(run_call(alice_call), run_call(bob_call));

  for stats in [&alice_stats, &bob_stats] {
    assert!(stats.frames_received > 50, "{stats:?}");
    assert!(stats.audible_frames_played > 30, "{stats:?}");
  }
  assert_eq!(alice_stats.maybe_privacy_code, bob_stats.maybe_privacy_code);

  // Both left: the server ends the call.
  let events = ws::events(&mut bob_socket, Duration::from_secs(2)).await;
  assert!(
    events
      .iter()
      .any(|event| matches!(event, GatewayEvent::CallEnd { .. })),
    "no call_end: {events:?}"
  );
  app.stop().await;
}

/// Requests a join token from the server and joins through the SFU with a test tone.
#[allow(clippy::too_many_arguments)]
async fn join(
  app: &TestApp,
  user: &TestUser,
  user_id: &str,
  device: &str,
  conversation: &str,
  other_device: &str,
  authenticators: &mut HashMap<String, Arc<dyn CallKeyAuthenticator>>,
  certificate_hash: [u8; 32],
  frequency_hz: f32,
) -> NativeCall {
  let reply = app
    .post(
      user,
      &format!("/conversations/{conversation}/call/join-token"),
      json!({ "device_id": device }),
    )
    .await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  let engine = CallEngineConfig {
    user_id: user_id.parse::<UserId>().unwrap(),
    device_id: device.parse::<DeviceId>().unwrap(),
    capabilities: MediaCapabilities::default(),
    bwe: BweConfig::default(),
  };
  let mut config = NativeCallConfig::new(
    reply.body["sfu_url"].as_str().unwrap().to_owned(),
    reply.body["token"].as_str().unwrap().to_owned(),
    engine,
  );
  config.maybe_certificate_hash = Some(certificate_hash);
  config.source = AudioSource::Tone {
    frequency_hz,
    amplitude: 0.3,
  };
  config.maybe_trusted_roster = Some(vec![other_device.parse().unwrap()]);
  config.maybe_authenticator = authenticators.remove(device);
  NativeCall::join(config).await.unwrap()
}

/// Lets the call run, fails on trust problems, then leaves.
async fn run_call(mut call: NativeCall) -> CallStats {
  let deadline = tokio::time::Instant::now() + CALL_DURATION;
  while let Ok(Some(event)) = tokio::time::timeout_at(deadline, call.next_event()).await {
    assert!(
      !matches!(
        event,
        CallEngineEvent::UntrustedRoster | CallEngineEvent::UnauthenticatedKeys | CallEngineEvent::Error(_)
      ),
      "{event:?}"
    );
  }
  call.leave().await.unwrap()
}

fn bind_sfu() -> Sfu {
  let config = SfuConfig {
    bind_addr: SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    ..SfuConfig::default()
  };
  let verifier = JwtJoinTokenVerifier::new(
    PUBLIC_KEY_PEM.as_bytes(),
    &JwtJoinTokenConfig {
      issuer: TEST_ISSUER.into(),
      leeway: config.join_token_leeway,
      max_lifetime: config.max_join_token_lifetime,
    },
  )
  .unwrap();
  let identity = Identity::self_signed(["localhost", "127.0.0.1", "::1"]).unwrap();
  Sfu::bind(config, identity, Arc::new(verifier)).unwrap()
}

fn calls_config(sfu_url: &str) -> CallsConfig {
  CallsConfig {
    sfu_url: sfu_url.into(),
    issuer: TEST_ISSUER.into(),
    signing_key_path: PathBuf::from("unused.pem"),
    join_token_ttl_secs: 60,
    call_idle_secs: 3600,
    maybe_sfu_registration_token: Some(SFU_TOKEN.to_owned().into()),
    ring_timeout_secs: 30,
  }
}

/// Device identities for the given (user, device) ids; every client trusts every certificate.
fn authenticators(devices: &[(&str, &str)]) -> HashMap<String, Arc<dyn CallKeyAuthenticator>> {
  let mut key_pairs = Vec::new();
  let mut verified = HashMap::new();
  for (user, device) in devices {
    let identity = IdentityKeyPair::generate(&mut OsRng);
    let crypto_user = CryptoUserId::new(user.parse().unwrap());
    let key_pair = DeviceKeyPair::generate(
      crypto_user,
      CryptoDeviceId::new(device.parse().unwrap()),
      &mut OsRng,
    );
    let certificate = identity
      .certify_device(crypto_user, &key_pair.public_keys())
      .unwrap();
    verified.insert(
      device.parse::<DeviceId>().unwrap(),
      certificate.verify(&identity.public_key()).unwrap(),
    );
    key_pairs.push((device.to_string(), key_pair));
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
