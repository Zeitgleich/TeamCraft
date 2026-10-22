//! Full sign-in against the dev stack (Keycloak on 8080, server on 8081; see the README). A fake
//! browser logs in through the Keycloak form and follows the redirect to the loopback listener.

// Test-only module: a failing step should abort the test with a clear message.
#![allow(clippy::expect_used)]

use std::sync::Arc;

use super::store::{KeyringStore, MemoryStore, TokenStore};
use super::{AuthError, AuthService, DesktopConfig};

/// Shares one in-memory store between two service instances, like the keyring across app restarts.
#[derive(Clone, Default)]
struct SharedStore(Arc<MemoryStore>);

impl TokenStore for SharedStore {
  async fn load(&self) -> Result<Option<secrecy::SecretString>, AuthError> {
    self.0.load().await
  }

  async fn save(&self, refresh_token: &str) -> Result<(), AuthError> {
    self.0.save(refresh_token).await
  }

  async fn clear(&self) -> Result<(), AuthError> {
    self.0.clear().await
  }
}

#[tokio::test]
async fn login_restore_api_and_logout_against_keycloak() {
  let store = SharedStore::default();
  let service = AuthService::new(DesktopConfig::from_env(), store.clone()).expect("service");
  assert!(
    !service.restore().await.expect("restore without token"),
    "starts signed out"
  );

  service
    .login(|url| spawn_fake_browser(url, "alice"))
    .await
    .expect("login");
  let me = service.api_get("/api/v1/me").await.expect("api call");
  assert_eq!(me.status, 200, "{}", me.body);
  assert!(me.body.contains("\"username\":\"alice\""), "{}", me.body);
  assert!(
    store.load().await.expect("load").is_some(),
    "refresh token was persisted"
  );

  // A second instance (app restart) signs in from the stored refresh token alone.
  let restarted = AuthService::new(DesktopConfig::from_env(), store.clone()).expect("service");
  assert!(restarted.restore().await.expect("restore"));
  assert_eq!(
    restarted.api_get("/api/v1/me").await.expect("api call").status,
    200
  );

  restarted.logout().await.expect("logout");
  assert!(store.load().await.expect("load").is_none());
  assert!(matches!(
    restarted.api_get("/api/v1/me").await,
    Err(AuthError::NotSignedIn)
  ));
  // The provider session is gone too: the old refresh token no longer works.
  let after = AuthService::new(DesktopConfig::from_env(), SharedStore::default()).expect("service");
  assert!(!after.restore().await.expect("restore"));
}

#[tokio::test]
async fn account_without_role_signs_in_but_the_server_says_so() {
  let service = AuthService::new(DesktopConfig::from_env(), SharedStore::default()).expect("service");
  service
    .login(|url| spawn_fake_browser(url, "frank"))
    .await
    .expect("login works for any realm user");
  let me = service.api_get("/api/v1/me").await.expect("api call");
  assert_eq!(me.status, 403);
  assert!(me.body.contains("no_teamcraft_role"), "{}", me.body);
  service.logout().await.expect("logout");
}

#[tokio::test]
async fn cancelling_stops_the_wait() {
  let service =
    Arc::new(AuthService::new(DesktopConfig::from_env(), SharedStore::default()).expect("service"));
  let waiting = tokio::spawn({
    let service = service.clone();
    async move { service.login(|_| Ok(())).await }
  });
  tokio::time::sleep(std::time::Duration::from_millis(800)).await;
  service.cancel_login();
  assert!(matches!(waiting.await.expect("task"), Err(AuthError::Cancelled)));
}

#[tokio::test]
async fn api_paths_outside_the_api_are_refused() {
  let service = AuthService::new(DesktopConfig::from_env(), SharedStore::default()).expect("service");
  assert!(matches!(
    service.api_get("/healthz").await,
    Err(AuthError::Unsupported(_))
  ));
}

/// Plays the browser: opens the authorization URL, submits the Keycloak login form with the dev
/// password of `user`, and requests the redirect target (the loopback listener of the app).
fn spawn_fake_browser(authorization_url: &str, user: &str) -> Result<(), String> {
  let url = authorization_url.to_owned();
  let user = user.to_owned();
  tokio::spawn(async move {
    let client = reqwest::Client::builder()
      .cookie_store(true)
      .redirect(reqwest::redirect::Policy::none())
      .build()
      .expect("client");
    let page = client
      .get(&url)
      .send()
      .await
      .expect("auth page")
      .text()
      .await
      .expect("text");
    let action = page
      .split("action=\"")
      .nth(1)
      .and_then(|rest| rest.split('"').next())
      .expect("login form")
      .replace("&amp;", "&");
    let form = [
      ("username", user.as_str()),
      ("password", &format!("{user}-dev-pass")),
      ("credentialId", ""),
    ];
    let redirect = client.post(action).form(&form).send().await.expect("login post");
    let location = redirect
      .headers()
      .get("location")
      .and_then(|value| value.to_str().ok())
      .expect("redirect")
      .to_owned();
    client.get(location).send().await.expect("loopback request");
  });
  Ok(())
}

/// Needs a running Secret Service (GNOME Keyring, KWallet) or the macOS Keychain.
#[tokio::test]
async fn keyring_store_round_trips() {
  let store = KeyringStore::new("http://issuer.test/realms/keyring-test");
  store.clear().await.expect("clear");
  assert!(store.load().await.expect("load").is_none());
  store.save("refresh-token-for-test").await.expect("save");
  let loaded = store.load().await.expect("load");
  assert_eq!(
    secrecy::ExposeSecret::expose_secret(&loaded.expect("stored")),
    "refresh-token-for-test"
  );
  store.clear().await.expect("clear");
  assert!(store.load().await.expect("load").is_none());
}
