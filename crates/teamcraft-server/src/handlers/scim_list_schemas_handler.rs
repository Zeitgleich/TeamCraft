use crate::services::scim::{self, ScimCaller, ScimResponse};

/// `GET /scim/v2/Schemas`: the attributes TeamCraft keeps of users and groups.
pub async fn scim_list_schemas_handler(_caller: ScimCaller) -> ScimResponse {
  scim::schemas()
}
