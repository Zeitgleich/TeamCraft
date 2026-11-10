//! SCIM PATCH (RFC 7644 §3.5.2). A user is patched as its JSON resource and read back
//! ([`super::UserAttributes::from_resource`]), so a PATCH, a PUT and a POST go through the same
//! validation. A group's PATCH becomes a list of [`GroupChange`]s, so a change of members never
//! needs the whole member list.
//!
//! Lenient where identity providers differ: `op` in any case, `active` as text, no-path operations
//! whose value names attributes by path (`{"name.givenName": "A"}`), and a replace through a value
//! filter that matches nothing adds the element (`emails[type eq "work"].value` on a user without
//! an address).

use serde::Deserialize;
use serde_json::{Map, Value};

use super::filter::{AttrPath, CompValue, CompareOp, Filter, PatchPath};
use super::resource::{ScimInputError, member_values};
use super::{ENTERPRISE_USER_SCHEMA, MAX_MEMBERS_PER_REQUEST, MAX_PATCH_OPERATIONS, is_core_schema};

/// A PATCH body after [`super::normalize_keys`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PatchRequest {
  #[serde(default)]
  pub schemas: Vec<String>,
  pub operations: Vec<PatchOperation>,
}

/// One operation; `op` is `add`, `replace` or `remove` in any case.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PatchOperation {
  pub op: String,
  #[serde(rename = "path", default)]
  pub maybe_path: Option<String>,
  #[serde(rename = "value", default)]
  pub maybe_value: Option<Value>,
}

/// A change of a group, in the order of the operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupChange {
  DisplayName(String),
  ExternalId(Option<String>),
  AddMembers(Vec<String>),
  RemoveMembers(Vec<String>),
  ReplaceMembers(Vec<String>),
  RemoveAllMembers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
  Add,
  Replace,
  Remove,
}

impl PatchRequest {
  /// Refuses more than [`MAX_PATCH_OPERATIONS`] operations.
  pub fn checked(self) -> Result<Self, ScimInputError> {
    if self.operations.len() > MAX_PATCH_OPERATIONS {
      return Err(ScimInputError::TooMany(MAX_PATCH_OPERATIONS));
    }
    Ok(self)
  }
}

impl PatchOperation {
  fn kind(&self) -> Result<Op, ScimInputError> {
    match self.op.to_ascii_lowercase().as_str() {
      "add" => Ok(Op::Add),
      "replace" => Ok(Op::Replace),
      "remove" => Ok(Op::Remove),
      _ => Err(ScimInputError::UnknownOperation),
    }
  }

  fn path(&self) -> Result<Option<PatchPath>, ScimInputError> {
    self.maybe_path.as_deref().map(parse_path).transpose()
  }
}

/// Applies `operations` to a user resource (keys lowered).
pub fn apply_user_patch(resource: &mut Value, operations: &[PatchOperation]) -> Result<(), ScimInputError> {
  for operation in operations {
    let op = operation.kind()?;
    match operation.path()? {
      Some(path) => apply_at(resource, op, &path, operation.maybe_value.as_ref())?,
      None => {
        if op == Op::Remove {
          return Err(ScimInputError::InvalidPath("remove needs a path".into()));
        }
        let Some(Value::Object(values)) = &operation.maybe_value else {
          return Err(ScimInputError::InvalidValue("value"));
        };
        for (key, value) in values {
          apply_no_path_entry(resource, op, key, value)?;
        }
      },
    }
  }
  Ok(())
}

/// One entry of a no-path operation's value: an attribute (or `name.givenName`), or the
/// enterprise extension's object of attributes.
fn apply_no_path_entry(resource: &mut Value, op: Op, key: &str, value: &Value) -> Result<(), ScimInputError> {
  if key.eq_ignore_ascii_case(ENTERPRISE_USER_SCHEMA) {
    let Value::Object(attributes) = value else {
      return Err(ScimInputError::InvalidValue("value"));
    };
    for (name, inner) in attributes {
      let path = parse_path(&format!("{key}:{name}"))?;
      apply_at(resource, op, &path, Some(inner))?;
    }
    return Ok(());
  }
  apply_at(resource, op, &parse_path(key)?, Some(value))
}

fn parse_path(text: &str) -> Result<PatchPath, ScimInputError> {
  PatchPath::parse(text).map_err(|error| ScimInputError::InvalidPath(error.to_string()))
}

fn apply_at(
  resource: &mut Value,
  op: Op,
  path: &PatchPath,
  maybe_value: Option<&Value>,
) -> Result<(), ScimInputError> {
  let Some(container) = scope_mut(resource, &path.attr) else {
    return Err(ScimInputError::InvalidPath("not an object".into()));
  };
  match &path.maybe_filter {
    None => apply_plain(container, op, &path.attr, maybe_value),
    Some(filter) => apply_filtered(container, op, path, filter, maybe_value),
  }
}

/// The object an attribute lives in: the resource for core attributes, else its extension's
/// object (made if missing).
fn scope_mut<'a>(resource: &'a mut Value, attr: &AttrPath) -> Option<&'a mut Map<String, Value>> {
  let map = resource.as_object_mut()?;
  match &attr.maybe_urn {
    Some(urn) if !is_core_schema(urn) => map
      .entry(urn.clone())
      .or_insert_with(|| Value::Object(Map::new()))
      .as_object_mut(),
    _ => Some(map),
  }
}

fn apply_plain(
  container: &mut Map<String, Value>,
  op: Op,
  attr: &AttrPath,
  maybe_value: Option<&Value>,
) -> Result<(), ScimInputError> {
  let (target, key): (&mut Map<String, Value>, &str) = match &attr.maybe_sub {
    None => (container, &attr.name),
    Some(sub) => {
      let parent = container
        .entry(attr.name.clone())
        .or_insert_with(|| Value::Object(Map::new()));
      if parent.is_null() {
        *parent = Value::Object(Map::new());
      }
      let Some(parent) = parent.as_object_mut() else {
        return Err(ScimInputError::InvalidPath(attr.name.clone()));
      };
      (parent, sub)
    },
  };
  match op {
    Op::Remove => {
      target.remove(key);
    },
    Op::Replace => {
      let value = maybe_value.ok_or(ScimInputError::InvalidValue("value"))?;
      target.insert(key.to_owned(), value.clone());
    },
    Op::Add => {
      let value = maybe_value.ok_or(ScimInputError::InvalidValue("value"))?;
      match (target.get_mut(key), value) {
        (Some(Value::Array(existing)), Value::Array(added)) => existing.extend(added.iter().cloned()),
        (Some(Value::Array(existing)), single) => existing.push(single.clone()),
        (Some(Value::Object(existing)), Value::Object(added)) => {
          existing.extend(added.iter().map(|(key, value)| (key.clone(), value.clone())));
        },
        _ => {
          target.insert(key.to_owned(), value.clone());
        },
      }
    },
  }
  Ok(())
}

fn apply_filtered(
  container: &mut Map<String, Value>,
  op: Op,
  path: &PatchPath,
  filter: &Filter,
  maybe_value: Option<&Value>,
) -> Result<(), ScimInputError> {
  let list = container
    .entry(path.attr.name.clone())
    .or_insert_with(|| Value::Array(Vec::new()));
  if list.is_null() {
    *list = Value::Array(Vec::new());
  }
  let Value::Array(elements) = list else {
    return Err(ScimInputError::InvalidPath(path.attr.name.clone()));
  };
  if op == Op::Remove {
    match &path.maybe_value_sub {
      None => elements.retain(|element| !filter.matches(element)),
      Some(sub) => {
        for element in elements.iter_mut().filter(|element| filter.matches(element)) {
          if let Some(element) = element.as_object_mut() {
            element.remove(sub);
          }
        }
      },
    }
    return Ok(());
  }
  let value = maybe_value.ok_or(ScimInputError::InvalidValue("value"))?;
  let mut matched = false;
  for element in elements.iter_mut().filter(|element| filter.matches(element)) {
    matched = true;
    match (&path.maybe_value_sub, element.as_object_mut(), value) {
      (Some(sub), Some(element), _) => {
        element.insert(sub.clone(), value.clone());
      },
      (None, Some(element), Value::Object(fields)) => {
        element.extend(fields.iter().map(|(key, value)| (key.clone(), value.clone())));
      },
      _ => return Err(ScimInputError::InvalidValue("value")),
    }
  }
  if matched {
    return Ok(());
  }
  // Nothing matched: `attr[key eq "x"].sub` makes the element `{key: x, sub: value}`.
  let Some((key, expected)) = simple_equality(filter) else {
    return Err(ScimInputError::NoTarget);
  };
  let mut element = Map::new();
  element.insert(key, expected);
  match (&path.maybe_value_sub, value) {
    (Some(sub), _) => {
      element.insert(sub.clone(), value.clone());
    },
    (None, Value::Object(fields)) => {
      element.extend(fields.iter().map(|(key, value)| (key.clone(), value.clone())))
    },
    _ => return Err(ScimInputError::InvalidValue("value")),
  }
  elements.push(Value::Object(element));
  Ok(())
}

/// `key eq <value>` on a plain attribute, as the attribute and the value to set.
fn simple_equality(filter: &Filter) -> Option<(String, Value)> {
  let Filter::Compare {
    path,
    op: CompareOp::Eq,
    value,
  } = filter
  else {
    return None;
  };
  if path.maybe_urn.is_some() || path.maybe_sub.is_some() {
    return None;
  }
  let value = match value {
    CompValue::Str(text) => Value::String(text.clone()),
    CompValue::Bool(flag) => Value::Bool(*flag),
    CompValue::Num(_) | CompValue::Null => return None,
  };
  Some((path.name.clone(), value))
}

/// The changes `operations` make to a group. Members are named by `value`; at most
/// [`MAX_MEMBERS_PER_REQUEST`] in all operations together.
pub fn group_changes(operations: &[PatchOperation]) -> Result<Vec<GroupChange>, ScimInputError> {
  let mut changes = Vec::new();
  for operation in operations {
    let op = operation.kind()?;
    match operation.path()? {
      None => {
        if op == Op::Remove {
          return Err(ScimInputError::InvalidPath("remove needs a path".into()));
        }
        let Some(Value::Object(values)) = &operation.maybe_value else {
          return Err(ScimInputError::InvalidValue("value"));
        };
        for (key, value) in values {
          group_change(&mut changes, op, &parse_path(key)?, Some(value))?;
        }
      },
      Some(path) => group_change(&mut changes, op, &path, operation.maybe_value.as_ref())?,
    }
  }
  let named: usize = changes
    .iter()
    .map(|change| match change {
      GroupChange::AddMembers(ids) | GroupChange::RemoveMembers(ids) | GroupChange::ReplaceMembers(ids) => {
        ids.len()
      },
      _ => 0,
    })
    .sum();
  if named > MAX_MEMBERS_PER_REQUEST {
    return Err(ScimInputError::TooMany(MAX_MEMBERS_PER_REQUEST));
  }
  Ok(changes)
}

fn group_change(
  changes: &mut Vec<GroupChange>,
  op: Op,
  path: &PatchPath,
  maybe_value: Option<&Value>,
) -> Result<(), ScimInputError> {
  let attr = &path.attr;
  if attr.maybe_urn.as_deref().is_some_and(|urn| !is_core_schema(urn)) {
    // Extensions of groups are not kept.
    return Ok(());
  }
  match (attr.name.as_str(), attr.maybe_sub.as_deref()) {
    ("members", None) => changes.push(member_change(op, path, maybe_value)?),
    ("displayname", None) => match op {
      Op::Remove => return Err(ScimInputError::Missing("displayName")),
      Op::Add | Op::Replace => {
        let value = maybe_value.ok_or(ScimInputError::InvalidValue("displayName"))?;
        let resource = serde_json::json!({ "displayname": value });
        let attributes = super::GroupAttributes::from_resource(&resource)?;
        changes.push(GroupChange::DisplayName(attributes.display_name));
      },
    },
    ("externalid", None) => match op {
      Op::Remove => changes.push(GroupChange::ExternalId(None)),
      Op::Add | Op::Replace => {
        let value = maybe_value.ok_or(ScimInputError::InvalidValue("externalId"))?;
        let resource = serde_json::json!({ "displayname": "x", "externalid": value });
        let attributes = super::GroupAttributes::from_resource(&resource)?;
        changes.push(GroupChange::ExternalId(attributes.maybe_external_id));
      },
    },
    // `id`, `meta`, `schemas` (Okta repeats the id in a replace) and anything unknown.
    _ => {},
  }
  Ok(())
}

fn member_change(
  op: Op,
  path: &PatchPath,
  maybe_value: Option<&Value>,
) -> Result<GroupChange, ScimInputError> {
  if let Some(filter) = &path.maybe_filter {
    // `members[value eq "1"]`, also `… or value eq "2"`.
    if op != Op::Remove {
      return Err(ScimInputError::InvalidPath(
        "a member filter is for remove".into(),
      ));
    }
    let mut ids = Vec::new();
    collect_member_ids(filter, &mut ids)?;
    return Ok(GroupChange::RemoveMembers(ids));
  }
  Ok(match op {
    Op::Add => GroupChange::AddMembers(member_values(maybe_value)?),
    Op::Replace => GroupChange::ReplaceMembers(member_values(maybe_value)?),
    Op::Remove => match maybe_value {
      None | Some(Value::Null) => GroupChange::RemoveAllMembers,
      Some(value) => GroupChange::RemoveMembers(member_values(Some(value))?),
    },
  })
}

fn collect_member_ids(filter: &Filter, ids: &mut Vec<String>) -> Result<(), ScimInputError> {
  match filter {
    Filter::Compare {
      path,
      op: CompareOp::Eq,
      value: CompValue::Str(id),
    } if path.name == "value" && path.maybe_sub.is_none() => {
      ids.push(id.clone());
      Ok(())
    },
    Filter::Or(left, right) => {
      collect_member_ids(left, ids)?;
      collect_member_ids(right, ids)
    },
    _ => Err(ScimInputError::InvalidFilter(
      "a member filter names members by value eq".into(),
    )),
  }
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;
  use crate::scim::{UserAttributes, normalize_keys};

  const FULL_USER: &str = r#"{
    "userName": "alice@example.com", "externalId": "a-1", "displayName": "Alice A",
    "name": {"givenName": "Alice", "familyName": "A"},
    "emails": [{"value": "alice@example.com", "type": "work", "primary": true}],
    "active": true
  }"#;

  fn patched(operations: Value) -> Result<UserAttributes, ScimInputError> {
    let mut resource = normalize_keys(serde_json::from_str(FULL_USER).unwrap()).unwrap();
    let request: PatchRequest =
      serde_json::from_value(normalize_keys(json!({"schemas": [], "Operations": operations})).unwrap())
        .unwrap();
    apply_user_patch(&mut resource, &request.operations)?;
    UserAttributes::from_resource(&resource)
  }

  mod users {
    use super::*;

    #[test]
    fn entra_id_disables_with_active_as_text() {
      let user = patched(json!([{"op": "Replace", "path": "active", "value": "False"}])).unwrap();
      assert!(!user.is_active);
    }

    #[test]
    fn okta_disables_without_a_path() {
      let user = patched(json!([{"op": "replace", "value": {"id": "1", "active": false}}])).unwrap();
      assert!(!user.is_active);
    }

    #[test]
    fn entra_id_changes_names_and_the_work_address() {
      let user = patched(json!([
        {"op": "Replace", "path": "name.givenName", "value": "Alicia"},
        {"op": "Replace", "path": "emails[type eq \"work\"].value", "value": "alicia@example.com"},
        {"op": "Add", "path": "urn:ietf:params:scim:schemas:extension:enterprise:2.0:User:department", "value": "R&D"},
      ]))
      .unwrap();
      assert_eq!(user.maybe_given_name.as_deref(), Some("Alicia"));
      assert_eq!(user.maybe_email.as_deref(), Some("alicia@example.com"));
    }

    #[test]
    fn dotted_keys_and_extension_objects_in_a_no_path_value_apply() {
      let user = patched(json!([{"op": "replace", "value": {
        "name.familyName": "B",
        "urn:ietf:params:scim:schemas:extension:enterprise:2.0:User": {"employeeNumber": "7"},
      }}]))
      .unwrap();
      assert_eq!(user.maybe_family_name.as_deref(), Some("B"));
    }

    #[test]
    fn a_filtered_replace_without_a_match_adds_the_element() {
      let mut resource = normalize_keys(json!({"userName": "a"})).unwrap();
      let operation = PatchOperation {
        op: "replace".into(),
        maybe_path: Some(r#"emails[type eq "work"].value"#.into()),
        maybe_value: Some(json!("a@example.com")),
      };
      apply_user_patch(&mut resource, &[operation]).unwrap();
      let user = UserAttributes::from_resource(&resource).unwrap();
      assert_eq!(user.maybe_email.as_deref(), Some("a@example.com"));
    }

    #[test]
    fn removing_an_attribute_clears_it() {
      let user = patched(json!([{"op": "remove", "path": "externalId"}])).unwrap();
      assert_eq!(user.maybe_external_id, None);
      let user = patched(json!([{"op": "remove", "path": "emails[type eq \"work\"]"}])).unwrap();
      assert_eq!(user.maybe_email, None);
    }

    #[test]
    fn invalid_operations_are_refused() {
      assert_eq!(
        patched(json!([{"op": "move", "path": "active", "value": true}])),
        Err(ScimInputError::UnknownOperation)
      );
      assert!(matches!(
        patched(json!([{"op": "replace", "path": "emails[", "value": "x"}])),
        Err(ScimInputError::InvalidPath(_))
      ));
      assert!(matches!(
        patched(json!([{"op": "remove"}])),
        Err(ScimInputError::InvalidPath(_))
      ));
      assert_eq!(
        patched(json!([{"op": "replace", "value": "not an object"}])),
        Err(ScimInputError::InvalidValue("value"))
      );
      assert_eq!(
        patched(json!([{"op": "replace", "path": "userName", "value": ""}])),
        Err(ScimInputError::Missing("userName"))
      );
      assert_eq!(
        patched(json!([{"op": "replace", "path": "emails[value pr].value", "value": "x@example.com"}]))
          .map(|user| user.maybe_email),
        Ok(Some("x@example.com".into()))
      );
      assert_eq!(
        patched(json!([{"op": "replace", "path": "phoneNumbers[value pr].value", "value": "1"}])),
        Err(ScimInputError::NoTarget)
      );
    }

    #[test]
    fn too_many_operations_are_refused() {
      let operations = vec![
        PatchOperation {
          op: "remove".into(),
          maybe_path: Some("title".into()),
          maybe_value: None,
        };
        MAX_PATCH_OPERATIONS + 1
      ];
      let request = PatchRequest {
        schemas: Vec::new(),
        operations,
      };
      assert_eq!(
        request.checked(),
        Err(ScimInputError::TooMany(MAX_PATCH_OPERATIONS))
      );
    }
  }

  mod groups {
    use super::*;

    fn changes(operations: Value) -> Result<Vec<GroupChange>, ScimInputError> {
      let request: PatchRequest =
        serde_json::from_value(normalize_keys(json!({"Operations": operations})).unwrap()).unwrap();
      group_changes(&request.operations)
    }

    #[test]
    fn entra_id_adds_and_removes_members_by_value() {
      assert_eq!(
        changes(json!([
          {"op": "Add", "path": "members", "value": [{"value": "1"}, {"value": "2"}]},
          {"op": "Remove", "path": "members", "value": [{"value": "3"}]},
        ])),
        Ok(vec![
          GroupChange::AddMembers(vec!["1".into(), "2".into()]),
          GroupChange::RemoveMembers(vec!["3".into()]),
        ])
      );
    }

    #[test]
    fn okta_removes_members_by_filter_and_renames_without_a_path() {
      assert_eq!(
        changes(json!([
          {"op": "remove", "path": "members[value eq \"1\" or value eq \"2\"]"},
          {"op": "replace", "value": {"id": "9", "displayName": "Ops"}},
        ])),
        Ok(vec![
          GroupChange::RemoveMembers(vec!["1".into(), "2".into()]),
          GroupChange::DisplayName("Ops".into()),
        ])
      );
    }

    #[test]
    fn replacing_and_clearing_members() {
      assert_eq!(
        changes(json!([
          {"op": "replace", "path": "members", "value": [{"value": "4"}]},
          {"op": "remove", "path": "members"},
          {"op": "remove", "path": "externalId"},
        ])),
        Ok(vec![
          GroupChange::ReplaceMembers(vec!["4".into()]),
          GroupChange::RemoveAllMembers,
          GroupChange::ExternalId(None),
        ])
      );
    }

    #[test]
    fn invalid_group_operations_are_refused() {
      assert!(matches!(
        changes(json!([{"op": "remove", "path": "members[display eq \"x\"]"}])),
        Err(ScimInputError::InvalidFilter(_))
      ));
      assert!(matches!(
        changes(json!([{"op": "add", "path": "members[value eq \"1\"]", "value": []}])),
        Err(ScimInputError::InvalidPath(_))
      ));
      assert_eq!(
        changes(json!([{"op": "remove", "path": "displayName"}])),
        Err(ScimInputError::Missing("displayName"))
      );
      let many: Vec<Value> = (0..=MAX_MEMBERS_PER_REQUEST)
        .map(|id| json!({"value": id.to_string()}))
        .collect();
      assert_eq!(
        changes(json!([
          {"op": "add", "path": "members", "value": many[..600]},
          {"op": "add", "path": "members", "value": many[600..]},
        ])),
        Err(ScimInputError::TooMany(MAX_MEMBERS_PER_REQUEST))
      );
    }
  }
}
