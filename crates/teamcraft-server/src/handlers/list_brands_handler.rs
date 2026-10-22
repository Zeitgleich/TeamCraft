use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListBrandsResponse};

use crate::auth::AuthenticatedUser;
use crate::state::AppState;

/// Lists the brands of this server (for tagging spaces).
#[utoipa::path(get, path = "/api/v1/brands", tag = "brand", summary = "Lists the brands of this server.", responses((status = 200, body = ListBrandsResponse), (status = 401, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_brands_handler(
  State(state): State<AppState>,
  _caller: AuthenticatedUser,
) -> Json<ListBrandsResponse> {
  Json(ListBrandsResponse {
    brands: state.brands.summaries(),
  })
}
