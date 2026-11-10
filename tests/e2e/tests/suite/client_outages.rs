//! Fault injection seen from a client (issues #743, #1615): a real core client against the real
//! server while a dependency or the network fails. The server side of the same outages is in
//! `dependency_outages.rs`; here the assertions are about what the user sees: the app stays usable,
//! nothing spins endlessly, a state asks the user to act only when they must (a file that stopped
//! with its cause, a network to sign in to), and everything recovers by itself (or on the retry the
//! user asked for) once the outage is over.
//!
//! The app scenarios (identity provider, captive portal, clock jump) run the apps' runner and
//! connection supervisor ([`teamcraft_e2e::app::App`]); the SFU restart runs two native calls as the
//! apps join them, with fresh join tickets for every rejoin.

use std::sync::Arc;
use std::time::{Duration, Instant};

use teamcraft_core::ClientSettings;
use teamcraft_core::api::Interception;
use teamcraft_core::client::OutgoingFile;
use teamcraft_core::client::files::{
  UploadRetrySettings, attachments_of, pending_upload, retry_stalled_upload,
};
use teamcraft_core::clock::SystemClock;
use teamcraft_core::connection::ConnectionState;
use teamcraft_core::retry::JitterMode;
use teamcraft_core::store::{LocalStore, MemoryStore, MessageContent, MessageFilter};
use teamcraft_e2e::app::{App, SkewedClock, StateLog};
use teamcraft_e2e::call::{SfuIdentity, join_call_with_tickets};
use teamcraft_e2e::relay::{PortalAnswer, TcpRelay};
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::tokens::AppTokens;

use teamcraft_e2e::{Peer, Stack, UserTokens};
use teamcraft_media::call_engine::{CallEngineEvent, CallStats};
use teamcraft_media::call_native::NativeCall;
use teamcraft_proto::{ConversationId, DeviceId, UserId};
use tokio::task::LocalSet;

/// Breakers probe again after a second, so recovery is quick to observe.
const OPEN_SECS: &str = "1";
/// Failed attempts after which a file waits for the user (the client's default is 6).
const ATTEMPTS: u32 = 3;
/// How long recovery may take once the dependency is back.
const RECOVERY: Duration = Duration::from_secs(30);
/// The longest the client may take to stop retrying: far more than the waits add up to, far less
/// than "endless".
const STALL_WITHIN: Duration = Duration::from_secs(60);
/// How long the apps' connection may take to come up at the start.
const CONNECT_WITHIN: Duration = Duration::from_secs(60);
/// How long an app scenario holds an outage while it watches that nothing goes wrong.
const HOLD: Duration = Duration::from_secs(10);
/// The longest an attempt to connect may show as "connecting": a handshake that hangs is cut off
/// well before (`ConnectionSettings::handshake_timeout`), so longer is an endless spinner.
const LONGEST_CONNECTING: Duration = Duration::from_secs(30);
/// How long an app may take to find out that a captive portal answers instead of the server.
const PORTAL_NOTICED_WITHIN: Duration = Duration::from_secs(30);
/// The gateway's pace in the clock scenario: short, so a held window covers several heartbeats and
/// the server's dead-connection timeout.
const HEARTBEAT_SECS: u64 = 5;
const HEARTBEAT_TIMEOUT_SECS: u64 = 20;
/// How long the clock scenario watches after each jump: past the server's heartbeat timeout.
const AFTER_JUMP: Duration = Duration::from_secs(25);
/// How long a message may take to arrive live while connected.
const LIVE_WITHIN: Duration = Duration::from_secs(10);
/// The jumps: an hour back (a clock corrected), two hours forward (NTP after a dead battery).
const JUMP_BACK_MS: i64 = -3_600_000;
const JUMP_FORWARD_MS: i64 = 7_200_000;
/// How long the calls may take to rejoin a restarted SFU (its start, the spread of the first
/// attempt, the back-off) and to hear each other again.
const REJOIN_WITHIN: Duration = Duration::from_secs(45);
/// How long the calls listen on the restarted SFU before they leave.
const LISTEN_AFTER_REJOIN: Duration = Duration::from_secs(4);
/// Opus frames of 20 ms.
const FRAMES_PER_SECOND: f64 = 50.0;
/// Frames that must have played on the restarted SFU: half of what it carried while listening.
const HEARD_AFTER_REJOIN: u64 = 100;

mod storage_outage {
  use super::*;

  /// The storage goes away while a file is on its way: other conversations keep working, the file
  /// stops retrying after its attempts (it names the cause and waits for the user), the pending
  /// message is not lost, and "Retry" sends it once the storage is back.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn a_file_stops_retrying_with_a_cause_and_goes_out_on_retry_while_chat_goes_on() {
    let Some(mut stack) = Stack::builder()
      .with_storage_relay()
      .with_env("TEAMCRAFT_DEPENDENCIES__OPEN_SECS", OPEN_SECS)
      .start()
      .await
    else {
      return;
    };
    let settings = ClientSettings {
      upload_retry: UploadRetrySettings {
        retry_after: Duration::from_millis(100),
        retry_max: Duration::from_millis(500),
        max_attempts: ATTEMPTS,
        stalled_retry_every: Duration::from_secs(300),
        jitter: JitterMode::Equal,
      },
      ..ClientSettings::default()
    };
    let mut alice = Peer::enroll_with_settings(
      &stack,
      "alice",
      "alice e2e device",
      Arc::new(MemoryStore::new()),
      settings,
    )
    .await;
    let mut carol = Peer::enroll(&stack, "carol").await;
    let space = space_with(&stack, &alice, "Project A", &[(&carol, "member")]).await;
    let files = channel_with(&mut alice, space, "files", &mut [&mut carol]).await;
    let general = channel_with(&mut alice, space, "general", &mut [&mut carol]).await;

    stack.storage_relay().cut();
    let content = b"quarterly numbers".to_vec();
    let started = Instant::now();
    // The message is stored at once; the upload failing is a state of the file, not an error the
    // user has to deal with right now.
    let message = alice
      .client
      .send_files(
        files,
        None,
        "report",
        vec![OutgoingFile {
          file_name: "numbers.txt".into(),
          media_type: "text/plain".into(),
          content: content.clone(),
        }],
      )
      .await;
    eprintln!(
      "PERF storage down: send_files answered after {:?}",
      started.elapsed()
    );
    assert!(
      started.elapsed() < Duration::from_secs(30),
      "the first attempt hung for {:?}",
      started.elapsed()
    );
    // The call reports the 503 (a host shows the file as waiting); the message itself is kept in
    // the outbox with its pending upload.
    assert!(message.is_err(), "the storage is down: {message:?}");
    let message = alice
      .store
      .messages(files, MessageFilter::All)
      .unwrap()
      .into_iter()
      .find(|stored| stored.maybe_server_id.is_none())
      .expect("the message waits in the outbox")
      .client_message_id;

    // Chat elsewhere is unaffected by the storage.
    alice
      .client
      .send_text(general, None, "still here")
      .await
      .expect("text does not depend on the storage");
    carol.wait_for_text(general, "still here").await;

    // The client retries on its backoff and then gives the file to the user.
    let cause = loop {
      let _ = alice.client.upload_pending().await;
      let upload = pending_upload(&alice.store, files, message)
        .unwrap()
        .expect("the pending upload is kept");
      if let Some(cause) = upload.files[0].retry.stall_cause() {
        break cause;
      }
      assert!(
        started.elapsed() < STALL_WITHIN,
        "still retrying after {:?}: {:?}",
        started.elapsed(),
        upload.files[0].retry
      );
      tokio::time::sleep(Duration::from_millis(50)).await;
    };
    eprintln!(
      "PERF storage down: file stalled ({cause:?}) after {:?}",
      started.elapsed()
    );

    // Stalled means quiet: no further attempt and nothing counted until the user asks.
    let stalled = pending_upload(&alice.store, files, message)
      .unwrap()
      .unwrap()
      .files[0]
      .retry
      .clone();
    assert_eq!(alice.client.upload_pending().await.unwrap(), 0);
    assert_eq!(
      pending_upload(&alice.store, files, message)
        .unwrap()
        .unwrap()
        .files[0]
        .retry,
      stalled
    );
    assert!(stack.server().is_running());

    // The storage is back: the user's Retry sends the message with its file.
    stack.storage_relay().restore();
    assert!(retry_stalled_upload(&alice.store, files, message).unwrap());
    let deadline = Instant::now() + STALL_WITHIN;
    loop {
      let _ = alice.client.upload_pending().await;
      if alice.client.flush_outbox().await.is_ok()
        && pending_upload(&alice.store, files, message).unwrap().is_none()
      {
        break;
      }
      assert!(
        Instant::now() < deadline,
        "the file did not go out after the storage came back"
      );
      tokio::time::sleep(Duration::from_millis(250)).await;
    }
    carol
      .wait_until("the file message arrives", |peer| {
        peer.store.message(files, message).unwrap().is_some()
      })
      .await;
    let received = carol.store.message(files, message).unwrap().unwrap();
    let attachment = attachments_of(&received.content)[0].clone();
    assert_eq!(
      carol
        .client
        .open_attachment(&attachment)
        .await
        .unwrap()
        .expose_secret(),
      content.as_slice()
    );
    eprintln!(
      "PERF storage down: file delivered {:?} after the cut",
      started.elapsed()
    );
    stack.stop().await;
  }
}

mod database_outage {
  use super::*;

  /// Postgres goes away: a message sent meanwhile is kept in the outbox (not lost, not shown as
  /// sent), the failure comes back at once instead of hanging, and once Postgres is back the same
  /// message is delivered exactly once.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn a_message_sent_while_postgres_is_down_waits_in_the_outbox_and_arrives_once() {
    let Some(mut stack) = Stack::builder().with_database_relay().start().await else {
      return;
    };
    let mut alice = Peer::enroll(&stack, "alice").await;
    let mut carol = Peer::enroll(&stack, "carol").await;
    let space = space_with(&stack, &alice, "Project A", &[(&carol, "member")]).await;
    let channel = channel_with(&mut alice, space, "general", &mut [&mut carol]).await;

    stack.database_relay().cut();
    let started = Instant::now();
    let sent = alice
      .client
      .send_text(channel, None, "written during the outage")
      .await;
    eprintln!("PERF postgres down: send answered after {:?}", started.elapsed());
    assert!(
      started.elapsed() < Duration::from_secs(10),
      "the send hung for {:?}",
      started.elapsed()
    );
    let stored = alice
      .store
      .messages(channel, MessageFilter::All)
      .unwrap()
      .into_iter()
      .find(|message| {
        matches!(&message.content, MessageContent::Text { body, .. } if body == "written during the outage")
      })
      .unwrap_or_else(|| panic!("the message is kept locally ({sent:?})"));
    assert!(stored.maybe_server_id.is_none(), "not shown as sent");

    stack.database_relay().restore();
    let deadline = Instant::now() + RECOVERY;
    loop {
      if alice.client.flush_outbox().await.is_ok() {
        break;
      }
      assert!(
        Instant::now() < deadline,
        "the outbox did not flush after Postgres came back"
      );
      tokio::time::sleep(Duration::from_millis(250)).await;
    }
    carol.wait_for_text(channel, "written during the outage").await;
    let copies = carol
      .texts(channel)
      .iter()
      .filter(|text| text.as_str() == "written during the outage")
      .count();
    assert_eq!(copies, 1, "a retried send must not duplicate");
    eprintln!(
      "PERF postgres down: delivered {:?} after the cut",
      started.elapsed()
    );
    stack.stop().await;
  }
}

mod identity_provider_outage {
  use super::*;

  /// The identity provider goes away while the app is signed in: first only the server can't
  /// reach its signing keys (it keeps the ones it has, chat goes on), then the app's token runs
  /// out and can't be refreshed. The app is not signed out: it keeps showing what it has, a message
  /// written meanwhile waits in the outbox, and once the provider is back the message goes out
  /// exactly once without the user doing anything.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn a_signed_in_app_stays_usable_and_recovers_by_itself() {
    LocalSet::new().run_until(identity_provider_outage()).await;
  }

  async fn identity_provider_outage() {
    let Some(mut stack) = Stack::builder().with_identity_relay().start().await else {
      return;
    };
    let mut chat = Chat::set_up(&stack).await;
    let tokens = AppTokens::new(chat.carol_tokens.clone());
    let app = App::start_with(
      &stack.base_url,
      tokens.clone(),
      chat.carol_store.clone(),
      None,
      Arc::new(SystemClock),
    );
    let me = chat.carol_id;
    chat.connect(&app).await;
    let states = app.record_states();

    // The provider is gone; the server keeps its signing keys and the app its token.
    stack.identity_relay().cut();
    tokens.set_provider_reachable(false);
    let sent = app.send_text(me, chat.channel, "while the provider is down");
    wait_until("the message is sent with the token the app has", RECOVERY, || {
      app.is_sent(chat.channel, sent)
    })
    .await;
    chat
      .alice
      .wait_for_text(chat.channel, "while the provider is down")
      .await;
    chat.say_to_app(&app, "to carol while the provider is down").await;

    // The token runs out and the provider can't give a new one.
    tokens.expire();
    let waiting = app.send_text(me, chat.channel, "after the token ran out");
    hold(HOLD, || {
      assert!(!app.is_sent(chat.channel, waiting), "sent without a token");
      assert!(
        !matches!(app.state(), ConnectionState::SignInRequired { .. }),
        "an unreachable provider signed the user out"
      );
    })
    .await;
    // What the app has stays readable, the waiting message included.
    let texts = app.texts(chat.channel);
    for text in ["to carol while the provider is down", "after the token ran out"] {
      assert!(
        texts.iter().any(|known| known == text),
        "{text:?} not shown: {texts:?}"
      );
    }

    // The provider is back: the app gets a token and sends, without a network signal.
    stack.identity_relay().restore();
    tokens.set_provider_reachable(true);
    let back = Instant::now();
    wait_until("the waiting message goes out", RECOVERY * 2, || {
      app.is_sent(chat.channel, waiting)
    })
    .await;
    eprintln!(
      "PERF identity provider back: outbox sent after {:?}",
      back.elapsed()
    );
    chat
      .alice
      .wait_for_text(chat.channel, "after the token ran out")
      .await;
    app.wait_connected(RECOVERY).await;
    chat.say_to_app(&app, "after the provider came back").await;
    chat.assert_once("after the token ran out");
    assert_never_stuck(&states);
    app.stop().await;
    stack.stop().await;
  }
}

mod captive_portal {
  use super::*;

  /// A portal that answers every request with its sign-in page (HTTP 200, HTML).
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn a_sign_in_page_makes_the_app_wait_and_say_so() {
    LocalSet::new()
      .run_until(captive_portal(PortalAnswer::SignInPage))
      .await;
  }

  /// A portal that answers with 511 Network Authentication Required.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn network_authentication_required_makes_the_app_wait_and_say_so() {
    LocalSet::new()
      .run_until(captive_portal(PortalAnswer::NetworkAuthenticationRequired))
      .await;
  }

  /// The network switches to a captive portal (hotel Wi-Fi before its terms are accepted): REST
  /// requests and the WebSocket handshake get the portal's answer. The app tells the user to sign
  /// in to the network (and nothing else: it doesn't sign out, doesn't spin), a message written
  /// meanwhile waits in the outbox, and once the user signed in to the portal the app reconnects
  /// and sends it exactly once, without being told the network changed.
  async fn captive_portal(answer: PortalAnswer) {
    let Some(stack) = Stack::builder().start().await else {
      return;
    };
    let relay = TcpRelay::start(stack.base_url.trim_start_matches("http://"))
      .await
      .unwrap();
    let mut chat = Chat::set_up(&stack).await;
    let app = App::start(
      &format!("http://127.0.0.1:{}", relay.port),
      chat.carol_tokens.clone(),
      chat.carol_store.clone(),
    );
    let me = chat.carol_id;
    chat.connect(&app).await;
    let states = app.record_states();

    relay.intercept(answer);
    let started = Instant::now();
    let waiting = app.send_text(me, chat.channel, "written behind the portal");
    wait_until("the app names the captive portal", PORTAL_NOTICED_WITHIN, || {
      is_behind_portal(app.state())
    })
    .await;
    eprintln!(
      "PERF captive portal ({answer:?}): named after {:?}",
      started.elapsed()
    );
    hold(HOLD, || {
      assert!(!app.is_sent(chat.channel, waiting), "sent through a portal?");
      let state = app.state();
      assert!(
        !matches!(
          state,
          ConnectionState::SignInRequired { .. } | ConnectionState::Connected
        ),
        "{state:?} behind a portal"
      );
    })
    .await;
    // Between attempts the app keeps naming the portal: the user must act.
    wait_until(
      "the app names the captive portal again",
      PORTAL_NOTICED_WITHIN,
      || is_behind_portal(app.state()),
    )
    .await;
    assert!(
      app
        .texts(chat.channel)
        .iter()
        .any(|text| text == "written behind the portal")
    );

    // The user signed in to the portal; nothing tells the app.
    relay.restore();
    let back = Instant::now();
    wait_until("the waiting message goes out", RECOVERY * 2, || {
      app.is_sent(chat.channel, waiting)
    })
    .await;
    eprintln!(
      "PERF captive portal ({answer:?}): outbox sent {:?} after the sign-in",
      back.elapsed()
    );
    chat
      .alice
      .wait_for_text(chat.channel, "written behind the portal")
      .await;
    app.wait_connected(RECOVERY).await;
    chat.say_to_app(&app, "past the portal").await;
    chat.assert_once("written behind the portal");
    assert_never_stuck(&states);
    app.stop().await;
    drop(relay);
    stack.stop().await;
  }

  fn is_behind_portal(state: ConnectionState) -> bool {
    matches!(
      state,
      ConnectionState::Offline {
        maybe_interception: Some(Interception::CaptivePortal),
        ..
      }
    )
  }
}

mod clock_jump {
  use super::*;

  /// The device's clock is set an hour back while the app is connected and the conversation is
  /// busy: the connection keeps its pace (heartbeats are not postponed by the hour, the server
  /// doesn't drop it), and messages keep arriving live and going out.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  #[ignore = "#1673: a busy conversation keeps the runner from noticing a clock set back; the server drops the app"]
  async fn the_app_keeps_its_connection_when_the_clock_goes_back() {
    LocalSet::new().run_until(clock_jump(Jump::Back)).await;
  }

  /// The device's clock is set two hours forward (NTP after a dead battery): that looks like a
  /// wake-up, worth a quick check of the connection, never its loss; chat goes on.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn the_app_keeps_its_connection_when_the_clock_goes_forward() {
    LocalSet::new().run_until(clock_jump(Jump::Forward)).await;
  }

  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
  enum Jump {
    Back,
    Forward,
  }

  async fn clock_jump(jump: Jump) {
    let Some(stack) = Stack::builder()
      .with_env(
        "TEAMCRAFT_GATEWAY__HEARTBEAT_INTERVAL_SECS",
        &HEARTBEAT_SECS.to_string(),
      )
      .with_env(
        "TEAMCRAFT_GATEWAY__HEARTBEAT_TIMEOUT_SECS",
        &HEARTBEAT_TIMEOUT_SECS.to_string(),
      )
      .start()
      .await
    else {
      return;
    };
    let mut chat = Chat::set_up(&stack).await;
    let clock = Arc::new(SkewedClock::default());
    let app = App::start_with(
      &stack.base_url,
      chat.carol_tokens.clone(),
      chat.carol_store.clone(),
      None,
      clock.clone(),
    );
    let me = chat.carol_id;
    chat.connect(&app).await;

    let states = app.record_states();
    let jumped = Instant::now();
    clock.jump(match jump {
      Jump::Back => JUMP_BACK_MS,
      Jump::Forward => JUMP_FORWARD_MS,
    });
    let mut round = 0;
    while jumped.elapsed() < AFTER_JUMP {
      let delay = chat
        .say_to_app(&app, &format!("to carol after the jump, {round}"))
        .await;
      assert!(delay < LIVE_WITHIN, "{delay:?} to arrive after the jump {jump:?}");
      let text = format!("from carol after the jump, {round}");
      let sent = app.send_text(me, chat.channel, &text);
      wait_until("carol's message goes out", LIVE_WITHIN, || {
        app.is_sent(chat.channel, sent)
      })
      .await;
      chat.alice.wait_for_text(chat.channel, &text).await;
      round += 1;
      tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let states = states.borrow().clone();
    eprintln!("PERF clock jump {jump:?}: states {:?}", summary(&states, jumped));
    let is_kept = states
      .iter()
      .all(|(_, state)| matches!(state, ConnectionState::Connected | ConnectionState::Slow));
    assert!(is_kept, "the jump {jump:?} cost the connection: {states:?}");
    app.stop().await;
    stack.stop().await;
  }

  fn summary(states: &[(Instant, ConnectionState)], since: Instant) -> Vec<String> {
    states
      .iter()
      .map(|(at, state)| {
        format!(
          "{:?} at {} ms",
          state,
          at.saturating_duration_since(since).as_millis()
        )
      })
      .collect()
  }
}

mod sfu_restart {
  use super::*;

  /// The SFU restarts during a call (a deploy) and comes back with a new certificate, which the
  /// server already names in its join tickets (`sfu_certificate_sha256` lists both while one
  /// replaces the other). Both sides rejoin by themselves with fresh tickets, accept the new
  /// certificate, re-key, and hear each other again.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn a_call_rejoins_a_restarted_sfu_with_its_new_certificate() {
    let (first, second) = (SfuIdentity::generate(), SfuIdentity::generate());
    let Some(mut stack) = Stack::builder()
      .with_calls()
      .with_sfu_identity(
        first.certificate_pem.clone(),
        first.key_pem.clone(),
        format!("{}, {}", first.sha256_hex, second.sha256_hex),
      )
      .start()
      .await
    else {
      return;
    };
    let mut alice = Peer::enroll(&stack, "alice").await;
    let mut carol = Peer::enroll(&stack, "carol").await;
    let dm = alice.client.open_dm(carol.user_id()).await.unwrap();
    carol.wait_joined(dm).await;
    let (alice_device, carol_device) = (alice.client.device_id(), carol.client.device_id());
    let base_url = stack.base_url.clone();
    let joined = Instant::now();
    let mut alice_call = join_call_with_tickets(&mut alice, &base_url, dm, 330.0).await;
    let mut carol_call = join_call_with_tickets(&mut carol, &base_url, dm, 550.0).await;
    tokio::join!(
      until_speaking(&mut alice_call, carol_device, false),
      until_speaking(&mut carol_call, alice_device, false)
    );

    let before_restart = joined.elapsed();
    let restarted = Instant::now();
    let hash = stack
      .restart_sfu(Some((second.certificate_pem.clone(), second.key_pem.clone())))
      .await;
    assert_eq!(
      hex(&hash),
      second.sha256_hex,
      "the SFU runs on the second certificate"
    );
    let (alice_back, carol_back) = tokio::join!(
      until_speaking(&mut alice_call, carol_device, true),
      until_speaking(&mut carol_call, alice_device, true)
    );
    eprintln!(
      "PERF SFU restart: speaking again after {alice_back:?} / {carol_back:?} ({:?} since the restart began)",
      restarted.elapsed()
    );
    // Listen a while on the new SFU: the calls play far more than they could have before it.
    let (alice_stats, carol_stats) = tokio::join!(
      listen_and_leave(alice_call, LISTEN_AFTER_REJOIN),
      listen_and_leave(carol_call, LISTEN_AFTER_REJOIN)
    );
    let at_most_before = (before_restart.as_secs_f64() * FRAMES_PER_SECOND) as u64;
    for stats in [&alice_stats, &carol_stats] {
      assert!(
        stats.audible_frames_played > at_most_before + HEARD_AFTER_REJOIN,
        "not heard on the restarted SFU ({at_most_before} frames at most before it): {stats:?}"
      );
    }
    stack.stop().await;
  }

  /// Follows `call` until it is secured and the SFU names `other` as speaking (its audio reaches
  /// the SFU, and so this call), after rejoining first when `must_rejoin`. Fails on trust problems,
  /// an SFU error or after [`REJOIN_WITHIN`]. Returns how long that took.
  async fn until_speaking(call: &mut NativeCall, other: DeviceId, must_rejoin: bool) -> Duration {
    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + REJOIN_WITHIN;
    let (mut has_rejoined, mut is_secured) = (!must_rejoin, false);
    let mut seen = Vec::new();
    loop {
      let event = match tokio::time::timeout_at(deadline, call.next_event()).await {
        Ok(Some(event)) => event,
        Ok(None) => panic!("the call ended; events: {seen:?}"),
        Err(_) => panic!(
          "not speaking within {REJOIN_WITHIN:?} (rejoined: {has_rejoined}, secured: {is_secured}); \
           events: {seen:?}"
        ),
      };
      match &event {
        CallEngineEvent::UntrustedRoster
        | CallEngineEvent::UnauthenticatedKeys
        | CallEngineEvent::EncryptionFailed { .. }
        | CallEngineEvent::Error(_) => panic!("{event:?}; events: {seen:?}"),
        CallEngineEvent::Reconnected => {
          has_rejoined = true;
          is_secured = false;
        },
        CallEngineEvent::Secured { .. } if has_rejoined => is_secured = true,
        CallEngineEvent::ActiveSpeakers(speakers)
          if has_rejoined && is_secured && speakers.contains(&other) =>
        {
          return started.elapsed();
        },
        _ => {},
      }
      if !matches!(
        event,
        CallEngineEvent::AudioFrame { .. }
          | CallEngineEvent::ActiveSpeakers(_)
          | CallEngineEvent::UplinkEstimate { .. }
      ) {
        seen.push(format!("{event:?}"));
      }
    }
  }

  async fn listen_and_leave(mut call: NativeCall, duration: Duration) -> CallStats {
    let deadline = tokio::time::Instant::now() + duration;
    while let Ok(Some(event)) = tokio::time::timeout_at(deadline, call.next_event()).await {
      assert!(
        !matches!(event, CallEngineEvent::Reconnecting | CallEngineEvent::Error(_)),
        "{event:?}"
      );
    }
    call.leave().await.expect("leaving the call")
  }

  fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
  }
}

/// alice (a headless client) and carol (the app under test) in a channel of a space.
struct Chat {
  alice: Peer,
  /// carol's enrolled device, whose store and tokens the app takes over (its own gateway
  /// connection is closed, so the app's is the device's only one).
  carol_id: UserId,
  carol_store: Arc<MemoryStore>,
  carol_tokens: UserTokens,
  channel: ConversationId,
}

impl Chat {
  async fn set_up(stack: &Stack) -> Self {
    let mut alice = Peer::enroll(stack, "alice").await;
    let mut carol = Peer::enroll(stack, "carol").await;
    let space = space_with(stack, &alice, "Project A", &[(&carol, "member")]).await;
    let channel = channel_with(&mut alice, space, "general", &mut [&mut carol]).await;
    Self {
      alice,
      carol_id: carol.user_id(),
      carol_store: carol.store.clone(),
      carol_tokens: carol.tokens.clone(),
      channel,
    }
  }

  /// Waits until the app is connected and caught up (a message from alice arrived).
  async fn connect<S>(&mut self, app: &App<S>)
  where
    S: teamcraft_core::store::LocalStore + Clone + 'static,
  {
    app.wait_connected(CONNECT_WITHIN).await;
    self.say_to_app(app, "before the outage").await;
  }

  /// alice writes `text`; returns how long it took to show in the app.
  async fn say_to_app<S>(&mut self, app: &App<S>, text: &str) -> Duration
  where
    S: teamcraft_core::store::LocalStore + Clone + 'static,
  {
    // alice is a headless client that only reads her gateway when she waits for a message: she
    // keeps it alive here, so the server's heartbeat timeout is about the app.
    self.alice.heartbeat().await;
    self
      .alice
      .client
      .send_text(self.channel, None, text)
      .await
      .unwrap();
    app.wait_for(&self.channel, text, RECOVERY).await
  }

  /// alice has `text` exactly once: a retried send must not duplicate.
  fn assert_once(&self, text: &str) {
    let copies = self
      .alice
      .texts(self.channel)
      .iter()
      .filter(|known| known.as_str() == text)
      .count();
    assert_eq!(copies, 1, "{text:?} arrived {copies} times");
  }
}

/// Polls `done` until it holds; fails after `timeout`.
async fn wait_until(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
  let started = Instant::now();
  while !done() {
    assert!(started.elapsed() < timeout, "{what}: not within {timeout:?}");
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
}

/// Checks `check` throughout `duration` (an outage that must not change what it checks).
async fn hold(duration: Duration, mut check: impl FnMut()) {
  let started = Instant::now();
  while started.elapsed() < duration {
    check();
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
  check();
}

/// The app never asked the user to sign in, and never showed "connecting" longer than one attempt
/// can take: no endless spinner.
fn assert_never_stuck(states: &StateLog) {
  let states = states.borrow().clone();
  assert!(
    !states
      .iter()
      .any(|(_, state)| matches!(state, ConnectionState::SignInRequired { .. })),
    "the user was asked to sign in: {states:?}"
  );
  for pair in states.windows(2) {
    let ((began, state), (ended, _)) = (&pair[0], &pair[1]);
    if *state == ConnectionState::Connecting {
      let took = ended.saturating_duration_since(*began);
      assert!(took < LONGEST_CONNECTING, "connecting for {took:?}: {states:?}");
    }
  }
}
