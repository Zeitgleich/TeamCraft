use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use teamcraft_proto::scim::ListQuery;

use crate::services::scim::{self, ScimCaller, ScimError, ScimResponse};
use crate::state::AppState;

/// `GET /scim/v2/Groups`: a page of the client's groups, filtered (docs/provisioning.md §7).
pub async fn scim_list_groups_handler(
  State(state): State<AppState>,
  caller: ScimCaller,
  query: Result<Query<ListQuery>, QueryRejection>,
) -> Result<ScimResponse, ScimError> {
  let Query(query) = query.map_err(|rejection| ScimError::InvalidSyntax(rejection.body_text()))?;
  scim::list_groups(&state, &caller, &query).await
}
