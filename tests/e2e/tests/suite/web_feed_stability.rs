//! What an open conversation does while nothing the reader did changes it: the rendered rows and
//! the position of the messages on screen stay as they are (#1132). Measured on the rendered page,
//! per frame, with the composer idle, with the other person typing and while imported history is
//! read into the conversation page by page (#1083).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use chromiumoxide::cdp::browser_protocol::network::{
  EmulateNetworkConditionsByRuleParams, NetworkConditions, OverrideNetworkStateParams,
};
use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
use chromiumoxide::page::Page;
use teamcraft_core::api::GatewayConnection;
use teamcraft_core::client::import::ImportDraft;
use teamcraft_core::imported::{ImportSource, ImportedAuthor, ImportedOrigin};
use teamcraft_core::store::{MessageContent, MessageMeta};
use teamcraft_core::workspace::RosterRole;
use teamcraft_e2e::peer::{DevClient, settle};
use teamcraft_e2e::{Peer, StackBuilder};
use teamcraft_proto::{AdminSelfJoin, ConversationId, ImportPolicy};

use crate::web::{Rig, WEB_ORIGIN, body_text, sign_in_as_alice, wait_for_text};

/// Live messages in the conversation: more than a page, and more than fit on the screen.
const LIVE_MESSAGES: usize = 40;
/// Imported messages: a page at once, the rest in pages of 200 at one per upkeep tick.
const IMPORTED_MESSAGES: usize = 1_600;
/// From here on the test reads a little above the newest message.
const IMPORTED_HALF: usize = 600;
const IMPORT_BATCH: usize = 100;
/// The time of the oldest imported message: long before everything else in the conversation.
const OLD_MS: i64 = 1_500_000_000_000;
const QUIET_PHASE: Duration = Duration::from_secs(12);
/// A typing frame per this: the server forwards one per 3 s and conversation.
const TYPING_INTERVAL: Duration = Duration::from_secs(3);
const TYPING_FRAMES: usize = 4;
/// How long the import may take to be read to its beginning.
const PAGING_DEADLINE: Duration = Duration::from_secs(120);
const SET_UP_DEADLINE: Duration = Duration::from_secs(120);
/// Rounds of a second without a new page that say the paging ended (a page per tick of 5 s).
const QUIET_ROUNDS: u32 = 8;
/// Longer than the floating day stays after a scroll.
const SCROLL_SETTLE: Duration = Duration::from_millis(2_000);
/// Scrolls up like a wheel: the event leaves the bottom, the position follows.
const SCROLL_UP: &str = r#"(() => {
  const list = document.querySelector('.message-list');
  list.dispatchEvent(new WheelEvent('wheel', { deltaY: -300, bubbles: true }));
  list.scrollTop -= 300;
})()"#;
/// More than the newest page the browser holds, and more than one further page.
const OLDER_HISTORY_MESSAGES: usize = 400;
/// How long a page of older history may take once the reader is at the top.
const OLDER_PAGE_WAIT: Duration = Duration::from_secs(10);
/// Goes to the very top like a wheel would, and starts the probe's count from there.
const SCROLL_TO_TOP: &str = r#"(() => {
  const list = document.querySelector('.message-list');
  list.dispatchEvent(new WheelEvent('wheel', { deltaY: -3000, bubbles: true }));
  list.scrollTop = 0;
})()"#;
/// What a message on screen may end up from where it was: rounding, not a move.
const MAX_DRIFT_PX: f64 = 1.5;
/// Two frames on, the list has followed the jump: the probe starts counting from there.
const AFTER_THE_JUMP: &str = r#"new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(() => {
  window.feedProbe.forgetPlaces();
  window.feedProbe.take();
  done();
})))"#;
/// Watches the feed once per frame. Reading layout per frame is the probe's, not the app's. A row
/// counts as visible when it intersects the list; its message's distance from the list's top
/// must not change, unless the reader scrolled, and there is no reader in this test.
const PROBE: &str = r#"
window.feedProbe = (() => {
  const empty = () => ({
    frames: 0, msgs_removed: 0, visible_removed: 0, rows_added: 0, rows_removed: 0, skeleton_frames: 0,
    moved_frames: 0, max_move_px: 0, scroll_changes: 0, list_replaced: 0, count_changes: 0, max_index: 0,
    blank_frames: 0, day_shown_frames: 0,
  });
  let stats = empty();
  let list = document.querySelector('.message-list');
  let lastTop = list.scrollTop;
  let lastScrollHeight = list.scrollHeight;
  let lastPlaces = new Map();
  let visible = new Set();
  const seen = new WeakSet(list.querySelectorAll('.row'));
  const observer = new MutationObserver((records) => {
    for (const record of records) {
      // A row that went and came back only moved (`For` reorders): it kept its state and its pixels.
      for (const node of record.addedNodes) {
        if (node.nodeType !== 1 || !node.classList.contains('row') || seen.has(node)) continue;
        seen.add(node);
        stats.rows_added += 1;
      }
      for (const node of record.removedNodes) {
        if (node.nodeType !== 1 || node.isConnected) continue;
        if (node.classList.contains('row')) stats.rows_removed += 1;
        const removed = node.matches('[data-msg-id]') ? [node] : [...node.querySelectorAll('[data-msg-id]')];
        for (const message of removed) {
          stats.msgs_removed += 1;
          if (visible.has(message.getAttribute('data-msg-id'))) stats.visible_removed += 1;
        }
      }
    }
  });
  const watch = () => observer.observe(list.querySelector('.rows'), { childList: true, subtree: true });
  watch();
  const frame = () => {
    stats.frames += 1;
    const now = document.querySelector('.message-list');
    if (now !== list) {
      stats.list_replaced += 1;
      list = now;
      observer.disconnect();
      if (list) watch();
    }
    if (list) {
      if (list.scrollTop !== lastTop) stats.scroll_changes += 1;
      lastTop = list.scrollTop;
      if (list.scrollHeight !== lastScrollHeight) stats.count_changes += 1;
      lastScrollHeight = list.scrollHeight;
      const box = list.getBoundingClientRect();
      const places = new Map();
      for (const message of list.querySelectorAll('[data-msg-id]')) {
        const rect = message.getBoundingClientRect();
        if (rect.bottom > box.top && rect.top < box.bottom) {
          // The bottom: a row whose header goes (older rows came, same author) keeps its text there.
          places.set(message.getAttribute('data-msg-id'), rect.bottom - box.top);
        }
      }
      for (const row of list.querySelectorAll('.row[data-idx]')) {
        stats.max_index = Math.max(stats.max_index, Number(row.getAttribute('data-idx')));
      }
      if (places.size === 0) stats.blank_frames += 1;
      if (document.querySelector('.floating-day:not(.is-idle)')) stats.day_shown_frames += 1;
      let moved = 0;
      for (const [id, top] of places) {
        if (lastPlaces.has(id)) moved = Math.max(moved, Math.abs(top - lastPlaces.get(id)));
      }
      if (moved > 1) stats.moved_frames += 1;
      stats.max_move_px = Math.max(stats.max_move_px, moved);
      const skeletons = [...list.querySelectorAll('.skeleton, .msg-skeleton')].some((node) => {
        const rect = node.getBoundingClientRect();
        return rect.bottom > box.top && rect.top < box.bottom;
      });
      if (skeletons) stats.skeleton_frames += 1;
      lastPlaces = places;
      visible = new Set(places.keys());
    }
    next();
  };
  // Sampled from a resize observer of a sentinel that changes size every frame: it runs after all
  // of the frame's callbacks and after the list's own observer (created earlier), right before the
  // paint. What it sees is what gets painted; any earlier or later it isn't.
  const sentinel = document.createElement('div');
  sentinel.style.position = 'fixed';
  sentinel.style.height = '1px';
  sentinel.style.visibility = 'hidden';
  document.body.appendChild(sentinel);
  let tick = 0;
  new ResizeObserver(frame).observe(sentinel);
  const next = () => requestAnimationFrame(() => {
    tick = (tick + 1) % 2;
    sentinel.style.width = `${tick + 1}px`;
  });
  next();
  return {
    take: () => { const taken = stats; stats = empty(); return taken; },
    // The reader moved the list: the next frame is what later ones compare with.
    forgetPlaces: () => { lastPlaces = new Map(); visible = new Set(); },
    places: () => JSON.stringify([...lastPlaces]),
  };
})();
"#;

/// What the probe saw in one phase.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct FeedStats {
  frames: u64,
  msgs_removed: u64,
  visible_removed: u64,
  rows_added: u64,
  rows_removed: u64,
  skeleton_frames: u64,
  moved_frames: u64,
  max_move_px: f64,
  scroll_changes: u64,
  list_replaced: u64,
  count_changes: u64,
  max_index: u64,
  blank_frames: u64,
  day_shown_frames: u64,
}

impl FeedStats {
  fn plus(self, other: Self) -> Self {
    Self {
      frames: self.frames + other.frames,
      msgs_removed: self.msgs_removed + other.msgs_removed,
      visible_removed: self.visible_removed + other.visible_removed,
      rows_added: self.rows_added + other.rows_added,
      rows_removed: self.rows_removed + other.rows_removed,
      skeleton_frames: self.skeleton_frames + other.skeleton_frames,
      moved_frames: self.moved_frames + other.moved_frames,
      max_move_px: self.max_move_px.max(other.max_move_px),
      scroll_changes: self.scroll_changes + other.scroll_changes,
      list_replaced: self.list_replaced + other.list_replaced,
      count_changes: self.count_changes + other.count_changes,
      max_index: self.max_index.max(other.max_index),
      blank_frames: self.blank_frames + other.blank_frames,
      day_shown_frames: self.day_shown_frames + other.day_shown_frames,
    }
  }
}

/// A direct message with live messages, open in the browser with the composer focused, stays as it
/// is while nothing happens, while the other person types and while imported history arrives.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_open_conversation_stays_still_while_the_other_person_types_and_old_history_arrives() {
  // Boxed: in a debug build the whole scenario overflows the test thread's stack, which aborts the
  // test and leaves its server on the web port.
  Box::pin(scenario()).await;
}

async fn scenario() {
  // An import sends many requests in a row; the limits are not what this test looks at.
  let unlimited =
    |builder: StackBuilder| builder.with_env("TEAMCRAFT_SECURITY__RATE_LIMITS__IS_ENABLED", "false");
  let Some(mut rig) = Rig::start_with(unlimited, &[]).await else {
    return;
  };
  let mut importer = Peer::enroll(&rig.stack, "bob").await;
  let dm = rig.alice.client.open_dm(rig.carol.user_id()).await.unwrap();
  rig.carol.wait_joined(dm).await;
  for index in 0..LIVE_MESSAGES {
    rig
      .carol
      .client
      .send_text(dm, None, &format!("live {index}"))
      .await
      .unwrap();
  }
  // The newest message, which `wait_until_set_up` looks for on the page.
  rig
    .carol
    .client
    .send_text(dm, None, "hello from carol")
    .await
    .unwrap();
  rig.alice.wait_for_text(dm, "hello from carol").await;
  // Everything of the import but its messages, before the browser: signing the roster and letting
  // the import account in change banners above the list, which moves it for another reason.
  prepare_import(&mut rig, &mut importer, dm).await;
  rig.alice.client.sync_account_data().await.unwrap();

  let page = rig.browser.page.clone();
  sign_in_as_alice(&page).await;
  wait_for_text(&page, "browser-channel").await;
  page.goto(format!("{WEB_ORIGIN}/dm/{dm}")).await.unwrap();
  wait_until_readable(&mut rig.alice, &mut rig.carol, &page, "live 0").await;
  // It opens at the "New messages" divider; the reader goes on to the newest message.
  page
    .evaluate("document.querySelector('.jump-latest').click()")
    .await
    .unwrap();
  wait_for_text(&page, "hello from carol").await;
  tokio::time::sleep(Duration::from_secs(2)).await;
  page
    .evaluate("document.querySelector('[data-composer]').focus()")
    .await
    .unwrap();
  page.evaluate(PROBE).await.unwrap();
  let _ = take_stats(&page).await;

  // (a) The composer has the focus, nothing happens.
  tokio::time::sleep(QUIET_PHASE).await;
  let quiet = take_stats(&page).await;

  // (b) The other person types.
  for _ in 0..TYPING_FRAMES {
    rig
      .carol
      .gateway
      .send(&<DevClient>::typing_frame(dm))
      .await
      .unwrap();
    tokio::time::sleep(TYPING_INTERVAL).await;
  }
  let typing = take_stats(&page).await;

  // (c) Imported history is read into the conversation, one page per tick.
  let drafts: Vec<ImportDraft> = (0..IMPORTED_MESSAGES).map(draft).collect();
  for batch in drafts.chunks(IMPORT_BATCH) {
    importer.client.send_imported(dm, batch).await.unwrap();
  }
  importer.client.finish_import(dm).await.unwrap();
  let following = watch_paging(&page, false, |stats| stats.max_index as usize > IMPORTED_HALF).await;

  // (d) The same for somebody reading a little further up.
  page.evaluate(SCROLL_UP).await.unwrap();
  tokio::time::sleep(SCROLL_SETTLE).await;
  let _ = take_stats(&page).await;
  let reading = watch_paging(&page, true, |stats| stats.max_index as usize > IMPORTED_MESSAGES).await;

  eprintln!("feed stability, composer idle: {quiet:?}");
  eprintln!("feed stability, peer typing:   {typing:?}");
  eprintln!("feed stability, history at the bottom: {following:?}");
  eprintln!("feed stability, history reading above: {reading:?}");
  assert_still("the composer idle", quiet);
  assert_still("the other person typing", typing);
  assert_still("imported history arriving at the bottom", following);
  assert_still("imported history arriving above the reader", reading);
  assert_eq!(
    reading.day_shown_frames, 0,
    "the floating day showed without scrolling: {reading:?}"
  );
  assert!(
    reading.max_index as usize > IMPORTED_MESSAGES,
    "the import was not read to its beginning: {reading:?}"
  );
  rig.finish().await;
}

/// A device that holds only the newest page reads the rest while the reader scrolls up, and what is
/// on screen stays where it is; a failed page says so at the top of the list and can be asked again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scrolling_above_what_the_device_holds_reads_the_server_and_nothing_moves() {
  // Boxed for the same reason as the scenario above.
  Box::pin(older_history_scenario()).await;
}

async fn older_history_scenario() {
  // Many messages in a row; the limits are not what this test looks at.
  let unlimited =
    |builder: StackBuilder| builder.with_env("TEAMCRAFT_SECURITY__RATE_LIMITS__IS_ENABLED", "false");
  let Some(mut rig) = Rig::start_with(unlimited, &[]).await else {
    return;
  };
  let dm = rig.alice.client.open_dm(rig.carol.user_id()).await.unwrap();
  rig.carol.wait_joined(dm).await;
  rig
    .carol
    .client
    .send_text(dm, None, "the very first")
    .await
    .unwrap();
  for index in 0..OLDER_HISTORY_MESSAGES {
    rig
      .carol
      .client
      .send_text(dm, None, &format!("live {index}"))
      .await
      .unwrap();
  }
  rig
    .carol
    .client
    .send_text(dm, None, "hello from carol")
    .await
    .unwrap();
  rig.alice.wait_for_text(dm, "hello from carol").await;
  rig.alice.client.sync_account_data().await.unwrap();

  let page = rig.browser.page.clone();
  sign_in_as_alice(&page).await;
  wait_for_text(&page, "browser-channel").await;
  page.goto(format!("{WEB_ORIGIN}/dm/{dm}")).await.unwrap();
  // It opens at the "New messages" divider, far from the first message and from the newest one.
  wait_until_readable(&mut rig.alice, &mut rig.carol, &page, "live ").await;
  page.evaluate(PROBE).await.unwrap();

  // A page that fails says so at the top, and the reader stays there: no scrolling from here on,
  // so whatever moves on screen moved because rows arrived. A page of the server answers faster
  // than a scroll settles; this way it can't arrive before the probe looks.
  let deadline = Instant::now() + PAGING_DEADLINE;
  let mut total = FeedStats::default();
  while !body_text(&page).await.contains("the very first") {
    assert!(
      Instant::now() < deadline,
      "the beginning was not reached: {total:?}"
    );
    set_offline(&page, true).await;
    page.evaluate(SCROLL_TO_TOP).await.unwrap();
    if !wait_for_failure_or_beginning(&page).await {
      break;
    }
    page.evaluate(AFTER_THE_JUMP).await.unwrap();
    let round = read_one_page(&page).await;
    eprintln!("older history, one page: {round:?}");
    assert_still_while_reading(round);
    total = total.plus(round);
  }
  set_offline(&page, false).await;
  assert!(
    total.count_changes > 0,
    "the list never grew while the probe watched: {total:?}"
  );
  rig.finish().await;
}

/// Whether the page failed, as it should while offline; `false` when none was needed any more.
async fn wait_for_failure_or_beginning(page: &Page) -> bool {
  let deadline = Instant::now() + OLDER_PAGE_WAIT;
  loop {
    let text = body_text(page).await;
    if text.contains("Older messages could not be loaded") {
      return true;
    }
    if text.contains("the very first") {
      return false;
    }
    assert!(Instant::now() < deadline, "no failure shown:\n{text}");
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
}

/// Back online, the page is asked for again; watches until the list grew.
async fn read_one_page(page: &Page) -> FeedStats {
  // A few painted frames, so the probe knows what is on screen.
  tokio::time::sleep(Duration::from_millis(200)).await;
  let before = places_on_screen(page).await;
  assert!(!before.is_empty(), "no message on screen before the page");
  set_offline(page, false).await;
  page
    .evaluate("document.querySelector('.older-history-retry')?.click()")
    .await
    .unwrap();
  let deadline = Instant::now() + OLDER_PAGE_WAIT;
  let mut round = FeedStats::default();
  while Instant::now() < deadline && round.rows_added == 0 && round.count_changes == 0 {
    tokio::time::sleep(Duration::from_millis(500)).await;
    round = round.plus(take_stats(page).await);
  }
  // What the rows above settle to, measured after they arrived.
  tokio::time::sleep(SCROLL_SETTLE).await;
  let after = places_on_screen(page).await;
  let drift = before
    .iter()
    .filter_map(|(id, place)| after.get(id).map(|now| (now - place).abs()))
    .collect::<Vec<_>>();
  assert!(
    !drift.is_empty(),
    "nothing that was on screen still is: {before:?} became {after:?}"
  );
  assert!(
    drift.iter().all(|moved| *moved <= MAX_DRIFT_PX),
    "messages on screen went elsewhere: {before:?} became {after:?}"
  );
  round.plus(take_stats(page).await)
}

/// Where each message on screen ends, from the list's top, as the probe last saw it.
async fn places_on_screen(page: &Page) -> HashMap<String, f64> {
  let params = EvaluateParams::builder()
    .expression("window.feedProbe.places()")
    .return_by_value(true)
    .build()
    .unwrap();
  let json: String = page.evaluate(params).await.unwrap().into_value().unwrap();
  let places: Vec<(String, f64)> = serde_json::from_str(&json).unwrap();
  places.into_iter().collect()
}

/// [`assert_still`] for a list that the reader scrolled and that grows on purpose.
fn assert_still_while_reading(stats: FeedStats) {
  assert_eq!(stats.list_replaced, 0, "the list was replaced: {stats:?}");
  // One at most: the row that was the oldest, which loses its header to the rows before it.
  assert!(
    stats.visible_removed <= 1,
    "messages on screen were rebuilt: {stats:?}"
  );
  assert_eq!(stats.blank_frames, 0, "frames without any message: {stats:?}");
  assert_eq!(
    stats.skeleton_frames, 0,
    "placeholders showed on screen: {stats:?}"
  );
  assert_eq!(stats.moved_frames, 0, "messages on screen moved: {stats:?}");
}

async fn set_offline(page: &Page, is_offline: bool) {
  // An empty pattern applies to every request.
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

/// Nothing on screen was rebuilt, blanked or moved.
fn assert_still(phase: &str, stats: FeedStats) {
  assert!(stats.frames > 0, "{phase}: no frame was drawn");
  assert_eq!(
    stats.list_replaced, 0,
    "{phase}: the list was replaced: {stats:?}"
  );
  assert_eq!(
    stats.visible_removed, 0,
    "{phase}: messages on screen were rebuilt: {stats:?}"
  );
  assert_eq!(
    stats.skeleton_frames, 0,
    "{phase}: placeholders showed on screen: {stats:?}"
  );
  assert_eq!(
    stats.blank_frames, 0,
    "{phase}: frames without any message: {stats:?}"
  );
  assert_eq!(
    stats.moved_frames, 0,
    "{phase}: messages on screen moved: {stats:?}"
  );
}

/// Pumps Alice's other device (and Carol's) until it added the browser to the conversation's group.
async fn wait_until_readable(alice: &mut Peer, carol: &mut Peer, page: &Page, shown: &str) {
  let deadline = Instant::now() + SET_UP_DEADLINE;
  let mut maybe_clear_since = None;
  loop {
    let text = body_text(page).await;
    if text.contains(shown) && !text.contains("isn't fully set up") {
      let since: Instant = *maybe_clear_since.get_or_insert_with(Instant::now);
      if since.elapsed() >= Duration::from_secs(3) {
        return;
      }
    } else {
      maybe_clear_since = None;
    }
    assert!(
      Instant::now() < deadline,
      "the browser was never added to the DM:\n{text}"
    );
    alice.pump_reconnecting().await;
    alice.client.apply_pending_changes().await.unwrap();
    carol.pump_reconnecting().await;
    carol.client.apply_pending_changes().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}

/// `bob` may import into the DM of Alice and Carol, who are in it already, and is in its group.
async fn prepare_import(rig: &mut Rig, importer: &mut Peer, dm: ConversationId) {
  let (alice_id, carol_id, importer_id) = (rig.alice.user_id(), rig.carol.user_id(), importer.user_id());
  rig
    .alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner), (importer_id, RosterRole::Admin)])
    .await
    .unwrap();
  rig
    .alice
    .set_policy(|policy| {
      policy.admin_self_join = AdminSelfJoin::Visible;
      policy.import = ImportPolicy {
        maybe_importer: Some(importer_id),
        maybe_joins_until_ms: Some(i64::MAX),
      };
    })
    .await;
  settle(&mut [&mut rig.alice, &mut *importer], Duration::from_secs(1)).await;
  importer.client.full_sync().await.unwrap();
  let participants = vec![alice_id, carol_id];
  let response = importer
    .client
    .open_import_dm(participants.clone())
    .await
    .unwrap();
  assert_eq!(response.id, dm);
  importer.client.grant_imported_history(dm, &participants).unwrap();
  importer
    .client
    .set_up_import_dm(response, &participants)
    .await
    .unwrap();
  importer.client.request_import_join(dm).await.unwrap();
  // Alice's and Carol's devices commit the import account into the group.
  for _ in 0..20 {
    settle(
      &mut [&mut rig.alice, &mut rig.carol, &mut *importer],
      Duration::from_millis(500),
    )
    .await;
    rig.alice.client.apply_pending_changes().await.unwrap();
    rig.carol.client.apply_pending_changes().await.unwrap();
    if importer.client.has_group(dm).unwrap() {
      break;
    }
  }
  assert!(
    importer.client.has_group(dm).unwrap(),
    "the import account never joined"
  );
}

/// Watches until `until` holds, with `wait_until_quiet` and for some ticks without a new page.
async fn watch_paging(page: &Page, wait_until_quiet: bool, until: impl Fn(&FeedStats) -> bool) -> FeedStats {
  let deadline = Instant::now() + PAGING_DEADLINE;
  let mut total = FeedStats::default();
  let mut quiet_rounds = 0;
  loop {
    tokio::time::sleep(Duration::from_secs(1)).await;
    let stats = take_stats(page).await;
    quiet_rounds = if stats.count_changes == 0 {
      quiet_rounds + 1
    } else {
      0
    };
    total = total.plus(stats);
    let is_done = until(&total) && (quiet_rounds >= QUIET_ROUNDS || !wait_until_quiet);
    if is_done || Instant::now() > deadline {
      return total;
    }
  }
}

async fn take_stats(page: &Page) -> FeedStats {
  let params = EvaluateParams::builder()
    .expression("window.feedProbe.take()")
    .return_by_value(true)
    .build()
    .unwrap();
  let value: serde_json::Value = page.evaluate(params).await.unwrap().into_value().unwrap();
  let count = |name: &str| value[name].as_u64().unwrap_or(0);
  FeedStats {
    frames: count("frames"),
    msgs_removed: count("msgs_removed"),
    visible_removed: count("visible_removed"),
    rows_added: count("rows_added"),
    rows_removed: count("rows_removed"),
    skeleton_frames: count("skeleton_frames"),
    moved_frames: count("moved_frames"),
    max_move_px: value["max_move_px"].as_f64().unwrap_or(0.0),
    scroll_changes: count("scroll_changes"),
    list_replaced: count("list_replaced"),
    count_changes: count("count_changes"),
    max_index: count("max_index"),
    blank_frames: count("blank_frames"),
    day_shown_frames: count("day_shown_frames"),
  }
}

fn draft(index: usize) -> ImportDraft {
  let offset = i64::try_from(index).unwrap();
  ImportDraft {
    content: MessageContent::Text {
      body: format!("old {index}"),
      mentions: Vec::new(),
      meta: MessageMeta {
        maybe_imported: Some(Box::new(ImportedOrigin {
          source: ImportSource::Synology,
          source_id: index.to_string(),
          author: ImportedAuthor::Deleted,
          sent_at_ms: OLD_MS + offset * 60_000,
          maybe_edited_at_ms: None,
          reactions: Vec::new(),
          missing_files: Vec::new(),
        })),
        ..MessageMeta::default()
      },
    },
    maybe_thread_root: None,
  }
}
