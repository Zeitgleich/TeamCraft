//! Harness for integration tests (feature `integration`): a real server on a loopback port, a
//! throwaway Postgres database and tokens signed with the throwaway test key.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use teamcraft_db::SnowflakeGenerator;
use teamcraft_db::testing::TestDatabase;
use tokio::task::JoinHandle;

use crate::auth::test_keys::{TEST_KEY_ID, TEST_MODULUS_B64, TEST_PRIVATE_KEY_PEM};
use crate::auth::{
  AuthError, BoxFuture, JwkSource, JwksCache, JwksCacheOptions, Revocations, RoleMapping, TokenValidator,
  ValidatorOptions,
};
use crate::config::{CallsConfig, MeetingsConfig, RoleNames, StorageConfig};
use crate::gateway::{Hub, HubOptions};
use crate::listener::bind_listener;
use time::OffsetDateTime;

use crate::meetings::caldav::{BusyPeriod, CalendarError, CalendarStore};
use crate::meetings::mailer::{MailError, Mailer, OutgoingMail};
use crate::meetings::{MeetingBackends, MeetingService};
use crate::push::PushDispatcher;
use crate::rendezvous::RendezvousHub;
use crate::router::router;
use crate::security::RateLimits;
use crate::services::calls::CallService;
use crate::services::local_accounts::LocalAuth;
use crate::services::permission_scheme::SchemeStore;
use crate::services::session::PolicyHolder;
use crate::state::{AppState, Settings};
use crate::storage::BlobStore;

pub const TEST_ISSUER: &str = "https://auth.example.com/realms/test";
pub const TEST_CLIENT: &str = "teamcraft";

/// A calendar for tests: keeps the written events in memory and can be told to fail.
#[derive(Default)]
pub struct FakeCalendar {
  events: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
  busy: std::sync::Mutex<std::collections::BTreeMap<String, Vec<BusyPeriod>>>,
  is_failing: std::sync::atomic::AtomicBool,
}

impl FakeCalendar {
  /// The stored event, if there is one.
  pub fn event(&self, object_name: &str) -> Option<String> {
    self.events.lock().unwrap().get(object_name).cloned()
  }

  /// Names of all stored events.
  pub fn names(&self) -> Vec<String> {
    self.events.lock().unwrap().keys().cloned().collect()
  }

  /// What the calendar of `email` answers to free-busy queries from now on.
  pub fn set_busy(&self, email: &str, periods: Vec<BusyPeriod>) {
    self.busy.lock().unwrap().insert(email.to_owned(), periods);
  }

  /// Makes every following write fail as if the calendar server were down.
  pub fn set_failing(&self, is_failing: bool) {
    self.is_failing.store(is_failing, Ordering::SeqCst);
  }

  fn check(&self) -> Result<(), CalendarError> {
    if self.is_failing.load(Ordering::SeqCst) {
      Err(CalendarError::Unreachable("the fake calendar is down".into()))
    } else {
      Ok(())
    }
  }
}

impl CalendarStore for FakeCalendar {
  fn put_event<'a>(&'a self, object_name: &'a str, ics: &'a str) -> BoxFuture<'a, Result<(), CalendarError>> {
    Box::pin(async move {
      self.check()?;
      self
        .events
        .lock()
        .unwrap()
        .insert(object_name.to_owned(), ics.to_owned());
      Ok(())
    })
  }

  fn delete_event<'a>(&'a self, object_name: &'a str) -> BoxFuture<'a, Result<(), CalendarError>> {
    Box::pin(async move {
      self.check()?;
      self.events.lock().unwrap().remove(object_name);
      Ok(())
    })
  }

  fn busy_periods<'a>(
    &'a self,
    email: &'a str,
    from: OffsetDateTime,
    to: OffsetDateTime,
  ) -> BoxFuture<'a, Result<Vec<BusyPeriod>, CalendarError>> {
    Box::pin(async move {
      self.check()?;
      Ok(
        self
          .busy
          .lock()
          .unwrap()
          .get(email)
          .map(|periods| {
            periods
              .iter()
              .filter(|p| p.end > from && p.start < to)
              .copied()
              .collect()
          })
          .unwrap_or_default(),
      )
    })
  }
}

/// A mail server for tests: remembers what was sent and can be told to refuse addresses.
#[derive(Default)]
pub struct FakeMailer {
  sent: std::sync::Mutex<Vec<OutgoingMail>>,
  refused: std::sync::Mutex<Vec<String>>,
}

impl FakeMailer {
  /// Everything sent so far, oldest first.
  pub fn sent(&self) -> Vec<OutgoingMail> {
    self.sent.lock().unwrap().clone()
  }

  /// Mail to this address fails from now on.
  pub fn refuse(&self, address: &str) {
    self.refused.lock().unwrap().push(address.to_owned());
  }
}

impl Mailer for FakeMailer {
  fn send<'a>(&'a self, mail: &'a OutgoingMail) -> BoxFuture<'a, Result<(), MailError>> {
    Box::pin(async move {
      if self.refused.lock().unwrap().contains(&mail.to_address) {
        return Err(MailError::Send(
          "the fake mail server refuses this address".into(),
        ));
      }
      self.sent.lock().unwrap().push(mail.clone());
      Ok(())
    })
  }
}

/// Optional parts a test server is started with.
#[derive(Default)]
struct Extras {
  maybe_storage: Option<StorageConfig>,
  maybe_calls: Option<CallService>,
  maybe_push: Option<PushDispatcher>,
  maybe_meetings: Option<MeetingService>,
  maybe_mailer: Option<Arc<dyn Mailer>>,
}

pub struct TestApp {
  pub http: reqwest::Client,
  pub base_url: String,
  pub ws_url: String,
  pub state: AppState,
  test_db: TestDatabase,
  server: JoinHandle<()>,
}

/// A person with the roles the identity provider would put in their token.
#[derive(Debug, Clone)]
pub struct TestUser {
  pub subject: String,
  pub username: String,
  pub email: String,
  pub is_email_verified: bool,
  pub roles: Vec<String>,
  /// The provider's session ID put into the `sid` claim.
  pub maybe_session_id: Option<String>,
  /// The `amr` claim: how the user signed in.
  pub authentication_methods: Vec<String>,
}

pub struct Reply {
  pub status: StatusCode,
  pub body: Value,
}

impl TestUser {
  pub fn new(username: &str, roles: &[&str]) -> Self {
    Self {
      subject: format!("sub-{username}"),
      username: username.to_owned(),
      email: format!("{username}@example.com"),
      is_email_verified: true,
      roles: roles.iter().map(|role| (*role).to_owned()).collect(),
      maybe_session_id: None,
      authentication_methods: vec!["pwd".to_owned()],
    }
  }

  pub fn with_authentication_methods(mut self, methods: &[&str]) -> Self {
    self.authentication_methods = methods.iter().map(|method| (*method).to_owned()).collect();
    self
  }

  pub fn with_session(mut self, session_id: &str) -> Self {
    self.maybe_session_id = Some(session_id.to_owned());
    self
  }

  pub fn with_email(mut self, email: &str, is_verified: bool) -> Self {
    self.email = email.to_owned();
    self.is_email_verified = is_verified;
    self
  }
}

impl TestApp {
  /// `None` (skip the test) when `DATABASE_URL` is not set.
  pub async fn start() -> Option<Self> {
    Self::start_with(|_| {}).await
  }

  pub async fn start_with(customize: impl FnOnce(&mut Settings)) -> Option<Self> {
    Self::start_full(customize, Extras::default()).await
  }

  /// Like [`Self::start`] with attachment storage on the dev Garage (`TEAMCRAFT_TEST_S3_ENDPOINT`,
  /// default `http://127.0.0.1:3900`; the bucket and key come from `dev/init-garage.sh`).
  /// `None` (skip the test) if the database or the storage is not reachable.
  pub async fn start_with_storage(customize: impl FnOnce(&mut Settings)) -> Option<Self> {
    let endpoint =
      std::env::var("TEAMCRAFT_TEST_S3_ENDPOINT").unwrap_or_else(|_| "http://127.0.0.1:3900".into());
    let reachable = reqwest::Client::new()
      .get(&endpoint)
      .timeout(Duration::from_secs(2))
      .send()
      .await
      .is_ok();
    if !reachable {
      eprintln!("skipping: no S3 storage at {endpoint}");
      return None;
    }
    let storage = StorageConfig {
      endpoint,
      region: "garage".into(),
      bucket: "teamcraft-dev".into(),
      access_key: "GKdeadbeef0000000000000000".into(),
      secret_key: "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
        .to_owned()
        .into(),
      path_style: true,
      presign_expiry_secs: 300,
      max_attachment_bytes: 1024 * 1024,
    };
    Self::start_full(
      customize,
      Extras {
        maybe_storage: Some(storage),
        ..Extras::default()
      },
    )
    .await
  }

  /// Like [`Self::start`] with calls enabled: join tokens are signed with `signing_key_pem`
  /// (Ed25519) and point to the SFU URL `sfu_url`.
  pub async fn start_with_calls(signing_key_pem: &str, sfu_url: &str) -> Option<Self> {
    let config = CallsConfig {
      sfu_url: sfu_url.into(),
      issuer: TEST_ISSUER.into(),
      signing_key_path: "unused.pem".into(),
      join_token_ttl_secs: 60,
      call_idle_secs: 3600,
      maybe_sfu_registration_token: None,
      ring_timeout_secs: 30,
      maybe_sfu_control_url: None,
    };
    Self::start_with_calls_config(&config, signing_key_pem).await
  }

  /// Like [`Self::start_with_calls`] with a full calls configuration (SFU token, ring timeout).
  pub async fn start_with_calls_config(config: &CallsConfig, signing_key_pem: &str) -> Option<Self> {
    let calls = CallService::new(config, signing_key_pem.as_bytes()).unwrap();
    Self::start_full(
      |_| {},
      Extras {
        maybe_calls: Some(calls),
        ..Extras::default()
      },
    )
    .await
  }

  /// Meetings and calls together (host controls need both).
  pub async fn start_with_meetings_and_calls(
    meetings: &MeetingsConfig,
    calls: &CallsConfig,
    signing_key_pem: &str,
  ) -> Option<Self> {
    let meetings = MeetingService::new(
      meetings,
      MeetingBackends {
        maybe_calendar: Some(Arc::new(FakeCalendar::default())),
        maybe_mailer: Some(Arc::new(FakeMailer::default())),
      },
    )
    .unwrap();
    let calls = CallService::new(calls, signing_key_pem.as_bytes()).unwrap();
    Self::start_full(
      |_| {},
      Extras {
        maybe_meetings: Some(meetings),
        maybe_calls: Some(calls),
        ..Extras::default()
      },
    )
    .await
  }

  /// Like [`Self::start`] with meetings enabled. Calendar writes go to `calendar`, invitation mail
  /// to `mailer`.
  pub async fn start_with_meetings(
    config: &MeetingsConfig,
    calendar: Arc<dyn CalendarStore>,
    mailer: Arc<dyn Mailer>,
  ) -> Option<Self> {
    let meetings = MeetingService::new(
      config,
      MeetingBackends {
        maybe_calendar: Some(calendar),
        maybe_mailer: Some(mailer),
      },
    )
    .unwrap();
    Self::start_full(
      |_| {},
      Extras {
        maybe_meetings: Some(meetings),
        ..Extras::default()
      },
    )
    .await
  }

  /// Like [`Self::start_with`] with the server's own outgoing mail going to `mailer`.
  pub async fn start_with_mailer(
    customize: impl FnOnce(&mut Settings),
    mailer: Arc<dyn Mailer>,
  ) -> Option<Self> {
    Self::start_full(
      customize,
      Extras {
        maybe_mailer: Some(mailer),
        ..Extras::default()
      },
    )
    .await
  }

  /// Like [`Self::start_with`] with the given push dispatcher (tests record or fake the senders).
  pub async fn start_with_push(
    customize: impl FnOnce(&mut Settings),
    dispatcher: PushDispatcher,
  ) -> Option<Self> {
    Self::start_full(
      customize,
      Extras {
        maybe_push: Some(dispatcher),
        ..Extras::default()
      },
    )
    .await
  }

  async fn start_full(customize: impl FnOnce(&mut Settings), extras: Extras) -> Option<Self> {
    let Extras {
      maybe_storage,
      maybe_calls,
      maybe_push,
      maybe_meetings,
      maybe_mailer,
    } = extras;
    let test_db = TestDatabase::create().await?;
    let mut settings = Settings::default();
    settings.gateway.presence_offline_grace_secs = 0;
    // Tests make many requests (and fail many authentications) from one address; the ones about
    // rate limits switch the limits on again.
    settings.security.rate_limits.is_enabled = false;
    customize(&mut settings);

    let keys = JwksCache::new(
      Box::new(StaticKeys),
      JwksCacheOptions {
        ttl: Duration::from_secs(3600),
        min_refresh_interval: Duration::from_secs(1),
      },
    );
    let validator = TokenValidator::new(
      keys,
      RoleMapping::new(RoleNames::default()),
      ValidatorOptions {
        issuer: TEST_ISSUER.into(),
        client_id: TEST_CLIENT.into(),
        algorithms: vec![Algorithm::RS256],
        leeway_secs: 30,
        logout_audiences: vec![TEST_CLIENT.into(), "teamcraft-server".into()],
      },
    );
    let state = AppState {
      database: test_db.db.clone(),
      validator: Arc::new(validator),
      // Each app gets its own worker ID: parallel tests share the S3 bucket, and IDs name its objects.
      ids: Arc::new(SnowflakeGenerator::new(next_worker_id()).unwrap()),
      push: Arc::new(maybe_push.unwrap_or_else(|| PushDispatcher::new(4))),
      revocations: Arc::new(Revocations::default()),
      hub: Arc::new(Hub::new(HubOptions {
        resume_window: Duration::from_secs(settings.gateway.resume_window_secs),
        buffer_events: settings.gateway.resume_buffer_events,
      })),
      rendezvous: Arc::new(RendezvousHub::new(&settings.rendezvous)),
      maybe_blob_store: maybe_storage.map(|config| Arc::new(BlobStore::new(&config).unwrap())),
      maybe_calls: maybe_calls.map(Arc::new),
      maybe_meetings: maybe_meetings.map(Arc::new),
      maybe_mailer,
      maybe_local: settings
        .local_accounts
        .is_enabled
        .then(|| Arc::new(LocalAuth::new(&settings.local_accounts).unwrap())),
      policy: Arc::new(PolicyHolder::default()),
      scheme: Arc::new(SchemeStore::default()),
      rate_limits: Arc::new(RateLimits::new(&settings.security.rate_limits)),
      settings: Arc::new(settings),
    };

    let listener = bind_listener(([127, 0, 0, 1], 0).into()).unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone());
    let server = tokio::spawn(async move {
      axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
      )
      .await
      .unwrap();
    });
    Some(Self {
      http: reqwest::Client::new(),
      base_url: format!("http://{addr}"),
      ws_url: format!("ws://{addr}/api/v1/gateway"),
      state,
      test_db,
      server,
    })
  }

  /// `true` if the table (e.g. a message partition) exists.
  pub async fn table_exists(&self, name: &str) -> bool {
    self.test_db.table_exists(name).await
  }

  /// A pool on the throwaway database, for tests that seed or inspect it directly.
  pub fn sql_pool(&self) -> &teamcraft_db::testing::PgPool {
    &self.test_db.pool
  }

  pub async fn stop(self) {
    self.server.abort();
    self.test_db.drop_database().await;
  }

  /// A signed access token for the user, as Keycloak would issue it.
  pub fn token(&self, user: &TestUser) -> String {
    self.token_issued_at(
      user,
      i64::try_from(jsonwebtoken::get_current_timestamp()).unwrap(),
    )
  }

  /// Like [`Self::token`] with a chosen issue time.
  pub fn token_issued_at(&self, user: &TestUser, issued_at: i64) -> String {
    let claims = json!({
      "iat": issued_at,
      "sid": user.maybe_session_id,
      "amr": user.authentication_methods,
      "iss": TEST_ISSUER,
      "sub": user.subject,
      "azp": TEST_CLIENT,
      "aud": "account",
      "exp": jsonwebtoken::get_current_timestamp() + 300,
      "preferred_username": user.username,
      "name": user.username,
      "email": user.email,
      "email_verified": user.is_email_verified,
      "resource_access": { TEST_CLIENT: { "roles": user.roles } },
    });
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(TEST_KEY_ID.into());
    encode(
      &header,
      &claims,
      &EncodingKey::from_rsa_pem(TEST_PRIVATE_KEY_PEM.as_bytes()).unwrap(),
    )
    .unwrap()
  }

  /// A signed back-channel logout token as the provider would send it.
  pub fn logout_token(&self, maybe_subject: Option<&str>, maybe_session_id: Option<&str>) -> String {
    let claims = json!({
      "iss": TEST_ISSUER,
      "aud": TEST_CLIENT,
      "iat": jsonwebtoken::get_current_timestamp(),
      "jti": "logout-token-1",
      "sub": maybe_subject,
      "sid": maybe_session_id,
      "events": {"http://schemas.openid.net/event/backchannel-logout": {}},
    });
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(TEST_KEY_ID.into());
    encode(
      &header,
      &claims,
      &EncodingKey::from_rsa_pem(TEST_PRIVATE_KEY_PEM.as_bytes()).unwrap(),
    )
    .unwrap()
  }

  /// Sends a request as `user` (`None`: without credentials) and parses the JSON reply.
  pub async fn call(
    &self,
    method: Method,
    user: Option<&TestUser>,
    path: &str,
    body: Option<Value>,
  ) -> Reply {
    let mut request = self
      .http
      .request(method, format!("{}/api/v1{path}", self.base_url));
    if let Some(user) = user {
      request = request.bearer_auth(self.token(user));
    }
    if let Some(body) = body {
      request = request.json(&body);
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    let bytes = response.bytes().await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Reply { status, body }
  }

  pub async fn get(&self, user: &TestUser, path: &str) -> Reply {
    self.call(Method::GET, Some(user), path, None).await
  }

  pub async fn post(&self, user: &TestUser, path: &str, body: Value) -> Reply {
    self.call(Method::POST, Some(user), path, Some(body)).await
  }

  pub async fn patch(&self, user: &TestUser, path: &str, body: Value) -> Reply {
    self.call(Method::PATCH, Some(user), path, Some(body)).await
  }

  pub async fn put(&self, user: &TestUser, path: &str, body: Value) -> Reply {
    self.call(Method::PUT, Some(user), path, Some(body)).await
  }

  pub async fn delete(&self, user: &TestUser, path: &str) -> Reply {
    self.call(Method::DELETE, Some(user), path, None).await
  }
}

impl Reply {
  /// The `id` of a created object.
  pub fn id(&self) -> String {
    self.body["id"]
      .as_str()
      .unwrap_or_else(|| panic!("no id in {}", self.body))
      .to_owned()
  }

  /// The `error_code` of an error reply, or `fallback` for successful ones.
  pub fn error_code_or<'a>(&'a self, fallback: &'a str) -> &'a str {
    self.body["error_code"].as_str().unwrap_or(fallback)
  }

  /// The stable `error_code` of an error reply.
  pub fn error_code(&self) -> &str {
    self.body["error_code"]
      .as_str()
      .unwrap_or_else(|| panic!("no error_code in {}", self.body))
  }
}

struct StaticKeys;

impl JwkSource for StaticKeys {
  fn fetch(&self) -> crate::auth::BoxFuture<'_, Result<JwkSet, AuthError>> {
    Box::pin(async {
      let jwk = json!({"kty": "RSA", "kid": TEST_KEY_ID, "use": "sig", "alg": "RS256", "n": TEST_MODULUS_B64, "e": "AQAB"});
      Ok(JwkSet {
        keys: vec![serde_json::from_value(jwk).unwrap()],
      })
    })
  }
}

fn next_worker_id() -> u16 {
  static NEXT: AtomicU16 = AtomicU16::new(1);
  NEXT.fetch_add(1, Ordering::Relaxed) % 1024
}
