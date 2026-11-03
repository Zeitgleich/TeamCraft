//! DPoP in the browser against the real Keycloak and server (e2ee.md §3.1.2): the dev realm's
//! provider advertises DPoP, so a server that knows its origin gets a bound sign-in, signed with a
//! non-extractable Web Crypto key that survives a reload.

use std::time::{Duration, Instant};

use chromiumoxide::page::Page;

use super::web::{Rig, WEB_ORIGIN, sign_in_as_alice, wait_for_text};

/// `/metrics` of the server under test, off the web port.
const METRICS_ADDR: &str = "127.0.0.1:8092";
const WAIT: Duration = Duration::from_secs(30);
/// Reloads of the start measurement.
const RELOADS: usize = 5;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_web_client_binds_its_sign_in_and_signs_its_requests() {
  let Some(rig) = Rig::start_with(
    |builder| {
      builder
        .with_env(
          "TEAMCRAFT_SECURITY__PUBLIC_ORIGINS",
          &format!(r#"["{WEB_ORIGIN}"]"#),
        )
        .with_env("TEAMCRAFT_METRICS__ENABLED", "true")
        .with_env(
          "TEAMCRAFT_METRICS__LISTEN_ADDRS",
          &format!(r#"["{METRICS_ADDR}"]"#),
        )
    },
    &[],
  )
  .await
  else {
    return;
  };
  rig
    .browser
    .screenshot_on_failure("web-dpop", |page| async move {
      sign_in_as_alice(&page).await;
      wait_for_text(&page, "browser-channel").await;
      assert_eq!(
        evaluate(&page, KEY_IS_EXTRACTABLE).await,
        "false",
        "the private key"
      );
      let after_sign_in = wait_for_bound_requests(0).await;

      // A reload restores the session with the same key, by the refresh token bound to it.
      let thumbprint = evaluate(&page, PUBLIC_KEY).await;
      page.reload().await.unwrap();
      wait_for_text(&page, "browser-channel").await;
      assert_eq!(evaluate(&page, PUBLIC_KEY).await, thumbprint);
      wait_for_bound_requests(after_sign_in).await;

      eprintln!("in Chromium: {}", evaluate(&page, MEASURE).await);
      eprintln!("bound: reload to the channel list {:?}", reload_time(&page).await);
    })
    .await;
  rig.finish().await;
}

/// A server that doesn't know the address it is reached on offers no nonce on `server-info`, like an
/// older server or a proxy that strips the header: the sign-in stays unbound, and everything works.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_nonce_from_the_server_the_sign_in_stays_unbound() {
  let Some(rig) = Rig::start_with(
    |builder| {
      builder.with_env("TEAMCRAFT_METRICS__ENABLED", "true").with_env(
        "TEAMCRAFT_METRICS__LISTEN_ADDRS",
        &format!(r#"["{METRICS_ADDR}"]"#),
      )
    },
    &[],
  )
  .await
  else {
    return;
  };
  rig
    .browser
    .screenshot_on_failure("web-dpop-unbound", |page| async move {
      sign_in_as_alice(&page).await;
      wait_for_text(&page, "browser-channel").await;
      assert_eq!(bound_requests().await, 0);
      eprintln!(
        "unbound: reload to the channel list {:?}",
        reload_time(&page).await
      );
      assert_eq!(bound_requests().await, 0);
    })
    .await;
  rig.finish().await;
}

/// The median time from a reload to the channel list.
async fn reload_time(page: &Page) -> Duration {
  let mut times = Vec::new();
  for _ in 0..RELOADS {
    let started = Instant::now();
    page.reload().await.unwrap();
    wait_for_text(page, "browser-channel").await;
    times.push(started.elapsed());
  }
  times.sort_unstable();
  times[times.len() / 2]
}

/// How many requests the server took with a verified proof, once more than `more_than` did.
async fn wait_for_bound_requests(more_than: u64) -> u64 {
  let deadline = Instant::now() + WAIT;
  loop {
    let bound = bound_requests().await;
    if bound > more_than {
      return bound;
    }
    assert!(
      Instant::now() < deadline,
      "no more than {more_than} requests with a verified proof"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
}

async fn bound_requests() -> u64 {
  let metrics = reqwest::get(format!("http://{METRICS_ADDR}/metrics"))
    .await
    .unwrap()
    .text()
    .await
    .unwrap();
  metrics
    .lines()
    .find(|line| line.starts_with(r#"teamcraft_dpop_requests_total{outcome="bound"}"#))
    .and_then(|line| line.rsplit(' ').next())
    .and_then(|value| value.parse().ok())
    .unwrap_or(0)
}

async fn evaluate(page: &Page, script: &str) -> String {
  page
    .evaluate(script)
    .await
    .unwrap()
    .into_value::<String>()
    .unwrap()
}

/// `keyPair` below is the stored `{privateKey, publicKey}`.
macro_rules! with_key_pair {
  ($body:literal) => {
    concat!(
      "(async () => { const database = await new Promise((resolve, reject) => {",
      " const open = indexedDB.open('teamcraft-dpop');",
      " open.onsuccess = () => resolve(open.result); open.onerror = () => reject(open.error); });",
      " const keyPair = await new Promise((resolve, reject) => {",
      " const read = database.transaction('keys').objectStore('keys').get('key_pair');",
      " read.onsuccess = () => resolve(read.result); read.onerror = () => reject(read.error); });",
      " database.close(); ",
      $body,
      " })()"
    )
  };
}

const KEY_IS_EXTRACTABLE: &str = with_key_pair!("return String(keyPair.privateKey.extractable);");

const PUBLIC_KEY: &str =
  with_key_pair!("return JSON.stringify(await crypto.subtle.exportKey('jwk', keyPair.publicKey));");

const MEASURE: &str = with_key_pair!(
  "const data = new Uint8Array(600); const rounds = 50; const started = performance.now();
   for (let i = 0; i < rounds; i++) {
     await crypto.subtle.sign({name: 'ECDSA', hash: 'SHA-256'}, keyPair.privateKey, data);
   }
   return 'one signature ' + ((performance.now() - started) / rounds).toFixed(2) + ' ms';"
);
