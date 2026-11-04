//! Owners and admins never sign roles by hand (e2ee.md §3.1.3, issue #1276): the real web client in two
//! headless Chromiums, the real Keycloak, the real server. An owner and an admin sign in for the first
//! time on an instance with nothing signed, a role changes in Keycloak, and nobody is ever asked. The
//! screenshots go to `target/e2e-artifacts/role-signing-*.png`.
//!
//! Each run makes its own two users in the dev realm, and adds the realm's role mapper if the running
//! Keycloak was imported before it.

use std::panic::AssertUnwindSafe;
use std::time::{Duration, Instant};

use chromiumoxide::page::Page;
use futures_util::FutureExt as _;
use serde_json::{Value, json};
use teamcraft_e2e::Stack;
use teamcraft_e2e::browser::{BrowserSession, lease_web_port, web_dir};
use teamcraft_e2e::stack::{ISSUER, KEYCLOAK_URL};

use crate::web::{TEST_CSP, body_text, click_text, shot, type_until_kept, wait_for_element, wait_for_text};

/// The dev realm registers this port too; 8082 is often taken by somebody's dev server.
const PORT: u16 = 8083;
const WEB_ORIGIN: &str = "http://127.0.0.1:8083";
const REALM: &str = "teamcraft-dev";
const CLIENT: &str = "teamcraft";
const PASSWORD: &str = "role-signing-dev-pass";
/// Twice the token lifetime of the dev realm plus a margin: a role change shows at the next refresh, and
/// evidence of a lost role goes out once that token has expired.
const ROLE_CHANGE_WAIT: Duration = Duration::from_secs(780);
const SIGN_WAIT: Duration = Duration::from_secs(180);

/// What the app used to say about signing, on every page. None of it may come back in normal operation.
const PROMPTS: [&str; 8] = [
  "Sign now",
  "isn't signed",
  "isn’t signed",
  "Review and sign",
  "Not signed",
  "first admin roster",
  "Changes to sign",
  "Open owners",
];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owner_and_an_admin_get_their_rights_with_no_click_and_a_role_change_follows() {
  let Some(web_dir) = web_dir() else {
    assert!(
      std::env::var("E2E_REQUIRED").map_or(true, |value| value != "1"),
      "no web build: run tests/e2e/web/build.sh"
    );
    eprintln!("skipping: no web build (tests/e2e/web/build.sh)");
    return;
  };
  let _turn = lease_web_port(PORT).await;
  let Some(stack) = Stack::builder()
    .with_port(PORT)
    .with_web_dir(web_dir)
    .with_env("TEAMCRAFT_WEB__CONTENT_SECURITY_POLICY", TEST_CSP)
    .with_env(
      "TEAMCRAFT_SECURITY__PUBLIC_ORIGINS",
      &format!(r#"["{WEB_ORIGIN}"]"#),
    )
    .start()
    .await
  else {
    return;
  };
  let keycloak = Keycloak::connect().await;
  keycloak.ensure_role_mapper().await;
  keycloak.delete_users_of_earlier_runs().await;
  let suffix = std::process::id();
  let (owner_name, admin_name) = (format!("rs-owner-{suffix}"), format!("rs-admin-{suffix}"));
  let owner_id = keycloak.make_user(&owner_name, &["owner"]).await;
  // Only `admin`: taking it leaves no client role at all, so Keycloak's mapper adds no claim and the
  // server refuses the person's tokens. Only the server then sees what the provider signed.
  let admin_id = keycloak.make_user(&admin_name, &["admin"]).await;

  let owner_browser = BrowserSession::launch().await.expect("a browser for the owner");
  let admin_browser = BrowserSession::launch().await.expect("a browser for the admin");
  let owner_page = owner_browser.new_page(WEB_ORIGIN).await;
  let admin_page = admin_browser.new_page(WEB_ORIGIN).await;
  let owner_console = crate::web_calls::console_log(&owner_page).await;
  let admin_console = crate::web_calls::console_log(&admin_page).await;
  let print_consoles = || {
    for (who, console) in [("owner", &owner_console), ("admin", &admin_console)] {
      for line in console.lock().unwrap().iter().filter(|line| is_about_roles(line)) {
        eprintln!("{who}'s console: {line}");
      }
    }
  };

  // The owner's first sign-in: the first roster appears with no dialog.
  first_sign_in(&owner_page, &owner_name).await;
  let reader = stack.tokens("carol");
  let scenario = AssertUnwindSafe(async {
    wait_for_roster(&stack, &reader, &["owner"], "the first owner's roster").await;
    shot(&owner_page, "role-signing-1-owner-signed-in").await;
    assert_no_prompt(&owner_page, "the owner's first sign-in").await;

    // The admin's first sign-in: their app publishes the token, the owner's app signs, nobody asked.
    first_sign_in(&admin_page, &admin_name).await;
    wait_for_roster(&stack, &reader, &["admin", "owner"], "the admin on the roster").await;
    shot(&admin_page, "role-signing-2-admin-signed-in").await;
    shot(&owner_page, "role-signing-3-owner-after-admin").await;
    assert_no_prompt(&admin_page, "the admin's first sign-in").await;
    assert_no_prompt(&owner_page, "the owner after the admin").await;

    // The console has nothing to ask either.
    open_security(&owner_page).await;
    shot(&owner_page, "role-signing-4-owner-console").await;
    assert_no_prompt(&owner_page, "the owner's console").await;
    open_security(&admin_page).await;
    shot(&admin_page, "role-signing-5-admin-console").await;
    assert_no_prompt(&admin_page, "the admin's console").await;

    // A role change in Keycloak: the admin's next token says so, their app publishes it, the owner's
    // app takes them off.
    keycloak.remove_role(&admin_id, "admin").await;
    wait_for_roster_for(&stack, &reader, &["owner"], "the demotion", ROLE_CHANGE_WAIT).await;
    shot(&owner_page, "role-signing-6-owner-after-demotion").await;
    assert_no_prompt(&owner_page, "the owner after the demotion").await;

    // And back again: a newer token counts.
    keycloak.add_role(&admin_id, "admin").await;
    wait_for_roster_for(
      &stack,
      &reader,
      &["admin", "owner"],
      "the role given back",
      ROLE_CHANGE_WAIT,
    )
    .await;
    shot(&admin_page, "role-signing-7-admin-role-back").await;
    assert_no_prompt(&admin_page, "the admin with the role back").await;
  })
  .catch_unwind()
  .await;
  print_consoles();
  if let Err(panic) = scenario {
    let probe = stack.tokens("alice");
    let answer: Value = stack
      .http
      .get(format!("{}/api/v1/role-evidence", stack.base_url))
      .bearer_auth(probe.token_async().await)
      .send()
      .await
      .unwrap()
      .json()
      .await
      .unwrap();
    eprintln!(
      "evidence entries: {}",
      answer["entries"].as_array().map_or(0, Vec::len)
    );
    eprintln!("withdrawn: {}", answer["withdrawn"]);
    for loss in answer["role_losses"].as_array().into_iter().flatten() {
      eprintln!(
        "role loss of {}: {}",
        loss["user_id"],
        jwt_payload(loss["token"].as_str().unwrap())
      );
    }
    for line in admin_console.lock().unwrap().iter().rev().take(12) {
      eprintln!("admin's console, newest first: {line}");
    }
    for line in stack.server_log().lines().filter(|line| is_about_roles(line)) {
      eprintln!("server: {line}");
    }
    let _ = shot(&owner_page, "role-signing-failed-owner").await;
    let _ = shot(&admin_page, "role-signing-failed-admin").await;
    std::panic::resume_unwind(panic);
  }

  let log = stack.server_log();
  assert!(!log.contains("eyJ"), "no token in the server's log");

  keycloak.delete_user(&owner_id).await;
  keycloak.delete_user(&admin_id).await;
  owner_browser.close().await;
  admin_browser.close().await;
  stack.stop().await;
}

/// The claims of a token, for a test's failure message.
fn jwt_payload(token: &str) -> String {
  use base64::Engine as _;

  let part = token.split('.').nth(1).unwrap_or_default();
  base64::engine::general_purpose::URL_SAFE_NO_PAD
    .decode(part)
    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    .unwrap_or_default()
}

fn is_about_roles(line: &str) -> bool {
  [
    "role", "roster", "identity", "provider", "attest", "ERROR", "WARN",
  ]
  .iter()
  .any(|word| line.contains(word))
}

/// Signs in through Keycloak's form on a browser that never saw this account, and creates its keys.
async fn first_sign_in(page: &Page, username: &str) {
  teamcraft_e2e::browser::prefer_mouse(page).await;
  page.goto(format!("{WEB_ORIGIN}/healthz")).await.unwrap();
  page
    .evaluate("localStorage.setItem('teamcraft.locale', 'en')")
    .await
    .unwrap();
  page.goto(format!("{WEB_ORIGIN}/")).await.unwrap();
  wait_for_text(page, "Sign in").await;
  click_text(page, "Sign in").await;
  let field = wait_for_element(page, "input[name=username]").await;
  field.click().await.unwrap().type_str(username).await.unwrap();
  let password = page.find_element("input[name=password]").await.unwrap();
  password
    .click()
    .await
    .unwrap()
    .type_str(PASSWORD)
    .await
    .unwrap()
    .press_key("Enter")
    .await
    .unwrap();
  // The recovery kit comes first, then the keys, then whatever else the first run shows.
  wait_for_text(page, "I saved my recovery kit").await;
  shot(page, &format!("role-signing-setup-{}", &username[..8])).await;
  let groups: Vec<String> = body_text(page)
    .await
    .lines()
    .map(str::trim)
    .filter(|line| line.len() == 4 && line.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()))
    .map(str::to_owned)
    .collect();
  assert_eq!(groups.len(), 8, "the recovery key on the page");
  click_text(page, "I saved my recovery kit").await;
  wait_for_text(page, "To make sure you kept it").await;
  let asked = body_text(page).await;
  let number: usize = asked
    .split("type group ")
    .nth(1)
    .and_then(|rest| rest.split(' ').next())
    .and_then(|number| number.parse().ok())
    .expect("which group the page asks for");
  type_until_kept(page, "#kit-confirm-group", &groups[number - 1]).await;
  click_text(page, "Create keys").await;
  finish_first_run(page).await;
}

/// Answers what the first run still asks, as a person would, until the app itself is there.
async fn finish_first_run(page: &Page) {
  let deadline = Instant::now() + Duration::from_secs(90);
  loop {
    let text = body_text(page).await;
    if !text.contains("Create your encryption keys") && !text.contains("Working…") {
      for answer in ["Continue", "Skip", "Later", "Not now", "Done", "Get started"] {
        if text.contains(answer) {
          click_text(page, answer).await;
          break;
        }
      }
      if page.find_element("nav, aside, [data-sidebar]").await.is_ok() {
        return;
      }
    }
    assert!(Instant::now() < deadline, "the first run never ended:\n{text}");
    tokio::time::sleep(Duration::from_millis(500)).await;
  }
}

/// Opens the console from the account menu, then its Security page, and back out of it again is the
/// next page load.
async fn open_security(page: &Page) {
  page
    .evaluate("document.querySelector('.avatar-btn').click()")
    .await
    .unwrap();
  click_text(page, "Administration").await;
  wait_for_element(page, ".admin-nav-item").await;
  page
    .evaluate(
      "[...document.querySelectorAll('.admin-nav-item')].find(e => e.innerText.trim() === 'Security policy').click()",
    )
    .await
    .unwrap();
  wait_for_text(page, "Who signs role changes").await;
  tokio::time::sleep(Duration::from_secs(2)).await;
}

async fn assert_no_prompt(page: &Page, when: &str) {
  let text = body_text(page).await;
  for prompt in PROMPTS {
    assert!(
      !text.contains(prompt),
      "{when}: the page says {prompt:?}:\n{text}"
    );
  }
}

async fn wait_for_roster(
  stack: &Stack,
  reader: &teamcraft_e2e::tokens::UserTokens,
  roles: &[&str],
  what: &str,
) {
  wait_for_roster_for(stack, reader, roles, what, SIGN_WAIT).await;
}

/// Waits until the newest roster on the server has exactly these roles (sorted).
async fn wait_for_roster_for(
  stack: &Stack,
  reader: &teamcraft_e2e::tokens::UserTokens,
  roles: &[&str],
  what: &str,
  wait: Duration,
) {
  let deadline = Instant::now() + wait;
  loop {
    let current = roster_roles(stack, reader).await;
    if current == roles {
      eprintln!("{what}: the roster has {current:?}");
      return;
    }
    assert!(Instant::now() < deadline, "{what}: the roster has {current:?}");
    tokio::time::sleep(Duration::from_secs(2)).await;
  }
}

async fn roster_roles(stack: &Stack, reader: &teamcraft_e2e::tokens::UserTokens) -> Vec<String> {
  use base64::Engine as _;

  let token = reader.token_async().await;
  let response: Value = stack
    .http
    .get(format!("{}/api/v1/workspace/roster", stack.base_url))
    .bearer_auth(token)
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
  let Some(entry) = response["entries"].as_array().and_then(|entries| entries.last()) else {
    return Vec::new();
  };
  let payload = base64::engine::general_purpose::STANDARD
    .decode(entry["payload"].as_str().unwrap())
    .unwrap();
  let document: Value = serde_json::from_slice(&payload).unwrap();
  let mut roles: Vec<String> = document["members"]
    .as_array()
    .unwrap()
    .iter()
    .map(|member| member["role"].as_str().unwrap().to_owned())
    .collect();
  roles.sort();
  roles
}

/// The dev Keycloak's admin API. Only the dev realm; its credentials are the dev stack's.
struct Keycloak {
  http: reqwest::Client,
  client_uuid: String,
}

impl Keycloak {
  async fn connect() -> Self {
    let http = reqwest::Client::new();
    let mut keycloak = Self {
      http,
      client_uuid: String::new(),
    };
    let clients = keycloak.get(&format!("/clients?clientId={CLIENT}")).await;
    keycloak.client_uuid = clients[0]["id"].as_str().unwrap().to_owned();
    assert!(ISSUER.ends_with(REALM));
    keycloak
  }

  /// A new token for every call: the admin API's only lives a minute, and the test takes longer.
  async fn token(&self) -> String {
    let answer: Value = self
      .http
      .post(format!(
        "{KEYCLOAK_URL}/realms/master/protocol/openid-connect/token"
      ))
      .form(&[
        ("grant_type", "password"),
        ("client_id", "admin-cli"),
        ("username", "admin"),
        ("password", "admin-dev-only"),
      ])
      .send()
      .await
      .unwrap()
      .json()
      .await
      .unwrap();
    answer["access_token"].as_str().unwrap().to_owned()
  }

  async fn ensure_role_mapper(&self) {
    let mappers: Value = self
      .get(&format!("/clients/{}/protocol-mappers/models", self.client_uuid))
      .await;
    let has_mapper = mappers
      .as_array()
      .unwrap()
      .iter()
      .any(|mapper| mapper["name"] == "client roles in ID token");
    if has_mapper {
      return;
    }
    self
      .send(
        reqwest::Method::POST,
        &format!("/clients/{}/protocol-mappers/models", self.client_uuid),
        &json!({
          "name": "client roles in ID token",
          "protocol": "openid-connect",
          "protocolMapper": "oidc-usermodel-client-role-mapper",
          "config": {
            "usermodel.clientRoleMapping.clientId": CLIENT,
            "claim.name": "resource_access.${client_id}.roles",
            "jsonType.label": "String",
            "multivalued": "true",
            "id.token.claim": "true",
            "access.token.claim": "false",
            "userinfo.token.claim": "false",
          },
        }),
      )
      .await;
  }

  /// A run that failed left its users behind; each only ever takes the realm's memory.
  async fn delete_users_of_earlier_runs(&self) {
    let found: Value = self.get("/users?search=rs-&max=100").await;
    for user in found.as_array().unwrap() {
      if user["username"]
        .as_str()
        .is_some_and(|name| name.starts_with("rs-"))
      {
        self.delete_user(user["id"].as_str().unwrap()).await;
      }
    }
  }

  async fn make_user(&self, username: &str, roles: &[&str]) -> String {
    self
      .send(
        reqwest::Method::POST,
        "/users",
        &json!({
          "username": username,
          "email": format!("{username}@example.com"),
          "firstName": "Role",
          "lastName": "Signing",
          "enabled": true,
          "emailVerified": true,
          "credentials": [{"type": "password", "value": PASSWORD, "temporary": false}],
        }),
      )
      .await;
    let found: Value = self.get(&format!("/users?username={username}&exact=true")).await;
    let id = found[0]["id"].as_str().unwrap().to_owned();
    for role in roles {
      self.add_role(&id, role).await;
    }
    id
  }

  async fn add_role(&self, user: &str, role: &str) {
    let representation = self.role(role).await;
    self
      .send(
        reqwest::Method::POST,
        &format!("/users/{user}/role-mappings/clients/{}", self.client_uuid),
        &json!([representation]),
      )
      .await;
  }

  async fn remove_role(&self, user: &str, role: &str) {
    let representation = self.role(role).await;
    self
      .send(
        reqwest::Method::DELETE,
        &format!("/users/{user}/role-mappings/clients/{}", self.client_uuid),
        &json!([representation]),
      )
      .await;
  }

  async fn role(&self, role: &str) -> Value {
    self
      .get(&format!("/clients/{}/roles/{role}", self.client_uuid))
      .await
  }

  async fn delete_user(&self, user: &str) {
    let answer = self
      .http
      .delete(format!("{}/users/{user}", admin_url()))
      .bearer_auth(self.token().await)
      .send()
      .await
      .unwrap();
    assert!(answer.status().is_success(), "{}", answer.status());
  }

  async fn get(&self, path: &str) -> Value {
    self
      .http
      .get(format!("{}{path}", admin_url()))
      .bearer_auth(self.token().await)
      .send()
      .await
      .unwrap()
      .json()
      .await
      .unwrap()
  }

  async fn send(&self, method: reqwest::Method, path: &str, body: &Value) {
    let answer = self
      .http
      .request(method, format!("{}{path}", admin_url()))
      .bearer_auth(self.token().await)
      .json(body)
      .send()
      .await
      .unwrap();
    assert!(answer.status().is_success(), "{path}: {}", answer.status());
  }
}

fn admin_url() -> String {
  format!("{KEYCLOAK_URL}/admin/realms/{REALM}")
}
