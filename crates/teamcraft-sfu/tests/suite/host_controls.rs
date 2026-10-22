//! Meeting host controls through the internal endpoint: mute all, lock, remove, end.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_native::{NativeCall, NativeCallConfig};
use teamcraft_proto::{
  CallId, ErrorCode, Id, MediaCapabilities, SfuControlAction, SfuControlRequest, Snowflake, UserId,
};
use teamcraft_sfu::auth::DevTokenVerifier;
use teamcraft_sfu::config::SfuConfig;
use teamcraft_sfu::control::{CONTROL_PATH, control_router};
use teamcraft_sfu::server::{CALL_PATH, Sfu};
use wtransport::Identity;

const CALL: i64 = 31;
const TOKEN: &str = "test-registration-token";
const HOST: i64 = 1;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_mutes_locks_removes_and_ends() {
  let (port, hash, control_url) = start_sfu().await;
  let mut host = join(port, hash, HOST).await;
  let mut guest = join(port, hash, 2).await;
  wait_for(&mut guest, |event| {
    matches!(event, CallEngineEvent::Secured { .. })
  })
  .await;

  // Wrong token: refused.
  let client = reqwest::Client::new();
  let status = client
    .post(&control_url)
    .bearer_auth("wrong")
    .json(&request(SfuControlAction::MuteAll))
    .send()
    .await
    .unwrap()
    .status();
  assert_eq!(status.as_u16(), 401);

  send(&client, &control_url, SfuControlAction::MuteAll).await;
  wait_for(&mut guest, |event| {
    matches!(event, CallEngineEvent::MutedByHost { .. })
  })
  .await;
  wait_for(&mut guest, |event| {
    matches!(event, CallEngineEvent::MuteChanged(true))
  })
  .await;

  send(&client, &control_url, SfuControlAction::Lock).await;
  let mut late = join(port, hash, 3).await;
  let error = wait_for(&mut late, |event| matches!(event, CallEngineEvent::Error(_))).await;
  assert!(matches!(error, CallEngineEvent::Error(body) if body.error_code == ErrorCode::CallLocked));
  send(&client, &control_url, SfuControlAction::Unlock).await;

  send(
    &client,
    &control_url,
    SfuControlAction::RemoveParticipant { user_id: user(2) },
  )
  .await;
  wait_for(&mut guest, |event| {
    matches!(event, CallEngineEvent::RemovedByHost { .. })
  })
  .await;
  assert!(ends(&mut guest).await, "the removed guest's connection closes");
  let mut again = join(port, hash, 2).await;
  let error = wait_for(&mut again, |event| matches!(event, CallEngineEvent::Error(_))).await;
  assert!(matches!(error, CallEngineEvent::Error(body) if body.error_code == ErrorCode::RemovedFromCall));

  send(&client, &control_url, SfuControlAction::End).await;
  wait_for(&mut host, |event| {
    matches!(event, CallEngineEvent::EndedByHost { .. })
  })
  .await;
  assert!(ends(&mut host).await, "everyone is disconnected");

  // The call is gone: further controls find nothing.
  tokio::time::sleep(Duration::from_millis(500)).await;
  let status = client
    .post(&control_url)
    .bearer_auth(TOKEN)
    .json(&request(SfuControlAction::MuteAll))
    .send()
    .await
    .unwrap()
    .status();
  assert_eq!(status.as_u16(), 404);
}

async fn send(client: &reqwest::Client, url: &str, action: SfuControlAction) {
  let status = client
    .post(url)
    .bearer_auth(TOKEN)
    .json(&request(action))
    .send()
    .await
    .unwrap()
    .status();
  assert_eq!(status.as_u16(), 204);
}

fn request(action: SfuControlAction) -> SfuControlRequest {
  SfuControlRequest {
    call_id: CallId::from_snowflake(Snowflake::new(CALL).unwrap()),
    by_user_id: user(HOST),
    action,
  }
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

/// Whether the call's event stream ends within five seconds.
async fn ends(call: &mut NativeCall) -> bool {
  tokio::time::timeout(Duration::from_secs(5), async {
    while call.next_event().await.is_some() {}
  })
  .await
  .is_ok()
}

async fn join(port: u16, certificate_hash: [u8; 32], id: i64) -> NativeCall {
  let engine = CallEngineConfig {
    user_id: user(id),
    device_id: format!("{}", id + 100).parse().unwrap(),
    capabilities: MediaCapabilities::default(),
    bwe: BweConfig::default(),
  };
  let sfu_url = format!("https://[::1]:{port}{CALL_PATH}");
  let mut config = NativeCallConfig::new(sfu_url, format!("dev:{CALL}:{id}:{}", id + 100), engine);
  config.maybe_certificate_hash = Some(certificate_hash);
  NativeCall::join(config).await.unwrap()
}

async fn start_sfu() -> (u16, [u8; 32], String) {
  let config = SfuConfig {
    bind_addr: SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    ..SfuConfig::default()
  };
  let identity = Identity::self_signed(["localhost", "127.0.0.1", "::1"]).unwrap();
  let sfu = Sfu::bind(config, identity, Arc::new(DevTokenVerifier)).unwrap();
  let port = sfu.local_addr().unwrap().port();
  let certificate_hash = *sfu.certificate_hash().as_ref();
  let router = control_router(sfu.control(), TOKEN);
  let listener = tokio::net::TcpListener::bind((Ipv6Addr::LOCALHOST, 0))
    .await
    .unwrap();
  let control_url = format!("http://{}{CONTROL_PATH}", listener.local_addr().unwrap());
  tokio::spawn(async move { axum::serve(listener, router).await });
  tokio::spawn(sfu.run());
  (port, certificate_hash, control_url)
}

fn user(id: i64) -> UserId {
  UserId::from_snowflake(Snowflake::new(id).unwrap())
}
