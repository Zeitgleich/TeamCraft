use crate::services::scim::ScimError;

/// Any other path below `/scim/v2`: a SCIM 404, not the web client's page.
pub async fn scim_unknown_path_handler() -> ScimError {
  ScimError::NotFound
}
