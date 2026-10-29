//! The web client on phones (issue #307): one pane at a time with the bottom navigation, back
//! through the browser history, a composer where Enter is a newline, and nothing that scrolls
//! sideways, in portrait and landscape. On a tablet the wide layout keeps finger-sized targets (#364)
//! and a channel header whose controls don't overlap (#365). Screenshots of the main views in light
//! and dark land in `target/e2e-artifacts/mobile-*.png`.

use std::time::{Duration, Instant};

use chromiumoxide::cdp::browser_protocol::emulation::{
  MediaFeature, SetDeviceMetricsOverrideParams, SetEmulatedMediaParams, SetTouchEmulationEnabledParams,
};
use chromiumoxide::page::Page;

use crate::web::{
  Rig, WEB_ORIGIN, body_text, sign_in_as_alice, wait_for_element, wait_for_text, wait_until_set_up,
};

/// Phones the layout is checked on: an iPhone 15 and a Pixel 8 Pro class device, both ways round.
const PHONES: [(i64, i64); 4] = [(390, 844), (844, 390), (412, 915), (915, 412)];
/// A tablet in landscape and in portrait: wide enough for the sidebar, but a finger is the pointer.
const TABLETS: [(i64, i64); 2] = [(1024, 768), (820, 1180)];
const WAIT: Duration = Duration::from_secs(30);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_web_client_shows_one_pane_at_a_time_on_phones() {
  let Some(mut rig) = Rig::start().await else { return };
  let (alice, carol) = (&mut rig.alice, &mut rig.carol);
  rig
    .browser
    .screenshot_on_failure("web-mobile", |page| async move {
      emulate_phone(&page, PHONES[0]).await;
      sign_in_as_alice(&page).await;
      wait_for_element(&page, ".bottom-nav").await;
      // The conversation as it is once Alice's other device let the browser in: without the
      // notice that it isn't set up yet, which a phone on its side has no room for.
      emulate_phone(&page, PHONES[0]).await;
      open_test_channel(&page).await;
      wait_until_set_up(alice, carol, &page).await;
      page.evaluate("history.back()").await.unwrap();
      wait_for_pane(&page, "list").await;
      for phone in PHONES {
        emulate_phone(&page, phone).await;
        check_phone(&page, phone).await;
      }
    })
    .await;
  rig.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn on_a_phone_enter_is_a_newline_and_the_send_button_sends() {
  let Some(rig) = Rig::start().await else { return };
  rig
    .browser
    .screenshot_on_failure("web-mobile-composer", |page| async move {
      emulate_phone(&page, PHONES[2]).await;
      sign_in_as_alice(&page).await;
      // Signing in turns touch off for the desktop tests (`prefer_mouse`); this is a phone again.
      emulate_phone(&page, PHONES[2]).await;
      open_test_channel(&page).await;
      let composer = wait_for_element(&page, "[data-composer]").await;
      composer
        .click()
        .await
        .unwrap()
        .type_str("first line")
        .await
        .unwrap();
      wait_for_composer(&page, "first line").await;
      composer
        .press_key("Enter")
        .await
        .unwrap()
        .type_str("second line")
        .await
        .unwrap();
      wait_for_composer(&page, "first line\nsecond line").await;
      assert_eq!(composer_lines(&page).await, "first line\nsecond line");
      page
        .find_element(".send-btn")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
      wait_for_text(&page, "second line").await;
      assert_eq!(composer_lines(&page).await, "");
    })
    .await;
  rig.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn on_a_tablet_targets_follow_the_finger_not_the_width() {
  let Some(rig) = Rig::start().await else { return };
  rig
    .browser
    .screenshot_on_failure("web-tablet", |page| async move {
      emulate_phone(&page, TABLETS[0]).await;
      sign_in_as_alice(&page).await;
      for tablet in TABLETS {
        emulate_phone(&page, tablet).await;
        check_tablet(&page, tablet).await;
      }
    })
    .await;
  rig.finish().await;
}

/// The list, the conversation and back again on one phone size, with screenshots in both themes.
async fn check_phone(page: &Page, (width, height): (i64, i64)) {
  let size = format!("{width}x{height}");
  assert_eq!(
    eval_string(page, "document.documentElement.dataset.layout").await,
    "compact",
    "{size}"
  );

  open_test_channel(page).await;
  wait_for_pane(page, "main").await;
  assert_no_sideways_scroll(page, &format!("conversation at {size}")).await;
  assert!(
    !is_shown(page, ".bottom-nav").await,
    "a conversation has no bottom navigation ({size})"
  );
  assert!(
    is_inside_viewport(page, ".composer-box").await,
    "the composer is on screen ({size})"
  );
  assert!(
    is_shown(page, ".compact-back").await,
    "the conversation has a back button ({size})"
  );
  shots(page, &format!("mobile-conversation-{size}"), (width, height)).await;

  // Browser back returns to the list (the routes of #250).
  page.evaluate("history.back()").await.unwrap();
  wait_for_pane(page, "list").await;
  assert!(
    is_shown(page, ".bottom-nav").await,
    "the list has the bottom navigation ({size})"
  );
  assert_no_sideways_scroll(page, &format!("channel list at {size}")).await;
  assert_touch_targets(page, ".bottom-nav-tab, .sb-item, .compact-back", &size).await;
  shots(page, &format!("mobile-channels-{size}"), (width, height)).await;

  click_tab(page, "Chats").await;
  wait_for_pane(page, "list").await;
  assert_no_sideways_scroll(page, &format!("chats at {size}")).await;
  shots(page, &format!("mobile-chats-{size}"), (width, height)).await;

  click_tab(page, "Activity").await;
  // Activity is a top-level place of its own (d70b4d0e), like Chats: the list pane with the tabs.
  wait_for_pane(page, "list").await;
  assert!(
    is_shown(page, ".bottom-nav").await,
    "Activity keeps the bottom navigation ({size})"
  );
  assert_no_sideways_scroll(page, &format!("activity at {size}")).await;
  shots(page, &format!("mobile-activity-{size}"), (width, height)).await;

  // The search is a full-screen sheet.
  page
    .find_element(".search-btn")
    .await
    .unwrap()
    .click()
    .await
    .unwrap();
  wait_for_element(page, ".modal-switcher").await;
  // The layout width: it scales in, which a bounding box would include.
  let sheet_width = eval_f64(page, "document.querySelector('.modal-switcher').offsetWidth").await;
  assert!(
    (sheet_width - width as f64).abs() < 1.0,
    "the search fills the width ({size}: {sheet_width})"
  );
  shots(page, &format!("mobile-search-{size}"), (width, height)).await;
  page
    .evaluate("document.querySelector('.switcher-input .compact-only').click()")
    .await
    .unwrap();
  wait_for_gone(page, ".modal-switcher").await;
}

/// A conversation on a tablet: not the phone layout, yet 44px targets because the pointer is coarse.
async fn check_tablet(page: &Page, (width, height): (i64, i64)) {
  let size = format!("{width}x{height}");
  assert_ne!(
    eval_string(page, "document.documentElement.dataset.layout").await,
    "compact",
    "{size}"
  );
  assert!(
    eval_f64(page, "matchMedia('(pointer: coarse)').matches ? 1 : 0").await > 0.0,
    "touch emulation gives a coarse pointer ({size})"
  );
  wait_for_text(page, "browser-channel").await;
  let opened = page
    .evaluate(
      "(() => { const row = [...document.querySelectorAll('.sidebar button')].find(e => e.innerText.includes('browser-channel')); if (!row) return false; row.click(); return true; })()",
    )
    .await
    .unwrap()
    .into_value::<bool>()
    .unwrap_or(false);
  assert!(opened, "no sidebar row for the test channel");
  wait_for_element(page, ".channel-header").await;
  assert_touch_targets(
    page,
    ".channel-header .icon-btn, .channel-header .header-btn, .composer .icon-btn, .send-btn, .sb-item",
    &size,
  )
  .await;
  assert_header_fits(page, &size).await;
  assert_no_sideways_scroll(page, &format!("conversation at {size}")).await;
  shots(page, &format!("tablet-conversation-{size}"), (width, height)).await;
}

/// Goes to the test channel through the Spaces tab.
async fn open_test_channel(page: &Page) {
  wait_for_element(page, ".bottom-nav").await;
  click_tab(page, "Spaces").await;
  wait_for_text(page, "browser-channel").await;
  // The sidebar row itself; its list item around it doesn't open anything.
  let opened = page
    .evaluate(
      "(() => { const row = [...document.querySelectorAll('.sidebar button')].find(e => e.innerText.includes('browser-channel')); if (!row) return false; row.click(); return true; })()",
    )
    .await
    .unwrap()
    .into_value::<bool>()
    .unwrap_or(false);
  assert!(opened, "no sidebar row for the test channel");
  wait_for_element(page, "[data-composer]").await;
}

async fn emulate_phone(page: &Page, (width, height): (i64, i64)) {
  // A fresh tab is still on about:blank, which has no window to measure.
  if page
    .url()
    .await
    .ok()
    .flatten()
    .is_none_or(|url| !url.starts_with(WEB_ORIGIN))
  {
    page.goto(format!("{WEB_ORIGIN}/healthz")).await.unwrap();
  }
  page
    .execute(SetDeviceMetricsOverrideParams::new(width, height, 3.0, true))
    .await
    .unwrap();
  page
    .execute(
      SetTouchEmulationEnabledParams::builder()
        .enabled(true)
        .max_touch_points(5)
        .build()
        .unwrap(),
    )
    .await
    .unwrap();
  // The layout follows media queries, which fire after the resize. A page without a viewport tag
  // (before the app loads) lays out 980px wide on a phone, so only the app is waited for.
  let has_viewport_tag =
    eval_f64(page, "document.querySelectorAll('meta[name=viewport]').length").await > 0.0;
  if !has_viewport_tag {
    return;
  }
  let deadline = Instant::now() + WAIT;
  while eval_f64(page, "innerWidth").await as i64 != width {
    assert!(
      Instant::now() < deadline,
      "the viewport did not become {width}x{height}"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
}

/// Screenshots of the screen in the light and the dark theme. Only the screen: a full-page
/// screenshot resizes the page, which for a moment is wide enough for the desktop layout, and that
/// opens a conversation where the phone showed the list.
async fn shots(page: &Page, name: &str, phone: (i64, i64)) {
  use chromiumoxide::page::ScreenshotParams;
  for scheme in ["light", "dark"] {
    page
      .execute(
        SetEmulatedMediaParams::builder()
          .feature(MediaFeature::new("prefers-color-scheme", scheme))
          .build(),
      )
      .await
      .unwrap();
    // Let the theme's colors settle (no transition is longer than this).
    tokio::time::sleep(Duration::from_millis(300)).await;
    let dir = teamcraft_e2e::browser::artifact_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = page
      .save_screenshot(
        ScreenshotParams::builder().build(),
        dir.join(format!("{name}-{scheme}.png")),
      )
      .await;
  }
  // The emulation stays, but make sure of the size the next steps measure.
  emulate_phone(page, phone).await;
}

async fn wait_for_pane(page: &Page, pane: &str) {
  let deadline = Instant::now() + WAIT;
  loop {
    // The pane slides in (`pane-push`): until it stopped, it sticks out sideways.
    let is_settled = eval_f64(
      page,
      "document.getAnimations().filter(a => a.playState === 'running' && isFinite(a.effect?.getComputedTiming().endTime)).length",
    )
    .await
      == 0.0;
    if is_settled && eval_string(page, "document.querySelector('.app')?.dataset.pane ?? ''").await == pane {
      return;
    }
    assert!(
      Instant::now() < deadline,
      "the {pane} pane did not show ({}); the page says:\n{}",
      eval_string(page, "location.pathname + ' pane=' + (document.querySelector('.app')?.dataset.pane ?? '-') + ' depth=' + history.state").await,
      body_text(page).await
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
}

/// Opens a tab of the bottom navigation (Chats, Activity, Spaces, You).
async fn click_tab(page: &Page, label: &str) {
  let script = format!(
    "(() => {{ const tab = [...document.querySelectorAll('.bottom-nav-tab')].find(e => e.innerText.includes({label:?})); if (!tab) return false; tab.click(); return true; }})()"
  );
  let deadline = Instant::now() + WAIT;
  while !page
    .evaluate(script.as_str())
    .await
    .ok()
    .and_then(|result| result.into_value::<bool>().ok())
    .unwrap_or(false)
  {
    assert!(
      Instant::now() < deadline,
      "no bottom navigation tab {label:?} ({})",
      eval_string(page, "location.pathname + ' pane=' + (document.querySelector('.app')?.dataset.pane ?? '-') + ' ' + innerWidth + 'x' + innerHeight").await
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
}

async fn wait_for_gone(page: &Page, selector: &str) {
  let deadline = Instant::now() + WAIT;
  while is_shown(page, selector).await {
    assert!(Instant::now() < deadline, "{selector} is still shown");
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
}

/// Nothing reaches past the right edge of the screen. Content of a row that scrolls sideways on its
/// own (the sidebar's chips on a phone) or clips may, as long as that row fits.
async fn assert_no_sideways_scroll(page: &Page, place: &str) {
  let overflow = eval_string(
    page,
    "(() => { \
       const isOut = e => e.getBoundingClientRect().right > innerWidth + 1; \
       const isContained = e => { for (let p = e.parentElement; p && p !== document.body; p = p.parentElement) { \
         if (getComputedStyle(p).overflowX !== 'visible' && !isOut(p)) return true; } return false; }; \
       const wide = [...document.querySelectorAll('body *')].filter(e => e.getBoundingClientRect().width > 0 && isOut(e) \
         && getComputedStyle(e).position !== 'fixed' && !isContained(e)); \
       return document.documentElement.scrollWidth > innerWidth ? 'page ' + document.documentElement.scrollWidth \
         : wide.slice(0, 3).map(e => (e.getAttribute('class') || e.tagName) + ' (right edge ' + Math.round(e.getBoundingClientRect().right) + ' of ' + innerWidth + ')').join(', '); \
     })()",
  )
  .await;
  assert!(
    overflow.is_empty(),
    "something sticks out sideways in the {place}: {overflow}"
  );
}

/// Every visible match of `selector` is at least 44 CSS pixels high (WCAG 2.5.5).
async fn assert_touch_targets(page: &Page, selector: &str, size: &str) {
  let script = format!(
    "[...document.querySelectorAll({selector:?})].filter(e => e.offsetParent && e.getBoundingClientRect().height < 44).map(e => e.className + ' ' + Math.round(e.getBoundingClientRect().height)).join('; ')"
  );
  let small = eval_string(page, &script).await;
  assert!(small.is_empty(), "touch targets under 44px at {size}: {small}");
}

/// Nothing in the channel header overlaps or sticks out of it (#365).
async fn assert_header_fits(page: &Page, size: &str) {
  let problems = eval_string(
    page,
    "(() => { const h = document.querySelector('.channel-header'); const hr = h.getBoundingClientRect(); const items = [...h.querySelectorAll('.header-title, .header-btn, .icon-btn, [role=tab], .conversation-more')].filter(e => e.getClientRects().length > 0 && getComputedStyle(e).display !== 'none'); const found = []; items.forEach((a, i) => { const ra = a.getBoundingClientRect(); if (ra.left < hr.left - 0.5 || ra.right > hr.right + 0.5) found.push('outside: ' + a.className); items.slice(i + 1).forEach(b => { if (a.contains(b) || b.contains(a)) return; const rb = b.getBoundingClientRect(); if (ra.left < rb.right - 0.5 && rb.left < ra.right - 0.5 && ra.top < rb.bottom && rb.top < ra.bottom) found.push(a.className + ' over ' + b.className); }); }); return found.join('; '); })()",
  )
  .await;
  assert!(
    problems.is_empty(),
    "the channel header overlaps at {size}: {problems}"
  );
}

async fn is_shown(page: &Page, selector: &str) -> bool {
  let script = format!(
    "(() => {{ const e = document.querySelector({selector:?}); return !!e && e.getClientRects().length > 0 && getComputedStyle(e).display !== 'none'; }})()"
  );
  page
    .evaluate(script)
    .await
    .ok()
    .and_then(|result| result.into_value::<bool>().ok())
    .unwrap_or(false)
}

async fn is_inside_viewport(page: &Page, selector: &str) -> bool {
  let script = format!(
    "(() => {{ const e = document.querySelector({selector:?}); if (!e) return false; const r = e.getBoundingClientRect(); return r.top >= 0 && r.bottom <= innerHeight && r.left >= 0 && r.right <= innerWidth; }})()"
  );
  page
    .evaluate(script)
    .await
    .ok()
    .and_then(|result| result.into_value::<bool>().ok())
    .unwrap_or(false)
}

async fn eval_string(page: &Page, script: &str) -> String {
  page
    .evaluate(script)
    .await
    .ok()
    .and_then(|result| result.into_value::<String>().ok())
    .unwrap_or_default()
}

/// The lines in the composer (an editor whose lines are paragraphs, not a text field), one per
/// line: without the blank lines between paragraphs and the line break after the last one.
async fn composer_lines(page: &Page) -> String {
  eval_string(
    page,
    "document.querySelector('[data-composer]').innerText.replace(/\\n+/g, '\\n').replace(/\\n$/, '')",
  )
  .await
}

/// Waits until the composer holds `text` (typing reaches the editor a moment later).
async fn wait_for_composer(page: &Page, text: &str) {
  let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
  loop {
    let lines = composer_lines(page).await;
    if lines == text {
      return;
    }
    assert!(
      std::time::Instant::now() < deadline,
      "the composer holds {lines:?}, not {text:?}"
    );
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
  }
}

async fn eval_f64(page: &Page, script: &str) -> f64 {
  page
    .evaluate(script)
    .await
    .ok()
    .and_then(|result| result.into_value::<f64>().ok())
    .unwrap_or_default()
}
