use axum::Extension;
use axum::extract::{State, WebSocketUpgrade};
use axum::response::Response;

use crate::rendezvous::connection;
use crate::security::ClientIp;
use crate::state::AppState;

/// `GET /api/v1/rendezvous`: upgrades to the device-linking relay (frames: `teamcraft_proto::rendezvous`).
/// The first frame authenticates and names the rendezvous; later binary frames are forwarded to the
/// other device of the same user.
pub async fn rendezvous_handler(
  State(state): State<AppState>,
  Extension(ClientIp(ip)): Extension<ClientIp>,
  upgrade: WebSocketUpgrade,
) -> Response {
  let max_frame_bytes = state
    .settings
    .get()
    .rendezvous
    .max_frame_bytes
    .max(state.settings.get().gateway.max_frame_bytes);
  upgrade
    .max_message_size(max_frame_bytes)
    .on_upgrade(move |socket| connection::run(state, socket, ip))
}
