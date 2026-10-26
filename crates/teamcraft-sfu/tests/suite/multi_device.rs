//! One account in a call from two devices at once (#505): the SFU keys participants by device, so
//! the second device joins next to the first instead of replacing it, and leaving on one keeps the
//! other in.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use teamcraft_media::audio_io::{AudioSink, AudioSource};
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_native::{NativeCall, NativeCallConfig};
use teamcraft_proto::{DeviceId, MediaCapabilities, UserId};
use teamcraft_sfu::auth::DevTokenVerifier;
use teamcraft_sfu::config::SfuConfig;
use teamcraft_sfu::server::{CALL_PATH, Sfu};
use wtransport::Identity;

const CALL_ID: u64 = 51;
const ALICE: u64 = 1;
const BOB: u64 = 2;
const ALICE_LAPTOP: u64 = 11;
const ALICE_PHONE: u64 = 12;
const BOB_DEVICE: u64 = 21;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_device_of_one_account_joins_without_dropping_the_first() {
  let (port, hash) = start_sfu().await;
  let mut laptop = join(port, hash, ALICE, ALICE_LAPTOP).await;
  let mut bob = join(port, hash, BOB, BOB_DEVICE).await;
  wait_for(&mut laptop, |event| {
    matches!(event, CallEngineEvent::Secured { .. })
  })
  .await;
  wait_for(&mut bob, |event| matches!(event, CallEngineEvent::Secured { .. })).await;

  let mut phone = join(port, hash, ALICE, ALICE_PHONE).await;
  wait_for(&mut phone, |event| {
    matches!(event, CallEngineEvent::Secured { .. })
  })
  .await;

  let (laptop_events, bob_events, phone_events) = tokio::join!(
    collect(&mut laptop, Duration::from_secs(2)),
    collect(&mut bob, Duration::from_secs(2)),
    collect(&mut phone, Duration::from_secs(2)),
  );
  for events in [&laptop_events, &bob_events, &phone_events] {
    assert!(
      !events.iter().any(|event| matches!(
        event,
        CallEngineEvent::ParticipantLeft(_) | CallEngineEvent::Reconnecting
      )),
      "{events:?}"
    );
  }
  assert!(
    laptop_events
      .iter()
      .any(|event| matches!(event, CallEngineEvent::ParticipantJoined(p) if p.device_id == id_of::<DeviceId>(ALICE_PHONE))),
    "the laptop did not see the phone join: {laptop_events:?}"
  );

  // The phone leaves: the laptop stays in the call with Bob.
  phone.leave().await.unwrap();
  let after = collect(&mut laptop, Duration::from_secs(1)).await;
  assert!(
    after
      .iter()
      .any(|event| matches!(event, CallEngineEvent::ParticipantLeft(device) if *device == id_of::<DeviceId>(ALICE_PHONE))),
    "{after:?}"
  );
  assert!(
    !after
      .iter()
      .any(|event| matches!(event, CallEngineEvent::Reconnecting)),
    "{after:?}"
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn moving_the_call_here_closes_only_the_accounts_other_devices() {
  let (port, hash) = start_sfu().await;
  let mut laptop = join(port, hash, ALICE, ALICE_LAPTOP).await;
  let mut bob = join(port, hash, BOB, BOB_DEVICE).await;
  wait_for(&mut laptop, |event| {
    matches!(event, CallEngineEvent::Secured { .. })
  })
  .await;
  let mut phone = join(port, hash, ALICE, ALICE_PHONE).await;
  wait_for(&mut phone, |event| {
    matches!(event, CallEngineEvent::Secured { .. })
  })
  .await;

  phone.move_here();
  wait_for(&mut laptop, |event| {
    matches!(event, CallEngineEvent::ConnectionReplaced)
  })
  .await;

  let (phone_events, bob_events) = tokio::join!(
    collect(&mut phone, Duration::from_secs(1)),
    collect(&mut bob, Duration::from_secs(1)),
  );
  assert!(
    bob_events
      .iter()
      .any(|event| matches!(event, CallEngineEvent::ParticipantLeft(device) if *device == id_of::<DeviceId>(ALICE_LAPTOP))),
    "{bob_events:?}"
  );
  for event in phone_events.iter().chain(&bob_events) {
    assert!(
      !matches!(
        event,
        CallEngineEvent::ConnectionReplaced | CallEngineEvent::Reconnecting
      ),
      "{event:?}"
    );
  }
  assert!(
    !bob_events
      .iter()
      .any(|event| matches!(event, CallEngineEvent::ParticipantLeft(device) if *device == id_of::<DeviceId>(ALICE_PHONE))),
    "{bob_events:?}"
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn others_see_who_muted_also_when_they_join_later() {
  let (port, hash) = start_sfu().await;
  let mut laptop = join(port, hash, ALICE, ALICE_LAPTOP).await;
  let mut bob = join(port, hash, BOB, BOB_DEVICE).await;
  wait_for(&mut laptop, |event| {
    matches!(event, CallEngineEvent::Secured { .. })
  })
  .await;
  wait_for(&mut bob, |event| matches!(event, CallEngineEvent::Secured { .. })).await;
  let bob_device: DeviceId = id_of(BOB_DEVICE);

  bob.set_muted(true);
  wait_for(&mut laptop, |event| {
    matches!(event, CallEngineEvent::ParticipantMuted { device_id, is_muted: true } if *device_id == bob_device)
  })
  .await;

  // Somebody joining now learns it with the call's state.
  let mut phone = join(port, hash, ALICE, ALICE_PHONE).await;
  wait_for(&mut phone, |event| {
    matches!(event, CallEngineEvent::ParticipantMuted { device_id, is_muted: true } if *device_id == bob_device)
  })
  .await;

  bob.set_muted(false);
  wait_for(&mut laptop, |event| {
    matches!(event, CallEngineEvent::ParticipantMuted { device_id, is_muted: false } if *device_id == bob_device)
  })
  .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_flood_of_mute_toggles_is_not_relayed_to_everybody() {
  let (port, hash) = start_sfu().await;
  let mut laptop = join(port, hash, ALICE, ALICE_LAPTOP).await;
  let mut bob = join(port, hash, BOB, BOB_DEVICE).await;
  wait_for(&mut laptop, |event| {
    matches!(event, CallEngineEvent::Secured { .. })
  })
  .await;
  wait_for(&mut bob, |event| matches!(event, CallEngineEvent::Secured { .. })).await;

  let defaults = SfuConfig::default();
  let toggles = 10 * defaults.mute_report_burst;
  for index in 0..toggles {
    bob.set_muted(index % 2 == 0);
  }
  let events = collect(&mut laptop, Duration::from_secs(1)).await;
  let relayed = events
    .iter()
    .filter(|event| matches!(event, CallEngineEvent::ParticipantMuted { .. }))
    .count();
  assert!(relayed > 0, "the first changes pass");
  assert!(
    relayed <= (defaults.mute_report_burst + defaults.mute_reports_per_second * 2) as usize,
    "{relayed} of {toggles} relayed"
  );
}

async fn join(port: u16, certificate_hash: [u8; 32], user: u64, device: u64) -> NativeCall {
  let engine = CallEngineConfig {
    user_id: id_of::<UserId>(user),
    device_id: id_of::<DeviceId>(device),
    capabilities: MediaCapabilities::default(),
    bwe: BweConfig::default(),
  };
  let mut config = NativeCallConfig::new(sfu_url(port), format!("dev:{CALL_ID}:{user}:{device}"), engine);
  config.maybe_certificate_hash = Some(certificate_hash);
  config.source = AudioSource::Tone {
    frequency_hz: 300.0 + 10.0 * device as f32,
    amplitude: 0.3,
  };
  config.sink = AudioSink::Discard;
  NativeCall::join(config).await.unwrap()
}

/// The next event matching `predicate` within five seconds.
async fn wait_for(call: &mut NativeCall, predicate: impl Fn(&CallEngineEvent) -> bool) -> CallEngineEvent {
  tokio::time::timeout(Duration::from_secs(5), async {
    loop {
      let event = call.next_event().await.expect("call ended before the event");
      if predicate(&event) {
        return event;
      }
    }
  })
  .await
  .expect("event did not arrive")
}

/// The events of the next `duration`, without uplink estimates.
async fn collect(call: &mut NativeCall, duration: Duration) -> Vec<CallEngineEvent> {
  let mut events = Vec::new();
  let deadline = tokio::time::Instant::now() + duration;
  while let Ok(Some(event)) = tokio::time::timeout_at(deadline, call.next_event()).await {
    if !matches!(event, CallEngineEvent::UplinkEstimate { .. }) {
      events.push(event);
    }
  }
  events
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

fn sfu_url(port: u16) -> String {
  format!(
    "https://{}{CALL_PATH}",
    SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port)
  )
}

fn id_of<T: std::str::FromStr>(raw: u64) -> T
where
  T::Err: std::fmt::Debug,
{
  raw.to_string().parse().unwrap()
}
