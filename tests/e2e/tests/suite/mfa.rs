//! The "require a second factor" policy: local accounts need their one-time code, identity
//! provider tokens without a second factor are refused.
use reqwest::{Method, StatusCode};
use serde_json::json;
use teamcraft_e2e::{Mailpit, Peer, Stack, totp};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_policy_makes_local_accounts_use_their_code_and_refuses_tokens_without_a_second_factor() {
  let Some(stack) = Stack::builder().with_mail().with_local_accounts().start().await else {
    return;
  };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let carol = Peer::enroll(&stack, "carol").await;
  let space = stack
    .create_space(&alice.tokens, "Project A", "invite_only")
    .await;

  // A local guest: the activation link arrives by e-mail, activating sets the password and shows
  // the authenticator secret.
  let address = format!("local-{}@example.org", alice.client.device_id());
  stack
    .post(
      &alice.tokens,
      &format!("/spaces/{space}/local-guests"),
      json!({"email": address, "display_name": "Local Guest"}),
    )
    .await
    .expect(StatusCode::CREATED);
  let mail = Mailpit::new()
    .wait_for_mail_to(&address, std::time::Duration::from_secs(20))
    .await;
  let token = mail
    .text
    .split("#token=")
    .nth(1)
    .and_then(|rest| rest.split_whitespace().next())
    .unwrap_or_else(|| panic!("no activation link in the mail:\n{}", mail.text))
    .to_owned();
  let password = "a long local password 123";
  let activated = stack
    .request_with(
      None,
      Method::POST,
      "/auth/local/activate",
      Some(json!({"token": token, "password": password})),
    )
    .await
    .expect(StatusCode::OK);
  let secret = activated.body["totp_secret"].as_str().unwrap().to_owned();
  let login = |maybe_code: Option<String>, password: &str| {
    let body = json!({"login": address, "password": password, "totp_code": maybe_code});
    let stack = &stack;
    async move {
      stack
        .request_with(None, Method::POST, "/auth/local/login", Some(body))
        .await
    }
  };

  // The server here doesn't force codes (`require_totp = false`) and none was confirmed yet.
  login(None, password).await.expect(StatusCode::OK);

  // The owner requires a second factor.
  alice
    .set_policy(|policy| policy.session.requires_mfa = true)
    .await;
  let without_code = login(None, password).await;
  assert_eq!(without_code.status, StatusCode::UNAUTHORIZED, "{without_code:?}");
  let wrong_code = login(Some("000000".into()), password).await;
  assert_eq!(wrong_code.status, StatusCode::UNAUTHORIZED, "{wrong_code:?}");
  let wrong_password = login(Some(totp::code(&secret, -1)), "not the password at all").await;
  assert_eq!(
    wrong_password.status,
    StatusCode::UNAUTHORIZED,
    "{wrong_password:?}"
  );
  let signed_in = login(Some(totp::code(&secret, -1)), password)
    .await
    .expect(StatusCode::OK);
  let session = signed_in.body["access_token"].as_str().unwrap().to_owned();
  // A code is good once: the same step again is a replay.
  let replay = login(Some(totp::code(&secret, -1)), password).await;
  assert_eq!(replay.status, StatusCode::UNAUTHORIZED, "{replay:?}");
  let me = stack
    .request_with(Some(&session), Method::GET, "/me", None)
    .await
    .expect(StatusCode::OK);
  assert_eq!(me.body["role"], "guest", "{}", me.body);

  // Identity provider tokens of the dev realm carry no `amr` of a second factor: refused, owner
  // included (the realm would have to require OTP).
  for peer in [&carol, &alice] {
    let refused = stack.get(&peer.tokens, "/me").await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{refused:?}");
    assert_eq!(refused.error_code(), "mfa_required", "{refused:?}");
  }

  stack.stop().await;
}
