//! Leaving and deleting a channel (issue #1130) against a real server and Postgres: every role ×
//! state (active, archived) × last admin / last member.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_server::testing::{TestApp, TestUser};

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
    let (alice, bob) = (member("ann"), member("bob"));
    let channel = channel_of(&app, &alice, &[&bob]).await;
    let alice_id = user_id(&app, &alice).await;

    for reply in [
      app
        .get(
          &alice,
          &format!("/conversations/{channel}/members/{alice_id}/removal"),
        )
        .await,
      app
        .delete(&alice, &format!("/conversations/{channel}/members/{alice_id}"))
        .await,
    ] {
      assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.body);
      assert!(
        reply.body.to_string().contains("delete the channel"),
        "{}",
        reply.body
      );
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn the_last_admin_of_an_archived_channel_leaves() {
    let app = app!();
    let (alice, bob) = (member("ann"), member("bob"));
    let channel = channel_of(&app, &alice, &[&bob]).await;
    let alice_id = user_id(&app, &alice).await;
    archive(&app, &alice, &channel).await;

    let check = app
      .get(
        &alice,
        &format!("/conversations/{channel}/members/{alice_id}/removal"),
      )
      .await;
    assert!(check.status.is_success(), "{}", check.body);
    let left = app
      .delete(&alice, &format!("/conversations/{channel}/members/{alice_id}"))
      .await;
    assert_eq!(left.status, StatusCode::NO_CONTENT, "{}", left.body);

    // The others stay in it.
    let listed = app.get(&bob, &format!("/conversations/{channel}/members")).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(listed.body["members"].as_array().unwrap().len(), 1);
    app.stop().await;
  }

  /// Only the admin themself: removing the last admin stays refused in an archived channel too.
  #[tokio::test]
  async fn nobody_else_removes_the_last_admin_of_an_archived_channel() {
    let app = app!();
    let (owner_user, alice, bob) = (owner(), member("ann"), member("bob"));
    let channel = channel_of(&app, &alice, &[&bob]).await;
    let alice_id = user_id(&app, &alice).await;
    user_id(&app, &owner_user).await;
    archive(&app, &alice, &channel).await;

    let removed = app
      .delete(
        &owner_user,
        &format!("/conversations/{channel}/members/{alice_id}"),
      )
      .await;
    assert_eq!(removed.status, StatusCode::CONFLICT, "{}", removed.body);
    app.stop().await;
  }

  #[tokio::test]
  async fn the_last_member_deletes_the_channel_in_either_state() {
    for is_archived in [false, true] {
      let app = app!();
      let alice = member("ann");
      let channel = channel_of(&app, &alice, &[]).await;
      let alice_id = user_id(&app, &alice).await;
      if is_archived {
        archive(&app, &alice, &channel).await;
      }

      let left = app
        .delete(&alice, &format!("/conversations/{channel}/members/{alice_id}"))
        .await;
      assert_eq!(left.status, StatusCode::NO_CONTENT, "{}", left.body);

      assert_deleted_for(&app, &alice, &channel).await;
      app.stop().await;
    }
  }
}

mod deleting {
  use super::*;

  /// Channel admin, space admin, workspace admin and owner, with members in it, in either state.
  #[tokio::test]
  async fn who_may_delete_does_so_with_members_in_it() {
    for is_archived in [false, true] {
      let app = app!();
      let (space_admin, alice, bob) = (member("sam"), member("ann"), member("bob"));
      let (server_admin, server_owner) = (admin(), owner());
      user_id(&app, &server_admin).await;
      user_id(&app, &server_owner).await;
      let sam_id = user_id(&app, &space_admin).await;
      let alice_id = user_id(&app, &alice).await;
      let bob_id = user_id(&app, &bob).await;
      let space = create_space(&app, &server_owner, "Project A", "open").await;
      add_space_member(&app, &server_owner, &space, &sam_id, "space_admin").await;
      add_space_member(&app, &server_owner, &space, &alice_id, "member").await;
      add_space_member(&app, &server_owner, &space, &bob_id, "member").await;

      for deleter in [&alice, &space_admin, &server_admin, &server_owner] {
        // A private channel Sam isn't in: the space's admin deletes it all the same.
        let channel = create_channel(&app, &alice, &space, "private", "private").await;
        add_channel_member(&app, &alice, &channel, &bob_id).await;
        if is_archived {
          archive(&app, &alice, &channel).await;
        }
        let reply = app.delete(deleter, &format!("/conversations/{channel}")).await;
        assert_eq!(
          reply.status,
          StatusCode::NO_CONTENT,
          "{}: {}",
          deleter.username,
          reply.body
        );
        assert_deleted_for(&app, &alice, &channel).await;
        assert_deleted_for(&app, &bob, &channel).await;
      }
      app.stop().await;
    }
  }

  #[tokio::test]
  async fn members_moderators_and_guests_may_not() {
    let app = app!();
    let (alice, bob, carol, erin) = (member("ann"), member("bob"), member("carol"), guest("erin"));
    let channel = channel_of(&app, &alice, &[&bob, &carol, &erin]).await;
    let carol_id = user_id(&app, &carol).await;
    let moderator = app
      .patch(
        &alice,
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
  async fn direct_messages_are_refused_and_a_second_delete_finds_it_gone() {
    let app = app!();
    let (alice, bob) = (member("ann"), member("bob"));
    let bob_id = user_id(&app, &bob).await;
    user_id(&app, &alice).await;
    let dm = app.post(&alice, "/dms", json!({"user_id": bob_id})).await;
    assert!(dm.status.is_success(), "{}", dm.body);
    let dm_id = dm.body["id"].as_str().unwrap();
    let refused = app.delete(&alice, &format!("/conversations/{dm_id}")).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);

    let channel = channel_of(&app, &alice, &[&bob]).await;
    let first = app.delete(&alice, &format!("/conversations/{channel}")).await;
    assert_eq!(first.status, StatusCode::NO_CONTENT, "{}", first.body);
    let second = app.delete(&alice, &format!("/conversations/{channel}")).await;
    assert_eq!(second.status, StatusCode::NOT_FOUND, "{}", second.body);
    app.stop().await;
  }
}

/// A channel `admin` made, so they are its only channel admin, with `others` in it.
async fn channel_of(app: &TestApp, admin: &TestUser, others: &[&TestUser]) -> String {
  let space_owner = TestUser::new("space-owner", &["owner"]);
  let space = create_space(app, &space_owner, "Team", "invite_only").await;
  let admin_id = user_id(app, admin).await;
  add_space_member(app, &space_owner, &space, &admin_id, "member").await;
  let channel = create_channel(app, admin, &space, "secure", "private").await;
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
      let channel = create_channel(&app, &ann, &space, "secure", "private").await;
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
