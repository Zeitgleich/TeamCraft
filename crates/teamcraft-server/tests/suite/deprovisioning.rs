//! Dependable deprovisioning (issue #210): the deactivation sequence, the safety brake, the sync's
//! health and the audit entries.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::time::Duration;

use common::fake_keycloak::{CLIENT_SECRET, FakeKeycloak, FakeUser, fake_user};
use common::*;
use futures_util::{SinkExt, StreamExt};
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_db::DeactivationAudit;
use teamcraft_proto::{DeviceActivity, GatewayClientMessage, UserId};
use teamcraft_server::config::OidcSyncRunMode;
use teamcraft_server::services::admin::retry_deactivation_followups;
use teamcraft_server::services::oidc::{SyncReport, sync_once};
use teamcraft_server::state::OidcSyncSettings;
use teamcraft_server::testing::{TestApp, TestUser};
use tokio_tungstenite::tungstenite::Message;

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(app: &TestApp, user: &TestUser, device: &str) -> Socket {
  let (mut socket, _) = tokio_tungstenite::connect_async(&app.ws_url).await.unwrap();
  socket.next().await;
  let identify = GatewayClientMessage::Identify {
    maybe_dpop_proof: None,
    token: app.token(user),
    device_id: serde_json::from_value(json!(device)).unwrap(),
    activity: DeviceActivity::Active,
  };
  socket
    .send(Message::Text(serde_json::to_string(&identify).unwrap().into()))
    .await
    .unwrap();
  socket.next().await;
  socket
}

async fn is_closed(socket: &mut Socket) -> bool {
  loop {
    match tokio::time::timeout(Duration::from_millis(700), socket.next()).await {
      Err(_) => return false,
      Ok(None | Some(Err(_)) | Some(Ok(Message::Close(_)))) => return true,
      Ok(Some(Ok(_))) => {},
    }
  }
}

/// A fake provider that knows the owner and the admin of every test.
async fn rig_with(
  users: Vec<FakeUser>,
  edit: impl FnOnce(&mut OidcSyncSettings),
) -> Option<(TestApp, FakeKeycloak)> {
  let mut users = users;
  users.push(fake_user("sub-alice", &["owner"]));
  users.push(fake_user("sub-bob", &["admin"]));
  let provider = FakeKeycloak::start(users).await;
  let mut settings = provider.sync_settings(CLIENT_SECRET, 100);
  edit(&mut settings);
  let app = TestApp::start_with(|s| s.maybe_oidc_sync = Some(settings)).await?;
  Some((app, provider))
}

macro_rules! rig {
  ($users:expr) => {
    match rig_with($users, |_| {}).await {
      Some(rig) => rig,
      None => return,
    }
  };
}

/// `count` members of the provider who have signed in, named `m0`, `m1`, ...
async fn sign_in_members(app: &TestApp, count: usize) -> Vec<String> {
  user_id(app, &owner()).await;
  user_id(app, &admin()).await;
  let mut ids = Vec::new();
  for index in 0..count {
    ids.push(user_id(app, &member(&format!("m{index}"))).await);
  }
  ids
}

fn provider_members(count: usize) -> Vec<FakeUser> {
  (0..count)
    .map(|index| fake_user(&format!("sub-m{index}"), &["member"]))
    .collect()
}

fn disable(provider: &FakeKeycloak, indices: std::ops::Range<usize>) {
  for index in indices {
    provider.update(&format!("sub-m{index}"), |user| user.is_enabled = false);
  }
}

fn status_path() -> &'static str {
  "/admin/oidc-sync"
}

async fn audit_of(app: &TestApp, user: &str) -> Vec<Value> {
  app
    .get(
      &admin(),
      &format!("/admin/audit?target_kind=user&target_id={user}"),
    )
    .await
    .body["entries"]
    .as_array()
    .unwrap()
    .clone()
}

/// The `user_deactivated` entry, with its detail (JSON as text on the wire) parsed.
fn deactivation_entry(entries: &[Value]) -> Value {
  let mut entry = entries
    .iter()
    .find(|entry| entry["action"] == "user_deactivated")
    .expect("a user_deactivated entry")
    .clone();
  entry["detail"] = detail_of(&entry);
  entry
}

fn detail_of(entry: &Value) -> Value {
  serde_json::from_str(entry["detail"].as_str().unwrap()).unwrap()
}

async fn count_followups(app: &TestApp) -> i64 {
  teamcraft_db::testing::sqlx::query_scalar("SELECT count(*) FROM user_deactivation_followups")
    .fetch_one(app.sql_pool())
    .await
    .unwrap()
}

mod sequence {
  use super::*;

  #[tokio::test]
  async fn a_leaver_at_the_provider_loses_everything_the_server_can_take() {
    let (app, provider) = rig!(vec![
      fake_user("sub-carol", &["admin"]),
      fake_user("sub-dave", &["member"])
    ]);
    let carol = TestUser::new("carol", &["admin"]);
    let dave = member("dave");
    let carol_id = user_id(&app, &carol).await;
    let channel = channel_with(&app, &[&carol, &dave]).await;
    let device = register_device(&app, &carol).await;
    let mut socket = connect(&app, &carol, &device).await;
    // A bot she created: she has seen its token.
    let bot = app.post(&carol, "/admin/bots", json!({"name": "Hooks"})).await;
    let bot_token = bot.body["token"].as_str().unwrap().to_owned();
    let bot_id = bot.body["bot"]["id"].as_str().unwrap().to_owned();
    let as_bot = |path: &'static str| {
      app
        .http
        .get(format!("{}/api/v1{path}", app.base_url))
        .bearer_auth(&bot_token)
        .send()
    };
    assert_eq!(as_bot("/me").await.unwrap().status(), StatusCode::OK);

    provider.update("sub-carol", |user| user.is_enabled = false);
    let report = sync_once(&app.state).await.unwrap();
    assert_eq!(report.deactivated, 1);
    assert_eq!(report.failed, 0);

    // The session ended at once and nothing she holds works.
    assert!(is_closed(&mut socket).await, "the gateway connection stayed open");
    assert_eq!(app.get(&carol, "/me").await.error_code(), "account_deactivated");
    // Her bot is paused: the token she knows no longer works.
    assert_eq!(as_bot("/me").await.unwrap().status(), StatusCode::FORBIDDEN);
    assert_eq!(
      app.get(&admin(), "/admin/bots").await.body["bots"][0]["is_paused"],
      true
    );
    // Suspended, not removed: the membership stays for a reactivation.
    let members = app.get(&dave, &format!("/conversations/{channel}/members")).await;
    assert!(
      members.body["members"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["user"]["id"] == carol_id || m["user_id"] == carol_id),
      "{}",
      members.body
    );
    assert_eq!(count_followups(&app).await, 0, "everything was finished");

    // Audited with the source.
    let entry = deactivation_entry(&audit_of(&app, &carol_id).await);
    assert_eq!(entry["detail"]["source"], "oidc_sync");
    assert_eq!(entry["detail"]["reason"], "oidc");
    assert_eq!(entry["detail"]["conversations"], 1);
    assert!(entry.get("actor_user_id").is_none(), "{entry}");
    let bot_entry = deactivation_entry(&audit_of(&app, &bot_id).await);
    assert_eq!(bot_entry["detail"]["source"], "system");
    assert_eq!(bot_entry["detail"]["reason"], "creator_deactivated");

    // The removal from the encryption groups waits for an admin's device, and says so.
    let status = app.get(&admin(), status_path()).await;
    assert_eq!(status.body["pending_removals"], 1, "{}", status.body);
    assert_eq!(status.body["unfinished_deactivations"], 0);

    // Once the provider enables her, she and her bot are back, with everything.
    provider.update("sub-carol", |user| user.is_enabled = true);
    let report = sync_once(&app.state).await.unwrap();
    assert_eq!(report.reactivated, 1);
    assert_eq!(app.get(&carol, "/me").await.status, StatusCode::OK);
    assert_eq!(as_bot("/me").await.unwrap().status(), StatusCode::OK);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_manual_deactivation_is_audited_as_manual_and_a_removal_ends_the_pending_list() {
    let (app, _provider) = rig!(vec![fake_user("sub-carol", &["member"])]);
    let carol = member("carol");
    let carol_id = user_id(&app, &carol).await;
    channel_with(&app, &[&carol]).await;
    let device = register_device(&app, &carol).await;

    let reply = app
      .post(
        &admin(),
        &format!("/admin/users/{carol_id}/deactivate"),
        json!({"note": "left the team"}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    let entry = deactivation_entry(&audit_of(&app, &carol_id).await);
    assert_eq!(entry["detail"]["source"], "manual");
    assert_eq!(entry["detail"]["note"], "left the team");
    let bob = user_id(&app, &admin()).await;
    assert_eq!(entry["actor_user_id"], bob);

    assert_eq!(app.get(&admin(), status_path()).await.body["pending_removals"], 1);
    let removed = app
      .post(
        &admin(),
        &format!("/admin/users/{carol_id}/remove-from-chats"),
        json!({}),
      )
      .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT, "{}", removed.body);
    assert_eq!(app.get(&admin(), status_path()).await.body["pending_removals"], 0);
    let devices = app
      .get(&admin(), &format!("/admin/users/{carol_id}/devices"))
      .await;
    assert_eq!(
      devices.body["devices"][0]["id"].as_str().map(str::to_owned),
      Some(device)
    );
    assert_eq!(devices.body["devices"][0]["is_revoked"], true);
    app.stop().await;
  }

  /// The commit is what counts: if the process dies right after it, the next round closes what
  /// the crash left open, and the account never looks active meanwhile.
  #[tokio::test]
  async fn a_crash_after_the_commit_is_finished_by_the_retry() {
    let (app, _provider) = rig!(vec![fake_user("sub-carol", &["member"])]);
    let carol = member("carol");
    let carol_id = user_id(&app, &carol).await;
    let device = register_device(&app, &carol).await;
    let mut socket = connect(&app, &carol, &device).await;

    // Only the transaction: what a process that died right after it would have done.
    let user: UserId = serde_json::from_value(json!(carol_id)).unwrap();
    let audit = DeactivationAudit {
      id: app.state.ids.next_snowflake().unwrap(),
      maybe_actor: None,
      detail: r#"{"reason":"admin","source":"manual","note":""}"#,
    };
    app
      .state
      .database
      .deactivate_user(user, "admin", &audit, &app.state.ids)
      .await
      .unwrap();

    assert_eq!(count_followups(&app).await, 1);
    assert_eq!(app.get(&carol, "/me").await.error_code(), "account_deactivated");
    assert_eq!(
      app.get(&admin(), status_path()).await.body["unfinished_deactivations"],
      1
    );

    assert_eq!(retry_deactivation_followups(&app.state).await.unwrap(), 1);

    assert!(
      is_closed(&mut socket).await,
      "the retry must close the connection"
    );
    assert_eq!(count_followups(&app).await, 0);
    // A second round finds nothing, and the audit entry is not written twice.
    assert_eq!(retry_deactivation_followups(&app.state).await.unwrap(), 0);
    let entries = audit_of(&app, &carol_id).await;
    assert_eq!(
      entries
        .iter()
        .filter(|e| e["action"] == "user_deactivated")
        .count(),
      1
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn a_failure_of_one_user_does_not_stop_the_others_and_is_tried_again() {
    let (app, provider) = rig!(provider_members(6));
    let ids = sign_in_members(&app, 6).await;
    let keep = ids[0].clone();
    // The database refuses to mark this one user.
    teamcraft_db::testing::sqlx::raw_sql(teamcraft_db::testing::sqlx::AssertSqlSafe(format!(
      r#"CREATE FUNCTION refuse_user() RETURNS trigger AS $$
         BEGIN
           IF NEW.id = {keep} AND NEW.deactivated_at IS NOT NULL THEN RAISE EXCEPTION 'refused'; END IF;
           RETURN NEW;
         END $$ LANGUAGE plpgsql;
         CREATE TRIGGER refuse_user BEFORE UPDATE ON users FOR EACH ROW EXECUTE FUNCTION refuse_user();"#
    )))
    .execute(app.sql_pool())
    .await
    .unwrap();
    disable(&provider, 0..2);

    let report = sync_once(&app.state).await.unwrap();
    assert_eq!(report.deactivated, 1, "the other one went through");
    assert_eq!(report.failed, 1);
    let status = app.get(&admin(), status_path()).await;
    assert_eq!(status.body["last_run"]["outcome"], "partial");
    assert_eq!(status.body["last_run"]["failed"], 1);

    // Nothing is half done for the one that failed: it is still fully active.
    assert_eq!(app.get(&member("m0"), "/me").await.status, StatusCode::OK);
    assert_eq!(
      app.get(&member("m1"), "/me").await.error_code(),
      "account_deactivated"
    );

    teamcraft_db::testing::sqlx::raw_sql("DROP TRIGGER refuse_user ON users")
      .execute(app.sql_pool())
      .await
      .unwrap();
    let report = sync_once(&app.state).await.unwrap();
    assert_eq!(report.deactivated, 1, "the next run does the rest");
    assert_eq!(report.failed, 0);
    assert_eq!(
      app.get(&member("m0"), "/me").await.error_code(),
      "account_deactivated"
    );
    let status = app.get(&admin(), status_path()).await;
    assert_eq!(status.body["last_run"]["outcome"], "ok");
    app.stop().await;
  }

  #[tokio::test]
  async fn a_provider_that_fails_midway_changes_nothing() {
    let (app, provider) = rig!(provider_members(6));
    sign_in_members(&app, 6).await;
    disable(&provider, 0..1);
    // m3's role lookup fails: the sync only gets that far after planning m0's deactivation.
    provider.break_roles_of("sub-m3");

    assert!(sync_once(&app.state).await.is_err());

    assert_eq!(app.get(&member("m0"), "/me").await.status, StatusCode::OK);
    let status = app.get(&admin(), status_path()).await;
    assert_eq!(status.body["last_run"]["outcome"], "failed", "{}", status.body);
    assert_eq!(status.body["last_run"]["error"], "provider_invalid_answer");
    assert_eq!(status.body["last_run"]["consecutive_failures"], 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_paused_bot_only_comes_back_with_a_new_token() {
    let (app, provider) = rig!(vec![fake_user("sub-carol", &["admin"])]);
    let carol = TestUser::new("carol", &["admin"]);
    user_id(&app, &carol).await;
    let bot = app.post(&carol, "/admin/bots", json!({"name": "Hooks"})).await;
    let bot_id = bot.body["bot"]["id"].as_str().unwrap().to_owned();
    let old_token = bot.body["token"].as_str().unwrap().to_owned();
    provider.update("sub-carol", |user| user.is_enabled = false);
    sync_once(&app.state).await.unwrap();

    // Lifting the pause by hand would leave the old token working for somebody who left.
    let reply = app
      .post(&admin(), &format!("/admin/users/{bot_id}/reactivate"), json!({}))
      .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.body);

    let rotated = app
      .post(&admin(), &format!("/admin/bots/{bot_id}/token"), json!({}))
      .await;
    assert_eq!(rotated.status, StatusCode::OK, "{}", rotated.body);
    let new_token = rotated.body["token"].as_str().unwrap().to_owned();
    let me = |token: String| {
      let url = format!("{}/api/v1/me", app.base_url);
      let http = app.http.clone();
      async move { http.get(url).bearer_auth(token).send().await.unwrap().status() }
    };
    assert_eq!(me(new_token).await, StatusCode::OK);
    assert_eq!(me(old_token).await, StatusCode::UNAUTHORIZED);
    app.stop().await;
  }
}

mod brake {
  use super::*;

  #[tokio::test]
  async fn it_fires_for_too_many_and_holds_all_of_them_visibly() {
    let (app, provider) = rig!(provider_members(20));
    sign_in_members(&app, 20).await;
    disable(&provider, 0..5);

    let report = sync_once(&app.state).await.unwrap();
    assert_eq!((report.deactivated, report.held), (0, 5), "{report:?}");
    assert_eq!(app.get(&member("m0"), "/me").await.status, StatusCode::OK);

    let status = app.get(&admin(), status_path()).await;
    assert_eq!(status.body["last_run"]["outcome"], "held");
    assert_eq!(status.body["held"].as_array().unwrap().len(), 5);
    assert_eq!(status.body["held"][0]["cause"], "disabled");
    let log = app
      .get(&admin(), "/admin/audit?action=oidc_sync_brake_engaged")
      .await;
    assert_eq!(log.body["entries"].as_array().unwrap().len(), 1);
    assert_eq!(app.state.oidc_sync.held(), 5, "the health check reads this");
    // A second run does not add to the log.
    sync_once(&app.state).await.unwrap();
    let log = app
      .get(&admin(), "/admin/audit?action=oidc_sync_brake_engaged")
      .await;
    assert_eq!(log.body["entries"].as_array().unwrap().len(), 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn it_does_not_fire_for_one_leaver() {
    let (app, provider) = rig!(provider_members(1));
    sign_in_members(&app, 1).await;
    disable(&provider, 0..1);
    let report = sync_once(&app.state).await.unwrap();
    assert_eq!((report.deactivated, report.held), (1, 0));
    app.stop().await;
  }

  #[tokio::test]
  async fn a_held_run_changes_no_roles_and_no_groups() {
    let (app, provider) = rig!(provider_members(20));
    sign_in_members(&app, 20).await;
    disable(&provider, 0..5);
    provider.update("sub-m6", |user| user.roles = vec!["admin".into()]);

    sync_once(&app.state).await.unwrap();

    let users = app.get(&admin(), "/admin/users?query=m6").await;
    assert_eq!(users.body["users"][0]["oidc_role"], "member", "{}", users.body);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_later_leaver_is_not_stopped_while_it_holds_others() {
    let (app, provider) = rig!(provider_members(20));
    sign_in_members(&app, 20).await;
    disable(&provider, 0..5);
    sync_once(&app.state).await.unwrap();

    disable(&provider, 19..20);
    let report = sync_once(&app.state).await.unwrap();
    assert_eq!((report.deactivated, report.held), (1, 5), "{report:?}");
    assert_eq!(
      app.get(&member("m19"), "/me").await.error_code(),
      "account_deactivated"
    );
    // And an admin can still deactivate anyone by hand.
    let id = user_id(&app, &member("m10")).await;
    assert_eq!(
      app
        .post(&admin(), &format!("/admin/users/{id}/deactivate"), json!({}))
        .await
        .status,
      StatusCode::NO_CONTENT
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn fixing_the_provider_ends_the_hold_by_itself() {
    let (app, provider) = rig!(provider_members(20));
    sign_in_members(&app, 20).await;
    disable(&provider, 0..5);
    sync_once(&app.state).await.unwrap();

    for index in 0..5 {
      provider.update(&format!("sub-m{index}"), |user| user.is_enabled = true);
    }
    let report = sync_once(&app.state).await.unwrap();
    assert_eq!((report.deactivated, report.held), (0, 0));
    assert_eq!(app.state.oidc_sync.held(), 0);
    let status = app.get(&admin(), status_path()).await;
    assert_eq!(status.body["held"].as_array().unwrap().len(), 0);
    assert_eq!(status.body["last_run"]["outcome"], "ok");
    app.stop().await;
  }

  #[tokio::test]
  async fn a_confirmation_applies_exactly_what_was_looked_at() {
    let (app, provider) = rig!(provider_members(20));
    sign_in_members(&app, 20).await;
    disable(&provider, 0..5);
    sync_once(&app.state).await.unwrap();
    let revision = app.get(&admin(), status_path()).await.body["held_revision"].clone();

    // Stale: the list moved on since the admin looked (too many new ones to pass on their own).
    disable(&provider, 5..9);
    sync_once(&app.state).await.unwrap();
    let stale = app
      .post(
        &admin(),
        "/admin/oidc-sync/brake/confirm",
        json!({"revision": revision}),
      )
      .await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "{}", stale.body);
    assert_eq!(app.get(&member("m0"), "/me").await.status, StatusCode::OK);

    let status = app.get(&admin(), status_path()).await;
    let revision = status.body["held_revision"].clone();
    let confirmed = app
      .post(
        &admin(),
        "/admin/oidc-sync/brake/confirm",
        json!({"revision": revision}),
      )
      .await;
    assert_eq!(confirmed.status, StatusCode::OK, "{}", confirmed.body);
    assert_eq!(confirmed.body["held"].as_array().unwrap().len(), 0);
    assert_eq!(
      app.get(&member("m0"), "/me").await.error_code(),
      "account_deactivated"
    );
    let log = app
      .get(&admin(), "/admin/audit?action=oidc_sync_brake_confirmed")
      .await;
    assert_eq!(detail_of(&log.body["entries"][0])["confirmed"], 9);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_confirmation_survives_a_provider_that_is_down() {
    let (app, provider) = rig!(provider_members(20));
    sign_in_members(&app, 20).await;
    disable(&provider, 0..5);
    sync_once(&app.state).await.unwrap();
    let status = app.get(&admin(), status_path()).await;

    provider.break_roles_of("sub-m9");
    let reply = app
      .post(
        &admin(),
        "/admin/oidc-sync/brake/confirm",
        json!({"revision": status.body["held_revision"]}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(app.get(&member("m0"), "/me").await.status, StatusCode::OK);
    assert_eq!(reply.body["held"][0]["is_confirmed"], true);

    provider.mend_roles();
    let report = sync_once(&app.state).await.unwrap();
    assert_eq!(
      (report.deactivated, report.held),
      (5, 0),
      "no second confirmation"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn admins_and_owners_among_the_held_need_an_owner() {
    let users = {
      let mut users = provider_members(20);
      users.push(fake_user("sub-carol", &["admin"]));
      users
    };
    let (app, provider) = rig!(users);
    sign_in_members(&app, 20).await;
    user_id(&app, &TestUser::new("carol", &["admin"])).await;
    disable(&provider, 0..5);
    provider.update("sub-carol", |user| user.is_enabled = false);
    sync_once(&app.state).await.unwrap();
    let status = app.get(&admin(), status_path()).await;
    let body = json!({"revision": status.body["held_revision"]});

    let refused = app
      .post(&admin(), "/admin/oidc-sync/brake/confirm", body.clone())
      .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    let confirmed = app.post(&owner(), "/admin/oidc-sync/brake/confirm", body).await;
    assert_eq!(confirmed.status, StatusCode::OK, "{}", confirmed.body);
    app.stop().await;
  }

  #[tokio::test]
  async fn members_cannot_see_or_confirm_it() {
    let (app, _provider) = rig!(vec![fake_user("sub-carol", &["member"])]);
    let carol = member("carol");
    assert_eq!(app.get(&carol, status_path()).await.status, StatusCode::FORBIDDEN);
    assert_eq!(
      app
        .post(&carol, "/admin/oidc-sync/brake/confirm", json!({"revision": 0}))
        .await
        .status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      app
        .post(&carol, "/admin/oidc-sync/preview", json!({}))
        .await
        .status,
      StatusCode::FORBIDDEN
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn a_provider_that_lists_nobody_changes_nothing() {
    let (app, provider) = rig!(provider_members(2));
    sign_in_members(&app, 2).await;
    provider.remove_all();

    assert!(sync_once(&app.state).await.is_err());

    assert_eq!(app.get(&member("m0"), "/me").await.status, StatusCode::OK);
    let status = app.get(&admin(), status_path()).await;
    assert_eq!(status.body["last_run"]["error"], "provider_listed_no_users");
    app.stop().await;
  }
}

mod dry_run {
  use super::*;

  #[tokio::test]
  async fn it_reports_and_changes_nothing() {
    let Some((app, provider)) = rig_with(provider_members(20), |settings| {
      settings.mode = OidcSyncRunMode::DryRun;
    })
    .await
    else {
      return;
    };
    sign_in_members(&app, 20).await;
    disable(&provider, 0..1);

    let report = sync_once(&app.state).await.unwrap();
    assert_eq!(
      report,
      SyncReport {
        deactivated: 1,
        ..SyncReport::default()
      }
    );
    assert_eq!(app.get(&member("m0"), "/me").await.status, StatusCode::OK);
    let status = app.get(&admin(), status_path()).await;
    assert_eq!(status.body["mode"], "dry_run");
    assert_eq!(status.body["last_run"]["outcome"], "dry_run");
    assert_eq!(status.body["held"].as_array().unwrap().len(), 0);
    let log = app.get(&admin(), "/admin/audit?action=user_deactivated").await;
    assert_eq!(log.body["entries"].as_array().unwrap().len(), 0);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_preview_says_what_a_run_would_do_and_changes_nothing() {
    let (app, provider) = rig!(provider_members(20));
    sign_in_members(&app, 20).await;
    disable(&provider, 0..5);

    let preview = app.post(&admin(), "/admin/oidc-sync/preview", json!({})).await;
    assert_eq!(preview.status, StatusCode::OK, "{}", preview.body);
    assert_eq!(preview.body["would_hold"].as_array().unwrap().len(), 5);
    assert_eq!(preview.body["would_deactivate"].as_array().unwrap().len(), 0);
    assert_eq!(preview.body["active_users"], 22);
    assert_eq!(preview.body["allowed"], 3);
    assert_eq!(
      preview.body["would_hold"][0]["display_name"]
        .as_str()
        .map(str::is_empty),
      Some(false)
    );

    assert_eq!(app.get(&member("m0"), "/me").await.status, StatusCode::OK);
    assert_eq!(app.state.oidc_sync.held(), 0);
    let status = app.get(&admin(), status_path()).await;
    assert!(status.body.get("last_run").is_none(), "a preview is no run");
    app.stop().await;
  }
}

mod health {
  use super::*;
  use teamcraft_server::services::oidc::load_brake_state;

  #[tokio::test]
  async fn the_status_reports_runs_and_says_when_it_is_stale() {
    let (app, provider) = rig!(provider_members(6));
    sign_in_members(&app, 6).await;
    let before = app.get(&admin(), status_path()).await;
    assert_eq!(before.body["state"], "active");
    assert!(before.body.get("last_run").is_none());
    assert_eq!(before.body["is_stale"], false, "no run yet is not late yet");

    disable(&provider, 0..1);
    sync_once(&app.state).await.unwrap();
    let status = app.get(&admin(), status_path()).await;
    let run = &status.body["last_run"];
    assert_eq!(run["outcome"], "ok");
    assert_eq!(run["deactivated"], 1);
    assert!(run["duration_ms"].as_i64().unwrap() >= 0);
    assert!(run["finished_at"].as_i64().unwrap() >= run["started_at"].as_i64().unwrap());
    assert!(status.body["last_success_at"].is_i64());
    assert_eq!(status.body["brake"]["max_percent"], 10);
    assert_eq!(status.body["brake"]["min_allowed"], 2);

    // Two hours without a success is more than three five-minute intervals.
    teamcraft_db::testing::sqlx::raw_sql(
      "UPDATE oidc_sync_status SET last_success_at = now() - interval '2 hours', last_finished_at = now() - interval '2 hours'",
    )
    .execute(app.sql_pool())
    .await
    .unwrap();
    let status = app.get(&admin(), status_path()).await;
    assert_eq!(status.body["is_stale"], true, "{}", status.body);
    app.stop().await;
  }

  #[tokio::test]
  async fn errors_appear_as_a_code_and_never_with_a_secret() {
    let (app, provider) = rig!(provider_members(1));
    sign_in_members(&app, 1).await;
    provider.refuse_token();

    assert!(sync_once(&app.state).await.is_err());

    let status = app.get(&admin(), status_path()).await;
    let text = status.body.to_string();
    assert_eq!(status.body["last_run"]["error"], "provider_refused", "{text}");
    assert!(!text.contains(CLIENT_SECRET), "{text}");
    assert!(!text.contains("fake-admin-token"), "{text}");
    app.stop().await;
  }

  #[tokio::test]
  async fn a_brake_that_holds_survives_a_restart_in_the_health_gauge() {
    let (app, provider) = rig!(provider_members(20));
    sign_in_members(&app, 20).await;
    disable(&provider, 0..5);
    sync_once(&app.state).await.unwrap();

    app.state.oidc_sync.held();
    load_brake_state(&app.state).await;

    assert_eq!(app.state.oidc_sync.held(), 5);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_server_without_the_sync_says_so() {
    let Some(app) = TestApp::start().await else {
      return;
    };
    let status = app.get(&admin(), status_path()).await;
    assert_eq!(status.body["state"], "not_configured", "{}", status.body);
    app.stop().await;
  }
}
