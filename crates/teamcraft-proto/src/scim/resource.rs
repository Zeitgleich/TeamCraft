//! What TeamCraft keeps of a SCIM user and group, read from a request's resource (keys lowered by
//! [`super::normalize_keys`]) and validated. Attributes TeamCraft doesn't keep (the enterprise
//! extension, phone numbers, addresses) are accepted and ignored, as identity providers send them.

use serde_json::{Value, json};

use super::{MAX_EMAIL_CHARS, MAX_IDENTIFIER_CHARS, MAX_MEMBERS_PER_REQUEST, MAX_NAME_CHARS, USER_SCHEMA};

/// The attributes of a user TeamCraft keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserAttributes {
  pub user_name: String,
  pub maybe_external_id: Option<String>,
  pub maybe_given_name: Option<String>,
  pub maybe_family_name: Option<String>,
  /// `displayName`, else `name.formatted`.
  pub maybe_display_name: Option<String>,
  /// The primary e-mail address, else the work one, else the first.
  pub maybe_email: Option<String>,
  pub is_active: bool,
}

/// The attributes of a group TeamCraft keeps; members are account IDs as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupAttributes {
  pub display_name: String,
  pub maybe_external_id: Option<String>,
  pub members: Vec<String>,
}

/// Why a resource or PATCH was refused; each maps to a `scimType` and a stable `error_code`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScimInputError {
  #[error("the body is not a JSON object")]
  NotAnObject,
  #[error("the body nests too deeply")]
  TooDeep,
  #[error("the required attribute {0} is missing")]
  Missing(&'static str),
  #[error("the attribute {0} has an invalid value")]
  InvalidValue(&'static str),
  #[error("the attribute {attribute} is longer than {max} characters")]
  TooLong { attribute: &'static str, max: usize },
  #[error("more than {0} items in one request")]
  TooMany(usize),
  #[error("unknown PATCH operation")]
  UnknownOperation,
  #[error("invalid PATCH path: {0}")]
  InvalidPath(String),
  #[error("the PATCH path matches nothing")]
  NoTarget,
  #[error("invalid filter: {0}")]
  InvalidFilter(String),
}

impl UserAttributes {
  /// Reads a user from a request resource (keys lowered).
  pub fn from_resource(resource: &Value) -> Result<Self, ScimInputError> {
    if !resource.is_object() {
      return Err(ScimInputError::NotAnObject);
    }
    let user_name = text(resource.get("username"), "userName", MAX_IDENTIFIER_CHARS)?
      .ok_or(ScimInputError::Missing("userName"))?;
    let name = resource.get("name").filter(|name| !name.is_null());
    if name.is_some_and(|name| !name.is_object()) {
      return Err(ScimInputError::InvalidValue("name"));
    }
    let name_part = |key: &str, attribute: &'static str| {
      text(name.and_then(|name| name.get(key)), attribute, MAX_NAME_CHARS)
    };
    let maybe_display_name = match text(resource.get("displayname"), "displayName", MAX_NAME_CHARS)? {
      Some(display_name) => Some(display_name),
      None => name_part("formatted", "name.formatted")?,
    };
    Ok(Self {
      maybe_external_id: text(resource.get("externalid"), "externalId", MAX_IDENTIFIER_CHARS)?,
      maybe_given_name: name_part("givenname", "name.givenName")?,
      maybe_family_name: name_part("familyname", "name.familyName")?,
      maybe_display_name,
      maybe_email: email(resource.get("emails"))?,
      is_active: active(resource.get("active"))?,
      user_name,
    })
  }

  /// The resource as PATCH operations see it (keys lowered), with what TeamCraft keeps.
  pub fn to_patchable(&self) -> Value {
    let mut resource = json!({
      "schemas": [USER_SCHEMA],
      "username": self.user_name,
      "active": self.is_active,
      "name": {},
    });
    if let Some(external_id) = &self.maybe_external_id {
      resource["externalid"] = json!(external_id);
    }
    if let Some(given) = &self.maybe_given_name {
      resource["name"]["givenname"] = json!(given);
    }
    if let Some(family) = &self.maybe_family_name {
      resource["name"]["familyname"] = json!(family);
    }
    if let Some(display_name) = &self.maybe_display_name {
      resource["displayname"] = json!(display_name);
    }
    if let Some(email) = &self.maybe_email {
      resource["emails"] = json!([{"value": email, "type": "work", "primary": true}]);
    }
    resource
  }

  /// The name shown for the account: the display name, else the name parts, else the user name.
  pub fn shown_name(&self) -> String {
    if let Some(display_name) = &self.maybe_display_name {
      return display_name.clone();
    }
    let parts: Vec<&str> = [&self.maybe_given_name, &self.maybe_family_name]
      .into_iter()
      .filter_map(|part| part.as_deref())
      .collect();
    if parts.is_empty() {
      self.user_name.clone()
    } else {
      parts.join(" ")
    }
  }
}

impl GroupAttributes {
  /// Reads a group from a request resource (keys lowered).
  pub fn from_resource(resource: &Value) -> Result<Self, ScimInputError> {
    if !resource.is_object() {
      return Err(ScimInputError::NotAnObject);
    }
    let display_name = text(resource.get("displayname"), "displayName", MAX_IDENTIFIER_CHARS)?
      .ok_or(ScimInputError::Missing("displayName"))?;
    Ok(Self {
      display_name,
      maybe_external_id: text(resource.get("externalid"), "externalId", MAX_IDENTIFIER_CHARS)?,
      members: member_values(resource.get("members"))?,
    })
  }
}

/// The `value`s of a member list: `[{"value": "<id>"}, …]`, at most [`MAX_MEMBERS_PER_REQUEST`].
pub(super) fn member_values(maybe_members: Option<&Value>) -> Result<Vec<String>, ScimInputError> {
  let items = match maybe_members {
    None | Some(Value::Null) => return Ok(Vec::new()),
    Some(Value::Array(items)) => items.as_slice(),
    // A single member object instead of a list.
    Some(single @ Value::Object(_)) => std::slice::from_ref(single),
    Some(_) => return Err(ScimInputError::InvalidValue("members")),
  };
  if items.len() > MAX_MEMBERS_PER_REQUEST {
    return Err(ScimInputError::TooMany(MAX_MEMBERS_PER_REQUEST));
  }
  items
    .iter()
    .map(|item| {
      text(item.get("value"), "members.value", MAX_IDENTIFIER_CHARS)?
        .ok_or(ScimInputError::Missing("members.value"))
    })
    .collect()
}

/// A text attribute: absent, null and blank are `None`; anything but a string is refused. Control
/// characters are refused, surrounding whitespace dropped.
fn text(
  maybe_value: Option<&Value>,
  attribute: &'static str,
  max: usize,
) -> Result<Option<String>, ScimInputError> {
  match maybe_value {
    None | Some(Value::Null) => Ok(None),
    Some(Value::String(text)) => {
      let text = text.trim();
      if text.is_empty() {
        return Ok(None);
      }
      if text.chars().count() > max {
        return Err(ScimInputError::TooLong { attribute, max });
      }
      if text.chars().any(char::is_control) {
        return Err(ScimInputError::InvalidValue(attribute));
      }
      Ok(Some(text.to_owned()))
    },
    Some(_) => Err(ScimInputError::InvalidValue(attribute)),
  }
}

/// The address TeamCraft keeps of `emails`: the primary, else a work one, else the first.
fn email(maybe_emails: Option<&Value>) -> Result<Option<String>, ScimInputError> {
  let items = match maybe_emails {
    None | Some(Value::Null) => return Ok(None),
    Some(Value::Array(items)) => items,
    Some(_) => return Err(ScimInputError::InvalidValue("emails")),
  };
  let is_true = |item: &Value| {
    matches!(item.get("primary"), Some(Value::Bool(true)))
      || item
        .get("primary")
        .and_then(Value::as_str)
        .is_some_and(|text| text.eq_ignore_ascii_case("true"))
  };
  let is_work = |item: &Value| {
    item
      .get("type")
      .and_then(Value::as_str)
      .is_some_and(|kind| kind.eq_ignore_ascii_case("work"))
  };
  let Some(chosen) = items
    .iter()
    .find(|item| is_true(item))
    .or_else(|| items.iter().find(|item| is_work(item)))
    .or_else(|| items.first())
  else {
    return Ok(None);
  };
  let maybe_address = text(chosen.get("value"), "emails.value", MAX_EMAIL_CHARS)?;
  if maybe_address
    .as_deref()
    .is_some_and(|address| !address.contains('@') || address.contains(char::is_whitespace))
  {
    return Err(ScimInputError::InvalidValue("emails.value"));
  }
  Ok(maybe_address)
}

/// `active`: a boolean, or the strings `"True"` / `"False"` some identity providers send in a
/// PATCH. Absent means active.
fn active(maybe_value: Option<&Value>) -> Result<bool, ScimInputError> {
  match maybe_value {
    None | Some(Value::Null) => Ok(true),
    Some(Value::Bool(is_active)) => Ok(*is_active),
    Some(Value::String(text)) if text.eq_ignore_ascii_case("true") => Ok(true),
    Some(Value::String(text)) if text.eq_ignore_ascii_case("false") => Ok(false),
    Some(_) => Err(ScimInputError::InvalidValue("active")),
  }
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;
  use crate::scim::normalize_keys;

  fn user(value: Value) -> Result<UserAttributes, ScimInputError> {
    UserAttributes::from_resource(&normalize_keys(value).unwrap())
  }

  #[test]
  fn a_minimal_user_is_active_and_named_by_user_name() {
    let attributes = user(json!({"userName": "alice"})).unwrap();
    assert!(attributes.is_active);
    assert_eq!(attributes.shown_name(), "alice");
  }

  #[test]
  fn the_primary_address_wins_then_work_then_the_first() {
    let attributes = user(json!({"userName": "a", "emails": [
      {"value": "a@home.example", "type": "home"},
      {"value": "a@example.com", "type": "work"},
    ]}))
    .unwrap();
    assert_eq!(attributes.maybe_email.as_deref(), Some("a@example.com"));
    let attributes = user(json!({"userName": "a", "emails": [
      {"value": "a@example.com", "type": "work"},
      {"value": "a@home.example", "primary": true},
    ]}))
    .unwrap();
    assert_eq!(attributes.maybe_email.as_deref(), Some("a@home.example"));
  }

  #[test]
  fn invalid_users_are_refused() {
    assert_eq!(user(json!({})), Err(ScimInputError::Missing("userName")));
    assert_eq!(
      user(json!({"userName": "  "})),
      Err(ScimInputError::Missing("userName"))
    );
    assert_eq!(
      user(json!({"userName": 5})),
      Err(ScimInputError::InvalidValue("userName"))
    );
    assert_eq!(
      user(json!({"userName": "a\u{0}b"})),
      Err(ScimInputError::InvalidValue("userName"))
    );
    assert!(matches!(
      user(json!({"userName": "a".repeat(MAX_IDENTIFIER_CHARS + 1)})),
      Err(ScimInputError::TooLong { .. })
    ));
    assert_eq!(
      user(json!({"userName": "a", "active": "maybe"})),
      Err(ScimInputError::InvalidValue("active"))
    );
    assert_eq!(
      user(json!({"userName": "a", "name": "x"})),
      Err(ScimInputError::InvalidValue("name"))
    );
    assert_eq!(
      user(json!({"userName": "a", "emails": [{"value": "no address"}]})),
      Err(ScimInputError::InvalidValue("emails.value"))
    );
    assert_eq!(
      UserAttributes::from_resource(&json!([])),
      Err(ScimInputError::NotAnObject)
    );
  }

  #[test]
  fn active_as_text_is_understood() {
    assert!(
      !user(json!({"userName": "a", "active": "False"}))
        .unwrap()
        .is_active
    );
  }

  #[test]
  fn a_user_survives_the_patchable_form() {
    let attributes = user(json!({
      "userName": "a", "externalId": "x", "displayName": "A B",
      "name": {"givenName": "A", "familyName": "B"},
      "emails": [{"value": "a@example.com"}], "active": false,
    }))
    .unwrap();
    assert_eq!(
      UserAttributes::from_resource(&attributes.to_patchable()),
      Ok(attributes)
    );
  }

  #[test]
  fn groups_are_read_with_their_members() {
    let group = GroupAttributes::from_resource(
      &normalize_keys(
        json!({"displayName": "Engineering", "members": [{"value": "1"}, {"value": "2", "display": "B"}]}),
      )
      .unwrap(),
    )
    .unwrap();
    assert_eq!(group.members, vec!["1", "2"]);
    let too_many: Vec<Value> = (0..=MAX_MEMBERS_PER_REQUEST)
      .map(|id| json!({"value": id.to_string()}))
      .collect();
    assert_eq!(
      GroupAttributes::from_resource(&json!({"displayname": "g", "members": too_many})),
      Err(ScimInputError::TooMany(MAX_MEMBERS_PER_REQUEST))
    );
    assert_eq!(
      GroupAttributes::from_resource(&json!({"members": []})),
      Err(ScimInputError::Missing("displayName"))
    );
  }
}
