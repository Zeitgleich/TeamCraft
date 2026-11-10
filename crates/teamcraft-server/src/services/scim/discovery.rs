//! What the endpoint supports (RFC 7644 §4): `ServiceProviderConfig`, `ResourceTypes` and
//! `Schemas`. Identity providers read them to decide what to send; they need a token like any
//! other SCIM request, so nobody learns from them that SCIM is set up.

use serde_json::{Value, json};
use teamcraft_proto::scim::{
  ENTERPRISE_USER_SCHEMA, GROUP_SCHEMA, ListResponse, MAX_PAGE_SIZE, RESOURCE_TYPE_SCHEMA, SCHEMA_SCHEMA,
  SERVICE_PROVIDER_CONFIG_SCHEMA, USER_SCHEMA,
};

use super::ScimResponse;
use super::error::ScimError;
use crate::state::AppState;

const DOCUMENTATION: &str = "https://github.com/Zeitgleich/teamcraft/blob/main/docs/provisioning.md";

pub fn service_provider_config(state: &AppState) -> ScimResponse {
  let max_body_bytes = state.settings.get().scim.max_body_bytes;
  ScimResponse::ok(json!({
    "schemas": [SERVICE_PROVIDER_CONFIG_SCHEMA],
    "documentationUri": DOCUMENTATION,
    "patch": {"supported": true},
    "bulk": {"supported": false, "maxOperations": 0, "maxPayloadSize": 0},
    "filter": {"supported": true, "maxResults": MAX_PAGE_SIZE},
    "changePassword": {"supported": false},
    "sort": {"supported": false},
    "etag": {"supported": true},
    "authenticationSchemes": [{
      "type": "oauthbearertoken",
      "name": "Bearer token",
      "description": "A token made for this client in the TeamCraft admin console",
      "primary": true,
    }],
    "maxPayloadSize": max_body_bytes,
    "meta": {"resourceType": "ServiceProviderConfig"},
  }))
}

pub fn resource_types() -> ScimResponse {
  let types = vec![user_type(), group_type()];
  ScimResponse::ok(serde_json::to_value(ListResponse::new(types.len(), 1, types)).unwrap_or_default())
}

pub fn resource_type(id: &str) -> Result<ScimResponse, ScimError> {
  match id {
    "User" => Ok(ScimResponse::ok(user_type())),
    "Group" => Ok(ScimResponse::ok(group_type())),
    _ => Err(ScimError::NotFound),
  }
}

pub fn schemas() -> ScimResponse {
  let schemas = vec![user_schema(), group_schema()];
  ScimResponse::ok(serde_json::to_value(ListResponse::new(schemas.len(), 1, schemas)).unwrap_or_default())
}

pub fn schema(id: &str) -> Result<ScimResponse, ScimError> {
  if id.eq_ignore_ascii_case(USER_SCHEMA) {
    Ok(ScimResponse::ok(user_schema()))
  } else if id.eq_ignore_ascii_case(GROUP_SCHEMA) {
    Ok(ScimResponse::ok(group_schema()))
  } else {
    Err(ScimError::NotFound)
  }
}

fn user_type() -> Value {
  json!({
    "schemas": [RESOURCE_TYPE_SCHEMA],
    "id": "User",
    "name": "User",
    "endpoint": "/Users",
    "schema": USER_SCHEMA,
    // Accepted and not kept, so providers that send it don't fail.
    "schemaExtensions": [{"schema": ENTERPRISE_USER_SCHEMA, "required": false}],
    "meta": {"resourceType": "ResourceType", "location": "/ResourceTypes/User"},
  })
}

fn group_type() -> Value {
  json!({
    "schemas": [RESOURCE_TYPE_SCHEMA],
    "id": "Group",
    "name": "Group",
    "endpoint": "/Groups",
    "schema": GROUP_SCHEMA,
    "meta": {"resourceType": "ResourceType", "location": "/ResourceTypes/Group"},
  })
}

/// One attribute of a schema.
fn attribute(name: &str, kind: &str, is_required: bool, mutability: &str, uniqueness: &str) -> Value {
  json!({
    "name": name,
    "type": kind,
    "multiValued": false,
    "required": is_required,
    "caseExact": false,
    "mutability": mutability,
    "returned": "default",
    "uniqueness": uniqueness,
  })
}

fn user_schema() -> Value {
  let mut emails = attribute("emails", "complex", false, "readWrite", "none");
  emails["multiValued"] = json!(true);
  emails["subAttributes"] = json!([
    attribute("value", "string", false, "readWrite", "none"),
    attribute("type", "string", false, "readWrite", "none"),
    attribute("primary", "boolean", false, "readWrite", "none"),
  ]);
  let mut name = attribute("name", "complex", false, "readWrite", "none");
  name["subAttributes"] = json!([
    attribute("givenName", "string", false, "readWrite", "none"),
    attribute("familyName", "string", false, "readWrite", "none"),
    attribute("formatted", "string", false, "readOnly", "none"),
  ]);
  let mut groups = attribute("groups", "complex", false, "readOnly", "none");
  groups["multiValued"] = json!(true);
  let mut external_id = attribute("externalId", "string", false, "readWrite", "none");
  external_id["caseExact"] = json!(true);
  json!({
    "schemas": [SCHEMA_SCHEMA],
    "id": USER_SCHEMA,
    "name": "User",
    "description": "A TeamCraft account",
    "attributes": [
      attribute("userName", "string", true, "readWrite", "server"),
      external_id,
      name,
      attribute("displayName", "string", false, "readWrite", "none"),
      emails,
      attribute("active", "boolean", false, "readWrite", "none"),
      groups,
    ],
    "meta": {"resourceType": "Schema", "location": format!("/Schemas/{USER_SCHEMA}")},
  })
}

fn group_schema() -> Value {
  let mut members = attribute("members", "complex", false, "readWrite", "none");
  members["multiValued"] = json!(true);
  members["subAttributes"] = json!([
    attribute("value", "string", false, "immutable", "none"),
    attribute("display", "string", false, "readOnly", "none"),
  ]);
  json!({
    "schemas": [SCHEMA_SCHEMA],
    "id": GROUP_SCHEMA,
    "name": "Group",
    "description": "A group whose members the group mapping adds to spaces and channels",
    "attributes": [
      attribute("displayName", "string", true, "readWrite", "server"),
      attribute("externalId", "string", false, "readWrite", "none"),
      members,
    ],
    "meta": {"resourceType": "Schema", "location": format!("/Schemas/{GROUP_SCHEMA}")},
  })
}
