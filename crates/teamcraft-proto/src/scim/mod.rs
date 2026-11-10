//! SCIM 2.0 (RFC 7643, RFC 7644) as the server speaks it to identity providers that push accounts
//! and groups (docs/provisioning.md §7): schema names, limits, the error and list bodies, and the
//! parsing of resources, filters and PATCH operations. Everything here is pure, so the parsers are
//! tested and fuzzed without a server.
//!
//! Attribute names are case-insensitive in SCIM: input is parsed after [`normalize_keys`] lowered
//! every key; output uses the canonical case of RFC 7643.

pub mod filter;
pub mod patch;
pub mod resource;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use filter::{AttrPath, CompValue, CompareOp, Filter, FilterError, PatchPath};
pub use patch::{GroupChange, PatchOperation, PatchRequest, apply_user_patch, group_changes};
pub use resource::{GroupAttributes, ScimInputError, UserAttributes};

/// Where the endpoint lives; identity providers are given `<origin>/scim/v2` as its base URL.
pub const SCIM_BASE_PATH: &str = "/scim/v2";
/// The media type of SCIM bodies (RFC 7644 §3.1); plain `application/json` is accepted too.
pub const SCIM_MEDIA_TYPE: &str = "application/scim+json";

pub const USER_SCHEMA: &str = "urn:ietf:params:scim:schemas:core:2.0:User";
pub const GROUP_SCHEMA: &str = "urn:ietf:params:scim:schemas:core:2.0:Group";
/// Entra ID and others send it with users; its attributes are accepted and not stored.
pub const ENTERPRISE_USER_SCHEMA: &str = "urn:ietf:params:scim:schemas:extension:enterprise:2.0:User";
pub const LIST_RESPONSE_SCHEMA: &str = "urn:ietf:params:scim:api:messages:2.0:ListResponse";
pub const PATCH_OP_SCHEMA: &str = "urn:ietf:params:scim:api:messages:2.0:PatchOp";
pub const ERROR_SCHEMA: &str = "urn:ietf:params:scim:api:messages:2.0:Error";
pub const SERVICE_PROVIDER_CONFIG_SCHEMA: &str =
  "urn:ietf:params:scim:schemas:core:2.0:ServiceProviderConfig";
pub const RESOURCE_TYPE_SCHEMA: &str = "urn:ietf:params:scim:schemas:core:2.0:ResourceType";
pub const SCHEMA_SCHEMA: &str = "urn:ietf:params:scim:schemas:core:2.0:Schema";

/// The most resources one list answer carries (`count` is lowered to it), and the default.
pub const MAX_PAGE_SIZE: usize = 200;
/// The most operations of one PATCH request.
pub const MAX_PATCH_OPERATIONS: usize = 100;
/// The most members one request may name (a PUT or POST of a group, a PATCH's operations together).
pub const MAX_MEMBERS_PER_REQUEST: usize = 1000;
/// The longest `userName`, `externalId`, and group `displayName`, in characters.
pub const MAX_IDENTIFIER_CHARS: usize = 256;
/// The longest name part and display name of a user, in characters.
pub const MAX_NAME_CHARS: usize = 200;
/// The longest e-mail address, in characters (RFC 5321 path limit).
pub const MAX_EMAIL_CHARS: usize = 320;
/// The deepest nesting of JSON objects and arrays in a request body.
pub const MAX_JSON_DEPTH: usize = 16;

crate::wire_enum::wire_enum! {
  /// TeamCraft's stable `error_code` of a SCIM error answer (api-conventions.md).
  pub enum ScimErrorCode {
    /// No token, a malformed one, or one no client has (also while the client was revoked).
    InvalidToken => "invalid_token",
    /// Too many requests of this client; `Retry-After` says when to come back.
    RateLimited => "rate_limited",
    /// No such resource of this client.
    NotFound => "scim_not_found",
    /// The body is not JSON, or not the JSON a resource or PATCH is.
    InvalidSyntax => "scim_invalid_syntax",
    /// An attribute has a value TeamCraft doesn't take.
    InvalidValue => "scim_invalid_value",
    /// The filter can't be parsed.
    InvalidFilter => "scim_invalid_filter",
    /// A PATCH path can't be parsed or names nothing.
    InvalidPath => "scim_invalid_path",
    /// Too many operations or members in one request, or a filter that would read too much.
    TooMany => "scim_too_many",
    /// The body is larger than the server takes.
    BodyTooLarge => "scim_body_too_large",
    /// This client has another user with the `userName`, or another group with the `displayName`.
    Uniqueness => "scim_uniqueness",
    /// The account belongs to another SCIM client.
    LinkedElsewhere => "scim_linked_elsewhere",
    /// An admin deleted the account of this identity; it comes back at its own next sign-in.
    IdentityReleased => "scim_identity_released",
    /// The attribute that is the sign-in subject is missing or would change.
    IdentityImmutable => "scim_identity_immutable",
    /// A group member that is no user of this client.
    UnknownMember => "scim_unknown_member",
    /// `If-Match` names another version.
    VersionMismatch => "scim_version_mismatch",
    /// The server can't take the request right now; `Retry-After` says when to come back.
    Unavailable => "unavailable",
    /// Something failed on the server.
    Internal => "internal",
  }
}

/// `scimType` of an error answer (RFC 7644 §3.12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScimType {
  InvalidFilter,
  TooMany,
  Uniqueness,
  Mutability,
  InvalidSyntax,
  InvalidPath,
  NoTarget,
  InvalidValue,
  InvalidVers,
}

/// An error answer. `error_code` is TeamCraft's stable code (api-conventions.md) next to the SCIM
/// fields, which clients that don't know it ignore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScimErrorBody {
  pub schemas: Vec<String>,
  /// The HTTP status as a string, as RFC 7644 §3.12 has it.
  pub status: String,
  #[serde(rename = "scimType", default, skip_serializing_if = "Option::is_none")]
  pub maybe_scim_type: Option<ScimType>,
  pub detail: String,
  pub error_code: String,
}

impl ScimErrorBody {
  pub fn new(
    status: u16,
    maybe_scim_type: Option<ScimType>,
    error_code: &ScimErrorCode,
    detail: String,
  ) -> Self {
    Self {
      schemas: vec![ERROR_SCHEMA.to_owned()],
      status: status.to_string(),
      maybe_scim_type,
      detail,
      error_code: error_code.as_str().to_owned(),
    }
  }
}

/// A page of resources (RFC 7644 §3.4.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListResponse {
  pub schemas: Vec<String>,
  #[serde(rename = "totalResults")]
  pub total_results: usize,
  #[serde(rename = "startIndex")]
  pub start_index: usize,
  #[serde(rename = "itemsPerPage")]
  pub items_per_page: usize,
  #[serde(rename = "Resources")]
  pub resources: Vec<Value>,
}

impl ListResponse {
  pub fn new(total_results: usize, start_index: usize, resources: Vec<Value>) -> Self {
    Self {
      schemas: vec![LIST_RESPONSE_SCHEMA.to_owned()],
      total_results,
      start_index,
      items_per_page: resources.len(),
      resources,
    }
  }
}

/// The query of a list request; `startIndex` is 1-based (RFC 7644 §3.4.2.4).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct ListQuery {
  #[serde(rename = "filter", default)]
  pub maybe_filter: Option<String>,
  #[serde(rename = "startIndex", default)]
  pub maybe_start_index: Option<i64>,
  #[serde(rename = "count", default)]
  pub maybe_count: Option<i64>,
  #[serde(rename = "attributes", default)]
  pub maybe_attributes: Option<String>,
  #[serde(rename = "excludedAttributes", default)]
  pub maybe_excluded_attributes: Option<String>,
}

impl ListQuery {
  /// The 0-based offset and page size this query asks for, within [`MAX_PAGE_SIZE`].
  pub fn window(&self) -> (usize, usize) {
    let start = self.maybe_start_index.unwrap_or(1).max(1);
    let offset = usize::try_from(start - 1).unwrap_or(usize::MAX);
    let count = self
      .maybe_count
      .map_or(MAX_PAGE_SIZE, |count| usize::try_from(count.max(0)).unwrap_or(0));
    (offset, count.min(MAX_PAGE_SIZE))
  }
}

/// Whether `urn` names a core schema (whose attributes sit at the resource's top level).
pub fn is_core_schema(urn: &str) -> bool {
  urn.eq_ignore_ascii_case(USER_SCHEMA) || urn.eq_ignore_ascii_case(GROUP_SCHEMA)
}

/// The same JSON with every object key lower-cased, so that attribute names match whatever case a
/// client wrote them in. Fails beyond [`MAX_JSON_DEPTH`].
pub fn normalize_keys(value: Value) -> Result<Value, ScimInputError> {
  normalize_at(value, 0)
}

fn normalize_at(value: Value, depth: usize) -> Result<Value, ScimInputError> {
  if depth > MAX_JSON_DEPTH {
    return Err(ScimInputError::TooDeep);
  }
  Ok(match value {
    Value::Object(map) => {
      let mut lowered = Map::with_capacity(map.len());
      for (key, value) in map {
        lowered.insert(key.to_lowercase(), normalize_at(value, depth + 1)?);
      }
      Value::Object(lowered)
    },
    Value::Array(items) => Value::Array(
      items
        .into_iter()
        .map(|item| normalize_at(item, depth + 1))
        .collect::<Result<_, _>>()?,
    ),
    other => other,
  })
}

/// Keeps or drops top-level attributes of an answer as `attributes` / `excludedAttributes` ask
/// (RFC 7644 §3.4.2.5). `id` and `schemas` always stay.
pub fn project(resource: &mut Value, maybe_attributes: Option<&str>, maybe_excluded: Option<&str>) {
  let Value::Object(map) = resource else {
    return;
  };
  let names = |list: &str| -> Vec<String> {
    list
      .split(',')
      .map(|name| {
        let name = name.trim();
        // `name.givenName` keeps all of `name`; a URN prefix is dropped.
        let name = name.rsplit(':').next().unwrap_or(name);
        name.split('.').next().unwrap_or(name).to_ascii_lowercase()
      })
      .filter(|name| !name.is_empty())
      .collect()
  };
  let is_kept = |key: &str| matches!(key, "id" | "schemas");
  if let Some(attributes) = maybe_attributes {
    let wanted = names(attributes);
    map.retain(|key, _| is_kept(key) || wanted.contains(&key.to_ascii_lowercase()));
  }
  if let Some(excluded) = maybe_excluded {
    let unwanted = names(excluded);
    map.retain(|key, _| is_kept(key) || !unwanted.contains(&key.to_ascii_lowercase()));
  }
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;

  #[test]
  fn keys_are_lowered_at_every_level() {
    let value =
      normalize_keys(json!({"userName": "a", "Name": {"givenName": "A"}, "emails": [{"Value": "x"}]}))
        .unwrap();
    assert_eq!(
      value,
      json!({"username": "a", "name": {"givenname": "A"}, "emails": [{"value": "x"}]})
    );
  }

  #[test]
  fn deep_json_is_refused() {
    let mut value = json!(1);
    for _ in 0..=MAX_JSON_DEPTH + 1 {
      value = json!([value]);
    }
    assert_eq!(normalize_keys(value), Err(ScimInputError::TooDeep));
  }

  #[test]
  fn the_window_is_one_based_and_bounded() {
    let query = |start: Option<i64>, count: Option<i64>| ListQuery {
      maybe_start_index: start,
      maybe_count: count,
      ..ListQuery::default()
    };
    assert_eq!(query(None, None).window(), (0, MAX_PAGE_SIZE));
    assert_eq!(query(Some(0), Some(10)).window(), (0, 10));
    assert_eq!(query(Some(-5), Some(-1)).window(), (0, 0));
    assert_eq!(query(Some(11), Some(100_000)).window(), (10, MAX_PAGE_SIZE));
  }

  #[test]
  fn projection_keeps_id_and_schemas() {
    let mut group = json!({"schemas": [], "id": "1", "displayName": "a", "members": []});
    project(&mut group, None, Some("members"));
    assert_eq!(group, json!({"schemas": [], "id": "1", "displayName": "a"}));
    project(
      &mut group,
      Some("urn:ietf:params:scim:schemas:core:2.0:Group:externalId"),
      None,
    );
    assert_eq!(group, json!({"schemas": [], "id": "1"}));
  }

  #[test]
  fn error_bodies_have_the_rfc_shape() {
    let body = ScimErrorBody::new(
      400,
      Some(ScimType::InvalidFilter),
      &ScimErrorCode::InvalidFilter,
      "bad".into(),
    );
    assert_eq!(
      serde_json::to_value(&body).unwrap(),
      json!({
        "schemas": [ERROR_SCHEMA],
        "status": "400",
        "scimType": "invalidFilter",
        "detail": "bad",
        "error_code": "scim_invalid_filter",
      })
    );
  }
}
