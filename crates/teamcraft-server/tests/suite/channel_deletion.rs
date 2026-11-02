//! Leaving and deleting channels and spaces (issue #1130) against a real server and Postgres: every
//! role × state (active, archived) × last admin / last member, who reaches what, what stays without
//! members, what happens at the same moment, and that background work clears storage and rows.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use image::ImageFormat;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_db::testing::sqlx;
use teamcraft_server::services::conversations::sweep_deleted;
use teamcraft_server::testing::{TestApp, TestUser};

use crate::space_images::{picture, put_image};

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

mod leaving {
  use super::*;

  #[tokio::test]
  async fn the_last_admin_of_an_active_channel_is_refused_and_told_the_way_out() {
    let app = app!();
    let (ann, bob) = (member("ann"), member("bob"));
    let channel = channel_of(&app, &ann, &[&bob], "private").await;
    let ann_id = user_id(&app, &ann).await;

    for reply in [
      app
        .get(
          &ann,
          &format!("/conversations/{channel}/members/{ann_id}/removal"),
        )
        .await,
      app
        .delete(&ann, &format!("/conversations/{channel}/members/{ann_id}"))
        .await,
    ] {
      assert_eq!(
        (reply.status, reply.error_code()),
        (StatusCode::CONFLICT, "last_channel_admin")
      );
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn the_last_admin_of_an_archived_channel_leaves() {
    let app = app!();
    let (ann, bob) = (member("ann"), member("bob"));
    let channel = channel_of(&app, &ann, &[&bob], "private").await;
    let ann_id = user_id(&app, &ann).await;
    archive(&app, &ann, &channel).await;

    let check = app
      .get(
        &ann,
        &format!("/conversations/{channel}/members/{ann_id}/removal"),
      )
      .await;
    assert!(check.status.is_success(), "{}", check.body);
    let left = app
      .delete(&ann, &format!("/conversations/{channel}/members/{ann_id}"))
      .await;
    assert_eq!(left.status, StatusCode::NO_CONTENT, "{}", left.body);

    let listed = app.get(&bob, &format!("/conversations/{channel}/members")).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(listed.body["members"].as_array().unwrap().len(), 1);
    app.stop().await;
  }

  /// Only the admin themself: removing the last admin stays refused in an archived channel too.
  #[tokio::test]
  async fn nobody_else_removes_the_last_admin_of_an_archived_channel() {
    let app = app!();
    let (owner_user, ann, bob) = (owner(), member("ann"), member("bob"));
    let channel = channel_of(&app, &ann, &[&bob], "private").await;
    let ann_id = user_id(&app, &ann).await;
    user_id(&app, &owner_user).await;
    archive(&app, &ann, &channel).await;

    let removed = app
      .delete(&owner_user, &format!("/conversations/{channel}/members/{ann_id}"))
      .await;
    assert_eq!(
      (removed.status, removed.error_code()),
      (StatusCode::CONFLICT, "last_channel_admin")
    );
    app.stop().await;
  }

  /// A stale list can't delete by accident: new clients say what they want, older ones say nothing
  /// and leave the channel without members.
  #[tokio::test]
  async fn the_last_member_of_a_private_channel_says_what_they_want() {
    for is_archived in [false, true] {
      let app = app!();
      let ann = member("ann");
      let channel = channel_of(&app, &ann, &[], "private").await;
      let ann_id = user_id(&app, &ann).await;
      if is_archived {
        archive(&app, &ann, &channel).await;
      }
      let path = format!("/conversations/{channel}/members/{ann_id}");

      for refused in [
        app
          .get(&ann, &format!("{path}/removal?delete_if_last=false"))
          .await,
        app.delete(&ann, &format!("{path}?delete_if_last=false")).await,
      ] {
        assert_eq!(
          (refused.status, refused.error_code()),
          (StatusCode::CONFLICT, "last_member")
        );
      }

      let left = app.delete(&ann, &format!("{path}?delete_if_last=true")).await;
      assert_eq!(left.status, StatusCode::NO_CONTENT, "{}", left.body);
      assert_deleted_for(&app, &ann, &channel).await;
      app.stop().await;
    }
  }

  #[tokio::test]
  async fn an_older_client_leaves_a_private_channel_without_members() {
    let app = app!();
    let ann = member("ann");
    let channel = channel_of(&app, &ann, &[], "private").await;
    let ann_id = user_id(&app, &ann).await;

    let left = app
      .delete(&ann, &format!("/conversations/{channel}/members/{ann_id}"))
      .await;
    assert_eq!(left.status, StatusCode::NO_CONTENT, "{}", left.body);

    assert!(is_there(&app, &channel).await);
    app.stop().await;
  }

  /// Anyone can join a public channel and leave it: that must not delete it, nor imported history.
  #[tokio::test]
  async fn public_and_imported_channels_stay_without_members() {
    let app = app!();
    let ann = member("ann");
    let ann_id = user_id(&app, &ann).await;
    let public = channel_of(&app, &ann, &[], "public").await;
    let imported = channel_of(&app, &ann, &[], "private").await;
    sqlx::query("UPDATE conversations SET imported_through_message_id = 1 WHERE id = $1")
      .bind(imported.parse::<i64>().unwrap())
      .execute(app.sql_pool())
      .await
      .unwrap();

    for channel in [&public, &imported] {
      let left = app
        .delete(
          &ann,
          &format!("/conversations/{channel}/members/{ann_id}?delete_if_last=true"),
        )
        .await;
      assert_eq!(left.status, StatusCode::NO_CONTENT, "{}", left.body);
      assert!(is_there(&app, channel).await, "{channel}");
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn removing_the_last_member_deletes_nothing() {
    let app = app!();
    let (server_owner, ann) = (owner(), member("ann"));
    user_id(&app, &server_owner).await;
    let bob = member("bob");
    let channel = channel_of(&app, &ann, &[&bob], "public").await;
    let ann_id = user_id(&app, &ann).await;
    let bob_id = user_id(&app, &bob).await;
    for user in [&bob_id, &ann_id] {
      let removed = app
        .delete(
          &server_owner,
          &format!("/conversations/{channel}/members/{user}?delete_if_last=true"),
        )
        .await;
      // The last admin needs another admin only while somebody else is in.
      assert!(removed.status.is_success(), "{}", removed.body);
    }
    assert!(is_there(&app, &channel).await);
    app.stop().await;
  }

  #[tokio::test]
  async fn leaving_a_space_deletes_the_private_channels_only_the_leaver_is_in() {
    let app = app!();
    let (server_owner, ann, bob) = (owner(), member("ann"), member("bob"));
    let space = create_space(&app, &server_owner, "Team", "open").await;
    let ann_id = user_id(&app, &ann).await;
    let bob_id = user_id(&app, &bob).await;
    add_space_member(&app, &server_owner, &space, &ann_id, "member").await;
    add_space_member(&app, &server_owner, &space, &bob_id, "member").await;
    let alone = create_channel(&app, &ann, &space, "alone", "private").await;
    let public = create_channel(&app, &ann, &space, "open", "public").await;
    let shared = create_channel(&app, &ann, &space, "shared", "private").await;
    add_channel_member(&app, &ann, &shared, &bob_id).await;
    let promoted = app
      .patch(
        &ann,
        &format!("/conversations/{shared}/members/{bob_id}"),
        json!({"role": "admin"}),
      )
      .await;
    assert!(promoted.status.is_success(), "{}", promoted.body);

    let left = app
      .delete(
        &ann,
        &format!("/spaces/{space}/members/{ann_id}?delete_if_last=true"),
      )
      .await;
    assert_eq!(left.status, StatusCode::NO_CONTENT, "{}", left.body);

    assert_deleted_for(&app, &ann, &alone).await;
    assert!(is_there(&app, &public).await);
    assert!(is_there(&app, &shared).await);
    app.stop().await;
  }
}

mod deleting {
  use super::*;

  /// Channel admin, workspace admin and owner, with members in it, in either state.
  #[tokio::test]
  async fn who_may_delete_does_so_with_members_in_it() {
    for is_archived in [false, true] {
      let app = app!();
      let (ann, bob) = (member("ann"), member("bob"));
      let (server_admin, server_owner) = (admin(), owner());
      user_id(&app, &server_admin).await;
      user_id(&app, &server_owner).await;
      for deleter in [&ann, &server_admin, &server_owner] {
        let channel = channel_of(&app, &ann, &[&bob], "private").await;
        if is_archived {
          archive(&app, &ann, &channel).await;
        }
        let reply = app.delete(deleter, &format!("/conversations/{channel}")).await;
        assert_eq!(
          reply.status,
          StatusCode::NO_CONTENT,
          "{}: {}",
          deleter.username,
          reply.body
        );
        assert_deleted_for(&app, &ann, &channel).await;
        assert_deleted_for(&app, &bob, &channel).await;
      }
      app.stop().await;
    }
  }

  /// A space admin reaches a public channel or one they are in, not a private one they can't see.
  #[tokio::test]
  async fn a_space_admin_deletes_only_what_they_can_see() {
    let app = app!();
    let (server_owner, sam, ann) = (owner(), member("sam"), member("ann"));
    let sam_id = user_id(&app, &sam).await;
    let ann_id = user_id(&app, &ann).await;
    let space = create_space(&app, &server_owner, "Team", "open").await;
    add_space_member(&app, &server_owner, &space, &sam_id, "space_admin").await;
    add_space_member(&app, &server_owner, &space, &ann_id, "member").await;

    let public = create_channel(&app, &ann, &space, "open", "public").await;
    let reply = app.delete(&sam, &format!("/conversations/{public}")).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);

    let private = create_channel(&app, &ann, &space, "secret", "private").await;
    let hidden = app.delete(&sam, &format!("/conversations/{private}")).await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND, "{}", hidden.body);
    assert!(is_there(&app, &private).await);

    add_channel_member(&app, &ann, &private, &sam_id).await;
    let reply = app.delete(&sam, &format!("/conversations/{private}")).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    app.stop().await;
  }

  #[tokio::test]
  async fn members_moderators_and_guests_may_not() {
    let app = app!();
    let (ann, bob, carol, erin) = (member("ann"), member("bob"), member("carol"), guest("erin"));
    let channel = channel_of(&app, &ann, &[&bob, &carol, &erin], "private").await;
    let carol_id = user_id(&app, &carol).await;
    let moderator = app
      .patch(
        &ann,
        &format!("/conversations/{channel}/members/{carol_id}"),
        json!({"role": "moderator"}),
      )
      .await;
    assert!(moderator.status.is_success(), "{}", moderator.body);

    for caller in [&bob, &carol, &erin] {
      let refused = app.delete(caller, &format!("/conversations/{channel}")).await;
      assert_eq!(
        refused.status,
        StatusCode::FORBIDDEN,
        "{}: {}",
        caller.username,
        refused.body
      );
    }
    // Somebody who doesn't see it isn't told it exists.
    let stranger = member("stranger");
    user_id(&app, &stranger).await;
    let hidden = app.delete(&stranger, &format!("/conversations/{channel}")).await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND, "{}", hidden.body);
    app.stop().await;
  }

  #[tokio::test]
  async fn while_an_import_account_holds_it_the_channel_stays() {
    let app = app!();
    let ann = member("ann");
    let ann_id = user_id(&app, &ann).await;
    let channel = channel_of(&app, &ann, &[], "private").await;
    sqlx::query("UPDATE conversations SET importer_user_id = $1 WHERE id = $2")
      .bind(ann_id.parse::<i64>().unwrap())
      .bind(channel.parse::<i64>().unwrap())
      .execute(app.sql_pool())
      .await
      .unwrap();

    let refused = app.delete(&ann, &format!("/conversations/{channel}")).await;
    assert_eq!(
      (refused.status, refused.error_code()),
      (StatusCode::CONFLICT, "import_in_progress")
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn direct_messages_are_refused_and_a_second_delete_finds_it_gone_with_one_audit_row() {
    let app = app!();
    let (ann, bob) = (member("ann"), member("bob"));
    let bob_id = user_id(&app, &bob).await;
    user_id(&app, &ann).await;
    let dm = app.post(&ann, "/dms", json!({"user_id": bob_id})).await;
    assert!(dm.status.is_success(), "{}", dm.body);
    let dm_id = dm.body["id"].as_str().unwrap();
    let refused = app.delete(&ann, &format!("/conversations/{dm_id}")).await;
    assert_eq!(
      (refused.status, refused.error_code()),
      (StatusCode::BAD_REQUEST, "not_a_channel")
    );

    let channel = channel_of(&app, &ann, &[&bob], "private").await;
    let first = app.delete(&ann, &format!("/conversations/{channel}")).await;
    assert_eq!(first.status, StatusCode::NO_CONTENT, "{}", first.body);
    let second = app.delete(&ann, &format!("/conversations/{channel}")).await;
    assert_eq!(
      (second.status, second.error_code()),
      (StatusCode::NOT_FOUND, "channel_not_found")
    );
    let rows: i64 = sqlx::query_scalar(
      "SELECT count(*) FROM audit_log WHERE action = 'channel_deleted' AND target_id = $1",
    )
    .bind(channel.parse::<i64>().unwrap())
    .fetch_one(app.sql_pool())
    .await
    .unwrap();
    assert_eq!(rows, 1);
    app.stop().await;
  }

  /// The request only marks; background work clears the rows, running twice is harmless.
  #[tokio::test]
  async fn the_sweep_clears_the_rows_and_may_run_again() {
    let app = app!();
    let (ann, bob) = (member("ann"), member("bob"));
    let channel = channel_of(&app, &ann, &[&bob], "private").await;
    let device = register_device(&app, &ann).await;
    send(&app, &ann, &channel, &device, 1).await;

    let reply = app.delete(&ann, &format!("/conversations/{channel}")).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);

    sweep_deleted(&app.state).await.unwrap();
    sweep_deleted(&app.state).await.unwrap();
    assert!(!row_exists(&app, &channel).await);
    let messages: i64 = sqlx::query_scalar("SELECT count(*) FROM messages WHERE conversation_id = $1")
      .bind(channel.parse::<i64>().unwrap())
      .fetch_one(app.sql_pool())
      .await
      .unwrap();
    assert_eq!(messages, 0);
    app.stop().await;
  }

  /// A channel with a picture: its object in storage goes before its rows.
  #[tokio::test]
  async fn the_sweep_deletes_objects_in_storage() {
    let Some(app) = TestApp::start_with_storage(|_| {}).await else {
      return;
    };
    let ann = member("ann");
    let channel = channel_of(&app, &ann, &[], "private").await;
    let (status, body) = put_image(
      &app,
      &ann,
      &format!("/conversations/{channel}/image"),
      "image/png",
      &picture(8, 8, ImageFormat::Png),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let key: String = sqlx::query_scalar("SELECT image_key FROM conversations WHERE id = $1")
      .bind(channel.parse::<i64>().unwrap())
      .fetch_one(app.sql_pool())
      .await
      .unwrap();
    let store = app.state.maybe_blob_store.as_ref().unwrap();
    assert!(store.object_size(&key).await.unwrap().is_some());

    let reply = app.delete(&ann, &format!("/conversations/{channel}")).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    sweep_deleted(&app.state).await.unwrap();

    assert!(store.object_size(&key).await.unwrap().is_none());
    assert!(!row_exists(&app, &channel).await);
    app.stop().await;
  }

  /// A channel with members and no admin isn't restored until somebody can run it.
  #[tokio::test]
  async fn restoring_a_channel_without_an_admin_takes_assigning_one() {
    let app = app!();
    let (server_owner, ann, bob) = (owner(), member("ann"), member("bob"));
    user_id(&app, &server_owner).await;
    let channel = channel_of(&app, &ann, &[&bob], "private").await;
    let ann_id = user_id(&app, &ann).await;
    archive(&app, &ann, &channel).await;
    let left = app
      .delete(&ann, &format!("/conversations/{channel}/members/{ann_id}"))
      .await;
    assert_eq!(left.status, StatusCode::NO_CONTENT, "{}", left.body);

    let refused = app
      .patch(
        &server_owner,
        &format!("/conversations/{channel}"),
        json!({"is_archived": false}),
      )
      .await;
    assert_eq!(
      (refused.status, refused.error_code()),
      (StatusCode::CONFLICT, "channel_needs_admin")
    );
    app.stop().await;
  }
}

mod at_the_same_moment {
  use super::*;

  /// Two admins leave together: one stays, whichever comes second is refused.
  #[tokio::test]
  async fn the_last_two_admins_can_not_both_leave() {
    let app = app!();
    let (ann, bob, carol) = (member("ann"), member("bob"), member("carol"));
    let channel = channel_of(&app, &ann, &[&bob, &carol], "private").await;
    let ann_id = user_id(&app, &ann).await;
    let bob_id = user_id(&app, &bob).await;
    let promoted = app
      .patch(
        &ann,
        &format!("/conversations/{channel}/members/{bob_id}"),
        json!({"role": "admin"}),
      )
      .await;
    assert!(promoted.status.is_success(), "{}", promoted.body);

    let ann_path = format!("/conversations/{channel}/members/{ann_id}");
    let bob_path = format!("/conversations/{channel}/members/{bob_id}");
    let (first, second) = tokio::join!(app.delete(&ann, &ann_path), app.delete(&bob, &bob_path));

    let statuses = [first.status, second.status];
    assert!(statuses.contains(&StatusCode::NO_CONTENT), "{statuses:?}");
    assert!(statuses.contains(&StatusCode::CONFLICT), "{statuses:?}");
    app.stop().await;
  }

  /// Whoever joins during a deletion ends up without a membership in it.
  #[tokio::test]
  async fn a_join_during_a_deletion_does_not_stick() {
    let app = app!();
    let (server_owner, ann, bob) = (owner(), member("ann"), member("bob"));
    let space = create_space(&app, &server_owner, "Team", "open").await;
    let ann_id = user_id(&app, &ann).await;
    let bob_id = user_id(&app, &bob).await;
    add_space_member(&app, &server_owner, &space, &ann_id, "member").await;
    add_space_member(&app, &server_owner, &space, &bob_id, "member").await;
    let channel = create_channel(&app, &ann, &space, "open", "public").await;

    let delete_path = format!("/conversations/{channel}");
    let join_path = format!("/conversations/{channel}/join");
    let (deleted, _joined) = tokio::join!(
      app.delete(&ann, &delete_path),
      app.post(&bob, &join_path, json!({}))
    );
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);

    assert!(!is_there(&app, &channel).await);
    let bobs: i64 = sqlx::query_scalar(
      "SELECT count(*) FROM memberships m JOIN conversations c ON c.id = m.conversation_id
       WHERE m.conversation_id = $1 AND c.deleted_at IS NULL",
    )
    .bind(channel.parse::<i64>().unwrap())
    .fetch_one(app.sql_pool())
    .await
    .unwrap();
    assert_eq!(bobs, 0);
    app.stop().await;
  }
}

mod deleting_a_space {
  use super::*;

  #[tokio::test]
  async fn owners_admins_and_its_space_admin_delete_it_with_its_channels() {
    let app = app!();
    let (sam, ann, server_admin, server_owner) = (member("sam"), member("ann"), admin(), owner());
    user_id(&app, &server_admin).await;
    let sam_id = user_id(&app, &sam).await;
    let ann_id = user_id(&app, &ann).await;
    for (deleter, is_archived) in [(&sam, false), (&server_admin, true), (&server_owner, false)] {
      let space = create_space(&app, &server_owner, "Project A", "open").await;
      add_space_member(&app, &server_owner, &space, &sam_id, "space_admin").await;
      add_space_member(&app, &server_owner, &space, &ann_id, "member").await;
      let channel = create_channel(&app, &ann, &space, "open", "public").await;
      if is_archived {
        let reply = app
          .patch(
            &server_owner,
            &format!("/spaces/{space}"),
            json!({"is_archived": true}),
          )
          .await;
        assert!(reply.status.is_success(), "{}", reply.body);
      }

      let reply = app.delete(deleter, &format!("/spaces/{space}")).await;
      assert_eq!(
        reply.status,
        StatusCode::NO_CONTENT,
        "{}: {}",
        deleter.username,
        reply.body
      );

      assert_eq!(
        app.get(&server_owner, &format!("/spaces/{space}")).await.status,
        StatusCode::NOT_FOUND
      );
      assert_deleted_for(&app, &ann, &channel).await;
    }
    sweep_deleted(&app.state).await.unwrap();
    app.stop().await;
  }

  /// A space admin can't take private channels they can't see with the space; a workspace admin can.
  #[tokio::test]
  async fn a_space_admin_is_refused_while_it_holds_private_channels_they_are_not_in() {
    let app = app!();
    let (server_owner, sam, ann) = (owner(), member("sam"), member("ann"));
    let sam_id = user_id(&app, &sam).await;
    let ann_id = user_id(&app, &ann).await;
    let space = create_space(&app, &server_owner, "Project A", "open").await;
    add_space_member(&app, &server_owner, &space, &sam_id, "space_admin").await;
    add_space_member(&app, &server_owner, &space, &ann_id, "member").await;
    let hidden = create_channel(&app, &ann, &space, "secret", "private").await;

    let shown = app.get(&sam, &format!("/spaces/{space}")).await;
    assert_ne!(shown.body["can_delete_space"], json!(true), "{}", shown.body);
    let refused = app.delete(&sam, &format!("/spaces/{space}")).await;
    assert_eq!(
      (refused.status, refused.error_code()),
      (StatusCode::CONFLICT, "space_has_hidden_channels")
    );
    assert!(is_there(&app, &hidden).await);

    let as_owner = app.get(&server_owner, &format!("/spaces/{space}")).await;
    assert_eq!(as_owner.body["can_delete_space"], json!(true));
    assert_eq!(as_owner.body["deleted_channel_count"], json!(1));
    let reply = app.delete(&server_owner, &format!("/spaces/{space}")).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    assert_deleted_for(&app, &ann, &hidden).await;
    app.stop().await;
  }

  #[tokio::test]
  async fn members_may_not() {
    let app = app!();
    let (server_owner, ann, stranger) = (owner(), member("ann"), member("stranger"));
    let ann_id = user_id(&app, &ann).await;
    user_id(&app, &stranger).await;
    let space = create_space(&app, &server_owner, "Project A", "invite_only").await;
    add_space_member(&app, &server_owner, &space, &ann_id, "member").await;

    let refused = app.delete(&ann, &format!("/spaces/{space}")).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    let hidden = app.delete(&stranger, &format!("/spaces/{space}")).await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND, "{}", hidden.body);
    app.stop().await;
  }
}

/// A channel `admin` made, so they are its only channel admin, with `others` in it.
async fn channel_of(app: &TestApp, admin: &TestUser, others: &[&TestUser], visibility: &str) -> String {
  let space_owner = TestUser::new("space-owner", &["owner"]);
  let space = create_space(app, &space_owner, "Team", "invite_only").await;
  let admin_id = user_id(app, admin).await;
  add_space_member(app, &space_owner, &space, &admin_id, "member").await;
  let channel = create_channel(app, admin, &space, "secure", visibility).await;
  for other in others {
    let id = user_id(app, other).await;
    let role = if other.roles.iter().any(|role| role == "guest") {
      "guest"
    } else {
      "member"
    };
    add_space_member(app, &space_owner, &space, &id, role).await;
    add_channel_member(app, admin, &channel, &id).await;
  }
  channel
}

async fn archive(app: &TestApp, by: &TestUser, channel: &str) {
  let reply = app
    .patch(
      by,
      &format!("/conversations/{channel}"),
      json!({"is_archived": true}),
    )
    .await;
  assert!(reply.status.is_success(), "{}", reply.body);
}

/// Not marked deleted.
async fn is_there(app: &TestApp, channel: &str) -> bool {
  let found: Option<i64> =
    sqlx::query_scalar("SELECT id FROM conversations WHERE id = $1 AND deleted_at IS NULL")
      .bind(channel.parse::<i64>().unwrap())
      .fetch_optional(app.sql_pool())
      .await
      .unwrap();
  found.is_some()
}

async fn row_exists(app: &TestApp, channel: &str) -> bool {
  let found: Option<i64> = sqlx::query_scalar("SELECT id FROM conversations WHERE id = $1")
    .bind(channel.parse::<i64>().unwrap())
    .fetch_optional(app.sql_pool())
    .await
    .unwrap();
  found.is_some()
}

/// Gone from the server, and named to `user`'s devices that were away.
async fn assert_deleted_for(app: &TestApp, user: &TestUser, channel: &str) {
  let reply = app.get(user, &format!("/conversations/{channel}")).await;
  assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.body);
  let listing = app.get(user, "/conversations").await;
  assert!(
    listing.body["deleted_conversation_ids"]
      .as_array()
      .is_some_and(|deleted| deleted.iter().any(|id| id == channel)),
    "{}",
    listing.body
  );
}
