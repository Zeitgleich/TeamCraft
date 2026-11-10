use axum::extract::Path;

use crate::services::scim::{self, ScimCaller, ScimError, ScimResponse};

/// `GET /scim/v2/Schemas/{id}`.
pub async fn scim_get_schema_handler(
  _caller: ScimCaller,
  Path(id): Path<String>,
) -> Result<ScimResponse, ScimError> {
  scim::schema(&id)
}
