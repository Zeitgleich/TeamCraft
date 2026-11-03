//! Signing in as the import account (docs/import-synology.md §2, §7): OpenID Connect with the
//! authorization code and PKCE in the operator's browser, the redirect to a loopback port of this
//! host (RFC 8252 §7.3; over SSH the port is forwarded), and `offline_access`, so `serve` can run for
//! weeks. The refresh token is kept in the state directory, readable by this user only; the access
//! token is renewed before it expires.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use oauth2::{AuthUrl, ClientId, PkceCodeChallenge, RedirectUrl, TokenUrl};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use teamcraft_client_api::oidc_login::{LoginRequest, authorization_url, oidc_client};
use teamcraft_core::api::{AccessTokenSource, ApiError};
use teamcraft_proto::ServerInfoResponse;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{info, warn};

const TOKENS_FILE: &str = "sign-in.json";
const CALLBACK_PATH: &str = "/callback";
/// How long the browser may take for the sign-in.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Renew this long before the access token expires.
const REFRESH_MARGIN: Duration = Duration::from_secs(60);
/// Wait after a failed renewal (the provider or the network away).
const REFRESH_RETRY: Duration = Duration::from_secs(30);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
  #[error("not signed in yet: run `teamcraft-import login --server <url>` first")]
  NotSignedIn,
  #[error("the sign-in has expired at the identity provider: run `login` again")]
  Expired,
  #[error("the server or identity provider answered unexpectedly: {0}")]
  Provider(String),
  #[error("the browser sign-in failed: {0}")]
  Browser(String),
  #[error("{path}: {source}")]
  File { path: PathBuf, source: std::io::Error },
}

/// What is kept between runs. The refresh token is a secret: no `Debug`.
#[derive(Serialize, Deserialize)]
struct StoredSignIn {
  server_url: String,
  client_id: String,
  token_url: String,
  refresh_token: String,
}

/// The live sign-in: hands the core the current access token and renews it in the background.
#[derive(Clone)]
pub struct Session {
  current: Arc<Mutex<Current>>,
  pub server_url: String,
}

struct Current {
  access_token: SecretString,
  maybe_failure: Option<String>,
}

#[derive(Deserialize)]
struct TokenAnswer {
  access_token: String,
  #[serde(default)]
  refresh_token: Option<String>,
  #[serde(default)]
  expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct Discovery {
  authorization_endpoint: String,
  token_endpoint: String,
}

#[derive(Deserialize)]
struct TokenErrorAnswer {
  error: String,
}

impl AccessTokenSource for Session {
  fn access_token(&self) -> Result<String, ApiError> {
    let current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(failure) = &current.maybe_failure {
      return Err(ApiError::Token(failure.clone()));
    }
    Ok(current.access_token.expose_secret().to_owned())
  }
}

/// Signs in in the browser and keeps the refresh token in `state`.
pub async fn login(server_url: &str, state: &Path) -> Result<(), AuthError> {
  let http = http_client()?;
  let server_url = server_url.trim_end_matches('/').to_owned();
  let info: ServerInfoResponse = get_json(&http, &format!("{server_url}/api/v1/server-info")).await?;
  let discovery: Discovery = get_json(
    &http,
    &format!(
      "{}/.well-known/openid-configuration",
      info.sign_in.issuer.trim_end_matches('/')
    ),
  )
  .await?;
  let listener = TcpListener::bind(("127.0.0.1", 0))
    .await
    .map_err(|error| AuthError::Browser(error.to_string()))?;
  let port = listener
    .local_addr()
    .map_err(|error| AuthError::Browser(error.to_string()))?
    .port();
  let redirect = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");
  let client = oidc_client(
    ClientId::new(info.sign_in.client_id.clone()),
    AuthUrl::new(discovery.authorization_endpoint).map_err(|error| AuthError::Provider(error.to_string()))?,
    TokenUrl::new(discovery.token_endpoint.clone())
      .map_err(|error| AuthError::Provider(error.to_string()))?,
    RedirectUrl::new(redirect.clone()).map_err(|error| AuthError::Provider(error.to_string()))?,
  );
  let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
  let mut scopes = info.sign_in.scopes.clone();
  if !scopes.iter().any(|scope| scope == "offline_access") {
    scopes.push("offline_access".into());
  }
  let request = LoginRequest {
    is_login_page_shown: true,
    ..LoginRequest::default()
  };
  let (url, csrf) = authorization_url(&client, &scopes, challenge, &request);
  // The operator opens it; on a host without a browser after `ssh -L {port}:127.0.0.1:{port}`.
  info!(port, "waiting for the sign-in in the browser");
  crate::files::say(&format!(
    "Open this address in a browser and sign in as the import account:\n\n  {url}\n\nOn a remote host, forward the port first: ssh -L {port}:127.0.0.1:{port} <host>\n"
  ));
  let code = tokio::time::timeout(LOGIN_TIMEOUT, receive_code(&listener, csrf.secret()))
    .await
    .map_err(|_| AuthError::Browser("no sign-in within 15 minutes".into()))??;
  let answer = post_token(
    &http,
    &discovery.token_endpoint,
    &[
      ("grant_type", "authorization_code"),
      ("code", code.as_str()),
      ("redirect_uri", redirect.as_str()),
      ("client_id", info.sign_in.client_id.as_str()),
      ("code_verifier", verifier.secret()),
    ],
  )
  .await?;
  let refresh_token = answer
    .refresh_token
    .ok_or_else(|| AuthError::Provider("no refresh token; the client needs offline_access".into()))?;
  save(
    state,
    &StoredSignIn {
      server_url,
      client_id: info.sign_in.client_id,
      token_url: discovery.token_endpoint,
      refresh_token,
    },
  )?;
  crate::files::say("Signed in.");
  Ok(())
}

/// Resumes the stored sign-in: renews the access token now and then in the background.
pub async fn resume(state: &Path) -> Result<Session, AuthError> {
  let stored = load(state)?;
  let http = http_client()?;
  let (answer, stored) = refresh(&http, state, stored).await?;
  let session = Session {
    current: Arc::new(Mutex::new(Current {
      access_token: SecretString::from(answer.access_token),
      maybe_failure: None,
    })),
    server_url: stored.server_url.clone(),
  };
  let renewing = session.clone();
  let state = state.to_owned();
  let first_expiry = expiry_of(answer.expires_in);
  tokio::spawn(async move { renew_forever(renewing, http, state, stored, first_expiry).await });
  Ok(session)
}

async fn renew_forever(
  session: Session,
  http: reqwest::Client,
  state: PathBuf,
  mut stored: StoredSignIn,
  mut expires_at: Instant,
) {
  loop {
    let wait = expires_at
      .saturating_duration_since(Instant::now())
      .saturating_sub(REFRESH_MARGIN);
    tokio::time::sleep(wait).await;
    match refresh(&http, &state, stored).await {
      Ok((answer, renewed)) => {
        expires_at = expiry_of(answer.expires_in);
        stored = renewed;
        let mut current = session.current.lock().unwrap_or_else(PoisonError::into_inner);
        current.access_token = SecretString::from(answer.access_token);
        current.maybe_failure = None;
      },
      Err(AuthError::Expired) => {
        warn!("the sign-in expired at the identity provider; run `login` again");
        session
          .current
          .lock()
          .unwrap_or_else(PoisonError::into_inner)
          .maybe_failure = Some(AuthError::Expired.to_string());
        return;
      },
      Err(error) => {
        warn!(%error, "renewing the sign-in failed; trying again");
        // The stored token is still the one to use.
        match load(&state) {
          Ok(reloaded) => stored = reloaded,
          Err(error) => {
            warn!(%error, "the stored sign-in can't be read");
            return;
          },
        }
        expires_at = Instant::now() + REFRESH_MARGIN + REFRESH_RETRY;
      },
    }
  }
}

/// Renews the access token; the provider may rotate the refresh token, which is stored at once.
async fn refresh(
  http: &reqwest::Client,
  state: &Path,
  stored: StoredSignIn,
) -> Result<(TokenAnswer, StoredSignIn), AuthError> {
  let answer = post_token(
    http,
    &stored.token_url,
    &[
      ("grant_type", "refresh_token"),
      ("refresh_token", stored.refresh_token.as_str()),
      ("client_id", stored.client_id.as_str()),
    ],
  )
  .await?;
  let renewed = StoredSignIn {
    refresh_token: answer.refresh_token.clone().unwrap_or(stored.refresh_token),
    ..stored
  };
  save(state, &renewed)?;
  Ok((answer, renewed))
}

fn expiry_of(maybe_expires_in: Option<u64>) -> Instant {
  Instant::now() + Duration::from_secs(maybe_expires_in.unwrap_or(300))
}

async fn post_token(
  http: &reqwest::Client,
  url: &str,
  form: &[(&str, &str)],
) -> Result<TokenAnswer, AuthError> {
  let response = http
    .post(url)
    .form(form)
    .send()
    .await
    .map_err(|error| AuthError::Provider(error.to_string()))?;
  if response.status().is_success() {
    return response
      .json()
      .await
      .map_err(|error| AuthError::Provider(error.to_string()));
  }
  let status = response.status();
  let maybe_error: Option<TokenErrorAnswer> = response.json().await.ok();
  match maybe_error {
    Some(answer) if answer.error == "invalid_grant" => Err(AuthError::Expired),
    Some(answer) => Err(AuthError::Provider(format!("{status}: {}", answer.error))),
    None => Err(AuthError::Provider(status.to_string())),
  }
}

/// Waits for the browser's redirect and answers it; returns the code once its state matches.
async fn receive_code(listener: &TcpListener, expected_state: &str) -> Result<String, AuthError> {
  loop {
    let (mut stream, _) = listener
      .accept()
      .await
      .map_err(|error| AuthError::Browser(error.to_string()))?;
    let mut buffer = vec![0_u8; 8 * 1024];
    let read = stream
      .read(&mut buffer)
      .await
      .map_err(|error| AuthError::Browser(error.to_string()))?;
    let request = String::from_utf8_lossy(&buffer[..read]);
    let Some(target) = request
      .lines()
      .next()
      .and_then(|line| line.split_whitespace().nth(1))
    else {
      continue;
    };
    let Ok(url) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
      continue;
    };
    if url.path() != CALLBACK_PATH {
      respond(&mut stream, "404 Not Found", "Not found.").await;
      continue;
    }
    let param = |name: &str| {
      url
        .query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
    };
    if let Some(error) = param("error") {
      respond(
        &mut stream,
        "200 OK",
        "The sign-in was refused. You can close this tab.",
      )
      .await;
      return Err(AuthError::Browser(error));
    }
    if param("state").as_deref() != Some(expected_state) {
      respond(
        &mut stream,
        "400 Bad Request",
        "This sign-in doesn't belong to the running import.",
      )
      .await;
      continue;
    }
    match param("code") {
      Some(code) => {
        respond(&mut stream, "200 OK", "Signed in. You can close this tab.").await;
        return Ok(code);
      },
      None => respond(&mut stream, "400 Bad Request", "The redirect carried no code.").await,
    }
  }
}

async fn respond(stream: &mut tokio::net::TcpStream, status: &str, text: &str) {
  let response = format!(
    "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
    text.len()
  );
  // The browser's tab is all that waits on it.
  drop(stream.write_all(response.as_bytes()).await);
}

async fn get_json<T: serde::de::DeserializeOwned>(http: &reqwest::Client, url: &str) -> Result<T, AuthError> {
  http
    .get(url)
    .send()
    .await
    .and_then(reqwest::Response::error_for_status)
    .map_err(|error| AuthError::Provider(format!("{url}: {error}")))?
    .json()
    .await
    .map_err(|error| AuthError::Provider(format!("{url}: {error}")))
}

fn http_client() -> Result<reqwest::Client, AuthError> {
  reqwest::Client::builder()
    .timeout(HTTP_TIMEOUT)
    .redirect(reqwest::redirect::Policy::none())
    .build()
    .map_err(|error| AuthError::Provider(error.to_string()))
}

fn save(state: &Path, stored: &StoredSignIn) -> Result<(), AuthError> {
  let path = state.join(TOKENS_FILE);
  let json = serde_json::to_vec(stored).map_err(|error| AuthError::Provider(error.to_string()))?;
  crate::files::write_private(&path, &json).map_err(|source| AuthError::File { path, source })
}

fn load(state: &Path) -> Result<StoredSignIn, AuthError> {
  let path = state.join(TOKENS_FILE);
  match std::fs::read(&path) {
    Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| AuthError::Provider(error.to_string())),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(AuthError::NotSignedIn),
    Err(source) => Err(AuthError::File { path, source }),
  }
}
