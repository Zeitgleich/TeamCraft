use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use teamcraft_proto::scim::ListQuery;

use crate::services::scim::{self, ScimCaller, ScimError};
use crate::state::AppState;

/// `GET /scim/v2/Groups/{id}`: one group of the client; 304 when `If-None-Match` names its version.
pub async fn scim_get_group_handler(
  State(state): State<AppState>,
  caller: ScimCaller,
  Path(id): Path<String>,
  headers: HeaderMap,
  query: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Response, ScimError> {
  let Query(query) = query.map_err(|rejection| ScimError::InvalidSyntax(rejection.body_text()))?;
  scim::get_group(&state, &caller, &id, &headers, &query).await
}
