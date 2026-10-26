//! v1 acceptance, PLAN.md §6 Files: encrypted upload and download, the server holds ciphertext
//! only, share links with a download cap, and revoking them.
use reqwest::{Method, StatusCode};
use serde_json::json;
use teamcraft_core::client::OutgoingFile;
use teamcraft_core::store::{LocalStore, MessageContent, MessageFilter};
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn files_are_encrypted_for_members_and_share_links_are_limited_and_revocable() {
  let Some(stack) = Stack::builder()
    .with_env("TEAMCRAFT_SHARES__PUBLIC_BASE_URL", "http://share.example.test")
    .start()
    .await
  else {
    return;
  };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let space = space_with(&stack, &alice, "Project A", &[(&carol, "member")]).await;
  let channel = channel_with(&mut alice, space, "files", &mut [&mut carol]).await;

  let content = b"quarterly numbers: very secret".to_vec();
  let sent = alice
    .client
    .send_file(
      channel,
      None,
      OutgoingFile {
        file_name: "numbers.txt".into(),
        media_type: "text/plain".into(),
        content: content.clone(),
      },
    )
    .await
    .unwrap();
  let message = alice.store.message(channel, sent).unwrap().unwrap();
  let MessageContent::File { attachment } = &message.content else {
    panic!("not a file message: {:?}", message.content);
  };
  let attachment_id = attachment.attachment_id.to_string();
  let server_id = message.maybe_server_id.unwrap();
  carol
    .wait_until("the file message arrives", |peer| {
      peer.store.message(channel, sent).unwrap().is_some()
    })
    .await;
  assert_eq!(
    carol
      .client
      .fetch_file(channel, sent)
      .await
      .unwrap()
      .expose_secret(),
    content.as_slice()
  );
  // The "files tab": the file messages of the conversation.
  let files: Vec<_> = carol
    .store
    .messages(channel, MessageFilter::All)
    .unwrap()
    .into_iter()
    .filter(|message| matches!(message.content, MessageContent::File { .. }))
    .collect();
  assert_eq!(files.len(), 1);
  let _ = server_id;

  // A share link for people outside: limited to two downloads.
  let link = stack
    .post(
      &alice.tokens,
      "/shares",
      json!({"attachment_id": attachment_id, "max_downloads": 2}),
    )
    .await
    .expect(StatusCode::CREATED);
  let id = link.body["id"].as_str().unwrap().to_owned();
  assert!(
    link.body["url"]
      .as_str()
      .unwrap()
      .starts_with("http://share.example.test/f/"),
    "{}",
    link.body
  );
  let public = |method: Method, path: String| {
    let stack = &stack;
    async move { stack.request_with(None, method, &path, None).await }
  };
  let info = public(Method::GET, format!("/shares/{id}"))
    .await
    .expect(StatusCode::OK);
  assert_eq!(info.body["is_password_protected"], false, "{}", info.body);
  for _ in 0..2 {
    let download = public(Method::POST, format!("/shares/{id}/download"))
      .await
      .expect(StatusCode::OK);
    // What is behind the URL is ciphertext.
    let bytes = reqwest::get(download.body["download_url"].as_str().unwrap())
      .await
      .unwrap()
      .bytes()
      .await
      .unwrap();
    assert!(
      !bytes.windows(6).any(|window| window == b"secret"),
      "plaintext in storage"
    );
  }
  let third = public(Method::POST, format!("/shares/{id}/download")).await;
  assert!(
    !third.status.is_success(),
    "a third download got through: {third:?}"
  );

  // Revoking ends a link at once; members of the conversation only can make links.
  let second = stack
    .post(&alice.tokens, "/shares", json!({"attachment_id": attachment_id}))
    .await
    .expect(StatusCode::CREATED);
  let second_id = second.body["id"].as_str().unwrap().to_owned();
  stack
    .delete(&alice.tokens, &format!("/shares/{second_id}"))
    .await
    .expect(StatusCode::NO_CONTENT);
  let revoked = public(Method::GET, format!("/shares/{second_id}")).await;
  assert_eq!(revoked.status, StatusCode::GONE, "{revoked:?}");
  let erin = Peer::enroll(&stack, "erin").await;
  let outsider = stack
    .post(&erin.tokens, "/shares", json!({"attachment_id": attachment_id}))
    .await;
  assert!(
    !outsider.status.is_success(),
    "a non-member shared the file: {outsider:?}"
  );

  stack.stop().await;
}
