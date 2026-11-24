//! The recorder bot of the call recording policy (e2ee.md §8.3, admin.md §3.13, #2273): owners can
//! publish only an active bot account as the recorder, and `recorder_bot_only` only with one.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_proto::{CallRecorders, PolicyDocument, UserId, WorkspacePolicy};
use teamcraft_server::testing::TestApp;

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

#[tokio::test]
async fn only_an_active_bot_account_is_published_as_the_recorder() {
  let app = app!();
  let owner = owner();
  let device = register_device(&app, &owner).await;
  let bot = create_bot(&app, "Recorder").await;
  let carol = app.get(&member("carol"), "/me").await.body["id"]
    .as_str()
    .unwrap()
    .to_owned();
  let append = |payload: Vec<u8>| {
    let app = &app;
    let owner = &owner;
    let device = device.clone();
    async move {
      app
        .post(
          owner,
          "/workspace/policy",
          json!({"expected_version": 0, "payload": b64(&payload), "signature": b64(b"sig"), "signer_device_id": device}),
        )
        .await
        .status
    }
  };
  let cases: Vec<Vec<u8>> = vec![
    // Only the bot records, but there is no bot.
    payload(|recording| recording["recorders"] = json!("recorder_bot_only")),
    // A person's account as the "bot".
    payload(|recording| {
      recording["recorders"] = json!("recorder_bot_only");
      recording["bot"] = json!(carol);
    }),
    // An account that doesn't exist.
    payload(|recording| {
      recording["recorders"] = json!("members");
      recording["bot"] = json!("123456789");
    }),
  ];
  for case in cases {
    assert_eq!(append(case).await, StatusCode::BAD_REQUEST);
  }
  assert_eq!(app.get(&owner, "/workspace/policy").await.body["version"], 0);
  let bot_text = bot.to_string();
  assert_eq!(
    append(payload(|recording| {
      recording["recorders"] = json!("recorder_bot_only");
      recording["bot"] = json!(bot_text);
    }))
    .await,
    StatusCode::CREATED
  );
  let policy = app.state.policy.get();
  assert_eq!(policy.call_recording.recorders, CallRecorders::RecorderBotOnly);
  assert_eq!(policy.call_recording.active_bot(), Some(bot));
  app.stop().await;
}

async fn create_bot(app: &TestApp, name: &str) -> UserId {
  let reply = app.post(&admin(), "/admin/bots", json!({"name": name})).await;
  assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
  reply.body["bot"]["id"].as_str().unwrap().parse().unwrap()
}

fn payload(edit: impl FnOnce(&mut Value)) -> Vec<u8> {
  let mut value = serde_json::to_value(PolicyDocument {
    version: 1,
    policy: WorkspacePolicy::default(),
  })
  .unwrap();
  edit(&mut value["policy"]["call_recording"]);
  serde_json::to_vec(&value).unwrap()
}

fn b64(bytes: &[u8]) -> String {
  use base64::Engine;
  base64::engine::general_purpose::STANDARD.encode(bytes)
}
