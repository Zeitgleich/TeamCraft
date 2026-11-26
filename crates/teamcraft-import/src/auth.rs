//! Signing in as the import account (docs/import-synology.md §2, §7): OpenID Connect with the
//! authorization code and PKCE in the operator's browser, the redirect to a loopback port of this
//! host (RFC 8252 §7.3; over SSH the port is forwarded), and `offline_access`, so `serve` can run for
//! weeks. The refresh token is kept in the state directory, readable by this user only; the access
//! token is renewed before it expires.
//!
//! Where the identity provider and the server both take DPoP proofs (e2ee.md §3.1.2), the sign-in
//! is bound to a key in the state directory (`proof_key.rs`) as the apps bind theirs, so that a
//! server under the policy `required` takes the import: token requests and API requests carry
//! proofs, and a bound refresh token is never sent without one.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use oauth2::{AuthUrl, ClientId, PkceCodeChallenge, RedirectUrl, TokenUrl};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use teamcraft_client_api::oidc_login::{LoginRequest, authorization_url, oidc_client};
use teamcraft_client_api::server_entry::{HttpPolicy, check_discovery, is_provider_endpoint};
use teamcraft_core::api::dpop::{
  Authorization, DPOP_ALGORITHM_FOR_DISCOVERY, DPOP_HEADER, DPOP_NONCE_HEADER, DpopAnswer, DpopProofError,
  DpopProver, ProofFallback, SoftwareDpopSigner, TokenEndpointAttempt, is_from_a_cache,
  with_token_endpoint_proof,
};
use teamcraft_core::api::{AccessTokenSource, ApiError};
use teamcraft_crypto::dpop::{DpopSigningKey, JwkThumbprint};
use teamcraft_proto::ServerInfoResponse;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{info, warn};

use crate::proof_key::{self, ProofKeyError};

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
  /// The refresh token and the authorization code go to the identity provider: only over https
  /// (plain http on loopback, a provider on this host, for development).
  #[error("the identity provider is not reached over https: {0}")]
  InsecureProvider(String),
  #[error("{path}: {source}")]
  File { path: PathBuf, source: std::io::Error },
  /// The sign-in is bound to a key the state directory doesn't hold (any more).
  #[error("the sign-in is bound to a DPoP key this state directory doesn't have: run `login` again")]
  KeyLost,
  #[error(transparent)]
  Key(#[from] ProofKeyError),
  #[error(transparent)]
  Proof(#[from] DpopProofError),
}

/// What is kept between runs. The refresh token is a secret: no `Debug`.
#[derive(Serialize, Deserialize)]
struct StoredSignIn {
  server_url: String,
  client_id: String,
  token_url: String,
  refresh_token: String,
  /// The thumbprint of the key the refresh token is bound to, if it is (`cnf.jkt`, RFC 7638).
  #[serde(default)]
  maybe_bound_key: Option<String>,
}

/// The live sign-in: hands the core the current access token and renews it in the background.
#[derive(Clone)]
pub struct Session {
  current: Arc<Mutex<Current>>,
  pub server_url: String,
  /// Signs the server's proofs; `Some` for a bound sign-in only.
  maybe_prover: Option<Arc<DpopProver>>,
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
  issuer: String,
  authorization_endpoint: String,
  token_endpoint: String,
  #[serde(default)]
  dpop_signing_alg_values_supported: Vec<String>,
}

/// The key a sign-in is about to be bound with: one prover for the server, one for the provider,
/// each with its own nonce.
struct Binding {
  key: JwkThumbprint,
  provider: DpopProver,
}

/// What `GET /server-info` says about proofs.
enum ServerProofs {
  /// It checks them: the nonce and the server's `Date` for the first proof.
  Offered {
    nonce: String,
    maybe_date: Option<String>,
  },
  NotOffered,
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

  fn dpop_prover(&self) -> Option<Arc<DpopProver>> {
    self.maybe_prover.clone()
  }
}

/// Signs in in the browser and keeps the refresh token in `state`.
pub async fn login(server_url: &str, state: &Path) -> Result<(), AuthError> {
  let http = http_client()?;
  let server_url = server_url.trim_end_matches('/').to_owned();
  let (info, server_proofs) = server_info(&http, &server_url).await?;
  let discovery = provider_discovery(&http, &info.sign_in.issuer).await?;
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
    AuthUrl::new(discovery.authorization_endpoint.clone())
      .map_err(|error| AuthError::Provider(error.to_string()))?,
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
  let maybe_binding = binding_for_login(state, &discovery, server_proofs)?;
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
    // Like the apps: a provider that refuses the proof after all leaves the sign-in unbound, which a
    // server under `required` then refuses with `dpop_required`.
    maybe_binding
      .as_ref()
      .map(|binding| (&binding.provider, ProofFallback::OnRefusal)),
  )
  .await?;
  let maybe_bound_key = bound_key_of(
    &answer.access_token,
    maybe_binding.as_ref().map(|binding| &binding.key),
  )?;
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
      maybe_bound_key: maybe_bound_key.as_ref().map(|key| key.as_str().to_owned()),
    },
  )?;
  match maybe_bound_key {
    Some(_) => crate::files::say("Signed in, bound to this state directory's key (DPoP)."),
    None => crate::files::say("Signed in."),
  }
  Ok(())
}

/// The key a new sign-in is bound with, if the provider takes proofs and the server checks them (it
/// offers its nonce). Otherwise the sign-in stays unbound, as it always was: nobody is locked out by
/// a binding the other side doesn't know. Each sign-in makes a new key.
fn binding_for_login(
  state: &Path,
  discovery: &Discovery,
  server_proofs: ServerProofs,
) -> Result<Option<Binding>, AuthError> {
  let takes_proofs = discovery
    .dpop_signing_alg_values_supported
    .iter()
    .any(|algorithm| algorithm == DPOP_ALGORITHM_FOR_DISCOVERY);
  let ServerProofs::Offered { nonce, maybe_date } = server_proofs else {
    info!("the server doesn't check DPoP proofs: the sign-in is not bound to a key");
    return Ok(None);
  };
  if !takes_proofs {
    info!("the identity provider doesn't take DPoP proofs: the sign-in is not bound to a key");
    return Ok(None);
  }
  let key = proof_key::create(state)?;
  let server = prover_for(key);
  // The server's `Date` dates the provider's proofs too, until the provider's own answer is read.
  server.note_answer(DpopAnswer {
    maybe_nonce: Some(&nonce),
    maybe_date: maybe_date.as_deref(),
  });
  Ok(Some(Binding {
    key: server.thumbprint(),
    provider: server.for_another_party(),
  }))
}

/// The key a token is bound to, checked against the one it should be bound to. A token bound to any
/// other key couldn't be used.
fn bound_key_of(
  access_token: &str,
  maybe_key: Option<&JwkThumbprint>,
) -> Result<Option<JwkThumbprint>, AuthError> {
  match (Authorization::of(access_token), maybe_key) {
    (Authorization::Bearer, maybe_key) => {
      if maybe_key.is_some() {
        warn!("the identity provider issued an unbound token although the request had a proof");
      }
      Ok(None)
    },
    (Authorization::Bound(bound), Some(key)) if bound == *key => Ok(Some(bound)),
    (Authorization::Bound(_), _) => Err(AuthError::KeyLost),
  }
}

/// Resumes the stored sign-in: renews the access token now and then in the background.
pub async fn resume(state: &Path) -> Result<Session, AuthError> {
  let stored = load(state)?;
  let http = http_client()?;
  let maybe_server_prover = stored_prover(state, &stored)?;
  // The provider's nonce is its own; the clock starts from the local one.
  let maybe_provider_prover = maybe_server_prover
    .as_ref()
    .map(|prover| Arc::new(prover.for_another_party()));
  let (answer, stored) = refresh(&http, state, stored, maybe_provider_prover.as_deref()).await?;
  let session = Session {
    current: Arc::new(Mutex::new(Current {
      access_token: SecretString::from(answer.access_token),
      maybe_failure: None,
    })),
    server_url: stored.server_url.clone(),
    maybe_prover: maybe_server_prover,
  };
  let renewing = session.clone();
  let state = state.to_owned();
  let first_expiry = expiry_of(answer.expires_in);
  tokio::spawn(async move {
    renew_forever(renewing, http, state, stored, maybe_provider_prover, first_expiry).await;
  });
  Ok(session)
}

/// The prover of a bound sign-in, with the key it is bound to and no other; `None` for an unbound one.
fn stored_prover(state: &Path, stored: &StoredSignIn) -> Result<Option<Arc<DpopProver>>, AuthError> {
  let Some(bound) = &stored.maybe_bound_key else {
    return Ok(None);
  };
  let key = proof_key::load(state)?.ok_or(AuthError::KeyLost)?;
  if key.thumbprint().as_str() != bound {
    return Err(AuthError::KeyLost);
  }
  Ok(Some(Arc::new(prover_for(key))))
}

fn prover_for(key: DpopSigningKey) -> DpopProver {
  DpopProver::new(Arc::new(SoftwareDpopSigner::new(key)))
}

async fn renew_forever(
  session: Session,
  http: reqwest::Client,
  state: PathBuf,
  mut stored: StoredSignIn,
  maybe_provider_prover: Option<Arc<DpopProver>>,
  mut expires_at: Instant,
) {
  loop {
    let wait = expires_at
      .saturating_duration_since(Instant::now())
      .saturating_sub(REFRESH_MARGIN);
    tokio::time::sleep(wait).await;
    match refresh(&http, &state, stored, maybe_provider_prover.as_deref()).await {
      Ok((answer, renewed)) => {
        expires_at = expiry_of(answer.expires_in);
        stored = renewed;
        let mut current = session.current.lock().unwrap_or_else(PoisonError::into_inner);
        current.access_token = SecretString::from(answer.access_token);
        current.maybe_failure = None;
      },
      Err(error @ (AuthError::Expired | AuthError::KeyLost)) => {
        warn!(%error, "the sign-in can't be renewed; run `login` again");
        session
          .current
          .lock()
          .unwrap_or_else(PoisonError::into_inner)
          .maybe_failure = Some(error.to_string());
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

/// Renews the access token; the provider may rotate the refresh token, which is stored at once. A
/// bound refresh token goes with a proof of its key and never without (`maybe_prover` is `Some`
/// exactly for a bound sign-in).
async fn refresh(
  http: &reqwest::Client,
  state: &Path,
  stored: StoredSignIn,
  maybe_prover: Option<&DpopProver>,
) -> Result<(TokenAnswer, StoredSignIn), AuthError> {
  if stored.maybe_bound_key.is_some() && maybe_prover.is_none() {
    return Err(AuthError::KeyLost);
  }
  // The state file is the operator's, but a refresh token never goes out in the clear.
  if !is_provider_endpoint(&stored.token_url, HttpPolicy::LoopbackOnly) {
    return Err(AuthError::InsecureProvider(stored.token_url));
  }
  let answer = post_token(
    http,
    &stored.token_url,
    &[
      ("grant_type", "refresh_token"),
      ("refresh_token", stored.refresh_token.as_str()),
      ("client_id", stored.client_id.as_str()),
    ],
    maybe_prover.map(|prover| (prover, ProofFallback::None)),
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

/// A token request; with `maybe_proof`, with a proof of its prover, a nonce challenge answered once,
/// and the fallback for a provider that refuses proofs.
async fn post_token(
  http: &reqwest::Client,
  url: &str,
  form: &[(&str, &str)],
  maybe_proof: Option<(&DpopProver, ProofFallback)>,
) -> Result<TokenAnswer, AuthError> {
  let attempt = |maybe_proof: Option<String>| async move {
    let mut request = http.post(url).form(form);
    if let Some(proof) = maybe_proof {
      request = request.header(DPOP_HEADER, proof);
    }
    let response = request
      .send()
      .await
      .map_err(|error| AuthError::Provider(error.to_string()))?;
    let header = |name: &str| {
      response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
    };
    let status = response.status().as_u16();
    let maybe_nonce = header(DPOP_NONCE_HEADER);
    let maybe_date = header("date").filter(|_| !is_from_a_cache(header("age").as_deref()));
    let body = response
      .bytes()
      .await
      .map_err(|error| AuthError::Provider(error.to_string()))?
      .to_vec();
    Ok::<_, AuthError>(TokenEndpointAttempt {
      status,
      body: body.clone(),
      maybe_nonce,
      maybe_date,
      response: (status, body),
    })
  };
  let (status, body) = match maybe_proof {
    Some((prover, fallback)) => with_token_endpoint_proof(prover, url, fallback, attempt).await?,
    None => attempt(None).await?.response,
  };
  token_answer(status, &body)
}

fn token_answer(status: u16, body: &[u8]) -> Result<TokenAnswer, AuthError> {
  if (200..300).contains(&status) {
    return serde_json::from_slice(body).map_err(|error| AuthError::Provider(error.to_string()));
  }
  match serde_json::from_slice::<TokenErrorAnswer>(body) {
    Ok(answer) if answer.error == "invalid_grant" => Err(AuthError::Expired),
    Ok(answer) => Err(AuthError::Provider(format!("{status}: {}", answer.error))),
    Err(_) => Err(AuthError::Provider(format!("status {status}"))),
  }
}

/// `GET /server-info`, past any cache on the way: the sign-in settings, and whether the server checks
/// proofs (its `DPoP-Nonce`), with its `Date` to date them by.
async fn server_info(
  http: &reqwest::Client,
  server_url: &str,
) -> Result<(ServerInfoResponse, ServerProofs), AuthError> {
  let url = format!("{server_url}/api/v1/server-info");
  let response = http
    .get(&url)
    .header(reqwest::header::CACHE_CONTROL, "no-store")
    .send()
    .await
    .and_then(reqwest::Response::error_for_status)
    .map_err(|error| AuthError::Provider(format!("{url}: {error}")))?;
  let header = |name: &str| {
    response
      .headers()
      .get(name)
      .and_then(|value| value.to_str().ok())
      .map(str::to_owned)
  };
  let maybe_date = header("date").filter(|_| !is_from_a_cache(header("age").as_deref()));
  let proofs = match header(DPOP_NONCE_HEADER) {
    Some(nonce) => ServerProofs::Offered { nonce, maybe_date },
    None => ServerProofs::NotOffered,
  };
  let info = response
    .json()
    .await
    .map_err(|error| AuthError::Provider(format!("{url}: {error}")))?;
  Ok((info, proofs))
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

/// The identity provider's discovery document, fetched and checked like the apps do: an https issuer
/// (plain http on loopback only), the document for exactly that issuer, and https endpoints.
async fn provider_discovery(http: &reqwest::Client, issuer: &str) -> Result<Discovery, AuthError> {
  if !is_provider_endpoint(issuer, HttpPolicy::LoopbackOnly) {
    return Err(AuthError::InsecureProvider(issuer.to_owned()));
  }
  let discovery: Discovery = get_json(
    http,
    &format!(
      "{}/.well-known/openid-configuration",
      issuer.trim_end_matches('/')
    ),
  )
  .await?;
  check_provider_discovery(issuer, &discovery)?;
  Ok(discovery)
}

fn check_provider_discovery(issuer: &str, discovery: &Discovery) -> Result<(), AuthError> {
  check_discovery(
    issuer,
    &discovery.issuer,
    &[&discovery.authorization_endpoint, &discovery.token_endpoint],
    HttpPolicy::LoopbackOnly,
  )
  .map_err(|error| AuthError::InsecureProvider(error.to_string()))
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

#[cfg(test)]
mod tests {
  use base64::Engine;
  use base64::engine::general_purpose::URL_SAFE_NO_PAD;

  use super::*;

  const NONCE: &str = "provider-nonce-1";

  mod binding {
    use super::*;

    #[test]
    fn a_sign_in_is_bound_only_where_provider_and_server_both_check_proofs() {
      let dir = temporary_dir("bound-when");
      for (algorithms, offered, is_bound) in [
        (vec!["ES256"], true, true),
        (vec!["RS256"], true, false),
        (vec![], true, false),
        (vec!["ES256"], false, false),
      ] {
        let discovery = discovery(&algorithms);
        let proofs = if offered {
          ServerProofs::Offered {
            nonce: "server-nonce".into(),
            maybe_date: None,
          }
        } else {
          ServerProofs::NotOffered
        };
        let binding = binding_for_login(&dir, &discovery, proofs).unwrap();
        assert_eq!(binding.is_some(), is_bound, "{algorithms:?} {offered}");
        if let Some(binding) = binding {
          assert_eq!(proof_key::load(&dir).unwrap().unwrap().thumbprint(), binding.key);
        }
      }
      drop(std::fs::remove_dir_all(&dir));
    }

    #[test]
    fn a_token_bound_to_another_key_is_refused() {
      let ours = DpopSigningKey::generate().thumbprint();
      let theirs = DpopSigningKey::generate().thumbprint();
      assert_eq!(
        bound_key_of(&token_bound_to(&ours), Some(&ours)).unwrap(),
        Some(ours.clone())
      );
      assert!(matches!(
        bound_key_of(&token_bound_to(&theirs), Some(&ours)),
        Err(AuthError::KeyLost)
      ));
      assert!(matches!(
        bound_key_of(&token_bound_to(&theirs), None),
        Err(AuthError::KeyLost)
      ));
      assert_eq!(bound_key_of("unbound.token.here", Some(&ours)).unwrap(), None);
    }

    #[test]
    fn a_bound_sign_in_needs_the_key_it_was_bound_with() {
      let dir = temporary_dir("needs-key");
      let mut stored = stored_sign_in(None);
      assert!(stored_prover(&dir, &stored).unwrap().is_none());

      let key = proof_key::create(&dir).unwrap();
      stored.maybe_bound_key = Some(key.thumbprint().as_str().to_owned());
      assert_eq!(
        stored_prover(&dir, &stored).unwrap().unwrap().thumbprint(),
        key.thumbprint()
      );

      proof_key::create(&dir).unwrap();
      assert!(matches!(stored_prover(&dir, &stored), Err(AuthError::KeyLost)));
      std::fs::remove_file(dir.join("dpop.key")).unwrap();
      assert!(matches!(stored_prover(&dir, &stored), Err(AuthError::KeyLost)));
      drop(std::fs::remove_dir_all(&dir));
    }

    /// A bound refresh token never goes out without its proof.
    #[tokio::test]
    async fn a_bound_refresh_without_its_key_sends_nothing() {
      let dir = temporary_dir("no-send");
      let stored = StoredSignIn {
        token_url: "http://[::1]:9/never".into(),
        ..stored_sign_in(Some("bound".into()))
      };
      let result = refresh(&http_client().unwrap(), &dir, stored, None).await;
      assert!(matches!(result, Err(AuthError::KeyLost)));
      drop(std::fs::remove_dir_all(&dir));
    }
  }

  mod provider_transport {
    use super::*;

    #[test]
    fn discovery_endpoints_must_be_https_except_on_loopback() {
      assert!(check_provider_discovery("https://id.example.com", &discovery(&[])).is_ok());
      let plain = Discovery {
        token_endpoint: "http://id.example.com/token".into(),
        ..discovery(&[])
      };
      assert!(matches!(
        check_provider_discovery("https://id.example.com", &plain),
        Err(AuthError::InsecureProvider(_))
      ));
      let other_issuer = Discovery {
        issuer: "https://other.example.com".into(),
        ..discovery(&[])
      };
      assert!(check_provider_discovery("https://id.example.com", &other_issuer).is_err());
      let local = Discovery {
        issuer: "http://127.0.0.1:8080".into(),
        authorization_endpoint: "http://127.0.0.1:8080/auth".into(),
        token_endpoint: "http://127.0.0.1:8080/token".into(),
        dpop_signing_alg_values_supported: Vec::new(),
      };
      assert!(check_provider_discovery("http://127.0.0.1:8080", &local).is_ok());
    }

    #[tokio::test]
    async fn a_plain_http_issuer_is_refused_before_any_request() {
      let result = provider_discovery(&http_client().unwrap(), "http://id.example.com").await;
      assert!(matches!(result, Err(AuthError::InsecureProvider(_))));
    }

    #[tokio::test]
    async fn a_refresh_token_never_goes_to_a_plain_http_provider() {
      let dir = temporary_dir("plain-http");
      let stored = StoredSignIn {
        token_url: "http://id.example.com/token".into(),
        ..stored_sign_in(None)
      };
      let result = refresh(&http_client().unwrap(), &dir, stored, None).await;
      assert!(matches!(result, Err(AuthError::InsecureProvider(_))));
      drop(std::fs::remove_dir_all(&dir));
    }
  }

  mod token_requests {
    use super::*;

    /// The provider's nonce challenge (RFC 9449 §8) gets one more try with its nonce.
    #[tokio::test]
    async fn a_token_request_with_a_proof_answers_the_nonce_challenge() {
      let key = DpopSigningKey::generate();
      let thumbprint = key.thumbprint();
      let (url, seen) = fake_provider(vec![
        (
          "400 Bad Request",
          Some(NONCE),
          r#"{"error":"use_dpop_nonce"}"#.to_owned(),
        ),
        (
          "200 OK",
          None,
          format!(
            r#"{{"access_token":"{}","refresh_token":"r","expires_in":300}}"#,
            token_bound_to(&thumbprint)
          ),
        ),
      ])
      .await;
      let prover = prover_for(key);
      let answer = post_token(
        &http_client().unwrap(),
        &url,
        &[("grant_type", "refresh_token")],
        Some((&prover, ProofFallback::None)),
      )
      .await
      .unwrap();
      assert_eq!(
        bound_key_of(&answer.access_token, Some(&thumbprint)).unwrap(),
        Some(thumbprint)
      );
      let proofs = seen.lock().unwrap().clone();
      assert_eq!(proofs.len(), 2);
      let first = claims(proofs[0].as_deref().unwrap());
      let second = claims(proofs[1].as_deref().unwrap());
      assert!(first.get("nonce").is_none());
      assert_eq!(second["nonce"], NONCE);
      assert_eq!(second["htm"], "POST");
      assert_eq!(second["htu"], url.as_str());
      assert!(second.get("ath").is_none());
    }

    #[tokio::test]
    async fn an_unbound_request_carries_no_proof() {
      let (url, seen) = fake_provider(vec![("200 OK", None, r#"{"access_token":"a.b.c"}"#.to_owned())]).await;
      post_token(&http_client().unwrap(), &url, &[], None)
        .await
        .unwrap();
      assert_eq!(seen.lock().unwrap().as_slice(), &[None]);
    }

    #[test]
    fn an_ended_sign_in_is_told_apart_from_other_refusals() {
      assert!(matches!(
        token_answer(400, br#"{"error":"invalid_grant"}"#),
        Err(AuthError::Expired)
      ));
      assert!(matches!(
        token_answer(400, br#"{"error":"invalid_dpop_proof"}"#),
        Err(AuthError::Provider(_))
      ));
      assert!(matches!(
        token_answer(502, b"<html>"),
        Err(AuthError::Provider(_))
      ));
    }
  }

  /// A provider that gives the `answers` in turn and keeps each request's `DPoP` header.
  async fn fake_provider(
    answers: Vec<(&'static str, Option<&'static str>, String)>,
  ) -> (String, Arc<Mutex<Vec<Option<String>>>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let url = format!("http://{}/token", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    tokio::spawn(async move {
      for (status, maybe_nonce, body) in answers {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        let maybe_proof = request
          .lines()
          .find_map(|line| {
            line
              .strip_prefix("dpop: ")
              .or_else(|| line.strip_prefix("DPoP: "))
          })
          .map(str::to_owned);
        recorded.lock().unwrap().push(maybe_proof);
        let nonce = maybe_nonce.map_or_else(String::new, |nonce| format!("DPoP-Nonce: {nonce}\r\n"));
        let response = format!(
          "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{nonce}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
          body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
      }
    });
    (url, seen)
  }

  /// Reads the head and as much of the body as `Content-Length` says.
  async fn read_request(stream: &mut tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
      let read = stream.read(&mut buffer).await.unwrap();
      bytes.extend_from_slice(&buffer[..read]);
      let text = String::from_utf8_lossy(&bytes).into_owned();
      if let Some((head, body)) = text.split_once("\r\n\r\n") {
        let length = head
          .lines()
          .find_map(|line| {
            line
              .to_ascii_lowercase()
              .strip_prefix("content-length: ")
              .map(str::to_owned)
          })
          .and_then(|value| value.trim().parse::<usize>().ok())
          .unwrap_or(0);
        if body.len() >= length {
          return head.to_owned();
        }
      }
      if read == 0 {
        return text;
      }
    }
  }

  fn claims(proof: &str) -> serde_json::Value {
    let payload = proof.split('.').nth(1).unwrap();
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap()
  }

  fn token_bound_to(key: &JwkThumbprint) -> String {
    let claims = format!(r#"{{"cnf":{{"jkt":"{}"}}}}"#, key.as_str());
    format!(
      "{}.{}.signature",
      URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256"}"#),
      URL_SAFE_NO_PAD.encode(claims)
    )
  }

  fn discovery(algorithms: &[&str]) -> Discovery {
    Discovery {
      issuer: "https://id.example.com".into(),
      authorization_endpoint: "https://id.example.com/auth".into(),
      token_endpoint: "https://id.example.com/token".into(),
      dpop_signing_alg_values_supported: algorithms.iter().map(|&algorithm| algorithm.to_owned()).collect(),
    }
  }

  fn stored_sign_in(maybe_bound_key: Option<String>) -> StoredSignIn {
    StoredSignIn {
      server_url: "https://chat.example.com".into(),
      client_id: "teamcraft".into(),
      token_url: "https://id.example.com/token".into(),
      refresh_token: "refresh".into(),
      maybe_bound_key,
    }
  }

  fn temporary_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("teamcraft-import-auth-{name}-{}", std::process::id()));
    drop(std::fs::remove_dir_all(&dir));
    crate::files::make_state_dir(&dir).unwrap();
    dir
  }
}
