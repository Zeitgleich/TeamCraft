//! Request extractors whose rejections are `ErrorBody` responses instead of plain text.

use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use serde::de::DeserializeOwned;

use crate::error::ApiError;

/// JSON request body.
pub struct ApiJson<T>(pub T);

/// URL path parameters.
pub struct ApiPath<T>(pub T);

/// URL query string.
pub struct ApiQuery<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for ApiJson<T> {
  type Rejection = ApiError;

  async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
    axum::Json::<T>::from_request(request, state)
      .await
      .map(|axum::Json(value)| Self(value))
      .map_err(|rejection: JsonRejection| match rejection.status() {
        axum::http::StatusCode::PAYLOAD_TOO_LARGE => {
          ApiError::PayloadTooLarge("request body is too large".into())
        },
        _ => ApiError::InvalidRequest(rejection.body_text()),
      })
  }
}

impl<S: Send + Sync, T: DeserializeOwned + Send> FromRequestParts<S> for ApiPath<T> {
  type Rejection = ApiError;

  async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
    axum::extract::Path::<T>::from_request_parts(parts, state)
      .await
      .map(|axum::extract::Path(value)| Self(value))
      .map_err(|rejection: PathRejection| ApiError::InvalidRequest(rejection.body_text()))
  }
}

impl<S: Send + Sync, T: DeserializeOwned> FromRequestParts<S> for ApiQuery<T> {
  type Rejection = ApiError;

  async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
    axum::extract::Query::<T>::from_request_parts(parts, state)
      .await
      .map(|axum::extract::Query(value)| Self(value))
      .map_err(|rejection: QueryRejection| ApiError::InvalidRequest(rejection.body_text()))
  }
}

/// The `Authorization: Bearer` value of an internal request, if there is one.
pub struct BearerToken(pub Option<String>);

impl<S: Send + Sync> FromRequestParts<S> for BearerToken {
  type Rejection = ApiError;

  async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
    let maybe_token = parts
      .headers
      .get(axum::http::header::AUTHORIZATION)
      .and_then(|value| value.to_str().ok())
      .and_then(|value| value.strip_prefix("Bearer "))
      .map(str::to_owned);
    Ok(Self(maybe_token))
  }
}
