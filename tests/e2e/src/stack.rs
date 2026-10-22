//! The system under test: a fresh database in the dev stack's Postgres and a `teamcraft-server`
//! process (optionally with SFU and meeting mail) in front of it, or an already deployed instance
//! (`E2E_DEPLOYED_URL`, see [`Stack::deployed`]).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use sqlx::Connection;

use crate::process::{Service, binary, free_tcp_port, free_udp_port};
use crate::tokens::UserTokens;

pub const KEYCLOAK_URL: &str = "http://127.0.0.1:8080";
pub const ISSUER: &str = "http://127.0.0.1:8080/realms/teamcraft-dev";
pub const MAILPIT_URL: &str = "http://127.0.0.1:8025";
const POSTGRES_ADMIN_URL: &str = "postgres://teamcraft:teamcraft-dev-only@127.0.0.1:5432/postgres";
const GARAGE_URL: &str = "http://127.0.0.1:3900";
const SFU_REGISTRATION_TOKEN: &str = "e2e-sfu-registration-token-0123456789";
const GUEST_TOKEN_SECRET: &str = "e2e-guest-token-secret-0123456789abcdef";
/// Throwaway Ed25519 key pair for call join tokens; it protects nothing (same as the server's tests).
const CALL_SIGNING_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MC4CAQAwBQYDK2VwBCIEIAxxS2yuWa9SUUAfZhI0772UQ48VXZLz45+L2A5IfcWA
-----END PRIVATE KEY-----
";
const CALL_PUBLIC_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEA8YVjY2Zv9XPhv003tFAgzW7J6gfDn+ICj5xCB2LXMVg=
-----END PUBLIC KEY-----
";
const START_TIMEOUT: Duration = Duration::from_secs(60);
/// Base URL of a deployed instance for [`Stack::deployed`], e.g. `https://chat.example.com`.
const DEPLOYED_URL_VAR: &str = "E2E_DEPLOYED_URL";

/// A running server with its own database, or a deployed instance the tests only talk to.
pub struct Stack {
  pub base_url: String,
  pub http: reqwest::Client,
  pub dir: PathBuf,
  pub maybe_sfu: Option<SfuHandle>,
  /// `None` for a deployed instance: no process and no database belong to the test.
  maybe_owned: Option<OwnedServer>,
}

/// The server process a test started and the database it created for it.
struct OwnedServer {
  database_name: String,
  server: Service,
}

/// The SFU process and what clients need to talk to it.
pub struct SfuHandle {
  pub service: Service,
  /// SHA-256 of the SFU's self-signed certificate.
  pub certificate_hash: [u8; 32],
}

/// What a test needs from the server beyond the defaults.
#[derive(Clone, Default)]
pub struct StackBuilder {
  has_calls: bool,
  has_meetings: bool,
  has_mail: bool,
  has_local_accounts: bool,
  retention_interval_secs: Option<u64>,
  web_dir: Option<PathBuf>,
  maybe_port: Option<u16>,
  extra_env: Vec<(String, String)>,
}

impl StackBuilder {
  /// Starts an SFU and configures calls on the server.
  pub fn with_calls(mut self) -> Self {
    self.has_calls = true;
    self
  }

  /// Configures meetings with invitations through the dev stack's Mailpit.
  pub fn with_meetings(mut self) -> Self {
    self.has_meetings = true;
    self
  }

  /// Configures the server's own mail (`[mail.smtp]`: activation and notification e-mail) to go to
  /// Mailpit.
  pub fn with_mail(mut self) -> Self {
    self.has_mail = true;
    self
  }

  /// Enables local accounts (password + TOTP), for guests without an identity provider account.
  /// Sign-in without a code is allowed until a code was confirmed, unless the policy requires MFA.
  pub fn with_local_accounts(mut self) -> Self {
    self.has_local_accounts = true;
    self
  }

  /// Runs the retention job every second instead of hourly.
  pub fn with_fast_retention(mut self) -> Self {
    self.retention_interval_secs = Some(1);
    self
  }

  /// Serves the web client (`trunk build` output) at `/`.
  pub fn with_web_dir(mut self, dir: PathBuf) -> Self {
    self.web_dir = Some(dir);
    self
  }

  /// Listens on a fixed port. The dev realm only registers the web client's redirect URIs for
  /// `http://127.0.0.1:8082/*`, so browser tests must use 8082.
  pub fn with_port(mut self, port: u16) -> Self {
    self.maybe_port = Some(port);
    self
  }

  pub fn with_env(mut self, key: &str, value: &str) -> Self {
    self.extra_env.push((key.to_owned(), value.to_owned()));
    self
  }

  /// `None` (a skip message on stderr) when the dev stack isn't running, unless
  /// `E2E_REQUIRED=1` demands it: then the missing stack fails the test. This keeps
  /// `cargo test --workspace --all-features` green on machines without Docker.
  pub async fn start(self) -> Option<Stack> {
    let is_required = std::env::var("E2E_REQUIRED").is_ok_and(|value| value == "1");
    if let Err(reason) = check_dev_stack(self.has_meetings || self.has_mail, self.has_calls).await {
      assert!(
        !is_required,
        "the dev stack is required (E2E_REQUIRED=1): {reason}"
      );
      eprintln!(
        "skipping: {reason} (docker compose -f docker-compose.dev.yml up -d; see tests/e2e/README.md)"
      );
      return None;
    }
    match Stack::launch(self).await {
      Ok(stack) => Some(stack),
      Err(error) => panic!("starting the stack failed: {error:#}"),
    }
  }
}

impl Stack {
  pub fn builder() -> StackBuilder {
    StackBuilder::default()
  }

  /// A server with the defaults.
  pub async fn start() -> Option<Stack> {
    Self::builder().start().await
  }

  /// An instance deployed elsewhere (the deploy dry run, docs/operations.md), at `$E2E_DEPLOYED_URL`.
  /// It must use the dev Keycloak realm, since the tokens come from there. `None` (the test skips)
  /// when the variable is not set; `E2E_REQUIRED` does not apply, the suite never needs one.
  /// Clients trust the system roots, so for a private CA point `SSL_CERT_FILE` at its certificate.
  pub fn deployed() -> Option<Stack> {
    let Ok(base_url) = std::env::var(DEPLOYED_URL_VAR) else {
      eprintln!("skipping: {DEPLOYED_URL_VAR} is not set");
      return None;
    };
    let dir = std::env::temp_dir().join(format!("teamcraft-e2e-deployed-{:016x}", rand_u64()));
    std::fs::create_dir_all(&dir).expect("creating the test directory");
    Some(Self {
      base_url: base_url.trim_end_matches('/').to_owned(),
      http: reqwest::Client::new(),
      dir,
      maybe_sfu: None,
      maybe_owned: None,
    })
  }

  /// A bearer-token source for a dev user (`alice`, `bob`, `carol`, `dave`, ...).
  pub fn tokens(&self, user: &str) -> UserTokens {
    UserTokens::new(user)
  }

  /// The log of the server process; empty for a deployed instance.
  pub fn server_log(&self) -> String {
    self
      .maybe_owned
      .as_ref()
      .map(|owned| owned.server.plain_log())
      .unwrap_or_default()
  }

  /// Stops the processes and drops the database.
  pub async fn stop(self) {
    let Self {
      maybe_owned,
      maybe_sfu,
      dir,
      ..
    } = self;
    drop(maybe_sfu);
    let _ = std::fs::remove_dir_all(&dir);
    let Some(OwnedServer {
      database_name,
      server,
    }) = maybe_owned
    else {
      return;
    };
    drop(server);
    // Best effort: the runner script also removes leftover e2e databases.
    if let Ok(mut connection) = sqlx::PgConnection::connect(POSTGRES_ADMIN_URL).await {
      let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name} WITH (FORCE)"
      )))
      .execute(&mut connection)
      .await;
    }
  }

  async fn launch(settings: StackBuilder) -> anyhow::Result<Self> {
    let suffix: u64 = rand_u64();
    let database_name = format!("teamcraft_e2e_{suffix:016x}");
    let dir = std::env::temp_dir().join(format!("teamcraft-e2e-{suffix:016x}"));
    std::fs::create_dir_all(&dir)?;
    let mut admin = sqlx::PgConnection::connect(POSTGRES_ADMIN_URL)
      .await
      .context("connecting to the dev Postgres")?;
    // Invariant: the name is `teamcraft_e2e_` plus hex digits.
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database_name}")))
      .execute(&mut admin)
      .await
      .context("creating the test database")?;

    let port = settings.maybe_port.unwrap_or_else(free_tcp_port);
    let base_url = format!("http://127.0.0.1:{port}");
    let sfu_port = free_udp_port();
    let mut envs = vec![
      ("TEAMCRAFT_SERVER__LISTEN_ADDR".into(), format!("[::]:{port}")),
      (
        "TEAMCRAFT_DATABASE__URL".into(),
        format!("postgres://teamcraft:teamcraft-dev-only@127.0.0.1:5432/{database_name}"),
      ),
      ("TEAMCRAFT_OIDC__ISSUER".into(), ISSUER.into()),
      ("TEAMCRAFT_OIDC__CLIENT_ID".into(), "teamcraft".into()),
      ("TEAMCRAFT_OIDC__SERVER_CLIENT".into(), "teamcraft-server".into()),
      ("TEAMCRAFT_STORAGE__ENDPOINT".into(), GARAGE_URL.into()),
      ("TEAMCRAFT_STORAGE__REGION".into(), "garage".into()),
      ("TEAMCRAFT_STORAGE__BUCKET".into(), "teamcraft-dev".into()),
      (
        "TEAMCRAFT_STORAGE__ACCESS_KEY".into(),
        "GKdeadbeef0000000000000000".into(),
      ),
      ("TEAMCRAFT_STORAGE__SECRET_KEY".into(), "deadbeef".repeat(8)),
      ("TEAMCRAFT_STORAGE__PATH_STYLE".into(), "true".into()),
      ("TEAMCRAFT_LOGGING__FILTER".into(), "info".into()),
      ("NO_COLOR".into(), "1".into()),
    ];
    if let Some(interval) = settings.retention_interval_secs {
      envs.push(("TEAMCRAFT_RETENTION__INTERVAL_SECS".into(), interval.to_string()));
      envs.push(("TEAMCRAFT_RETENTION__INITIAL_DELAY_SECS".into(), "1".into()));
    }
    if let Some(web_dir) = &settings.web_dir {
      envs.push(("TEAMCRAFT_WEB__STATIC_DIR".into(), web_dir.display().to_string()));
    }
    if settings.has_mail {
      envs.extend([
        ("TEAMCRAFT_MAIL__PUBLIC_BASE_URL".into(), base_url.clone()),
        ("TEAMCRAFT_MAIL__SMTP__HOST".into(), "127.0.0.1".into()),
        ("TEAMCRAFT_MAIL__SMTP__PORT".into(), "1025".into()),
        ("TEAMCRAFT_MAIL__SMTP__SECURITY".into(), "none".into()),
        (
          "TEAMCRAFT_MAIL__SMTP__FROM_ADDRESS".into(),
          "noreply@example.com".into(),
        ),
      ]);
    }
    if settings.has_local_accounts {
      envs.extend([
        ("TEAMCRAFT_LOCAL_ACCOUNTS__IS_ENABLED".into(), "true".into()),
        (
          "TEAMCRAFT_LOCAL_ACCOUNTS__SESSION_SECRET".into(),
          "e2e-local-session-secret-0123456789abcdef".into(),
        ),
        ("TEAMCRAFT_LOCAL_ACCOUNTS__REQUIRE_TOTP".into(), "false".into()),
        (
          "TEAMCRAFT_LOCAL_ACCOUNTS__PASSWORD_MEMORY_KIB".into(),
          "1024".into(),
        ),
        ("TEAMCRAFT_LOCAL_ACCOUNTS__PASSWORD_ITERATIONS".into(), "1".into()),
      ]);
    }
    if settings.has_meetings {
      envs.extend([
        ("TEAMCRAFT_MEETINGS__PUBLIC_BASE_URL".into(), base_url.clone()),
        (
          "TEAMCRAFT_MEETINGS__ORGANIZER_FALLBACK_ADDRESS".into(),
          "meetings@example.com".into(),
        ),
        (
          "TEAMCRAFT_MEETINGS__GUEST_TOKEN_SECRET".into(),
          GUEST_TOKEN_SECRET.into(),
        ),
        ("TEAMCRAFT_MEETINGS__SMTP__HOST".into(), "127.0.0.1".into()),
        ("TEAMCRAFT_MEETINGS__SMTP__PORT".into(), "1025".into()),
        ("TEAMCRAFT_MEETINGS__SMTP__SECURITY".into(), "none".into()),
        (
          "TEAMCRAFT_MEETINGS__SMTP__FROM_ADDRESS".into(),
          "meetings@example.com".into(),
        ),
      ]);
    }
    if settings.has_calls {
      let key_path = dir.join("call-signing.pem");
      std::fs::write(&key_path, CALL_SIGNING_KEY_PEM)?;
      envs.extend([
        (
          "TEAMCRAFT_CALLS__SFU_URL".into(),
          format!("https://[::1]:{sfu_port}/call"),
        ),
        ("TEAMCRAFT_CALLS__ISSUER".into(), base_url.clone()),
        (
          "TEAMCRAFT_CALLS__SIGNING_KEY_PATH".into(),
          key_path.display().to_string(),
        ),
        (
          "TEAMCRAFT_CALLS__SFU_REGISTRATION_TOKEN".into(),
          SFU_REGISTRATION_TOKEN.into(),
        ),
      ]);
    }
    envs.extend(settings.extra_env.iter().cloned());

    let mut server = Service::spawn("server", &binary("teamcraft-server"), &[], &envs, &dir)?;
    server
      .wait_for_http(&format!("{base_url}/healthz"), START_TIMEOUT)
      .await?;

    let maybe_sfu = if settings.has_calls {
      Some(start_sfu(&dir, &base_url, sfu_port).await?)
    } else {
      None
    };
    Ok(Self {
      base_url,
      http: reqwest::Client::new(),
      dir,
      maybe_sfu,
      maybe_owned: Some(OwnedServer {
        database_name,
        server,
      }),
    })
  }
}

async fn start_sfu(dir: &std::path::Path, server_url: &str, port: u16) -> anyhow::Result<SfuHandle> {
  let public_key_path = dir.join("call-signing.pub.pem");
  std::fs::write(&public_key_path, CALL_PUBLIC_KEY_PEM)?;
  let envs = vec![
    ("TEAMCRAFT_SFU_BIND".into(), format!("[::]:{port}")),
    (
      "TEAMCRAFT_SFU_HEALTH_BIND".into(),
      format!("[::]:{}", free_tcp_port()),
    ),
    (
      "TEAMCRAFT_SFU_JOIN_TOKEN_PUBLIC_KEY".into(),
      public_key_path.display().to_string(),
    ),
    ("TEAMCRAFT_SFU_JOIN_TOKEN_ISSUER".into(), server_url.into()),
    ("TEAMCRAFT_SFU_SERVER_URL".into(), server_url.into()),
    (
      "TEAMCRAFT_SFU_REGISTRATION_TOKEN".into(),
      SFU_REGISTRATION_TOKEN.into(),
    ),
    ("NO_COLOR".into(), "1".into()),
    ("RUST_LOG".into(), "info".into()),
  ];
  let mut service = Service::spawn("sfu", &binary("teamcraft-sfu"), &[], &envs, dir)?;
  service.wait_for_log("SFU listening", START_TIMEOUT).await?;
  let certificate_hash = parse_certificate_hash(&service.plain_log())?;
  Ok(SfuHandle {
    service,
    certificate_hash,
  })
}

/// `certificate_hash=AB:CD:...` (dotted hex) from the SFU's startup line.
fn parse_certificate_hash(log: &str) -> anyhow::Result<[u8; 32]> {
  let line = log
    .lines()
    .find(|line| line.contains("certificate_hash="))
    .context("no certificate_hash in the SFU log")?;
  let value = line
    .split("certificate_hash=")
    .nth(1)
    .and_then(|rest| rest.split_whitespace().next())
    .context("certificate_hash has no value")?;
  let bytes: Vec<u8> = value
    .split(':')
    .map(|byte| u8::from_str_radix(byte, 16))
    .collect::<Result<_, _>>()
    .with_context(|| format!("certificate hash {value:?}"))?;
  bytes
    .try_into()
    .map_err(|bytes: Vec<u8>| anyhow::anyhow!("certificate hash has {} bytes", bytes.len()))
}

async fn check_dev_stack(needs_mailpit: bool, needs_sfu: bool) -> Result<(), String> {
  let http = reqwest::Client::builder()
    .timeout(Duration::from_secs(3))
    .build()
    .map_err(|error| error.to_string())?;
  let realm = format!("{ISSUER}/.well-known/openid-configuration");
  http
    .get(&realm)
    .send()
    .await
    .and_then(reqwest::Response::error_for_status)
    .map_err(|error| format!("Keycloak is not reachable at {KEYCLOAK_URL}: {error}"))?;
  sqlx::PgConnection::connect(POSTGRES_ADMIN_URL)
    .await
    .map_err(|error| format!("Postgres is not reachable: {error}"))?;
  tokio::net::TcpStream::connect("127.0.0.1:3900")
    .await
    .map_err(|error| format!("Garage is not reachable: {error}"))?;
  if needs_mailpit {
    http
      .get(format!("{MAILPIT_URL}/api/v1/info"))
      .send()
      .await
      .map_err(|error| format!("Mailpit is not reachable at {MAILPIT_URL}: {error}"))?;
  }
  let missing = ["teamcraft-server"]
    .into_iter()
    .chain(needs_sfu.then_some("teamcraft-sfu"))
    .find(|name| !binary(name).exists());
  if let Some(name) = missing {
    return Err(format!(
      "{} is not built: cargo build -p teamcraft-server -p teamcraft-sfu -p teamcraft-bridge",
      binary(name).display()
    ));
  }
  Ok(())
}

fn rand_u64() -> u64 {
  use rand_core::RngCore;
  rand_core::OsRng.next_u64()
}
