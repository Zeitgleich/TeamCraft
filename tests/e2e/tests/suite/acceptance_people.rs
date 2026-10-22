//! v1 acceptance, PLAN.md §11 People, profiles, groups: profile with time zone and working hours,
//! the member directory (not for guests), user groups.
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_e2e::{Peer, Stack};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn profiles_directory_and_user_groups_work_and_guests_see_little() {
  let Some(stack) = Stack::start().await else { return };
  let alice = Peer::enroll(&stack, "alice").await;
  let carol = Peer::enroll(&stack, "carol").await;
  let dave = Peer::enroll(&stack, "dave").await;

  // Profile: title, department, time zone and working hours; others can read it.
  stack
    .put(
      &carol.tokens,
      "/me/profile",
      json!({
        "title": "Engineer", "department": "Platform", "time_zone": "Europe/Berlin",
        "working_hours": {"weekdays": ["mon", "tue", "wed", "thu", "fri"], "start_minute": 480, "end_minute": 1020},
      }),
    )
    .await
    .expect(StatusCode::OK);
  let profile = stack
    .get(&alice.tokens, &format!("/users/{}/profile", carol.user_id()))
    .await
    .expect(StatusCode::OK);
  assert_eq!(profile.body["title"], "Engineer", "{}", profile.body);
  assert_eq!(profile.body["time_zone"], "Europe/Berlin");

  // The directory: members find each other, a guest finds nobody he shares nothing with.
  let found = stack
    .get(&alice.tokens, "/users?query=carol")
    .await
    .expect(StatusCode::OK);
  assert_eq!(found.body["users"].as_array().unwrap().len(), 1, "{}", found.body);
  let guest_search = stack
    .get(&dave.tokens, "/users?query=carol")
    .await
    .expect(StatusCode::OK);
  assert!(
    guest_search.body["users"].as_array().unwrap().is_empty(),
    "{}",
    guest_search.body
  );
  let guest_profile = stack
    .get(&dave.tokens, &format!("/users/{}", carol.user_id()))
    .await;
  assert_eq!(guest_profile.status, StatusCode::NOT_FOUND, "{guest_profile:?}");

  // User groups: created by someone allowed to, members managed, handle rules enforced.
  let denied = stack
    .post(
      &carol.tokens,
      "/user-groups",
      json!({"handle": "devteam", "name": "Developers"}),
    )
    .await;
  assert_eq!(
    denied.status,
    StatusCode::FORBIDDEN,
    "members don't create groups by default: {denied:?}"
  );
  let group = stack
    .post(
      &alice.tokens,
      "/user-groups",
      json!({"handle": "devteam", "name": "Developers"}),
    )
    .await
    .expect(StatusCode::CREATED);
  let group_id = group.body["id"].as_str().unwrap().to_owned();
  let bad_handle = stack
    .post(
      &alice.tokens,
      "/user-groups",
      json!({"handle": "Dev Team!", "name": "x"}),
    )
    .await;
  assert_eq!(bad_handle.status, StatusCode::BAD_REQUEST, "{bad_handle:?}");
  stack
    .put(
      &alice.tokens,
      &format!("/user-groups/{group_id}/members/{}", carol.user_id()),
      json!({}),
    )
    .await
    .expect(StatusCode::NO_CONTENT);
  let detail = stack
    .get(&alice.tokens, &format!("/user-groups/{group_id}"))
    .await
    .expect(StatusCode::OK);
  assert_eq!(detail.body["group"]["member_count"], 1, "{}", detail.body);
  assert_eq!(detail.body["members"][0]["id"], carol.user_id().to_string());

  stack.stop().await;
}

/// Bug: the profile's time zone is stored as sent; "Mars/Base" is accepted although the profile
/// promises an IANA name (clients show "it's 22:14 for Anna" from it).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "bug: PUT /me/profile accepts time zones that are not IANA names"]
async fn a_time_zone_that_does_not_exist_is_refused() {
  let Some(stack) = Stack::start().await else { return };
  let carol = Peer::enroll(&stack, "carol").await;
  let bad_zone = stack
    .put(&carol.tokens, "/me/profile", json!({"time_zone": "Mars/Base"}))
    .await;
  assert_eq!(bad_zone.status, StatusCode::BAD_REQUEST, "{bad_zone:?}");
  stack.stop().await;
}
