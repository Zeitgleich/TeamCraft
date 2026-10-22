//! Announcement banner, system events, orphaned channels and the permission scheme admin API.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::*;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_proto::{GatewayEvent, SystemEventKind};
use teamcraft_server::services::permission_scheme::load_scheme;
use teamcraft_server::testing::TestApp;

const QUIET: Duration = Duration::from_millis(400);

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

fn kinds(reply: &teamcraft_server::testing::Reply) -> Vec<String> {
  reply.body["events"]
    .as_array()
    .unwrap()
    .iter()
    .map(|event| event["kind"].as_str().unwrap().to_owned())
    .collect()
}

mod announcements {
  use super::*;

  #[tokio::test]
  async fn the_banner_reaches_everybody_and_can_be_dismissed_individually() {
    let app = app!();
    let (bob, carol, dave) = (admin(), member("carol"), member("dave"));
    let device = register_device(&app, &carol).await;
    let mut socket = ws::connect(&app, &carol, &device).await;
    ws::events(&mut socket, QUIET).await;
    assert!(
      app
        .get(&carol, "/announcement")
        .await
        .body
        .get("announcement")
        .is_none()
    );

    let denied = app
      .put(
        &carol,
        "/admin/announcement",
        json!({"text": "x", "level": "info", "is_dismissible": true}),
      )
      .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let set = app
      .put(
        &bob,
        "/admin/announcement",
        json!({"text": "Maintenance tonight", "level": "warning", "is_dismissible": true}),
      )
      .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.body);
    let events = ws::events(&mut socket, QUIET).await;
    assert!(
      events
        .iter()
        .any(|event| matches!(event, GatewayEvent::AnnouncementUpdate))
    );

    let seen = app.get(&carol, "/announcement").await;
    assert_eq!(seen.body["announcement"]["text"], "Maintenance tonight");
    assert_eq!(seen.body["announcement"]["is_dismissed"], false);
    let id = seen.body["announcement"]["id"].as_str().unwrap().to_owned();

    assert_eq!(
      app
        .post(&carol, &format!("/announcement/{id}/dismiss"), json!({}))
        .await
        .status,
      StatusCode::NO_CONTENT
    );
    assert_eq!(
      app.get(&carol, "/announcement").await.body["announcement"]["is_dismissed"],
      true
    );
    assert_eq!(
      app.get(&dave, "/announcement").await.body["announcement"]["is_dismissed"],
      false
    );

    // A new banner is not dismissed yet.
    app
      .put(
        &bob,
        "/admin/announcement",
        json!({"text": "Back", "level": "info", "is_dismissible": false}),
      )
      .await;
    let fresh = app.get(&carol, "/announcement").await;
    assert_eq!(fresh.body["announcement"]["text"], "Back");
    assert_eq!(fresh.body["announcement"]["is_dismissed"], false);
    let fresh_id = fresh.body["announcement"]["id"].as_str().unwrap().to_owned();
    assert_eq!(
      app
        .post(&carol, &format!("/announcement/{fresh_id}/dismiss"), json!({}))
        .await
        .status,
      StatusCode::FORBIDDEN,
      "not dismissible"
    );
    assert_eq!(
      app
        .post(&carol, &format!("/announcement/{id}/dismiss"), json!({}))
        .await
        .error_code(),
      "announcement_not_found",
      "the old banner is gone"
    );

    assert_eq!(
      app.delete(&bob, "/admin/announcement").await.status,
      StatusCode::NO_CONTENT
    );
    assert!(
      app
        .get(&carol, "/announcement")
        .await
        .body
        .get("announcement")
        .is_none()
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn invalid_banners_and_expired_ones() {
    let app = app!();
    let (bob, carol) = (admin(), member("carol"));
    for body in [
      json!({"text": "  ", "level": "info", "is_dismissible": true}),
      json!({"text": "x".repeat(1001), "level": "info", "is_dismissible": true}),
      json!({"text": "x", "level": "loud", "is_dismissible": true}),
      json!({"text": "x", "level": "info", "is_dismissible": true, "expires_at": 1000}),
    ] {
      assert_eq!(
        app.put(&bob, "/admin/announcement", body.clone()).await.status,
        StatusCode::BAD_REQUEST,
        "{body}"
      );
    }
    app
      .put(
        &bob,
        "/admin/announcement",
        json!({"text": "Short", "level": "info", "is_dismissible": true}),
      )
      .await;
    sqlx_expire(&app).await;
    assert!(
      app
        .get(&carol, "/announcement")
        .await
        .body
        .get("announcement")
        .is_none()
    );
    app.stop().await;
  }

  async fn sqlx_expire(app: &TestApp) {
    use teamcraft_db::testing::sqlx;
    sqlx::query("UPDATE announcements SET expires_at = now() - interval '1 minute'")
      .execute(app.sql_pool())
      .await
      .unwrap();
  }
}

mod system_events {
  use super::*;

  #[tokio::test]
  async fn what_happens_to_a_channel_is_written_down_and_sent_to_its_members() {
    let app = app!();
    let (alice, carol, dave) = (owner(), member("carol"), member("dave"));
    user_id(&app, &carol).await;
    let dave_id = user_id(&app, &dave).await;
    let channel = channel_with(&app, &[&carol]).await;
    let device = register_device(&app, &carol).await;
    let mut socket = ws::connect(&app, &carol, &device).await;
    ws::events(&mut socket, QUIET).await;
    let space = app.get(&alice, &format!("/conversations/{channel}")).await.body["space_id"]
      .as_str()
      .unwrap()
      .to_owned();
    add_space_member(&app, &alice, &space, &dave_id, "member").await;

    add_channel_member(&app, &alice, &channel, &dave_id).await;
    app
      .patch(
        &alice,
        &format!("/conversations/{channel}/members/{dave_id}"),
        json!({"role": "admin"}),
      )
      .await;
    app
      .patch(
        &alice,
        &format!("/conversations/{channel}"),
        json!({"name": "renamed"}),
      )
      .await;
    app
      .patch(
        &alice,
        &format!("/conversations/{channel}"),
        json!({"moderation": {"member_denied": ["add_reaction"]}}),
      )
      .await;
    app
      .patch(
        &alice,
        &format!("/conversations/{channel}"),
        json!({"is_archived": true}),
      )
      .await;
    app
      .patch(
        &alice,
        &format!("/conversations/{channel}"),
        json!({"is_archived": false}),
      )
      .await;
    app
      .delete(&alice, &format!("/conversations/{channel}/members/{dave_id}"))
      .await;

    let listed = app
      .get(&carol, &format!("/conversations/{channel}/system-events"))
      .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    // Carol was added by the helper too, before she could look.
    assert_eq!(
      kinds(&listed)[kinds(&listed).len() - 7..],
      [
        "member_added",
        "role_changed",
        "renamed",
        "moderation_changed",
        "archived",
        "unarchived",
        "member_removed"
      ]
    );
    let removed = listed.body["events"].as_array().unwrap().last().unwrap();
    assert_eq!(removed["subject_id"], dave_id);
    assert!(removed["actor_id"].is_string());
    assert!(removed["created_at"].is_i64());

    let events = ws::events(&mut socket, QUIET).await;
    let live: Vec<_> = events
      .iter()
      .filter_map(|event| match event {
        GatewayEvent::SystemEvent { event } => Some(event.kind.clone()),
        _ => None,
      })
      .collect();
    assert_eq!(live.len(), 7, "{live:?}");
    assert_eq!(live[0], SystemEventKind::MemberAdded);

    // Nothing for non-members; paging works.
    assert_eq!(
      app
        .get(&dave, &format!("/conversations/{channel}/system-events"))
        .await
        .status,
      StatusCode::NOT_FOUND
    );
    let page = app
      .get(&carol, &format!("/conversations/{channel}/system-events?limit=2"))
      .await;
    assert_eq!(page.body["events"].as_array().unwrap().len(), 2);
    assert_eq!(page.body["has_more"], true);
    let after = page.body["events"][1]["id"].as_str().unwrap();
    let rest = app
      .get(
        &carol,
        &format!("/conversations/{channel}/system-events?after={after}&limit=50"),
      )
      .await;
    assert_eq!(rest.body["has_more"], false);
    app.stop().await;
  }

  #[tokio::test]
  async fn leaving_is_not_removal() {
    let app = app!();
    let carol = member("carol");
    let alice = owner();
    let carol_id = user_id(&app, &carol).await;
    let channel = channel_with(&app, &[&carol]).await;
    app
      .delete(&carol, &format!("/conversations/{channel}/members/{carol_id}"))
      .await;
    let listed = app
      .get(&alice, &format!("/conversations/{channel}/system-events"))
      .await;
    assert_eq!(kinds(&listed).last().unwrap(), "member_left");
    app.stop().await;
  }
}

mod orphaned_channels {
  use super::*;
  use teamcraft_server::testing::TestUser;

  #[tokio::test]
  async fn channels_without_an_active_admin_get_a_new_owner() {
    let app = app!();
    let (alice, bob, carol, dave) = (owner(), admin(), member("carol"), member("dave"));
    let (alice_id, carol_id, dave_id) = (
      user_id(&app, &alice).await,
      user_id(&app, &carol).await,
      user_id(&app, &dave).await,
    );
    let channel = channel_with(&app, &[&carol]).await;
    let listed = app.get(&bob, "/admin/conversations?orphaned=true").await;
    assert_eq!(
      listed.body["conversations"].as_array().unwrap().len(),
      0,
      "alice is the admin"
    );

    // Alice leaves the company. Only owners may deactivate an owner, so a second owner does it.
    let olivia = TestUser::new("olivia", &["owner"]);
    let deactivated = app
      .post(&olivia, &format!("/admin/users/{alice_id}/deactivate"), json!({}))
      .await;
    assert_eq!(deactivated.status, StatusCode::NO_CONTENT, "{}", deactivated.body);
    let listed = app.get(&bob, "/admin/conversations?orphaned=true").await;
    let found: Vec<&str> = listed.body["conversations"]
      .as_array()
      .unwrap()
      .iter()
      .map(|c| c["id"].as_str().unwrap())
      .collect();
    assert_eq!(found, vec![channel.as_str()]);
    assert_eq!(
      app.get(&bob, "/admin/conversations?orphaned=false").await.body["conversations"]
        .as_array()
        .unwrap()
        .len(),
      0
    );

    let path = format!("/admin/conversations/{channel}/owner");
    assert_eq!(
      app.post(&carol, &path, json!({"user_id": carol_id})).await.status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      app.post(&bob, &path, json!({"user_id": dave_id})).await.status,
      StatusCode::CONFLICT,
      "dave is no member"
    );
    assert_eq!(
      app.post(&bob, &path, json!({"user_id": alice_id})).await.status,
      StatusCode::CONFLICT,
      "alice is deactivated"
    );
    let assigned = app.post(&bob, &path, json!({"user_id": carol_id})).await;
    assert_eq!(assigned.status, StatusCode::NO_CONTENT, "{}", assigned.body);
    assert_eq!(
      app.get(&bob, "/admin/conversations?orphaned=true").await.body["conversations"]
        .as_array()
        .unwrap()
        .len(),
      0
    );
    let members = app
      .get(&carol, &format!("/conversations/{channel}/members"))
      .await;
    let carol_row = members.body["members"]
      .as_array()
      .unwrap()
      .iter()
      .find(|m| m["user"]["id"] == carol_id)
      .unwrap();
    assert_eq!(carol_row["role"], "admin");
    let events = app
      .get(&carol, &format!("/conversations/{channel}/system-events"))
      .await;
    assert_eq!(kinds(&events).last().unwrap(), "owner_assigned");
    let audit = app.get(&bob, "/admin/audit").await;
    assert!(audit.body.to_string().contains("channel_owner_assigned"));
    app.stop().await;
  }
}

mod permission_scheme {
  use super::*;

  fn granted(reply: &teamcraft_server::testing::Reply, role: &str) -> Vec<String> {
    reply.body["roles"]
      .as_array()
      .unwrap()
      .iter()
      .find(|r| r["role"] == role)
      .unwrap()["granted"]
      .as_array()
      .unwrap()
      .iter()
      .map(|p| p.as_str().unwrap().to_owned())
      .collect()
  }

  #[tokio::test]
  async fn instance_changes_take_effect_at_once_and_can_be_undone() {
    let app = app!();
    let (alice, bob, carol) = (owner(), admin(), member("carol"));
    let carol_id = user_id(&app, &carol).await;
    let space = create_space(&app, &alice, "Team", "open").await;
    add_space_member(&app, &alice, &space, &carol_id, "member").await;
    let path = format!("/spaces/{space}/channels");
    assert!(
      granted(&app.get(&bob, "/admin/permission-scheme").await, "member")
        .contains(&"create_channel".to_owned())
    );

    let reply = app
      .put(
        &bob,
        "/admin/permission-scheme",
        json!({"changes": [
          {"role": "member", "permission": "create_channel", "effect": "revoke"},
          {"role": "member", "permission": "create_user_group", "effect": "grant"},
        ]}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(!granted(&reply, "member").contains(&"create_channel".to_owned()));
    assert_eq!(reply.body["roles"][1]["overrides"].as_array().unwrap().len(), 2);
    assert_eq!(
      app
        .post(&carol, &path, json!({"name": "ideas", "visibility": "public"}))
        .await
        .status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      app
        .post(
          &carol,
          "/user-groups",
          json!({"handle": "carols", "name": "Carols"})
        )
        .await
        .status,
      StatusCode::CREATED
    );

    // Persisted: reloading from the database gives the same schemes.
    load_scheme(&app.state).await.unwrap();
    assert_eq!(
      app
        .post(&carol, &path, json!({"name": "ideas", "visibility": "public"}))
        .await
        .status,
      StatusCode::FORBIDDEN
    );

    let undone = app
      .put(
        &bob,
        "/admin/permission-scheme",
        json!({"changes": [{"role": "member", "permission": "create_channel", "effect": "inherit"}]}),
      )
      .await;
    assert!(granted(&undone, "member").contains(&"create_channel".to_owned()));
    assert_eq!(
      app
        .post(&carol, &path, json!({"name": "ideas", "visibility": "public"}))
        .await
        .status,
      StatusCode::CREATED
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn a_space_can_differ_from_the_instance() {
    let app = app!();
    let (alice, bob, carol) = (owner(), admin(), member("carol"));
    let carol_id = user_id(&app, &carol).await;
    let (a, b) = (
      create_space(&app, &alice, "A", "open").await,
      create_space(&app, &alice, "B", "open").await,
    );
    for space in [&a, &b] {
      add_space_member(&app, &alice, space, &carol_id, "member").await;
    }
    let reply = app
      .put(
        &bob,
        &format!("/admin/spaces/{a}/permission-scheme"),
        json!({"changes": [{"role": "member", "permission": "create_channel", "effect": "revoke"}]}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let body = json!({"name": "ideas", "visibility": "public"});
    assert_eq!(
      app
        .post(&carol, &format!("/spaces/{a}/channels"), body.clone())
        .await
        .status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      app
        .post(&carol, &format!("/spaces/{b}/channels"), body.clone())
        .await
        .status,
      StatusCode::CREATED
    );
    // The space inherits instance changes it does not override itself.
    app
      .put(
        &bob,
        "/admin/permission-scheme",
        json!({"changes": [{"role": "member", "permission": "create_emoji", "effect": "revoke"}]}),
      )
      .await;
    let space_scheme = app
      .get(&bob, &format!("/admin/spaces/{a}/permission-scheme"))
      .await;
    assert!(!granted(&space_scheme, "member").contains(&"create_emoji".to_owned()));
    assert_eq!(
      space_scheme.body["roles"][1]["overrides"]
        .as_array()
        .unwrap()
        .len(),
      1,
      "only the space's own change is listed"
    );
    // Space admins keep their own rights.
    assert!(granted(&space_scheme, "space_admin").contains(&"invite_guest".to_owned()));
    app.stop().await;
  }

  #[tokio::test]
  async fn dangerous_or_meaningless_changes_are_refused_and_only_admins_may_change() {
    let app = app!();
    let (alice, bob, carol) = (owner(), admin(), member("carol"));
    let space = create_space(&app, &alice, "Team", "open").await;
    let change = |role: &str, permission: &str, effect: &str| json!({"changes": [{"role": role, "permission": permission, "effect": effect}]});
    let instance = "/admin/permission-scheme".to_owned();
    let in_space = format!("/admin/spaces/{space}/permission-scheme");
    for (path, body) in [
      (&instance, change("owner", "create_post", "revoke")),
      (&instance, change("admin", "sign_workspace", "grant")),
      (&instance, change("guest", "create_channel", "grant")),
      (&instance, change("member", "teleport", "grant")),
      (&instance, change("wizard", "create_post", "grant")),
      (&instance, change("member", "create_post", "maybe")),
      (&in_space, change("member", "create_user_group", "grant")),
      (&in_space, change("admin", "create_post", "revoke")),
    ] {
      let reply = app.put(&bob, path, body.clone()).await;
      assert_eq!(
        reply.status,
        StatusCode::BAD_REQUEST,
        "{path} {body}: {}",
        reply.body
      );
    }
    // A refused batch changes nothing.
    let mixed = json!({"changes": [
      {"role": "member", "permission": "create_emoji", "effect": "revoke"},
      {"role": "owner", "permission": "create_post", "effect": "revoke"},
    ]});
    assert_eq!(
      app.put(&bob, &instance, mixed).await.status,
      StatusCode::BAD_REQUEST
    );
    assert!(granted(&app.get(&bob, &instance).await, "member").contains(&"create_emoji".to_owned()));

    for path in [&instance, &in_space] {
      assert_eq!(app.get(&carol, path).await.status, StatusCode::FORBIDDEN);
      assert_eq!(
        app
          .put(&carol, path, change("member", "create_emoji", "revoke"))
          .await
          .status,
        StatusCode::FORBIDDEN
      );
    }
    let audit = app.get(&bob, "/admin/audit").await;
    assert!(
      !audit.body.to_string().contains("permission_scheme_changed"),
      "refused changes are not audited"
    );
    app.stop().await;
  }
}

mod moderation {
  use super::*;

  #[tokio::test]
  async fn admins_toggle_what_members_and_guests_may_do_without_joining() {
    let app = app!();
    let (alice, bob, carol, ext) = (owner(), admin(), member("carol"), guest("ext"));
    let (carol_id, ext_id) = (user_id(&app, &carol).await, user_id(&app, &ext).await);
    let carol_device = register_device(&app, &carol).await;
    let ext_device = register_device(&app, &ext).await;
    let space = create_space(&app, &alice, "Team", "invite_only").await;
    add_space_member(&app, &alice, &space, &carol_id, "member").await;
    add_space_member(&app, &alice, &space, &ext_id, "guest").await;
    let channel = create_channel(&app, &alice, &space, "news", "public").await;
    add_channel_member(&app, &alice, &channel, &carol_id).await;
    add_channel_member(&app, &alice, &channel, &ext_id).await;
    let path = format!("/admin/conversations/{channel}/moderation");

    assert_eq!(app.get(&bob, &path).await.body["member_denied"], json!([]));
    assert_eq!(
      app
        .put(
          &carol,
          &path,
          json!({"moderation": {"member_denied": ["create_post"]}})
        )
        .await
        .status,
      StatusCode::FORBIDDEN
    );
    for bad in [json!(["manage_webhooks"]), json!(["teleport"])] {
      let reply = app
        .put(&bob, &path, json!({"moderation": {"member_denied": bad}}))
        .await;
      assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let set = app
      .put(
        &bob,
        &path,
        json!({"moderation": {"member_denied": ["create_post"], "guest_denied": ["add_reaction"]}}),
      )
      .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.body);

    let post = |user, device: String, n| {
      let app = &app;
      let channel = channel.clone();
      async move {
        app
          .post(
            user,
            &format!("/conversations/{channel}/messages"),
            message_body(n, &device),
          )
          .await
          .status
      }
    };
    assert_eq!(
      post(&carol, carol_device.clone(), 1).await,
      StatusCode::FORBIDDEN,
      "members may not post"
    );
    assert_eq!(
      post(&ext, ext_device.clone(), 2).await,
      StatusCode::CREATED,
      "guests still may"
    );
    let events = app
      .get(&carol, &format!("/conversations/{channel}/system-events"))
      .await;
    assert!(kinds(&events).contains(&"moderation_changed".to_owned()));

    let cleared = app.put(&bob, &path, json!({"moderation": {}})).await;
    assert_eq!(cleared.status, StatusCode::OK);
    assert_eq!(post(&carol, carol_device, 3).await, StatusCode::CREATED);
    app.stop().await;
  }
}
