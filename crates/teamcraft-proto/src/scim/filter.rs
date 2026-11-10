//! SCIM filters (RFC 7644 §3.4.2.2) and PATCH paths (§3.5.2): parsed from network input, so the
//! parser is bounded in length and depth, never panics, and is fuzzed (`fuzz/fuzz_targets/scim.rs`).
//!
//! Attribute names are case-insensitive (RFC 7643 §2.1): the parser lower-cases them, and the
//! evaluator expects resources whose keys were lower-cased by [`super::normalize_keys`].

use serde_json::Value;

/// The longest filter or path accepted, in bytes.
pub const MAX_FILTER_BYTES: usize = 1024;
/// The deepest nesting of parentheses, `not` and value paths.
pub const MAX_FILTER_DEPTH: usize = 16;

/// A parsed filter.
#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
  /// `attr op value`.
  Compare {
    path: AttrPath,
    op: CompareOp,
    value: CompValue,
  },
  /// `attr pr`.
  Present(AttrPath),
  And(Box<Filter>, Box<Filter>),
  Or(Box<Filter>, Box<Filter>),
  Not(Box<Filter>),
  /// `attr[filter]`: some element of the multi-valued `attr` matches `filter`.
  ValuePath {
    path: AttrPath,
    filter: Box<Filter>,
  },
}

/// `[urn:]name[.sub]`, lower-cased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttrPath {
  /// The schema URN in front of the name, if any (`urn:ietf:params:scim:schemas:core:2.0:user`).
  pub maybe_urn: Option<String>,
  pub name: String,
  pub maybe_sub: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
  Eq,
  Ne,
  Co,
  Sw,
  Ew,
  Gt,
  Lt,
  Ge,
  Le,
}

/// A comparison value. Numbers are kept as written: SCIM resources here carry none that are
/// compared by size, and equality on the text is exact.
#[derive(Debug, Clone, PartialEq)]
pub enum CompValue {
  Str(String),
  Num(f64),
  Bool(bool),
  Null,
}

/// A PATCH operation's `path`: `attr`, `attr.sub`, `attr[filter]` or `attr[filter].sub`.
#[derive(Debug, Clone, PartialEq)]
pub struct PatchPath {
  pub attr: AttrPath,
  pub maybe_filter: Option<Filter>,
  /// The sub-attribute after a value filter (`emails[type eq "work"].value`).
  pub maybe_value_sub: Option<String>,
}

/// Why a filter or path was refused (`scimType` `invalidFilter` / `invalidPath`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FilterError {
  #[error("the filter is longer than {MAX_FILTER_BYTES} bytes")]
  TooLong,
  #[error("the filter nests deeper than {MAX_FILTER_DEPTH}")]
  TooDeep,
  #[error("unexpected end of the filter")]
  UnexpectedEnd,
  #[error("unexpected input at byte {0}")]
  Unexpected(usize),
  #[error("unknown operator at byte {0}")]
  UnknownOperator(usize),
  #[error("invalid value at byte {0}")]
  InvalidValue(usize),
  #[error("invalid attribute name at byte {0}")]
  InvalidAttribute(usize),
}

impl Filter {
  /// Parses a filter (`filter=` query parameter, a value path's inner filter).
  pub fn parse(input: &str) -> Result<Self, FilterError> {
    let mut parser = Parser::new(input)?;
    let filter = parser.filter(0)?;
    parser.skip_spaces();
    if !parser.is_at_end() {
      return Err(FilterError::Unexpected(parser.position));
    }
    Ok(filter)
  }

  /// Whether `resource` (keys lower-cased) matches. Attributes it doesn't have match nothing but
  /// `ne` and `not`.
  pub fn matches(&self, resource: &Value) -> bool {
    match self {
      Self::Compare { path, op, value } => values_at(resource, path)
        .iter()
        .any(|actual| compare(actual, *op, value, is_case_exact(path))),
      Self::Present(path) => values_at(resource, path).iter().any(|value| is_present(value)),
      Self::And(left, right) => left.matches(resource) && right.matches(resource),
      Self::Or(left, right) => left.matches(resource) || right.matches(resource),
      Self::Not(inner) => !inner.matches(resource),
      Self::ValuePath { path, filter } => elements_at(resource, path)
        .iter()
        .any(|element| filter.matches(element)),
    }
  }

  /// The value `name eq "<value>"` asks for, if this filter is that or an `and` with it: a query
  /// can then look the resource up by that attribute instead of reading all of them.
  pub fn equality_on(&self, name: &str) -> Option<&str> {
    match self {
      Self::Compare {
        path,
        op: CompareOp::Eq,
        value: CompValue::Str(value),
      } if path.maybe_sub.is_none() && path.name == name => Some(value),
      Self::And(left, right) => left.equality_on(name).or_else(|| right.equality_on(name)),
      _ => None,
    }
  }
}

impl PatchPath {
  pub fn parse(input: &str) -> Result<Self, FilterError> {
    let mut parser = Parser::new(input)?;
    parser.skip_spaces();
    let attr = parser.attr_path()?;
    let mut path = PatchPath {
      attr,
      maybe_filter: None,
      maybe_value_sub: None,
    };
    if parser.eat(b'[') {
      path.maybe_filter = Some(parser.filter(1)?);
      parser.skip_spaces();
      if !parser.eat(b']') {
        return Err(parser.unexpected());
      }
      if parser.eat(b'.') {
        path.maybe_value_sub = Some(parser.name()?);
      }
    }
    parser.skip_spaces();
    if !parser.is_at_end() {
      return Err(FilterError::Unexpected(parser.position));
    }
    Ok(path)
  }
}

impl AttrPath {
  /// A path without a URN and sub-attribute.
  pub fn simple(name: &str) -> Self {
    Self {
      maybe_urn: None,
      name: name.to_ascii_lowercase(),
      maybe_sub: None,
    }
  }
}

/// `id` and `externalId` compare exactly (RFC 7643 §3.1), everything else ignores case.
fn is_case_exact(path: &AttrPath) -> bool {
  path.maybe_sub.is_none() && matches!(path.name.as_str(), "id" | "externalid")
}

/// The values a path points at in `resource`: the attribute itself, its sub-attribute, or that
/// sub-attribute of every element of a multi-valued attribute.
fn values_at<'a>(resource: &'a Value, path: &AttrPath) -> Vec<&'a Value> {
  let root = scoped(resource, path);
  let Some(value) = root.and_then(|root| root.get(&path.name)) else {
    return Vec::new();
  };
  let elements: Vec<&Value> = match value {
    Value::Array(items) => items.iter().collect(),
    other => vec![other],
  };
  match &path.maybe_sub {
    None => elements,
    Some(sub) => elements
      .into_iter()
      .filter_map(|element| element.get(sub))
      .collect(),
  }
}

/// The elements of a multi-valued attribute (a single value counts as one element).
fn elements_at<'a>(resource: &'a Value, path: &AttrPath) -> Vec<&'a Value> {
  match scoped(resource, path).and_then(|root| root.get(&path.name)) {
    Some(Value::Array(items)) => items.iter().collect(),
    Some(other) => vec![other],
    None => Vec::new(),
  }
}

/// Where the attribute lives: the resource itself for its core schema, or the extension's object.
fn scoped<'a>(resource: &'a Value, path: &AttrPath) -> Option<&'a Value> {
  match &path.maybe_urn {
    None => Some(resource),
    Some(urn) if super::is_core_schema(urn) => Some(resource),
    Some(urn) => resource.get(urn),
  }
}

fn is_present(value: &Value) -> bool {
  match value {
    Value::Null => false,
    Value::String(text) => !text.is_empty(),
    Value::Array(items) => !items.is_empty(),
    Value::Object(map) => !map.is_empty(),
    _ => true,
  }
}

fn compare(actual: &Value, op: CompareOp, expected: &CompValue, is_case_exact: bool) -> bool {
  match (actual, expected) {
    (Value::String(actual), CompValue::Str(expected)) => {
      let (actual, expected) = if is_case_exact {
        (actual.clone(), expected.clone())
      } else {
        (actual.to_lowercase(), expected.to_lowercase())
      };
      match op {
        CompareOp::Eq => actual == expected,
        CompareOp::Ne => actual != expected,
        CompareOp::Co => actual.contains(&expected),
        CompareOp::Sw => actual.starts_with(&expected),
        CompareOp::Ew => actual.ends_with(&expected),
        CompareOp::Gt => actual > expected,
        CompareOp::Lt => actual < expected,
        CompareOp::Ge => actual >= expected,
        CompareOp::Le => actual <= expected,
      }
    },
    (Value::Bool(actual), CompValue::Bool(expected)) => match op {
      CompareOp::Eq => actual == expected,
      CompareOp::Ne => actual != expected,
      _ => false,
    },
    (Value::Number(actual), CompValue::Num(expected)) => {
      let Some(actual) = actual.as_f64() else {
        return false;
      };
      match op {
        CompareOp::Eq => actual == *expected,
        CompareOp::Ne => actual != *expected,
        CompareOp::Gt => actual > *expected,
        CompareOp::Lt => actual < *expected,
        CompareOp::Ge => actual >= *expected,
        CompareOp::Le => actual <= *expected,
        _ => false,
      }
    },
    (Value::Null, CompValue::Null) => op == CompareOp::Eq,
    (_, CompValue::Null) => op == CompareOp::Ne,
    _ => op == CompareOp::Ne,
  }
}

/// A recursive descent parser over bytes; every step consumes input or fails, and the depth is
/// counted, so no input loops or overflows the stack.
struct Parser<'a> {
  input: &'a [u8],
  position: usize,
}

impl<'a> Parser<'a> {
  fn new(input: &'a str) -> Result<Self, FilterError> {
    if input.len() > MAX_FILTER_BYTES {
      return Err(FilterError::TooLong);
    }
    Ok(Self {
      input: input.as_bytes(),
      position: 0,
    })
  }

  /// `FILTER = term *(SP "or" SP term)`, where `and` binds tighter than `or`.
  fn filter(&mut self, depth: usize) -> Result<Filter, FilterError> {
    if depth > MAX_FILTER_DEPTH {
      return Err(FilterError::TooDeep);
    }
    let mut left = self.conjunction(depth)?;
    while self.keyword("or") {
      let right = self.conjunction(depth)?;
      left = Filter::Or(Box::new(left), Box::new(right));
    }
    Ok(left)
  }

  fn conjunction(&mut self, depth: usize) -> Result<Filter, FilterError> {
    let mut left = self.term(depth)?;
    while self.keyword("and") {
      let right = self.term(depth)?;
      left = Filter::And(Box::new(left), Box::new(right));
    }
    Ok(left)
  }

  fn term(&mut self, depth: usize) -> Result<Filter, FilterError> {
    self.skip_spaces();
    if self.eat(b'(') {
      let inner = self.filter(depth + 1)?;
      self.skip_spaces();
      if !self.eat(b')') {
        return Err(self.unexpected());
      }
      return Ok(inner);
    }
    if self.keyword_followed_by("not", b'(') {
      self.skip_spaces();
      self.eat(b'(');
      let inner = self.filter(depth + 1)?;
      self.skip_spaces();
      if !self.eat(b')') {
        return Err(self.unexpected());
      }
      return Ok(Filter::Not(Box::new(inner)));
    }
    let path = self.attr_path()?;
    if self.eat(b'[') {
      let inner = self.filter(depth + 1)?;
      self.skip_spaces();
      if !self.eat(b']') {
        return Err(self.unexpected());
      }
      return Ok(Filter::ValuePath {
        path,
        filter: Box::new(inner),
      });
    }
    if !self.skip_spaces() {
      return Err(self.unexpected());
    }
    let operator_at = self.position;
    let operator = self.word().to_ascii_lowercase();
    let op = match operator.as_str() {
      "pr" => return Ok(Filter::Present(path)),
      "eq" => CompareOp::Eq,
      "ne" => CompareOp::Ne,
      "co" => CompareOp::Co,
      "sw" => CompareOp::Sw,
      "ew" => CompareOp::Ew,
      "gt" => CompareOp::Gt,
      "lt" => CompareOp::Lt,
      "ge" => CompareOp::Ge,
      "le" => CompareOp::Le,
      _ => return Err(FilterError::UnknownOperator(operator_at)),
    };
    if !self.skip_spaces() {
      return Err(self.unexpected());
    }
    let value = self.comp_value()?;
    Ok(Filter::Compare { path, op, value })
  }

  /// `[URI ":"] ATTRNAME *1subAttr`. A URN is recognised by its `urn:` prefix: everything up to
  /// the last `:` belongs to it.
  fn attr_path(&mut self) -> Result<AttrPath, FilterError> {
    let start = self.position;
    while let Some(byte) = self.peek() {
      if byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'.' | b'_' | b'-' | b'$') {
        self.position += 1;
      } else {
        break;
      }
    }
    let text = std::str::from_utf8(&self.input[start..self.position])
      .map_err(|_| FilterError::InvalidAttribute(start))?
      .to_ascii_lowercase();
    if text.is_empty() {
      return Err(FilterError::InvalidAttribute(start));
    }
    let (maybe_urn, rest) = if text.starts_with("urn:") {
      // The attribute name follows the last colon; a URN's own parts contain dots (`2.0`).
      let Some(colon) = text.rfind(':') else {
        return Err(FilterError::InvalidAttribute(start));
      };
      (Some(text[..colon].to_owned()), text[colon + 1..].to_owned())
    } else {
      (None, text)
    };
    let (name, maybe_sub) = match rest.split_once('.') {
      Some((name, sub)) => (name.to_owned(), Some(sub.to_owned())),
      None => (rest, None),
    };
    let is_valid = |part: &str| {
      part
        .bytes()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'$')
        && part
          .bytes()
          .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'$'))
    };
    if !is_valid(&name) || maybe_sub.as_deref().is_some_and(|sub| !is_valid(sub)) {
      return Err(FilterError::InvalidAttribute(start));
    }
    Ok(AttrPath {
      maybe_urn,
      name,
      maybe_sub,
    })
  }

  fn name(&mut self) -> Result<String, FilterError> {
    let start = self.position;
    while self
      .peek()
      .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'$'))
    {
      self.position += 1;
    }
    if start == self.position {
      return Err(FilterError::InvalidAttribute(start));
    }
    Ok(String::from_utf8_lossy(&self.input[start..self.position]).to_ascii_lowercase())
  }

  fn comp_value(&mut self) -> Result<CompValue, FilterError> {
    let start = self.position;
    match self.peek() {
      Some(b'"') => self.string().map(CompValue::Str),
      Some(_) => {
        let word = self.word();
        match word.to_ascii_lowercase().as_str() {
          "true" => Ok(CompValue::Bool(true)),
          "false" => Ok(CompValue::Bool(false)),
          "null" => Ok(CompValue::Null),
          _ => word
            .parse::<f64>()
            .ok()
            .filter(|number| number.is_finite())
            .map(CompValue::Num)
            .ok_or(FilterError::InvalidValue(start)),
        }
      },
      None => Err(FilterError::UnexpectedEnd),
    }
  }

  /// A JSON string literal, escapes included.
  fn string(&mut self) -> Result<String, FilterError> {
    let start = self.position;
    self.position += 1;
    loop {
      match self.peek() {
        None => return Err(FilterError::UnexpectedEnd),
        Some(b'\\') => self.position += 2,
        Some(b'"') => {
          self.position += 1;
          break;
        },
        Some(_) => self.position += 1,
      }
    }
    let end = self.position.min(self.input.len());
    let literal =
      std::str::from_utf8(&self.input[start..end]).map_err(|_| FilterError::InvalidValue(start))?;
    serde_json::from_str(literal).map_err(|_| FilterError::InvalidValue(start))
  }

  /// The next run of non-space, non-bracket bytes.
  fn word(&mut self) -> String {
    let start = self.position;
    while self
      .peek()
      .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(byte, b'(' | b')' | b'[' | b']'))
    {
      self.position += 1;
    }
    String::from_utf8_lossy(&self.input[start..self.position]).into_owned()
  }

  /// Consumes ` <keyword> ` (case-insensitive) if it comes next; otherwise consumes nothing.
  fn keyword(&mut self, keyword: &str) -> bool {
    let saved = self.position;
    if self.skip_spaces() {
      let start = self.position;
      let end = start + keyword.len();
      let is_keyword = self
        .input
        .get(start..end)
        .is_some_and(|bytes| bytes.eq_ignore_ascii_case(keyword.as_bytes()));
      let is_followed_by_space = self.input.get(end).is_some_and(u8::is_ascii_whitespace);
      if is_keyword && is_followed_by_space {
        self.position = end;
        self.skip_spaces();
        return true;
      }
    }
    self.position = saved;
    false
  }

  /// Consumes `<keyword>` if it is followed (after optional spaces) by `next`.
  fn keyword_followed_by(&mut self, keyword: &str, next: u8) -> bool {
    let start = self.position;
    let end = start + keyword.len();
    let is_keyword = self
      .input
      .get(start..end)
      .is_some_and(|bytes| bytes.eq_ignore_ascii_case(keyword.as_bytes()));
    if !is_keyword {
      return false;
    }
    let mut after = end;
    while self.input.get(after).is_some_and(u8::is_ascii_whitespace) {
      after += 1;
    }
    if self.input.get(after) == Some(&next) {
      self.position = end;
      return true;
    }
    false
  }

  /// Skips spaces; whether there were any.
  fn skip_spaces(&mut self) -> bool {
    let start = self.position;
    while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
      self.position += 1;
    }
    self.position > start
  }

  fn eat(&mut self, byte: u8) -> bool {
    if self.peek() == Some(byte) {
      self.position += 1;
      true
    } else {
      false
    }
  }

  fn peek(&self) -> Option<u8> {
    self.input.get(self.position).copied()
  }

  fn is_at_end(&self) -> bool {
    self.position >= self.input.len()
  }

  fn unexpected(&self) -> FilterError {
    if self.is_at_end() {
      FilterError::UnexpectedEnd
    } else {
      FilterError::Unexpected(self.position)
    }
  }
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;

  mod parsing {
    use super::*;

    #[test]
    fn the_filters_identity_providers_send_parse() {
      for input in [
        r#"userName eq "alice@example.com""#,
        r#"externalId eq "00u1abcd""#,
        r#"displayName eq "Engineering""#,
        r#"id eq "123" and members[value eq "456"]"#,
        r#"emails[type eq "work" and value co "@example.com"]"#,
        r#"meta.lastModified gt "2026-01-01T00:00:00Z""#,
        r#"urn:ietf:params:scim:schemas:core:2.0:User:userName sw "a""#,
        r#"title pr"#,
        r#"not (active eq false) or userName ew ".org""#,
        r#"USERNAME EQ "Bob""#,
      ] {
        assert!(Filter::parse(input).is_ok(), "{input}");
      }
    }

    #[test]
    fn and_binds_tighter_than_or() {
      let filter = Filter::parse(r#"a eq 1 or b eq 2 and c eq 3"#).unwrap();
      assert!(matches!(filter, Filter::Or(_, right) if matches!(*right, Filter::And(_, _))));
    }

    #[test]
    fn a_urn_keeps_its_dots_and_the_name_is_after_the_last_colon() {
      let Filter::Present(path) =
        Filter::parse("urn:ietf:params:scim:schemas:extension:enterprise:2.0:User:department pr").unwrap()
      else {
        panic!("not a presence filter");
      };
      assert_eq!(
        path.maybe_urn.as_deref(),
        Some("urn:ietf:params:scim:schemas:extension:enterprise:2.0:user")
      );
      assert_eq!(path.name, "department");
    }

    #[test]
    fn escapes_in_strings_are_decoded() {
      let filter = Filter::parse(r#"userName eq "a\"b\\c""#).unwrap();
      assert_eq!(filter.equality_on("username"), Some("a\"b\\c"));
    }
  }

  mod hostile_input {
    use super::*;

    #[test]
    fn malformed_filters_are_refused() {
      for input in [
        "",
        " ",
        "userName",
        "userName eq",
        r#"userName eq "unterminated"#,
        r#"userName xx "a""#,
        r#"(userName eq "a""#,
        r#"userName eq "a")"#,
        r#"members[value eq "a""#,
        r#"userName eq "a" and"#,
        r#"userName eq "a" garbage"#,
        "userName eq nope",
        "1abc eq 1",
        "a..b eq 1",
        "urn: eq 1",
        "userName eq NaN",
        "userName eq inf",
        r#"userName eq "\u12""#,
        "not userName eq 1",
        "\u{0}",
      ] {
        assert!(Filter::parse(input).is_err(), "{input:?}");
      }
    }

    #[test]
    fn oversized_and_deeply_nested_filters_are_refused() {
      let long = format!(r#"userName eq "{}""#, "a".repeat(MAX_FILTER_BYTES));
      assert_eq!(Filter::parse(&long), Err(FilterError::TooLong));
      let deep = format!(
        "{}a pr{}",
        "(".repeat(MAX_FILTER_DEPTH + 2),
        ")".repeat(MAX_FILTER_DEPTH + 2)
      );
      assert_eq!(Filter::parse(&deep), Err(FilterError::TooDeep));
      let deep_not = format!(
        "{}a pr{}",
        "not (".repeat(MAX_FILTER_DEPTH + 2),
        ")".repeat(MAX_FILTER_DEPTH + 2)
      );
      assert_eq!(Filter::parse(&deep_not), Err(FilterError::TooDeep));
    }

    #[test]
    fn multibyte_text_never_splits_a_character() {
      for input in [
        "ä eq 1",
        r#"userName eq "ä"#,
        "userName eq ä",
        "a[ä]",
        "\u{1F600}",
      ] {
        let _ = Filter::parse(input);
        let _ = PatchPath::parse(input);
      }
    }
  }

  mod evaluation {
    use super::*;

    #[test]
    fn user_name_compares_without_case_and_external_id_exactly() {
      let user = json!({"username": "Alice@Example.com", "externalid": "AbC"});
      assert!(
        Filter::parse(r#"userName eq "alice@example.com""#)
          .unwrap()
          .matches(&user)
      );
      assert!(Filter::parse(r#"externalId eq "AbC""#).unwrap().matches(&user));
      assert!(!Filter::parse(r#"externalId eq "abc""#).unwrap().matches(&user));
    }

    #[test]
    fn value_paths_match_one_element() {
      let user = json!({"emails": [
        {"type": "home", "value": "a@home.example"},
        {"type": "work", "value": "a@example.com", "primary": true},
      ]});
      assert!(
        Filter::parse(r#"emails[type eq "work" and primary eq true]"#)
          .unwrap()
          .matches(&user)
      );
      assert!(
        !Filter::parse(r#"emails[type eq "home" and primary eq true]"#)
          .unwrap()
          .matches(&user)
      );
      assert!(
        Filter::parse(r#"emails.value ew "@example.com""#)
          .unwrap()
          .matches(&user)
      );
    }

    #[test]
    fn missing_attributes_match_only_negations() {
      let user = json!({"username": "a"});
      assert!(!Filter::parse(r#"title eq "x""#).unwrap().matches(&user));
      assert!(!Filter::parse("title pr").unwrap().matches(&user));
      assert!(Filter::parse(r#"not (title eq "x")"#).unwrap().matches(&user));
    }

    #[test]
    fn equality_is_found_in_a_conjunction_only() {
      let filter = Filter::parse(r#"active eq true and userName eq "a""#).unwrap();
      assert_eq!(filter.equality_on("username"), Some("a"));
      let filter = Filter::parse(r#"active eq true or userName eq "a""#).unwrap();
      assert_eq!(filter.equality_on("username"), None);
    }
  }

  mod patch_paths {
    use super::*;

    #[test]
    fn the_paths_identity_providers_send_parse() {
      let path = PatchPath::parse(r#"emails[type eq "work"].value"#).unwrap();
      assert_eq!(path.attr.name, "emails");
      assert_eq!(path.maybe_value_sub.as_deref(), Some("value"));
      let path = PatchPath::parse("name.givenName").unwrap();
      assert_eq!(path.attr.maybe_sub.as_deref(), Some("givenname"));
      let path = PatchPath::parse(r#"members[value eq "123"]"#).unwrap();
      assert!(path.maybe_filter.is_some());
      let path =
        PatchPath::parse("urn:ietf:params:scim:schemas:extension:enterprise:2.0:User:manager").unwrap();
      assert_eq!(path.attr.name, "manager");
    }

    #[test]
    fn malformed_paths_are_refused() {
      for input in ["", "[", "members[", r#"members[value eq "1"]."#, "a b", "emails]"] {
        assert!(PatchPath::parse(input).is_err(), "{input:?}");
      }
    }
  }
}
