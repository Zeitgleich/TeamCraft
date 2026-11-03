//! The app's own HTTP requests through the core's network stack (issue #433): the identity
//! provider (AppAuth's discovery, code exchange and token refresh), a local account's sign-in, the
//! server check, brand images and the video codec download. Android's `HttpURLConnection` tries a
//! host's addresses one after another and waits out a 15 s connect timeout on a black-holed IPv6
//! route; the core races IPv6 and IPv4 (#420) and uses the proxy and CAs the app handed over
//! ([`crate::set_network_proxy`], [`crate::set_trusted_certificates`], #157).
//!
//! The requests run on a small runtime of their own: they are needed before the engine exists
//! (server check, sign-in) and while it waits for a token (refresh).

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::task::{Context, Poll};
use std::time::Duration;

use teamcraft_core::api::fetch::{
  self, FetchError, FetchMethod, FetchRequest, FetchResponse, PlainHttp, Redirects,
};
use tokio::runtime::Runtime;
use tokio::task::{JoinError, JoinHandle};

/// The runtime's worker threads: requests are few and short, and wait on the network.
const WORKER_THREADS: usize = 2;

/// Sends `request` and reads the answer, whatever its status (the app decides what a 4xx means).
/// Cancelling the call (the coroutine, the task) cancels the request.
#[uniffi::export]
pub async fn http_fetch(request: HttpFetchRequest) -> Result<HttpFetchResponse, HttpFetchError> {
  let runtime = runtime().ok_or_else(|| HttpFetchError::Network("no runtime for HTTP requests".into()))?;
  let task = AbortOnDrop(runtime.spawn(fetch::fetch(request.into())));
  match task.await {
    Ok(answer) => answer.map(HttpFetchResponse::from).map_err(HttpFetchError::from),
    Err(ended) => Err(HttpFetchError::Network(format!("the request ended: {ended}"))),
  }
}

/// One request of the app. No `Debug`: the body may hold a password, a header a token.
#[derive(Clone, PartialEq, Eq, uniffi::Record)]
pub struct HttpFetchRequest {
  pub method: HttpFetchMethod,
  pub url: String,
  /// `Content-Length`, `Transfer-Encoding`, `Connection` and `Host` are left to the transport.
  pub headers: Vec<HttpFetchHeader>,
  /// Sent with `Post`, also when empty; empty with `Get`.
  pub body: Vec<u8>,
  pub redirects: HttpFetchRedirects,
  /// Plain `http` to other hosts than this device: development builds, and the video codec (Cisco
  /// offers it over `http` only; the core checks it against Cisco's published hash). `https` and
  /// `http` to a loopback address are always allowed.
  pub allow_insecure_http: bool,
  /// How long connecting (TCP over both address families, TLS) may take.
  pub connect_timeout: Duration,
  /// How long the server may stay silent: until the answer starts (counted from the start of the
  /// request), then between two parts of the body.
  pub read_timeout: Duration,
  /// A longer body fails as `TooLarge`.
  pub max_body_bytes: u64,
}

#[derive(Clone, PartialEq, Eq, uniffi::Record)]
pub struct HttpFetchHeader {
  pub name: String,
  pub value: String,
}

/// An answer as received. No `Debug`: the body may hold tokens.
#[derive(Clone, PartialEq, Eq, uniffi::Record)]
pub struct HttpFetchResponse {
  pub status: u16,
  /// Names in lower case, in the order received.
  pub headers: Vec<HttpFetchHeader>,
  pub body: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum HttpFetchMethod {
  Get,
  Post,
}

/// What happens to a redirect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum HttpFetchRedirects {
  /// Followed (at most ten hops, never from `https` to `http`).
  Follow,
  /// The redirect is the answer: a captive portal sends one, and credentials must not go to
  /// another host.
  DontFollow,
}

/// Why a request got no answer. Flat: the app turns each into the `IOException` its callers know
/// from the platform's client; the message is for logs.
#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum HttpFetchError {
  /// A malformed URL or header, a scheme other than `http(s)`, a body with `Get`, a zero timeout.
  #[error("invalid request: {0}")]
  InvalidRequest(String),
  /// Plain `http` to another host than this device without `allow_insecure_http`, also as a
  /// redirect.
  #[error("plain http is not allowed to {0}")]
  PlainHttpRefused(String),
  /// Most often a TLS-inspecting proxy with a CA the device doesn't trust.
  #[error("untrusted certificate: {0}")]
  UntrustedCertificate(String),
  #[error("timed out: {0}")]
  TimedOut(String),
  #[error("too large: {0}")]
  TooLarge(String),
  /// No connection, a reset, a broken answer, a redirect loop.
  #[error("network: {0}")]
  Network(String),
}

/// The request's task, aborted when the caller stops waiting for it.
struct AbortOnDrop<T>(JoinHandle<T>);

/// Built on first use; `None` if the threads can't be started.
fn runtime() -> Option<&'static Runtime> {
  static RUNTIME: OnceLock<Option<Runtime>> = OnceLock::new();
  RUNTIME
    .get_or_init(|| {
      let built = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .thread_name("teamcraft-http")
        .enable_all()
        .build();
      match built {
        Ok(runtime) => Some(runtime),
        Err(error) => {
          tracing::error!(%error, "no runtime for HTTP requests");
          None
        },
      }
    })
    .as_ref()
}

impl<T> Future for AbortOnDrop<T> {
  type Output = Result<T, JoinError>;

  fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
    Pin::new(&mut self.0).poll(context)
  }
}

impl<T> Drop for AbortOnDrop<T> {
  fn drop(&mut self) {
    self.0.abort();
  }
}

impl From<HttpFetchRequest> for FetchRequest {
  fn from(request: HttpFetchRequest) -> Self {
    Self {
      method: match request.method {
        HttpFetchMethod::Get => FetchMethod::Get,
        HttpFetchMethod::Post => FetchMethod::Post,
      },
      url: request.url,
      headers: request
        .headers
        .into_iter()
        .map(|header| (header.name, header.value))
        .collect(),
      body: request.body,
      redirects: match request.redirects {
        HttpFetchRedirects::Follow => Redirects::Follow,
        HttpFetchRedirects::DontFollow => Redirects::DontFollow,
      },
      plain_http: if request.allow_insecure_http {
        PlainHttp::Allowed
      } else {
        PlainHttp::LoopbackOnly
      },
      connect_timeout: request.connect_timeout,
      read_timeout: request.read_timeout,
      max_body_bytes: request.max_body_bytes,
    }
  }
}

impl From<FetchResponse> for HttpFetchResponse {
  fn from(response: FetchResponse) -> Self {
    Self {
      status: response.status,
      headers: response
        .headers
        .into_iter()
        .map(|(name, value)| HttpFetchHeader { name, value })
        .collect(),
      body: response.body,
    }
  }
}

impl From<FetchError> for HttpFetchError {
  fn from(error: FetchError) -> Self {
    match error {
      FetchError::InvalidRequest(detail) => Self::InvalidRequest(detail),
      FetchError::PlainHttpRefused(host) => Self::PlainHttpRefused(host),
      FetchError::UntrustedCertificate(detail) => Self::UntrustedCertificate(detail),
      FetchError::TimedOut(detail) => Self::TimedOut(detail),
      too_large @ FetchError::TooLarge { .. } => Self::TooLarge(too_large.to_string()),
      FetchError::Network(detail) => Self::Network(detail),
    }
  }
}

#[cfg(test)]
mod tests {
  use std::net::{Ipv4Addr, SocketAddr};
  use std::time::Instant;

  use teamcraft_core::api::black_hole::BlackHole;
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;
  use tokio::sync::oneshot;

  use super::*;

  const ANSWER: &str =
    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\n\r\n{\"ok\":true}";

  /// The 1.0.2 failure (issue #420) for the app's own requests (#433), end to end through the
  /// entry point the app calls: `localhost` resolves to `::1` first, which swallows the SYN, and
  /// to `127.0.0.1`, which answers. The answer comes over IPv4 well within a second.
  #[tokio::test]
  async fn a_request_falls_back_to_ipv4_within_a_second_when_ipv6_is_black_holed() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(answer_every_request(listener));
    let Some(_black_hole) = BlackHole::ipv6(port).await else {
      return;
    };
    // Only meaningful where the system's resolver puts IPv6 first, as phones do.
    let Ok(addresses) = tokio::net::lookup_host(("localhost", port)).await else {
      return;
    };
    let addresses: Vec<SocketAddr> = addresses.collect();
    if !addresses.first().is_some_and(SocketAddr::is_ipv6) || !addresses.iter().any(SocketAddr::is_ipv4) {
      return;
    }

    let started = Instant::now();
    let answer = tokio::time::timeout(
      Duration::from_secs(5),
      http_fetch(get(&format!("http://localhost:{port}/api/v1/server-info"))),
    )
    .await
    .expect("fell back to IPv4 within the timeout")
    .unwrap();

    assert_eq!(answer.status, 200);
    assert_eq!(answer.body, br#"{"ok":true}"#);
    assert!(answer.headers.contains(&HttpFetchHeader {
      name: "content-type".into(),
      value: "application/json".into(),
    }));
    let elapsed = started.elapsed();
    assert!(
      elapsed >= Duration::from_millis(250),
      "IPv6 wasn't tried first: {elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
  }

  /// A cancelled coroutine (the user left the screen, AppAuth gave up) ends the request instead of
  /// leaving it waiting on the server.
  #[tokio::test]
  async fn dropping_the_call_closes_the_connection() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let (accepted, is_accepted) = oneshot::channel();
    let server = tokio::spawn(async move {
      let (mut socket, _) = listener.accept().await.unwrap();
      let mut request = [0u8; 4096];
      let _ = socket.read(&mut request).await.unwrap();
      accepted.send(()).unwrap();
      // Never answers; the next read sees the client go away.
      let mut rest = [0u8; 16];
      tokio::time::timeout(Duration::from_secs(5), socket.read(&mut rest)).await
    });
    let call = http_fetch(get(&format!("http://{address}/slow")));
    tokio::select! {
      _ = call => panic!("the server never answers"),
      _ = is_accepted => {},
    }
    // `call` is dropped here, with the request in flight.
    let closed = server.await.unwrap().expect("the connection was closed");
    // End of stream, or a reset.
    assert!(!matches!(closed, Ok(read) if read > 0), "{closed:?}");
  }

  #[tokio::test]
  async fn refusals_reach_the_app_as_their_kind() {
    let refused = http_fetch(get("http://chat.example.com/api/v1/server-info")).await;
    assert!(matches!(refused, Err(HttpFetchError::PlainHttpRefused(host)) if host == "chat.example.com"));
    let invalid = http_fetch(get("chat.example.com")).await;
    assert!(matches!(invalid, Err(HttpFetchError::InvalidRequest(_))));
  }

  #[test]
  fn the_request_reaches_the_core_as_the_app_sent_it() {
    let request = HttpFetchRequest {
      method: HttpFetchMethod::Post,
      headers: vec![HttpFetchHeader {
        name: "Accept".into(),
        value: "application/json".into(),
      }],
      body: b"form".to_vec(),
      redirects: HttpFetchRedirects::Follow,
      allow_insecure_http: true,
      max_body_bytes: 7,
      ..get("http://10.0.2.2:8081/token")
    };
    let core = FetchRequest::from(request);
    assert_eq!(core.method, FetchMethod::Post);
    assert_eq!(core.url, "http://10.0.2.2:8081/token");
    assert_eq!(
      core.headers,
      vec![("Accept".to_owned(), "application/json".to_owned())]
    );
    assert_eq!(core.body, b"form");
    assert_eq!(core.redirects, Redirects::Follow);
    assert_eq!(core.plain_http, PlainHttp::Allowed);
    assert_eq!(core.connect_timeout, Duration::from_secs(10));
    assert_eq!(core.read_timeout, Duration::from_secs(15));
    assert_eq!(core.max_body_bytes, 7);
    assert_eq!(
      FetchRequest::from(get("https://chat.example.com/")).plain_http,
      PlainHttp::LoopbackOnly
    );
  }

  #[test]
  fn every_failure_keeps_its_kind() {
    let cases = [
      (FetchError::InvalidRequest("x".into()), "InvalidRequest"),
      (FetchError::PlainHttpRefused("x".into()), "PlainHttpRefused"),
      (
        FetchError::UntrustedCertificate("x".into()),
        "UntrustedCertificate",
      ),
      (FetchError::TimedOut("x".into()), "TimedOut"),
      (FetchError::TooLarge { max_body_bytes: 1 }, "TooLarge"),
      (FetchError::Network("x".into()), "Network"),
    ];
    for (core, kind) in cases {
      let mapped = format!("{:?}", HttpFetchError::from(core));
      assert!(mapped.starts_with(kind), "{mapped}");
    }
  }

  /// A GET with the release builds' policy and the server check's limits.
  fn get(url: &str) -> HttpFetchRequest {
    HttpFetchRequest {
      method: HttpFetchMethod::Get,
      url: url.to_owned(),
      headers: Vec::new(),
      body: Vec::new(),
      redirects: HttpFetchRedirects::DontFollow,
      allow_insecure_http: false,
      connect_timeout: Duration::from_secs(10),
      read_timeout: Duration::from_secs(15),
      max_body_bytes: 1024 * 1024,
    }
  }

  /// Answers every request on `listener` with [`ANSWER`] and closes the connection.
  async fn answer_every_request(listener: TcpListener) {
    while let Ok((mut socket, _)) = listener.accept().await {
      let mut request = [0u8; 4096];
      let _ = socket.read(&mut request).await;
      let _ = socket.write_all(ANSWER.as_bytes()).await;
    }
  }
}
