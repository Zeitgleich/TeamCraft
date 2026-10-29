//! Calls: state, join tokens, moving and leaving, history, and the SFU's internal reports.

use axum::routing::post;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use teamcraft_proto::{ListCallHistoryQuery, SfuCallRoster, SfuParticipantReport, SfuRosterReport};

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::with_openapi(Schemas::openapi())
    .routes(routes!(handlers::list_call_history_handler))
    .routes(routes!(handlers::get_call_handler))
    .routes(routes!(handlers::create_call_join_token_handler))
    .routes(routes!(handlers::leave_call_everywhere_handler))
    .routes(routes!(handlers::move_call_handler))
    // The notetaker's API key, for its bot account (e2ee.md §8.2).
    .routes(routes!(handlers::get_notetaker_credentials_handler))
    // The SFU's internal API (its own bearer token): not part of the public OpenAPI document.
    .route("/api/internal/v1/calls/participants/joined", post(handlers::report_participant_joined_handler))
    .route("/api/internal/v1/calls/participants/left", post(handlers::report_participant_left_handler))
    .route("/api/internal/v1/calls/roster", post(handlers::report_call_roster_handler))
}

/// Schemas in the document that no documented route names: query parameters and the SFU's reports.
#[derive(OpenApi)]
#[openapi(components(schemas(ListCallHistoryQuery, SfuCallRoster, SfuParticipantReport, SfuRosterReport)))]
struct Schemas;
