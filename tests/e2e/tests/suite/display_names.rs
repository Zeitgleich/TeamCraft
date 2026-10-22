//! The directory policy (display name format, full name and e-mail privacy) in every response that
//! names a person: directory, members, /me, meeting attendees and the invitation mail.
use std::time::Duration;

use reqwest::StatusCode;
use serde_json::json;
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Mailpit, Peer, Stack};
use teamcraft_proto::DisplayNameFormat;

const STARTS_AT: i64 = 1_893_488_400_000;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_directory_policy_applies_to_every_response_that_names_a_person() {
  let Some(stack) = Stack::builder().with_meetings().start().await else {
    return;
  };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let mut dave = Peer::enroll(&stack, "dave").await;
  let space = space_with(
    &stack,
    &alice,
    "Project A",
    &[(&carol, "member"), (&dave, "guest")],
  )
  .await;
  let channel = channel_with(&mut alice, space, "team", &mut [&mut carol, &mut dave]).await;
  let carol_id = carol.user_id().to_string();
  let guest_address = format!("gus-{}@example.org", alice.client.device_id());
  let meeting = stack
    .post(
      &alice.tokens,
      "/meetings",
      json!({
        "title": "Review", "starts_at": STARTS_AT, "ends_at": STARTS_AT + 3_600_000,
        "invitees": [
          {"kind": "user", "user_id": carol_id},
          {"kind": "external", "email": guest_address, "name": "Gus"},
        ],
      }),
    )
    .await
    .expect(StatusCode::CREATED);
  let meeting_id = meeting.id();

  // Every place a person's name appears, as seen by `viewer`.
  let names_of_carol = |viewer: teamcraft_e2e::UserTokens, is_guest: bool| {
    let (stack, carol_id, meeting_id) = (&stack, carol_id.clone(), meeting_id.clone());
    async move {
      let mut names = Vec::new();
      let user = stack
        .get(&viewer, &format!("/users/{carol_id}"))
        .await
        .expect(StatusCode::OK);
      names.push(("user", user.body["display_name"].as_str().unwrap().to_owned()));
      let search = stack
        .get(&viewer, "/users?query=carol")
        .await
        .expect(StatusCode::OK);
      names.push((
        "search",
        search.body["users"][0]["display_name"]
          .as_str()
          .unwrap()
          .to_owned(),
      ));
      let members = stack
        .get(&viewer, &format!("/conversations/{channel}/members"))
        .await
        .expect(StatusCode::OK);
      let member = members.body["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["user"]["id"] == carol_id.as_str())
        .unwrap();
      names.push((
        "conversation members",
        member["user"]["display_name"].as_str().unwrap().to_owned(),
      ));
      if is_guest {
        // Guests see neither the space's members nor the meeting.
        return names;
      }
      let space_members = stack
        .get(&viewer, &format!("/spaces/{space}/members"))
        .await
        .expect(StatusCode::OK);
      let member = space_members.body["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["user"]["id"] == carol_id.as_str())
        .unwrap();
      names.push((
        "space members",
        member["user"]["display_name"].as_str().unwrap().to_owned(),
      ));
      let meeting = stack.get(&viewer, &format!("/meetings/{meeting_id}")).await;
      if meeting.status == StatusCode::OK {
        let attendee = meeting.body["attendees"]
          .as_array()
          .unwrap()
          .iter()
          .find(|attendee| attendee["user_id"] == carol_id.as_str())
          .unwrap();
        names.push(("meeting attendees", attendee["name"].as_str().unwrap().to_owned()));
      }
      names
    }
  };
  let assert_all = |names: Vec<(&str, String)>, expected: &str| {
    for (place, name) in names {
      assert_eq!(name, expected, "{place}");
    }
  };

  // Default: the name as the identity provider delivers it.
  assert_all(names_of_carol(alice.tokens.clone(), false).await, "Carol Member");

  // "Last, First".
  alice
    .set_policy(|policy| policy.directory.display_name_format = DisplayNameFormat::LastFirst)
    .await;
  assert_all(names_of_carol(alice.tokens.clone(), false).await, "Member, Carol");
  assert_all(names_of_carol(dave.tokens.clone(), true).await, "Member, Carol");
  let me = stack.get(&carol.tokens, "/me").await.expect(StatusCode::OK);
  assert_eq!(me.body["display_name"], "Member, Carol", "{}", me.body);

  // The meeting mail names the organizer in the same format, never the raw name.
  stack
    .patch(
      &alice.tokens,
      &format!("/meetings/{meeting_id}"),
      json!({"title": "Review (moved)"}),
    )
    .await
    .expect(StatusCode::OK);
  let mail = Mailpit::new()
    .wait_for_mail_to(&guest_address, Duration::from_secs(20))
    .await;
  assert!(mail.raw.contains("Owner"), "{}", mail.raw);
  assert!(
    !mail.raw.contains("Alice Owner"),
    "the raw name is in the mail:\n{}",
    mail.raw
  );

  // Full names off: only the username.
  alice
    .set_policy(|policy| policy.directory.shows_full_name = false)
    .await;
  assert_all(names_of_carol(alice.tokens.clone(), false).await, "carol");
  let me = stack.get(&carol.tokens, "/me").await.expect(StatusCode::OK);
  assert_eq!(me.body["display_name"], "carol", "{}", me.body);

  // E-mail addresses: members see them when the policy says so, guests never.
  alice
    .set_policy(|policy| {
      policy.directory.shows_full_name = true;
      policy.directory.display_name_format = DisplayNameFormat::AsProvided;
      policy.directory.shows_email = true;
    })
    .await;
  let for_member = stack
    .get(&alice.tokens, &format!("/users/{carol_id}"))
    .await
    .expect(StatusCode::OK);
  assert_eq!(
    for_member.body["email"], "carol@example.com",
    "{}",
    for_member.body
  );
  let for_guest = stack
    .get(&dave.tokens, &format!("/users/{carol_id}"))
    .await
    .expect(StatusCode::OK);
  assert!(
    for_guest.body.get("email").is_none(),
    "a guest saw an e-mail address: {}",
    for_guest.body
  );

  stack.stop().await;
}
