//! Trace context shared by clients and the server (docs/tracing.md): the route template that names
//! a request without its IDs, and the header that carries a trace from the client to the server.

/// The header a client sends with an API request: `<trace id>-<span id>`, 32 and 16 hex digits.
pub const SENTRY_TRACE_HEADER: &str = "sentry-trace";

/// Where the current span's `sentry-trace` value comes from: the trace layer, once a process
/// installs it. The core's HTTP client asks here, so it needs no dependency on the layer.
static HEADER_SOURCE: std::sync::OnceLock<fn() -> Option<String>> = std::sync::OnceLock::new();

/// What a route that isn't one becomes (too long).
const OTHER: &str = "other";
/// A path segment kept as it is: a lowercase word of at most this many characters.
const MAX_WORD_LEN: usize = 32;
/// A route template at most this long; longer ones become `other`.
const MAX_ROUTE_LEN: usize = 160;
/// Path segments after which the next segment is always a secret (a meeting or share link's token).
const TOKEN_PARENTS: [&str; 4] = ["link", "links", "share", "shares"];

/// Installs the source of [`current_trace_header`] (once per process; later calls change nothing).
pub fn set_trace_header_source(source: fn() -> Option<String>) {
  // A second install is ignored on purpose: one trace layer per process.
  let _ = HEADER_SOURCE.set(source);
}

/// The `sentry-trace` value of the current span, while one is collected.
pub fn current_trace_header() -> Option<String> {
  HEADER_SOURCE.get().and_then(|source| source())
}

/// A path or URL as a route template: the path only (no scheme, host, query or fragment), every
/// segment that isn't a lowercase word replaced by `{id}`, a link's token by `{token}`.
pub fn route_template(path_or_url: &str) -> String {
  let without_origin = match path_or_url.find("://") {
    Some(at) => {
      let rest = &path_or_url[at + 3..];
      rest.find('/').map_or("/", |slash| &rest[slash..])
    },
    None => path_or_url,
  };
  let path = without_origin.split(['?', '#']).next().unwrap_or_default();
  let mut template = String::with_capacity(path.len());
  let mut is_token_next = false;
  for segment in path.split('/').filter(|segment| !segment.is_empty()) {
    template.push('/');
    if is_token_next {
      template.push_str("{token}");
      is_token_next = false;
    } else if is_word(segment) {
      template.push_str(segment);
      is_token_next = TOKEN_PARENTS.contains(&segment);
    } else {
      template.push_str("{id}");
    }
  }
  if template.is_empty() {
    template.push('/');
  }
  if template.len() > MAX_ROUTE_LEN {
    return OTHER.to_owned();
  }
  template
}

fn is_word(segment: &str) -> bool {
  is_api_version(segment)
    || !segment.is_empty()
      && segment.len() <= MAX_WORD_LEN
      && segment.as_bytes()[0].is_ascii_lowercase()
      && segment
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte == b'_' || byte == b'-')
}

/// `v1`, `v2`: the API's version segment.
fn is_api_version(segment: &str) -> bool {
  segment.len() <= 3
    && segment.starts_with('v')
    && segment.len() > 1
    && segment[1..].bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn ids_and_queries_become_placeholders() {
    assert_eq!(
      route_template("/api/v1/conversations/1234567890/messages?before=99&limit=50"),
      "/api/v1/conversations/{id}/messages"
    );
    assert_eq!(
      route_template("https://chat.example.com/api/v1/users/42/enrollment#x"),
      "/api/v1/users/{id}/enrollment"
    );
    assert_eq!(
      route_template("/me/backgrounds/a1b2/complete"),
      "/me/backgrounds/{id}/complete"
    );
  }

  #[test]
  fn anything_that_is_not_a_lowercase_word_goes() {
    for segment in [
      "Alice",
      "alice.example",
      "alice@example.com",
      "3f2a9c",
      "dGhpcyBpcyBh",
      "550e8400-e29b-41d4-a716-446655440000",
      "a_very_long_lowercase_word_that_could_be_text",
      "%20secret",
    ] {
      assert_eq!(route_template(&format!("/x/{segment}")), "/x/{id}", "{segment}");
    }
  }

  #[test]
  fn a_link_token_goes_even_when_it_looks_like_a_word() {
    assert_eq!(
      route_template("/api/v1/meetings/link/abcdef/knock"),
      "/api/v1/meetings/link/{token}/knock"
    );
  }

  #[test]
  fn empty_and_odd_input_is_safe() {
    assert_eq!(route_template(""), "/");
    assert_eq!(route_template("https://example.com"), "/");
    assert_eq!(route_template("?query=secret"), "/");
    assert_eq!(route_template(&"/abc".repeat(100)), OTHER);
  }
}
