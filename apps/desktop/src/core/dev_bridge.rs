//! A dev bridge: serves the commands of the desktop shell over HTTP so the web UI in a plain browser
//! (headless, for screenshots) can run against a real client core, with alice's account. A second core
//! (carol) chats with her like a colleague would. Run by hand against the dev stack:
//!
//! ```sh
//! cargo test -p teamcraft-desktop --features integration -- --ignored serve_dev_bridge --nocapture
//! ```
//!
//! `dev/bridge-shim.js` implements `window.__TAURI_INTERNALS__` on top of it. Dev tool only: no authentication,
//! listens on loopback, and the core's database key is a fixed test key.
// Test-only module: a failing step should abort with a clear message.
// The progress lines are the point of this hand-run tool.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::print_stderr)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use teamcraft_client_api::model::thread_id_of;
use teamcraft_client_api::remote::{ApiMethod, CallAction, ChatAction, CoreStatus, EnrollRequest};
use teamcraft_proto::{ConversationId, UserId};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

use super::fake_calls::FakeCalls;
use super::integration_tests::{Person, conversation_named, create_space, direct_with, enroll, find, person};
use crate::auth::DesktopConfig;

const DEFAULT_PORT: u16 = 8099;
const DEFAULT_SECONDS: u64 = 600;
const EVENT_WAIT: Duration = Duration::from_secs(20);

struct Bridge {
  alice: Person,
  calls: FakeCalls,
  changes: AtomicU64,
  changed: Notify,
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "long-running dev tool: serves the bridge until it times out"]
async fn serve_dev_bridge() {
  let config = DesktopConfig::from_env();
  let port = env_or("TEAMCRAFT_BRIDGE_PORT", DEFAULT_PORT.to_string())
    .parse::<u16>()
    .expect("port");
  let seconds = env_or("TEAMCRAFT_BRIDGE_SECONDS", DEFAULT_SECONDS.to_string())
    .parse::<u64>()
    .expect("seconds");
  let alice = person("alice", &config).await;
  let carol = Arc::new(person("carol", &config).await);
  let bridge = Arc::new(Bridge {
    calls: FakeCalls::new(carol.id),
    alice,
    changes: AtomicU64::new(0),
    changed: Notify::new(),
  });

  let listener = TcpListener::bind(("127.0.0.1", port)).await.expect("bind");
  eprintln!("dev bridge ready on http://127.0.0.1:{port} for {seconds} s");
  let serving = tokio::spawn(serve(listener, bridge.clone()));
  let peer = tokio::spawn(colleague(bridge.clone(), carol, config));
  tokio::time::sleep(Duration::from_secs(seconds)).await;
  serving.abort();
  peer.abort();
}

/// Carol waits until alice's device is enrolled (done through the UI), then sets up a space, a channel
/// and a DM with her, writes first, and answers whatever alice writes.
async fn colleague(bridge: Arc<Bridge>, carol: Arc<Person>, config: DesktopConfig) {
  enroll(&carol).await;
  loop {
    if matches!(bridge.alice.runtime.status().await, CoreStatus::Ready { .. }) {
      break;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
  }
  let space = create_space(&bridge.alice.token, &carol.token, &config).await;
  let name = "launch-planning";
  carol
    .runtime
    .action(ChatAction::CreateChannel {
      space,
      name: name.into(),
    })
    .await
    .expect("channel");
  let channel = find(&carol, |snapshot| conversation_named(snapshot, name)).await;
  carol
    .runtime
    .action(ChatAction::AddMember {
      conversation: channel,
      user: bridge.alice.id,
    })
    .await
    .expect("add alice");
  say(
    &carol,
    channel,
    "Welcome to #launch-planning, Alice. Notes from today's kickoff are in the pinned doc.",
  )
  .await;
  say(
    &carol,
    channel,
    "Can you take the **API review** on Thursday? I'll cover the `rollout checklist`.",
  )
  .await;
  set_status(&carol, &config, "🏖️", "Out on Friday").await;
  log_channel(&carol, &bridge, &config, space).await;
  carol
    .runtime
    .action(ChatAction::OpenDm {
      user: bridge.alice.id,
    })
    .await
    .expect("dm");
  let dm = find(&carol, |snapshot| direct_with(snapshot, bridge.alice.id)).await;
  say(
    &carol,
    dm,
    "Hi @alice, this chat is end-to-end encrypted: the server only ever sees ciphertext.",
  )
  .await;
  eprintln!("colleague ready: channel and DM exist");
  answer_alice(&carol, bridge.alice.id, [channel, dm]).await;
}

async fn say(person: &Person, conversation: ConversationId, body: &str) {
  person
    .runtime
    .action(ChatAction::PostMessage {
      conversation,
      maybe_thread: None,
      body: body.to_owned(),
    })
    .await
    .expect("post");
}

/// An integration channel with lines like the bridge writes them (`[LEVEL] text`).
async fn log_channel(
  carol: &Person,
  bridge: &Bridge,
  config: &DesktopConfig,
  space: teamcraft_proto::SpaceId,
) {
  let name = "app-logs";
  carol
    .runtime
    .action(ChatAction::CreateChannel {
      space,
      name: name.into(),
    })
    .await
    .expect("log channel");
  let channel = find(carol, |snapshot| conversation_named(snapshot, name)).await;
  carol
    .runtime
    .action(ChatAction::AddMember {
      conversation: channel,
      user: bridge.alice.id,
    })
    .await
    .expect("add alice to the log channel");
  let base = config.server_url.trim_end_matches('/');
  let response = reqwest::Client::new()
    .patch(format!("{base}/api/v1/conversations/{channel}"))
    .bearer_auth(&carol.token)
    .json(&json!({ "is_integration": true }))
    .send()
    .await
    .expect("mark the channel as integration channel");
  assert!(
    response.status().is_success(),
    "integration flag: {}",
    response.status()
  );
  let lines = [
    "[INFO] worker started, 4 queues attached",
    "[INFO] job 4812 finished in 120 ms",
    "[WARN] queue `mail` is 40 jobs behind",
    "[INFO] job 4813 finished in 98 ms",
    "[ERROR] job 4814 failed: upstream timed out after 30 s",
    "[DEBUG] retry scheduled in 60 s",
    "[INFO] job 4814 finished in 210 ms",
  ];
  for line in lines {
    say(carol, channel, line).await;
  }
}

/// Sets a custom status over REST (the server stores it; it is not message content).
async fn set_status(person: &Person, config: &DesktopConfig, emoji: &str, text: &str) {
  let base = config.server_url.trim_end_matches('/');
  let response = reqwest::Client::new()
    .put(format!("{base}/api/v1/me/status"))
    .bearer_auth(&person.token)
    .json(&json!({ "emoji": emoji, "text": text }))
    .send()
    .await
    .expect("set status");
  assert!(response.status().is_success(), "status: {}", response.status());
}

/// Replies once to each new message from alice.
async fn answer_alice(carol: &Person, alice: UserId, conversations: [ConversationId; 2]) {
  let mut answered = std::collections::HashSet::new();
  loop {
    for conversation in conversations {
      let window = carol
        .runtime
        .messages(conversation, 0, 200)
        .await
        .expect("window");
      for message in window.messages {
        if message.author == alice && answered.insert(message.id) {
          let reply = format!(
            "Got it. \"{}\": I'll look into it.",
            message.body.chars().take(60).collect::<String>()
          );
          say(carol, conversation, &reply).await;
          if conversation == conversations[0] {
            // A thread under alice's message: she follows it and the inbox lists it with a mention.
            carol
              .runtime
              .action(ChatAction::PostMessage {
                conversation,
                maybe_thread: Some(thread_id_of(message.id)),
                body: "@alice I added the open questions to the doc.".to_owned(),
              })
              .await
              .expect("thread reply");
          }
          // Carol has read it: a DM shows alice "Seen by Carol".
          carol
            .runtime
            .action(ChatAction::MarkRead { conversation })
            .await
            .expect("mark read");
        }
      }
    }
    tokio::time::sleep(Duration::from_millis(400)).await;
  }
}

async fn serve(listener: TcpListener, bridge: Arc<Bridge>) {
  loop {
    let Ok((stream, _)) = listener.accept().await else {
      continue;
    };
    tokio::spawn(connection(stream, bridge.clone()));
  }
}

async fn connection(mut stream: TcpStream, bridge: Arc<Bridge>) {
  let Some((method, path, body)) = read_request(&mut stream).await else {
    return;
  };
  let (status, payload) = match (method.as_str(), path.split('?').next().unwrap_or("")) {
    ("OPTIONS", _) => (204, String::new()),
    ("POST", "/invoke") => (200, invoke(&bridge, &body).await.to_string()),
    // Dev only: carol calls alice in the conversation named in the body.
    ("POST", "/dev/ring") => {
      match serde_json::from_str::<ConversationId>(body.trim()) {
        Ok(conversation) => bridge.calls.ring(conversation),
        Err(error) => eprintln!("ring: {error}"),
      }
      bump(&bridge);
      (200, "{}".to_owned())
    },
    ("GET", "/events") => {
      let since = path
        .split("since=")
        .nth(1)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
      let waiting = bridge.changed.notified();
      if bridge.changes.load(Ordering::Relaxed) <= since {
        drop(tokio::time::timeout(EVENT_WAIT, waiting).await);
      }
      (
        200,
        json!({ "counter": bridge.changes.load(Ordering::Relaxed) }).to_string(),
      )
    },
    _ => (404, "{}".to_owned()),
  };
  let response = format!(
    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nConnection: close\r\n\r\n{payload}",
    payload.len()
  );
  drop(stream.write_all(response.as_bytes()).await);
  drop(stream.shutdown().await);
}

/// `{ "ok": value }` or `{ "error": "text" }`.
async fn invoke(bridge: &Arc<Bridge>, body: &str) -> Value {
  let request: Value = serde_json::from_str(body).unwrap_or(Value::Null);
  let args = &request["args"];
  let result = match request["cmd"].as_str().unwrap_or("") {
    "auth_restore" => Ok(json!(true)),
    "auth_login" | "auth_cancel" | "auth_logout" | "core_stop" => Ok(Value::Null),
    "api_get" => {
      let path = args["path"].as_str().unwrap_or("");
      match bridge.alice.service.api_get(path).await {
        Ok(response) => Ok(json!({ "status": response.status, "body": response.body })),
        Err(error) => Err(error.to_string()),
      }
    },
    "api_request" => {
      let method: ApiMethod = serde_json::from_value(args["method"].clone()).expect("method");
      let path = args["path"].as_str().unwrap_or("");
      let body = args["body"].as_str().map(str::to_owned);
      match bridge.alice.service.api_request(method, path, body).await {
        Ok(response) => Ok(json!({ "status": response.status, "body": response.body })),
        Err(error) => Err(error.to_string()),
      }
    },
    "core_link_start" => bridge
      .alice
      .runtime
      .link_start(args["device_name"].as_str().unwrap_or("Desktop").to_owned())
      .await
      .map(|()| Value::Null),
    "core_link_status" => {
      Ok(serde_json::to_value(bridge.alice.runtime.link_status().await).expect("link status"))
    },
    "core_link_cancel" => {
      bridge.alice.runtime.link_cancel();
      Ok(Value::Null)
    },
    "core_approval_start" => bridge
      .alice
      .runtime
      .approval_start(args["code"].as_str().unwrap_or("").to_owned())
      .await
      .map(|()| Value::Null),
    "core_approval_confirm" => bridge
      .alice
      .runtime
      .approval_confirm(args["short_code"].as_str().unwrap_or("").to_owned())
      .await
      .map(|()| Value::Null),
    "core_approval_cancel" => {
      bridge.alice.runtime.approval_cancel();
      Ok(Value::Null)
    },
    "core_link_announce" => bridge.alice.runtime.link_announce().await.map(|()| Value::Null),
    "core_link_requests" => {
      Ok(serde_json::to_value(bridge.alice.runtime.link_requests().await).expect("link requests"))
    },
    "core_link_request_review" => bridge
      .alice
      .runtime
      .link_request_review(args["id"].as_str().unwrap_or("").to_owned())
      .await
      .map(|()| Value::Null),
    "core_link_request_reject" => bridge
      .alice
      .runtime
      .link_request_reject(args["id"].as_str().unwrap_or("").to_owned())
      .await
      .map(|()| Value::Null),
    "call_snapshot" => Ok(serde_json::to_value(bridge.calls.snapshot()).expect("call snapshot")),
    "call_media_devices" => Ok(serde_json::to_value(bridge.calls.snapshot().devices).expect("devices")),
    "call_action" => {
      let action: CallAction = serde_json::from_value(args["action"].clone()).expect("call action");
      bridge.calls.act(action);
      Ok(Value::Null)
    },
    "core_start" | "core_status" => {
      Ok(serde_json::to_value(bridge.alice.runtime.status().await).expect("status"))
    },
    "core_enroll" => {
      let request: EnrollRequest = serde_json::from_value(args["request"].clone()).expect("enroll request");
      bridge.alice.runtime.enroll(request).await.map(|()| Value::Null)
    },
    "core_snapshot" => bridge
      .alice
      .runtime
      .snapshot()
      .await
      .map(|snapshot| serde_json::to_value(snapshot).expect("snapshot")),
    "core_snapshot_changes" => {
      let maybe_since = serde_json::from_value(args["since"].clone()).expect("since");
      bridge
        .alice
        .runtime
        .snapshot_changes(maybe_since)
        .await
        .map(|changes| serde_json::to_value(changes).expect("snapshot changes"))
    },
    "core_focus" => {
      let conversation = serde_json::from_value(args["conversation"].clone()).expect("conversation");
      bridge.alice.runtime.focus(conversation);
      Ok(Value::Null)
    },
    "core_messages" => {
      let conversation = serde_json::from_value(args["conversation"].clone()).expect("conversation");
      let (start, len) = (
        args["start"].as_u64().unwrap_or(0) as usize,
        args["len"].as_u64().unwrap_or(100) as usize,
      );
      bridge
        .alice
        .runtime
        .messages(conversation, start, len)
        .await
        .map(|window| serde_json::to_value(window).expect("window"))
    },
    "core_thread" => {
      let conversation = serde_json::from_value(args["conversation"].clone()).expect("conversation");
      let thread = serde_json::from_value(args["thread"].clone()).expect("thread");
      bridge
        .alice
        .runtime
        .thread(conversation, thread)
        .await
        .map(|messages| serde_json::to_value(messages).expect("thread"))
    },
    "core_action" => {
      let action: ChatAction = serde_json::from_value(args["action"].clone()).expect("action");
      let outcome = bridge.alice.runtime.action(action).await;
      // The real shell emits `core-changed` from the core; the bridge approximates it per action.
      bump(bridge);
      outcome.map(|()| Value::Null)
    },
    other => Err(format!("the dev bridge does not know the command {other}")),
  };
  // Whatever the core did in the background (gateway events), the UI polls cheaply.
  bump_soon(bridge.clone());
  match result {
    Ok(value) => json!({ "ok": value }),
    Err(error) => json!({ "error": error }),
  }
}

fn bump(bridge: &Bridge) {
  bridge.changes.fetch_add(1, Ordering::Relaxed);
  bridge.changed.notify_waiters();
}

/// The bridge has no hook into the core thread's change callback, so it bumps the counter on a short
/// timer after commands; the UI then re-reads. Good enough for a dev tool.
fn bump_soon(bridge: Arc<Bridge>) {
  tokio::spawn(async move {
    for delay in [300_u64, 1000, 2500] {
      tokio::time::sleep(Duration::from_millis(delay)).await;
      bump(&bridge);
    }
  });
}

fn env_or(name: &str, default: String) -> String {
  std::env::var(name)
    .ok()
    .filter(|value| !value.is_empty())
    .unwrap_or(default)
}

async fn read_request(stream: &mut TcpStream) -> Option<(String, String, String)> {
  let mut buffer = Vec::new();
  let mut chunk = [0_u8; 4096];
  let head_end = loop {
    let read = stream.read(&mut chunk).await.ok()?;
    if read == 0 {
      return None;
    }
    buffer.extend_from_slice(&chunk[..read]);
    if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
      break position + 4;
    }
    if buffer.len() > 64 * 1024 {
      return None;
    }
  };
  let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
  let length = head
    .lines()
    .find_map(|line| {
      line
        .to_ascii_lowercase()
        .strip_prefix("content-length:")
        .map(|value| value.trim().parse::<usize>().unwrap_or(0))
    })
    .unwrap_or(0);
  while buffer.len() < head_end + length {
    let read = stream.read(&mut chunk).await.ok()?;
    if read == 0 {
      break;
    }
    buffer.extend_from_slice(&chunk[..read]);
  }
  let mut parts = head.lines().next()?.split_whitespace();
  let (method, path) = (parts.next()?.to_owned(), parts.next()?.to_owned());
  let body = String::from_utf8_lossy(&buffer[head_end..(head_end + length).min(buffer.len())]).into_owned();
  Some((method, path, body))
}
