//! Encrypted attachments: uploads (single and multipart) and downloads.

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::new()
    .routes(routes!(handlers::create_attachment_handler))
    .routes(routes!(handlers::create_multipart_attachment_handler))
    .routes(routes!(handlers::abandon_attachment_handler))
    .routes(routes!(handlers::complete_attachment_handler))
    .routes(routes!(handlers::download_attachment_handler))
    .routes(routes!(handlers::complete_multipart_attachment_handler))
    .routes(routes!(handlers::presign_attachment_part_handler))
}
