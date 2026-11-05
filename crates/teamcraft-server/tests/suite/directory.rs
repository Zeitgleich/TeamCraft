//! The directory tells who hasn't set up the app yet (`has_devices`), for colleagues the caller
//! shares nothing with too.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use serde_json::{Value, json};
use teamcraft_server::testing::TestApp;

#[tokio::test]
async fn the_directory_says_who_has_a_device() {
  let Some(app) = TestApp::start().await else { return };
  let (carol, dave, erin) = (member("carol"), member("dave"), member("erin"));
  // Carol shares no space or conversation with either of them.
  user_id(&app, &carol).await;
  let dave_id = user_id(&app, &dave).await;
  let erin_id = user_id(&app, &erin).await;
  register_device(&app, &erin).await;

  let found = app.get(&carol, "/users?query=").await;
  let has_devices = |name: &str| {
    found.body["users"]
      .as_array()
      .unwrap()
      .iter()
      .find(|user| user["username"] == name)
      .map(|user| user["has_devices"].clone())
  };
  assert_eq!(has_devices("dave"), Some(json!(false)));
  assert_eq!(has_devices("erin"), Some(json!(true)));

  assert_eq!(
    app.get(&carol, &format!("/users/{dave_id}")).await.body["has_devices"],
    json!(false)
  );
  assert_eq!(
    app.get(&carol, &format!("/users/{erin_id}")).await.body["has_devices"],
    Value::Bool(true)
  );
  app.stop().await;
}

#[tokio::test]
async fn the_query_narrows_the_directory_and_special_characters_match_literally() {
  let Some(app) = TestApp::start().await else { return };
  let (carol, dave) = (member("carol"), member("dave"));
  user_id(&app, &carol).await;
  user_id(&app, &dave).await;

  let found = app.get(&carol, "/users?query=dav").await;
  let names: Vec<&str> = found.body["users"]
    .as_array()
    .unwrap()
    .iter()
    .filter_map(|user| user["username"].as_str())
    .collect();
  assert_eq!(names, vec!["dave"]);
  // `%` is a LIKE wildcard in SQL; here it is only a character nobody's name contains.
  let none = app.get(&carol, "/users?query=%25").await;
  assert_eq!(none.body["users"], json!([]));
  app.stop().await;
}

mod pages {
  use super::*;

  #[tokio::test]
  async fn people_come_in_pages_with_profile_filters_and_without_bots() {
    let Some(app) = TestApp::start_with(|settings| settings.limits.max_directory_results = 2).await else {
      return;
    };
    let carol = member("carol");
    user_id(&app, &carol).await;
    for (name, department) in [("dora", "Sales"), ("emil", "Sales"), ("fred", "Finance")] {
      let person = member(name);
      user_id(&app, &person).await;
      let saved = app
        .put(
          &person,
          "/me/profile",
          json!({"department": department, "title": "Lead"}),
        )
        .await;
      assert_eq!(saved.status, reqwest::StatusCode::OK, "{}", saved.body);
    }

    let mut seen = Vec::new();
    let mut path = "/directory/people?query=".to_owned();
    loop {
      let page = app.get(&carol, &path).await;
      let people = page.body["people"].as_array().unwrap();
      assert!(people.len() <= 2);
      seen.extend(people.iter().map(|person| person["user"]["id"].clone()));
      let Some(next) = page.body["next_after"].as_str() else {
        break;
      };
      path = format!("/directory/people?query=&after={next}");
    }
    let unique: std::collections::HashSet<_> = seen.iter().map(ToString::to_string).collect();
    assert_eq!(unique.len(), seen.len(), "no one twice across pages");

    let sales = app.get(&carol, "/directory/people?department=Sales").await;
    assert_eq!(names(&sales.body), vec!["dora", "emil"]);
    assert_eq!(sales.body["people"][0]["department"], json!("Sales"));
    assert_eq!(sales.body["people"][0]["title"], json!("Lead"));
    assert_eq!(
      names(&app.get(&carol, "/directory/people?query=finance").await.body),
      vec!["fred"]
    );

    let filters = app.get(&carol, "/directory/filters").await;
    assert_eq!(filters.body["departments"], json!(["Finance", "Sales"]));
    assert_eq!(filters.body["titles"], json!(["Lead"]));
    app.stop().await;
  }

  #[tokio::test]
  async fn the_external_tab_lists_guests_and_guests_see_only_who_they_share_with() {
    let Some(app) = TestApp::start().await else { return };
    let (alice, carol, ext, other) = (owner(), member("carol"), guest("ext"), guest("other"));
    let carol_id = user_id(&app, &carol).await;
    let other_id = user_id(&app, &other).await;
    let ext_id = user_id(&app, &ext).await;
    let space = create_space(&app, &alice, "Team", "invite_only").await;
    let channel = create_channel(&app, &alice, &space, "shared", "private").await;
    add_space_member(&app, &alice, &space, &carol_id, "member").await;
    add_channel_member(&app, &alice, &channel, &carol_id).await;
    add_space_member(&app, &alice, &space, &ext_id, "guest").await;
    add_channel_member(&app, &alice, &channel, &ext_id).await;

    let external = app.get(&carol, "/directory/people?kind=guest").await;
    assert_eq!(names(&external.body), vec!["ext", "other"]);

    let as_guest = app.get(&ext, "/directory/people").await;
    let ids = ids(&as_guest.body);
    assert!(ids.contains(&carol_id) && !ids.contains(&other_id), "{ids:?}");
    assert_eq!(
      app.get(&ext, "/directory/filters").await.body,
      json!({"organizations": [], "departments": [], "titles": []})
    );
    assert_eq!(
      app.get(&ext, "/directory/channels").await.status,
      reqwest::StatusCode::FORBIDDEN
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn a_space_scope_needs_the_right_to_see_its_roster() {
    let Some(app) = TestApp::start().await else { return };
    let (alice, carol, dora, ext) = (owner(), member("carol"), member("dora"), guest("ext"));
    let carol_id = user_id(&app, &carol).await;
    user_id(&app, &dora).await;
    let ext_id = user_id(&app, &ext).await;
    let space = create_space(&app, &alice, "Team", "invite_only").await;
    add_space_member(&app, &alice, &space, &carol_id, "member").await;
    add_space_member(&app, &alice, &space, &ext_id, "guest").await;

    let path = format!("/directory/people?space_id={space}");
    assert!(ids(&app.get(&carol, &path).await.body).contains(&carol_id));
    assert_eq!(app.get(&dora, &path).await.status, reqwest::StatusCode::NOT_FOUND);
    assert_eq!(app.get(&ext, &path).await.status, reqwest::StatusCode::FORBIDDEN);
    app.stop().await;
  }

  #[tokio::test]
  async fn channels_span_the_callers_spaces_and_keep_private_ones_hidden() {
    let Some(app) = TestApp::start().await else { return };
    let (alice, carol) = (owner(), member("carol"));
    let carol_id = user_id(&app, &carol).await;
    let team = create_space(&app, &alice, "Team", "invite_only").await;
    let closed = create_space(&app, &alice, "Closed", "invite_only").await;
    add_space_member(&app, &alice, &team, &carol_id, "member").await;
    let open = create_channel(&app, &alice, &team, "open-one", "public").await;
    let hidden = create_channel(&app, &alice, &team, "hidden-one", "private").await;
    let elsewhere = create_channel(&app, &alice, &closed, "elsewhere", "public").await;

    let found = app.get(&carol, "/directory/channels").await;
    let channels: Vec<&str> = found.body["channels"]
      .as_array()
      .unwrap()
      .iter()
      .filter_map(|channel| channel["id"].as_str())
      .collect();
    assert!(channels.contains(&open.as_str()), "{}", found.body);
    assert!(!channels.contains(&hidden.as_str()) && !channels.contains(&elsewhere.as_str()));
    assert_eq!(found.body["channels"][0]["space_name"], json!("Team"));
    app.stop().await;
  }

  fn names(body: &Value) -> Vec<&str> {
    body["people"]
      .as_array()
      .unwrap()
      .iter()
      .filter_map(|person| person["user"]["username"].as_str())
      .collect()
  }

  fn ids(body: &Value) -> Vec<String> {
    body["people"]
      .as_array()
      .unwrap()
      .iter()
      .filter_map(|person| person["user"]["id"].as_str().map(str::to_owned))
      .collect()
  }
}

/// Who may see, resend and revoke an invitation, per role.
mod invitations {
  use std::sync::Arc;

  use reqwest::StatusCode;
  use teamcraft_server::testing::{FakeMailer, TestUser};

  use super::*;

  struct Fixture {
    app: TestApp,
    mailer: Arc<FakeMailer>,
    space: String,
    /// Sent by the space owner.
    owners_member: String,
    /// Sent by `sponsor`, a space admin demoted to member by the test that needs it.
    sponsors_guest: String,
  }

  async fn fixture() -> Option<(Fixture, TestUser, TestUser, TestUser, TestUser, TestUser)> {
    let mailer = Arc::new(FakeMailer::default());
    let app = TestApp::start_with_mailer(
      |settings| settings.limits.invite_resend_interval_secs = 0,
      mailer.clone(),
    )
    .await?;
    let (alice, sponsor, plain, outsider, ext) = (
      owner(),
      member("sponsor"),
      member("plain"),
      member("outsider"),
      guest("ext"),
    );
    let sponsor_id = user_id(&app, &sponsor).await;
    let plain_id = user_id(&app, &plain).await;
    user_id(&app, &outsider).await;
    let ext_id = user_id(&app, &ext).await;
    let space = create_space(&app, &alice, "Team", "invite_only").await;
    add_space_member(&app, &alice, &space, &sponsor_id, "space_admin").await;
    add_space_member(&app, &alice, &space, &plain_id, "member").await;
    add_space_member(&app, &alice, &space, &ext_id, "guest").await;
    let invite = |by: &TestUser, email: &str, role: &str| {
      let (by, email, role, path) = (
        by.clone(),
        email.to_owned(),
        role.to_owned(),
        format!("/spaces/{space}/invites"),
      );
      let app = &app;
      async move {
        let reply = app.post(&by, &path, json!({"email": email, "role": role})).await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
        reply.id()
      }
    };
    let owners_member = invite(&alice, "m@example.org", "member").await;
    let sponsors_guest = invite(&sponsor, "g@example.org", "guest").await;
    Some((
      Fixture {
        app,
        mailer,
        space,
        owners_member,
        sponsors_guest,
      },
      alice,
      sponsor,
      plain,
      outsider,
      ext,
    ))
  }

  fn listed(body: &Value) -> Vec<&str> {
    body["invitations"]
      .as_array()
      .unwrap()
      .iter()
      .filter_map(|invitation| invitation["email"].as_str())
      .collect()
  }

  #[tokio::test]
  async fn the_list_shows_each_role_what_it_may_manage() {
    let Some((f, alice, sponsor, plain, outsider, ext)) = fixture().await else {
      return;
    };
    let all = vec!["g@example.org", "m@example.org"];
    let list = |user: TestUser| {
      let app = &f.app;
      async move { app.get(&user, "/directory/invitations").await }
    };
    assert_eq!(listed(&list(alice.clone()).await.body), all, "owner");
    assert_eq!(listed(&list(sponsor.clone()).await.body), all, "space admin");
    assert!(listed(&list(plain.clone()).await.body).is_empty(), "plain member");
    assert!(
      listed(&list(outsider.clone()).await.body).is_empty(),
      "not in the space"
    );
    assert_eq!(list(ext.clone()).await.status, StatusCode::FORBIDDEN, "guest");
    let first = list(alice).await;
    assert_eq!(first.body["invitations"][0]["can_resend"], json!(true));
    f.app.stop().await;
  }

  #[tokio::test]
  async fn a_sponsor_who_lost_the_right_to_invite_no_longer_manages_their_invitations() {
    let Some((f, alice, sponsor, ..)) = fixture().await else {
      return;
    };
    let sponsor_id = user_id(&f.app, &sponsor).await;
    let reply = f
      .app
      .patch(
        &alice,
        &format!("/spaces/{}/members/{sponsor_id}", f.space),
        json!({"role": "member"}),
      )
      .await;
    assert!(reply.status.is_success(), "{}", reply.body);
    // A plain member may not invite guests in the built-in scheme: the invitation stays with the
    // admins. The case where the scheme lets them is covered by `invites.rs`'s unit tests.
    assert!(listed(&f.app.get(&sponsor, "/directory/invitations").await.body).is_empty());
    let own = f
      .app
      .post(
        &sponsor,
        &format!("/spaces/{}/invites/{}/resend", f.space, f.sponsors_guest),
        json!({}),
      )
      .await;
    assert_eq!(own.status, StatusCode::FORBIDDEN, "{}", own.body);
    assert_eq!(
      listed(&f.app.get(&alice, "/directory/invitations").await.body).len(),
      2,
      "the owner still has both"
    );
    f.app.stop().await;
  }

  #[tokio::test]
  async fn resend_and_revoke_are_refused_per_role() {
    let Some((f, alice, _sponsor, plain, outsider, ext)) = fixture().await else {
      return;
    };
    let resend = format!("/spaces/{}/invites/{}/resend", f.space, f.owners_member);
    let revoke = format!("/spaces/{}/invites/{}", f.space, f.owners_member);
    let before = f.mailer.sent().len();
    for (who, status) in [
      (&plain, StatusCode::FORBIDDEN),
      (&outsider, StatusCode::NOT_FOUND),
      (&ext, StatusCode::FORBIDDEN),
    ] {
      assert_eq!(f.app.post(who, &resend, json!({})).await.status, status);
      assert_eq!(f.app.delete(who, &revoke).await.status, status);
    }
    assert_eq!(f.mailer.sent().len(), before, "no mail for a refused resend");

    let done = f.app.post(&alice, &resend, json!({})).await;
    assert_eq!(done.status, StatusCode::NO_CONTENT, "{}", done.body);
    assert_eq!(f.mailer.sent().len(), before + 1);
    assert_eq!(f.app.delete(&alice, &revoke).await.status, StatusCode::NO_CONTENT);
    assert_eq!(
      f.app.post(&alice, &resend, json!({})).await.status,
      StatusCode::NOT_FOUND,
      "a revoked invitation is gone"
    );
    f.app.stop().await;
  }

  #[tokio::test]
  async fn resending_is_limited_and_needs_a_valid_invitation() {
    let mailer = Arc::new(FakeMailer::default());
    let Some(app) = TestApp::start_with_mailer(
      |settings| settings.limits.invite_resend_interval_secs = 3600,
      mailer.clone(),
    )
    .await
    else {
      return;
    };
    let alice = owner();
    let space = create_space(&app, &alice, "Team", "invite_only").await;
    let reply = app
      .post(
        &alice,
        &format!("/spaces/{space}/invites"),
        json!({"email": "m@example.org", "role": "member"}),
      )
      .await;
    let path = format!("/spaces/{space}/invites/{}/resend", reply.id());
    // The invitation's own mail just went out.
    let limited = app.post(&alice, &path, json!({})).await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS, "{}", limited.body);
    // A mail that could not be sent doesn't count: the next try need not wait.
    app
      .state
      .database
      .forget_invite_resend(reply.id().parse().unwrap())
      .await
      .unwrap();
    let before = mailer.sent().len();
    let sent = app.post(&alice, &path, json!({})).await;
    assert_eq!(sent.status, StatusCode::NO_CONTENT, "{}", sent.body);
    assert_eq!(mailer.sent().len(), before + 1);
    assert_eq!(
      app.post(&alice, &path, json!({})).await.status,
      StatusCode::TOO_MANY_REQUESTS,
      "two clicks send one mail"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn a_mail_server_refusing_the_mail_is_the_callers_answer() {
    let mailer = Arc::new(FakeMailer::default());
    mailer.refuse("m@example.org");
    let Some(app) = TestApp::start_with_mailer(
      |settings| settings.limits.invite_resend_interval_secs = 0,
      mailer.clone(),
    )
    .await
    else {
      return;
    };
    let alice = owner();
    let space = create_space(&app, &alice, "Team", "invite_only").await;
    let reply = app
      .post(
        &alice,
        &format!("/spaces/{space}/invites"),
        json!({"email": "m@example.org", "role": "member"}),
      )
      .await;
    assert_eq!(
      reply.status,
      StatusCode::CREATED,
      "the invite stands without its mail"
    );
    let path = format!("/spaces/{space}/invites/{}/resend", reply.id());
    let refused = app.post(&alice, &path, json!({})).await;
    assert_eq!(
      refused.status,
      StatusCode::SERVICE_UNAVAILABLE,
      "{}",
      refused.body
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn without_a_mail_server_nothing_offers_or_accepts_a_resend() {
    let Some(app) = TestApp::start().await else { return };
    let alice = owner();
    let space = create_space(&app, &alice, "Team", "invite_only").await;
    let reply = app
      .post(
        &alice,
        &format!("/spaces/{space}/invites"),
        json!({"email": "m@example.org", "role": "member"}),
      )
      .await;
    let list = app.get(&alice, "/directory/invitations").await;
    assert_eq!(list.body["invitations"][0]["can_resend"], json!(false));
    let refused = app
      .post(
        &alice,
        &format!("/spaces/{space}/invites/{}/resend", reply.id()),
        json!({}),
      )
      .await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.body);
    app.stop().await;
  }
}
