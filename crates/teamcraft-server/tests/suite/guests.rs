//! Guest lifecycle: expiry, default terms and e-mail domain restrictions.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::time::Duration as StdDuration;

use common::*;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_proto::GatewayEvent;
use teamcraft_server::services::guests::{expire_once, send_notices_once};
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

mod notices_and_extending {
  use super::*;

  /// A guest in a space of `alice`, ending in `days`.
  async fn guest_ending_in(
    app: &TestApp,
    alice: &TestUser,
    ext: &TestUser,
    days: i64,
  ) -> (String, String, i64) {
    let ext_id = user_id(app, ext).await;
    let space = create_space(app, alice, "Client", "invite_only").await;
    let expires_at = (OffsetDateTime::now_utc() + Duration::days(days)).unix_timestamp() * 1000;
    let reply = app
      .post(
        alice,
        &format!("/spaces/{space}/members"),
        json!({"user_id": ext_id, "role": "guest", "expires_at": expires_at}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    (space, ext_id, expires_at)
  }

  #[tokio::test]
  async fn each_lead_time_gives_one_notice_and_a_new_expiry_starts_over() {
    let app = app!();
    let alice = owner();
    let ext = guest("ext");
    let (space, ext_id, expires_at) = guest_ending_in(&app, &alice, &ext, 30).await;
    let at = |days_before: i64| {
      OffsetDateTime::from_unix_timestamp(expires_at / 1000).unwrap() - Duration::days(days_before)
    };

    // Default lead times: 14 and 3 days.
    assert_eq!(
      send_notices_once(&app.state, at(15)).await.unwrap(),
      0,
      "too early"
    );
    assert_eq!(send_notices_once(&app.state, at(14)).await.unwrap(), 1, "14 days");
    assert_eq!(send_notices_once(&app.state, at(13)).await.unwrap(), 0, "once");
    assert_eq!(send_notices_once(&app.state, at(3)).await.unwrap(), 1, "3 days");
    assert_eq!(send_notices_once(&app.state, at(2)).await.unwrap(), 0, "once");

    let later = expires_at + 30 * 86_400_000;
    let reply = app
      .post(
        &alice,
        &format!("/spaces/{space}/guests/{ext_id}/extend"),
        json!({"expires_at": later}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let again = OffsetDateTime::from_unix_timestamp(later / 1000).unwrap() - Duration::days(3);
    assert_eq!(
      send_notices_once(&app.state, again).await.unwrap(),
      1,
      "a new expiry starts over"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn a_guest_past_two_lead_times_hears_once() {
    let app = app!();
    let alice = owner();
    let ext = guest("ext");
    guest_ending_in(&app, &alice, &ext, 2).await;
    let now = OffsetDateTime::now_utc();
    assert_eq!(send_notices_once(&app.state, now).await.unwrap(), 1);
    assert_eq!(send_notices_once(&app.state, now).await.unwrap(), 0);
    app.stop().await;
  }

  #[tokio::test]
  async fn who_extends_how_far_and_what_they_see() {
    let app = app!();
    let alice = owner();
    let ext = guest("ext");
    let carol = member("carol");
    let carol_id = user_id(&app, &carol).await;
    let (space, ext_id, expires_at) = guest_ending_in(&app, &alice, &ext, 5).await;
    let path = format!("/spaces/{space}/guests/{ext_id}/extend");
    let later = expires_at + 30 * 86_400_000;

    // The guest sees their own, without an offer; a member who isn't concerned sees nothing.
    let own = app.get(&ext, "/guests/expiring").await;
    assert_eq!(own.body["guests"][0]["is_own"], true, "{}", own.body);
    assert_eq!(own.body["guests"][0]["extend_offers"], json!([]), "{}", own.body);
    assert_eq!(
      app.get(&carol, "/guests/expiring").await.body["guests"],
      json!([])
    );

    assert_eq!(
      app.post(&ext, &path, json!({"expires_at": later})).await.status,
      StatusCode::FORBIDDEN,
      "a guest doesn't extend themself"
    );
    assert_eq!(
      app.post(&carol, &path, json!({"expires_at": later})).await.status,
      StatusCode::NOT_FOUND,
      "somebody outside an invite-only space doesn't learn about it"
    );
    let reply = app
      .post(
        &alice,
        &format!("/spaces/{space}/members"),
        json!({"user_id": carol_id, "role": "member"}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(
      app.post(&carol, &path, json!({"expires_at": later})).await.status,
      StatusCode::FORBIDDEN,
      "a member who isn't the sponsor doesn't extend"
    );

    // Extending is audited, a retry changes nothing and it never shortens.
    let reply = app.post(&alice, &path, json!({"expires_at": later})).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["expires_at"], later);
    let reply = app.post(&alice, &path, json!({"expires_at": later})).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let audit = app.get(&admin(), "/admin/audit").await;
    assert_eq!(
      audit.body.to_string().matches("guest_extended").count(),
      1,
      "one entry"
    );
    assert_eq!(
      app
        .post(&alice, &path, json!({"expires_at": expires_at}))
        .await
        .status,
      StatusCode::BAD_REQUEST,
      "extending doesn't shorten"
    );
    app.stop().await;
  }

  /// Carol, a space admin when she adds the guest, is the sponsor; then she becomes a plain member.
  async fn sponsored_by_a_member(
    app: &TestApp,
    alice: &TestUser,
    carol: &TestUser,
    ext: &TestUser,
    days: i64,
  ) -> (String, String, String, i64) {
    let carol_id = user_id(app, carol).await;
    let ext_id = user_id(app, ext).await;
    let space = create_space(app, alice, "Client", "invite_only").await;
    let add = |user: String, role: &'static str| json!({"user_id": user, "role": role});
    let path = format!("/spaces/{space}/members");
    let reply = app.post(alice, &path, add(carol_id.clone(), "space_admin")).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let expires_at = (OffsetDateTime::now_utc() + Duration::days(days)).unix_timestamp() * 1000;
    let reply = app
      .post(
        carol,
        &path,
        json!({"user_id": ext_id, "role": "guest", "expires_at": expires_at}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let reply = app
      .patch(alice, &format!("{path}/{carol_id}"), json!({"role": "member"}))
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    (space, ext_id, carol_id, expires_at)
  }

  #[tokio::test]
  async fn a_sponsor_stays_within_the_limit_and_what_is_offered_is_accepted() {
    let app = app!();
    let (alice, carol, ext) = (owner(), member("carol"), guest("ext"));
    let (space, ext_id, _, expires_at) = sponsored_by_a_member(&app, &alice, &carol, &ext, 5).await;
    let path = format!("/spaces/{space}/guests/{ext_id}/extend");

    let listed = app.get(&carol, "/guests/expiring").await;
    let offers = listed.body["guests"][0]["extend_offers"]
      .as_array()
      .unwrap()
      .clone();
    assert_eq!(offers.len(), 2, "{}", listed.body);

    let too_far = (OffsetDateTime::now_utc() + Duration::days(200)).unix_timestamp() * 1000;
    assert_eq!(
      app
        .post(&carol, &path, json!({"expires_at": too_far}))
        .await
        .status,
      StatusCode::FORBIDDEN
    );
    let reply = app
      .post(&carol, &path, json!({"expires_at": offers[1]["until"]}))
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(reply.body["expires_at"].as_i64().unwrap() > expires_at);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_sponsor_removed_from_the_space_hears_nothing_more_of_it() {
    let app = app!();
    let (alice, carol, ext) = (owner(), member("carol"), guest("ext"));
    let (space, ext_id, carol_id, _) = sponsored_by_a_member(&app, &alice, &carol, &ext, 2).await;
    let reply = app
      .delete(&alice, &format!("/spaces/{space}/members/{carol_id}"))
      .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);

    assert_eq!(
      app.get(&carol, "/guests/expiring").await.body["guests"],
      json!([])
    );
    let due = app
      .state
      .database
      .list_guests_due_notice(OffsetDateTime::now_utc(), &[14, 3], 10)
      .await
      .unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].user.to_string(), ext_id);
    assert_eq!(due[0].maybe_sponsor, None, "no mail to somebody who left");
    app.stop().await;
  }

  mod races {
    use super::*;

    #[tokio::test]
    async fn the_later_of_two_extensions_stands() {
      let app = app!();
      let (alice, ext) = (owner(), guest("ext"));
      let (space, ext_id, expires_at) = guest_ending_in(&app, &alice, &ext, 5).await;
      let (space_id, user) = (space.parse().unwrap(), ext_id.parse().unwrap());
      let at =
        |days: i64| OffsetDateTime::from_unix_timestamp(expires_at / 1000).unwrap() + Duration::days(days);
      let database = &app.state.database;
      assert!(
        database
          .extend_guest_expiry(space_id, user, at(90))
          .await
          .unwrap()
      );
      assert!(
        !database
          .extend_guest_expiry(space_id, user, at(30))
          .await
          .unwrap(),
        "no shortening"
      );
      let terms = database
        .space_member_terms(space_id, user)
        .await
        .unwrap()
        .unwrap();
      assert_eq!(terms.0, Some(at(90)));
      app.stop().await;
    }

    #[tokio::test]
    async fn an_extension_undoes_neither_a_removal_nor_a_promotion() {
      let app = app!();
      let (alice, ext) = (owner(), guest("ext"));
      let (space, ext_id, expires_at) = guest_ending_in(&app, &alice, &ext, 5).await;
      let (space_id, user) = (space.parse().unwrap(), ext_id.parse().unwrap());
      let later = OffsetDateTime::from_unix_timestamp(expires_at / 1000).unwrap() + Duration::days(30);
      let database = &app.state.database;

      database.remove_space_member(space_id, user, false).await.unwrap();
      assert!(!database.extend_guest_expiry(space_id, user, later).await.unwrap());
      assert_eq!(
        database.space_role(space_id, user).await.unwrap(),
        None,
        "still gone"
      );
      let reply = app
        .post(
          &alice,
          &format!("/spaces/{space}/guests/{ext_id}/extend"),
          json!({"expires_at": later.unix_timestamp() * 1000}),
        )
        .await;
      assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.body);

      // A guest made a member meanwhile stays one.
      let carol = member("carol");
      let (space, carol_id, _) = guest_ending_in(&app, &alice, &carol, 5).await;
      let (space_id, user) = (space.parse().unwrap(), carol_id.parse().unwrap());
      let reply = app
        .patch(
          &alice,
          &format!("/spaces/{space}/members/{carol_id}"),
          json!({"role": "member"}),
        )
        .await;
      assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
      assert!(!database.extend_guest_expiry(space_id, user, later).await.unwrap());
      assert_eq!(
        database.space_role(space_id, user).await.unwrap(),
        Some(teamcraft_proto::SpaceRole::Member)
      );
      app.stop().await;
    }

    /// The job read its list, then somebody extended: its `now` no longer finds it expired.
    #[tokio::test]
    async fn the_expiry_job_leaves_a_guest_extended_after_it_read_its_list() {
      let app = app!();
      let (alice, ext) = (owner(), guest("ext"));
      let (space, ext_id, expires_at) = guest_ending_in(&app, &alice, &ext, 5).await;
      let (space_id, user) = (space.parse().unwrap(), ext_id.parse().unwrap());
      let old_end = OffsetDateTime::from_unix_timestamp(expires_at / 1000).unwrap();
      let job_now = old_end + Duration::minutes(1);
      let database = &app.state.database;

      assert_eq!(database.list_expired_guests(job_now, 10).await.unwrap().len(), 1);
      assert!(
        database
          .extend_guest_expiry(space_id, user, old_end + Duration::days(30))
          .await
          .unwrap()
      );

      assert!(
        database
          .remove_expired_guest(space_id, user, job_now)
          .await
          .unwrap()
          .is_none()
      );
      assert!(
        database.space_role(space_id, user).await.unwrap().is_some(),
        "still in"
      );
      app.stop().await;
    }

    #[tokio::test]
    async fn a_notice_is_not_sent_for_an_expiry_that_moved() {
      let app = app!();
      let (alice, ext) = (owner(), guest("ext"));
      let (space, ext_id, expires_at) = guest_ending_in(&app, &alice, &ext, 2).await;
      let (space_id, user) = (space.parse().unwrap(), ext_id.parse().unwrap());
      let database = &app.state.database;
      let due = database
        .list_guests_due_notice(OffsetDateTime::now_utc(), &[14, 3], 10)
        .await
        .unwrap();
      let later = OffsetDateTime::from_unix_timestamp(expires_at / 1000).unwrap() + Duration::days(30);
      assert!(database.extend_guest_expiry(space_id, user, later).await.unwrap());
      assert!(!database.record_guest_notice(&due[0]).await.unwrap());
      app.stop().await;
    }
  }

  #[tokio::test]
  async fn changes_are_audited_and_those_concerned_are_told() {
    let app = app!();
    let (alice, ext) = (owner(), guest("ext"));
    let (space, ext_id, expires_at) = guest_ending_in(&app, &alice, &ext, 5).await;
    let device = register_device(&app, &ext).await;
    let mut socket = ws::connect(&app, &ext, &device).await;
    ws::events(&mut socket, StdDuration::from_millis(200)).await;

    let later = expires_at + 30 * 86_400_000;
    let path = format!("/spaces/{space}/members/{ext_id}");
    let reply = app
      .patch(&alice, &path, json!({"role": "guest", "expires_at": later}))
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let events = ws::events(&mut socket, StdDuration::from_millis(400)).await;
    assert!(
      events
        .iter()
        .any(|event| matches!(event, GatewayEvent::GuestTermsUpdate))
    );
    let audit = app.get(&admin(), "/admin/audit").await;
    assert!(audit.body.to_string().contains("guest_extended"));

    assert_eq!(app.delete(&alice, &path).await.status, StatusCode::NO_CONTENT);
    let events = ws::events(&mut socket, StdDuration::from_millis(400)).await;
    assert!(
      events
        .iter()
        .any(|event| matches!(event, GatewayEvent::GuestTermsUpdate))
    );
    app.stop().await;
  }

  /// A removed sponsor and a demoted admin see their list change while connected.
  #[tokio::test]
  async fn role_changes_and_removals_tell_the_person_concerned() {
    let app = app!();
    let (alice, carol) = (owner(), member("carol"));
    let carol_id = user_id(&app, &carol).await;
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    let path = format!("/spaces/{space}/members");
    let reply = app
      .post(&alice, &path, json!({"user_id": carol_id, "role": "space_admin"}))
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let device = register_device(&app, &carol).await;
    let mut socket = ws::connect(&app, &carol, &device).await;
    ws::events(&mut socket, StdDuration::from_millis(200)).await;
    let is_told = |events: &[GatewayEvent]| {
      events
        .iter()
        .any(|event| matches!(event, GatewayEvent::GuestTermsUpdate))
    };

    let member_path = format!("{path}/{carol_id}");
    let reply = app.patch(&alice, &member_path, json!({"role": "member"})).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(
      is_told(&ws::events(&mut socket, StdDuration::from_millis(400)).await),
      "demoted"
    );

    assert_eq!(
      app.delete(&alice, &member_path).await.status,
      StatusCode::NO_CONTENT
    );
    assert!(
      is_told(&ws::events(&mut socket, StdDuration::from_millis(400)).await),
      "removed"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn a_promoted_guest_keeps_no_expiry_and_a_patch_brings_nobody_back() {
    let app = app!();
    let (alice, carol) = (owner(), member("carol"));
    let (space, carol_id, _) = guest_ending_in(&app, &alice, &carol, 5).await;
    let path = format!("/spaces/{space}/members/{carol_id}");

    let reply = app.patch(&alice, &path, json!({"role": "member"})).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(reply.body["expires_at"].is_null(), "{}", reply.body);

    let (space_id, user) = (space.parse().unwrap(), carol_id.parse().unwrap());
    let database = &app.state.database;
    database.remove_space_member(space_id, user, false).await.unwrap();
    let role = teamcraft_proto::SpaceRole::Guest;
    let keep = teamcraft_db::ExpiryChange::Keep;
    assert!(
      !database
        .update_space_member(space_id, user, &role, keep)
        .await
        .unwrap()
    );
    assert_eq!(
      database.space_role(space_id, user).await.unwrap(),
      None,
      "still gone"
    );
    assert_eq!(
      app.patch(&alice, &path, json!({"role": "guest"})).await.status,
      StatusCode::NOT_FOUND
    );
    app.stop().await;
  }

  /// A custom scheme can give members `invite_guest`; that is no licence to demote people.
  mod demoting {
    use super::*;

    async fn in_a_space(app: &TestApp, alice: &TestUser) -> (String, String, String) {
      let (carol, dave) = (member("carol"), member("dave"));
      let (carol_id, dave_id) = (user_id(app, &carol).await, user_id(app, &dave).await);
      let space = create_space(app, alice, "Client", "invite_only").await;
      for (id, role) in [(&carol_id, "member"), (&dave_id, "space_admin")] {
        let reply = app
          .post(
            alice,
            &format!("/spaces/{space}/members"),
            json!({"user_id": id, "role": role}),
          )
          .await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
      }
      (space, carol_id, dave_id)
    }

    async fn grant_to_members(app: &TestApp, path: &str, permission: &str) {
      let reply = app
        .put(
          &admin(),
          path,
          json!({"changes": [{"role": "member", "permission": permission, "effect": "grant"}]}),
        )
        .await;
      assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    }

    #[tokio::test]
    async fn a_member_who_may_only_invite_guests_may_not_but_keeps_a_guests_terms() {
      let app = app!();
      let alice = owner();
      let (space, _, dave_id) = in_a_space(&app, &alice).await;
      let ext = guest("ext");
      let ext_id = user_id(&app, &ext).await;
      let reply = app
        .post(
          &alice,
          &format!("/spaces/{space}/members"),
          json!({"user_id": ext_id, "role": "guest"}),
        )
        .await;
      assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
      grant_to_members(
        &app,
        &format!("/admin/spaces/{space}/permission-scheme"),
        "invite_guest",
      )
      .await;

      let carol = member("carol");
      let reply = app
        .patch(
          &carol,
          &format!("/spaces/{space}/members/{dave_id}"),
          json!({"role": "guest"}),
        )
        .await;
      assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.body);
      let reply = app
        .patch(
          &carol,
          &format!("/spaces/{space}/members/{ext_id}"),
          json!({"role": "guest"}),
        )
        .await;
      assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
      app.stop().await;
    }

    #[tokio::test]
    async fn a_plain_member_and_a_guest_may_not() {
      let app = app!();
      let alice = owner();
      let (space, _, dave_id) = in_a_space(&app, &alice).await;
      let ext = guest("ext");
      let ext_id = user_id(&app, &ext).await;
      let reply = app
        .post(
          &alice,
          &format!("/spaces/{space}/members"),
          json!({"user_id": ext_id, "role": "guest"}),
        )
        .await;
      assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
      let path = format!("/spaces/{space}/members/{dave_id}");
      for who in [member("carol"), ext] {
        let reply = app.patch(&who, &path, json!({"role": "guest"})).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.body);
      }
      app.stop().await;
    }

    #[tokio::test]
    async fn a_space_admin_a_workspace_admin_and_a_grant_may() {
      let app = app!();
      let alice = owner();
      let (space, carol_id, _) = in_a_space(&app, &alice).await;
      let path = format!("/spaces/{space}/members/{carol_id}");
      for who in [member("dave"), admin()] {
        let reply = app.patch(&who, &path, json!({"role": "guest"})).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        let reply = app.patch(&alice, &path, json!({"role": "member"})).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
      }
      grant_to_members(&app, "/admin/permission-scheme", "demote_to_guest").await;
      let erin = member("erin");
      let erin_id = user_id(&app, &erin).await;
      let reply = app
        .post(
          &alice,
          &format!("/spaces/{space}/members"),
          json!({"user_id": erin_id, "role": "member"}),
        )
        .await;
      assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
      let reply = app.patch(&erin, &path, json!({"role": "guest"})).await;
      assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
      app.stop().await;
    }
  }

  #[tokio::test]
  async fn a_batch_of_notices_is_one_event() {
    let app = app!();
    let (alice, one, two) = (owner(), guest("one"), guest("two"));
    let device = register_device(&app, &alice).await;
    let (one_id, two_id) = (user_id(&app, &one).await, user_id(&app, &two).await);
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    let expires_at = (OffsetDateTime::now_utc() + Duration::days(2)).unix_timestamp() * 1000;
    for id in [one_id, two_id] {
      let reply = app
        .post(
          &alice,
          &format!("/spaces/{space}/members"),
          json!({"user_id": id, "role": "guest", "expires_at": expires_at}),
        )
        .await;
      assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    }
    let mut socket = ws::connect(&app, &alice, &device).await;
    ws::events(&mut socket, StdDuration::from_millis(200)).await;

    assert_eq!(
      send_notices_once(&app.state, OffsetDateTime::now_utc())
        .await
        .unwrap(),
      2
    );
    let events = ws::events(&mut socket, StdDuration::from_millis(400)).await;
    let updates = events
      .iter()
      .filter(|event| matches!(event, GatewayEvent::GuestTermsUpdate))
      .count();
    assert_eq!(updates, 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_deactivated_guest_gets_no_notice() {
    let app = app!();
    let (alice, ext) = (owner(), guest("ext"));
    let (_, ext_id, _) = guest_ending_in(&app, &alice, &ext, 2).await;
    let reply = app
      .post(&admin(), &format!("/admin/users/{ext_id}/deactivate"), json!({}))
      .await;
    assert!(reply.status.is_success(), "{}", reply.body);
    assert_eq!(
      send_notices_once(&app.state, OffsetDateTime::now_utc())
        .await
        .unwrap(),
      0
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn archiving_is_audited_and_posting_is_refused() {
    let app = app!();
    let alice = owner();
    let space = create_space(&app, &alice, "Client", "open").await;
    let channel = create_channel(&app, &alice, &space, "client", "public").await;
    let path = format!("/conversations/{channel}");
    let reply = app.patch(&alice, &path, json!({"is_archived": true})).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let device = register_device(&app, &alice).await;
    let refused = app
      .post(&alice, &format!("{path}/messages"), message_body(1, &device))
      .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    let reply = app.patch(&alice, &path, json!({"is_archived": false})).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let audit = app.get(&admin(), "/admin/audit").await.body.to_string();
    assert!(audit.contains("channel_archived"), "{audit}");
    assert!(audit.contains("channel_restored"), "{audit}");
    app.stop().await;
  }

  #[tokio::test]
  async fn the_overview_filters_and_is_for_admins() {
    let app = app!();
    let alice = owner();
    let ext = guest("ext");
    let (_, ext_id, _) = guest_ending_in(&app, &alice, &ext, 5).await;

    assert_eq!(
      app.get(&member("carol"), "/admin/guests").await.status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(app.get(&ext, "/admin/guests").await.status, StatusCode::FORBIDDEN);

    let all = app.get(&admin(), "/admin/guests").await;
    assert_eq!(all.status, StatusCode::OK, "{}", all.body);
    assert_eq!(all.body["guests"][0]["user"]["id"], ext_id);
    let soon = app.get(&admin(), "/admin/guests?filter=expiring_soon").await;
    assert_eq!(soon.body["guests"][0]["user"]["id"], ext_id);
    let unsponsored = app.get(&admin(), "/admin/guests?filter=no_sponsor").await;
    assert_eq!(unsponsored.body["guests"], json!([]), "alice sponsors");
    // By the sponsor's name, any case; wildcards are plain characters.
    let sponsored = app.get(&admin(), "/admin/guests?sponsor_query=ALIC").await;
    assert_eq!(
      sponsored.body["guests"][0]["user"]["id"], ext_id,
      "{}",
      sponsored.body
    );
    let nobody = app.get(&admin(), "/admin/guests?sponsor_query=zzz").await;
    assert_eq!(nobody.body["guests"], json!([]));
    for wildcard in ["_", "%25"] {
      let reply = app
        .get(&admin(), &format!("/admin/guests?sponsor_query={wildcard}"))
        .await;
      assert_eq!(reply.body["guests"], json!([]), "{wildcard} is literal");
    }
    let everyone = app.get(&admin(), "/admin/guests?sponsor_query=").await;
    assert_eq!(
      everyone.body["guests"][0]["user"]["id"], ext_id,
      "empty text searches nothing"
    );
    app.stop().await;
  }
}

mod expiry {
  use super::*;

  #[tokio::test]
  async fn expired_guests_leave_the_space_and_the_sponsor_is_told() {
    let app = app!();
    let alice = owner();
    let ext = guest("ext");
    let ext_id = user_id(&app, &ext).await;
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    let channel = create_channel(&app, &alice, &space, "client", "public").await;
    let reply = app
      .post(
        &alice,
        &format!("/spaces/{space}/members"),
        json!({"user_id": ext_id, "role": "guest"}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert!(reply.body["expires_at"].is_i64(), "default expiry applies");
    add_channel_member(&app, &alice, &channel, &ext_id).await;

    let device = register_device(&app, &alice).await;
    let mut socket = ws::connect(&app, &alice, &device).await;
    ws::events(&mut socket, StdDuration::from_millis(200)).await;

    // Nothing happens before the expiry.
    assert_eq!(
      expire_once(&app.state, OffsetDateTime::now_utc()).await.unwrap(),
      0
    );
    let later = OffsetDateTime::now_utc() + Duration::days(365);
    assert_eq!(expire_once(&app.state, later).await.unwrap(), 1);

    assert_eq!(
      app.get(&ext, &format!("/conversations/{channel}")).await.status,
      StatusCode::NOT_FOUND
    );
    let events = ws::events(&mut socket, StdDuration::from_millis(400)).await;
    assert!(events.iter().any(|event| matches!(
      event,
      GatewayEvent::GuestExpired { user, .. } if user.id.to_string() == ext_id
    )));
    let audit = app.get(&admin(), "/admin/audit").await;
    assert!(audit.body.to_string().contains("guest_expired"));
    app.stop().await;
  }

  #[tokio::test]
  async fn expiry_can_be_extended_and_cleared_but_only_for_guests() {
    let app = app!();
    let alice = owner();
    let (ext, carol) = (guest("ext"), member("carol"));
    let (ext_id, carol_id) = (user_id(&app, &ext).await, user_id(&app, &carol).await);
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    add_space_member(&app, &alice, &space, &ext_id, "guest").await;
    add_space_member(&app, &alice, &space, &carol_id, "member").await;

    let soon = (OffsetDateTime::now_utc() + Duration::days(1)).unix_timestamp() * 1000;
    let path = format!("/spaces/{space}/members/{ext_id}");
    let reply = app
      .patch(&alice, &path, json!({"role": "guest", "expires_at": soon}))
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["expires_at"], soon);
    let reply = app
      .patch(&alice, &path, json!({"role": "guest", "clears_expiry": true}))
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(reply.body["expires_at"].is_null());

    let far = OffsetDateTime::now_utc() + Duration::days(3650);
    assert_eq!(
      expire_once(&app.state, far).await.unwrap(),
      0,
      "cleared expiry never expires"
    );

    let reply = app
      .patch(
        &alice,
        &format!("/spaces/{space}/members/{carol_id}"),
        json!({"role": "member", "expires_at": soon}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    app.stop().await;
  }

  #[tokio::test]
  async fn invited_guests_get_the_default_expiry() {
    let app = app!();
    let alice = owner();
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    let invite = app
      .post(
        &alice,
        &format!("/spaces/{space}/invites"),
        json!({"email": "ext@example.com", "role": "guest"}),
      )
      .await;
    assert_eq!(invite.status, StatusCode::CREATED, "{}", invite.body);
    let ext = guest("ext").with_email("ext@example.com", true);
    assert_eq!(app.get(&ext, "/spaces").await.status, StatusCode::OK);
    let members = app.get(&alice, &format!("/spaces/{space}/members")).await;
    let entry = members.body["members"]
      .as_array()
      .unwrap()
      .iter()
      .find(|member| member["role"] == "guest")
      .expect("guest joined");
    assert!(entry["expires_at"].is_i64(), "{entry}");
    app.stop().await;
  }
}

mod domains {
  use super::*;

  #[tokio::test]
  async fn guest_invites_respect_the_allowed_domains() {
    let Some(app) = TestApp::start_with(|settings| {
      settings.guests.allowed_email_domains = vec!["partner.test".into()];
    })
    .await
    else {
      return;
    };
    let alice = owner();
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    let path = format!("/spaces/{space}/invites");
    for (email, role, status) in [
      ("x@partner.test", "guest", StatusCode::CREATED),
      ("x@team.partner.test", "guest", StatusCode::CREATED),
      ("x@notpartner.test", "guest", StatusCode::BAD_REQUEST),
      ("x@other.test", "guest", StatusCode::BAD_REQUEST),
      ("y@other.test", "member", StatusCode::CREATED),
    ] {
      let reply = app
        .post(&alice, &path, json!({"email": email, "role": role}))
        .await;
      assert_eq!(reply.status, status, "{email}: {}", reply.body);
    }
    app.stop().await;
  }
}

mod mail {
  use std::sync::Arc;

  use teamcraft_server::testing::FakeMailer;

  use super::*;

  #[tokio::test]
  async fn invites_are_mailed_when_a_mail_server_is_configured() {
    let mailer = Arc::new(FakeMailer::default());
    let Some(app) = TestApp::start_with_mailer(
      |settings| settings.mail.public_base_url = "https://chat.example.com".into(),
      mailer.clone(),
    )
    .await
    else {
      return;
    };
    let alice = owner();
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    let reply = app
      .post(
        &alice,
        &format!("/spaces/{space}/invites"),
        json!({"email": "ext@example.com", "role": "guest"}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);

    let sent = mailer.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to_address, "ext@example.com");
    assert!(sent[0].subject.contains("Client"), "{}", sent[0].subject);
    assert!(sent[0].text.contains("https://chat.example.com"));
    assert!(sent[0].text.contains("guest"));

    // A refusing mail server does not undo the invite.
    mailer.refuse("other@example.com");
    let reply = app
      .post(
        &alice,
        &format!("/spaces/{space}/invites"),
        json!({"email": "other@example.com", "role": "member"}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    app.stop().await;
  }
}
