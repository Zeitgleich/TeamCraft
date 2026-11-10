use axum::extract::Path;

use crate::services::scim::{self, ScimCaller, ScimError, ScimResponse};

/// `GET /scim/v2/ResourceTypes/{id}`.
pub async fn scim_get_resource_type_handler(
  _caller: ScimCaller,
  Path(id): Path<String>,
) -> Result<ScimResponse, ScimError> {
  scim::resource_type(&id)
}
