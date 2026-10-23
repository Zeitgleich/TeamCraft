//! The realtime gateway (docs/gateway.md): handshake with resume, and applying events.

use futures_util::FutureExt;
use futures_util::future::{Either, select};
use teamcraft_proto::{
  DeviceActivity, GatewayClientMessage, GatewayEvent, GatewayServerMessage, MlsMessageKind,
};

use super::Client;
use super::groups::MlsProgress;
use crate::api::{GatewayConnection, ServerApi};
use crate::error::CoreError;
use crate::platform;
use crate::read_state;
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
    // Events may have been missed while disconnected: every group is caught up again before the
    // outbox sends into it.
    self.mls_current.clear();
    let mut connection = self.open_gateway().await?;
    // A resumed session skips the full sync; the own name may still be missing from older stores.
    self.refresh_own_summary().await;
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
        GatewayServerMessage::SessionInvalid => {
          // The server forgot the session (a restart, or the resume window passed) and closes this
          // connection. Forget it here too, or every reconnect would try it again forever.
          self.sync.maybe_gateway_session = None;
          self.persist_sync()?;
          connection = self.open_gateway().await?;
        },
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
    let session_id = match receive(&mut connection).await? {
      GatewayServerMessage::Ready { session_id, .. } => session_id,
      other => {
        return Err(CoreError::Gateway(format!(
          "unexpected answer to identify: {other:?}"
        )));
      },
    };
    // The session is kept only once the sync succeeded: a stored session makes the next connect
    // resume instead of syncing, so a failed sync would otherwise leave its gap unfilled forever.
    self.full_sync().await?;
    self.sync.maybe_gateway_session = Some(GatewaySession {
      session_id,
      last_seq: 0,
    });
    self.persist_sync()?;
    Ok((connection, GatewayOutcome::Fresh))
  }

  /// A new gateway connection, after the server's `hello`.
  async fn open_gateway(&self) -> Result<A::Gateway, CoreError> {
    let mut connection = self.api.connect_gateway().await?;
    match receive(&mut connection).await? {
      GatewayServerMessage::Hello { .. } => Ok(connection),
      other => Err(CoreError::Gateway(format!("expected hello, got {other:?}"))),
    }
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
        self.apply_pending_changes().await?;
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

  /// Runs a connection until it closes, sending a heartbeat every `heartbeat`.
  pub async fn run_gateway(
    &mut self,
    connection: &mut A::Gateway,
    heartbeat: std::time::Duration,
  ) -> Result<(), CoreError> {
    let mut tick = Box::pin(platform::sleep(heartbeat));
    loop {
      let next = match select(Box::pin(connection.receive()), &mut tick).await {
        Either::Left((maybe_frame, _)) => Some(maybe_frame),
        Either::Right(((), _)) => None,
      };
      match next {
        Some(Some(frame)) => self.handle_frame(frame?).await?,
        Some(None) => return Ok(()),
        None => {
          connection.send(&GatewayClientMessage::Heartbeat).await?;
          tick = Box::pin(platform::sleep(heartbeat));
        },
      }
    }
  }

  async fn handle_event(&mut self, event: GatewayEvent) -> Result<(), CoreError> {
    match event {
      GatewayEvent::MessageCreate { message } => {
        self.clear_typing(message.conversation_id, message.sender_user_id);
        self.receive_message(message)
      },
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
          // Only a hint: the server's word doesn't make this device sign a removal (security
          // review A8). The remover's client, or an admin's signed change, removes them from the
          // group.
          self.refresh_conversation(conversation_id).await
        }
      },
      GatewayEvent::PresenceUpdate { user_id, state, .. } => {
        records::put_json(&self.store, Table::Presence, &user_id.to_string(), &state)?;
        if state == teamcraft_proto::PresenceState::Offline {
          // The server records "last seen" when the person stops being active; the event doesn't
          // carry it. Read in one batch on the next tick, so a burst of people going offline (or
          // someone toggling "appear offline") costs one request, not one per event.
          self.stale_last_seen.insert(user_id);
        } else {
          self.stale_last_seen.remove(&user_id);
          let record = super::people::LastSeenRecord {
            maybe_at_ms: None,
            fetched_at_ms: self.now_ms(),
          };
          records::put_json(
            &self.store,
            Table::Presence,
            &super::people::last_seen_key(user_id),
            &record,
          )?;
        }
        Ok(())
      },
      GatewayEvent::SpaceUpdate { .. }
      | GatewayEvent::SpaceMemberAdd { .. }
      | GatewayEvent::SpaceMemberRemove { .. } => self.refresh_spaces().await,
      GatewayEvent::WorkspaceUpdate { .. } => self.sync_workspace().await,
      // The user's own device that revoked it signs the removal (`Client::revoke_device`).
      GatewayEvent::DeviceRemove { user_id, device_id } => {
        if device_id == self.account.device_id {
          tracing::warn!("this device was revoked; it will no longer receive messages");
        }
        tracing::debug!(user_id = %user_id, "device revoked on the server");
        Ok(())
      },
      GatewayEvent::TypingStart {
        conversation_id,
        user_id,
      } => {
        self.note_typing(conversation_id, user_id);
        Ok(())
      },
      GatewayEvent::DeviceAdd { user_id, device_id } => self.queue_device_addition(user_id, device_id),
      GatewayEvent::ThreadUpdate { .. } => self.refresh_read_state().await,
      GatewayEvent::NotificationSettingsUpdate => self.refresh_notification_settings().await,
      GatewayEvent::ReadReceipt {
        conversation_id,
        user_id,
        message_id,
      } => read_state::note_read_receipt(&self.store, conversation_id, user_id, message_id),
      GatewayEvent::MessageAck {
        conversation_id,
        message_id,
        user_id,
      } => read_state::note_acknowledgement(&self.store, conversation_id, message_id, user_id),
      // Another of the user's devices changed the key bag (account data: saved items, drafts,
      // backgrounds, pins): take it now instead of at the next periodic sync.
      GatewayEvent::KeyBagUpdate { version } if version > self.sync.key_bag_version => {
        self.sync_account_data().await
      },
      GatewayEvent::KeyBagUpdate { .. } => Ok(()),
      // Meetings are not shown by the shell yet.
      GatewayEvent::MeetingUpdate { .. }
      | GatewayEvent::LobbyJoin { .. }
      | GatewayEvent::GuestExpired { .. }
      | GatewayEvent::ProfileUpdate { .. }
      | GatewayEvent::AnnouncementUpdate
      | GatewayEvent::OrgRecoveryUpdate { .. }
      | GatewayEvent::SystemEvent { .. }
      | GatewayEvent::UserGroupUpdate { .. }
      | GatewayEvent::LobbyDecision { .. } => Ok(()),
      // Call roster and ringing go to the app's call handling (media runs outside the core).
      GatewayEvent::CallStart { .. }
      | GatewayEvent::CallUpdate { .. }
      | GatewayEvent::CallEnd { .. }
      | GatewayEvent::CallRing { .. } => {
        self.emit_call_event(&event);
        Ok(())
      },
      GatewayEvent::Unknown => Ok(()),
    }
  }
}

async fn receive<G: GatewayConnection>(connection: &mut G) -> Result<GatewayServerMessage, CoreError> {
  match connection.receive().await {
    Some(frame) => Ok(frame?),
    None => Err(CoreError::Gateway("connection closed during handshake".into())),
  }
}
