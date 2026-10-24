//! The realtime gateway (docs/gateway.md): handshake with resume, and applying events.

use std::future::Future;
use std::time::Duration;

use futures_util::FutureExt;
use futures_util::future::{Either, select};
use teamcraft_proto::{
  DeviceActivity, ErrorBody, ErrorCode, GatewayClientMessage, GatewayEvent, GatewayServerMessage,
  MlsMessageKind, UserPresence,
};

use super::Client;
use super::groups::MlsProgress;
use crate::api::{ApiError, GatewayConnection, ServerApi};
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

/// How long each step of the handshake (socket and `hello`, the answer to `resume` or `identify`)
/// may take when the caller doesn't say: a server that accepts the socket and then stays silent must
/// not hold the reconnect loop forever.
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// A gateway connection that finished its handshake.
pub struct GatewayStart<G> {
  pub connection: G,
  pub outcome: GatewayOutcome,
  /// The heartbeat interval the server asked for in `hello`.
  pub heartbeat_interval: Duration,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Connects the gateway, resuming the stored session if possible; a fresh session triggers a
  /// full sync. Process the returned connection with [`Self::pump`] or [`Self::run_gateway`].
  pub async fn connect_gateway(&mut self) -> Result<(A::Gateway, GatewayOutcome), CoreError> {
    let start = self.start_gateway(DEFAULT_HANDSHAKE_TIMEOUT).await?;
    Ok((start.connection, start.outcome))
  }

  /// Like [`Self::connect_gateway`], with a deadline for every handshake step and the heartbeat
  /// interval from `hello`. A rejected token comes back as an API error with status 401
  /// ([`CoreError::is_sign_in_required`]).
  pub async fn start_gateway(
    &mut self,
    handshake_timeout: Duration,
  ) -> Result<GatewayStart<A::Gateway>, CoreError> {
    // Events may have been missed while disconnected: every group is caught up again before the
    // outbox sends into it.
    self.mls_current.clear();
    let (mut connection, mut heartbeat_interval) = self.open_gateway(handshake_timeout).await?;
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
      match receive(&mut connection, handshake_timeout).await? {
        GatewayServerMessage::Resumed { .. } => {
          self.resync_live_state().await?;
          return Ok(GatewayStart {
            connection,
            outcome: GatewayOutcome::Resumed,
            heartbeat_interval,
          });
        },
        GatewayServerMessage::SessionInvalid => {
          // The server forgot the session (a restart, or the resume window passed) and closes this
          // connection. Forget it here too, or every reconnect would try it again forever.
          self.sync.maybe_gateway_session = None;
          self.persist_sync()?;
          (connection, heartbeat_interval) = self.open_gateway(handshake_timeout).await?;
        },
        GatewayServerMessage::Error { error } => return Err(rejected(error)),
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
    let session_id = match receive(&mut connection, handshake_timeout).await? {
      GatewayServerMessage::Ready { session_id, .. } => session_id,
      GatewayServerMessage::Error { error } => return Err(rejected(error)),
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
    // The full sync already read the counters; the UI still reads what only it keeps.
    read_state::update_marks(&self.store, |marks| marks.connections += 1)?;
    Ok(GatewayStart {
      connection,
      outcome: GatewayOutcome::Fresh,
      heartbeat_interval,
    })
  }

  /// After a resumed connection: the missed events follow, but counters are read once anyway, so a
  /// count that drifted (an event lost before this device stored it) is repaired on every connect
  /// instead of by a timer (issue #183). One request each for threads and counts. A failed read
  /// doesn't fail the connection: the events keep the counts current, the next connect retries.
  async fn resync_live_state(&mut self) -> Result<(), CoreError> {
    if let Err(error) = self.refresh_read_state().await {
      tracing::warn!(%error, "could not read the unread counts after resuming");
    }
    read_state::update_marks(&self.store, |marks| marks.connections += 1)
  }

  /// The token the handshake sends (`identify`/`resume`).
  pub(crate) fn gateway_token(&self) -> Result<String, ApiError> {
    self.api.gateway_token()
  }

  /// A new gateway connection after the server's `hello`, and the heartbeat interval it asked for.
  async fn open_gateway(&self, handshake_timeout: Duration) -> Result<(A::Gateway, Duration), CoreError> {
    let opened = within(handshake_timeout, async {
      let mut connection = self.api.connect_gateway().await?;
      let hello = receive(&mut connection, handshake_timeout).await?;
      Ok::<_, CoreError>((connection, hello))
    })
    .await??;
    match opened {
      (
        connection,
        GatewayServerMessage::Hello {
          heartbeat_interval_ms,
        },
      ) => Ok((connection, Duration::from_millis(heartbeat_interval_ms))),
      (_, other) => Err(CoreError::Gateway(format!("expected hello, got {other:?}"))),
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
    heartbeat: Duration,
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
        read_state::note_new_message(&self.store, &message, self.account.user_id)?;
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
      GatewayEvent::PresenceUpdate {
        user_id,
        state,
        maybe_status,
      } => {
        records::put_json(&self.store, Table::Presence, &user_id.to_string(), &state)?;
        read_state::note_announced_presence(
          &self.store,
          &UserPresence {
            user_id,
            state: state.clone(),
            maybe_status,
            maybe_last_seen: None,
          },
        )?;
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
      GatewayEvent::UnreadUpdate {
        conversation_id,
        unread_count,
        mention_count,
      } => read_state::note_unread(&self.store, conversation_id, unread_count, mention_count),
      // The UI reads the announcement itself; the mark tells it to do so now.
      GatewayEvent::AnnouncementUpdate => {
        read_state::update_marks(&self.store, |marks| marks.announcements += 1)
      },
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

async fn receive<G: GatewayConnection>(
  connection: &mut G,
  timeout: Duration,
) -> Result<GatewayServerMessage, CoreError> {
  match within(timeout, connection.receive()).await? {
    Some(frame) => Ok(frame?),
    None => Err(CoreError::Gateway("connection closed during handshake".into())),
  }
}

/// `future`'s output, or an error once `timeout` has passed.
async fn within<T>(timeout: Duration, future: impl Future<Output = T>) -> Result<T, CoreError> {
  match select(Box::pin(future), Box::pin(platform::sleep(timeout))).await {
    Either::Left((output, _)) => Ok(output),
    Either::Right(((), _)) => Err(CoreError::Gateway(format!(
      "no answer from the gateway within {} s",
      timeout.as_secs()
    ))),
  }
}

/// The server refused the handshake. A refused token reads like a refused HTTP request (401), so
/// the caller can tell "sign in again" apart from an outage.
fn rejected(error: ErrorBody) -> CoreError {
  let status = match error.error_code {
    ErrorCode::Unauthorized | ErrorCode::InvalidToken => 401,
    ErrorCode::Unavailable | ErrorCode::Internal => 503,
    ErrorCode::RateLimited => 429,
    _ => 403,
  };
  CoreError::Api(ApiError::Status {
    status,
    maybe_body: Some(error),
  })
}
