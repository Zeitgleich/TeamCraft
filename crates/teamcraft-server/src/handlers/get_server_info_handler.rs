use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use teamcraft_proto::{ErrorBody, SERVER_INFO_PRODUCT, ServerInfoResponse, SignInInfo};

use super::get_brand_handler::set_public_cache;
use super::get_client_compatibility_handler::MINIMUM_CLIENT_VERSION;
use crate::brand::ResolvedBrand;
use crate::state::AppState;

/// Tells a native app what it needs before sign-in: that this is a TeamCraft server, which
/// versions work together, the brand of the host and how to sign in (docs/operations.md "Server
/// address in the apps"). Public and without secrets.
#[utoipa::path(get, path = "/api/v1/server-info", tag = "health", summary = "Returns what an app needs to sign in to this server.", responses((status = 200, body = ServerInfoResponse), (status = 421, body = ErrorBody)))]
pub async fn get_server_info_handler(State(state): State<AppState>, resolved: ResolvedBrand) -> Response {
  let settings = state.settings.get();
  let sign_in = &settings.sign_in;
  let body = ServerInfoResponse {
    product: SERVER_INFO_PRODUCT.to_owned(),
    server_version: env!("CARGO_PKG_VERSION").to_owned(),
    minimum_client_version: MINIMUM_CLIENT_VERSION.to_owned(),
    brand: resolved.brand.response.clone(),
    sign_in: SignInInfo {
      issuer: sign_in.issuer.clone(),
      client_id: sign_in.client_id.clone(),
      scopes: sign_in.scopes.clone(),
      maybe_redirect_uris: sign_in.maybe_redirect_uris.clone(),
      has_local_accounts: state.maybe_local.is_some(),
    },
  };
  let mut response = Json(body).into_response();
  set_public_cache(&mut response);
  response
}
