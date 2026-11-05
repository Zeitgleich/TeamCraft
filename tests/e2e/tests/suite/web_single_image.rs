//! One picture of an album in the web client (issue #241): the right-click menu on a tile, a reply
//! whose quote shows that picture and opens it, and a forward of it alone from the lightbox.

use std::time::Duration;

use chromiumoxide::page::Page;
use teamcraft_core::store::{LocalStore, MessageContent, MessageFilter, MessageMeta, StoredMessage};
use teamcraft_e2e::browser::{BrowserSession, lease_web_port, prefer_mouse, web_dir};
use teamcraft_e2e::scenario::space_with;
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::{ConversationId, UserId};

use crate::pending_echo::type_into_composer;
use crate::perf::sign_in_and_unlock;
use crate::web::{TEST_CSP, click_text, shot, wait_for_element, wait_for_text};

const PORT: u16 = 8083;
const WEB_ORIGIN: &str = "http://127.0.0.1:8083";
const WAIT: Duration = Duration::from_secs(60);
const CAPTION: &str = "the site, all four sides";
const REPLY: &str = "the third one is the door";

/// Four small pictures of different colours into the composer's file input, as a picker does.
const PICK_FOUR_PICTURES: &str = r"(async () => {
  const transfer = new DataTransfer();
  const names = ['a.png', 'b.png', 'c.png', 'd.png'];
  const colours = ['#c33', '#3c3', '#33c', '#cc3'];
  for (let index = 0; index < names.length; index++) {
    const canvas = document.createElement('canvas');
    canvas.width = 320;
    canvas.height = 240;
    const context = canvas.getContext('2d');
    context.fillStyle = colours[index];
    context.fillRect(0, 0, 320, 240);
    const blob = await new Promise(resolve => canvas.toBlob(resolve, 'image/png'));
    transfer.items.add(new File([blob], names[index], { type: 'image/png' }));
  }
  const input = document.querySelector('input[type=file][multiple]');
  input.files = transfer.files;
  input.dispatchEvent(new Event('change', { bubbles: true }));
})()";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_picture_of_an_album_is_replied_to_and_forwarded_alone() {
  let Some(web_dir) = web_dir() else {
    let is_required = std::env::var("E2E_REQUIRED").is_ok_and(|value| value == "1");
    assert!(!is_required, "no web build: run tests/e2e/web/build.sh");
    eprintln!("skipping: no web build (tests/e2e/web/build.sh)");
    return;
  };
  let _turn = lease_web_port(PORT).await;
  let Some(stack) = Stack::builder()
    .with_port(PORT)
    .with_web_dir(web_dir)
    // The picture goes up from the page, to the dev stack's storage.
    .with_env(
      "TEAMCRAFT_WEB__CONTENT_SECURITY_POLICY",
      &TEST_CSP.replace("connect-src 'self'", "connect-src 'self' http://127.0.0.1:3900"),
    )
    .start()
    .await
  else {
    return;
  };
  let Some(browser) = BrowserSession::launch().await else {
    return;
  };
  let mut alice = Peer::enroll(&stack, "alice").await;
  drop(Peer::enroll(&stack, "carol").await);
  let carol_tokens = stack.tokens("carol");
  let carol: UserId = stack.get(&carol_tokens, "/me").await.id().parse().unwrap();
  let space = space_with(&stack, &alice, "Space", &[]).await;
  stack
    .add_space_member(&alice.tokens, &space.to_string(), &carol.to_string(), "member")
    .await;

  let alice_ref = &mut alice;
  browser
    .screenshot_on_failure("single-image", |page| async move {
      prefer_mouse(&page).await;
      sign_in_and_unlock(&page, "carol").await;
      let album_channel = shared_channel(alice_ref, space, carol, "album").await;
      let elsewhere = shared_channel(alice_ref, space, carol, "elsewhere").await;
      page
        .goto(format!("{WEB_ORIGIN}/c/{album_channel}"))
        .await
        .unwrap();
      wait_for_element(&page, "[data-composer]").await;

      send_album(&page).await;
      reply_to_the_third_picture(&page).await;
      shot(&page, "single-image-quote").await;
      let reply = received_reply(alice_ref, album_channel).await;
      let quoted = reply.maybe_reply_to.unwrap().maybe_attachment.unwrap();
      assert_eq!(quoted.file_name, "c.png");
      assert!(
        quoted.maybe_thumbnail.is_some(),
        "the quote carries the picture's thumbnail"
      );

      forward_the_shown_picture_from_the_lightbox(&page).await;
      let forward = received_forward(alice_ref, elsewhere).await;
      assert_eq!(forward.attachments.len(), 1, "only the picture, not the album");
      assert_eq!(forward.attachments[0].file_name, "c.png");
      assert_eq!(forward.maybe_forwarded.unwrap().body, "", "not the album's text");
    })
    .await;
  browser.close().await;
  stack.stop().await;
}

async fn shared_channel(
  alice: &mut Peer,
  space: teamcraft_proto::SpaceId,
  carol: UserId,
  name: &str,
) -> ConversationId {
  let channel = alice.client.create_channel(space, name).await.unwrap();
  alice.client.add_member(channel, carol).await.unwrap();
  channel
}

/// Sends the four pictures with a caption, and waits until they are on the server.
async fn send_album(page: &Page) {
  page.evaluate(PICK_FOUR_PICTURES).await.unwrap();
  wait_for_text(page, "d.png").await;
  let composer = type_into_composer(page, CAPTION).await;
  composer.press_key("Enter").await.unwrap();
  wait_until(
    page,
    "all four tiles are sent",
    "document.querySelectorAll('.media-tile [data-media-key]').length === 4",
  )
  .await;
}

/// The third tile's menu, the reply in the composer with the picture above it, the sent quote.
async fn reply_to_the_third_picture(page: &Page) {
  page
    .evaluate(
      "document.querySelectorAll('.media-tile')[2].dispatchEvent(new MouseEvent('contextmenu', \
       { bubbles: true, cancelable: true, clientX: 200, clientY: 200 }))",
    )
    .await
    .unwrap();
  wait_for_element(page, ".attachment-menu").await;
  shot(page, "single-image-menu").await;
  click_text(page, "Reply to this image").await;
  wait_for_element(page, ".reply-banner img.quote-thumb").await;
  let composer = type_into_composer(page, REPLY).await;
  composer.press_key("Enter").await.unwrap();
  wait_for_text(page, REPLY).await;
  wait_for_element(page, "button.msg-quote-image img.quote-thumb").await;
}

/// A click on the quote opens that picture; "Forward this image" in its menu goes to another channel.
async fn forward_the_shown_picture_from_the_lightbox(page: &Page) {
  page
    .evaluate("document.querySelector('button.msg-quote-image').click()")
    .await
    .unwrap();
  wait_until(
    page,
    "the lightbox shows the quoted picture",
    "!!document.querySelector('.lightbox-menu-wrap') && document.body.innerText.includes('c.png')",
  )
  .await;
  page
    .evaluate("document.querySelector('.lightbox-menu-wrap [aria-haspopup=menu]').click()")
    .await
    .unwrap();
  wait_for_text(page, "Reply to this image").await;
  shot(page, "single-image-lightbox-menu").await;
  click_text(page, "Forward this image").await;
  wait_for_element(page, ".forward-list").await;
  page
    .evaluate("[...document.querySelectorAll('.forward-item')].find(item => item.textContent.includes('elsewhere')).click()")
    .await
    .unwrap();
  page
    .evaluate("document.querySelector('.modal-foot button[type=submit]').click()")
    .await
    .unwrap();
  wait_until(
    page,
    "the dialog closes",
    "!document.querySelector('.forward-list')",
  )
  .await;
}

async fn received_reply(alice: &mut Peer, channel: ConversationId) -> MessageMeta {
  alice.wait_for_text(channel, REPLY).await;
  stored_metas(alice, channel)
    .into_iter()
    .find(|meta| meta.maybe_reply_to.is_some())
    .unwrap()
}

async fn received_forward(alice: &mut Peer, channel: ConversationId) -> MessageMeta {
  alice
    .wait_until("the forward arrives", |peer| {
      stored_metas(peer, channel)
        .iter()
        .any(|meta| meta.maybe_forwarded.is_some())
    })
    .await;
  stored_metas(alice, channel)
    .into_iter()
    .find(|meta| meta.maybe_forwarded.is_some())
    .unwrap()
}

fn stored_metas(alice: &Peer, channel: ConversationId) -> Vec<MessageMeta> {
  alice
    .store
    .messages(channel, MessageFilter::All)
    .unwrap()
    .into_iter()
    .filter_map(|stored: StoredMessage| match stored.content {
      MessageContent::Text { meta, .. } => Some(meta),
      _ => None,
    })
    .collect()
}

async fn wait_until(page: &Page, what: &str, condition: &str) {
  let deadline = tokio::time::Instant::now() + WAIT;
  while tokio::time::Instant::now() < deadline {
    let is_met: bool = page
      .evaluate(condition)
      .await
      .ok()
      .and_then(|value| value.into_value().ok())
      .unwrap_or(false);
    if is_met {
      return;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
  panic!("waited in vain until {what}");
}
