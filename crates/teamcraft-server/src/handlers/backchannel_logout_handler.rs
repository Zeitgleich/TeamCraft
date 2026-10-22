use axum::Form;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::error::ApiError;
use crate::services::oidc;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct BackchannelLogoutForm {
  logout_token: String,
}

/// `POST /api/v1/auth/backchannel-logout`: OpenID Connect Back-Channel Logout 1.0. The identity
/// provider posts a signed logout token (form field `logout_token`); there is no bearer token, the
/// signature is the authentication. Register this URL at the app client in the provider.
pub async fn backchannel_logout_handler(
  State(state): State<AppState>,
  Form(form): Form<BackchannelLogoutForm>,
) -> Result<Response, ApiError> {
  oidc::backchannel_logout(&state, &form.logout_token).await?;
  let mut response = StatusCode::OK.into_response();
  // Required by the specification.
  response
    .headers_mut()
    .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
  Ok(response)
}
