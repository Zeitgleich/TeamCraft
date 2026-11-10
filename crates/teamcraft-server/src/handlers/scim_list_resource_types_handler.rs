use crate::services::scim::{self, ScimCaller, ScimResponse};

/// `GET /scim/v2/ResourceTypes`: users and groups.
pub async fn scim_list_resource_types_handler(_caller: ScimCaller) -> ScimResponse {
  scim::resource_types()
}
