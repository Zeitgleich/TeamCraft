//! Meetings, their lobby and guest links (docs/meetings.md), and the mail inbox's internal
//! replies.

use axum::routing::post;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use teamcraft_proto::{JoinMeetingRequest, JoinMeetingResponse, ListMeetingsQuery, MeetingReplyResponse};

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::with_openapi(Schemas::openapi())
    .routes(routes!(handlers::list_meetings_handler, handlers::create_meeting_handler))
    .routes(routes!(handlers::create_instant_meeting_handler))
    .routes(routes!(
      handlers::get_meeting_handler,
      handlers::update_meeting_handler,
      handlers::cancel_meeting_handler
    ))
    .routes(routes!(handlers::control_meeting_handler))
    .routes(routes!(handlers::create_guest_link_handler))
    .routes(routes!(handlers::leave_meeting_handler))
    .routes(routes!(handlers::list_lobby_handler))
    .routes(routes!(handlers::get_my_lobby_state_handler, handlers::knock_meeting_handler))
    .routes(routes!(handlers::put_guest_proof_handler))
    .routes(routes!(handlers::get_lobby_entry_handler))
    .routes(routes!(handlers::admit_guest_handler))
    .routes(routes!(handlers::deny_guest_handler))
    .routes(routes!(handlers::set_meeting_rsvp_handler))
    .routes(routes!(handlers::get_shared_guest_link_handler, handlers::revoke_shared_guest_link_handler))
    // Annotated but not part of the OpenAPI document.
    .route("/api/v1/meetings/join", post(handlers::join_meeting_handler))
    // The mail inbox's internal API (its own bearer token): not part of the public OpenAPI document.
    .route("/api/internal/v1/meetings/replies", post(handlers::receive_meeting_reply_handler))
}

/// Schemas in the document that no documented route names: query parameters, joining by link and
/// the inbox's replies.
#[derive(OpenApi)]
#[openapi(components(schemas(
  JoinMeetingRequest,
  JoinMeetingResponse,
  ListMeetingsQuery,
  MeetingReplyResponse
)))]
struct Schemas;
