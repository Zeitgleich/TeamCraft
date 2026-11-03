//! The web client starts from the cached account (issue #778): the stored chats are on screen at
//! once while the sign-in is checked beside them, a network failure signs nobody out, and what the
//! server or the identity provider refuses still ends the session, visibly.
//!
//! The network is simulated in the page's own browser (DevTools): requests to the server's API and
//! to the identity provider are held back or fail, the app's files keep coming from the server. One
//! real test server per run.
//!
//! The first part measures: signed-in start until the channel list is usable, on a good network, with
//! every API and identity provider request a second late, and without them. `CACHED_START_RUNS` says
//! how many starts per case (default 3), `CACHED_START_MEASURE_ONLY=1` stops after the measurement (to
//! measure a build from before this start existed):
//!
//! ```sh
//! CACHED_START_RUNS=7 cargo test -p teamcraft-e2e --features browser --test suite -- --nocapture cached_start::
//! ```

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chromiumoxide::cdp::browser_protocol::fetch::{
  ContinueRequestParams, DisableParams as FetchDisableParams, EnableParams as FetchEnableParams,
  EventRequestPaused, FulfillRequestParams, HeaderEntry, RequestPattern,
};
use chromiumoxide::cdp::browser_protocol::network::{
  BlockPattern, ClearBrowserCookiesParams, EnableParams as NetworkEnableParams, EventWebSocketCreated,
  SetBlockedUrLsParams,
};
use chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams;
use chromiumoxide::cdp::js_protocol::runtime::EventConsoleApiCalled;
use chromiumoxide::page::Page;
use futures_util::StreamExt;
use teamcraft_e2e::browser::{BrowserSession, lease_web_port, web_dir};
use teamcraft_e2e::scenario::space_with;
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::UserId;

use crate::perf::{sign_in_and_unlock, sign_in_at_keycloak};
use crate::web::{TEST_CSP, body_text, wait_for_element, wait_for_text};

const PORT: u16 = 8083;
const WEB_ORIGIN: &str = "http://127.0.0.1:8083";
const CHANNEL: &str = "start-from-cache";
const MESSAGE: &str = "this message waits on the device";
/// Everything that needs an answer from the server or the identity provider. The app's own files
/// (the page, the wasm, its chunks) are not behind these.
const API_PATTERNS: [&str; 2] = ["*/api/v1/*", "http://127.0.0.1:8080/*"];
/// The same as URL patterns, the WebSocket of the gateway included.
const BLOCKED_URLS: [&str; 2] = ["*://*:*/api/v1/*", "http://127.0.0.1:8080/*"];
/// What a start without an answer may take before the test calls it broken; the budget is 1.5 s
/// (.claude/rules/robustness.md), measured on the same machine by the numbers this test prints.
const START_LIMIT: Duration = Duration::from_secs(15);
const HELD_BACK: Duration = Duration::from_secs(1);
/// Every page load turns the page's clock an hour further on before the app runs: the saved access
/// token has always run out, so a start has to refresh it, as after every night. It also keeps the
/// gateway from connecting on an old token while the test pretends there is no network: DevTools can
/// block requests, but not the page's WebSocket.
const CLOCK_RUNS_ON: &str = "try { \
    const hours = Number(localStorage.getItem('e2e-clock-hours') || 0) + 1; \
    localStorage.setItem('e2e-clock-hours', hours); \
    const realNow = Date.now.bind(Date); \
    Date.now = () => realNow() + hours * 3600000; \
  } catch (_) {}";

/// What a measured start ended with.
#[derive(Clone, Copy, Debug, PartialEq)]
enum StartEnd {
  ChannelList,
  SignInPage,
  Neither,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_signed_in_start_needs_no_answer_from_anybody() {
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
    .with_env("TEAMCRAFT_WEB__CONTENT_SECURITY_POLICY", TEST_CSP)
    .start()
    .await
  else {
    return;
  };
  let Some(browser) = BrowserSession::launch_for_measurements().await else {
    return;
  };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let bob = Peer::enroll(&stack, "bob").await;
  drop(Peer::enroll(&stack, "carol").await);
  let carol_tokens = stack.tokens("carol");
  let carol: UserId = stack.get(&carol_tokens, "/me").await.id().parse().unwrap();
  let space = space_with(&stack, &alice, "Space", &[(&bob, "member")]).await;
  stack
    .add_space_member(&alice.tokens, &space.to_string(), &carol.to_string(), "member")
    .await;

  let stack_ref = &stack;
  let browser_ref = &browser;
  let alice_ref = &mut alice;
  browser
    .screenshot_on_failure("cached-start", |page| async move {
      sign_in_and_unlock(&page, "carol").await;
      // After the browser's device exists, so it gets its welcome without anybody's help.
      let channel = alice_ref.client.create_channel(space, CHANNEL).await.unwrap();
      alice_ref.client.add_member(channel, carol).await.unwrap();
      alice_ref.client.send_text(channel, None, MESSAGE).await.unwrap();
      page.goto(format!("{WEB_ORIGIN}/c/{channel}")).await.unwrap();
      wait_for_text(&page, MESSAGE).await;
      // The account is written after the start; give it its moment before the page goes.
      tokio::time::sleep(Duration::from_secs(2)).await;
      measure_starts(&page, channel.to_string()).await;
      if std::env::var("CACHED_START_MEASURE_ONLY").is_ok() {
        return;
      }
      offline_start_shows_the_chats_and_connects_later(&page, browser_ref, alice_ref, channel).await;
      provider_hiccup_keeps_the_session(&page, channel.to_string()).await;
      proxy_401_keeps_the_session(&page, channel.to_string()).await;
      nothing_goes_out_before_the_answer(&page, channel.to_string()).await;
      refused_refresh_token_goes_to_the_identity_provider(&page, channel.to_string()).await;
      another_account_in_the_answer_closes_the_data(&page, channel.to_string()).await;
      ended_session_says_so(&page, channel.to_string()).await;
      hung_answer_is_given_up(&page, channel.to_string()).await;
      refresh_token_stays_away_from_another_provider(&page, channel.to_string()).await;
      deactivation_while_offline_ends_the_session(
        &page,
        channel.to_string(),
        stack_ref,
        &alice_ref.tokens,
        carol,
      )
      .await;
    })
    .await;
  browser.close().await;
  stack.stop().await;
}

/// Start until the channel list shows, per network. Prints the medians.
async fn measure_starts(page: &Page, channel: String) {
  let runs: usize = std::env::var("CACHED_START_RUNS")
    .ok()
    .and_then(|value| value.parse().ok())
    .unwrap_or(3);
  page.execute(NetworkEnableParams::default()).await.unwrap();
  add_script(page, CLOCK_RUNS_ON).await;
  let url = format!("{WEB_ORIGIN}/c/{channel}");

  let connected = watch_gateway_connected(page).await;
  let mut good = Vec::new();
  let mut until_connected = Vec::new();
  for _ in 0..runs {
    *connected.lock().unwrap() = None;
    let started = Instant::now();
    good.push(start_until_list(page, &url).await);
    until_connected.push(wait_for_connection(&connected, started).await);
  }
  report("good network", &good);
  report_connected("good network", until_connected);

  let held = hold_back(page).await;
  let mut slow = Vec::new();
  let mut until_connected = Vec::new();
  for _ in 0..runs {
    *connected.lock().unwrap() = None;
    let started = Instant::now();
    slow.push(start_until_list(page, &url).await);
    until_connected.push(wait_for_connection(&connected, started).await);
  }
  held.stop(page).await;
  report("every request 1 s late", &slow);
  report_connected("every request 1 s late", until_connected);

  block_the_network(page).await;
  let mut absent = Vec::new();
  for _ in 0..runs {
    absent.push(start_until_list(page, &url).await);
  }
  unblock_the_network(page).await;
  report("no answer", &absent);

  // Leave a page that talks to the server, for the account to be written again where it went.
  page.goto(url).await.unwrap();
  wait_for_text(page, CHANNEL).await;
  tokio::time::sleep(Duration::from_secs(2)).await;
}

/// When the core logged its gateway connection, each time.
async fn watch_gateway_connected(page: &Page) -> Arc<Mutex<Option<Instant>>> {
  let connected = Arc::new(Mutex::new(None));
  let noted = connected.clone();
  let mut console = page.event_listener::<EventConsoleApiCalled>().await.unwrap();
  tokio::spawn(async move {
    while let Some(event) = console.next().await {
      let is_connected = event
        .args
        .iter()
        .filter_map(|arg| arg.value.as_ref())
        .any(|value| value.to_string().contains("gateway connected"));
      if is_connected {
        noted.lock().unwrap().get_or_insert_with(Instant::now);
      }
    }
  });
  connected
}

async fn wait_for_connection(connected: &Mutex<Option<Instant>>, started: Instant) -> Duration {
  let deadline = Instant::now() + Duration::from_secs(30);
  loop {
    if let Some(at) = *connected.lock().unwrap() {
      return at.saturating_duration_since(started);
    }
    assert!(Instant::now() < deadline, "the gateway never connected");
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
}

fn report_connected(name: &str, samples: Vec<Duration>) {
  let mut millis: Vec<u128> = samples.iter().map(Duration::as_millis).collect();
  millis.sort_unstable();
  let median = millis.get(millis.len() / 2).copied().unwrap_or(0);
  eprintln!("cached start: {name}: connected, median {median} ms, all {millis:?} ms");
}

fn report(name: &str, samples: &[(StartEnd, Duration)]) {
  let mut millis: Vec<u128> = samples.iter().map(|(_, took)| took.as_millis()).collect();
  millis.sort_unstable();
  let median = millis.get(millis.len() / 2).copied().unwrap_or(0);
  let ends: Vec<StartEnd> = samples.iter().map(|(end, _)| *end).collect();
  eprintln!("cached start: {name}: median {median} ms, all {millis:?} ms, ended {ends:?}");
}

/// Loads the app and waits for the channel list, the sign-in page or the end of the patience.
async fn start_until_list(page: &Page, url: &str) -> (StartEnd, Duration) {
  page.goto("about:blank").await.unwrap();
  let started = Instant::now();
  page.goto(url).await.unwrap();
  let deadline = started + Duration::from_secs(30);
  loop {
    let text = body_text(page).await;
    if text.contains(CHANNEL) {
      return (StartEnd::ChannelList, started.elapsed());
    }
    if text.contains("Sign in") {
      return (StartEnd::SignInPage, started.elapsed());
    }
    if Instant::now() > deadline {
      return (StartEnd::Neither, started.elapsed());
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
}

/// No answer from anybody: the chats show, the connection line says what is going on and nobody is
/// signed out. The network comes back: the same page connects and new messages arrive.
async fn offline_start_shows_the_chats_and_connects_later(
  page: &Page,
  browser: &BrowserSession,
  alice: &mut Peer,
  channel: teamcraft_proto::ConversationId,
) {
  block_the_network(page).await;
  let (end, took) = start_until_list(page, &format!("{WEB_ORIGIN}/c/{channel}")).await;
  assert_eq!(end, StartEnd::ChannelList, "after {took:?}");
  assert!(took < START_LIMIT, "{took:?}");
  wait_for_text(page, MESSAGE).await;
  wait_for_element(page, ".connection-hint").await;
  tokio::time::sleep(Duration::from_secs(5)).await;
  let text = body_text(page).await;
  assert!(text.contains(CHANNEL) && !text.contains("Sign in"), "{text}");

  // A second tab, offline too: it finds the first one, not the sign-in page.
  let second = browser.new_page("about:blank").await;
  second.execute(NetworkEnableParams::default()).await.unwrap();
  block_the_network(&second).await;
  second.goto(format!("{WEB_ORIGIN}/c/{channel}")).await.unwrap();
  wait_for_text(&second, "already open in another tab").await;
  second.close().await.unwrap();

  let newer = "written while the device was away";
  alice.client.send_text(channel, None, newer).await.unwrap();
  unblock_the_network(page).await;
  wait_for_text(page, newer).await;
  wait_until_gone(page, ".connection-hint").await;
}

/// The identity provider answers a refresh with something other than `invalid_grant`, with and without
/// a body the OAuth client reads. Nobody is signed out: its cookies are gone, so tokens lost here would
/// end at its login page.
async fn provider_hiccup_keeps_the_session(page: &Page, channel: String) {
  page.execute(ClearBrowserCookiesParams::default()).await.unwrap();
  let url = format!("{WEB_ORIGIN}/c/{channel}");
  for (status, body) in [
    (503, r#"{"error":"temporarily_unavailable"}"#),
    (500, r#"{"error":"server_error"}"#),
    (502, "<html>Bad gateway</html>"),
  ] {
    let hiccup = answer_refresh_with(page, status, body).await;
    let (end, took) = start_until_list(page, &url).await;
    assert_eq!(end, StartEnd::ChannelList, "{status} {body} after {took:?}");
    tokio::time::sleep(Duration::from_secs(3)).await;
    let text = body_text(page).await;
    assert!(text.contains(MESSAGE), "{status} {body}: {text}");
    assert!(
      page.find_element("input[name=username]").await.is_err(),
      "{status} {body}"
    );
    hiccup.stop(page).await;
  }
  // The same tokens work once the provider does.
  let (end, took) = start_until_list(page, &url).await;
  assert_eq!(end, StartEnd::ChannelList, "after {took:?}");
  wait_until_gone(page, ".connection-hint").await;
  tokio::time::sleep(Duration::from_secs(2)).await;
}

/// A proxy answers `GET /me` with its own 401 page: the saved tokens and the cached account stay, which
/// the next start without any network shows.
async fn proxy_401_keeps_the_session(page: &Page, channel: String) {
  let url = format!("{WEB_ORIGIN}/c/{channel}");
  let proxy = intercept(page, &["*/api/v1/me"], |page, event| async move {
    fulfill(
      &page,
      &event,
      401,
      "text/html",
      "<html>Proxy authentication required</html>",
    )
    .await;
  })
  .await;
  let (end, took) = start_until_list(page, &url).await;
  assert_eq!(end, StartEnd::ChannelList, "after {took:?}");
  tokio::time::sleep(Duration::from_secs(3)).await;
  let text = body_text(page).await;
  assert!(text.contains(MESSAGE), "{text}");
  proxy.stop(page).await;

  block_the_network(page).await;
  let (end, took) = start_until_list(page, &url).await;
  assert_eq!(end, StartEnd::ChannelList, "after {took:?}");
  unblock_the_network(page).await;
  wait_until_gone(page, ".connection-hint").await;
  tokio::time::sleep(Duration::from_secs(2)).await;
}

/// Until the server answered `GET /me`, nothing with the session's token goes out and no gateway
/// connection starts: the session's user may not be the cached one. The chats show meanwhile.
async fn nothing_goes_out_before_the_answer(page: &Page, channel: String) {
  let url = format!("{WEB_ORIGIN}/c/{channel}");
  let is_answered = Arc::new(AtomicBool::new(false));
  let signed_requests = Arc::new(Mutex::new(Vec::<String>::new()));
  let sockets = Arc::new(Mutex::new(Vec::<String>::new()));
  let mut created = page.event_listener::<EventWebSocketCreated>().await.unwrap();
  let (seen_sockets, seen_answer) = (sockets.clone(), is_answered.clone());
  tokio::spawn(async move {
    while let Some(event) = created.next().await {
      if !seen_answer.load(Ordering::SeqCst) {
        seen_sockets.lock().unwrap().push(event.url.clone());
      }
    }
  });
  let (noted, answer) = (signed_requests.clone(), is_answered.clone());
  let hold = intercept(page, &["*/api/v1/*"], move |page, event| {
    let (noted, answer) = (noted.clone(), answer.clone());
    async move {
      if event.request.url.ends_with("/api/v1/me") {
        tokio::time::sleep(Duration::from_secs(8)).await;
        answer.store(true, Ordering::SeqCst);
      } else if !answer.load(Ordering::SeqCst)
        && serde_json::to_string(&event.request.headers)
          .unwrap()
          .to_lowercase()
          .contains("authorization")
      {
        noted.lock().unwrap().push(event.request.url.clone());
      }
      let _ = page
        .execute(ContinueRequestParams::new(event.request_id.clone()))
        .await;
    }
  })
  .await;
  let (end, took) = start_until_list(page, &url).await;
  assert_eq!(end, StartEnd::ChannelList, "after {took:?}");
  assert!(
    !is_answered.load(Ordering::SeqCst),
    "the chats waited for the answer: {took:?}"
  );
  let deadline = Instant::now() + Duration::from_secs(30);
  while !is_answered.load(Ordering::SeqCst) {
    assert!(Instant::now() < deadline, "GET /me never went out");
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
  assert_eq!(*signed_requests.lock().unwrap(), Vec::<String>::new());
  assert_eq!(*sockets.lock().unwrap(), Vec::<String>::new());
  hold.stop(page).await;
  wait_until_gone(page, ".connection-hint").await;
  tokio::time::sleep(Duration::from_secs(2)).await;
}

/// The server says the session ended: the chats go, the screen says why in plain words, and signing
/// in again through the provider's session brings them back.
async fn ended_session_says_so(page: &Page, channel: String) {
  let url = format!("{WEB_ORIGIN}/c/{channel}");
  let ended = intercept(page, &["*/api/v1/me"], |page, event| async move {
    let body = r#"{"error_code":"session_ended","message":"ended"}"#;
    fulfill(&page, &event, 401, "application/json", body).await;
  })
  .await;
  page.goto(url.clone()).await.unwrap();
  wait_for_text(page, "You were signed out").await;
  let text = body_text(page).await;
  assert!(!text.contains(CHANNEL) && !text.contains(MESSAGE), "{text}");
  ended.stop(page).await;
  page.goto(url).await.unwrap();
  sign_in_again_if_asked(page).await;
  tokio::time::sleep(Duration::from_secs(2)).await;
}

/// The tokens are gone, and the app goes through the identity provider: back in by the provider's own
/// session or by its login page, which either may show. Whichever it is, the chats return.
async fn sign_in_again_if_asked(page: &Page) {
  let deadline = Instant::now() + Duration::from_secs(30);
  while !body_text(page).await.contains(MESSAGE) {
    assert!(Instant::now() < deadline, "no way back in");
    if page.find_element("input[name=username]").await.is_ok() {
      // The login page knows the last account (issue #448); typed after it, the name would be wrong.
      page
        .evaluate("document.querySelector('input[name=username]').value = ''")
        .await
        .unwrap();
      sign_in_at_keycloak(page, "carol").await;
      wait_for_text(page, MESSAGE).await;
      return;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}

/// `GET /me` never comes back: the check asks again by itself and the app connects.
async fn hung_answer_is_given_up(page: &Page, channel: String) {
  let url = format!("{WEB_ORIGIN}/c/{channel}");
  let requests = Arc::new(AtomicUsize::new(0));
  let counted = requests.clone();
  let hang = intercept(page, &["*/api/v1/me"], move |page, event| {
    let counted = counted.clone();
    async move {
      if counted.fetch_add(1, Ordering::SeqCst) > 0 {
        let _ = page
          .execute(ContinueRequestParams::new(event.request_id.clone()))
          .await;
      }
    }
  })
  .await;
  let (end, took) = start_until_list(page, &url).await;
  assert_eq!(end, StartEnd::ChannelList, "after {took:?}");
  let deadline = Instant::now() + Duration::from_secs(25);
  while requests.load(Ordering::SeqCst) < 2 {
    assert!(Instant::now() < deadline, "the check never asked again");
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
  wait_until_gone(page, ".connection-hint").await;
  hang.stop(page).await;
  tokio::time::sleep(Duration::from_secs(2)).await;
}

/// The server names another identity provider. Nothing of the saved sign-in reaches it: the new provider's
/// token endpoint never hears from the app, and the app goes there to sign in like anybody.
async fn refresh_token_stays_away_from_another_provider(page: &Page, channel: String) {
  let url = format!("{WEB_ORIGIN}/c/{channel}");
  let moved = "http://127.0.0.1:8080/realms/moved";
  let answer = page
    .evaluate("fetch('/api/v1/server-info').then(answer => answer.text())")
    .await
    .unwrap()
    .into_value::<String>()
    .unwrap();
  let mut info: serde_json::Value = serde_json::from_str(&answer).unwrap();
  info["sign_in"]["issuer"] = serde_json::json!(moved);
  let info = info.to_string();
  let discovery = serde_json::json!({
    "issuer": moved,
    "authorization_endpoint": format!("{moved}/auth"),
    "token_endpoint": format!("{moved}/token"),
    "end_session_endpoint": format!("{moved}/logout"),
  })
  .to_string();
  let token_requests = Arc::new(Mutex::new(Vec::<String>::new()));
  let noted = token_requests.clone();
  let other = intercept(
    page,
    &["*/api/v1/server-info", "http://127.0.0.1:8080/realms/moved/*"],
    move |page, event| {
      let (info, discovery, noted) = (info.clone(), discovery.clone(), noted.clone());
      async move {
        let request_url = event.request.url.clone();
        if request_url.ends_with("/api/v1/server-info") {
          fulfill(&page, &event, 200, "application/json", &info).await;
        } else if request_url.ends_with("/.well-known/openid-configuration") {
          fulfill(&page, &event, 200, "application/json", &discovery).await;
        } else if request_url.contains("/auth") {
          fulfill(&page, &event, 200, "text/html", "<html>Moved provider</html>").await;
        } else {
          noted.lock().unwrap().push(request_url);
          fulfill(
            &page,
            &event,
            400,
            "application/json",
            r#"{"error":"invalid_grant"}"#,
          )
          .await;
        }
      }
    },
  )
  .await;
  page.goto(url.clone()).await.unwrap();
  wait_for_text(page, "Moved provider").await;
  assert_eq!(*token_requests.lock().unwrap(), Vec::<String>::new());
  other.stop(page).await;

  // Back, with the tokens gone: a sign-in, through the provider's session or its login page.
  page.goto(format!("{WEB_ORIGIN}/healthz")).await.unwrap();
  page.evaluate("sessionStorage.clear()").await.unwrap();
  page.goto(url).await.unwrap();
  sign_in_again_if_asked(page).await;
  tokio::time::sleep(Duration::from_secs(2)).await;
}

/// The identity provider refuses the refresh token: the app goes to the provider, which has no
/// session any more either, so its login page shows, and signing in there brings the chats back.
async fn refused_refresh_token_goes_to_the_identity_provider(page: &Page, channel: String) {
  let refusal = refuse_refresh_tokens(page).await;
  page.execute(ClearBrowserCookiesParams::default()).await.unwrap();
  page.goto(format!("{WEB_ORIGIN}/c/{channel}")).await.unwrap();
  wait_for_element(page, "input[name=username]").await;
  refusal.stop(page).await;
  // The login page knows the last account (issue #448); typed after it, the name would be wrong.
  page
    .evaluate("document.querySelector('input[name=username]').value = ''")
    .await
    .unwrap();
  sign_in_at_keycloak(page, "carol").await;
  wait_for_text(page, CHANNEL).await;
  tokio::time::sleep(Duration::from_secs(2)).await;
}

/// The server names another account for this session: the data of the cached one goes, nothing of
/// it stays on screen, and the next start doesn't begin from it.
async fn another_account_in_the_answer_closes_the_data(page: &Page, channel: String) {
  let other = serve_another_account(page).await;
  page.goto(format!("{WEB_ORIGIN}/c/{channel}")).await.unwrap();
  // The channel list may flash up first; what counts is that it goes.
  wait_for_text(page, "Stranger").await;
  let text = body_text(page).await;
  assert!(!text.contains(CHANNEL) && !text.contains(MESSAGE), "{text}");
  other.stop(page).await;
  page.goto(format!("{WEB_ORIGIN}/c/{channel}")).await.unwrap();
  wait_for_text(page, MESSAGE).await;
  tokio::time::sleep(Duration::from_secs(2)).await;
}

/// An admin deactivates the account while the device is offline. The chats show; when the
/// network is back the server's answer takes them off the screen, and the screen says why.
async fn deactivation_while_offline_ends_the_session(
  page: &Page,
  channel: String,
  stack: &Stack,
  admin: &teamcraft_e2e::tokens::UserTokens,
  carol: UserId,
) {
  block_the_network(page).await;
  let (end, took) = start_until_list(page, &format!("{WEB_ORIGIN}/c/{channel}")).await;
  assert_eq!(end, StartEnd::ChannelList, "after {took:?}");
  stack
    .post(
      admin,
      &format!("/admin/users/{carol}/deactivate"),
      serde_json::json!({}),
    )
    .await;
  unblock_the_network(page).await;
  wait_for_text(page, "Your account is deactivated").await;
  let text = body_text(page).await;
  assert!(!text.contains(CHANNEL) && !text.contains(MESSAGE), "{text}");
}

async fn add_script(page: &Page, source: &str) {
  page
    .execute(AddScriptToEvaluateOnNewDocumentParams::new(source))
    .await
    .unwrap();
}

async fn block_the_network(page: &Page) {
  let patterns = BLOCKED_URLS.map(|url| BlockPattern::new(url, true));
  let params = SetBlockedUrLsParams::builder().url_patterns(patterns).build();
  page.execute(params).await.unwrap();
}

async fn unblock_the_network(page: &Page) {
  let params = SetBlockedUrLsParams::builder()
    .url_patterns(Vec::<BlockPattern>::new())
    .build();
  page.execute(params).await.unwrap();
}

async fn wait_until_gone(page: &Page, selector: &str) {
  let deadline = Instant::now() + Duration::from_secs(60);
  while page.find_element(selector).await.is_ok() {
    assert!(Instant::now() < deadline, "{selector} is still there");
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}

/// Requests paused in the page until dropped through [`Self::stop`].
struct Interception {
  is_running: Arc<AtomicBool>,
}

impl Interception {
  async fn stop(self, page: &Page) {
    self.is_running.store(false, Ordering::SeqCst);
    page.execute(FetchDisableParams::default()).await.unwrap();
  }
}

/// Starts pausing the requests of `patterns`; `handle` answers each.
async fn intercept<F, Fut>(page: &Page, patterns: &[&str], handle: F) -> Interception
where
  F: Fn(Page, Arc<EventRequestPaused>) -> Fut + Send + Sync + 'static,
  Fut: Future<Output = ()> + Send + 'static,
{
  let mut paused = page.event_listener::<EventRequestPaused>().await.unwrap();
  let is_running = Arc::new(AtomicBool::new(true));
  let still_running = is_running.clone();
  let page_for_events = page.clone();
  tokio::spawn(async move {
    while let Some(event) = paused.next().await {
      if !still_running.load(Ordering::SeqCst) {
        return;
      }
      tokio::spawn(handle(page_for_events.clone(), event));
    }
  });
  let patterns = patterns
    .iter()
    .map(|pattern| RequestPattern {
      url_pattern: Some((*pattern).to_owned()),
      resource_type: None,
      request_stage: None,
    })
    .collect();
  page
    .execute(FetchEnableParams {
      patterns: Some(patterns),
      handle_auth_requests: None,
    })
    .await
    .unwrap();
  Interception { is_running }
}

/// Every request to the server's API and the identity provider comes back a second late.
async fn hold_back(page: &Page) -> Interception {
  intercept(page, &API_PATTERNS, |page, event| async move {
    tokio::time::sleep(HELD_BACK).await;
    // The page may have gone meanwhile.
    let _ = page
      .execute(ContinueRequestParams::new(event.request_id.clone()))
      .await;
  })
  .await
}

/// The identity provider answers refresh requests with `invalid_grant`.
async fn refuse_refresh_tokens(page: &Page) -> Interception {
  answer_refresh_with(page, 400, r#"{"error":"invalid_grant"}"#).await
}

/// The identity provider answers refresh requests like this; everything else goes through.
async fn answer_refresh_with(page: &Page, status: i64, body: &'static str) -> Interception {
  intercept(
    page,
    &["*/protocol/openid-connect/token"],
    move |page, event| async move {
      let is_refresh = event
        .request
        .post_data_entries
        .iter()
        .flatten()
        .filter_map(|entry| entry.bytes.as_ref())
        .any(|bytes| {
          let encoded: &str = bytes.as_ref();
          use base64::Engine;
          base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .is_ok_and(|body| String::from_utf8_lossy(&body).contains("refresh_token"))
        });
      if is_refresh {
        fulfill(&page, &event, status, "application/json", body).await;
      } else {
        let _ = page
          .execute(ContinueRequestParams::new(event.request_id.clone()))
          .await;
      }
    },
  )
  .await
}

/// `GET /me` names an account nobody signed in as.
async fn serve_another_account(page: &Page) -> Interception {
  intercept(page, &["*/api/v1/me"], |page, event| async move {
    let body = r#"{"id":"9007199254740001","display_name":"Stranger","role":"member"}"#;
    fulfill(&page, &event, 200, "application/json", body).await;
  })
  .await
}

async fn fulfill(page: &Page, event: &EventRequestPaused, status: i64, content_type: &str, body: &str) {
  use base64::Engine;
  let mut response = FulfillRequestParams::new(event.request_id.clone(), status);
  response.response_headers = Some(vec![
    HeaderEntry::new("Content-Type", content_type),
    HeaderEntry::new("Access-Control-Allow-Origin", WEB_ORIGIN),
  ]);
  response.body = Some(base64::engine::general_purpose::STANDARD.encode(body).into());
  // The page may have gone meanwhile.
  let _ = page.execute(response).await;
}
