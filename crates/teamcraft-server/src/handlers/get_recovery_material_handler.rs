use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, RecoveryMaterialResponse, RecoveryRequestId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::org_recovery;
use crate::state::AppState;

/// The combining admin fetches the shares and the wrapped backup keys.
#[utoipa::path(get, path = "/api/v1/org-recovery/requests/{request_id}/material", tag = "org recovery", summary = "The combining admin fetches the shares and the wrapped backup keys.", params(("request_id" = RecoveryRequestId, Path)), responses((status = 200, body = RecoveryMaterialResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_recovery_material_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(request_id): ApiPath<RecoveryRequestId>,
) -> Result<Json<RecoveryMaterialResponse>, ApiError> {
  let response = org_recovery::material(&state, &caller, request_id).await?;
  Ok(Json(response))
}
