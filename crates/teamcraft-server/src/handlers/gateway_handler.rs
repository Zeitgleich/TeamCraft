use axum::Extension;
use axum::extract::{State, WebSocketUpgrade};
use axum::response::Response;

use crate::gateway::connection;
use crate::security::ClientIp;
use crate::state::AppState;

/// `GET /api/v1/gateway`: upgrades to the realtime WebSocket (frames: `teamcraft_proto::gateway`).
/// Authentication happens in the first frame, not in the URL or headers.
pub async fn gateway_handler(
  State(state): State<AppState>,
  Extension(ClientIp(ip)): Extension<ClientIp>,
  upgrade: WebSocketUpgrade,
) -> Response {
  let max_frame_bytes = state.settings.gateway.max_frame_bytes;
  upgrade
    .max_message_size(max_frame_bytes)
    .on_upgrade(move |socket| connection::run(state, socket, ip))
}
