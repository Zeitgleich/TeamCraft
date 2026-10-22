use std::collections::HashMap;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use teamcraft_proto::{
  ConversationId, DeviceActivity, DeviceId, ErrorBody, GatewayClientMessage, GatewayEvent,
  GatewayServerMessage, Permission,
};
use tokio::time::{Instant, sleep_until, timeout};
use tracing::{debug, warn};

use super::hub::Attached;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::access::{load_conversation, require};
use crate::services::{accounts, presence};
use crate::state::AppState;

/// Minimum time between two typing events of one connection for the same conversation.
const TYPING_THROTTLE: Duration = Duration::from_secs(3);
/// WebSocket close code "policy violation" (RFC 6455).
const CLOSE_POLICY: u16 = 1008;
const CLOSE_NORMAL: u16 = 1000;

/// Runs one gateway connection from `hello` to close.
pub async fn run(state: AppState, mut socket: WebSocket) {
  let config = state.settings.gateway.clone();
  let hello = GatewayServerMessage::Hello {
    heartbeat_interval_ms: config.heartbeat_interval_secs * 1000,
  };
  if send(&mut socket, &hello).await.is_err() {
    return;
  }

  let first = timeout(
    Duration::from_secs(config.identify_timeout_secs),
    next_client_message(&mut socket),
  )
  .await;
  let handshake = match first {
    Ok(Some(message)) => handshake(&state, &mut socket, message).await,
    _ => None,
  };
  let Some((caller, mut attached)) = handshake else {
    close(&mut socket, CLOSE_POLICY, "handshake failed").await;
    return;
  };

  let user = caller.user.id;
  debug!(user_id = %user, "gateway connected");
  if let Err(error) = presence::refresh_presence(&state, user).await {
    warn!(%error, "presence refresh failed");
  }

  serve(&state, &caller, &mut attached, &mut socket).await;

  state.hub.detach(&attached.session_id);
  debug!(user_id = %user, "gateway disconnected");
  // Reconnects are common (network changes); only report offline if the user stays gone.
  let grace = Duration::from_secs(config.presence_offline_grace_secs);
  tokio::spawn(async move {
    tokio::time::sleep(grace).await;
    if let Err(error) = presence::refresh_presence(&state, user).await {
      warn!(%error, "presence refresh failed");
    }
  });
}

async fn handshake(
  state: &AppState,
  socket: &mut WebSocket,
  message: GatewayClientMessage,
) -> Option<(AuthenticatedUser, Attached)> {
  match message {
    GatewayClientMessage::Identify {
      token,
      device_id,
      activity,
    } => {
      let caller = authenticate(state, socket, &token, device_id).await?;
      let attached = state.hub.open_session(caller.user.id, device_id, activity);
      if let Some(oidc_session) = &caller.maybe_oidc_session {
        state.hub.set_oidc_session(&attached.session_id, oidc_session);
      }
      let ready = GatewayServerMessage::Ready {
        session_id: attached.session_id.clone(),
        user: crate::services::convert::user_summary(&caller.user),
      };
      send(socket, &ready).await.ok()?;
      Some((caller, attached))
    },
    GatewayClientMessage::Resume {
      token,
      session_id,
      last_seq,
      device_id,
    } => {
      let caller = authenticate(state, socket, &token, device_id).await?;
      match state
        .hub
        .resume_session(&session_id, caller.user.id, device_id, last_seq)
      {
        Ok(mut attached) => {
          let replay = std::mem::take(&mut attached.replay);
          send(
            socket,
            &GatewayServerMessage::Resumed {
              replayed: replay.len() as u64,
            },
          )
          .await
          .ok()?;
          for event in &replay {
            send(socket, event).await.ok()?;
          }
          Some((caller, attached))
        },
        Err(error) => {
          debug!(%error, "resume rejected");
          let _ = send(socket, &GatewayServerMessage::SessionInvalid).await;
          None
        },
      }
    },
    _ => None,
  }
}

/// Validates the token and the device; sends an error frame on failure.
async fn authenticate(
  state: &AppState,
  socket: &mut WebSocket,
  token: &str,
  device: DeviceId,
) -> Option<AuthenticatedUser> {
  let result = async {
    let identity = state.validator.validate(token).await?;
    let caller = accounts::authenticate(state, identity).await?;
    if !state.database.is_active_device_of(caller.user.id, device).await? {
      return Err(ApiError::device_not_found());
    }
    Ok::<_, ApiError>(caller)
  }
  .await;
  match result {
    Ok(caller) => Some(caller),
    Err(error) => {
      let body = ErrorBody::new(error.error_code(), error.to_string());
      let _ = send(socket, &GatewayServerMessage::Error { error: body }).await;
      None
    },
  }
}

async fn serve(
  state: &AppState,
  caller: &AuthenticatedUser,
  attached: &mut Attached,
  socket: &mut WebSocket,
) {
  let config = &state.settings.gateway;
  let heartbeat_timeout = Duration::from_secs(config.heartbeat_timeout_secs);
  let mut deadline = Instant::now() + heartbeat_timeout;
  let max_age = Instant::now() + Duration::from_secs(config.connection_max_age_secs);
  let mut last_typing: HashMap<ConversationId, Instant> = HashMap::new();

  loop {
    tokio::select! {
      incoming = next_client_message(socket) => {
        let Some(message) = incoming else { break };
        deadline = Instant::now() + heartbeat_timeout;
        if !handle_client_message(state, caller, attached, socket, &mut last_typing, message).await {
          break;
        }
      }
      outgoing = attached.receiver.recv() => {
        // `None`: the hub dropped this connection because it was too slow.
        let Some(message) = outgoing else { break };
        if send(socket, &message).await.is_err() {
          break;
        }
      }
      () = sleep_until(deadline) => {
        close(socket, CLOSE_POLICY, "heartbeat timeout").await;
        break;
      }
      () = sleep_until(max_age) => {
        close(socket, CLOSE_NORMAL, "reconnect to renew the session").await;
        break;
      }
    }
  }
}

/// Returns `false` to close the connection.
async fn handle_client_message(
  state: &AppState,
  caller: &AuthenticatedUser,
  attached: &Attached,
  socket: &mut WebSocket,
  last_typing: &mut HashMap<ConversationId, Instant>,
  message: GatewayClientMessage,
) -> bool {
  match message {
    GatewayClientMessage::Heartbeat => send(socket, &GatewayServerMessage::HeartbeatAck).await.is_ok(),
    GatewayClientMessage::SetActivity { activity } => {
      set_activity(state, caller, attached, activity).await;
      true
    },
    GatewayClientMessage::Typing { conversation_id } => {
      let now = Instant::now();
      if last_typing
        .get(&conversation_id)
        .is_some_and(|at| now.duration_since(*at) < TYPING_THROTTLE)
      {
        return true;
      }
      last_typing.insert(conversation_id, now);
      if let Err(error) = publish_typing(state, caller, conversation_id).await {
        let body = ErrorBody::new(error.error_code(), error.to_string());
        return send(socket, &GatewayServerMessage::Error { error: body })
          .await
          .is_ok();
      }
      true
    },
    // A second identify/resume or a frame this version doesn't know: ignore, stay compatible.
    GatewayClientMessage::Identify { .. }
    | GatewayClientMessage::Resume { .. }
    | GatewayClientMessage::Unknown => true,
  }
}

async fn set_activity(
  state: &AppState,
  caller: &AuthenticatedUser,
  attached: &Attached,
  activity: DeviceActivity,
) {
  state.hub.set_activity(&attached.session_id, activity);
  if let Err(error) = presence::refresh_presence(state, caller.user.id).await {
    warn!(%error, "presence refresh failed");
  }
}

async fn publish_typing(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
) -> Result<(), ApiError> {
  let access = load_conversation(state, caller, conversation).await?;
  require(state, caller, Permission::CreatePost, &access.target())?;
  let members = state.database.conversation_member_ids(conversation).await?;
  state.hub.publish(
    members.into_iter().filter(|member| *member != caller.user.id),
    &GatewayEvent::TypingStart {
      conversation_id: conversation,
      user_id: caller.user.id,
    },
  );
  Ok(())
}

/// The next text frame parsed as client message; `None` when the connection is closed or broken.
async fn next_client_message(socket: &mut WebSocket) -> Option<GatewayClientMessage> {
  loop {
    match socket.recv().await? {
      Ok(Message::Text(text)) => match serde_json::from_str(text.as_str()) {
        Ok(message) => return Some(message),
        // Unparseable frames are skipped like unknown ones; the heartbeat timeout ends dead peers.
        Err(error) => debug!(%error, "ignoring malformed gateway frame"),
      },
      Ok(Message::Close(_)) | Err(_) => return None,
      Ok(_) => {},
    }
  }
}

async fn send(socket: &mut WebSocket, message: &GatewayServerMessage) -> Result<(), ()> {
  let Ok(text) = serde_json::to_string(message) else {
    return Err(());
  };
  socket.send(Message::Text(text.into())).await.map_err(|_| ())
}

async fn close(socket: &mut WebSocket, code: u16, reason: &'static str) {
  let _ = socket
    .send(Message::Close(Some(CloseFrame {
      code,
      reason: reason.into(),
    })))
    .await;
}
