//! Meeting host controls through the internal endpoint: mute all, lock, remove, end.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_native::{NativeCall, NativeCallConfig, NativeCallError};
use teamcraft_proto::{
  CallId, ErrorCode, Id, MediaCapabilities, SfuControlAction, SfuControlRequest, Snowflake, UserId,
};
use teamcraft_sfu::auth::{DevTokenVerifier, JoinGrant};
use teamcraft_sfu::config::SfuConfig;
use teamcraft_sfu::control::{CONTROL_PATH, control_router};
use teamcraft_sfu::reporting::ParticipantReporter;
use teamcraft_sfu::server::{CALL_PATH, Sfu};
use tokio::sync::mpsc;
use wtransport::Identity;

const CALL: i64 = 31;
const TOKEN: &str = "test-registration-token";
const HOST: i64 = 1;
/// Connection attempts per join; see [`join`].
const CONNECT_ATTEMPTS: usize = 2;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_mutes_locks_removes_and_ends() {
  let (port, hash, control_url, mut left) = start_sfu().await;
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

  // The guest shares the computer's sound; "mute all" stops it at the SFU too.
  guest
    .start_screen_audio(Box::new(
      teamcraft_media::system_audio::ManualSystemAudio::default(),
    ))
    .await
    .unwrap();
  wait_for(&mut guest, |event| {
    matches!(event, CallEngineEvent::ScreenAudioChanged { is_shared: true, .. })
  })
  .await;

  send(&client, &control_url, SfuControlAction::MuteAll).await;
  wait_for(&mut guest, |event| {
    matches!(event, CallEngineEvent::MutedByHost { .. })
  })
  .await;
  wait_for(&mut guest, |event| {
    matches!(event, CallEngineEvent::MuteChanged(true))
  })
  .await;
  wait_for(&mut guest, |event| {
    matches!(
      event,
      CallEngineEvent::ScreenAudioChanged { is_shared: false, .. }
    )
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

  // The call is gone once both admitted participants have left the room on the SFU's side (the
  // refused joins never entered it): further controls find nothing.
  wait_until_left(&mut left, &[user(HOST), user(2)]).await;
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

/// Waits until the SFU has reported every user in `users` as left, in any order.
async fn wait_until_left(left: &mut mpsc::UnboundedReceiver<UserId>, users: &[UserId]) {
  let mut pending = users.to_vec();
  tokio::time::timeout(Duration::from_secs(5), async {
    while !pending.is_empty() {
      let user_id = left.recv().await.expect("the SFU stopped reporting");
      pending.retain(|pending_user| *pending_user != user_id);
    }
  })
  .await
  .expect("participants did not leave");
}

/// Joins the call. A connection the transport refuses before anything was sent is attempted
/// again: under heavy parallel load the QUIC/HTTP/3 layer has been seen to close a fresh
/// connection with H3_FRAME_UNEXPECTED before the SFU code ever saw the session, which is not
/// what this test is about. Refusals by the SFU itself arrive as events, not as errors here.
async fn join(port: u16, certificate_hash: [u8; 32], id: i64) -> NativeCall {
  let mut attempt = 1;
  loop {
    match NativeCall::join(join_config(port, certificate_hash, id)).await {
      Ok(call) => return call,
      // A repeated refusal fails below with the error.
      Err(NativeCallError::Connect(_)) if attempt < CONNECT_ATTEMPTS => attempt += 1,
      Err(error) => panic!("join {id} failed: {error}"),
    }
  }
}

fn join_config(port: u16, certificate_hash: [u8; 32], id: i64) -> NativeCallConfig {
  let engine = CallEngineConfig {
    user_id: user(id),
    device_id: format!("{}", id + 100).parse().unwrap(),
    capabilities: MediaCapabilities::default(),
    bwe: BweConfig::default(),
  };
  let sfu_url = format!("https://[::1]:{port}{CALL_PATH}");
  let mut config = NativeCallConfig::new(sfu_url, format!("dev:{CALL}:{id}:{}", id + 100), engine);
  config.maybe_certificate_hash = Some(certificate_hash);
  config
}

/// The SFU, its certificate hash, the control URL and the users it reports as left.
async fn start_sfu() -> (u16, [u8; 32], String, mpsc::UnboundedReceiver<UserId>) {
  let config = SfuConfig {
    bind_addr: SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    ..SfuConfig::default()
  };
  let identity = Identity::self_signed(["localhost", "127.0.0.1", "::1"]).unwrap();
  let (left_tx, left_rx) = mpsc::unbounded_channel();
  let sfu = Sfu::bind(config, identity, Arc::new(DevTokenVerifier))
    .unwrap()
    .with_reporter(Arc::new(LeftReporter(left_tx)));
  let port = sfu.local_addr().unwrap().port();
  let certificate_hash = *sfu.certificate_hash().as_ref();
  let router = control_router(sfu.control(), TOKEN);
  let listener = tokio::net::TcpListener::bind((Ipv6Addr::LOCALHOST, 0))
    .await
    .unwrap();
  let control_url = format!("http://{}{CONTROL_PATH}", listener.local_addr().unwrap());
  tokio::spawn(async move { axum::serve(listener, router).await });
  tokio::spawn(sfu.run());
  (port, certificate_hash, control_url, left_rx)
}

/// Forwards the users the SFU reports as left; the room is gone after its last one.
struct LeftReporter(mpsc::UnboundedSender<UserId>);

impl ParticipantReporter for LeftReporter {
  fn joined(&self, _grant: JoinGrant) {}

  fn left(&self, grant: JoinGrant) {
    // The test may already be done listening; nothing to report to then.
    let _ = self.0.send(grant.user_id);
  }
}

fn user(id: i64) -> UserId {
  UserId::from_snowflake(Snowflake::new(id).unwrap())
}
