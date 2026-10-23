use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListBrandsResponse};

use crate::auth::AuthenticatedUser;
use crate::services::access::is_guest;
use crate::state::AppState;

/// Lists the brands of this server (for tagging spaces). Guests get an empty list.
#[utoipa::path(get, path = "/api/v1/brands", tag = "brand", summary = "Lists the brands of this server.", responses((status = 200, body = ListBrandsResponse), (status = 401, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_brands_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Json<ListBrandsResponse> {
  // Guests get no roster of the workspace's companies, like they get no directory.
  let brands = if is_guest(&caller) {
    Vec::new()
  } else {
    state.brands.current().summaries()
  };
  Json(ListBrandsResponse { brands })
}
