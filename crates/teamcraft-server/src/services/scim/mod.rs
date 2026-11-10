//! SCIM 2.0 provisioning (docs/provisioning.md §7, RFC 7643/7644): identity providers that push
//! instead of being polled create, change, disable and delete accounts and groups at `/scim/v2`.
//!
//! It is a second source of the same plan as the sync (`oidc.rs`): a create makes the account the
//! first sign-in finds (docs/provisioning.md §6), a disable or delete runs the deactivation sequence
//! with `source: scim` (leavers then leave the encryption groups, §3), group members feed the group
//! mapping (§5), and the safety brake applies per time window.
//!
//! What a SCIM token reaches: the accounts and groups its own client provisioned or linked, with the
//! attributes the client sent itself. No message content, keys, devices or other accounts. It can't
//! give or change a role (accounts it makes are members, the sign-in's token decides the role after
//! that), and it can't deactivate an owner or admin without an owner or admin confirming.

mod auth;
mod clients;
mod discovery;
mod error;
mod groups;
mod users;

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use teamcraft_proto::scim::{SCIM_BASE_PATH, SCIM_MEDIA_TYPE, normalize_keys};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

pub use auth::ScimCaller;
pub use clients::{create_client, delete_client, list_clients, rotate_token};
pub use discovery::{resource_type, resource_types, schema, schemas, service_provider_config};
pub use error::ScimError;
pub use groups::{
  apply_mappings_to_all_groups, create_group, delete_group, get_group, list_groups, patch_group,
  replace_group,
};
pub use users::{
  apply_confirmed_holds, create_user, delete_user, get_user, list_users, patch_user, replace_user,
};

use crate::state::AppState;

/// A SCIM answer: the JSON with `application/scim+json`, and the resource's `ETag` and `Location`.
#[derive(Debug)]
pub struct ScimResponse {
  status: StatusCode,
  body: Value,
  maybe_version: Option<i64>,
  maybe_location: Option<String>,
}

impl ScimResponse {
  pub fn ok(body: Value) -> Self {
    Self {
      status: StatusCode::OK,
      body,
      maybe_version: None,
      maybe_location: None,
    }
  }

  /// 204 with the resource's new `ETag`: what a group PATCH answers (RFC 7644 §3.5.2 allows it), so
  /// a large group is not sent back for every member change.
  fn no_content(version: i64) -> Self {
    Self {
      status: StatusCode::NO_CONTENT,
      body: Value::Null,
      maybe_version: Some(version),
      maybe_location: None,
    }
  }

  fn resource(status: StatusCode, body: Value, version: i64, location: String) -> Self {
    Self {
      status,
      body,
      maybe_version: Some(version),
      maybe_location: Some(location),
    }
  }
}

impl IntoResponse for ScimResponse {
  fn into_response(self) -> Response {
    let mut response = if self.status == StatusCode::NO_CONTENT {
      StatusCode::NO_CONTENT.into_response()
    } else {
      let mut response = (self.status, axum::Json(self.body)).into_response();
      response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(SCIM_MEDIA_TYPE));
      response
    };
    let headers = response.headers_mut();
    if let Some(version) = self.maybe_version
      && let Ok(value) = HeaderValue::from_str(&etag(version))
    {
      headers.insert(header::ETAG, value);
    }
    if self.status == StatusCode::CREATED
      && let Some(location) = self.maybe_location
      && let Ok(value) = HeaderValue::from_str(&location)
    {
      headers.insert(header::LOCATION, value);
    }
    response
  }
}

/// A request body as JSON with its keys lowered, within `[scim] max_body_bytes`.
pub(super) fn parse_body(state: &AppState, body: &[u8]) -> Result<Value, ScimError> {
  let max_bytes = state.settings.get().scim.max_body_bytes;
  if body.len() > max_bytes {
    return Err(ScimError::BodyTooLarge { max_bytes });
  }
  let value: Value =
    serde_json::from_slice(body).map_err(|error| ScimError::InvalidSyntax(error.to_string()))?;
  Ok(normalize_keys(value)?)
}

/// A resource's ETag (RFC 7644 §3.14): weak, its version.
pub(super) fn etag(version: i64) -> String {
  format!("W/\"{version}\"")
}

/// What `If-Match` asks for: `None` for no header or `*`, else the version it names. A tag that is
/// not one of ours names no version, so it never matches.
pub(super) fn expected_version(headers: &HeaderMap) -> Option<i64> {
  let value = headers.get(header::IF_MATCH)?.to_str().ok()?.trim();
  if value == "*" {
    return None;
  }
  Some(parse_etag(value).unwrap_or(-1))
}

/// Whether `If-None-Match` names the current version (a GET then answers 304).
pub(super) fn is_not_modified(headers: &HeaderMap, version: i64) -> bool {
  headers
    .get(header::IF_NONE_MATCH)
    .and_then(|value| value.to_str().ok())
    .is_some_and(|value| {
      value
        .split(',')
        .any(|tag| tag.trim() == "*" || parse_etag(tag.trim()) == Some(version))
    })
}

fn parse_etag(tag: &str) -> Option<i64> {
  tag.trim_start_matches("W/").trim_matches('"').parse().ok()
}

/// `meta` of a resource.
pub(super) fn meta(
  state: &AppState,
  kind: &str,
  id: &str,
  created: OffsetDateTime,
  modified: OffsetDateTime,
  version: i64,
) -> Value {
  serde_json::json!({
    "resourceType": kind,
    "created": rfc3339(created),
    "lastModified": rfc3339(modified),
    "location": location(state, kind, id),
    "version": etag(version),
  })
}

/// The URL of a resource: absolute when the server knows its public address.
pub(super) fn location(state: &AppState, kind: &str, id: &str) -> String {
  let collection = if kind == "Group" { "Groups" } else { "Users" };
  let base = state
    .settings
    .get()
    .mail
    .public_base_url
    .trim_end_matches('/')
    .to_owned();
  format!("{base}{SCIM_BASE_PATH}/{collection}/{id}")
}

fn rfc3339(at: OffsetDateTime) -> String {
  at.format(&Rfc3339).unwrap_or_default()
}

/// Answer of a GET whose `If-None-Match` matched.
pub(super) fn not_modified(version: i64) -> Response {
  let mut response = StatusCode::NOT_MODIFIED.into_response();
  if let Ok(value) = HeaderValue::from_str(&etag(version)) {
    response.headers_mut().insert(header::ETAG, value);
  }
  response
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn if_match_names_a_version_or_anything() {
    let headers = |value: &str| {
      let mut headers = HeaderMap::new();
      headers.insert(header::IF_MATCH, HeaderValue::from_str(value).unwrap());
      headers
    };
    assert_eq!(expected_version(&HeaderMap::new()), None);
    assert_eq!(expected_version(&headers("*")), None);
    assert_eq!(expected_version(&headers("W/\"7\"")), Some(7));
    assert_eq!(expected_version(&headers("\"7\"")), Some(7));
    assert_eq!(expected_version(&headers("W/\"other\"")), Some(-1));
  }

  #[test]
  fn if_none_match_is_a_list() {
    let mut headers = HeaderMap::new();
    headers.insert(
      header::IF_NONE_MATCH,
      HeaderValue::from_static("W/\"1\", W/\"3\""),
    );
    assert!(is_not_modified(&headers, 3));
    assert!(!is_not_modified(&headers, 2));
  }
}
