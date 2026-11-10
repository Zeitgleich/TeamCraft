//! What a span may carry (docs/tracing.md, "What a span may carry"): the layer keeps a field only
//! when its value can't be content, whatever a caller wrote. Names pass a code-name check, paths
//! become route templates, numbers are kept only for measure-like field names, and every kept text
//! is interned into a bounded set of `&'static str`, so a transaction holds no run-time text.

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};

use teamcraft_proto::route_template;

/// Distinct descriptions per process at most; later ones become [`OTHER`].
const MAX_INTERNED: usize = 1024;
/// A code name: lowercase letters, digits, `_` and `.`, at most this long.
const MAX_CODE_NAME_LEN: usize = 48;
/// An SQL summary at most this many characters.
const MAX_SQL_LEN: usize = 120;
/// Fields kept as numbers besides those ending in `_ms` or `_bytes`.
const NUMBER_FIELDS: [&str; 7] = ["status", "attempt", "depth", "rows", "count", "items", "pages"];

/// What text that failed the checks becomes.
pub const OTHER: &str = "other";

static INTERNED: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);

/// What the layer made of one field.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Kept {
  /// The span's name in the code (`name`, `kind`, `event`).
  Name(&'static str),
  /// An HTTP method.
  Method(&'static str),
  /// A route template.
  Route(&'static str),
  /// The class of an error (`timeout`), from the code; marks the span failed.
  Error(&'static str),
  /// A number for the span's data (`status`, `rows`, `ttfb_ms`).
  Number(&'static str, f64),
  /// A boolean flag (`is_*`): kept as 0 or 1.
  Flag(&'static str, bool),
  Dropped,
}

/// A text field: kept only in the shapes above.
pub(super) fn keep_text(field: &'static str, value: &str) -> Kept {
  match field {
    "name" | "kind" | "event" | "job" | "step" => Kept::Name(code_name(value)),
    "error" => Kept::Error(code_name(value)),
    "method" => method(value).map_or(Kept::Dropped, Kept::Method),
    "route" | "path" | "url" => Kept::Route(intern(&route_template(value))),
    _ => Kept::Dropped,
  }
}

/// A number field: kept for measures, never for IDs.
pub(super) fn keep_number(field: &'static str, value: f64) -> Kept {
  if is_number_field(field) && value.is_finite() {
    Kept::Number(field, value)
  } else {
    Kept::Dropped
  }
}

/// A boolean field: `is_*`, `has_*` and `error` (which marks the span failed).
pub(super) fn keep_flag(field: &'static str, value: bool) -> Kept {
  if field == "error" {
    if value {
      Kept::Error("error")
    } else {
      Kept::Dropped
    }
  } else if field.starts_with("is_") || field.starts_with("has_") {
    Kept::Flag(field, value)
  } else {
    Kept::Dropped
  }
}

/// `value` when it looks like a name in the code, else [`OTHER`]; interned.
pub fn code_name(value: &str) -> &'static str {
  let is_code = !value.is_empty()
    && value.len() <= MAX_CODE_NAME_LEN
    && value
      .bytes()
      .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'.');
  if is_code { intern(value) } else { OTHER }
}

/// A description that came over a boundary (the desktop shell to its page): a code name, or an API
/// route template with or without its method (`GET /api/v1/users/{id}`); anything else is
/// [`OTHER`]. Interned.
pub fn checked_description(text: &str) -> &'static str {
  let route = match text.split_once(' ') {
    Some((method_text, route)) if method(method_text).is_some_and(|known| known == method_text) => route,
    Some(_) => return OTHER,
    None => text,
  };
  // Only the API's routes: a bare path from elsewhere (a file) could hold a person's name.
  let is_route = route.starts_with("/api/")
    && route.len() <= 160
    && route
      .bytes()
      .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-/{}".contains(&byte));
  if is_route { intern(text) } else { code_name(text) }
}

/// A number's name that came over a boundary: kept when a span's field of that name would be.
pub(super) fn checked_number_name(name: &str) -> Option<&'static str> {
  (is_number_field(name) || name == "blocked_by" || name.starts_with("is_") || name.starts_with("has_"))
    .then(|| code_name(name))
    .filter(|name| *name != OTHER)
}

/// An SQL statement as a span's description: string and number literals become `?`, whitespace
/// one space, at most [`MAX_SQL_LEN`] characters. Statements come from the code; bind values are
/// never part of them, and a literal written into one goes anyway.
pub fn sql_summary(statement: &str) -> String {
  let mut summary = String::with_capacity(statement.len().min(MAX_SQL_LEN));
  let mut chars = statement.chars().peekable();
  let mut previous = ' ';
  while let Some(c) = chars.next() {
    // Single quotes hold literals; double quotes are identifiers in Postgres and stay.
    let next = if c == '\'' {
      for inner in chars.by_ref() {
        if inner == '\'' {
          break;
        }
      }
      '?'
    } else if c.is_ascii_digit() && !(previous.is_ascii_alphanumeric() || previous == '_' || previous == '$')
    {
      while chars
        .next_if(|d| d.is_ascii_alphanumeric() || *d == '.')
        .is_some()
      {}
      '?'
    } else if c.is_whitespace() {
      if previous == ' ' {
        continue;
      }
      ' '
    } else {
      c
    };
    summary.push(next);
    previous = next;
    if summary.chars().count() >= MAX_SQL_LEN {
      break;
    }
  }
  summary.trim().to_owned()
}

/// One copy of `text` for the rest of the process; [`OTHER`] once [`MAX_INTERNED`] are taken.
pub(super) fn intern(text: &str) -> &'static str {
  let mut slot = INTERNED.lock().unwrap_or_else(PoisonError::into_inner);
  let interned = slot.get_or_insert_with(HashSet::new);
  if let Some(known) = interned.get(text) {
    return known;
  }
  if interned.len() >= MAX_INTERNED {
    return OTHER;
  }
  // Leaked on purpose: at most MAX_INTERNED short strings per process, the price of `'static`.
  let leaked: &'static str = Box::leak(text.to_owned().into_boxed_str());
  interned.insert(leaked);
  leaked
}

fn is_number_field(field: &str) -> bool {
  !field.ends_with("_id")
    && !field.ends_with(".id")
    && (field.ends_with("_ms") || field.ends_with("_bytes") || NUMBER_FIELDS.contains(&field))
}

fn method(value: &str) -> Option<&'static str> {
  ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"]
    .into_iter()
    .find(|known| known.eq_ignore_ascii_case(value))
}

#[cfg(test)]
mod tests {
  use super::*;

  mod fields {
    use super::*;

    #[test]
    fn only_code_names_pass_as_names() {
      assert_eq!(
        keep_text("name", "catch_up_conversation"),
        Kept::Name("catch_up_conversation")
      );
      assert_eq!(keep_text("name", "Hello Bob"), Kept::Name(OTHER));
      assert_eq!(keep_text("kind", "message_create"), Kept::Name("message_create"));
      assert_eq!(keep_text("error", "timeout"), Kept::Error("timeout"));
      assert_eq!(
        keep_text("error", "connection refused by 10.0.0.1"),
        Kept::Error(OTHER)
      );
    }

    #[test]
    fn other_text_fields_are_dropped() {
      for field in [
        "message",
        "body",
        "file_name",
        "user",
        "conversation_id",
        "token",
        "text",
      ] {
        assert_eq!(keep_text(field, "anything"), Kept::Dropped, "{field}");
      }
    }

    #[test]
    fn numbers_only_for_measures_never_ids() {
      assert_eq!(keep_number("status", 200.0), Kept::Number("status", 200.0));
      assert_eq!(keep_number("ttfb_ms", 12.0), Kept::Number("ttfb_ms", 12.0));
      assert_eq!(keep_number("body_bytes", 9.0), Kept::Number("body_bytes", 9.0));
      for field in ["conversation_id", "user", "message", "seq", "phone"] {
        assert_eq!(keep_number(field, 7.0), Kept::Dropped, "{field}");
      }
      assert_eq!(keep_number("rows", f64::NAN), Kept::Dropped);
    }

    #[test]
    fn sql_keeps_the_statement_not_its_literals() {
      assert_eq!(
        sql_summary(
          "SELECT id, body\n   FROM messages WHERE author = 'Alice Example' AND seq > 42 AND t2.x = $1"
        ),
        "SELECT id, body FROM messages WHERE author = ? AND seq > ? AND t2.x = $1"
      );
      assert_eq!(sql_summary(&"x".repeat(500)).len(), MAX_SQL_LEN);
      assert_eq!(sql_summary("'unterminated secret"), "?");
    }

    #[test]
    fn descriptions_from_the_shell_are_names_or_routes() {
      assert_eq!(checked_description("messages"), "messages");
      assert_eq!(
        checked_description("GET /api/v1/users/{id}"),
        "GET /api/v1/users/{id}"
      );
      assert_eq!(checked_description("/api/v1/users/{id}"), "/api/v1/users/{id}");
      assert_eq!(checked_description("GET /api/v1/users/Alice"), OTHER);
      assert_eq!(checked_description("Hello Bob"), OTHER);
      assert_eq!(checked_description("get /x"), OTHER);
      assert_eq!(checked_number_name("busy_ms"), Some("busy_ms"));
      assert_eq!(checked_number_name("conversation_id"), None);
    }

    #[test]
    fn methods_are_known_verbs() {
      assert_eq!(keep_text("method", "get"), Kept::Method("GET"));
      assert_eq!(keep_text("method", "BREW"), Kept::Dropped);
    }
  }
}
