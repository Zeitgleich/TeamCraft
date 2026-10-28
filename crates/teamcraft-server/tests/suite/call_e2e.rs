//! A call through the whole stack (`--features integration`, needs `DATABASE_URL`): the server
//! issues join tokens and rings, the SFU verifies the tokens and reports joins, and two headless
//! native clients with device-bound DAVE keys hear each other's test tones.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

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
use teamcraft_server::config::{CallsConfig, MailTexts, MeetingsConfig};
use teamcraft_server::testing::{TEST_ISSUER, TestApp, TestUser};
use teamcraft_sfu::auth::{JwtJoinTokenConfig, JwtJoinTokenVerifier};
use teamcraft_sfu::config::SfuConfig;
use teamcraft_sfu::control::control_router;
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

/// A meeting's host mutes everyone, removes a participant and ends the meeting through the
/// server; the real SFU carries it out on the media connections and the native clients see it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn meeting_host_controls_reach_the_participants_through_the_sfu() {
  let sfu = bind_sfu();
  let port = sfu.local_addr().unwrap().port();
  let certificate_hash = *sfu.certificate_hash().as_ref();
  let sfu_url = format!(
    "https://{}{CALL_PATH}",
    SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port)
  );
  // The SFU's control endpoint listens before the server starts, so the server knows its URL.
  let control_listener = tokio::net::TcpListener::bind((Ipv6Addr::LOCALHOST, 0))
    .await
    .unwrap();
  let control_url = format!("http://{}", control_listener.local_addr().unwrap());
  let mut config = calls_config(&sfu_url);
  config.maybe_sfu_control_url = Some(control_url);
  let Some(app) = TestApp::start_with_meetings_and_calls(&meetings_config(), &config, SIGNING_KEY_PEM).await
  else {
    return;
  };
  let sfu = sfu.with_reporter(Arc::new(HttpReporter::new(app.base_url.clone(), SFU_TOKEN)));
  let router = control_router(sfu.control(), SFU_TOKEN);
  tokio::spawn(async move { axum::serve(control_listener, router).await });
  tokio::spawn(sfu.run());

  let (host, carol, dave) = (owner(), member("carol"), member("dave"));
  let ids = [
    user_id(&app, &host).await,
    user_id(&app, &carol).await,
    user_id(&app, &dave).await,
  ];
  let devices = [
    register_device(&app, &host).await,
    register_device(&app, &carol).await,
    register_device(&app, &dave).await,
  ];
  let created = app
    .post(
      &host,
      "/meetings",
      json!({
        "title": "Review",
        "starts_at": 1_893_488_400_000_i64,
        "ends_at": 1_893_492_000_000_i64,
        "invitees": [
          {"kind": "user", "user_id": ids[1]},
          {"kind": "user", "user_id": ids[2]},
        ],
      }),
    )
    .await;
  assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
  let meeting = created.body["id"].as_str().unwrap().to_owned();
  let chat = created.body["conversation_id"].as_str().unwrap().to_owned();
  let pairs: Vec<(&str, &str)> = ids
    .iter()
    .map(String::as_str)
    .zip(devices.iter().map(String::as_str))
    .collect();
  let mut authenticators = authenticators(&pairs);
  let users = [&host, &carol, &dave];
  let mut calls = Vec::new();
  for index in 0..3 {
    let roster: Vec<&str> = devices
      .iter()
      .enumerate()
      .filter(|(other, _)| *other != index)
      .map(|(_, device)| device.as_str())
      .collect();
    calls.push(
      join_with_roster(
        &app,
        users[index],
        &ids[index],
        &devices[index],
        &chat,
        &roster,
        &mut authenticators,
        certificate_hash,
      )
      .await,
    );
  }
  let mut dave_call = calls.pop().unwrap();
  let mut carol_call = calls.pop().unwrap();
  let mut host_call = calls.pop().unwrap();
  for call in [&mut host_call, &mut carol_call, &mut dave_call] {
    wait_for(call, |event| matches!(event, CallEngineEvent::Secured { .. })).await;
  }
  let controls = format!("/meetings/{meeting}/controls");

  let muted = app.post(&host, &controls, json!({"action": "mute_all"})).await;
  assert_eq!(muted.status, StatusCode::NO_CONTENT, "{}", muted.body);
  for call in [&mut carol_call, &mut dave_call] {
    wait_for(call, |event| matches!(event, CallEngineEvent::MutedByHost { .. })).await;
  }

  let removed = app
    .post(
      &host,
      &controls,
      json!({"action": "remove_participant", "user_id": ids[2]}),
    )
    .await;
  assert_eq!(removed.status, StatusCode::NO_CONTENT, "{}", removed.body);
  wait_for(&mut dave_call, |event| {
    matches!(event, CallEngineEvent::RemovedByHost { .. })
  })
  .await;
  assert!(
    ends(&mut dave_call).await,
    "the removed participant's connection closes"
  );
  let again = app
    .post(
      &dave,
      &format!("/conversations/{chat}/call/join-token"),
      json!({"device_id": devices[2]}),
    )
    .await;
  assert_eq!(again.status, StatusCode::FORBIDDEN, "{}", again.body);

  let ended = app.post(&host, &controls, json!({"action": "end"})).await;
  assert_eq!(ended.status, StatusCode::NO_CONTENT, "{}", ended.body);
  for call in [&mut host_call, &mut carol_call] {
    wait_for(call, |event| matches!(event, CallEngineEvent::EndedByHost { .. })).await;
    assert!(ends(call).await, "everyone is disconnected");
  }
  let after = app
    .post(
      &carol,
      &format!("/conversations/{chat}/call/join-token"),
      json!({"device_id": devices[1]}),
    )
    .await;
  assert_eq!(
    after.status,
    StatusCode::CONFLICT,
    "the meeting is over: {}",
    after.body
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

/// Joins `conversation`'s call trusting `roster`'s devices, sending silence.
#[allow(clippy::too_many_arguments)]
async fn join_with_roster(
  app: &TestApp,
  user: &TestUser,
  user_id: &str,
  device: &str,
  conversation: &str,
  roster: &[&str],
  authenticators: &mut HashMap<String, Arc<dyn CallKeyAuthenticator>>,
  certificate_hash: [u8; 32],
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
  config.maybe_trusted_roster = Some(roster.iter().map(|device| device.parse().unwrap()).collect());
  config.maybe_authenticator = authenticators.remove(device);
  NativeCall::join(config).await.unwrap()
}

/// The next event matching `predicate` within five seconds; fails on trust problems.
async fn wait_for(call: &mut NativeCall, predicate: impl Fn(&CallEngineEvent) -> bool) -> CallEngineEvent {
  tokio::time::timeout(Duration::from_secs(5), async {
    loop {
      let event = call.next_event().await.expect("call ended before the event");
      assert!(
        !matches!(
          event,
          CallEngineEvent::UntrustedRoster | CallEngineEvent::UnauthenticatedKeys
        ),
        "{event:?}"
      );
      if predicate(&event) {
        return event;
      }
    }
  })
  .await
  .expect("event did not arrive")
}

/// Whether the call's event stream ends within five seconds.
async fn ends(call: &mut NativeCall) -> bool {
  tokio::time::timeout(Duration::from_secs(5), async {
    while call.next_event().await.is_some() {}
  })
  .await
  .is_ok()
}

fn meetings_config() -> MeetingsConfig {
  MeetingsConfig {
    public_base_url: "https://chat.example.com".into(),
    organizer_fallback_address: "meetings@example.com".into(),
    caldav: None,
    max_invitees: 5,
    guest_token_secret: "0123456789abcdef0123456789abcdef".to_owned().into(),
    guest_grace_secs: 7200,
    max_shared_link_guests: 50,
    maybe_inbox_token: None,
    smtp: None,
    mail_texts: MailTexts::default(),
  }
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
    maybe_sfu_control_url: None,
    maybe_sfu_certificate_sha256: None,
    roster_grace_secs: 60,
    roster_timeout_secs: 120,
    max_devices_per_user_per_call: 3,
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
