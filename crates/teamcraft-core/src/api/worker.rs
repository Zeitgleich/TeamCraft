//! The service worker's few API requests over the browser's `fetch` (issue #1909): the message a push
//! names, a planned meeting's chat, the read position, a meeting notice dealt with, and a rotated
//! Web Push subscription. [`HttpServerApi`](super::HttpServerApi) would bring reqwest, `http`'s
//! header table and `url` into the worker, which every visitor of the web app downloads, a meeting's
//! guest too (.claude/rules/robustness.md, "Download size").
//!
//! The requests follow the same rules as the app's (e2ee.md §3.1.2): a token bound to a key goes
//! with a proof and only from this browser's key, the answer's nonce and clock are kept, and a proof
//! refused for them is made again once, never more. Nothing follows a redirect (the API sends none,
//! and a proof must not reach another host), and tokens only go to `https://` or a loopback
//! `http://`. The proof flow is written against [`WorkerTransport`], so it is tested natively
//! against the server's own proof checks.

use std::future::Future;
use std::net::IpAddr;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use teamcraft_proto::{
  ChangeWebPushSubscriptionRequest, ConversationId, ConversationResponse, DismissMeetingNoticeRequest,
  ErrorBody, ListMessagesQuery, ListMessagesResponse, MeetingId, SetReadPositionRequest,
};

use super::dpop::{DPOP_HEADER, DpopAnswer, DpopProver, Learned, is_from_a_cache, signed_headers};
use super::{ApiError, ProvenToken};
use crate::push_preview::PreviewApi;

/// The API as the service worker uses it, with the stored push session's token.
pub struct WorkerApi<T> {
  base_url: String,
  token: String,
  maybe_prover: Option<Arc<DpopProver>>,
  transport: T,
}

/// Sends one request and hands back what the proof flow reads of the answer.
pub trait WorkerTransport {
  fn send(&self, request: WorkerRequest) -> impl Future<Output = Result<WorkerAnswer, ApiError>>;
}

/// One request as it goes out: `headers` are set as they are, `maybe_body` is JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerRequest {
  pub method: &'static str,
  pub url: String,
  pub headers: Vec<(&'static str, String)>,
  pub maybe_body: Option<String>,
}

/// What an answer says: its status, the headers the proof flow and the error read, and the body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkerAnswer {
  pub status: u16,
  pub maybe_nonce: Option<String>,
  pub maybe_date: Option<String>,
  pub maybe_age: Option<String>,
  pub maybe_retry_after: Option<String>,
  pub body: Vec<u8>,
}

impl<T: WorkerTransport> WorkerApi<T> {
  pub fn new(server_url: &str, token: ProvenToken, transport: T) -> Self {
    Self {
      base_url: server_url.trim_end_matches('/').to_owned(),
      token: token.token,
      maybe_prover: token.maybe_prover,
      transport,
    }
  }

  pub async fn conversation(&self, conversation: ConversationId) -> Result<ConversationResponse, ApiError> {
    self.get(&format!("/conversations/{conversation}")).await
  }

  pub async fn list_messages(
    &self,
    conversation: ConversationId,
    query: &ListMessagesQuery,
  ) -> Result<ListMessagesResponse, ApiError> {
    let parameters: Vec<String> = [
      ("before", query.maybe_before.map(|id| id.to_string())),
      ("after", query.maybe_after.map(|id| id.to_string())),
      ("limit", query.maybe_limit.map(|limit| limit.to_string())),
    ]
    .into_iter()
    .filter_map(|(name, maybe_value)| maybe_value.map(|value| format!("{name}={value}")))
    .collect();
    let query = if parameters.is_empty() {
      String::new()
    } else {
      format!("?{}", parameters.join("&"))
    };
    self
      .get(&format!("/conversations/{conversation}/messages{query}"))
      .await
  }

  pub async fn set_read_position(
    &self,
    conversation: ConversationId,
    request: &SetReadPositionRequest,
  ) -> Result<(), ApiError> {
    let path = format!("/conversations/{conversation}/read-position");
    self.call("PUT", &path, Some(json(request)?)).await.map(drop)
  }

  pub async fn dismiss_meeting_notice(
    &self,
    meeting: MeetingId,
    request: &DismissMeetingNoticeRequest,
  ) -> Result<(), ApiError> {
    let path = format!("/meetings/{meeting}/notices/dismiss");
    self.call("POST", &path, Some(json(request)?)).await.map(drop)
  }

  async fn get<R: DeserializeOwned>(&self, path: &str) -> Result<R, ApiError> {
    let body = self.call("GET", path, None).await?;
    serde_json::from_slice(&body).map_err(|error| ApiError::Decode(error.to_string()))
  }

  /// Sends `/api/v1{path}` with the token, and once more with a new proof when the answer gave what
  /// a refused proof lacked.
  async fn call(
    &self,
    method: &'static str,
    path: &str,
    maybe_body: Option<String>,
  ) -> Result<Vec<u8>, ApiError> {
    if let Some(reason) = insecure_reason(&self.base_url) {
      return Err(ApiError::InsecureServerUrl(reason));
    }
    let url = format!("{}/api/v1{path}", self.base_url);
    let answer = self.send_signed(method, &url, maybe_body.clone()).await?;
    let learned = self.note(&answer);
    let error = match answer_body(answer) {
      Ok(body) => return Ok(body),
      Err(error) => error,
    };
    let is_retried = matches!(error, ApiError::Status { status: 401, .. })
      && error
        .error_code()
        .is_some_and(|code| learned.is_worth_a_retry(code));
    if !is_retried {
      return Err(error);
    }
    let answer = self.send_signed(method, &url, maybe_body).await?;
    self.note(&answer);
    answer_body(answer)
  }

  async fn send_signed(
    &self,
    method: &'static str,
    url: &str,
    maybe_body: Option<String>,
  ) -> Result<WorkerAnswer, ApiError> {
    let signed = signed_headers(self.maybe_prover.as_deref(), &self.token, method, url)
      .await
      .map_err(ApiError::from)?;
    let mut headers = vec![("Authorization", signed.authorization)];
    if let Some(proof) = signed.maybe_proof {
      headers.push((DPOP_HEADER, proof.as_str().to_owned()));
    }
    if maybe_body.is_some() {
      headers.push(("Content-Type", "application/json".to_owned()));
    }
    let request = WorkerRequest {
      method,
      url: url.to_owned(),
      headers,
      maybe_body,
    };
    self.transport.send(request).await
  }

  /// Takes in what the answer says for the next proof; nothing without a prover (a bearer token).
  fn note(&self, answer: &WorkerAnswer) -> Learned {
    let Some(prover) = &self.maybe_prover else {
      return Learned::default();
    };
    let is_cached = is_from_a_cache(answer.maybe_age.as_deref());
    prover.note_answer(DpopAnswer {
      maybe_nonce: answer.maybe_nonce.as_deref(),
      maybe_date: answer.maybe_date.as_deref().filter(|_| !is_cached),
    })
  }
}

impl<T: WorkerTransport> PreviewApi for WorkerApi<T> {
  async fn conversation(&self, conversation: ConversationId) -> Result<ConversationResponse, ApiError> {
    WorkerApi::conversation(self, conversation).await
  }

  async fn list_messages(
    &self,
    conversation: ConversationId,
    query: &ListMessagesQuery,
  ) -> Result<ListMessagesResponse, ApiError> {
    WorkerApi::list_messages(self, conversation, query).await
  }
}

/// Sends the service worker's proof for a rotated Web Push subscription (e2ee.md §7). No access
/// token: the request authenticates itself (`ChangeWebPushSubscriptionRequest::mac`).
pub async fn change_web_push_subscription(
  transport: &impl WorkerTransport,
  server_url: &str,
  request: &ChangeWebPushSubscriptionRequest,
) -> Result<(), ApiError> {
  let base_url = server_url.trim_end_matches('/');
  if let Some(reason) = insecure_reason(base_url) {
    return Err(ApiError::InsecureServerUrl(reason));
  }
  let request = WorkerRequest {
    method: "POST",
    url: format!("{base_url}/api/v1/push/web/subscription-change"),
    headers: vec![("Content-Type", "application/json".to_owned())],
    maybe_body: Some(json(request)?),
  };
  answer_body(transport.send(request).await?).map(drop)
}

fn json(body: &impl Serialize) -> Result<String, ApiError> {
  serde_json::to_string(body).map_err(|error| ApiError::Decode(error.to_string()))
}

/// The body of a successful answer, or the error the server named.
fn answer_body(answer: WorkerAnswer) -> Result<Vec<u8>, ApiError> {
  if (200..300).contains(&answer.status) {
    return Ok(answer.body);
  }
  let maybe_retry_after = answer
    .maybe_retry_after
    .as_deref()
    .and_then(super::parse_retry_after);
  let maybe_body = serde_json::from_slice::<ErrorBody>(&answer.body)
    .ok()
    .map(|body| match (body.maybe_retry_after_secs, maybe_retry_after) {
      (None, Some(wait)) => body.with_retry_after_secs(wait.as_secs()),
      _ => body,
    });
  Err(ApiError::Status {
    status: answer.status,
    maybe_body,
    maybe_retry_after,
  })
}

/// `None` if `base_url` may carry tokens: `https://`, or `http://` to a loopback address. The same
/// rule as the app's, without the `url` crate: anything this doesn't read plainly is refused.
fn insecure_reason(base_url: &str) -> Option<String> {
  let refused = || Some(format!("{base_url:?} may not carry tokens"));
  let Some((scheme, rest)) = base_url.split_once("://") else {
    return refused();
  };
  let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
  if authority.is_empty() || authority.contains('@') {
    return refused();
  }
  if scheme.eq_ignore_ascii_case("https") {
    return None;
  }
  let host = match authority.strip_prefix('[') {
    Some(bracketed) => bracketed.split(']').next().unwrap_or(""),
    None => authority.split(':').next().unwrap_or(""),
  };
  let is_loopback =
    host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
  if scheme.eq_ignore_ascii_case("http") && is_loopback {
    return None;
  }
  refused()
}

/// The browser's `fetch` in the service worker's scope.
#[cfg(all(target_arch = "wasm32", feature = "service-worker"))]
#[derive(Debug, Clone, Copy, Default)]
pub struct FetchTransport;

#[cfg(all(target_arch = "wasm32", feature = "service-worker"))]
impl WorkerTransport for FetchTransport {
  async fn send(&self, request: WorkerRequest) -> Result<WorkerAnswer, ApiError> {
    use js_sys::Uint8Array;
    use wasm_bindgen::{JsCast, JsValue};
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{Headers, Request, RequestInit, RequestRedirect, Response, WorkerGlobalScope};

    let failed = |error: JsValue| ApiError::Network(crate::platform::describe_js_error(&error));
    let headers = Headers::new().map_err(failed)?;
    for (name, value) in &request.headers {
      headers.set(name, value).map_err(failed)?;
    }
    let init = RequestInit::new();
    init.set_method(request.method);
    init.set_headers(&headers);
    // A redirect fails the request: the API sends none, and a proof must not reach another host.
    init.set_redirect(RequestRedirect::Error);
    if let Some(body) = &request.maybe_body {
      init.set_body(&JsValue::from_str(body));
    }
    let outgoing = Request::new_with_str_and_init(&request.url, &init).map_err(failed)?;
    let scope: WorkerGlobalScope = js_sys::global().unchecked_into();
    let response: Response = JsFuture::from(scope.fetch_with_request(&outgoing))
      .await
      .map_err(failed)?
      .unchecked_into();
    let answer_headers = response.headers();
    let header = |name: &str| answer_headers.get(name).ok().flatten();
    let body = JsFuture::from(response.array_buffer().map_err(failed)?)
      .await
      .map_err(failed)?;
    Ok(WorkerAnswer {
      status: response.status(),
      maybe_nonce: header(super::dpop::DPOP_NONCE_HEADER),
      maybe_date: header("Date"),
      maybe_age: header("Age"),
      maybe_retry_after: header("Retry-After"),
      body: Uint8Array::new(&body).to_vec(),
    })
  }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
  use std::cell::RefCell;
  use std::time::{SystemTime, UNIX_EPOCH};

  use base64::Engine as _;
  use base64::engine::general_purpose::URL_SAFE_NO_PAD;
  use teamcraft_crypto::dpop::{
    DpopExpectation, DpopNonces, DpopSigningKey, JwkThumbprint, PublicOrigin, verify_proof,
  };
  use teamcraft_proto::{Base64Bytes, DeviceId, ErrorCode, Id, MessageId, Snowflake};

  use super::*;
  use crate::api::dpop::{Authorization, SoftwareDpopSigner};

  const SERVER: &str = "https://chat.example.com";
  const NONCE_ROTATION_SECS: u64 = 300;

  mod proofs {
    use super::*;

    #[tokio::test]
    async fn a_nonce_challenge_gets_exactly_one_more_try() {
      let (api, server) = bound_api(Challenge::First);

      api
        .set_read_position(conversation(), &read_position())
        .await
        .unwrap();

      let sent = server.sent();
      assert_eq!(sent.len(), 2);
      assert_eq!(server.accepted(), 1);
      assert!(header(&sent[1], "Authorization").unwrap().starts_with("DPoP "));
      assert_eq!(sent[1].maybe_body.as_deref(), Some(r#"{"message_id":"7"}"#));
    }

    #[tokio::test]
    async fn a_challenge_that_never_ends_is_no_loop() {
      let (api, server) = bound_api(Challenge::Always);

      let error = api.conversation(conversation()).await.unwrap_err();

      assert_eq!(server.sent().len(), 2);
      assert_eq!(error.error_code(), Some(&ErrorCode::UseDpopNonce));
    }

    #[tokio::test]
    async fn a_refused_proof_without_news_is_not_tried_again() {
      let (api, server) = bound_api(Challenge::RefuseProofs);

      let error = api.conversation(conversation()).await.unwrap_err();

      assert_eq!(server.sent().len(), 1);
      assert_eq!(error.error_code(), Some(&ErrorCode::InvalidDpopProof));
    }

    #[tokio::test]
    async fn the_nonce_is_kept_for_the_next_request() {
      let (api, server) = bound_api(Challenge::First);

      api
        .set_read_position(conversation(), &read_position())
        .await
        .unwrap();
      api
        .set_read_position(conversation(), &read_position())
        .await
        .unwrap();

      assert_eq!(server.sent().len(), 3);
      assert_eq!(server.accepted(), 2);
    }

    #[tokio::test]
    async fn a_token_bound_to_another_key_never_goes() {
      let server = FakeServer::new(Challenge::Never);
      let other = DpopSigningKey::generate().thumbprint();
      let api = WorkerApi::new(SERVER, token(&other, Some(prover())), &server);

      let error = api.conversation(conversation()).await.unwrap_err();

      assert!(matches!(error, ApiError::BoundKeyLost), "{error}");
      assert!(server.sent().is_empty());
    }

    #[tokio::test]
    async fn a_bound_token_without_the_key_never_goes() {
      let server = FakeServer::new(Challenge::Never);
      let key = DpopSigningKey::generate().thumbprint();
      let api = WorkerApi::new(SERVER, token(&key, None), &server);

      assert!(api.conversation(conversation()).await.is_err());
      assert!(server.sent().is_empty());
    }

    #[tokio::test]
    async fn a_bearer_token_goes_without_a_proof() {
      let server = FakeServer::new(Challenge::Never);
      let api = WorkerApi::new(
        SERVER,
        ProvenToken {
          token: "opaque".into(),
          maybe_prover: None,
        },
        &server,
      );

      api
        .set_read_position(conversation(), &read_position())
        .await
        .unwrap();

      let sent = server.sent();
      assert_eq!(header(&sent[0], "Authorization"), Some("Bearer opaque"));
      assert_eq!(header(&sent[0], DPOP_HEADER), None);
    }
  }

  mod requests {
    use super::*;

    #[tokio::test]
    async fn messages_are_listed_with_the_query() {
      let (api, server) = bound_api(Challenge::Never);
      let query = ListMessagesQuery {
        maybe_before: Some(MessageId::from_snowflake(snowflake(8))),
        maybe_after: None,
        maybe_limit: Some(1),
      };

      let _ = api.list_messages(conversation(), &query).await;

      assert_eq!(
        server.sent()[0].url,
        format!("{SERVER}/api/v1/conversations/3/messages?before=8&limit=1")
      );
      assert_eq!(server.accepted(), 1, "the proof leaves the query out");
    }

    #[tokio::test]
    async fn tokens_go_only_over_tls_or_to_loopback() {
      let server = FakeServer::new(Challenge::Never);
      let api = WorkerApi::new(
        "http://chat.example.com",
        ProvenToken {
          token: "opaque".into(),
          maybe_prover: None,
        },
        &server,
      );

      let error = api.conversation(conversation()).await.unwrap_err();

      assert!(matches!(error, ApiError::InsecureServerUrl(_)), "{error}");
      assert!(server.sent().is_empty());
    }

    #[tokio::test]
    async fn an_error_answer_keeps_its_body_and_wait() {
      let server = FakeServer::new(Challenge::Never);
      server.answer_next(WorkerAnswer {
        status: 409,
        maybe_retry_after: Some("30".into()),
        body: br#"{"error_code":"conflict","message":"gone"}"#.to_vec(),
        ..WorkerAnswer::default()
      });
      let request = ChangeWebPushSubscriptionRequest {
        device_id: DeviceId::from_snowflake(snowflake(5)),
        previous_token: "old".into(),
        token: "new".into(),
        mac: Base64Bytes::new(vec![1; 32]),
      };

      let error = change_web_push_subscription(&&server, SERVER, &request)
        .await
        .unwrap_err();

      let ApiError::Status {
        status,
        maybe_body: Some(body),
        maybe_retry_after,
      } = error
      else {
        panic!("{error}");
      };
      assert_eq!(status, 409);
      assert_eq!(body.maybe_retry_after_secs, Some(30));
      assert_eq!(maybe_retry_after, Some(std::time::Duration::from_secs(30)));
      let sent = server.sent();
      assert_eq!(
        sent[0].url,
        format!("{SERVER}/api/v1/push/web/subscription-change")
      );
      assert_eq!(header(&sent[0], "Authorization"), None);
    }

    #[test]
    fn the_transport_rule_matches_the_apps() {
      for allowed in [
        "https://chat.example.com",
        "HTTPS://chat.example.com:8443/base",
        "http://localhost:8081",
        "http://127.0.0.1",
        "http://[::1]:8081",
      ] {
        assert_eq!(insecure_reason(allowed), None, "{allowed}");
        assert_eq!(
          super::super::super::http::insecure_reason(allowed),
          None,
          "{allowed}"
        );
      }
      for refused in [
        "http://chat.example.com",
        "http://localhost.example.com",
        "http://127.0.0.1.example.com",
        "ftp://localhost",
        "chat.example.com",
        "https://",
      ] {
        assert!(insecure_reason(refused).is_some(), "{refused}");
      }
      assert!(insecure_reason("http://user@localhost").is_some());
    }
  }

  /// When the fake server refuses a proof.
  #[derive(Clone, Copy, PartialEq, Eq)]
  enum Challenge {
    /// Every proof without its current nonce, so the first one.
    First,
    /// Every proof, each time with a new nonce.
    Always,
    /// Every proof, as one with a wrong date, without a `Date`.
    RefuseProofs,
    /// No proof; bearer tokens go too.
    Never,
  }

  /// The server's side of a request with a proof, as `teamcraft-server` checks it.
  struct FakeServer {
    challenge: Challenge,
    nonces: RefCell<DpopNonces>,
    sent: RefCell<Vec<WorkerRequest>>,
    accepted: RefCell<usize>,
    maybe_next: RefCell<Option<WorkerAnswer>>,
  }

  impl FakeServer {
    fn new(challenge: Challenge) -> Self {
      Self {
        challenge,
        nonces: RefCell::new(DpopNonces::new(now_unix())),
        sent: RefCell::default(),
        accepted: RefCell::default(),
        maybe_next: RefCell::default(),
      }
    }

    fn sent(&self) -> Vec<WorkerRequest> {
      self.sent.borrow().clone()
    }

    fn accepted(&self) -> usize {
      *self.accepted.borrow()
    }

    fn answer_next(&self, answer: WorkerAnswer) {
      *self.maybe_next.borrow_mut() = Some(answer);
    }

    fn answer(&self, request: &WorkerRequest) -> WorkerAnswer {
      if let Some(answer) = self.maybe_next.borrow_mut().take() {
        return answer;
      }
      let Some(proof) = header(request, DPOP_HEADER) else {
        return ok();
      };
      let authorization = header(request, "Authorization").unwrap_or("");
      let token = authorization
        .strip_prefix("DPoP ")
        .expect("a proof with the DPoP scheme");
      let path = request.url.strip_prefix(SERVER).unwrap();
      let path = path.split('?').next().unwrap();
      let origins = [PublicOrigin::parse(SERVER).unwrap()];
      let bound = match Authorization::of(token) {
        Authorization::Bound(key) => key,
        Authorization::Bearer => panic!("a proof for a bearer token"),
      };
      let verified = verify_proof(
        proof,
        &DpopExpectation {
          method: request.method,
          path,
          public_origins: &origins,
          now_unix: now_unix(),
          max_age_secs: 60,
          leeway_secs: 60,
          access_token: token,
          maybe_bound_key: Some(&bound),
        },
      )
      .expect("a proof the server accepts");
      if self.challenge == Challenge::RefuseProofs {
        return refused(ErrorCode::InvalidDpopProof, None);
      }
      let mut nonces = self.nonces.borrow_mut();
      let is_current = verified
        .maybe_nonce
        .as_deref()
        .is_some_and(|nonce| nonces.accepts(nonce, now_unix(), NONCE_ROTATION_SECS));
      if self.challenge == Challenge::Always {
        *nonces = DpopNonces::new(now_unix());
      }
      if self.challenge != Challenge::Never && (!is_current || self.challenge == Challenge::Always) {
        let nonce = nonces.current(now_unix(), NONCE_ROTATION_SECS).to_owned();
        return refused(ErrorCode::UseDpopNonce, Some(nonce));
      }
      *self.accepted.borrow_mut() += 1;
      ok()
    }
  }

  impl WorkerTransport for &FakeServer {
    async fn send(&self, request: WorkerRequest) -> Result<WorkerAnswer, ApiError> {
      let answer = self.answer(&request);
      self.sent.borrow_mut().push(request);
      Ok(answer)
    }
  }

  fn bound_api(challenge: Challenge) -> (WorkerApi<&'static FakeServer>, &'static FakeServer) {
    let server: &'static FakeServer = Box::leak(Box::new(FakeServer::new(challenge)));
    let prover = prover();
    let api = WorkerApi::new(SERVER, token(&prover.thumbprint(), Some(prover)), server);
    (api, server)
  }

  fn prover() -> Arc<DpopProver> {
    Arc::new(DpopProver::new(Arc::new(SoftwareDpopSigner::new(
      DpopSigningKey::generate(),
    ))))
  }

  fn token(key: &JwkThumbprint, maybe_prover: Option<Arc<DpopProver>>) -> ProvenToken {
    let claims = format!(r#"{{"cnf":{{"jkt":"{}"}}}}"#, key.as_str());
    ProvenToken {
      token: format!("e30.{}.c2ln", URL_SAFE_NO_PAD.encode(claims)),
      maybe_prover,
    }
  }

  fn ok() -> WorkerAnswer {
    WorkerAnswer {
      status: 200,
      body: b"{}".to_vec(),
      ..WorkerAnswer::default()
    }
  }

  fn refused(code: ErrorCode, maybe_nonce: Option<String>) -> WorkerAnswer {
    let body = serde_json::to_vec(&ErrorBody::new(code, "refused")).unwrap();
    WorkerAnswer {
      status: 401,
      maybe_nonce,
      body,
      ..WorkerAnswer::default()
    }
  }

  fn header<'a>(request: &'a WorkerRequest, name: &str) -> Option<&'a str> {
    request
      .headers
      .iter()
      .find(|(key, _)| key.eq_ignore_ascii_case(name))
      .map(|(_, value)| value.as_str())
  }

  fn conversation() -> ConversationId {
    ConversationId::from_snowflake(snowflake(3))
  }

  fn read_position() -> SetReadPositionRequest {
    SetReadPositionRequest {
      message_id: MessageId::from_snowflake(snowflake(7)),
    }
  }

  fn snowflake(value: i64) -> Snowflake {
    Snowflake::new(value).unwrap()
  }

  fn now_unix() -> i64 {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    i64::try_from(elapsed.as_secs()).unwrap()
  }
}
