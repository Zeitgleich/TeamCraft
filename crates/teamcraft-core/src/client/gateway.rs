//! The realtime gateway (docs/gateway.md): handshake with resume, and applying events.

use futures_util::FutureExt;
use teamcraft_proto::{
  DeviceActivity, GatewayClientMessage, GatewayEvent, GatewayServerMessage, MlsMessageKind,
};

use super::Client;
use super::groups::MlsProgress;
use crate::api::{GatewayConnection, ServerApi};
use crate::error::CoreError;
use crate::records::{self, GatewaySession};
use crate::store::{LocalStore, Table};

/// How a gateway connection started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GatewayOutcome {
  /// The previous session continued; missed events follow on the connection.
  Resumed,
  /// A new session: events in between were lost, so state was refetched over REST.
  Fresh,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Connects the gateway, resuming the stored session if possible; a fresh session triggers a
  /// full sync. Process the returned connection with [`Self::pump`] or [`Self::run_gateway`].
  pub async fn connect_gateway(&mut self) -> Result<(A::Gateway, GatewayOutcome), CoreError> {
    let mut connection = self.api.connect_gateway().await?;
    match receive(&mut connection).await? {
      GatewayServerMessage::Hello { .. } => {},
      other => return Err(CoreError::Gateway(format!("expected hello, got {other:?}"))),
    }
    if let Some(session) = self.sync.maybe_gateway_session.clone() {
      connection
        .send(&GatewayClientMessage::Resume {
          token: self.api.gateway_token()?,
          session_id: session.session_id,
          last_seq: session.last_seq,
          device_id: self.account.device_id,
        })
        .await?;
      match receive(&mut connection).await? {
        GatewayServerMessage::Resumed { .. } => return Ok((connection, GatewayOutcome::Resumed)),
        GatewayServerMessage::SessionInvalid => {},
        other => {
          return Err(CoreError::Gateway(format!(
            "unexpected answer to resume: {other:?}"
          )));
        },
      }
    }
    connection
      .send(&GatewayClientMessage::Identify {
        token: self.api.gateway_token()?,
        device_id: self.account.device_id,
        activity: DeviceActivity::Active,
      })
      .await?;
    match receive(&mut connection).await? {
      GatewayServerMessage::Ready { session_id, .. } => {
        self.sync.maybe_gateway_session = Some(GatewaySession {
          session_id,
          last_seq: 0,
        });
        self.persist_sync()?;
      },
      other => {
        return Err(CoreError::Gateway(format!(
          "unexpected answer to identify: {other:?}"
        )));
      },
    }
    self.full_sync().await?;
    Ok((connection, GatewayOutcome::Fresh))
  }

  /// Applies every frame that is already waiting on the connection, without blocking. Returns
  /// how many frames were handled; `Err(Gateway)` if the connection closed.
  pub async fn pump(&mut self, connection: &mut A::Gateway) -> Result<usize, CoreError> {
    let mut handled = 0;
    loop {
      let Some(next) = connection.receive().now_or_never() else {
        return Ok(handled);
      };
      match next {
        Some(frame) => {
          self.handle_frame(frame?).await?;
          handled += 1;
        },
        None => return Err(CoreError::Gateway("connection closed".into())),
      }
    }
  }

  /// Applies one frame from the server.
  pub async fn handle_frame(&mut self, frame: GatewayServerMessage) -> Result<(), CoreError> {
    match frame {
      GatewayServerMessage::Event { seq, event } => {
        self.handle_event(event).await?;
        if let Some(session) = &mut self.sync.maybe_gateway_session {
          session.last_seq = session.last_seq.max(seq);
        }
        self.persist_sync()
      },
      GatewayServerMessage::SessionInvalid => {
        self.sync.maybe_gateway_session = None;
        self.persist_sync()
      },
      GatewayServerMessage::Error { error } => Err(CoreError::Gateway(error.message)),
      _ => Ok(()),
    }
  }

  /// Runs a connection until it closes, sending heartbeats. Native only (needs a timer).
  #[cfg(not(target_arch = "wasm32"))]
  pub async fn run_gateway(
    &mut self,
    connection: &mut A::Gateway,
    heartbeat: std::time::Duration,
  ) -> Result<(), CoreError> {
    let mut interval = tokio::time::interval(heartbeat);
    loop {
      tokio::select! {
        _ = interval.tick() => connection.send(&GatewayClientMessage::Heartbeat).await?,
        maybe_frame = connection.receive() => match maybe_frame {
          Some(frame) => self.handle_frame(frame?).await?,
          None => return Ok(()),
        },
      }
    }
  }

  async fn handle_event(&mut self, event: GatewayEvent) -> Result<(), CoreError> {
    match event {
      GatewayEvent::MessageCreate { message } => self.receive_message(message),
      GatewayEvent::MlsMessage { message } => {
        if message.kind == MlsMessageKind::Welcome {
          if message.maybe_recipient_device_id == Some(self.account.device_id) {
            self.join_from_welcome(&message).await?;
          }
          return Ok(());
        }
        if self.process_mls(&message).await? == MlsProgress::NeedsBacklog {
          self.sync_mls(message.conversation_id).await?;
        }
        Ok(())
      },
      GatewayEvent::MembershipAdd { conversation_id, .. }
      | GatewayEvent::ConversationUpdate { conversation_id } => {
        self.refresh_conversation(conversation_id).await
      },
      GatewayEvent::MembershipRemove {
        conversation_id,
        user_id,
      } => {
        if user_id == self.account.user_id {
          // Removed: the group is gone for this device; local history stays readable.
          self.drop_group(conversation_id)?;
          self
            .store
            .delete(Table::Conversations, &conversation_id.to_string())?;
          self.store.delete(Table::Members, &conversation_id.to_string())?;
          Ok(())
        } else {
          self.refresh_conversation(conversation_id).await
        }
      },
      GatewayEvent::PresenceUpdate { user_id, state } => {
        records::put_json(&self.store, Table::Presence, &user_id.to_string(), &state)
      },
      GatewayEvent::SpaceUpdate { .. }
      | GatewayEvent::SpaceMemberAdd { .. }
      | GatewayEvent::SpaceMemberRemove { .. } => self.refresh_spaces().await,
      // Not handled yet: verifying and applying roster/policy log entries, and committing the
      // removal of a removed device from every shared group. Both are follow-ups for the core.
      GatewayEvent::WorkspaceUpdate { .. } | GatewayEvent::DeviceRemove { .. } => Ok(()),
      GatewayEvent::TypingStart { .. } | GatewayEvent::Unknown => Ok(()),
    }
  }
}

async fn receive<G: GatewayConnection>(connection: &mut G) -> Result<GatewayServerMessage, CoreError> {
  match connection.receive().await {
    Some(frame) => Ok(frame?),
    None => Err(CoreError::Gateway("connection closed during handshake".into())),
  }
}
