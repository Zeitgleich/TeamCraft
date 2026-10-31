//! Message search: query syntax, matching, snippets. Pure logic with no I/O, so the same code can run
//! over decrypted data in `teamcraft-core` (encrypted index segments, e2ee.md §10) and over the mock.
//!
//! Syntax: plain words (all must match), `"exact phrase"`, and the filters `from:name`, `in:channel`,
//! `space:name`, `has:file`, `has:link`, `before:YYYY-MM-DD`, `after:YYYY-MM-DD` and `in-thread`.

use crate::model::{Message, TimestampMs};

const MS_PER_DAY: i64 = 86_400_000;
/// Characters of context kept on each side of the first match in a snippet.
const SNIPPET_CONTEXT: usize = 70;
const FILE_EXTENSIONS: [&str; 8] = [".pdf", ".png", ".jpg", ".xlsx", ".docx", ".zip", ".csv", ".txt"];

/// One filter of a query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Filter {
  From(String),
  In(String),
  /// Only messages of channels in the space with this name.
  InSpace(String),
  HasFile,
  HasLink,
  /// Messages sent before this day (UTC midnight).
  Before(TimestampMs),
  /// Messages sent after the end of this day.
  After(TimestampMs),
  InThread,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenKind {
  Term(String),
  Filter(Filter),
  /// A filter the parser could not read, e.g. `before:yesterday`. It matches nothing special and is
  /// shown to the user as a problem.
  Invalid,
}

/// A piece of the query as typed, so it can be shown as a chip and removed again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
  pub raw: String,
  pub kind: TokenKind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedQuery {
  pub tokens: Vec<Token>,
}

impl ParsedQuery {
  pub fn terms(&self) -> Vec<&str> {
    self
      .tokens
      .iter()
      .filter_map(|token| {
        if let TokenKind::Term(term) = &token.kind {
          Some(term.as_str())
        } else {
          None
        }
      })
      .collect()
  }

  pub fn filters(&self) -> Vec<&Filter> {
    self
      .tokens
      .iter()
      .filter_map(|token| {
        if let TokenKind::Filter(filter) = &token.kind {
          Some(filter)
        } else {
          None
        }
      })
      .collect()
  }

  /// Nothing to search for yet.
  pub fn is_empty(&self) -> bool {
    self.terms().is_empty() && self.filters().is_empty()
  }

  /// The space the query is limited to (`space:name`), if it names one.
  pub fn space_scope(&self) -> Option<&str> {
    self.filters().into_iter().find_map(|filter| match filter {
      Filter::InSpace(name) => Some(name.as_str()),
      _ => None,
    })
  }

  /// The query limited to `maybe_space` (`None`: all spaces). Replaces an existing `space:` filter
  /// and keeps every other token.
  pub fn with_space_scope(&self, maybe_space: Option<&str>) -> String {
    let mut parts: Vec<String> = self
      .tokens
      .iter()
      .filter(|token| !matches!(token.kind, TokenKind::Filter(Filter::InSpace(_))))
      .map(|token| token.raw.clone())
      .collect();
    if let Some(name) = maybe_space {
      parts.push(if name.contains(char::is_whitespace) {
        format!("space:\"{name}\"")
      } else {
        format!("space:{name}")
      });
    }
    parts.join(" ")
  }

  /// The query without the token at `index`, for removing a chip.
  pub fn without_token(&self, index: usize) -> String {
    self
      .tokens
      .iter()
      .enumerate()
      .filter(|(i, _)| *i != index)
      .map(|(_, token)| token.raw.as_str())
      .collect::<Vec<_>>()
      .join(" ")
  }
}

pub fn parse_query(raw: &str) -> ParsedQuery {
  ParsedQuery {
    tokens: split_tokens(raw).into_iter().map(|raw| classify(&raw)).collect(),
  }
}

/// Splits on whitespace but keeps `"quoted phrases"` together (quotes stay in the raw text).
fn split_tokens(raw: &str) -> Vec<String> {
  let mut tokens = Vec::new();
  let mut current = String::new();
  let mut in_quotes = false;
  for ch in raw.chars() {
    match ch {
      '"' => {
        in_quotes = !in_quotes;
        current.push(ch);
      },
      ch if ch.is_whitespace() && !in_quotes => {
        if !current.is_empty() {
          tokens.push(std::mem::take(&mut current));
        }
      },
      ch => current.push(ch),
    }
  }
  if !current.is_empty() {
    tokens.push(current);
  }
  tokens
}

fn classify(raw: &str) -> Token {
  let kind = if raw.eq_ignore_ascii_case("in-thread") {
    TokenKind::Filter(Filter::InThread)
  } else if let Some((key, value)) = raw.split_once(':').filter(|(key, _)| is_filter_key(key)) {
    classify_filter(&key.to_ascii_lowercase(), value)
  } else {
    let term = raw.trim_matches('"');
    if term.is_empty() {
      TokenKind::Invalid
    } else {
      TokenKind::Term(term.to_owned())
    }
  };
  Token {
    raw: raw.to_owned(),
    kind,
  }
}

fn is_filter_key(key: &str) -> bool {
  ["from", "in", "space", "has", "before", "after"]
    .iter()
    .any(|known| key.eq_ignore_ascii_case(known))
}

fn classify_filter(key: &str, value: &str) -> TokenKind {
  let value = value.trim_matches('"');
  // `in:"@Ada Lovelace"` and `in:@"Ada Lovelace"` both name the conversation "Ada Lovelace".
  let named = |value: &str| value.trim_start_matches(['@', '#']).trim_matches('"').to_owned();
  match (key, value) {
    (_, "") => TokenKind::Invalid,
    // `in:@` names nothing; as a filter it would match every conversation.
    ("from" | "in", value) if named(value).is_empty() => TokenKind::Invalid,
    ("from", value) => TokenKind::Filter(Filter::From(named(value))),
    ("in", value) => TokenKind::Filter(Filter::In(named(value))),
    ("space", value) => TokenKind::Filter(Filter::InSpace(value.to_owned())),
    ("has", value) if value.eq_ignore_ascii_case("file") => TokenKind::Filter(Filter::HasFile),
    ("has", value) if value.eq_ignore_ascii_case("link") => TokenKind::Filter(Filter::HasLink),
    ("before", value) => {
      parse_date(value).map_or(TokenKind::Invalid, |day| TokenKind::Filter(Filter::Before(day)))
    },
    ("after", value) => parse_date(value).map_or(TokenKind::Invalid, |day| {
      TokenKind::Filter(Filter::After(day + MS_PER_DAY))
    }),
    _ => TokenKind::Invalid,
  }
}

/// `YYYY-MM-DD` as UTC midnight in Unix milliseconds.
pub fn parse_date(text: &str) -> Option<TimestampMs> {
  let mut parts = text.split('-');
  let (year, month, day) = (
    parts.next()?.parse::<i64>().ok()?,
    parts.next()?.parse::<i64>().ok()?,
    parts.next()?.parse::<i64>().ok()?,
  );
  if parts.next().is_some()
    || !(1..=12).contains(&month)
    || !(1..=31).contains(&day)
    || !(1970..=9999).contains(&year)
  {
    return None;
  }
  Some(days_from_civil(year, month, day) * MS_PER_DAY)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
  let year = if month <= 2 { year - 1 } else { year };
  let era = year.div_euclid(400);
  let year_of_era = year - era * 400;
  let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
  let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
  era * 146_097 + day_of_era - 719_468
}

/// What the matcher needs to know about a message besides its text.
pub struct MatchContext<'a> {
  pub author_username: &'a str,
  pub author_name: &'a str,
  pub conversation_name: &'a str,
  /// Empty for conversations outside spaces (they never match `space:`).
  pub space_name: &'a str,
}

/// Whether `message` satisfies every term and filter of `query`.
pub fn matches(query: &ParsedQuery, message: &Message, context: &MatchContext<'_>) -> bool {
  query
    .terms()
    .iter()
    .all(|term| find_ci(&message.body, term).is_some())
    && query.filters().iter().all(|filter| match filter {
      Filter::From(name) => {
        context.author_username.eq_ignore_ascii_case(name) || find_ci(context.author_name, name).is_some()
      },
      Filter::In(name) => find_ci(context.conversation_name, name).is_some(),
      Filter::InSpace(name) => is_in_space(context.space_name, name),
      Filter::HasFile => has_file(&message.body),
      Filter::HasLink => has_link(&message.body),
      Filter::Before(limit) => message.sent_at < *limit,
      Filter::After(limit) => message.sent_at >= *limit,
      Filter::InThread => message.maybe_thread.is_some(),
    })
}

/// Whether a space name matches the `space:` value: the same name ignoring case, or, for a partial
/// value, a name that contains it.
pub fn is_in_space(space_name: &str, wanted: &str) -> bool {
  !space_name.is_empty() && find_ci(space_name, wanted).is_some()
}

pub fn has_link(body: &str) -> bool {
  body.contains("http://") || body.contains("https://")
}

/// Heuristic until messages carry real attachments: a file name with a known extension.
pub fn has_file(body: &str) -> bool {
  let lower = body.to_lowercase();
  FILE_EXTENSIONS.iter().any(|extension| lower.contains(extension))
}

/// A run of snippet text, marked when it matched a search term.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SnippetPart {
  pub text: String,
  pub is_match: bool,
}

/// A short excerpt around the first match with every term highlighted. Falls back to the start of
/// the message when no term matches (filter-only searches).
pub fn make_snippet(body: &str, terms: &[&str]) -> Vec<SnippetPart> {
  let flat: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
  let mut matches: Vec<(usize, usize)> = terms
    .iter()
    .filter(|term| !term.is_empty())
    .flat_map(|term| find_all_ci(&flat, term))
    .collect();
  matches.sort_unstable();
  // Merge overlaps so two terms that touch become one highlight.
  let mut merged: Vec<(usize, usize)> = Vec::new();
  for (start, end) in matches {
    match merged.last_mut() {
      Some(last) if start <= last.1 => last.1 = last.1.max(end),
      _ => merged.push((start, end)),
    }
  }

  let first = merged.first().map_or(0, |(start, _)| *start);
  let from = floor_boundary(&flat, first.saturating_sub(SNIPPET_CONTEXT));
  let to = ceil_boundary(&flat, (first + SNIPPET_CONTEXT * 2).min(flat.len()));
  let mut parts = Vec::new();
  if from > 0 {
    parts.push(SnippetPart {
      text: "…".to_owned(),
      is_match: false,
    });
  }
  let mut cursor = from;
  for (start, end) in merged
    .into_iter()
    .filter(|(start, _)| *start >= from && *start < to)
  {
    let end = end.min(to);
    if start > cursor {
      parts.push(SnippetPart {
        text: flat[cursor..start].to_owned(),
        is_match: false,
      });
    }
    parts.push(SnippetPart {
      text: flat[start..end].to_owned(),
      is_match: true,
    });
    cursor = end;
  }
  if cursor < to {
    parts.push(SnippetPart {
      text: flat[cursor..to].to_owned(),
      is_match: false,
    });
  }
  if to < flat.len() {
    parts.push(SnippetPart {
      text: "…".to_owned(),
      is_match: false,
    });
  }
  parts
}

fn floor_boundary(text: &str, mut index: usize) -> usize {
  while index > 0 && !text.is_char_boundary(index) {
    index -= 1;
  }
  index
}

fn ceil_boundary(text: &str, mut index: usize) -> usize {
  while index < text.len() && !text.is_char_boundary(index) {
    index += 1;
  }
  index
}

/// First case-insensitive occurrence of `needle` as byte range of `haystack`.
pub fn find_ci(haystack: &str, needle: &str) -> Option<(usize, usize)> {
  find_all_ci(haystack, needle).into_iter().next()
}

/// All non-overlapping case-insensitive occurrences, compared per character so umlauts fold too.
fn find_all_ci(haystack: &str, needle: &str) -> Vec<(usize, usize)> {
  let needle: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
  if needle.is_empty() {
    return Vec::new();
  }
  let hay: Vec<(usize, char)> = haystack.char_indices().collect();
  let mut found = Vec::new();
  let mut i = 0;
  while i < hay.len() {
    // Compare lowercase characters; one source character may lowercase to several.
    let mut matched = 0;
    let mut j = i;
    while j < hay.len() && matched < needle.len() {
      let lowered: Vec<char> = hay[j].1.to_lowercase().collect();
      if needle[matched..].starts_with(&lowered) {
        matched += lowered.len();
        j += 1;
      } else {
        break;
      }
    }
    if matched == needle.len() {
      let end = hay.get(j).map_or(haystack.len(), |(offset, _)| *offset);
      found.push((hay[i].0, end));
      i = j;
    } else {
      i += 1;
    }
  }
  found
}

/// One search hit with everything the result list shows.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SearchHit {
  pub message: Message,
  pub conversation_name: String,
  pub maybe_space_name: Option<String>,
  pub snippet: Vec<SnippetPart>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SearchResults {
  pub hits: Vec<SearchHit>,
  /// Matches in total; `hits` is capped.
  pub total: usize,
  /// The answer is still being worked out (the desktop and web cores answer asynchronously), so
  /// "nothing found" must not be shown yet.
  #[serde(default)]
  pub is_pending: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SavedSearch {
  pub id: u64,
  pub name: String,
  pub query: String,
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{MessageKind, id_from_raw};

  #[test]
  fn plain_words_phrases_and_filters_are_tokenized() {
    let query =
      parse_query(r#"deploy "release notes" from:@sam in:#general has:file after:2026-10-01 in-thread"#);
    assert_eq!(query.terms(), ["deploy", "release notes"]);
    let filters: Vec<&Filter> = query.filters();
    assert_eq!(filters.len(), 5);
    assert!(filters.contains(&&Filter::From("sam".into())));
    assert!(filters.contains(&&Filter::In("general".into())));
    assert!(filters.contains(&&Filter::HasFile));
    assert!(filters.contains(&&Filter::InThread));
    assert!(filters.contains(&&Filter::After(parse_date("2026-10-02").expect("date"))));
  }

  mod conversation_filter {
    use super::*;

    #[test]
    fn a_direct_message_is_named_with_or_without_quotes() {
      for raw in [
        r#"in:"@Alice Example" 3000"#,
        r#"in:@"Alice Example" 3000"#,
        r#"3000 in:"Alice Example""#,
      ] {
        let query = parse_query(raw);
        assert_eq!(query.terms(), ["3000"], "{raw}");
        assert_eq!(query.filters(), [&Filter::In("Alice Example".into())], "{raw}");
      }
      let unquoted = parse_query("in:@alice 3000");
      assert_eq!(unquoted.terms(), ["3000"]);
      assert_eq!(unquoted.filters(), [&Filter::In("alice".into())]);
    }

    #[test]
    fn a_channel_is_named_with_or_without_its_hash() {
      for raw in [
        "in:#dev budget review",
        "budget in:dev review",
        r##"in:"#dev" budget review"##,
      ] {
        let query = parse_query(raw);
        assert_eq!(query.terms(), ["budget", "review"], "{raw}");
        assert_eq!(query.filters(), [&Filter::In("dev".into())], "{raw}");
      }
      let spaced = parse_query(r##"in:"#team news" plan"##);
      assert_eq!(spaced.filters(), [&Filter::In("team news".into())]);
    }

    #[test]
    fn a_filter_alone_is_a_query() {
      let query = parse_query(r#"in:"@Alice Example""#);
      assert!(query.terms().is_empty());
      assert!(!query.is_empty());
    }

    #[test]
    fn empty_and_malformed_names_are_flagged() {
      for raw in ["in:", "in:@", "in:\"\"", "in:@\"\"", "from:#"] {
        let query = parse_query(raw);
        assert_eq!(query.tokens[0].kind, TokenKind::Invalid, "{raw}");
        assert!(query.is_empty(), "{raw}");
      }
      assert!(parse_query("").is_empty());
      assert!(parse_query("   ").tokens.is_empty());
      // An unclosed quote takes the rest of the line as the name, nothing is lost or panics.
      let unclosed = parse_query(r#"in:"@Alice Example 3000"#);
      assert_eq!(unclosed.filters(), [&Filter::In("Alice Example 3000".into())]);
    }
  }

  #[test]
  fn the_space_filter_takes_quoted_names() {
    let query = parse_query(r#"budget space:"Project A""#);
    assert_eq!(query.terms(), ["budget"]);
    assert_eq!(query.filters(), [&Filter::InSpace("Project A".into())]);
    assert!(is_in_space("Project A", "project a"));
    assert!(!is_in_space("Project B", "Project A"));
    assert!(
      !is_in_space("", "Project A"),
      "conversations outside spaces never match"
    );
  }

  #[test]
  fn the_space_scope_is_replaced_added_and_removed() {
    let query = parse_query(r#"budget space:"Project A" has:file"#);
    assert_eq!(query.space_scope(), Some("Project A"));
    assert_eq!(
      query.with_space_scope(Some("Internal")),
      "budget has:file space:Internal"
    );
    assert_eq!(
      query.with_space_scope(Some("Project B")),
      r#"budget has:file space:"Project B""#
    );
    assert_eq!(query.with_space_scope(None), "budget has:file");
    assert_eq!(parse_query("budget").space_scope(), None);
  }

  #[test]
  fn unreadable_filters_are_flagged_not_dropped() {
    let query = parse_query("before:yesterday has:video from:");
    assert!(query.tokens.iter().all(|token| token.kind == TokenKind::Invalid));
    assert!(query.is_empty());
  }

  #[test]
  fn colons_in_ordinary_text_stay_terms() {
    assert_eq!(parse_query("12:30 note:x").terms(), ["12:30", "note:x"]);
  }

  #[test]
  fn removing_a_token_rebuilds_the_query() {
    let query = parse_query(r#"alpha from:sam "two words""#);
    assert_eq!(query.without_token(1), r#"alpha "two words""#);
  }

  #[test]
  fn dates_use_utc_midnight() {
    assert_eq!(parse_date("1970-01-02"), Some(MS_PER_DAY));
    assert_eq!(parse_date("2026-10-21"), Some(1_792_540_800_000));
    assert_eq!(parse_date("2026-13-01"), None);
    assert_eq!(parse_date("2026-10"), None);
  }

  #[test]
  fn matching_applies_terms_and_filters() {
    let message = message(
      "Deploy done, see https://example.com/x and report.pdf",
      1_792_540_800_000 + 1000,
      None,
    );
    let context = MatchContext {
      author_username: "sam",
      author_name: "Sam Rivera",
      conversation_name: "engineering",
      space_name: "Project A",
    };
    let check = |text: &str| matches(&parse_query(text), &message, &context);
    assert!(check("deploy has:link has:file from:sam in:eng"));
    assert!(check("DEPLOY from:rivera"));
    assert!(!check("deploy from:jordan"));
    assert!(!check("deploy in:design"));
    assert!(!check("deploy before:2026-10-21"));
    assert!(check("deploy after:2026-10-20 before:2026-10-23"));
    assert!(!check("deploy in-thread"));
    assert!(!check("missing"));
  }

  #[test]
  fn thread_replies_match_in_thread() {
    let reply = message("ok", 0, Some(id_from_raw(5)));
    let context = MatchContext {
      author_username: "a",
      author_name: "A",
      conversation_name: "c",
      space_name: "",
    };
    assert!(matches(&parse_query("in-thread"), &reply, &context));
  }

  #[test]
  fn snippet_highlights_every_term_and_trims_long_text() {
    let body = format!(
      "{} needle in the middle with Haystack {}",
      "x".repeat(200),
      "y".repeat(200)
    );
    let parts = make_snippet(&body, &["needle", "haystack"]);
    let marked: Vec<&str> = parts
      .iter()
      .filter(|part| part.is_match)
      .map(|part| part.text.as_str())
      .collect();
    assert_eq!(marked, ["needle", "Haystack"]);
    assert_eq!(parts.first().map(|part| part.text.as_str()), Some("…"));
    assert_eq!(parts.last().map(|part| part.text.as_str()), Some("…"));
    assert!(parts.iter().map(|part| part.text.len()).sum::<usize>() < 260);
  }

  #[test]
  fn snippet_without_terms_shows_the_start() {
    let parts = make_snippet("Just a short message", &[]);
    assert_eq!(
      parts,
      [SnippetPart {
        text: "Just a short message".into(),
        is_match: false
      }]
    );
  }

  #[test]
  fn matching_folds_umlauts_and_never_splits_characters() {
    assert_eq!(find_ci("Größe und GRÖSSE", "größe"), Some((0, 7)));
    let parts = make_snippet(&format!("{}Äpfel", "é".repeat(100)), &["äpfel"]);
    assert!(parts.iter().any(|part| part.is_match && part.text == "Äpfel"));
  }

  fn message(body: &str, sent_at: i64, maybe_thread: Option<crate::model::ThreadId>) -> Message {
    Message {
      id: id_from_raw(1),
      conversation: id_from_raw(1),
      author: id_from_raw(1),
      kind: MessageKind::User,
      sent_at,
      body: body.to_owned(),
      is_edited: false,
      reactions: Vec::new(),
      reply_count: 0,
      maybe_thread,
      maybe_imported: None,
    }
  }
}
