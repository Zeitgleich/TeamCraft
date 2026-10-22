//! The loopback redirect receiver of RFC 8252 §7.3: a one-shot HTTP listener on `127.0.0.1` with a
//! random port that the browser is redirected to after login.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::AuthError;

const CALLBACK_PATH: &str = "/callback";
const MAX_REQUEST_BYTES: usize = 8 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Page shown in the browser tab after the redirect (English and German, shown together because the
/// shell does not know the user's language here).
const DONE_PAGE: &str = "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><title>TeamCraft</title>\
<body style=\"font:16px system-ui,sans-serif;max-width:32rem;margin:4rem auto;padding:0 1rem\">\
<h1>TeamCraft</h1><p>You are signed in. You can close this tab and return to the app.</p>\
<p lang=\"de\">Du bist angemeldet. Du kannst diesen Tab schlie&szlig;en und zur App zur&uuml;ckkehren.</p></body></html>";

/// Parameters the identity provider put on the redirect.
#[derive(Debug, PartialEq, Eq)]
pub struct CallbackParams {
  maybe_code: Option<String>,
  maybe_error: Option<String>,
}

impl CallbackParams {
  /// The authorization code, or the provider's refusal as an error.
  pub fn into_code(self) -> Result<String, AuthError> {
    if let Some(error) = self.maybe_error {
      return Err(AuthError::Denied(error));
    }
    self
      .maybe_code
      .ok_or_else(|| AuthError::InvalidResponse("the redirect carried no code".to_owned()))
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
/// (favicon requests, port scanners) is answered with an error and ignored.
pub async fn wait_for_redirect(
  listener: TcpListener,
  expected_state: &str,
  redirect_uri: &str,
) -> Result<CallbackParams, AuthError> {
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
    match parse_request(&request, &host, expected_state) {
      Parsed::Callback(params) => {
        respond(&mut stream, 200, "OK", DONE_PAGE).await;
        return Ok(params);
      },
      Parsed::WrongState => {
        respond(&mut stream, 400, "Bad Request", "state mismatch").await;
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
  let (mut code, mut state, mut error, mut description) = (None, None, None, None);
  for (key, value) in url.query_pairs() {
    match key.as_ref() {
      "code" => code = Some(value.into_owned()),
      "state" => state = Some(value.into_owned()),
      "error" => error = Some(value.into_owned()),
      "error_description" => description = Some(value.into_owned()),
      _ => {},
    }
  }
  if state.as_deref() != Some(expected_state) {
    return Parsed::WrongState;
  }
  Parsed::Callback(CallbackParams {
    maybe_code: code,
    maybe_error: error.map(|error| description.unwrap_or(error)),
  })
}

async fn respond(stream: &mut TcpStream, status: u16, reason: &str, body: &str) {
  let response = format!(
    "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
     Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}",
    body.len()
  );
  // The browser may already be gone; there is nobody to tell.
  let _ = stream.write_all(response.as_bytes()).await;
  let _ = stream.shutdown().await;
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
  fn provider_errors_become_denied() {
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
    assert!(matches!(params.into_code(), Err(AuthError::Denied(text)) if text == "No thanks"));
  }

  #[tokio::test]
  async fn listener_answers_the_redirect_over_tcp() {
    let (listener, redirect) = bind().await.expect("bind");
    assert!(redirect.starts_with("http://127.0.0.1:") && redirect.ends_with("/callback"));
    let addr = redirect
      .trim_start_matches("http://")
      .split('/')
      .next()
      .expect("host")
      .to_owned();
    let waiter = tokio::spawn({
      let redirect = redirect.clone();
      async move { wait_for_redirect(listener, "st", &redirect).await }
    });
    let mut stream = TcpStream::connect(&addr).await.expect("connect");
    stream
      .write_all(get("/callback?code=the-code&state=st", &addr).as_bytes())
      .await
      .expect("write");
    let mut answer = String::new();
    stream.read_to_string(&mut answer).await.expect("read");
    assert!(answer.starts_with("HTTP/1.1 200"));
    let params = waiter.await.expect("task").expect("params");
    assert_eq!(params.into_code().expect("code"), "the-code");
  }
}
