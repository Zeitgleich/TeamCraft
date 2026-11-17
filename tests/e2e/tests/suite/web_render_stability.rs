//! What the whole page does while the user only watches: the sidebar's rows, the open
//! conversation's messages and the avatars stay the nodes they are while messages arrive, people
//! type and change their presence, and across a reconnect of the gateway and a restart of the
//! server. A rebuilt node is drawn anew (an `<img>` loads again, a row loses its hover and
//! animations), which users see as flicker.
//!
//! Measured on the rendered page with a mutation observer over the document: element nodes that
//! were removed and not put back (a `For` that moves a row removes and re-inserts it; that is no
//! rebuild), per area, and `src` changes of images.

use std::time::{Duration, Instant};

use chromiumoxide::cdp::browser_protocol::network::{
  EmulateNetworkConditionsByRuleParams, NetworkConditions, OverrideNetworkStateParams,
};
use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
use chromiumoxide::page::Page;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_core::api::GatewayConnection;
use teamcraft_e2e::peer::DevClient;
use teamcraft_e2e::scenario::channel_with;
use teamcraft_e2e::{Peer, StackBuilder};

use crate::web::{Rig, TEST_CSP, WEB_ORIGIN, sign_in_as_alice, wait_for_text};
use crate::web_feed_stability::wait_until_readable;

/// A minute: the unit the counts are compared in.
const IDLE_PHASE: Duration = Duration::from_secs(20);
const ACTIVITY_ROUNDS: usize = 6;
const ACTIVITY_INTERVAL: Duration = Duration::from_secs(4);
/// Lines in the busy channel, one per interval.
const BUSY_LINES: usize = 15;
const BUSY_INTERVAL: Duration = Duration::from_secs(2);
const OFFLINE_FOR: Duration = Duration::from_secs(6);
/// Long enough for the gateway to come back and the fresh sync to land.
const RECOVERY: Duration = Duration::from_secs(20);
/// How often the peers' gateways are kept and read while the test waits.
const PEER_TICK: Duration = Duration::from_secs(2);
/// A 1×1 PNG: a profile picture the avatars load as a `blob:` URL.
const PICTURE_PNG: &[u8] = &[
  0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00,
  0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00,
  0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05, 0x00, 0x01, 0xff,
  0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];
/// Counts what was rebuilt. Removed nodes are looked at a task later: one that is connected again
/// by then was moved, not rebuilt.
const PROBE: &str = r#"
window.renderProbe = (() => {
  const empty = () => ({
    removed_elements: 0, topbar_removed: 0, sidebar_removed: 0, sidebar_rows_rebuilt: 0, feed_removed: 0,
    messages_rebuilt: 0, avatars_rebuilt: 0, images_rebuilt: 0, image_src_changes: 0,
    other_removed: 0, top_other: {}, attributes: {},
  });
  let stats = empty();
  const areaOf = (target) => {
    if (!target || !target.closest) return 'other';
    if (target.closest('.top-bar')) return 'topbar';
    if (target.closest('.sb-section, .sidebar')) return 'sidebar';
    if (target.closest('.message-list')) return 'feed';
    return 'other';
  };
  const firstClass = (node) => (typeof node.className === 'string' ? node.className : '').split(' ')[0];
  const describe = (node) => {
    const parent = node.parentElement || node;
    return `${parent.tagName.toLowerCase()}.${firstClass(parent)}>${node.tagName.toLowerCase()}.${firstClass(node)}`;
  };
  const pending = [];
  const observer = new MutationObserver((records) => {
    for (const record of records) {
      if (record.type === 'attributes') {
        const now = record.target.getAttribute(record.attributeName);
        if (record.oldValue === now) continue;
        if (record.target.tagName === 'IMG' && record.attributeName === 'src') {
          stats.image_src_changes += 1;
        }
        const key = `${describe(record.target)}@${record.attributeName}`;
        stats.attributes[key] = (stats.attributes[key] || 0) + 1;
        continue;
      }
      for (const node of record.removedNodes) {
        if (node.nodeType === 1) pending.push([node, areaOf(record.target), describe(record.target.nodeType === 1 ? record.target : node)]);
      }
    }
    setTimeout(settle, 0);
  });
  const settle = () => {
    while (pending.length) {
      const [node, area, where] = pending.pop();
      if (node.isConnected) continue;
      const count = 1 + node.querySelectorAll('*').length;
      stats.removed_elements += count;
      const has = (selector) => (node.matches(selector) ? 1 : 0) + node.querySelectorAll(selector).length;
      stats.avatars_rebuilt += has('.avatar-img');
      stats.images_rebuilt += has('img');
      if (area === 'topbar') {
        stats.topbar_removed += count;
      } else if (area === 'sidebar') {
        stats.sidebar_removed += count;
        stats.sidebar_rows_rebuilt += has('.sb-item');
      } else if (area === 'feed') {
        stats.feed_removed += count;
        stats.messages_rebuilt += has('[data-msg-id]');
      } else {
        stats.other_removed += count;
        stats.top_other[where] = (stats.top_other[where] || 0) + count;
      }
    }
  };
  observer.observe(document.body, {
    childList: true, subtree: true, attributes: true, attributeOldValue: true,
    attributeFilter: ['src', 'class', 'hidden', 'style'],
  });
  return {
    take: () => {
      settle();
      const taken = stats;
      const top = Object.entries(taken.top_other).sort((a, b) => b[1] - a[1]).slice(0, 8);
      taken.top_other = top.map(([where, count]) => `${where}:${count}`).join(' ');
      const attributes = Object.entries(taken.attributes).sort((a, b) => b[1] - a[1]).slice(0, 10);
      taken.attributes = attributes.map(([where, count]) => `${where}:${count}`).join(' ');
      taken.avatar_pictures = document.querySelectorAll('.avatar-img:not([hidden])').length;
      taken.avatar_sources = document.querySelectorAll('.avatar-img[src]').length;
      taken.avatars = document.querySelectorAll('.avatar-img').length;
      taken.picture_requests = performance.getEntriesByType('resource')
        .filter((entry) => /avatar|profile|:3900/.test(entry.name))
        .map((entry) => `${entry.name.replace(location.origin, '')}:${entry.responseStatus}`).slice(-6).join(' ');
      taken.dots = [...document.querySelectorAll('.presence')].map((dot) => dot.className).join(',');
      stats = empty();
      return JSON.stringify(taken);
    },
  };
})();
"#;

/// What the probe counted in one phase.
#[derive(Clone, Debug, Default)]
struct RenderStats {
  removed_elements: u64,
  topbar_removed: u64,
  sidebar_removed: u64,
  sidebar_rows_rebuilt: u64,
  feed_removed: u64,
  messages_rebuilt: u64,
  avatars_rebuilt: u64,
  images_rebuilt: u64,
  image_src_changes: u64,
  other_removed: u64,
  top_other: String,
  attributes: String,
  avatar_pictures: u64,
  avatar_sources: u64,
  avatars: u64,
  picture_requests: String,
  dots: String,
}

/// Alice reads the channel in the browser while Carol, who has a profile picture and a DM with
/// her, writes, types and changes her presence; then the browser goes offline for a moment and the
/// server restarts. Nothing that didn't change is built again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_page_keeps_its_nodes_while_watching_and_across_reconnects() {
  // Boxed: in a debug build the whole scenario overflows the test thread's stack.
  Box::pin(scenario()).await;
}

async fn scenario() {
  // The test CSP plus the dev stack's object storage, where pictures are downloaded from (a real
  // instance names its storage too).
  let csp = TEST_CSP.replace(
    "connect-src 'self'",
    "connect-src 'self' http://127.0.0.1:3900 http://localhost:3900",
  );
  let unlimited = |builder: StackBuilder| {
    builder
      .with_env("TEAMCRAFT_SECURITY__RATE_LIMITS__IS_ENABLED", "false")
      .with_env("TEAMCRAFT_WEB__CONTENT_SECURITY_POLICY", &csp)
  };
  let Some(mut rig) = Rig::start_with(unlimited, &[]).await else {
    return;
  };
  upload_picture(&rig, &rig.carol).await;
  let dm = rig.alice.client.open_dm(rig.carol.user_id()).await.unwrap();
  let busy = channel_with(&mut rig.alice, rig.space, "busy-log", &mut [&mut rig.carol]).await;
  rig.carol.wait_joined(dm).await;
  rig
    .carol
    .client
    .send_text(dm, None, "dm from carol")
    .await
    .unwrap();
  for index in 0..12 {
    rig
      .carol
      .client
      .send_text(rig.channel, None, &format!("channel line {index}"))
      .await
      .unwrap();
  }
  rig.alice.client.sync_account_data().await.unwrap();

  let page = rig.browser.page.clone();
  rig
    .browser
    .screenshot_on_failure("render_stability_sign_in", |page| async move {
      sign_in_as_alice(&page).await;
    })
    .await;
  wait_for_text(&page, "browser-channel").await;
  page
    .goto(format!("{WEB_ORIGIN}/c/{}", rig.channel))
    .await
    .unwrap();
  wait_until_readable(&mut rig.alice, &mut rig.carol, &page, "channel line 11").await;
  tokio::time::sleep(Duration::from_secs(3)).await;
  page.evaluate(PROBE).await.unwrap();
  let _ = take(&page).await;

  // (a) Nothing happens for a minute.
  keep_peers(&mut rig, IDLE_PHASE).await;
  let idle = take(&page).await;

  // (b) Carol writes in the channel and the DM, types and changes her presence.
  for round in 0..ACTIVITY_ROUNDS {
    rig
      .carol
      .gateway
      .send(&<DevClient>::typing_frame(rig.channel))
      .await
      .unwrap();
    let manual = if round % 2 == 0 { "dnd" } else { "auto" };
    rig
      .stack
      .put(&rig.carol.tokens, "/me/presence", json!({ "manual": manual }))
      .await
      .expect(StatusCode::OK);
    let target = if round % 2 == 0 { rig.channel } else { dm };
    rig
      .carol
      .client
      .send_text(target, None, &format!("activity {round}"))
      .await
      .unwrap();
    keep_peers(&mut rig, ACTIVITY_INTERVAL).await;
  }
  let activity = take(&page).await;

  // (c) Another channel is busy (an integration posting its log lines): rows of the open
  // conversation have nothing to do with it.
  for line in 0..BUSY_LINES {
    rig
      .carol
      .client
      .send_text(busy, None, &format!("busy line {line}"))
      .await
      .unwrap();
    keep_peers(&mut rig, BUSY_INTERVAL).await;
  }
  let busy_phase = take(&page).await;

  // (d) The browser loses the network and gets it back: the gateway reconnects and syncs.
  set_offline(&page, true).await;
  keep_peers(&mut rig, OFFLINE_FOR).await;
  set_offline(&page, false).await;
  keep_peers(&mut rig, RECOVERY).await;
  let network = take(&page).await;

  // (e) The server restarts (a deploy).
  rig.stack.restart_server().await;
  keep_peers(&mut rig, RECOVERY).await;
  let restart = take(&page).await;

  // (f) The DM is open, its search chip shows Carol's picture, and she writes.
  page.goto(format!("{WEB_ORIGIN}/c/{dm}")).await.unwrap();
  wait_until_readable(&mut rig.alice, &mut rig.carol, &page, "activity 1").await;
  keep_peers(&mut rig, Duration::from_secs(3)).await;
  page.evaluate(PROBE).await.unwrap();
  for round in 0..ACTIVITY_ROUNDS {
    rig
      .carol
      .client
      .send_text(dm, None, &format!("in the dm {round}"))
      .await
      .unwrap();
    keep_peers(&mut rig, ACTIVITY_INTERVAL).await;
  }
  let direct = take(&page).await;

  eprintln!("render stability, idle minute:     {idle:?}");
  eprintln!("render stability, activity:        {activity:?}");
  eprintln!("render stability, busy channel:    {busy_phase:?}");
  eprintln!("render stability, network toggle:  {network:?}");
  eprintln!("render stability, server restart:  {restart:?}");
  eprintln!("render stability, DM messages:     {direct:?}");
  for (phase, stats) in [
    ("idle", &idle),
    ("activity", &activity),
    ("busy channel", &busy_phase),
    ("network toggle", &network),
    ("server restart", &restart),
    ("DM messages", &direct),
  ] {
    assert_kept(phase, stats);
  }
  assert!(direct.avatar_pictures > 0, "no picture showed: {direct:?}");
  rig.finish().await;
}

/// Nothing that stayed on screen was built again: no sidebar row, no message, no avatar, nothing of
/// the top bar (its search chip), and no picture loaded again.
fn assert_kept(phase: &str, stats: &RenderStats) {
  assert_eq!(
    stats.sidebar_rows_rebuilt, 0,
    "{phase}: sidebar rows were rebuilt: {stats:?}"
  );
  assert_eq!(
    stats.messages_rebuilt, 0,
    "{phase}: messages were rebuilt: {stats:?}"
  );
  assert_eq!(
    stats.feed_removed, 0,
    "{phase}: parts of the feed were rebuilt: {stats:?}"
  );
  assert_eq!(
    stats.images_rebuilt, 0,
    "{phase}: pictures were rebuilt: {stats:?}"
  );
  assert_eq!(
    stats.image_src_changes, 0,
    "{phase}: pictures loaded again: {stats:?}"
  );
  assert_eq!(
    stats.topbar_removed, 0,
    "{phase}: the top bar was rebuilt: {stats:?}"
  );
}

/// Waits `duration` while Alice's other device and Carol keep their gateways (the server closes
/// one without heartbeats) and apply what arrives, as running apps do.
async fn keep_peers(rig: &mut Rig, duration: Duration) {
  let deadline = Instant::now() + duration;
  while Instant::now() < deadline {
    for peer in [&mut rig.alice, &mut rig.carol] {
      peer.heartbeat().await;
      peer.pump_reconnecting().await;
      // Right after the restart a change may not go through yet; the next tick tries again.
      if let Err(error) = peer.client.apply_pending_changes().await {
        eprintln!("{} applies pending changes: {error}", peer.name);
      }
    }
    tokio::time::sleep(PEER_TICK).await;
  }
}

async fn take(page: &Page) -> RenderStats {
  let params = EvaluateParams::builder()
    .expression("window.renderProbe.take()")
    .return_by_value(true)
    .build()
    .unwrap();
  let json: String = page.evaluate(params).await.unwrap().into_value().unwrap();
  let value: serde_json::Value = serde_json::from_str(&json).unwrap();
  let count = |name: &str| value[name].as_u64().unwrap_or_default();
  RenderStats {
    removed_elements: count("removed_elements"),
    topbar_removed: count("topbar_removed"),
    sidebar_removed: count("sidebar_removed"),
    sidebar_rows_rebuilt: count("sidebar_rows_rebuilt"),
    feed_removed: count("feed_removed"),
    messages_rebuilt: count("messages_rebuilt"),
    avatars_rebuilt: count("avatars_rebuilt"),
    images_rebuilt: count("images_rebuilt"),
    image_src_changes: count("image_src_changes"),
    other_removed: count("other_removed"),
    top_other: value["top_other"].as_str().unwrap_or_default().to_owned(),
    attributes: value["attributes"].as_str().unwrap_or_default().to_owned(),
    avatar_pictures: count("avatar_pictures"),
    avatar_sources: count("avatar_sources"),
    avatars: count("avatars"),
    picture_requests: value["picture_requests"].as_str().unwrap_or_default().to_owned(),
    dots: value["dots"].as_str().unwrap_or_default().to_owned(),
  }
}

/// Gives `peer` a profile picture.
async fn upload_picture(rig: &Rig, peer: &Peer) {
  let upload = rig
    .stack
    .post(
      &peer.tokens,
      "/me/avatar",
      json!({ "content_type": "image/png", "size_bytes": PICTURE_PNG.len() }),
    )
    .await
    .expect(StatusCode::CREATED);
  let response = rig
    .stack
    .http
    .put(upload.body["upload_url"].as_str().unwrap())
    .header("content-type", "image/png")
    .body(PICTURE_PNG.to_vec())
    .send()
    .await
    .unwrap();
  assert!(response.status().is_success(), "{}", response.status());
  rig
    .stack
    .post(&peer.tokens, "/me/avatar/complete", json!({}))
    .await
    .expect(StatusCode::OK);
}

async fn set_offline(page: &Page, is_offline: bool) {
  let every_request = NetworkConditions::new("", 0.0, -1.0, -1.0);
  page
    .execute(EmulateNetworkConditionsByRuleParams::new(
      is_offline,
      vec![every_request],
    ))
    .await
    .unwrap();
  page
    .execute(OverrideNetworkStateParams::new(is_offline, 0.0, -1.0, -1.0))
    .await
    .unwrap();
}
