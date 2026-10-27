//! One-off HTTP requests the mobile apps make outside the server API (issue #433): the identity
//! provider's discovery, code exchange and token refresh, a local account's sign-in, the server
//! check, brand images and the video codec download. They go through the core's HTTP client like
//! REST does, so they race IPv6 and IPv4 (Happy Eyeballs, #420) and use the proxy and certificate
//! authorities the app handed over (#157). The platform's own clients (Android's
//! `HttpURLConnection`) try a host's addresses one after another and wait out the connect timeout
//! on a black-holed IPv6 route.
//!
//! The answer comes back as received, whatever its status: the caller decides what a 4xx means.
//! The platform's network security policy doesn't apply here, so plain `http` is checked in this
//! module ([`PlainHttp`]).

use std::fmt;
use std::time::Duration;

use reqwest::header::{AUTHORIZATION, CONTENT_LENGTH, HeaderMap, HeaderName, HeaderValue};
use url::Url;

use super::http::{error_chain, follow_redirect, http_client_builder, insecure_reason};
use super::tls::is_certificate_rejection;

/// Request headers the transport writes itself: a caller's copy (AppAuth sets `Content-Length`)
/// could contradict the body that is actually sent.
const TRANSPORT_HEADERS: [&str; 4] = ["content-length", "transfer-encoding", "connection", "host"];

/// One request as the app wants it sent.
#[derive(Clone, PartialEq, Eq)]
pub struct FetchRequest {
  pub method: FetchMethod,
  pub url: String,
  /// Names and values; `Content-Length`, `Transfer-Encoding`, `Connection` and `Host` are left to
  /// the transport.
  pub headers: Vec<(String, String)>,
  /// Sent with `Post`, also when empty (`Content-Length: 0`); must be empty with `Get`.
  pub body: Vec<u8>,
  pub redirects: Redirects,
  pub plain_http: PlainHttp,
  /// How long connecting (TCP over both address families, TLS) may take.
  pub connect_timeout: Duration,
  /// How long the server may stay silent: until the answer starts (counted from the start of the
  /// request), then between two parts of the body.
  pub read_timeout: Duration,
  /// The body is read up to this many bytes; a longer one fails with [`FetchError::TooLarge`].
  pub max_body_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchMethod {
  Get,
  Post,
}

/// What happens to a redirect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redirects {
  /// Followed as for REST (at most ten hops, never from `https` to `http`), each hop allowed by
  /// [`PlainHttp`] like the first.
  Follow,
  /// The redirect is the answer: a captive portal sends one, and credentials must not go to
  /// another host.
  DontFollow,
}

/// Where plain `http` may go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlainHttp {
  /// Release builds: `https`, plain `http` only to this device (a loopback address).
  LoopbackOnly,
  /// Development builds, and downloads whose content the core checks itself (the video codec,
  /// which Cisco offers over `http` only, against its published hash).
  Allowed,
}

/// An answer as received.
#[derive(Clone, PartialEq, Eq)]
pub struct FetchResponse {
  pub status: u16,
  /// Names in lower case, in the order received; values that aren't text are left out.
  pub headers: Vec<(String, String)>,
  pub body: Vec<u8>,
}

/// Why a request got no answer.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
  /// A malformed URL or header, a scheme other than `http(s)`, a body with `GET`, a zero timeout.
  #[error("invalid request: {0}")]
  InvalidRequest(String),
  /// Plain `http` to another host than this device where [`PlainHttp::LoopbackOnly`] applies, also
  /// as a redirect.
  #[error("plain http is not allowed to {0}")]
  PlainHttpRefused(String),
  /// The server's certificate isn't trusted: most often a TLS-inspecting proxy with a CA the
  /// device doesn't trust.
  #[error("untrusted certificate: {0}")]
  UntrustedCertificate(String),
  #[error("timed out: {0}")]
  TimedOut(String),
  #[error("the answer is larger than {max_body_bytes} bytes")]
  TooLarge { max_body_bytes: u64 },
  /// No connection, a reset, a broken answer, a redirect loop.
  #[error("network: {0}")]
  Network(String),
}

/// A plain `http` URL that [`PlainHttp`] doesn't allow, as the redirect policy reports it.
#[derive(Debug, thiserror::Error)]
#[error("plain http is not allowed to {host}")]
struct RefusedPlainHttp {
  host: String,
}

/// Sends `request` through the core's HTTP client and reads the answer. Dropping the future
/// cancels the request.
pub async fn fetch(request: FetchRequest) -> Result<FetchResponse, FetchError> {
  let client = client_builder(&request)
    .build()
    .map_err(|error| FetchError::InvalidRequest(error_chain(&error)))?;
  send(&client, request).await
}

/// The REST client's configuration (Happy Eyeballs, proxy, keepalive, the platform's CAs) with the
/// request's timeouts and redirect rule. A client per request: these requests are rare (sign-in, a
/// token refresh every few minutes, the server check), and a fresh one uses the CAs the app handed
/// over meanwhile.
fn client_builder(request: &FetchRequest) -> reqwest::ClientBuilder {
  http_client_builder()
    .connect_timeout(request.connect_timeout)
    .read_timeout(request.read_timeout)
    .redirect(redirect_policy(request.redirects, request.plain_http))
}

async fn send(client: &reqwest::Client, request: FetchRequest) -> Result<FetchResponse, FetchError> {
  if request.connect_timeout.is_zero() || request.read_timeout.is_zero() {
    return Err(FetchError::InvalidRequest("a timeout is zero".into()));
  }
  let url = checked_url(&request.url, request.plain_http)?;
  let headers = header_map(&request.headers)?;
  let builder = match request.method {
    FetchMethod::Get if !request.body.is_empty() => {
      return Err(FetchError::InvalidRequest("a GET request has no body".into()));
    },
    FetchMethod::Get => client.get(url),
    // hyper would send no length at all, which some proxies refuse for a POST (411).
    FetchMethod::Post if request.body.is_empty() => client.post(url).header(CONTENT_LENGTH, 0),
    FetchMethod::Post => client.post(url).body(request.body),
  };
  let mut response = builder.headers(headers).send().await.map_err(failure)?;
  let status = response.status().as_u16();
  let headers = response
    .headers()
    .iter()
    .filter_map(|(name, value)| Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned())))
    .collect();
  let body = read_body(&mut response, request.max_body_bytes).await?;
  Ok(FetchResponse {
    status,
    headers,
    body,
  })
}

/// Parses `text` and checks its scheme against `policy`.
fn checked_url(text: &str, policy: PlainHttp) -> Result<Url, FetchError> {
  let url = Url::parse(text).map_err(|error| FetchError::InvalidRequest(format!("{text}: {error}")))?;
  if !matches!(url.scheme(), "http" | "https") {
    return Err(FetchError::InvalidRequest(format!(
      "unsupported scheme {}",
      url.scheme()
    )));
  }
  match refused_plain_http(&url, policy) {
    Some(refused) => Err(FetchError::PlainHttpRefused(refused.host)),
    None => Ok(url),
  }
}

/// `Some` for a plain `http` URL to another host than this device where `policy` allows only
/// loopback; `None` for `https` and every allowed `http`.
fn refused_plain_http(url: &Url, policy: PlainHttp) -> Option<RefusedPlainHttp> {
  let is_refused =
    url.scheme() == "http" && policy == PlainHttp::LoopbackOnly && insecure_reason(url.as_str()).is_some();
  is_refused.then(|| RefusedPlainHttp {
    host: url.host_str().unwrap_or_default().to_owned(),
  })
}

fn redirect_policy(redirects: Redirects, plain_http: PlainHttp) -> reqwest::redirect::Policy {
  match redirects {
    Redirects::DontFollow => reqwest::redirect::Policy::none(),
    Redirects::Follow => {
      reqwest::redirect::Policy::custom(
        move |attempt| match refused_plain_http(attempt.url(), plain_http) {
          Some(refused) => attempt.error(refused),
          None => follow_redirect(attempt),
        },
      )
    },
  }
}

/// The caller's headers; values are never part of an error (one may be a token).
fn header_map(headers: &[(String, String)]) -> Result<HeaderMap, FetchError> {
  let mut map = HeaderMap::new();
  for (name, value) in headers {
    let name = HeaderName::from_bytes(name.as_bytes())
      .map_err(|_| FetchError::InvalidRequest(format!("header name {name:?}")))?;
    if TRANSPORT_HEADERS.contains(&name.as_str()) {
      continue;
    }
    let mut value = HeaderValue::from_str(value)
      .map_err(|_| FetchError::InvalidRequest(format!("value of header {name}")))?;
    value.set_sensitive(name == AUTHORIZATION);
    map.append(name, value);
  }
  Ok(map)
}

/// The body up to `max_body_bytes`; a longer one is refused as soon as it shows (by its
/// `Content-Length` or while reading).
async fn read_body(response: &mut reqwest::Response, max_body_bytes: u64) -> Result<Vec<u8>, FetchError> {
  let too_large = FetchError::TooLarge { max_body_bytes };
  if response
    .content_length()
    .is_some_and(|length| length > max_body_bytes)
  {
    return Err(too_large);
  }
  let mut body = Vec::new();
  while let Some(chunk) = response.chunk().await.map_err(failure)? {
    if (body.len() + chunk.len()) as u64 > max_body_bytes {
      return Err(too_large);
    }
    body.extend_from_slice(&chunk);
  }
  Ok(body)
}

/// A request that got no (complete) answer.
fn failure(error: reqwest::Error) -> FetchError {
  if let Some(refused) = cause::<RefusedPlainHttp>(&error) {
    return FetchError::PlainHttpRefused(refused.host.clone());
  }
  let detail = error_chain(&error);
  if is_certificate_rejection(&error) {
    FetchError::UntrustedCertificate(detail)
  } else if error.is_timeout() {
    FetchError::TimedOut(detail)
  } else {
    FetchError::Network(detail)
  }
}

/// The first error of type `E` among `error`'s causes.
fn cause<'a, E: std::error::Error + 'static>(error: &'a (dyn std::error::Error + 'static)) -> Option<&'a E> {
  let mut maybe_error = Some(error);
  while let Some(error) = maybe_error {
    if let Some(found) = error.downcast_ref::<E>() {
      return Some(found);
    }
    maybe_error = error.source();
  }
  None
}

/// Header names and sizes only: the request may carry a password or a token.
impl fmt::Debug for FetchRequest {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("FetchRequest")
      .field("method", &self.method)
      .field("url", &self.url)
      .field(
        "headers",
        &self.headers.iter().map(|(name, _)| name).collect::<Vec<_>>(),
      )
      .field("body_bytes", &self.body.len())
      .field("redirects", &self.redirects)
      .field("plain_http", &self.plain_http)
      .finish_non_exhaustive()
  }
}

/// The status and sizes only: the body may hold tokens.
impl fmt::Debug for FetchResponse {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("FetchResponse")
      .field("status", &self.status)
      .field("headers", &self.headers.len())
      .field("body_bytes", &self.body.len())
      .finish()
  }
}

#[cfg(test)]
mod tests {
  use std::net::{Ipv4Addr, SocketAddr};
  use std::sync::Arc;
  use std::time::Instant;

  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::{TcpListener, TcpStream};
  use tokio::sync::mpsc;

  use super::*;
  use crate::api::connect::black_hole::{BlackHole, FixedResolver};

  const TOKEN_ANSWER: &str = r#"{"error":"invalid_grant"}"#;

  /// AppAuth's token refresh: a form POST whose error answer the caller reads as it came.
  #[tokio::test]
  async fn a_post_goes_out_with_its_headers_and_body_and_any_answer_comes_back() {
    let answer = format!(
      "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ndate: Tue, 27 Oct 2026 10:00:00 GMT\r\ncontent-length: {}\r\n\r\n{TOKEN_ANSWER}",
      TOKEN_ANSWER.len()
    );
    let (base, mut received) = server(vec![answer]).await;
    let body = b"grant_type=refresh_token&refresh_token=r1".to_vec();
    let request = FetchRequest {
      method: FetchMethod::Post,
      headers: vec![
        ("Content-Type".into(), "application/x-www-form-urlencoded".into()),
        ("Accept".into(), "application/json".into()),
        // AppAuth's own, which must not contradict the body actually sent.
        ("Content-Length".into(), "999".into()),
      ],
      body: body.clone(),
      ..get(format!("{base}/realms/team/token"))
    };

    let response = fetch(request).await.unwrap();

    assert_eq!(response.status, 400);
    assert_eq!(response.body, TOKEN_ANSWER.as_bytes());
    assert!(
      response
        .headers
        .contains(&("date".into(), "Tue, 27 Oct 2026 10:00:00 GMT".into()))
    );
    let sent = received.recv().await.unwrap();
    assert!(
      sent.head.starts_with("POST /realms/team/token HTTP/1.1\r\n"),
      "{}",
      sent.head
    );
    let head = sent.head.to_ascii_lowercase();
    assert!(
      head.contains("content-type: application/x-www-form-urlencoded\r\n"),
      "{head}"
    );
    assert!(head.contains("accept: application/json\r\n"), "{head}");
    assert!(
      head.contains(&format!("content-length: {}\r\n", body.len())),
      "{head}"
    );
    assert!(!head.contains("999"), "{head}");
    assert_eq!(sent.body, body);
  }

  #[tokio::test]
  async fn an_empty_post_says_so() {
    let (base, mut received) = server(vec![ok("")]).await;
    let request = FetchRequest {
      method: FetchMethod::Post,
      ..get(format!("{base}/api/v1/auth/local/logout"))
    };
    assert_eq!(fetch(request).await.unwrap().status, 200);
    let sent = received.recv().await.unwrap();
    assert!(
      sent.head.to_ascii_lowercase().contains("content-length: 0\r\n"),
      "{}",
      sent.head
    );
  }

  #[tokio::test]
  async fn malformed_requests_are_refused_before_anything_is_sent() {
    for request in [
      get("not a url".into()),
      get("ftp://chat.example.com/file".into()),
      FetchRequest {
        body: b"body".to_vec(),
        ..get("https://chat.example.com/".into())
      },
      FetchRequest {
        headers: vec![("bad header".into(), "x".into())],
        ..get("https://chat.example.com/".into())
      },
      FetchRequest {
        headers: vec![("Authorization".into(), "Bearer t\r\nx: y".into())],
        ..get("https://chat.example.com/".into())
      },
      FetchRequest {
        read_timeout: Duration::ZERO,
        ..get("https://chat.example.com/".into())
      },
    ] {
      let error = fetch(request.clone()).await.unwrap_err();
      assert!(
        matches!(error, FetchError::InvalidRequest(_)),
        "{request:?}: {error}"
      );
      // Never a header's value: it may be a token.
      assert!(!error.to_string().contains("Bearer"), "{error}");
    }
  }

  mod redirects {
    use super::*;

    /// The server check and the sign-in: a captive portal's redirect is the answer.
    #[tokio::test]
    async fn a_redirect_is_the_answer_unless_followed() {
      let (target, _) = server(vec![ok("the codec")]).await;
      let redirect = format!("HTTP/1.1 302 Found\r\nlocation: {target}/codec\r\ncontent-length: 0\r\n\r\n");
      let (base, _) = server(vec![redirect.clone(), redirect]).await;

      let answer = fetch(get(format!("{base}/codec"))).await.unwrap();
      assert_eq!(answer.status, 302);
      assert!(
        answer
          .headers
          .contains(&("location".into(), format!("{target}/codec")))
      );

      let followed = FetchRequest {
        redirects: Redirects::Follow,
        ..get(format!("{base}/codec"))
      };
      let answer = fetch(followed).await.unwrap();
      assert_eq!(answer.status, 200);
      assert_eq!(answer.body, b"the codec");
    }

    #[tokio::test]
    async fn a_redirect_to_plain_http_elsewhere_is_refused_in_release_builds() {
      let redirect =
        "HTTP/1.1 302 Found\r\nlocation: http://chat.example.com/login\r\ncontent-length: 0\r\n\r\n";
      let (base, _) = server(vec![redirect.to_owned()]).await;
      let request = FetchRequest {
        redirects: Redirects::Follow,
        ..get(format!("{base}/"))
      };
      let error = fetch(request).await.unwrap_err();
      assert!(
        matches!(&error, FetchError::PlainHttpRefused(host) if host == "chat.example.com"),
        "{error}"
      );
    }
  }

  mod hostile_networks {
    use super::*;

    #[tokio::test]
    async fn plain_http_goes_only_to_this_device_in_release_builds() {
      let error = fetch(get("http://chat.example.com/api/v1/server-info".into()))
        .await
        .unwrap_err();
      assert!(
        matches!(&error, FetchError::PlainHttpRefused(host) if host == "chat.example.com"),
        "{error}"
      );
      // The loopback server of a development stack, and anywhere in development builds.
      let (base, _) = server(vec![ok("local"), ok("anywhere")]).await;
      assert_eq!(fetch(get(format!("{base}/"))).await.unwrap().body, b"local");
      let anywhere = FetchRequest {
        plain_http: PlainHttp::Allowed,
        ..get(format!("{base}/"))
      };
      assert_eq!(fetch(anywhere).await.unwrap().body, b"anywhere");
    }

    #[tokio::test]
    async fn an_answer_past_the_cap_is_refused() {
      let declared = "HTTP/1.1 200 OK\r\ncontent-length: 11\r\n\r\n01234567890".to_owned();
      let chunked =
        "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n6\r\n012345\r\n5\r\n67890\r\n0\r\n\r\n"
          .to_owned();
      let (base, _) = server(vec![declared, chunked, ok("0123456789")]).await;
      let capped = || FetchRequest {
        max_body_bytes: 10,
        ..get(format!("{base}/"))
      };
      for _ in 0..2 {
        let error = fetch(capped()).await.unwrap_err();
        assert!(
          matches!(error, FetchError::TooLarge { max_body_bytes: 10 }),
          "{error}"
        );
      }
      assert_eq!(fetch(capped()).await.unwrap().body, b"0123456789");
    }

    #[tokio::test(start_paused = true)]
    async fn a_server_that_never_answers_times_out() {
      let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
      let address = listener.local_addr().unwrap();
      // Takes the request and never answers, like a black-holed connection.
      tokio::spawn(async move {
        let (_socket, _) = listener.accept().await.unwrap();
        std::future::pending::<()>().await;
      });
      // Paused time jumps ahead while everything waits, so the timeout fires without a real wait.
      let error = fetch(get(format!("http://{address}/"))).await.unwrap_err();
      assert!(matches!(error, FetchError::TimedOut(_)), "{error}");
    }

    /// A TLS-inspecting proxy whose CA the device doesn't trust.
    #[tokio::test]
    async fn an_untrusted_certificate_is_named_as_such() {
      let base = crate::api::tls::untrusted_tls_server().await;
      let error = fetch(get(format!("{base}/"))).await.unwrap_err();
      assert!(matches!(error, FetchError::UntrustedCertificate(_)), "{error}");
    }

    /// The 1.0.2 failure (issue #420) for the app's own requests (#433): the identity provider's
    /// name resolves to an IPv6 address that swallows the SYN and an IPv4 address that works. The
    /// token refresh goes out over IPv4 well within a second instead of after the connect timeout.
    #[tokio::test]
    async fn the_request_falls_back_to_ipv4_within_a_second_when_ipv6_is_black_holed() {
      let (base, _) = server(vec![ok(r#"{"access_token":"a"}"#)]).await;
      let v4: SocketAddr = base.trim_start_matches("http://").parse().unwrap();
      let Some(black_hole) = BlackHole::ipv6(v4.port()).await else {
        return;
      };
      let request = FetchRequest {
        method: FetchMethod::Post,
        body: b"grant_type=refresh_token".to_vec(),
        ..get(format!("http://idp.example.com:{}/token", v4.port()))
      };
      // The identity provider's name is no loopback address: a development build's policy.
      let request = FetchRequest {
        plain_http: PlainHttp::Allowed,
        ..request
      };
      let client = client_builder(&request)
        .dns_resolver(Arc::new(FixedResolver(vec![black_hole.address, v4])))
        .build()
        .unwrap();

      let started = Instant::now();
      let answer = tokio::time::timeout(Duration::from_secs(5), send(&client, request))
        .await
        .expect("fell back to IPv4 within the timeout")
        .unwrap();

      assert_eq!(answer.status, 200);
      let elapsed = started.elapsed();
      // IPv6 was tried first and swallowed the attempt; IPv4 followed after the attempt delay.
      assert!(elapsed >= Duration::from_millis(250), "{elapsed:?}");
      assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
    }
  }

  #[test]
  fn debug_output_names_no_secrets() {
    let request = FetchRequest {
      method: FetchMethod::Post,
      headers: vec![("Authorization".into(), "Bearer secret-token".into())],
      body: b"password=hunter2".to_vec(),
      ..get("https://chat.example.com/api/v1/auth/local/login".into())
    };
    let response = FetchResponse {
      status: 200,
      headers: vec![("set-cookie".into(), "session=secret".into())],
      body: br#"{"access_token":"secret"}"#.to_vec(),
    };
    let printed = format!("{request:?} {response:?}");
    assert!(
      !printed.contains("secret") && !printed.contains("hunter2"),
      "{printed}"
    );
    assert!(printed.contains("Authorization"), "{printed}");
  }

  /// A GET with the release builds' policy, short timeouts and a 64 KiB cap.
  fn get(url: String) -> FetchRequest {
    FetchRequest {
      method: FetchMethod::Get,
      url,
      headers: Vec::new(),
      body: Vec::new(),
      redirects: Redirects::DontFollow,
      plain_http: PlainHttp::LoopbackOnly,
      connect_timeout: Duration::from_secs(10),
      read_timeout: Duration::from_secs(15),
      max_body_bytes: 64 * 1024,
    }
  }

  fn ok(body: &str) -> String {
    format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}", body.len())
  }

  /// What the server received on one connection.
  struct Received {
    head: String,
    body: Vec<u8>,
  }

  /// Answers one connection after another on `127.0.0.1` with the next of `answers` (whole HTTP
  /// responses, each closing its connection) and passes on what it received.
  async fn server(answers: Vec<String>) -> (String, mpsc::UnboundedReceiver<Received>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::unbounded_channel();
    tokio::spawn(async move {
      for answer in answers {
        let Ok((mut socket, _)) = listener.accept().await else {
          return;
        };
        let received = read_request(&mut socket).await;
        let _ = sender.send(received);
        let _ = socket.write_all(answer.as_bytes()).await;
        let _ = socket.shutdown().await;
      }
    });
    (format!("http://{address}"), receiver)
  }

  async fn read_request(socket: &mut TcpStream) -> Received {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    let head_end = loop {
      if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
        break end + 4;
      }
      let read = socket.read(&mut buffer).await.unwrap();
      assert!(read > 0, "the connection closed before the request was complete");
      bytes.extend_from_slice(&buffer[..read]);
    };
    let head = String::from_utf8(bytes[..head_end].to_vec()).unwrap();
    let length = head
      .lines()
      .find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name
          .eq_ignore_ascii_case("content-length")
          .then(|| value.trim().parse::<usize>().unwrap())
      })
      .unwrap_or(0);
    while bytes.len() < head_end + length {
      let read = socket.read(&mut buffer).await.unwrap();
      assert!(read > 0, "the connection closed before the body was complete");
      bytes.extend_from_slice(&buffer[..read]);
    }
    Received {
      head,
      body: bytes[head_end..head_end + length].to_vec(),
    }
  }
}
