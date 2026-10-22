//! Restricting guest e-mail domains in the signed policy: invitations, meeting invitees and sign-in.
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_e2e::{Peer, Stack};

const STARTS_AT: i64 = 1_893_488_400_000;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn guests_need_an_allowed_e_mail_domain_to_be_invited_or_to_sign_in() {
  let Some(stack) = Stack::builder().with_meetings().start().await else {
    return;
  };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let dave = Peer::enroll(&stack, "dave").await;
  let space = stack
    .create_space(&alice.tokens, "Project A", "invite_only")
    .await;
  stack.get(&dave.tokens, "/me").await.expect(StatusCode::OK);

  // Without a restriction every domain is fine.
  let alice_tokens = alice.tokens.clone();
  let invite = |email: &str, role: &str| {
    let body = json!({"email": email, "role": role});
    let (stack, tokens, path) = (&stack, alice_tokens.clone(), format!("/spaces/{space}/invites"));
    async move { stack.post(&tokens, &path, body).await }
  };
  invite("early@evil.test", "guest")
    .await
    .expect(StatusCode::CREATED);

  // The owner restricts guests to partner.test (and its subdomains) in the signed policy.
  alice
    .set_policy(|policy| policy.guests.allowed_email_domains = vec!["partner.test".into()])
    .await;
  let refused = invite("gus@evil.test", "guest").await;
  assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{refused:?}");
  let lookalike = invite("gus@partner.test.evil.test", "guest").await;
  assert_eq!(lookalike.status, StatusCode::BAD_REQUEST, "{lookalike:?}");
  invite("gus@team.partner.test", "guest")
    .await
    .expect(StatusCode::CREATED);
  // Members aren't guests: the rule doesn't touch them.
  invite("colleague@evil.test", "member")
    .await
    .expect(StatusCode::CREATED);

  // Meeting invitees from outside are guests too.
  let meeting = |email: &str| {
    let body = json!({
      "title": "Customer call", "starts_at": STARTS_AT, "ends_at": STARTS_AT + 3_600_000,
      "invitees": [{"kind": "external", "email": email, "name": "Gus"}],
    });
    let (stack, tokens) = (&stack, alice_tokens.clone());
    async move { stack.post(&tokens, "/meetings", body).await }
  };
  let outsider = meeting("gus-meeting@evil.test").await;
  assert_eq!(outsider.status, StatusCode::BAD_REQUEST, "{outsider:?}");
  meeting("gus-meeting@partner.test")
    .await
    .expect(StatusCode::CREATED);

  // Dave is a guest from example.com: his valid token is refused from now on...
  let blocked = stack.get(&dave.tokens, "/me").await;
  assert_eq!(blocked.status, StatusCode::FORBIDDEN, "{blocked:?}");
  // ...while members and owners are not guests.
  stack.get(&alice.tokens, "/me").await.expect(StatusCode::OK);

  // Allowing his domain lets him back in.
  alice
    .set_policy(|policy| policy.guests.allowed_email_domains = vec!["example.com".into()])
    .await;
  stack.get(&dave.tokens, "/me").await.expect(StatusCode::OK);

  // Domain names only.
  let nonsense = alice
    .client
    .publish_policy({
      let mut policy = alice.client.workspace().unwrap().policy;
      policy.guests.allowed_email_domains = vec!["not a domain".into()];
      policy
    })
    .await;
  assert!(
    nonsense.is_err(),
    "the server accepted a policy with a bad domain"
  );

  stack.stop().await;
}
