//! Messages the server holds until their time (docs/scheduled-messages.md), against a real server and
//! Postgres. The scheduler takes its time as an argument, so the tests move time by asking for later.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_server::services::scheduled_messages::run_due;
use teamcraft_server::testing::{Reply, TestApp, TestUser};
use time::{Duration as TimeDuration, OffsetDateTime};

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

/// carol, who schedules, and dave in a private channel.
struct Scene {
  channel: String,
  carol: TestUser,
  dave: TestUser,
  carol_device: String,
  dave_device: String,
}

async fn scene(app: &TestApp) -> Scene {
  let (carol, dave) = (member("carol"), member("dave"));
  let channel = channel_with(app, &[&carol, &dave]).await;
  let (carol_device, dave_device) = (
    register_device(app, &carol).await,
    register_device(app, &dave).await,
  );
  Scene {
    channel,
    carol,
    dave,
    carol_device,
    dave_device,
  }
}

fn later(seconds: i64) -> OffsetDateTime {
  OffsetDateTime::now_utc() + TimeDuration::seconds(seconds)
}

fn millis(time: OffsetDateTime) -> i64 {
  i64::try_from(time.unix_timestamp_nanos() / 1_000_000).unwrap()
}

fn sealed(device: &str, epoch: u64, text: &str) -> Value {
  use base64::Engine;
  json!({
    "device_id": device,
    "content_key_id": "a2V5",
    "ciphertext": base64::engine::general_purpose::STANDARD.encode(text),
    "mls_epoch": epoch,
  })
}

async fn schedule_at(app: &TestApp, s: &Scene, n: u8, due_at: i64, sealed: Value) -> Reply {
  app
    .post(
      &s.carol,
      &format!("/conversations/{}/scheduled-messages", s.channel),
      json!({"client_message_id": client_id(n), "sealed": sealed, "due_at": due_at}),
    )
    .await
}

/// Schedules for a minute from now and returns the entry's ID.
async fn schedule(app: &TestApp, s: &Scene, n: u8) -> String {
  let reply = schedule_at(
    app,
    s,
    n,
    millis(later(60)),
    sealed(&s.carol_device, 0, &format!("text {n}")),
  )
  .await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  reply.id()
}

async fn commit(app: &TestApp, s: &Scene, epoch: u64) {
  let reply = app
    .post(
      &s.dave,
      &format!("/conversations/{}/mls/commits", s.channel),
      json!({"device_id": s.dave_device, "epoch": epoch, "commit": "Y29tbWl0", "welcomes": []}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
}

/// The ciphertexts dave reads in the channel, oldest first.
async fn delivered(app: &TestApp, s: &Scene) -> Vec<String> {
  let reply = app
    .get(&s.dave, &format!("/conversations/{}/messages", s.channel))
    .await;
  let mut found: Vec<String> = reply.body["messages"]
    .as_array()
    .unwrap()
    .iter()
    .map(|message| message["ciphertext"].as_str().unwrap().to_owned())
    .collect();
  found.reverse();
  found
}

async fn listed(app: &TestApp, user: &TestUser) -> Vec<Value> {
  let reply = app.get(user, "/me/scheduled-messages").await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  reply.body["messages"].as_array().unwrap().clone()
}

async fn hold_of_first(app: &TestApp, user: &TestUser) -> Option<String> {
  listed(app, user).await[0]["hold"].as_str().map(str::to_owned)
}

mod release {
  use super::*;

  #[tokio::test]
  async fn goes_out_at_its_time_once_with_no_device_around() {
    let app = app!();
    let s = scene(&app).await;
    schedule(&app, &s, 1).await;

    assert_eq!(run_due(&app.state, later(30)).await.unwrap(), 0, "not yet");
    assert!(delivered(&app, &s).await.is_empty());

    assert_eq!(run_due(&app.state, later(61)).await.unwrap(), 1);
    assert_eq!(delivered(&app, &s).await.len(), 1);
    assert!(listed(&app, &s.carol).await.is_empty());

    assert_eq!(run_due(&app.state, later(120)).await.unwrap(), 0);
    assert_eq!(delivered(&app, &s).await.len(), 1);
  }

  #[tokio::test]
  async fn members_hear_of_it_over_the_gateway() {
    let app = app!();
    let s = scene(&app).await;
    let mut socket = ws::connect(&app, &s.dave, &s.dave_device).await;
    schedule(&app, &s, 1).await;
    run_due(&app.state, later(61)).await.unwrap();
    let events = ws::events(&mut socket, std::time::Duration::from_millis(300)).await;
    assert!(
      events
        .iter()
        .any(|event| matches!(event, teamcraft_proto::GatewayEvent::MessageCreate { .. })),
      "{events:?}"
    );
  }

  /// A crash after the claim: the next instance takes over once the lease ran out, and the message
  /// still goes out once.
  #[tokio::test]
  async fn what_a_crash_left_behind_goes_out_after_the_lease() {
    let app = app!();
    let s = scene(&app).await;
    schedule(&app, &s, 1).await;
    let claimed = app
      .state
      .database
      .claim_due_scheduled_messages(later(61), std::time::Duration::from_secs(60), 10)
      .await
      .unwrap();
    assert_eq!(claimed.len(), 1);

    assert_eq!(run_due(&app.state, later(62)).await.unwrap(), 0, "still claimed");
    assert!(delivered(&app, &s).await.is_empty());

    assert_eq!(run_due(&app.state, later(122)).await.unwrap(), 1);
    assert_eq!(delivered(&app, &s).await.len(), 1);
  }

  #[tokio::test]
  async fn two_scheduler_instances_send_it_once() {
    let app = app!();
    let s = scene(&app).await;
    for n in 1..=20 {
      schedule(&app, &s, n).await;
    }
    let at = later(61);
    let (first, second) = tokio::join!(run_due(&app.state, at), run_due(&app.state, at));
    assert_eq!(first.unwrap() + second.unwrap(), 20, "each took different ones");
    assert_eq!(delivered(&app, &s).await.len(), 20);
  }

  /// A send with the same client message ID came first: the entry ends and nothing is stored twice.
  #[tokio::test]
  async fn the_client_message_id_keeps_it_to_one_message() {
    let app = app!();
    let s = scene(&app).await;
    schedule(&app, &s, 1).await;
    send(&app, &s.carol, &s.channel, &s.carol_device, 1).await;
    run_due(&app.state, later(61)).await.unwrap();
    assert_eq!(delivered(&app, &s).await.len(), 1);
    assert!(listed(&app, &s.carol).await.is_empty());
  }

  #[tokio::test]
  async fn people_who_left_are_no_longer_mentioned() {
    let app = app!();
    let s = scene(&app).await;
    let (carol_id, dave_id) = (user_id(&app, &s.carol).await, user_id(&app, &s.dave).await);
    let mut body = sealed(&s.carol_device, 0, "hello");
    body["mentioned_user_ids"] = json!([carol_id, dave_id]);
    let reply = schedule_at(&app, &s, 1, millis(later(60)), body).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let removal = app
      .delete(
        &s.dave,
        &format!("/conversations/{}/members/{dave_id}", s.channel),
      )
      .await;
    assert_eq!(removal.status, StatusCode::NO_CONTENT, "{}", removal.body);

    run_due(&app.state, later(61)).await.unwrap();
    let reply = app
      .get(&s.carol, &format!("/conversations/{}/messages", s.channel))
      .await;
    assert_eq!(reply.body["messages"][0]["mentioned_user_ids"], json!([carol_id]));
  }
}

mod racing {
  use super::*;

  #[tokio::test]
  async fn a_cancel_before_the_release_stops_it() {
    let app = app!();
    let s = scene(&app).await;
    let id = schedule(&app, &s, 1).await;
    // The scheduler claimed it and is about to send when the cancel comes in.
    app
      .state
      .database
      .claim_due_scheduled_messages(later(61), std::time::Duration::from_secs(60), 10)
      .await
      .unwrap();
    let cancel = app
      .delete(&s.carol, &format!("/me/scheduled-messages/{id}"))
      .await;
    assert_eq!(cancel.status, StatusCode::NO_CONTENT, "{}", cancel.body);

    assert_eq!(run_due(&app.state, later(122)).await.unwrap(), 0);
    assert!(delivered(&app, &s).await.is_empty());
  }

  #[tokio::test]
  async fn a_cancel_after_the_release_says_it_is_gone() {
    let app = app!();
    let s = scene(&app).await;
    let id = schedule(&app, &s, 1).await;
    run_due(&app.state, later(61)).await.unwrap();
    let cancel = app
      .delete(&s.carol, &format!("/me/scheduled-messages/{id}"))
      .await;
    assert_eq!(cancel.status, StatusCode::NOT_FOUND);
    assert_eq!(cancel.error_code(), "scheduled_message_not_found");
    assert_eq!(delivered(&app, &s).await.len(), 1);
  }

  /// A release that read the row before an edit doesn't send the old text.
  #[tokio::test]
  async fn an_edit_after_the_claim_wins() {
    let app = app!();
    let s = scene(&app).await;
    let id = schedule(&app, &s, 1).await;
    let claimed = app
      .state
      .database
      .claim_due_scheduled_messages(later(61), std::time::Duration::from_secs(60), 10)
      .await
      .unwrap();
    let due_at = millis(later(300));
    let edit = app
      .put(
        &s.carol,
        &format!("/me/scheduled-messages/{id}"),
        json!({"expected_version": 1, "due_at": due_at, "sealed": sealed(&s.carol_device, 0, "edited")}),
      )
      .await;
    assert_eq!(edit.status, StatusCode::OK, "{}", edit.body);

    // What the instance with the claim does next: its version is old, so nothing happens.
    let outcome = app
      .state
      .database
      .release_scheduled_message(
        claimed[0].id,
        claimed[0].version,
        claimed[0].mls_epoch,
        &teamcraft_db::NewMessage {
          id: app.state.next_id().unwrap(),
          client_message_id: claimed[0].client_message_id,
          conversation_id: claimed[0].conversation_id,
          sender_user_id: claimed[0].user_id,
          sender_device_id: claimed[0].sender_device_id,
          maybe_thread_id: None,
          mentioned_user_ids: &[],
          is_urgent: false,
          content_key_id: &claimed[0].content_key_id,
          ciphertext: &claimed[0].ciphertext,
        },
      )
      .await
      .unwrap();
    assert!(matches!(outcome, teamcraft_db::ReleaseOutcome::Changed));
    assert!(delivered(&app, &s).await.is_empty());
  }

  #[tokio::test]
  async fn the_second_of_two_racing_seals_loses() {
    let app = app!();
    let s = scene(&app).await;
    let id = schedule(&app, &s, 1).await;
    commit(&app, &s, 0).await;
    let due_at = listed(&app, &s.carol).await[0]["due_at"].clone();
    let path = format!("/me/scheduled-messages/{id}");
    let body = |text: &str| json!({"expected_version": 1, "due_at": due_at, "sealed": sealed(&s.carol_device, 1, text)});
    assert_eq!(
      app.put(&s.carol, &path, body("first")).await.status,
      StatusCode::OK
    );
    let second = app.put(&s.carol, &path, body("second")).await;
    assert_eq!(second.status, StatusCode::CONFLICT);
    assert_eq!(second.error_code(), "version_conflict");
  }
}

mod stale_epoch {
  use super::*;

  #[tokio::test]
  async fn a_commit_keeps_it_from_going_out_under_the_old_key() {
    let app = app!();
    let s = scene(&app).await;
    schedule(&app, &s, 1).await;
    commit(&app, &s, 0).await;
    assert_eq!(listed(&app, &s.carol).await[0]["is_epoch_current"], json!(false));

    run_due(&app.state, later(61)).await.unwrap();
    assert!(delivered(&app, &s).await.is_empty());
    assert_eq!(
      hold_of_first(&app, &s.carol).await.as_deref(),
      Some("stale_epoch")
    );
    assert_eq!(
      run_due(&app.state, later(120)).await.unwrap(),
      0,
      "held, not tried again"
    );
  }

  #[tokio::test]
  async fn sealed_again_it_goes_out_at_its_time() {
    let app = app!();
    let s = scene(&app).await;
    let id = schedule(&app, &s, 1).await;
    commit(&app, &s, 0).await;
    let due_at = listed(&app, &s.carol).await[0]["due_at"].clone();
    let seal = app
      .put(
        &s.carol,
        &format!("/me/scheduled-messages/{id}"),
        json!({"expected_version": 1, "due_at": due_at, "sealed": sealed(&s.carol_device, 1, "new key")}),
      )
      .await;
    assert_eq!(seal.status, StatusCode::OK, "{}", seal.body);

    run_due(&app.state, later(61)).await.unwrap();
    use base64::Engine;
    let text = base64::engine::general_purpose::STANDARD.encode("new key");
    assert_eq!(delivered(&app, &s).await, [text]);
  }

  /// Late: the time has passed. A device that comes back seals it again at the same time, and it goes.
  #[tokio::test]
  async fn a_late_one_goes_out_once_a_device_is_back() {
    let app = app!();
    let s = scene(&app).await;
    let id = schedule(&app, &s, 1).await;
    commit(&app, &s, 0).await;
    run_due(&app.state, later(61)).await.unwrap();
    let held = listed(&app, &s.carol).await[0].clone();
    let seal = app
      .put(
        &s.carol,
        &format!("/me/scheduled-messages/{id}"),
        json!({"expected_version": held["version"], "due_at": held["due_at"], "sealed": sealed(&s.carol_device, 1, "new key")}),
      )
      .await;
    assert_eq!(seal.status, StatusCode::OK, "{}", seal.body);
    assert_eq!(listed(&app, &s.carol).await[0].get("hold"), None);

    run_due(&app.state, later(62)).await.unwrap();
    assert_eq!(delivered(&app, &s).await.len(), 1);
  }

  /// Somebody mentioned left: that commit moves the epoch, and sealing the same text again must work.
  #[tokio::test]
  async fn a_mention_of_somebody_who_left_doesn_t_block_sealing_again() {
    let app = app!();
    let s = scene(&app).await;
    let (carol_id, dave_id) = (user_id(&app, &s.carol).await, user_id(&app, &s.dave).await);
    let mut body = sealed(&s.carol_device, 0, "hello");
    body["mentioned_user_ids"] = json!([carol_id, dave_id]);
    let id = schedule_at(&app, &s, 1, millis(later(60)), body.clone())
      .await
      .id();
    let removal = app
      .delete(
        &s.dave,
        &format!("/conversations/{}/members/{dave_id}", s.channel),
      )
      .await;
    assert_eq!(removal.status, StatusCode::NO_CONTENT, "{}", removal.body);
    let reply = app
      .post(
        &s.carol,
        &format!("/conversations/{}/mls/commits", s.channel),
        json!({"device_id": s.carol_device, "epoch": 0, "commit": "Y29tbWl0", "welcomes": []}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);

    let held = listed(&app, &s.carol).await[0].clone();
    body["mls_epoch"] = json!(1);
    let seal = app
      .put(
        &s.carol,
        &format!("/me/scheduled-messages/{id}"),
        json!({"expected_version": held["version"], "due_at": held["due_at"], "sealed": body}),
      )
      .await;
    assert_eq!(seal.status, StatusCode::OK, "{}", seal.body);
    assert_eq!(seal.body["mentioned_user_ids"], json!([carol_id]));
  }

  #[tokio::test]
  async fn an_absurd_expected_version_is_a_bad_request() {
    let app = app!();
    let s = scene(&app).await;
    let id = schedule(&app, &s, 1).await;
    let due_at = millis(later(90));
    let reply = app
      .put(
        &s.carol,
        &format!("/me/scheduled-messages/{id}"),
        json!({"expected_version": u64::MAX, "due_at": due_at}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
  }

  #[tokio::test]
  async fn sealing_needs_the_current_epoch() {
    let app = app!();
    let s = scene(&app).await;
    let id = schedule(&app, &s, 1).await;
    commit(&app, &s, 0).await;
    let due_at = listed(&app, &s.carol).await[0]["due_at"].clone();
    let seal = app
      .put(
        &s.carol,
        &format!("/me/scheduled-messages/{id}"),
        json!({"expected_version": 1, "due_at": due_at, "sealed": sealed(&s.carol_device, 0, "old key")}),
      )
      .await;
    assert_eq!(seal.status, StatusCode::CONFLICT);
    assert_eq!(seal.error_code(), "epoch_conflict");
  }

  #[tokio::test]
  async fn send_now_refuses_the_old_key_too() {
    let app = app!();
    let s = scene(&app).await;
    let id = schedule(&app, &s, 1).await;
    commit(&app, &s, 0).await;
    let send = app
      .post(&s.carol, &format!("/me/scheduled-messages/{id}/send"), json!({}))
      .await;
    assert_eq!(send.status, StatusCode::CONFLICT);
    assert_eq!(send.error_code(), "epoch_conflict");
    assert!(delivered(&app, &s).await.is_empty());
  }

  /// The device that signed it is gone: members would refuse the message, so another device seals it.
  #[tokio::test]
  async fn a_revoked_signing_device_holds_it_like_a_stale_epoch() {
    let app = app!();
    let s = scene(&app).await;
    schedule(&app, &s, 1).await;
    let revoked = app
      .delete(&s.carol, &format!("/devices/{}", s.carol_device))
      .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT, "{}", revoked.body);
    run_due(&app.state, later(61)).await.unwrap();
    assert!(delivered(&app, &s).await.is_empty());
    assert_eq!(
      hold_of_first(&app, &s.carol).await.as_deref(),
      Some("stale_epoch")
    );
  }
}

mod must_not_go_out {
  use super::*;

  #[tokio::test]
  async fn a_sender_who_left() {
    let app = app!();
    let s = scene(&app).await;
    schedule(&app, &s, 1).await;
    let carol_id = user_id(&app, &s.carol).await;
    let left = app
      .delete(
        &s.carol,
        &format!("/conversations/{}/members/{carol_id}", s.channel),
      )
      .await;
    assert_eq!(left.status, StatusCode::NO_CONTENT, "{}", left.body);

    run_due(&app.state, later(61)).await.unwrap();
    assert!(delivered(&app, &s).await.is_empty());
    assert_eq!(
      hold_of_first(&app, &s.carol).await.as_deref(),
      Some("not_a_member")
    );
  }

  #[tokio::test]
  async fn a_deleted_conversation() {
    let app = app!();
    let s = scene(&app).await;
    schedule(&app, &s, 1).await;
    let deleted = app
      .delete(&s.carol, &format!("/conversations/{}", s.channel))
      .await;
    if deleted.status == StatusCode::FORBIDDEN {
      let owner = owner();
      let by_owner = app.delete(&owner, &format!("/conversations/{}", s.channel)).await;
      assert_eq!(by_owner.status, StatusCode::NO_CONTENT, "{}", by_owner.body);
    } else {
      assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);
    }

    run_due(&app.state, later(61)).await.unwrap();
    assert_eq!(
      hold_of_first(&app, &s.carol).await.as_deref(),
      Some("conversation_gone")
    );
  }

  #[tokio::test]
  async fn a_deactivated_sender() {
    let app = app!();
    let s = scene(&app).await;
    schedule(&app, &s, 1).await;
    let carol_id = user_id(&app, &s.carol).await;
    let deactivated = app
      .post(
        &owner(),
        &format!("/admin/users/{carol_id}/deactivate"),
        json!({}),
      )
      .await;
    assert_eq!(deactivated.status, StatusCode::NO_CONTENT, "{}", deactivated.body);

    run_due(&app.state, later(61)).await.unwrap();
    let reply = app
      .get(&s.dave, &format!("/conversations/{}/messages", s.channel))
      .await;
    assert_eq!(
      reply.body["messages"],
      json!([]),
      "nothing goes out in their name"
    );
  }

  #[tokio::test]
  async fn a_sender_who_lost_the_right_to_post() {
    let app = app!();
    let (alice, bob, carol) = (owner(), admin(), member("carol"));
    let carol_id = user_id(&app, &carol).await;
    let team = create_space(&app, &alice, "Team", "invite_only").await;
    add_space_member(&app, &alice, &team, &carol_id, "member").await;
    let channel = create_channel(&app, &alice, &team, "news", "private").await;
    add_channel_member(&app, &alice, &channel, &carol_id).await;
    let device = register_device(&app, &carol).await;
    let scheduled = app
      .post(
        &carol,
        &format!("/conversations/{channel}/scheduled-messages"),
        json!({"client_message_id": client_id(1), "sealed": sealed(&device, 0, "text"), "due_at": millis(later(60))}),
      )
      .await;
    assert_eq!(scheduled.status, StatusCode::OK, "{}", scheduled.body);
    let revoked = app
      .put(
        &bob,
        &format!("/admin/spaces/{team}/permission-scheme"),
        json!({"changes": [{"role": "member", "permission": "create_post", "effect": "revoke"}]}),
      )
      .await;
    assert_eq!(revoked.status, StatusCode::OK, "{}", revoked.body);

    run_due(&app.state, later(61)).await.unwrap();
    assert_eq!(
      hold_of_first(&app, &carol).await.as_deref(),
      Some("no_permission")
    );
    let reply = app
      .get(&alice, &format!("/conversations/{channel}/messages"))
      .await;
    assert_eq!(reply.body["messages"], json!([]));
  }
}

mod cleaning_up {
  use super::*;

  #[tokio::test]
  async fn a_deleted_account_takes_its_ciphertext_along() {
    let app = app!();
    let s = scene(&app).await;
    schedule(&app, &s, 1).await;
    let carol_id = user_id(&app, &s.carol).await;
    let deactivated = app
      .post(
        &owner(),
        &format!("/admin/users/{carol_id}/deactivate"),
        json!({}),
      )
      .await;
    assert_eq!(deactivated.status, StatusCode::NO_CONTENT, "{}", deactivated.body);
    let deleted = app.delete(&owner(), &format!("/admin/users/{carol_id}")).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);
    let held = app
      .state
      .database
      .list_scheduled_messages(carol_id.parse().unwrap())
      .await
      .unwrap();
    assert!(held.is_empty());
  }

  #[tokio::test]
  async fn a_hold_nothing_can_solve_goes_after_its_time() {
    let app = app!();
    let s = scene(&app).await;
    schedule(&app, &s, 1).await;
    let carol_id = user_id(&app, &s.carol).await;
    let left = app
      .delete(
        &s.carol,
        &format!("/conversations/{}/members/{carol_id}", s.channel),
      )
      .await;
    assert_eq!(left.status, StatusCode::NO_CONTENT, "{}", left.body);
    run_due(&app.state, later(61)).await.unwrap();
    let held = listed(&app, &s.carol).await;
    assert_eq!(held.len(), 1, "they still see why");
    assert!(held[0]["removes_at"].is_i64(), "and when it goes: {}", held[0]);

    let days = i64::from(app.state.settings.get().limits.scheduled_hold_retention_days);
    run_due(&app.state, later(days * 86_400 + 61)).await.unwrap();
    assert!(listed(&app, &s.carol).await.is_empty());
  }

  /// Held for an old epoch, then the sender left: nothing can solve it any more, so it counts as theirs
  /// no more and goes after the retention like any such hold.
  #[tokio::test]
  async fn a_stale_one_whose_sender_left_is_held_as_not_a_member() {
    let app = app!();
    let s = scene(&app).await;
    schedule(&app, &s, 1).await;
    commit(&app, &s, 0).await;
    run_due(&app.state, later(61)).await.unwrap();
    assert_eq!(
      hold_of_first(&app, &s.carol).await.as_deref(),
      Some("stale_epoch")
    );
    assert!(listed(&app, &s.carol).await[0]["removes_at"].is_null());

    let carol_id = user_id(&app, &s.carol).await;
    let left = app
      .delete(
        &s.carol,
        &format!("/conversations/{}/members/{carol_id}", s.channel),
      )
      .await;
    assert_eq!(left.status, StatusCode::NO_CONTENT, "{}", left.body);
    run_due(&app.state, later(62)).await.unwrap();
    assert_eq!(
      hold_of_first(&app, &s.carol).await.as_deref(),
      Some("not_a_member")
    );
    assert!(listed(&app, &s.carol).await[0]["removes_at"].is_i64());

    let days = i64::from(app.state.settings.get().limits.scheduled_hold_retention_days);
    run_due(&app.state, later(days * 86_400 + 63)).await.unwrap();
    assert!(listed(&app, &s.carol).await.is_empty());
  }

  #[tokio::test]
  async fn a_stale_one_isn_t_purged() {
    let app = app!();
    let s = scene(&app).await;
    schedule(&app, &s, 1).await;
    commit(&app, &s, 0).await;
    run_due(&app.state, later(61)).await.unwrap();
    let days = i64::from(app.state.settings.get().limits.scheduled_hold_retention_days);
    run_due(&app.state, later(days * 86_400 + 61)).await.unwrap();
    assert_eq!(listed(&app, &s.carol).await.len(), 1);
  }
}

mod policy {
  use super::*;

  fn set(app: &TestApp, change: impl FnOnce(&mut teamcraft_proto::ScheduledMessagePolicy)) {
    let mut policy = app.state.policy.get().as_ref().clone();
    change(&mut policy.scheduled_messages);
    app.state.policy.set(policy);
  }

  #[tokio::test]
  async fn off_refuses_new_ones_holds_waiting_ones_and_leaves_send_now() {
    let app = app!();
    let s = scene(&app).await;
    let id = schedule(&app, &s, 1).await;
    set(&app, |policy| policy.is_enabled = false);

    let refused = schedule_at(&app, &s, 2, millis(later(60)), sealed(&s.carol_device, 0, "x")).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert_eq!(refused.error_code(), "scheduled_messages_disabled");

    run_due(&app.state, later(61)).await.unwrap();
    assert!(delivered(&app, &s).await.is_empty());
    assert_eq!(hold_of_first(&app, &s.carol).await.as_deref(), Some("policy_off"));

    let sent = app
      .post(&s.carol, &format!("/me/scheduled-messages/{id}/send"), json!({}))
      .await;
    assert_eq!(sent.status, StatusCode::OK, "{}", sent.body);
    assert_eq!(delivered(&app, &s).await.len(), 1);
  }

  #[tokio::test]
  async fn the_horizon_and_the_cap_hold() {
    let app = app!();
    let s = scene(&app).await;
    set(&app, |policy| {
      policy.max_horizon_days = 1;
      policy.max_per_user = 1;
    });
    let far = schedule_at(
      &app,
      &s,
      1,
      millis(later(2 * 86_400)),
      sealed(&s.carol_device, 0, "x"),
    )
    .await;
    assert_eq!(far.status, StatusCode::BAD_REQUEST);
    assert_eq!(far.error_code(), "scheduled_time_invalid");

    schedule(&app, &s, 1).await;
    let over = schedule_at(&app, &s, 2, millis(later(60)), sealed(&s.carol_device, 0, "x")).await;
    assert_eq!(over.status, StatusCode::CONFLICT);
    assert_eq!(over.error_code(), "scheduled_message_limit");
  }
}

mod hostile_input {
  use super::*;

  #[tokio::test]
  async fn a_past_time_another_epoch_and_a_foreign_device_are_refused() {
    let app = app!();
    let s = scene(&app).await;
    let past = schedule_at(&app, &s, 1, millis(later(-1)), sealed(&s.carol_device, 0, "x")).await;
    assert_eq!(past.status, StatusCode::BAD_REQUEST);
    assert_eq!(past.error_code(), "scheduled_time_invalid");

    let far_out = schedule_at(&app, &s, 1, i64::MAX, sealed(&s.carol_device, 0, "x")).await;
    assert_eq!(far_out.status, StatusCode::BAD_REQUEST);

    let epoch = schedule_at(&app, &s, 1, millis(later(60)), sealed(&s.carol_device, 7, "x")).await;
    assert_eq!(epoch.status, StatusCode::CONFLICT);
    assert_eq!(epoch.error_code(), "epoch_conflict");
    assert_eq!(epoch.body["current_epoch"], json!(0));

    let device = schedule_at(&app, &s, 1, millis(later(60)), sealed(&s.dave_device, 0, "x")).await;
    assert_eq!(device.status, StatusCode::NOT_FOUND);
  }

  #[tokio::test]
  async fn an_empty_or_oversized_text_and_strangers_in_the_mentions_are_refused() {
    let app = app!();
    let s = scene(&app).await;
    let mut empty = sealed(&s.carol_device, 0, "x");
    empty["ciphertext"] = json!("");
    let reply = schedule_at(&app, &s, 1, millis(later(60)), empty).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);

    let huge = "x".repeat(app.state.settings.get().limits.max_ciphertext_bytes + 1);
    let reply = schedule_at(&app, &s, 1, millis(later(60)), sealed(&s.carol_device, 0, &huge)).await;
    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);

    let stranger = user_id(&app, &member("stranger")).await;
    let mut body = sealed(&s.carol_device, 0, "x");
    body["mentioned_user_ids"] = json!([stranger]);
    let reply = schedule_at(&app, &s, 1, millis(later(60)), body).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
  }

  #[tokio::test]
  async fn somebody_outside_can_t_schedule_into_a_conversation() {
    let app = app!();
    let s = scene(&app).await;
    let erin = member("erin");
    let device = register_device(&app, &erin).await;
    let reply = app
      .post(
        &erin,
        &format!("/conversations/{}/scheduled-messages", s.channel),
        json!({"client_message_id": client_id(1), "sealed": sealed(&device, 0, "x"), "due_at": millis(later(60))}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
  }

  #[tokio::test]
  async fn only_the_sender_sees_or_touches_an_entry() {
    let app = app!();
    let s = scene(&app).await;
    let id = schedule(&app, &s, 1).await;
    assert!(listed(&app, &s.dave).await.is_empty());
    let path = format!("/me/scheduled-messages/{id}");
    let due_at = millis(later(90));
    for reply in [
      app.delete(&s.dave, &path).await,
      app.post(&s.dave, &format!("{path}/send"), json!({})).await,
      app
        .put(&s.dave, &path, json!({"expected_version": 1, "due_at": due_at}))
        .await,
    ] {
      assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.body);
      assert_eq!(reply.error_code(), "scheduled_message_not_found");
    }
    assert_eq!(listed(&app, &s.carol).await.len(), 1);
  }

  #[tokio::test]
  async fn the_same_client_message_id_again_answers_with_what_is_stored() {
    let app = app!();
    let s = scene(&app).await;
    let first = schedule(&app, &s, 1).await;
    let again = schedule_at(
      &app,
      &s,
      1,
      millis(later(60)),
      sealed(&s.carol_device, 0, "other"),
    )
    .await;
    assert_eq!(again.id(), first);
    assert_eq!(listed(&app, &s.carol).await.len(), 1);
  }
}

mod the_senders_devices {
  use super::*;

  /// The list shows only what the server holds, and tells the sender's other devices of changes.
  #[tokio::test]
  async fn hear_of_changes_and_read_the_same_list() {
    let app = app!();
    let s = scene(&app).await;
    let second = register_device(&app, &s.carol).await;
    let mut socket = ws::connect(&app, &s.carol, &second).await;
    schedule(&app, &s, 1).await;
    let events = ws::events(&mut socket, std::time::Duration::from_millis(300)).await;
    assert!(
      events
        .iter()
        .any(|event| matches!(event, teamcraft_proto::GatewayEvent::ScheduledMessagesUpdate)),
      "{events:?}"
    );
    let entry = &listed(&app, &s.carol).await[0];
    assert_eq!(entry["conversation_id"], json!(s.channel));
    assert_eq!(entry["version"], json!(1));
  }
}
