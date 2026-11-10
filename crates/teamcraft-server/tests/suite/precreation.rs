//! Accounts listed and addable before their first sign-in (issue #1632, docs/provisioning.md §6):
//! the identity provider sync makes them, the directory marks them, channels take them, and a
//! provider that drops them before a sign-in has them removed outright.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::fake_keycloak::{CLIENT_SECRET, FakeKeycloak, FakeUser, fake_group, fake_user};
use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_server::config::OidcSyncRunMode;
use teamcraft_server::services::oidc::{AccountPrecreation, sync_once};
use teamcraft_server::state::OidcSyncSettings;
use teamcraft_server::testing::{TestApp, TestUser};

const LIMIT: u32 = 100;

/// A fake provider that knows the owner and the admin of every test; pre-creation on.
async fn rig_with(
  mut users: Vec<FakeUser>,
  edit: impl FnOnce(&mut OidcSyncSettings),
) -> Option<(TestApp, FakeKeycloak)> {
  users.push(fake_user("sub-alice", &["owner"]));
  users.push(fake_user("sub-bob", &["admin"]));
  let provider = FakeKeycloak::start(users).await;
  let mut settings = provider.sync_settings(CLIENT_SECRET, 100);
  settings.precreation = AccountPrecreation::UpTo(LIMIT);
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

mod creation {
  use super::*;

  #[tokio::test]
  async fn members_are_made_before_their_first_sign_in_and_listed_as_not_signed_in() {
    let (app, _provider) = rig!(vec![
      fake_user("sub-carol", &["member"]),
      fake_user("sub-gwen", &["guest"]),
      fake_user("sub-nora", &[]),
      disabled(fake_user("sub-dora", &["member"])),
    ]);
    signed_in_staff(&app).await;

    let report = sync_once(&app.state).await.unwrap();
    assert_eq!(
      report.created, 1,
      "carol only: no guest, no role-less, no disabled user"
    );
    let carol = find(&app, &admin(), "sub-carol").await;
    assert_eq!(carol["is_awaiting_first_sign_in"], true);
    assert_eq!(carol["has_devices"], false);
    assert!(find_maybe(&app, &admin(), "sub-gwen").await.is_none());
    let carol_id = carol["id"].as_str().unwrap().to_owned();
    let audit = audit_actions(&app, &carol_id).await;
    assert!(audit.contains(&"user_provisioned".to_owned()), "{audit:?}");
    // A second run finds her there.
    assert_eq!(sync_once(&app.state).await.unwrap().created, 0);

    // Her first sign-in takes the account made for her and ends the mark.
    assert_eq!(user_id(&app, &member("carol")).await, carol_id);
    // The token's user name replaces the provider's.
    let carol = find(&app, &admin(), "carol").await;
    assert_eq!(carol["id"], carol_id.as_str());
    assert!(carol.get("is_awaiting_first_sign_in").is_none(), "{carol}");
    app.stop().await;
  }

  #[tokio::test]
  async fn a_run_makes_at_most_the_limit_and_the_next_one_goes_on() {
    let mut users: Vec<FakeUser> = (0..5)
      .map(|index| fake_user(&format!("sub-m{index}"), &["member"]))
      .collect();
    // Looked up and skipped every round: never keeps the others waiting.
    users.push(fake_user("sub-a-no-role", &[]));
    let Some((app, _provider)) = rig_with(users, |settings| {
      settings.precreation = AccountPrecreation::UpTo(2);
    })
    .await
    else {
      return;
    };
    signed_in_staff(&app).await;

    let mut made = Vec::new();
    for _ in 0..5 {
      made.push(sync_once(&app.state).await.unwrap().created);
    }
    assert_eq!(made.iter().sum::<u32>(), 5, "{made:?}");
    assert!(made.iter().all(|count| *count <= 2), "{made:?}");
    assert_eq!(made.last(), Some(&0));
    app.stop().await;
  }

  #[tokio::test]
  async fn a_dry_run_says_how_many_it_would_make_and_makes_none() {
    let Some((app, _provider)) = rig_with(vec![fake_user("sub-carol", &["member"])], |settings| {
      settings.mode = OidcSyncRunMode::DryRun;
    })
    .await
    else {
      return;
    };
    signed_in_staff(&app).await;
    assert_eq!(sync_once(&app.state).await.unwrap().created, 1);
    assert!(find_maybe(&app, &admin(), "sub-carol").await.is_none());
    let preview = app.post(&admin(), "/admin/oidc-sync/preview", json!({})).await;
    assert_eq!(preview.status, StatusCode::OK, "{}", preview.body);
    assert_eq!(preview.body["would_create"], 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn switched_off_it_makes_nobody() {
    let Some((app, _provider)) = rig_with(vec![fake_user("sub-carol", &["member"])], |settings| {
      settings.precreation = AccountPrecreation::Off;
    })
    .await
    else {
      return;
    };
    signed_in_staff(&app).await;
    assert_eq!(sync_once(&app.state).await.unwrap().created, 0);
    assert!(find_maybe(&app, &admin(), "sub-carol").await.is_none());
    app.stop().await;
  }

  #[tokio::test]
  async fn an_account_an_admin_deleted_is_not_made_again_before_its_sign_in() {
    let (app, _provider) = rig!(vec![fake_user("sub-carol", &["member"])]);
    signed_in_staff(&app).await;
    sync_once(&app.state).await.unwrap();
    let carol_id = find(&app, &admin(), "sub-carol").await["id"]
      .as_str()
      .unwrap()
      .to_owned();
    let reply = app.delete(&admin(), &format!("/admin/users/{carol_id}")).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);

    assert_eq!(sync_once(&app.state).await.unwrap().created, 0);
    assert!(find_maybe(&app, &admin(), "sub-carol").await.is_none());
    app.stop().await;
  }

  #[tokio::test]
  async fn the_group_mapping_applies_before_the_first_sign_in() {
    let (app, provider) = rig!(vec![fake_user("sub-carol", &["member"])]);
    signed_in_staff(&app).await;
    let space = create_space(&app, &admin(), "Engineering", "invite_only").await;
    let general = create_channel(&app, &admin(), &space, "general", "public").await;
    provider.set_groups(vec![fake_group("g-eng", "engineering", &["sub-carol"])]);
    let reply = app
      .put(
        &admin(),
        "/admin/identity-provider/group-mappings",
        json!({"mappings": [{"group_path": "/engineering", "space_ids": [space], "channel_ids": [general]}]}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);

    let report = sync_once(&app.state).await.unwrap();
    assert_eq!(report.created, 1);
    let carol_id = find(&app, &admin(), "sub-carol").await["id"]
      .as_str()
      .unwrap()
      .to_owned();
    assert!(is_member(&app, &general, &carol_id).await, "in the same run");
    app.stop().await;
  }
}

mod deprovisioning {
  use super::*;

  #[tokio::test]
  async fn an_account_disabled_before_its_first_sign_in_is_removed_outright_and_comes_back() {
    let (app, provider) = rig!(vec![fake_user("sub-carol", &["member"])]);
    signed_in_staff(&app).await;
    sync_once(&app.state).await.unwrap();
    let carol_id = find(&app, &admin(), "sub-carol").await["id"]
      .as_str()
      .unwrap()
      .to_owned();
    let space = create_space(&app, &admin(), "Team", "invite_only").await;
    let channel = create_channel(&app, &admin(), &space, "team", "private").await;
    add_space_member(&app, &admin(), &space, &carol_id, "member").await;
    add_channel_member(&app, &admin(), &channel, &carol_id).await;

    provider.update("sub-carol", |user| user.is_enabled = false);
    let report = sync_once(&app.state).await.unwrap();
    assert_eq!((report.deleted, report.deactivated), (1, 0));
    assert!(!is_member(&app, &channel, &carol_id).await);
    assert!(is_deleted(&app, &carol_id).await);
    let entry = audit_entry(&app, &carol_id, "user_deleted").await;
    assert_eq!(entry["source"], "oidc_sync");
    assert_eq!(entry["reason"], "never_signed_in");
    assert_eq!(entry["cause"], "disabled");

    // Enabled again: the next run makes a new account, the identity was never released.
    provider.update("sub-carol", |user| user.is_enabled = true);
    assert_eq!(sync_once(&app.state).await.unwrap().created, 1);
    let again = find(&app, &admin(), "sub-carol").await;
    assert_ne!(again["id"].as_str().unwrap(), carol_id);
    assert_eq!(
      user_id(&app, &member("carol")).await,
      again["id"].as_str().unwrap()
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn an_account_that_signed_in_is_deactivated_not_removed() {
    let (app, provider) = rig!(vec![fake_user("sub-carol", &["member"])]);
    signed_in_staff(&app).await;
    sync_once(&app.state).await.unwrap();
    let carol_id = user_id(&app, &member("carol")).await;

    provider.update("sub-carol", |user| user.is_enabled = false);
    let report = sync_once(&app.state).await.unwrap();
    assert_eq!((report.deleted, report.deactivated), (0, 1));
    assert!(!is_deleted(&app, &carol_id).await);
    app.stop().await;
  }
}

/// Who may add an account that never signed in to a channel: exactly who may add anybody (the same
/// permission check), never a guest.
mod permissions {
  use super::*;

  #[tokio::test]
  async fn adding_a_pre_made_account_is_allowed_exactly_like_adding_anybody() {
    let (app, _provider) = rig!(vec![
      fake_user("sub-kim", &["member"]),
      fake_user("sub-lee", &["member"])
    ]);
    signed_in_staff(&app).await;
    sync_once(&app.state).await.unwrap();
    let kim = find(&app, &admin(), "sub-kim").await["id"]
      .as_str()
      .unwrap()
      .to_owned();
    // Lee signed in: the comparison.
    let lee = user_id(&app, &member("lee")).await;
    let (dave, gwen) = (member("dave"), guest("gwen"));
    let dave_id = user_id(&app, &dave).await;
    let gwen_id = user_id(&app, &gwen).await;

    for visibility in ["public", "private"] {
      let space = create_space(&app, &admin(), &format!("Space {visibility}"), "invite_only").await;
      let channel = create_channel(&app, &admin(), &space, "team", visibility).await;
      for (user, role) in [
        (&kim, "member"),
        (&lee, "member"),
        (&dave_id, "member"),
        (&gwen_id, "guest"),
      ] {
        add_space_member(&app, &admin(), &space, user, role).await;
      }
      for user in [&dave_id, &gwen_id] {
        add_channel_member(&app, &admin(), &channel, user).await;
      }

      for (caller, name) in [
        (owner(), "owner"),
        (admin(), "admin"),
        (dave.clone(), "member"),
        (gwen.clone(), "guest"),
      ] {
        let for_kim = add(&app, &caller, &channel, &kim).await;
        let for_lee = add(&app, &caller, &channel, &lee).await;
        assert_eq!(for_kim, for_lee, "{name} in a {visibility} channel");
        if name == "guest" {
          assert_eq!(for_kim, StatusCode::FORBIDDEN, "{visibility}");
        }
        if name == "admin" || name == "owner" {
          assert!(
            for_kim.is_success() || for_kim == StatusCode::CONFLICT,
            "{name}: {for_kim}"
          );
        }
        // Back to the start for the next caller.
        remove(&app, &channel, &kim).await;
        remove(&app, &channel, &lee).await;
      }
    }
    app.stop().await;
  }

  /// The status of adding `user`, without asserting.
  async fn add(app: &TestApp, by: &TestUser, channel: &str, user: &str) -> StatusCode {
    app
      .post(
        by,
        &format!("/conversations/{channel}/members"),
        json!({"user_id": user}),
      )
      .await
      .status
  }

  async fn remove(app: &TestApp, channel: &str, user: &str) {
    teamcraft_db::testing::sqlx::query(
      "DELETE FROM memberships WHERE conversation_id = $1::text::bigint AND user_id = $2::text::bigint",
    )
    .bind(channel)
    .bind(user)
    .execute(app.sql_pool())
    .await
    .unwrap();
  }
}

fn disabled(mut user: FakeUser) -> FakeUser {
  user.is_enabled = false;
  user
}

/// The owner and the admin sign in, so runs only make the accounts a test is about.
async fn signed_in_staff(app: &TestApp) {
  user_id(app, &owner()).await;
  user_id(app, &admin()).await;
}

/// The directory entry of the one person `query` finds.
async fn find(app: &TestApp, by: &TestUser, query: &str) -> Value {
  find_maybe(app, by, query)
    .await
    .unwrap_or_else(|| panic!("{query} is not in the directory"))
}

async fn find_maybe(app: &TestApp, by: &TestUser, query: &str) -> Option<Value> {
  let reply = app.get(by, &format!("/users?query={query}")).await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  reply.body["users"]
    .as_array()
    .unwrap()
    .iter()
    .find(|user| user["username"] == query)
    .cloned()
}

async fn audit_actions(app: &TestApp, user: &str) -> Vec<String> {
  audit_of(app, user)
    .await
    .iter()
    .map(|entry| entry["action"].as_str().unwrap().to_owned())
    .collect()
}

/// The detail of the user's latest audit entry with `action`.
async fn audit_entry(app: &TestApp, user: &str, action: &str) -> Value {
  let entries = audit_of(app, user).await;
  let entry = entries
    .iter()
    .find(|entry| entry["action"] == action)
    .unwrap_or_else(|| panic!("no {action} in {entries:?}"));
  serde_json::from_str(entry["detail"].as_str().unwrap()).unwrap()
}

async fn audit_of(app: &TestApp, user: &str) -> Vec<Value> {
  app
    .get(
      &owner(),
      &format!("/admin/audit?target_kind=user&target_id={user}"),
    )
    .await
    .body["entries"]
    .as_array()
    .unwrap()
    .clone()
}

async fn is_member(app: &TestApp, channel: &str, user: &str) -> bool {
  teamcraft_db::testing::sqlx::query_scalar::<_, bool>(
    "SELECT EXISTS (SELECT 1 FROM memberships WHERE conversation_id = $1::text::bigint AND user_id = $2::text::bigint)",
  )
  .bind(channel)
  .bind(user)
  .fetch_one(app.sql_pool())
  .await
  .unwrap()
}

async fn is_deleted(app: &TestApp, user: &str) -> bool {
  teamcraft_db::testing::sqlx::query_scalar::<_, bool>(
    "SELECT deleted_at IS NOT NULL FROM users WHERE id = $1::text::bigint",
  )
  .bind(user)
  .fetch_one(app.sql_pool())
  .await
  .unwrap()
}
