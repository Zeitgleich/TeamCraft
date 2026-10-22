//! The e-mail digest of missed mentions: opt-in, batched, content-free, and only for mentions that
//! are still unread.
use std::collections::HashSet;
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use serde_json::json;
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Mailpit, Peer, ReceivedMail, Stack};

/// The realm's address of carol and erin; every run sends to the same mailbox.
const CAROL: &str = "carol@example.com";
const ERIN: &str = "erin@example.com";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missed_mentions_arrive_as_one_content_free_mail_to_people_who_opted_in() {
  let Some(stack) = Stack::builder()
    .with_mail()
    .with_env("TEAMCRAFT_NOTIFICATIONS__EMAIL_DIGEST__IS_ENABLED", "true")
    .with_env(
      "TEAMCRAFT_NOTIFICATIONS__EMAIL_DIGEST__DEFAULT_INTERVAL_SECS",
      "5",
    )
    .with_env("TEAMCRAFT_NOTIFICATIONS__EMAIL_DIGEST__MIN_INTERVAL_SECS", "120")
    .with_env("TEAMCRAFT_NOTIFICATIONS__EMAIL_DIGEST__GRACE_SECS", "0")
    .with_env(
      "TEAMCRAFT_NOTIFICATIONS__EMAIL_DIGEST__SCHEDULER_INTERVAL_SECS",
      "1",
    )
    .start()
    .await
  else {
    return;
  };
  let mailpit = Mailpit::new();
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let mut erin = Peer::enroll(&stack, "erin").await;
  let space = space_with(
    &stack,
    &alice,
    "Project A",
    &[(&carol, "member"), (&erin, "member")],
  )
  .await;
  let channel = channel_with(
    &mut alice,
    space,
    "secret-launch-plans",
    &mut [&mut carol, &mut erin],
  )
  .await;
  let seen_before: HashSet<String> = mailpit
    .all_to(CAROL)
    .await
    .into_iter()
    .chain(mailpit.all_to(ERIN).await)
    .map(|mail| mail.id)
    .collect();
  let new_mails = |address: &'static str| {
    let (mailpit, seen) = (&mailpit, &seen_before);
    async move {
      mailpit
        .all_to(address)
        .await
        .into_iter()
        .filter(|mail| !seen.contains(&mail.id) && is_digest(mail))
        .collect::<Vec<ReceivedMail>>()
    }
  };

  // Off by default; the interval can't go below what the instance allows.
  let initial = stack
    .get(&carol.tokens, "/me/email-digest")
    .await
    .expect(StatusCode::OK);
  assert_eq!(initial.body["is_enabled"], false, "{}", initial.body);
  assert_eq!(initial.body["is_available"], true, "{}", initial.body);
  assert_eq!(initial.body["min_interval_minutes"], 2, "{}", initial.body);
  let too_often = stack
    .put(
      &carol.tokens,
      "/me/email-digest",
      json!({"is_enabled": true, "interval_minutes": 1}),
    )
    .await;
  assert_eq!(too_often.status, StatusCode::BAD_REQUEST, "{too_often:?}");
  let enabled = stack
    .put(&carol.tokens, "/me/email-digest", json!({"is_enabled": true}))
    .await
    .expect(StatusCode::OK);
  assert_eq!(enabled.body["is_enabled"], true, "{}", enabled.body);

  // Three mentions of both while they are away: Carol opted in, Erin didn't.
  for text in ["the launch moves to May", "budget is 12000", "call me"] {
    alice
      .client
      .send_text_mentioning(channel, None, text, vec![carol.user_id(), erin.user_id()])
      .await
      .unwrap();
  }
  let deadline = Instant::now() + Duration::from_secs(40);
  let mails = loop {
    let mails = new_mails(CAROL).await;
    let counted: u32 = mails.iter().map(|mail| count_in(&mail.subject)).sum();
    if counted >= 3 {
      break mails;
    }
    assert!(
      Instant::now() < deadline,
      "the digest never counted all mentions: {mails:?}"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
  };
  assert!(
    mails.len() <= 2,
    "one mail per mention instead of a batch: {mails:?}"
  );
  assert_eq!(
    mails.iter().map(|mail| count_in(&mail.subject)).sum::<u32>(),
    3,
    "{mails:?}"
  );
  // Content-free: not who, not where, not what.
  for mail in &mails {
    let everything = format!("{}\n{}\n{}", mail.subject, mail.text, mail.raw).to_lowercase();
    for secret in [
      "alice",
      "launch",
      "secret-launch-plans",
      "may",
      "12000",
      "call me",
    ] {
      // "may" would also match "Date: ... May"; only check it in subject and text.
      let haystack = if secret == "may" {
        format!("{}\n{}", mail.subject, mail.text).to_lowercase()
      } else {
        everything.clone()
      };
      assert!(
        !haystack.contains(secret),
        "{secret:?} leaked into the digest:\n{}",
        mail.raw
      );
    }
    assert!(
      mail.text.contains(&stack.base_url),
      "no link to the app:\n{}",
      mail.text
    );
  }
  assert!(new_mails(ERIN).await.is_empty(), "Erin never opted in");

  // A mention that is read in time isn't mailed.
  let sent = new_mails(CAROL).await.len();
  alice
    .client
    .send_text_mentioning(channel, None, "one more", vec![carol.user_id()])
    .await
    .unwrap();
  let newest = stack
    .get(
      &carol.tokens,
      &format!("/conversations/{channel}/messages?limit=1"),
    )
    .await
    .expect(StatusCode::OK);
  let newest_id = newest.body["messages"][0]["id"].as_str().unwrap().to_owned();
  stack
    .put(
      &carol.tokens,
      &format!("/conversations/{channel}/read-position"),
      json!({"message_id": newest_id}),
    )
    .await
    .expect(StatusCode::NO_CONTENT);
  tokio::time::sleep(Duration::from_secs(13)).await;
  assert_eq!(
    new_mails(CAROL).await.len(),
    sent,
    "a mention that was read still got mailed"
  );

  // Turning it off stops the mails.
  stack
    .put(&carol.tokens, "/me/email-digest", json!({"is_enabled": false}))
    .await
    .expect(StatusCode::OK);
  alice
    .client
    .send_text_mentioning(channel, None, "and again", vec![carol.user_id()])
    .await
    .unwrap();
  tokio::time::sleep(Duration::from_secs(8)).await;
  assert_eq!(new_mails(CAROL).await.len(), sent);

  stack.stop().await;
}

/// Other tests of the suite mail the same dev realm addresses (meeting invitations).
fn is_digest(mail: &ReceivedMail) -> bool {
  mail.subject.ends_with("in TeamCraft") && mail.subject.contains("new mention")
}

/// The number in "3 new mentions in TeamCraft" (1 for "1 new mention").
fn count_in(subject: &str) -> u32 {
  subject
    .split_whitespace()
    .next()
    .and_then(|word| word.parse().ok())
    .unwrap_or(0)
}
