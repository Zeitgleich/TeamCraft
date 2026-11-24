//! Ballot boxes of anonymous polls (issue #2121, e2ee.md §4.12.1) against a real server and
//! Postgres: one ballot per account, refused once closed, handed out without voters only then.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_server::testing::{TestApp, TestUser};
use time::{Duration, OffsetDateTime};

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

/// Stand-ins for sealed ballots: the server can't read them and doesn't try.
const BALLOT_A: &str = "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyAhIiMkJSYnKCkqKywt";
const BALLOT_B: &str = "LS0tLS0tLS0tLS0tLS0tLS0tLS0tLS0tLS0tLS0tLS0tLS0tLS0tLS0tLS0t";

/// Alice's channel with Bob and Carol, and the poll Alice sent there.
struct Poll {
  alice: TestUser,
  bob: TestUser,
  carol: TestUser,
  path: String,
}

async fn poll(app: &TestApp) -> Poll {
  let alice = owner();
  let bob = member("bob");
  let carol = member("carol");
  let channel = channel_with(app, &[&bob, &carol]).await;
  let device = register_device(app, &alice).await;
  let message = send(app, &alice, &channel, &device, 1).await;
  Poll {
    alice,
    bob,
    carol,
    path: format!("/conversations/{channel}/messages/{message}/ballot-box"),
  }
}

impl Poll {
  async fn open(&self, app: &TestApp, by: &TestUser, closes_at: Option<i64>) -> StatusCode {
    app
      .put(by, &self.path, json!({"closes_at": closes_at}))
      .await
      .status
  }

  async fn cast(&self, app: &TestApp, by: &TestUser, ballot: &str) -> (StatusCode, Value) {
    let reply = app
      .put(by, &format!("{}/ballot", self.path), json!({"ballot": ballot}))
      .await;
    (reply.status, reply.body)
  }

  async fn read(&self, app: &TestApp, by: &TestUser) -> Value {
    let reply = app.get(by, &self.path).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    reply.body
  }
}

#[tokio::test]
async fn one_ballot_per_account_and_none_handed_out_while_open() {
  let app = app!();
  let p = poll(&app).await;
  let (status, body) = p.cast(&app, &p.bob, BALLOT_A).await;
  assert_eq!(status, StatusCode::NOT_FOUND, "no box before its sender opens it");
  assert_eq!(body["error_code"], "ballot_box_not_found");
  assert_eq!(
    p.open(&app, &p.bob, None).await,
    StatusCode::FORBIDDEN,
    "only the poll's sender opens it"
  );
  assert_eq!(p.open(&app, &p.alice, None).await, StatusCode::NO_CONTENT);
  assert_eq!(
    p.open(&app, &p.alice, None).await,
    StatusCode::NO_CONTENT,
    "opening again changes nothing"
  );

  assert_eq!(p.cast(&app, &p.bob, BALLOT_A).await.0, StatusCode::OK);
  let (status, body) = p.cast(&app, &p.bob, BALLOT_B).await;
  assert_eq!(status, StatusCode::OK);
  assert_eq!(body["ballot_count"], 1, "a second ballot replaces the first");
  assert_eq!(body["mine"], BALLOT_B);
  assert_eq!(p.cast(&app, &p.carol, BALLOT_A).await.1["ballot_count"], 2);

  let as_alice = p.read(&app, &p.alice).await;
  assert_eq!(as_alice["ballot_count"], 2);
  assert_eq!(as_alice["is_closed"], false);
  assert!(as_alice.get("mine").is_none() && as_alice.get("ballots").is_none());

  let withdrawn = app.delete(&p.carol, &format!("{}/ballot", p.path)).await;
  assert_eq!(withdrawn.status, StatusCode::OK, "{}", withdrawn.body);
  assert_eq!(withdrawn.body["ballot_count"], 1);
  app.stop().await;
}

#[tokio::test]
async fn closing_hands_out_the_ballots_and_forgets_the_voters() {
  let app = app!();
  let p = poll(&app).await;
  p.open(&app, &p.alice, None).await;
  p.cast(&app, &p.bob, BALLOT_B).await;
  p.cast(&app, &p.carol, BALLOT_A).await;
  let refused = app.post(&p.bob, &format!("{}/close", p.path), json!({})).await;
  assert_eq!(
    refused.status,
    StatusCode::FORBIDDEN,
    "a member who isn't the sender can't close it"
  );

  let closed = app.post(&p.alice, &format!("{}/close", p.path), json!({})).await;
  assert_eq!(closed.status, StatusCode::OK, "{}", closed.body);
  assert_eq!(closed.body["is_closed"], true);
  assert_eq!(
    closed.body["ballots"],
    json!([BALLOT_A, BALLOT_B]),
    "sorted by their bytes"
  );
  let as_bob = p.read(&app, &p.bob).await;
  assert!(as_bob.get("mine").is_none(), "nobody's ballot is theirs any more");

  let (status, body) = p.cast(&app, &p.bob, BALLOT_A).await;
  assert_eq!(status, StatusCode::CONFLICT);
  assert_eq!(body["error_code"], "poll_closed");
  app.stop().await;
}

#[tokio::test]
async fn a_box_closes_by_itself_at_its_time_by_the_servers_clock() {
  let app = app!();
  let p = poll(&app).await;
  let soon_ms = (OffsetDateTime::now_utc() + Duration::seconds(2)).unix_timestamp() * 1_000;
  p.open(&app, &p.alice, Some(soon_ms)).await;
  assert_eq!(p.cast(&app, &p.bob, BALLOT_A).await.0, StatusCode::OK);
  let later = OffsetDateTime::now_utc() + Duration::minutes(1);
  let closed = teamcraft_server::services::poll_ballots::close_due(&app.state, later)
    .await
    .unwrap();
  assert_eq!(closed, 1, "the sweep closes it");
  let read = p.read(&app, &p.carol).await;
  assert_eq!(read["is_closed"], true);
  assert_eq!(read["ballots"], json!([BALLOT_A]));
  assert_eq!(p.cast(&app, &p.carol, BALLOT_B).await.0, StatusCode::CONFLICT);
  app.stop().await;
}

#[tokio::test]
async fn outsiders_and_oversized_ballots_are_refused() {
  let app = app!();
  let p = poll(&app).await;
  p.open(&app, &p.alice, None).await;
  let dave = member("dave");
  user_id(&app, &dave).await;
  let (status, _) = p.cast(&app, &dave, BALLOT_A).await;
  assert!(
    status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND,
    "not a member: {status}"
  );
  let big = base64_of(&[7u8; 65]);
  assert_eq!(p.cast(&app, &p.bob, &big).await.0, StatusCode::BAD_REQUEST);
  app.stop().await;
}

fn base64_of(bytes: &[u8]) -> String {
  use base64::Engine;
  base64::engine::general_purpose::STANDARD.encode(bytes)
}
