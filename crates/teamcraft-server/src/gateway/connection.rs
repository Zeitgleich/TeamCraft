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
/// Conversations a connection tracks typing for; more is ignored until entries age out.
const MAX_TRACKED_TYPING: usize = 256;
/// Changes of the activity state a connection may make: one per this long (each one costs queries
/// and may broadcast to everybody who shares a conversation).
const ACTIVITY_CHANGE_INTERVAL: Duration = Duration::from_secs(1);
/// WebSocket close code "policy violation" (RFC 6455).
const CLOSE_POLICY: u16 = 1008;
const CLOSE_NORMAL: u16 = 1000;

/// Runs one gateway connection from `hello` to close.
pub async fn run(state: AppState, mut socket: WebSocket, ip: String) {
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
    Ok(Some(message)) => handshake(&state, &mut socket, message, &ip).await,
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
  ip: &str,
) -> Option<(AuthenticatedUser, Attached)> {
  match message {
    GatewayClientMessage::Identify {
      token,
      device_id,
      activity,
    } => {
      let caller = authenticate(state, socket, &token, device_id, ip).await?;
      let attached = state.hub.open_session(caller.user.id, device_id, activity);
      if let Some(oidc_session) = &caller.maybe_oidc_session {
        state.hub.set_oidc_session(&attached.session_id, oidc_session);
      }
      let ready = GatewayServerMessage::Ready {
        session_id: attached.session_id.clone(),
        user: crate::services::convert::user_summary(state, &caller.user),
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
      let caller = authenticate(state, socket, &token, device_id, ip).await?;
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
  ip: &str,
) -> Option<AuthenticatedUser> {
  let result = async {
    let identity = accounts::validate_token(state, token).await?;
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
      // Guessing tokens over the gateway counts like guessing them over HTTP.
      if matches!(error, ApiError::Auth(_)) {
        state
          .rate_limits
          .record_auth_failure(ip, std::time::Instant::now());
      }
      let body = ErrorBody::new(error.error_code(), error.public_message());
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
  let mut connection = ConnectionState::default();
  let mut bucket = FrameBucket::new(
    config.client_frame_burst,
    config.max_client_frames_per_minute,
    Instant::now(),
  );

  loop {
    tokio::select! {
      incoming = next_client_frame(socket) => {
        let Some(frame) = incoming else { break };
        deadline = Instant::now() + heartbeat_timeout;
        // Every frame counts, parseable or not: the HTTP rate limits only saw the upgrade.
        if !bucket.take(Instant::now()) {
          close(socket, CLOSE_POLICY, "too many messages").await;
          break;
        }
        let Frame::Message(message) = frame else { continue };
        if !handle_client_message(state, caller, attached, socket, &mut connection, message).await {
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
  connection: &mut ConnectionState,
  message: GatewayClientMessage,
) -> bool {
  match message {
    GatewayClientMessage::Heartbeat => {
      state.hub.renew_attention(&attached.session_id);
      send(socket, &GatewayServerMessage::HeartbeatAck).await.is_ok()
    },
    GatewayClientMessage::SetActivity { activity } => {
      let now = Instant::now();
      let is_too_soon = connection
        .maybe_last_activity_change
        .is_some_and(|at| now.duration_since(at) < ACTIVITY_CHANGE_INTERVAL);
      if !is_too_soon {
        connection.maybe_last_activity_change = Some(now);
        set_activity(state, caller, attached, activity).await;
      }
      true
    },
    GatewayClientMessage::Attention { is_attentive } => {
      // Kept in memory only, never stored or logged (e2ee.md §7). Cheap and idempotent: no throttle
      // needed. Ignored where the instance turned attention off; the user's own choice is applied
      // in the routing, and clients that know it send nothing.
      if state.settings.notifications.is_desktop_attention_enabled {
        state.hub.set_attention(&attached.session_id, is_attentive);
      }
      true
    },
    GatewayClientMessage::Typing { conversation_id } => {
      state.hub.touch_activity(&attached.session_id);
      let now = Instant::now();
      let last_typing = &mut connection.last_typing;
      if last_typing
        .get(&conversation_id)
        .is_some_and(|at| now.duration_since(*at) < TYPING_THROTTLE)
      {
        return true;
      }
      if last_typing.len() >= MAX_TRACKED_TYPING {
        last_typing.retain(|_, at| now.duration_since(*at) < TYPING_THROTTLE);
        if last_typing.len() >= MAX_TRACKED_TYPING {
          return true;
        }
      }
      // Only conversations the caller may type in are remembered: made-up IDs cost a lookup each
      // (bounded by the frame limit) but never grow the map.
      if let Err(error) = publish_typing(state, caller, conversation_id).await {
        let body = ErrorBody::new(error.error_code(), error.public_message());
        return send(socket, &GatewayServerMessage::Error { error: body })
          .await
          .is_ok();
      }
      last_typing.insert(conversation_id, now);
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

/// What arrived on the socket.
enum Frame {
  Message(GatewayClientMessage),
  /// A frame that carries nothing for us: malformed JSON, binary, ping/pong. It still counts
  /// against the frame limit.
  Ignored,
}

/// The next frame; `None` when the connection is closed or broken.
async fn next_client_frame(socket: &mut WebSocket) -> Option<Frame> {
  match socket.recv().await? {
    Ok(Message::Text(text)) => match serde_json::from_str(text.as_str()) {
      Ok(message) => Some(Frame::Message(message)),
      // Unparseable frames are skipped like unknown ones; the heartbeat timeout ends dead peers.
      Err(error) => {
        debug!(%error, "ignoring malformed gateway frame");
        Some(Frame::Ignored)
      },
    },
    Ok(Message::Close(_)) | Err(_) => None,
    Ok(_) => Some(Frame::Ignored),
  }
}

/// The next text frame parsed as client message; `None` when the connection is closed or broken.
/// Used for the handshake, which is bounded by its own timeout.
async fn next_client_message(socket: &mut WebSocket) -> Option<GatewayClientMessage> {
  loop {
    match next_client_frame(socket).await? {
      Frame::Message(message) => return Some(message),
      Frame::Ignored => {},
    }
  }
}

/// What one connection remembers between frames.
#[derive(Default)]
struct ConnectionState {
  last_typing: HashMap<ConversationId, Instant>,
  maybe_last_activity_change: Option<Instant>,
}

/// A token bucket over the frames of one connection.
struct FrameBucket {
  tokens: f64,
  capacity: f64,
  per_second: f64,
  updated_at: Instant,
}

impl FrameBucket {
  fn new(burst: u32, per_minute: u32, now: Instant) -> Self {
    let capacity = f64::from(burst.max(1));
    Self {
      tokens: capacity,
      capacity,
      per_second: f64::from(per_minute) / 60.0,
      updated_at: now,
    }
  }

  /// Takes one frame's worth; `false` if the connection is over its limit.
  fn take(&mut self, now: Instant) -> bool {
    let elapsed = now.saturating_duration_since(self.updated_at).as_secs_f64();
    self.tokens = (self.tokens + elapsed * self.per_second).min(self.capacity);
    self.updated_at = now;
    if self.tokens >= 1.0 {
      self.tokens -= 1.0;
      true
    } else {
      false
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_bucket_allows_a_burst_then_the_average_rate() {
    let start = Instant::now();
    let mut bucket = FrameBucket::new(5, 60, start);
    assert!((0..5).all(|_| bucket.take(start)), "the burst");
    assert!(!bucket.take(start), "then it is empty");
    assert!(!bucket.take(start + Duration::from_millis(900)));
    assert!(
      bucket.take(start + Duration::from_millis(1100)),
      "one per second refills"
    );
    assert!(!bucket.take(start + Duration::from_millis(1200)));
    // Idling never banks more than the burst.
    let later = start + Duration::from_secs(3600);
    assert!((0..5).all(|_| bucket.take(later)));
    assert!(!bucket.take(later));
  }
}
