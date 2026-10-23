use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use teamcraft_proto::{ErrorBody, ErrorCode, RendezvousClientMessage, RendezvousServerMessage};
use tokio::time::{Instant, sleep_until, timeout};
use tracing::debug;

use super::hub::{JoinError, RelayError, Signal};
use crate::services::accounts;
use crate::state::AppState;

const CLOSE_POLICY: u16 = 1008;
const CLOSE_NORMAL: u16 = 1000;
const ID_HEX_CHARS: usize = 32;

/// Runs one device's side of a rendezvous: join, relay binary frames, end.
pub async fn run(state: AppState, mut socket: WebSocket, ip: String) {
  let config = state.settings.get().rendezvous.clone();
  let first = timeout(
    Duration::from_secs(config.join_timeout_secs),
    next_text(&mut socket),
  )
  .await;
  let Ok(Some(RendezvousClientMessage::Join { token, rendezvous_id })) = first else {
    close(&mut socket, CLOSE_POLICY, "expected join").await;
    return;
  };
  if !is_valid_id(&rendezvous_id) {
    fail(
      &mut socket,
      ErrorCode::InvalidRequest,
      "rendezvous_id must be 32 hex characters",
    )
    .await;
    return;
  }
  let user = match authenticate(&state, &token).await {
    Ok(user) => user,
    Err(body) => {
      state
        .rate_limits
        .record_auth_failure(&ip, std::time::Instant::now());
      fail_with(&mut socket, body).await;
      return;
    },
  };
  let mut joined = match state.rendezvous.join(&rendezvous_id, user) {
    Ok(joined) => joined,
    Err(error) => {
      let code = match error {
        JoinError::NotYours => ErrorCode::Forbidden,
        JoinError::Full => ErrorCode::Conflict,
        JoinError::TooMany => ErrorCode::RateLimited,
      };
      fail(&mut socket, code, &error.to_string()).await;
      return;
    },
  };
  let slot = joined.slot;
  if send(
    &mut socket,
    &RendezvousServerMessage::Joined {
      peer_present: joined.peer_present,
    },
  )
  .await
  .is_err()
  {
    state.rendezvous.leave(&rendezvous_id, slot);
    return;
  }

  let expires = Instant::now() + Duration::from_secs(config.ttl_secs);
  loop {
    tokio::select! {
      incoming = socket.recv() => match incoming {
        Some(Ok(Message::Binary(frame))) => {
          if let Err(error) = state.rendezvous.relay(&rendezvous_id, slot, frame.to_vec()) {
            debug!(%error, "rendezvous relay stopped");
            let code = match error {
              RelayError::FrameTooLarge => ErrorCode::PayloadTooLarge,
              RelayError::QuotaExceeded => ErrorCode::RateLimited,
              RelayError::Closed => ErrorCode::Conflict,
            };
            fail(&mut socket, code, &error.to_string()).await;
            break;
          }
        }
        // Text frames are control frames and only `join` exists; ping/pong are handled by the library.
        Some(Ok(Message::Text(_) | Message::Ping(_) | Message::Pong(_))) => {}
        Some(Ok(Message::Close(_)) | Err(_)) | None => break,
      },
      signal = joined.receiver.recv() => match signal {
        Some(Signal::Frame(frame)) => {
          if socket.send(Message::Binary(frame.into())).await.is_err() {
            break;
          }
        }
        Some(Signal::PeerJoined) => {
          if send(&mut socket, &RendezvousServerMessage::PeerJoined).await.is_err() {
            break;
          }
        }
        Some(Signal::PeerLeft) => {
          let _ = send(&mut socket, &RendezvousServerMessage::PeerLeft).await;
          close(&mut socket, CLOSE_NORMAL, "peer left").await;
          break;
        }
        None => break,
      },
      () = sleep_until(expires) => {
        let _ = send(&mut socket, &RendezvousServerMessage::Expired).await;
        close(&mut socket, CLOSE_NORMAL, "expired").await;
        break;
      }
    }
  }
  state.rendezvous.leave(&rendezvous_id, slot);
}

async fn authenticate(state: &AppState, token: &str) -> Result<teamcraft_proto::UserId, ErrorBody> {
  let result = async {
    let identity = accounts::validate_token(state, token).await?;
    accounts::authenticate(state, identity).await
  }
  .await;
  result
    .map(|caller| caller.user.id)
    .map_err(|error| ErrorBody::new(error.error_code(), error.public_message()))
}

fn is_valid_id(id: &str) -> bool {
  id.len() == ID_HEX_CHARS && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

async fn next_text(socket: &mut WebSocket) -> Option<RendezvousClientMessage> {
  loop {
    match socket.recv().await? {
      Ok(Message::Text(text)) => return serde_json::from_str(text.as_str()).ok(),
      Ok(Message::Close(_)) | Err(_) => return None,
      Ok(_) => {},
    }
  }
}

async fn send(socket: &mut WebSocket, message: &RendezvousServerMessage) -> Result<(), ()> {
  let Ok(text) = serde_json::to_string(message) else {
    return Err(());
  };
  socket.send(Message::Text(text.into())).await.map_err(|_| ())
}

async fn fail(socket: &mut WebSocket, code: ErrorCode, message: &str) {
  fail_with(socket, ErrorBody::new(code, message)).await;
}

async fn fail_with(socket: &mut WebSocket, body: ErrorBody) {
  let _ = send(socket, &RendezvousServerMessage::Error { error: body }).await;
  close(socket, CLOSE_POLICY, "rendezvous failed").await;
}

async fn close(socket: &mut WebSocket, code: u16, reason: &'static str) {
  let _ = socket
    .send(Message::Close(Some(CloseFrame {
      code,
      reason: reason.into(),
    })))
    .await;
}
