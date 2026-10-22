use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{CreateGuestLinkRequest, ErrorBody, GuestLinkResponse, MeetingId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::meeting_guests;
use crate::state::AppState;

/// Creates a personal one-time join link for a guest (host only).
#[utoipa::path(post, path = "/api/v1/meetings/{meeting_id}/guest-links", tag = "meetings", summary = "Creates a personal one-time join link for a guest (host only).", request_body = CreateGuestLinkRequest, params(("meeting_id" = MeetingId, Path)), responses((status = 201, body = GuestLinkResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn create_guest_link_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(meeting_id): ApiPath<MeetingId>,
  ApiJson(request): ApiJson<CreateGuestLinkRequest>,
) -> Result<(StatusCode, Json<GuestLinkResponse>), ApiError> {
  let response = meeting_guests::create_guest_link(&state, &caller, meeting_id, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
