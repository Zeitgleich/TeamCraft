//! OIDC deprovisioning: back-channel logout and the sync with the provider's admin API.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::time::Duration;

use common::fake_keycloak::{CLIENT_SECRET, FakeKeycloak, fake_user};
use common::*;
use futures_util::{SinkExt, StreamExt};
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_proto::{DeviceActivity, GatewayClientMessage};
use teamcraft_server::services::oidc::{SyncReport, sync_once};
use teamcraft_server::testing::{Reply, TestApp, TestUser};
use tokio_tungstenite::tungstenite::Message;

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

async fn post_logout(app: &TestApp, token: &str) -> reqwest::Response {
  app
    .http
    .post(format!("{}/api/v1/auth/backchannel-logout", app.base_url))
    .form(&[("logout_token", token)])
    .send()
    .await
    .unwrap()
}

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(app: &TestApp, user: &TestUser, device: &str) -> Socket {
  let (mut socket, _) = tokio_tungstenite::connect_async(&app.ws_url).await.unwrap();
  socket.next().await;
  let identify = GatewayClientMessage::Identify {
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

fn audit_actions(reply: &Reply) -> Vec<String> {
  reply.body["entries"]
    .as_array()
    .unwrap()
    .iter()
    .map(|e| e["action"].as_str().unwrap().to_owned())
    .collect()
}

mod backchannel_logout {
  use super::*;

  #[tokio::test]
  async fn a_logout_by_subject_ends_gateway_sessions_and_older_tokens() {
    let app = app!();
    let carol = member("carol");
    let device = register_device(&app, &carol).await;
    let mut socket = connect(&app, &carol, &device).await;
    let old_token = app.token(&carol);
    let old_request = |app: &TestApp| {
      app
        .http
        .get(format!("{}/api/v1/me", app.base_url))
        .bearer_auth(&old_token)
        .send()
    };
    assert_eq!(old_request(&app).await.unwrap().status(), StatusCode::OK);

    let response = post_logout(&app, &app.logout_token(Some("sub-carol"), None)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");

    assert!(is_closed(&mut socket).await, "the gateway connection stayed open");
    assert_eq!(
      old_request(&app).await.unwrap().status(),
      StatusCode::UNAUTHORIZED
    );
    // Logging in again afterwards works: the new token is issued after the logout.
    let later = app.token_issued_at(
      &carol,
      i64::try_from(jsonwebtoken::get_current_timestamp()).unwrap() + 5,
    );
    let reply = app
      .http
      .get(format!("{}/api/v1/me", app.base_url))
      .bearer_auth(later)
      .send()
      .await
      .unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_logout_by_provider_session_only_ends_that_session() {
    let app = app!();
    let (laptop, phone) = (
      member("carol").with_session("sid-laptop"),
      member("carol").with_session("sid-phone"),
    );
    let device = register_device(&app, &laptop).await;
    let phone_device = register_device(&app, &phone).await;
    let mut laptop_socket = connect(&app, &laptop, &device).await;
    let mut phone_socket = connect(&app, &phone, &phone_device).await;

    assert_eq!(
      post_logout(&app, &app.logout_token(None, Some("sid-laptop")))
        .await
        .status(),
      StatusCode::OK
    );
    assert!(is_closed(&mut laptop_socket).await);
    assert!(!is_closed(&mut phone_socket).await, "the other session must stay");
    assert_eq!(app.get(&laptop, "/me").await.error_code(), "invalid_token");
    assert_eq!(app.get(&phone, "/me").await.status, StatusCode::OK);
    app.stop().await;
  }

  /// Keycloak names the subject and the session in every logout token. Signing out on one device
  /// (or signing in again there, which signs out first) must leave the user's other devices alone.
  #[tokio::test]
  async fn two_devices_stay_signed_in_when_one_signs_out() {
    let app = app!();
    let (desktop, phone) = (
      member("carol").with_session("sid-desktop"),
      member("carol").with_session("sid-phone"),
    );
    let desktop_device = register_device(&app, &desktop).await;
    let phone_device = register_device(&app, &phone).await;
    let mut desktop_socket = connect(&app, &desktop, &desktop_device).await;
    let mut phone_socket = connect(&app, &phone, &phone_device).await;

    let token = app.logout_token(Some("sub-carol"), Some("sid-phone"));
    assert_eq!(post_logout(&app, &token).await.status(), StatusCode::OK);

    assert!(is_closed(&mut phone_socket).await);
    assert_eq!(app.get(&phone, "/me").await.error_code(), "invalid_token");
    assert!(
      !is_closed(&mut desktop_socket).await,
      "the desktop's gateway connection must stay"
    );
    assert_eq!(app.get(&desktop, "/me").await.status, StatusCode::OK);

    // The phone signs in again: a new provider session, and both devices work.
    let phone_again = member("carol").with_session("sid-phone-2");
    assert_eq!(app.get(&phone_again, "/me").await.status, StatusCode::OK);
    assert_eq!(app.get(&desktop, "/me").await.status, StatusCode::OK);
    app.stop().await;
  }

  #[tokio::test]
  async fn invalid_logout_tokens_are_rejected_and_change_nothing() {
    let app = app!();
    let carol = member("carol");
    user_id(&app, &carol).await;

    for token in ["garbage".to_owned(), app.token(&carol)] {
      let response = post_logout(&app, &token).await;
      assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{token}");
      let body: Value = response.json().await.unwrap();
      assert_eq!(body["error_code"], "invalid_token");
    }
    let empty = app
      .http
      .post(format!("{}/api/v1/auth/backchannel-logout", app.base_url))
      .send()
      .await
      .unwrap();
    assert!(empty.status().is_client_error());
    assert_eq!(app.get(&carol, "/me").await.status, StatusCode::OK);
    app.stop().await;
  }

  #[tokio::test]
  async fn logouts_are_audited() {
    let app = app!();
    let carol = member("carol");
    user_id(&app, &carol).await;
    post_logout(&app, &app.logout_token(Some("sub-carol"), None)).await;
    let log = app
      .get(&admin(), "/admin/audit?action=oidc_backchannel_logout")
      .await;
    assert_eq!(audit_actions(&log), vec!["oidc_backchannel_logout"]);
    assert_eq!(log.body["entries"][0]["target_kind"], "user");
    app.stop().await;
  }
}

mod sync {
  use super::*;

  /// The provider knows alice (owner) and bob (admin) in every test, like the real realm would.
  fn with_staff(mut users: Vec<common::fake_keycloak::FakeUser>) -> Vec<common::fake_keycloak::FakeUser> {
    users.push(fake_user("sub-alice", &["owner"]));
    users.push(fake_user("sub-bob", &["admin"]));
    users
  }

  async fn app_with_provider(
    users: Vec<common::fake_keycloak::FakeUser>,
    page_size: u32,
  ) -> Option<(TestApp, FakeKeycloak)> {
    let provider = FakeKeycloak::start(with_staff(users)).await;
    let settings = provider.sync_settings(CLIENT_SECRET, page_size);
    let app = TestApp::start_with(|s| s.maybe_oidc_sync = Some(settings)).await?;
    Some((app, provider))
  }

  #[tokio::test]
  async fn a_user_disabled_at_the_provider_is_deactivated_and_comes_back_when_enabled() {
    let Some((app, provider)) = app_with_provider(
      vec![
        fake_user("sub-carol", &["member"]),
        fake_user("sub-dave", &["member"]),
      ],
      100,
    )
    .await
    else {
      return;
    };
    let (carol, dave) = (member("carol"), member("dave"));
    let channel = channel_with(&app, &[&carol, &dave]).await;
    let carol_id = user_id(&app, &carol).await;

    assert_eq!(
      sync_once(&app.state).await.unwrap(),
      SyncReport::default(),
      "nothing differs yet"
    );
    provider.update("sub-carol", |user| user.is_enabled = false);
    let report = sync_once(&app.state).await.unwrap();
    assert_eq!(report.deactivated, 1);

    assert_eq!(app.get(&carol, "/me").await.status, StatusCode::FORBIDDEN);
    // Suspended, not removed: she stays in the channel for when the provider enables her again.
    assert_eq!(
      app
        .get(&dave, &format!("/conversations/{channel}/members"))
        .await
        .body["members"]
        .as_array()
        .unwrap()
        .len(),
      3
    );
    let users = app.get(&admin(), "/admin/users?query=carol").await;
    assert_eq!(users.body["users"][0]["deactivation_reason"], "oidc");
    // A second run does not repeat the work.
    assert_eq!(sync_once(&app.state).await.unwrap(), SyncReport::default());

    provider.update("sub-carol", |user| user.is_enabled = true);
    assert_eq!(sync_once(&app.state).await.unwrap().reactivated, 1);
    assert_eq!(app.get(&carol, "/me").await.status, StatusCode::OK);
    assert_eq!(
      app.get(&carol, &format!("/conversations/{channel}")).await.status,
      StatusCode::OK
    );
    let log = app
      .get(
        &admin(),
        &format!("/admin/audit?target_kind=user&target_id={carol_id}"),
      )
      .await;
    assert!(audit_actions(&log).contains(&"user_deactivated".to_owned()));
    assert!(audit_actions(&log).contains(&"user_reactivated".to_owned()));
    app.stop().await;
  }

  #[tokio::test]
  async fn admin_deactivations_are_not_undone_by_the_provider() {
    let Some((app, _provider)) = app_with_provider(vec![fake_user("sub-carol", &["member"])], 100).await
    else {
      return;
    };
    let carol = member("carol");
    let carol_id = user_id(&app, &carol).await;
    app
      .post(
        &admin(),
        &format!("/admin/users/{carol_id}/deactivate"),
        json!({}),
      )
      .await;

    assert_eq!(sync_once(&app.state).await.unwrap(), SyncReport::default());
    assert_eq!(app.get(&carol, "/me").await.status, StatusCode::FORBIDDEN);
    app.stop().await;
  }

  #[tokio::test]
  async fn users_deleted_or_stripped_of_all_roles_are_deactivated() {
    let Some((app, provider)) = app_with_provider(
      vec![
        fake_user("sub-carol", &["member"]),
        fake_user("sub-dave", &["member"]),
        fake_user("sub-erin", &["member"]),
      ],
      100,
    )
    .await
    else {
      return;
    };
    let (carol, dave, erin) = (member("carol"), member("dave"), member("erin"));
    for user in [&carol, &dave, &erin] {
      user_id(&app, user).await;
    }

    provider.remove("sub-carol");
    provider.update("sub-dave", |user| user.roles = vec!["offline_access".into()]);
    let report = sync_once(&app.state).await.unwrap();
    assert_eq!(report.deactivated, 2);
    assert_eq!(app.get(&carol, "/me").await.status, StatusCode::FORBIDDEN);
    assert_eq!(app.get(&dave, "/me").await.status, StatusCode::FORBIDDEN);
    assert_eq!(app.get(&erin, "/me").await.status, StatusCode::OK);
    app.stop().await;
  }

  #[tokio::test]
  async fn role_changes_are_recorded_and_change_the_kind() {
    let Some((app, provider)) = app_with_provider(vec![fake_user("sub-carol", &["member"])], 100).await
    else {
      return;
    };
    let carol = member("carol");
    user_id(&app, &carol).await;

    provider.update("sub-carol", |user| {
      user.roles = vec!["member".into(), "guest".into(), "admin".into()]
    });
    assert_eq!(sync_once(&app.state).await.unwrap().role_changes, 1);
    let users = app.get(&admin(), "/admin/users?query=carol").await;
    assert_eq!(users.body["users"][0]["oidc_role"], "admin");
    assert_eq!(users.body["users"][0]["kind"], "member");

    provider.update("sub-carol", |user| user.roles = vec!["guest".into()]);
    sync_once(&app.state).await.unwrap();
    let users = app.get(&admin(), "/admin/users?query=carol").await;
    assert_eq!(
      (
        users.body["users"][0]["oidc_role"].clone(),
        users.body["users"][0]["kind"].clone()
      ),
      (json!("guest"), json!("guest"))
    );
    let log = app.get(&admin(), "/admin/audit?action=oidc_role_changed").await;
    assert_eq!(log.body["entries"].as_array().unwrap().len(), 2);
    assert_eq!(sync_once(&app.state).await.unwrap(), SyncReport::default());
    app.stop().await;
  }

  #[tokio::test]
  async fn all_pages_of_the_provider_are_read() {
    let users: Vec<_> = (0..5)
      .map(|n| fake_user(&format!("sub-user{n}"), &["member"]))
      .collect();
    let Some((app, provider)) = app_with_provider(users, 2).await else {
      return;
    };
    let members: Vec<TestUser> = (0..5).map(|n| member(&format!("user{n}"))).collect();
    for user in &members {
      user_id(&app, user).await;
    }
    provider.update("sub-user4", |user| user.is_enabled = false);
    assert_eq!(
      sync_once(&app.state).await.unwrap().deactivated,
      1,
      "the user on the third page was found"
    );
    assert_eq!(app.get(&members[4], "/me").await.status, StatusCode::FORBIDDEN);
    app.stop().await;
  }

  #[tokio::test]
  async fn an_unreachable_or_unauthorized_provider_changes_nothing() {
    let provider = FakeKeycloak::start(vec![fake_user("sub-carol", &[])]).await;
    let settings = provider.sync_settings("wrong-secret", 100);
    let Some(app) = TestApp::start_with(|s| s.maybe_oidc_sync = Some(settings)).await else {
      return;
    };
    let carol = member("carol");
    user_id(&app, &carol).await;

    let failure = sync_once(&app.state).await.unwrap_err();
    assert_eq!(failure.error_code().as_str(), "unavailable");
    assert_eq!(
      app.get(&carol, "/me").await.status,
      StatusCode::OK,
      "a failing sync must not lock anyone out"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn without_configuration_the_sync_does_nothing() {
    let app = app!();
    assert_eq!(sync_once(&app.state).await.unwrap(), SyncReport::default());
    app.stop().await;
  }
}
