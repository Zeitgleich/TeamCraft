//! Typed REST calls for the server-owned parts of the app (profiles, user groups, meeting controls):
//! the same shape as the meetings client, for data that is not end-to-end encrypted content.

use std::rc::Rc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use teamcraft_proto::{ErrorBody, ErrorCode};

use crate::auth::{ApiMethod, ApiResponse, AuthError, Authenticator};
use crate::feedback::{ActionFailure, FailureKind};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RestError {
  #[error("{0}")]
  Auth(#[from] AuthError),
  #[error("the server refused the request ({status}): {code}")]
  Refused { status: u16, code: String },
  #[error("unreadable response: {0}")]
  Unreadable(String),
}

/// Authenticated calls of a signed-in member.
#[derive(Clone)]
pub struct RestClient {
  auth: Rc<dyn Authenticator>,
}

impl RestError {
  /// The failure to show the user (issue #322): the server's code, a network error, or other.
  pub fn to_failure(&self) -> ActionFailure {
    let kind = match self {
      Self::Auth(AuthError::Network(_)) => FailureKind::Network,
      Self::Refused { code, .. } => match ErrorCode::from(code.clone()) {
        ErrorCode::Unknown(_) => FailureKind::Other,
        known => FailureKind::Server(known),
      },
      Self::Auth(_) | Self::Unreadable(_) => FailureKind::Other,
    };
    ActionFailure::new(kind, self.to_string())
  }
}

impl RestClient {
  pub fn new(auth: Rc<dyn Authenticator>) -> Self {
    Self { auth }
  }

  pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, RestError> {
    self.call(ApiMethod::Get, path, None).await
  }

  pub async fn put<T: DeserializeOwned>(&self, path: &str, body: &impl Serialize) -> Result<T, RestError> {
    self.call(ApiMethod::Put, path, Some(to_json(body)?)).await
  }

  pub async fn post<T: DeserializeOwned>(&self, path: &str, body: &impl Serialize) -> Result<T, RestError> {
    self.call(ApiMethod::Post, path, Some(to_json(body)?)).await
  }

  pub async fn patch<T: DeserializeOwned>(&self, path: &str, body: &impl Serialize) -> Result<T, RestError> {
    self.call(ApiMethod::Patch, path, Some(to_json(body)?)).await
  }

  /// A request whose answer has no body (204).
  pub async fn send(
    &self,
    method: ApiMethod,
    path: &str,
    maybe_body: Option<String>,
  ) -> Result<(), RestError> {
    check(self.auth.api_request(method, path, maybe_body).await?).map(|_| ())
  }

  async fn call<T: DeserializeOwned>(
    &self,
    method: ApiMethod,
    path: &str,
    maybe_body: Option<String>,
  ) -> Result<T, RestError> {
    let response = check(self.auth.api_request(method, path, maybe_body).await?)?;
    serde_json::from_str(&response.body).map_err(|error| RestError::Unreadable(error.to_string()))
  }
}

pub fn to_json<T: Serialize + ?Sized>(value: &T) -> Result<String, RestError> {
  serde_json::to_string(value).map_err(|error| RestError::Unreadable(error.to_string()))
}

/// The response if it succeeded, otherwise the refusal with the server's `error_code`.
pub(crate) fn check(response: ApiResponse) -> Result<ApiResponse, RestError> {
  if response.status < 300 {
    return Ok(response);
  }
  let code = serde_json::from_str::<ErrorBody>(&response.body)
    .map(|body| body.error_code.as_str().to_owned())
    .unwrap_or_else(|_| "unknown".to_owned());
  Err(RestError::Refused {
    status: response.status,
    code,
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn refusals_carry_the_stable_error_code() {
    let body = r#"{"error_code":"forbidden","message":"no"}"#;
    let outcome = check(ApiResponse {
      status: 403,
      body: body.into(),
    });
    assert_eq!(
      outcome,
      Err(RestError::Refused {
        status: 403,
        code: "forbidden".into()
      })
    );
    assert!(
      check(ApiResponse {
        status: 204,
        body: String::new()
      })
      .is_ok()
    );
  }

  #[test]
  fn failures_name_the_cause_for_the_toast() {
    let refused = RestError::Refused {
      status: 403,
      code: "forbidden".into(),
    };
    assert_eq!(
      refused.to_failure().kind,
      FailureKind::Server(ErrorCode::Forbidden)
    );
    let odd = RestError::Refused {
      status: 400,
      code: "avatar_upload_failed".into(),
    };
    assert_eq!(odd.to_failure().kind, FailureKind::Other);
    let offline = RestError::Auth(AuthError::Network("down".into()));
    assert_eq!(offline.to_failure().kind, FailureKind::Network);
    assert_eq!(
      RestError::Unreadable("x".into()).to_failure().kind,
      FailureKind::Other
    );
  }
}
