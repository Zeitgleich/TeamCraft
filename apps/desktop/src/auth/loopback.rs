//! The loopback redirect receiver of RFC 8252 §7.3: a one-shot HTTP listener on `127.0.0.1` with a
//! random port that the browser is redirected to after login. The browser's request stays open
//! until the sign-in is finished, so the page it then shows tells the truth ([`callback_page`]).

use std::time::Duration;

use teamcraft_client_api::oidc_login::shown_error_code;
use teamcraft_client_api::sign_in_look::SignInLook;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::AuthError;
use super::callback_page::{self, CallbackOutcome};

const CALLBACK_PATH: &str = "/callback";
const MAX_REQUEST_BYTES: usize = 8 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// The browser's redirect with the connection it came on; [`Redirect::answer`] shows the page.
pub struct Redirect {
  pub params: CallbackParams,
  stream: TcpStream,
  maybe_accept_language: Option<String>,
}

/// Parameters the identity provider put on the redirect.
#[derive(Debug, PartialEq, Eq)]
pub struct CallbackParams {
  maybe_code: Option<String>,
  /// The provider's `error` code, cleaned by `shown_error_code`. Its `error_description` is
  /// free text in the provider's language, neither shown nor logged.
  maybe_error: Option<String>,
}

impl Redirect {
  /// Shows the page for `outcome` in the browser and closes the connection.
  pub async fn answer(mut self, outcome: CallbackOutcome, look: &SignInLook) {
    let page = callback_page::render(outcome, look, self.maybe_accept_language.as_deref());
    let (status, reason) = outcome.status();
    respond(&mut self.stream, status, reason, &page).await;
  }

  /// Sends the browser on to `url` (the next authorization request of an automatic retry) in the
  /// same tab, and closes the connection.
  pub async fn send_to(mut self, url: &str) {
    let response = format!(
      "HTTP/1.1 303 See Other\r\nLocation: {url}\r\nContent-Length: 0\r\nCache-Control: no-store\r\n\
       Referrer-Policy: no-referrer\r\nConnection: close\r\n\r\n"
    );
    write_response(&mut self.stream, &response).await;
  }
}

impl CallbackParams {
  /// The authorization code, or the provider's refusal as an error.
  pub fn code(&self) -> Result<String, AuthError> {
    if let Some(error) = &self.maybe_error {
      return Err(AuthError::AuthorizationRefused(error.clone()));
    }
    self
      .maybe_code
      .clone()
      .ok_or_else(|| AuthError::InvalidResponse("the redirect carried no code".to_owned()))
  }

  /// The provider's `error` code, when it refused the sign-in.
  pub fn maybe_refusal(&self) -> Option<String> {
    self.maybe_error.clone()
  }
}

/// Binds a random port on the IPv4 loopback address and returns the listener and its redirect URI.
pub async fn bind() -> Result<(TcpListener, String), AuthError> {
  let listener = TcpListener::bind(("127.0.0.1", 0))
    .await
    .map_err(|error| AuthError::Unsupported(error.to_string()))?;
  let port = listener
    .local_addr()
    .map_err(|error| AuthError::Unsupported(error.to_string()))?
    .port();
  Ok((listener, format!("http://127.0.0.1:{port}{CALLBACK_PATH}")))
}

/// Accepts connections until one carries a valid redirect with the expected `state`. Anything else
/// (favicon requests, port scanners) is answered with an error and ignored; a redirect with the
/// wrong `state` gets the error page and ends the wait.
pub async fn wait_for_redirect(
  listener: TcpListener,
  expected_state: &str,
  redirect_uri: &str,
  look: &SignInLook,
) -> Result<Redirect, AuthError> {
  let host = redirect_uri
    .trim_start_matches("http://")
    .split('/')
    .next()
    .unwrap_or_default()
    .to_owned();
  loop {
    let (mut stream, _) = listener
      .accept()
      .await
      .map_err(|error| AuthError::Network(error.to_string()))?;
    let Some(request) = read_request(&mut stream).await else {
      continue;
    };
    let maybe_accept_language = header(&request, "accept-language");
    match parse_request(&request, &host, expected_state) {
      Parsed::Callback(params) => {
        return Ok(Redirect {
          params,
          stream,
          maybe_accept_language,
        });
      },
      Parsed::WrongState => {
        let redirect = Redirect {
          params: CallbackParams {
            maybe_code: None,
            maybe_error: None,
          },
          stream,
          maybe_accept_language,
        };
        redirect.answer(CallbackOutcome::StateMismatch, look).await;
        return Err(AuthError::StateMismatch);
      },
      Parsed::Ignore => respond(&mut stream, 404, "Not Found", "not found").await,
    }
  }
}

#[derive(Debug, PartialEq, Eq)]
enum Parsed {
  Callback(CallbackParams),
  WrongState,
  Ignore,
}

/// Reads the request head (up to the blank line).
async fn read_request(stream: &mut TcpStream) -> Option<String> {
  let mut buffer = Vec::new();
  let mut chunk = [0_u8; 1024];
  loop {
    let read = tokio::time::timeout(READ_TIMEOUT, stream.read(&mut chunk))
      .await
      .ok()?
      .ok()?;
    if read == 0 {
      break;
    }
    buffer.extend_from_slice(&chunk[..read]);
    if buffer.windows(4).any(|window| window == b"\r\n\r\n") || buffer.len() > MAX_REQUEST_BYTES {
      break;
    }
  }
  String::from_utf8(buffer).ok()
}

/// The value of a request header, by case-insensitive name.
fn header(request: &str, name: &str) -> Option<String> {
  request
    .lines()
    .skip(1)
    .take_while(|line| !line.is_empty())
    .filter_map(|line| line.split_once(':'))
    .find(|(key, _)| key.trim().eq_ignore_ascii_case(name))
    .map(|(_, value)| value.trim().to_owned())
}

fn parse_request(request: &str, expected_host: &str, expected_state: &str) -> Parsed {
  let mut lines = request.lines();
  let Some(request_line) = lines.next() else {
    return Parsed::Ignore;
  };
  let mut parts = request_line.split_whitespace();
  let (Some("GET"), Some(target)) = (parts.next(), parts.next()) else {
    return Parsed::Ignore;
  };
  // Refuse requests addressed to another host name (DNS rebinding).
  let host_ok = lines
    .take_while(|line| !line.is_empty())
    .filter_map(|line| line.split_once(':'))
    .any(|(name, value)| name.eq_ignore_ascii_case("host") && value.trim() == expected_host);
  if !host_ok {
    return Parsed::Ignore;
  }
  let Ok(url) = url::Url::parse(&format!("http://{expected_host}{target}")) else {
    return Parsed::Ignore;
  };
  if url.path() != CALLBACK_PATH {
    return Parsed::Ignore;
  }
  let (mut code, mut state, mut error) = (None, None, None);
  for (key, value) in url.query_pairs() {
    match key.as_ref() {
      "code" => code = Some(value.into_owned()),
      "state" => state = Some(value.into_owned()),
      "error" => error = Some(value.into_owned()),
      _ => {},
    }
  }
  if state.as_deref() != Some(expected_state) {
    return Parsed::WrongState;
  }
  Parsed::Callback(CallbackParams {
    maybe_code: code,
    maybe_error: error.map(|error| shown_error_code(&error)),
  })
}

async fn respond(stream: &mut TcpStream, status: u16, reason: &str, body: &str) {
  write_response(stream, &response(status, reason, body)).await;
}

async fn write_response(stream: &mut TcpStream, response: &str) {
  // The browser may already be gone, or stall; there is nobody to tell.
  let _ = tokio::time::timeout(WRITE_TIMEOUT, async {
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
  })
  .await;
}

/// The HTTP response with the page's security headers.
fn response(status: u16, reason: &str, body: &str) -> String {
  format!(
    "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
     Content-Security-Policy: {}\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\n\
     Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}",
    body.len(),
    callback_page::CONTENT_SECURITY_POLICY,
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  const HOST: &str = "127.0.0.1:50000";

  fn get(target: &str, host: &str) -> String {
    format!("GET {target} HTTP/1.1\r\nHost: {host}\r\nAccept: */*\r\n\r\n")
  }

  #[test]
  fn valid_redirect_yields_the_code() {
    let parsed = parse_request(
      &get("/callback?code=abc&state=s1&session_state=x", HOST),
      HOST,
      "s1",
    );
    assert_eq!(
      parsed,
      Parsed::Callback(CallbackParams {
        maybe_code: Some("abc".into()),
        maybe_error: None
      })
    );
  }

  #[test]
  fn wrong_state_is_rejected() {
    assert_eq!(
      parse_request(&get("/callback?code=abc&state=evil", HOST), HOST, "s1"),
      Parsed::WrongState
    );
    assert_eq!(
      parse_request(&get("/callback?code=abc", HOST), HOST, "s1"),
      Parsed::WrongState
    );
  }

  #[test]
  fn other_paths_methods_and_hosts_are_ignored() {
    assert_eq!(
      parse_request(&get("/favicon.ico", HOST), HOST, "s1"),
      Parsed::Ignore
    );
    assert_eq!(
      parse_request(&get("/callback?code=a&state=s1", "attacker.example"), HOST, "s1"),
      Parsed::Ignore
    );
    assert_eq!(
      parse_request(
        "POST /callback HTTP/1.1\r\nHost: 127.0.0.1:50000\r\n\r\n",
        HOST,
        "s1"
      ),
      Parsed::Ignore
    );
    assert_eq!(parse_request("", HOST, "s1"), Parsed::Ignore);
  }

  #[test]
  fn provider_errors_become_refusals_with_their_code() {
    let parsed = parse_request(
      &get(
        "/callback?error=access_denied&error_description=No%20thanks&state=s1",
        HOST,
      ),
      HOST,
      "s1",
    );
    let Parsed::Callback(params) = parsed else {
      unreachable!("state matches")
    };
    assert!(matches!(params.code(), Err(AuthError::AuthorizationRefused(code)) if code == "access_denied"));
    assert_eq!(params.maybe_refusal().as_deref(), Some("access_denied"));
  }

  #[test]
  fn headers_are_read_case_insensitively_from_the_head_only() {
    let request =
      "GET /callback HTTP/1.1\r\nHost: x\r\naccept-LANGUAGE: de-DE,de;q=0.9\r\n\r\nAccept-Language: en";
    assert_eq!(
      header(request, "Accept-Language").as_deref(),
      Some("de-DE,de;q=0.9")
    );
    assert_eq!(header(&get("/", HOST), "accept-language"), None);
  }

  #[test]
  fn every_answer_carries_the_content_security_policy() {
    let answer = response(200, "OK", "<p>x</p>");
    assert!(answer.contains(
      "\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; img-src data:;"
    ));
    assert!(answer.contains("\r\nX-Content-Type-Options: nosniff\r\n"));
    assert!(answer.contains("\r\nCache-Control: no-store\r\n"));
  }

  #[tokio::test]
  async fn the_page_comes_after_the_sign_in_and_never_echoes_the_redirect() {
    let (listener, addr, redirect) = listen().await;
    let waiter = tokio::spawn(async move {
      let look = SignInLook::default();
      let answered = wait_for_redirect(listener, "st-secret", &redirect, &look)
        .await
        .expect("redirect");
      let code = answered.params.code().expect("code");
      answered.answer(CallbackOutcome::SignedIn, &look).await;
      code
    });
    let answer = send(
      &addr,
      &format!(
        "GET /callback?code=the-code&state=st-secret HTTP/1.1\r\nHost: {addr}\r\n\
         Accept-Language: de-DE,de;q=0.9\r\n\r\n"
      ),
    )
    .await;
    assert!(answer.starts_with("HTTP/1.1 200"));
    assert!(answer.contains("Content-Security-Policy: default-src 'none'"));
    assert!(answer.contains("<html lang=\"de\"") && answer.contains("Angemeldet"));
    assert!(!answer.contains("the-code") && !answer.contains("st-secret"));
    assert_eq!(waiter.await.expect("task"), "the-code");
  }

  #[tokio::test]
  async fn a_wrong_state_shows_the_error_page() {
    let (listener, addr, redirect) = listen().await;
    let waiter = tokio::spawn(async move {
      let look = SignInLook {
        maybe_locale: Some("en".to_owned()),
        ..SignInLook::default()
      };
      wait_for_redirect(listener, "st", &redirect, &look).await.err()
    });
    let answer = send(&addr, &get("/callback?code=c&state=forged", &addr)).await;
    assert!(answer.starts_with("HTTP/1.1 400"));
    assert!(answer.contains("This sign-in was not started here"));
    assert!(!answer.contains("forged"));
    assert!(matches!(
      waiter.await.expect("task"),
      Some(AuthError::StateMismatch)
    ));
  }

  async fn listen() -> (TcpListener, String, String) {
    let (listener, redirect) = bind().await.expect("bind");
    assert!(redirect.starts_with("http://127.0.0.1:") && redirect.ends_with("/callback"));
    let addr = redirect
      .trim_start_matches("http://")
      .split('/')
      .next()
      .expect("host")
      .to_owned();
    (listener, addr, redirect)
  }

  async fn send(addr: &str, request: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut answer = String::new();
    stream.read_to_string(&mut answer).await.expect("read");
    answer
  }
}
