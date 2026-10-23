use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use teamcraft_proto::{
  BrandAssetSlot, ErrorBody, SERVER_INFO_PRODUCT, ServerInfoResponse, SignInInfo, SignInMethod,
};

use super::get_brand_handler::set_public_cache;
use super::get_client_compatibility_handler::MINIMUM_CLIENT_VERSION;
use crate::brand::ResolvedBrand;
use crate::services::local_accounts;
use crate::state::AppState;

/// Tells a native app what it needs before sign-in: that this is a TeamCraft server, which
/// versions work together, the brand of the host and how to sign in (docs/operations.md "Server
/// address in the apps"). Public and without secrets.
#[utoipa::path(get, path = "/api/v1/server-info", tag = "health", summary = "Returns what an app needs to sign in to this server.", responses((status = 200, body = ServerInfoResponse), (status = 421, body = ErrorBody)))]
pub async fn get_server_info_handler(State(state): State<AppState>, resolved: ResolvedBrand) -> Response {
  let settings = state.settings.get();
  let sign_in = &settings.sign_in;
  let has_local_accounts = local_accounts::is_available(&state);
  let brand = &resolved.brand;
  let body = ServerInfoResponse {
    product: SERVER_INFO_PRODUCT.to_owned(),
    server_version: env!("CARGO_PKG_VERSION").to_owned(),
    minimum_client_version: MINIMUM_CLIENT_VERSION.to_owned(),
    brand: brand.response.clone(),
    sign_in: SignInInfo {
      issuer: sign_in.issuer.clone(),
      client_id: sign_in.client_id.clone(),
      scopes: sign_in.scopes.clone(),
      maybe_redirect_uris: sign_in.maybe_redirect_uris.clone(),
      has_local_accounts,
      local_requires_code: settings.local_accounts.require_totp || state.policy.get().session.requires_mfa,
      methods: offered_methods(&settings.sign_in_page.methods, has_local_accounts),
      maybe_sso_label: brand.config.maybe_sso_label.clone(),
      maybe_sso_icon: brand
        .response
        .assets
        .get(BrandAssetSlot::SsoIcon.as_str())
        .cloned(),
      help_text: settings.sign_in_page.help_text.clone(),
      maybe_help_link: settings.sign_in_page.maybe_help_link.clone(),
    },
  };
  let mut response = Json(body).into_response();
  set_public_cache(&mut response);
  response
}

/// The configured methods that work now, in their order; the identity provider when nothing is
/// left (it always works).
fn offered_methods(configured: &[SignInMethod], has_local_accounts: bool) -> Vec<SignInMethod> {
  let mut methods: Vec<SignInMethod> = configured
    .iter()
    .filter(|method| match method {
      SignInMethod::Sso => true,
      SignInMethod::Local => has_local_accounts,
      SignInMethod::Unknown(_) => false,
    })
    .cloned()
    .collect();
  if methods.is_empty() {
    methods.push(SignInMethod::Sso);
  }
  methods
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn local_shows_only_while_available_and_sso_is_the_fallback() {
    let both = [SignInMethod::Local, SignInMethod::Sso];
    assert_eq!(offered_methods(&both, true), both);
    assert_eq!(offered_methods(&both, false), [SignInMethod::Sso]);
    assert_eq!(
      offered_methods(&[SignInMethod::Local], false),
      [SignInMethod::Sso]
    );
    assert_eq!(offered_methods(&[SignInMethod::Sso], true), [SignInMethod::Sso]);
  }
}
