//! The session of a meeting guest: the server's session token from the join link, nothing else.
//! There is no sign-in, no refresh and no identity provider; the token ends with the meeting
//! (docs/meetings.md, "Guests in the browser").

use super::fetch::fetch;
use super::{
  ApiBytes, ApiMethod, ApiResponse, AppConfig, AuthError, Authenticator, BinaryBody, LocalFuture, SignInLook,
};

/// The guest's token as an [`Authenticator`], so the same client core and chat views run for a guest.
pub struct GuestAuth {
  config: AppConfig,
  token: String,
}

impl GuestAuth {
  pub fn new(config: AppConfig, token: String) -> Self {
    Self { config, token }
  }
}

impl Authenticator for GuestAuth {
  fn restore(&self) -> LocalFuture<'_, Result<bool, AuthError>> {
    Box::pin(async { Ok(true) })
  }

  fn login(&self, _look: SignInLook) -> LocalFuture<'_, Result<(), AuthError>> {
    Box::pin(async { Err(AuthError::Unsupported("guests do not sign in".to_owned())) })
  }

  fn logout(&self) -> LocalFuture<'_, Result<(), AuthError>> {
    Box::pin(async { Ok(()) })
  }

  fn api_request(
    &self,
    method: ApiMethod,
    path: &str,
    maybe_body: Option<String>,
  ) -> LocalFuture<'_, Result<ApiResponse, AuthError>> {
    let url = self.config.api_url(path);
    Box::pin(async move {
      let mut headers = vec![
        ("Authorization".to_owned(), format!("Bearer {}", self.token)),
        ("Accept".to_owned(), "application/json".to_owned()),
      ];
      if maybe_body.is_some() {
        headers.push(("Content-Type".to_owned(), "application/json".to_owned()));
      }
      let response = fetch(
        method.as_str(),
        &url,
        &headers,
        maybe_body.map(String::into_bytes),
      )
      .await
      .map_err(AuthError::Network)?;
      Ok(ApiResponse {
        status: response.status,
        body: String::from_utf8_lossy(&response.body).into_owned(),
      })
    })
  }

  fn api_request_bytes(
    &self,
    method: ApiMethod,
    path: &str,
    maybe_body: Option<BinaryBody>,
  ) -> LocalFuture<'_, Result<ApiBytes, AuthError>> {
    let url = self.config.api_url(path);
    Box::pin(async move {
      let mut headers = vec![("Authorization".to_owned(), format!("Bearer {}", self.token))];
      if let Some(body) = &maybe_body {
        headers.push(("Content-Type".to_owned(), body.content_type.clone()));
      }
      let response = fetch(method.as_str(), &url, &headers, maybe_body.map(|body| body.bytes))
        .await
        .map_err(AuthError::Network)?;
      Ok(ApiBytes {
        status: response.status,
        maybe_content_type: response.maybe_content_type,
        body: response.body,
      })
    })
  }

  fn access_token(&self) -> LocalFuture<'_, Result<String, AuthError>> {
    Box::pin(async { Ok(self.token.clone()) })
  }
}

impl std::fmt::Debug for GuestAuth {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("GuestAuth").field("token", &"[redacted]").finish()
  }
}
