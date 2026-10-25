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
  Rig, WEB_ORIGIN, body_text, click_text, shot, sign_in_as_alice, wait_for_element, wait_for_text,
};

/// Phones the layout is checked on: an iPhone 15 and a Pixel 8 Pro class device, both ways round.
const PHONES: [(i64, i64); 4] = [(390, 844), (844, 390), (412, 915), (915, 412)];
/// A tablet in landscape and in portrait: wide enough for the sidebar, but a finger is the pointer.
const TABLETS: [(i64, i64); 2] = [(1024, 768), (820, 1180)];
const WAIT: Duration = Duration::from_secs(30);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_web_client_shows_one_pane_at_a_time_on_phones() {
  let Some(rig) = Rig::start().await else { return };
  rig
    .browser
    .screenshot_on_failure("web-mobile", |page| async move {
      emulate_phone(&page, PHONES[0]).await;
      sign_in_as_alice(&page).await;
      wait_for_element(&page, ".bottom-nav").await;
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
      open_test_channel(&page).await;
      let composer = wait_for_element(&page, "textarea").await;
      composer
        .click()
        .await
        .unwrap()
        .type_str("first line")
        .await
        .unwrap()
        .press_key("Enter")
        .await
        .unwrap()
        .type_str("second line")
        .await
        .unwrap();
      assert_eq!(
        eval_string(&page, "document.querySelector('textarea').value").await,
        "first line\nsecond line"
      );
      page
        .find_element(".send-btn")
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
      wait_for_text(&page, "second line").await;
      assert_eq!(
        eval_string(&page, "document.querySelector('textarea').value").await,
        ""
      );
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

  click_text(page, "Home").await;
  wait_for_pane(page, "list").await;
  assert_no_sideways_scroll(page, &format!("home at {size}")).await;
  shots(page, &format!("mobile-home-{size}"), (width, height)).await;

  click_text(page, "Activity").await;
  wait_for_pane(page, "main").await;
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
  click_text(page, "Spaces").await;
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
  wait_for_element(page, "textarea").await;
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

/// Screenshots of the page in the light and the dark theme. A full-page screenshot resizes the
/// page and drops the device emulation, so the phone is emulated again after each.
async fn shots(page: &Page, name: &str, phone: (i64, i64)) {
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
    shot(page, &format!("{name}-{scheme}")).await;
    emulate_phone(page, phone).await;
  }
}

async fn wait_for_pane(page: &Page, pane: &str) {
  let deadline = Instant::now() + WAIT;
  loop {
    if eval_string(page, "document.querySelector('.app')?.dataset.pane ?? ''").await == pane {
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

async fn wait_for_gone(page: &Page, selector: &str) {
  let deadline = Instant::now() + WAIT;
  while is_shown(page, selector).await {
    assert!(Instant::now() < deadline, "{selector} is still shown");
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
}

async fn assert_no_sideways_scroll(page: &Page, place: &str) {
  let overflow = eval_string(
    page,
    "(() => { const wide = [...document.querySelectorAll('body *')].filter(e => { const r = e.getBoundingClientRect(); return r.width > 0 && r.right > innerWidth + 1 && getComputedStyle(e).position !== 'fixed'; }); return document.documentElement.scrollWidth > innerWidth ? 'page ' + document.documentElement.scrollWidth : wide.slice(0, 3).map(e => e.className).join(', '); })()",
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

async fn eval_f64(page: &Page, script: &str) -> f64 {
  page
    .evaluate(script)
    .await
    .ok()
    .and_then(|result| result.into_value::<f64>().ok())
    .unwrap_or_default()
}
