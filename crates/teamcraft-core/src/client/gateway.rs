//! The realtime gateway (docs/gateway.md): handshake with resume, applying events, and keeping the
//! connection alive while long work runs (issue #325).

use std::collections::VecDeque;
use std::future::Future;
use std::pin::pin;
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
  /// What arrived on the connection while the full sync ran; to be applied before anything newer.
  pub held: HeldFrames,
}

/// How [`Client::start_gateway`] runs the handshake and keeps the connection alive during the
/// sync that follows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GatewayTiming {
  /// How long each handshake step may take (socket and `hello`, the answer to `identify`/`resume`).
  pub handshake_timeout: Duration,
  /// Longest heartbeat interval, also when the server's `hello` allows more.
  pub max_heartbeat: Duration,
  /// How much is held while long work runs ([`HeldFrames`]).
  pub held_limits: HeldLimits,
}

/// Bounds of [`HeldFrames`]: beyond either the connection isn't read until the work is done (the
/// server then drops it as too slow, and the client resumes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeldLimits {
  pub max_frames: usize,
  /// The frames' size as JSON, as they came over the wire.
  pub max_bytes: usize,
}

/// Frames that arrived while [`keep_alive`] ran other work, in their order, to be applied once it is
/// done. A closed connection or a failed read or heartbeat is held too, as the last entry.
#[derive(Debug, Default)]
pub struct HeldFrames {
  frames: VecDeque<Option<Result<GatewayServerMessage, ApiError>>>,
  bytes: usize,
  is_closed: bool,
  heartbeats_sent: u32,
  /// Whether the server answered meanwhile (a heartbeat answer or any other frame).
  was_heard: bool,
}

/// What [`HeldFrames`] says about the time the work ran, taken once by the supervisor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeptAlive {
  pub heartbeats_sent: u32,
  pub was_heard: bool,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Connects the gateway, resuming the stored session if possible; a fresh session triggers a
  /// full sync. Process the returned connection with [`Self::pump`] or [`Self::run_gateway`].
  /// Frames that arrive during the sync are lost; apps use [`Self::start_gateway`] through the
  /// connection supervisor.
  pub async fn connect_gateway(&mut self) -> Result<(A::Gateway, GatewayOutcome), CoreError> {
    let start = self.start_gateway(GatewayTiming::default()).await?;
    Ok((start.connection, start.outcome))
  }

  /// Like [`Self::connect_gateway`], with a deadline for every handshake step and the heartbeat
  /// interval from `hello`. The connection keeps sending heartbeats while the sync after the
  /// handshake runs, which can take minutes on a workspace that grew (issue #325); what arrives
  /// meanwhile comes back in [`GatewayStart::held`]. A rejected token comes back as an API error
  /// with status 401 ([`CoreError::is_sign_in_required`]).
  /// Remembers what the device says about its activity, so the next gateway session identifies
  /// with it instead of claiming to be active (the shells send the report on the live connection).
  pub fn set_device_activity(&mut self, activity: DeviceActivity) {
    self.device_activity = activity;
  }

  pub async fn start_gateway(
    &mut self,
    timing: GatewayTiming,
  ) -> Result<GatewayStart<A::Gateway>, CoreError> {
    let handshake_timeout = timing.handshake_timeout;
    // Events may have been missed while disconnected: every group is caught up again before the
    // outbox sends into it, and the welcomes are read again before a spent key package goes
    // (issue #395).
    self.mls_current.clear();
    self.are_welcomes_synced = false;
    // A replay cut short by the drop is not waited for; this connection brings its own.
    self.maybe_replay_left = None;
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
        GatewayServerMessage::Resumed { replayed } => {
          let mut held = HeldFrames::default();
          if replayed == 0 {
            let heartbeat = heartbeat_interval.min(timing.max_heartbeat);
            keep_alive(
              &mut connection,
              heartbeat,
              heartbeat,
              timing.held_limits,
              &mut held,
              self.resync_live_state(),
            )
            .await?;
          } else {
            self.maybe_replay_left = Some(replayed);
          }
          self.refresh_link_requests().await;
          return Ok(GatewayStart {
            connection,
            outcome: GatewayOutcome::Resumed,
            heartbeat_interval,
            held,
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
        activity: self.device_activity.clone(),
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
    let mut held = HeldFrames::default();
    let heartbeat = heartbeat_interval.min(timing.max_heartbeat);
    keep_alive(
      &mut connection,
      heartbeat,
      heartbeat,
      timing.held_limits,
      &mut held,
      self.sync_fresh_session(session_id),
    )
    .await?;
    self.refresh_link_requests().await;
    Ok(GatewayStart {
      connection,
      outcome: GatewayOutcome::Fresh,
      heartbeat_interval,
      held,
    })
  }

  /// After `identify`: the full sync, then the session is kept. Only once the sync succeeded: a
  /// stored session makes the next connect resume instead of syncing, so a failed sync would
  /// otherwise leave its gap unfilled forever.
  async fn sync_fresh_session(&mut self, session_id: String) -> Result<(), CoreError> {
    self.full_sync().await?;
    self.sync.maybe_gateway_session = Some(GatewaySession {
      session_id,
      last_seq: 0,
    });
    self.persist_sync()?;
    // The full sync already read the counters; the UI still reads what only it keeps.
    self.finish_connection().await
  }

  /// After a resumed connection, once the missed events were replayed: counters are read once, so a
  /// count that drifted (an event lost before this device stored it) is repaired on every connect
  /// instead of by a timer (issue #183). Not before the replay has drained: `GET /unread` already
  /// counts the replayed messages, which [`read_state::note_new_message`] would count again. One
  /// request each for threads and counts. A failed read doesn't fail the connection: the events
  /// keep the counts current, the next connect retries.
  async fn resync_live_state(&mut self) -> Result<(), CoreError> {
    if let Err(error) = self.refresh_read_state().await {
      tracing::warn!(%error, "could not read the unread counts after resuming");
    }
    self.finish_connection().await
  }

  /// Every connection, once caught up: presence is read again (events in between were lost), then
  /// the connection is counted, which tells the UI and the upkeep to do their once-per-connection
  /// reads. A failed presence read doesn't fail the connection; the next one retries.
  async fn finish_connection(&mut self) -> Result<(), CoreError> {
    if let Err(error) = self.refresh_known_presence().await {
      tracing::warn!(%error, "could not read presence after connecting");
    }
    read_state::update_marks(&self.store, |marks| {
      marks.connections += 1;
      // Meeting updates in between may have been lost.
      marks.meetings += 1;
    })
  }

  /// How many gateway connections this device made (fresh or resumed), counted once each was
  /// caught up. Work that only has to happen once per connection compares it with the value it
  /// saw last ([`crate::upkeep::Upkeep`]). `None` if the store can't be read.
  pub fn gateway_connections(&self) -> Option<u64> {
    read_state::marks(&self.store)
      .inspect_err(|error| tracing::warn!(%error, "could not read the connection count"))
      .ok()
      .map(|marks| marks.connections)
  }

  /// The roster changed: a rotated org recovery key needs the backup key wrapped to it (security
  /// review B4), now rather than at the next connection.
  async fn sync_workspace_and_recovery(&mut self) -> Result<(), CoreError> {
    let before = self.workspace()?.roster.maybe_org_recovery_key;
    self.sync_workspace().await?;
    if self.workspace()?.roster.maybe_org_recovery_key != before
      && let Err(error) = self.rewrap_for_current_org_key().await
    {
      tracing::warn!(%error, "could not wrap the backup key to the new org recovery key");
    }
    Ok(())
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
        self.persist_sync()?;
        self.note_replayed_event().await
      },
      GatewayServerMessage::SessionInvalid => {
        self.sync.maybe_gateway_session = None;
        self.persist_sync()
      },
      GatewayServerMessage::Error { error } => Err(CoreError::Gateway(error.message)),
      _ => Ok(()),
    }
  }

  /// Counts down the resume replay (the server sends it right after `resumed`, before any live
  /// event); the last replayed event triggers the resync.
  async fn note_replayed_event(&mut self) -> Result<(), CoreError> {
    let Some(left) = self.maybe_replay_left else {
      return Ok(());
    };
    if left > 1 {
      self.maybe_replay_left = Some(left - 1);
      return Ok(());
    }
    self.maybe_replay_left = None;
    self.resync_live_state().await
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
          self.forget_conversation(conversation_id)
        } else {
          // Only a hint: the server's word doesn't make this device sign a removal (security
          // review A8). The remover's client, or an admin's signed change, removes them from the
          // group; if neither comes, the admin console lists who is left in the group (#661).
          self.refresh_conversation(conversation_id).await?;
          self.note_group_leftovers(conversation_id)?;
          if let Err(error) = self.check_group_leftovers(conversation_id).await {
            tracing::debug!(conversation_id = %conversation_id, %error, "could not check users left in the group yet");
          }
          // A meeting guest whose session ended: their leaves go with a signed removal (queued
          // above, only for guests the signed roster doesn't vouch for). A failure is retried on
          // the next sync.
          if let Err(error) = self.apply_meeting_removals(conversation_id).await {
            tracing::warn!(conversation_id = %conversation_id, %error, "removing a guest from the meeting chat's group did not go through yet");
          }
          Ok(())
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
          self.with_queued_own_status(UserPresence {
            user_id,
            state: state.clone(),
            maybe_status,
            maybe_last_seen: None,
          })?,
          self.settings.max_announced_presence,
        )?;
        if state == teamcraft_proto::PresenceState::Offline {
          // The server records "last seen" when the person stops being active; the event doesn't
          // carry it. Forgotten here and read when the UI shows the person (`ui_last_seen`), so
          // people going offline cost no request while nobody looks (issue #252).
          self
            .store
            .delete(Table::Presence, &super::people::last_seen_key(user_id))?;
        } else {
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
      GatewayEvent::WorkspaceUpdate { .. } => self.sync_workspace_and_recovery().await,
      // The user's own device that revoked it signs the removal (`Client::revoke_device`); the
      // user's other devices finish it if the signed device list says so (#232).
      GatewayEvent::DeviceRemove { user_id, device_id } => {
        if device_id == self.account.device_id {
          tracing::warn!("this device was revoked; it will no longer receive messages");
        }
        tracing::debug!(user_id = %user_id, "device revoked on the server");
        if user_id == self.account.user_id {
          self.queue_revocation_check(device_id)?;
        }
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
      // backgrounds, pins): the only way it arrives between connections (issue #252).
      GatewayEvent::KeyBagUpdate { version } if version > self.sync.key_bag_version => {
        self.sync_account_data().await
      },
      GatewayEvent::KeyBagUpdate { .. } => Ok(()),
      // The UI reads the meetings itself (reminders, the meetings view); the mark tells it to now.
      GatewayEvent::MeetingUpdate { .. } => {
        read_state::update_marks(&self.store, |marks| marks.meetings += 1)
      },
      GatewayEvent::LobbyJoin { .. }
      | GatewayEvent::GuestExpired { .. }
      | GatewayEvent::OrgRecoveryUpdate { .. }
      | GatewayEvent::SystemEvent { .. }
      | GatewayEvent::UserGroupUpdate { .. }
      | GatewayEvent::LobbyDecision { .. } => Ok(()),
      // A new name, username or avatar, or the account was deactivated: the cached summary goes.
      GatewayEvent::ProfileUpdate { user_id } => {
        if let Err(error) = self.refresh_user(user_id).await {
          // Not fatal: the next full sync rechecks the people no list names.
          tracing::warn!(%error, "could not refresh a changed profile");
        }
        Ok(())
      },
      GatewayEvent::DeviceLinkRequested { request } => {
        self.note_link_request(request);
        Ok(())
      },
      GatewayEvent::DeviceLinkClosed { request_id, .. } => {
        self.forget_link_request(&request_id);
        Ok(())
      },
      // Call roster and ringing go to the app's call handling (media runs outside the core).
      GatewayEvent::CallEnd {
        conversation_id,
        call_id,
      } => {
        self.forget_call(conversation_id, call_id)?;
        self.emit_call_event(&event);
        Ok(())
      },
      GatewayEvent::CallStart { .. }
      | GatewayEvent::CallUpdate { .. }
      | GatewayEvent::CallRing { .. }
      | GatewayEvent::CallMove { .. } => {
        self.emit_call_event(&event);
        Ok(())
      },
      GatewayEvent::Unknown => Ok(()),
    }
  }
}

impl Default for GatewayTiming {
  fn default() -> Self {
    Self {
      handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
      max_heartbeat: Duration::from_secs(25),
      held_limits: HeldLimits::default(),
    }
  }
}

impl Default for HeldLimits {
  fn default() -> Self {
    Self {
      max_frames: 10_000,
      max_bytes: 16 * 1024 * 1024,
    }
  }
}

impl HeldFrames {
  /// The oldest held frame, if any.
  pub fn pop(&mut self) -> Option<Option<Result<GatewayServerMessage, ApiError>>> {
    let maybe_frame = self.frames.pop_front();
    if self.frames.is_empty() {
      self.bytes = 0;
    }
    maybe_frame
  }

  pub fn is_empty(&self) -> bool {
    self.frames.is_empty()
  }

  /// Heartbeats sent and whether the server was heard since the last call.
  pub fn take_kept_alive(&mut self) -> KeptAlive {
    let kept = KeptAlive {
      heartbeats_sent: self.heartbeats_sent,
      was_heard: self.was_heard,
    };
    self.heartbeats_sent = 0;
    self.was_heard = false;
    kept
  }

  fn hold(&mut self, maybe_frame: Option<Result<GatewayServerMessage, ApiError>>) {
    match &maybe_frame {
      // Only proves the connection is alive; nothing to apply.
      Some(Ok(GatewayServerMessage::HeartbeatAck)) => {
        self.was_heard = true;
        return;
      },
      Some(Ok(frame)) => {
        self.was_heard = true;
        // Only while long work runs; a frame is a few hundred bytes, an MLS message more.
        let size = serde_json::to_vec(frame).map_or(0, |json| json.len());
        self.bytes = self.bytes.saturating_add(size);
      },
      Some(Err(_)) | None => self.is_closed = true,
    }
    self.frames.push_back(maybe_frame);
  }

  fn is_reading(&self, limits: HeldLimits) -> bool {
    !self.is_closed && self.frames.len() < limits.max_frames && self.bytes < limits.max_bytes
  }
}

/// Runs `work` while keeping `connection` alive (issue #325): a heartbeat after `first_heartbeat_in`
/// and then every `heartbeat`, and
/// what arrives meanwhile is read into `held` (within `limits`), so neither the server's
/// heartbeat timeout nor its slow-reader limit drops the connection while a long sync or command
/// holds the loop. Frames are only held, never applied: they come after what `work` does, in their
/// order, once the caller takes them.
pub async fn keep_alive<G: GatewayConnection, T>(
  connection: &mut G,
  first_heartbeat_in: Duration,
  heartbeat: Duration,
  limits: HeldLimits,
  held: &mut HeldFrames,
  work: impl Future<Output = T>,
) -> T {
  let mut work = pin!(work);
  let mut timer = Box::pin(platform::sleep(first_heartbeat_in));
  loop {
    let step = {
      let is_reading = held.is_reading(limits);
      let read = async {
        if is_reading {
          connection.receive().await
        } else {
          std::future::pending().await
        }
      };
      match select(work.as_mut(), select(pin!(read), timer.as_mut())).await {
        Either::Left((output, _)) => return output,
        Either::Right((Either::Left((maybe_frame, _)), _)) => KeepAliveStep::Read(maybe_frame),
        Either::Right((Either::Right(((), _)), _)) => KeepAliveStep::Heartbeat,
      }
    };
    match step {
      KeepAliveStep::Read(maybe_frame) => held.hold(maybe_frame),
      KeepAliveStep::Heartbeat => {
        // A closed connection gets no more heartbeats; the caller replaces it afterwards.
        if !held.is_closed {
          match connection.send(&GatewayClientMessage::Heartbeat).await {
            Ok(()) => held.heartbeats_sent = held.heartbeats_sent.saturating_add(1),
            Err(error) => held.hold(Some(Err(error))),
          }
        }
        timer = Box::pin(platform::sleep(heartbeat));
      },
    }
  }
}

/// What woke [`keep_alive`] other than the end of the work.
enum KeepAliveStep {
  Read(Option<Result<GatewayServerMessage, ApiError>>),
  Heartbeat,
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
    maybe_retry_after: None,
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_deactivated_account_needs_a_sign_in_not_a_retry_loop() {
    let error = rejected(ErrorBody::new(
      ErrorCode::AccountDeactivated,
      "the account is deactivated",
    ));
    assert!(error.is_sign_in_required());
    let error = rejected(ErrorBody::new(ErrorCode::Forbidden, "no"));
    assert!(!error.is_sign_in_required());
  }

  #[test]
  fn heartbeat_answers_are_heard_not_held() {
    let mut held = HeldFrames::default();
    held.hold(Some(Ok(GatewayServerMessage::HeartbeatAck)));
    assert!(held.is_empty());
    assert_eq!(
      held.take_kept_alive(),
      KeptAlive {
        heartbeats_sent: 0,
        was_heard: true
      }
    );
    assert_eq!(held.take_kept_alive(), KeptAlive::default());
  }

  #[test]
  fn frames_are_held_in_order_and_a_close_comes_last() {
    let mut held = HeldFrames::default();
    held.hold(Some(Ok(GatewayServerMessage::SessionInvalid)));
    held.hold(None);
    assert!(!held.is_reading(HeldLimits::default()));
    assert!(matches!(
      held.pop(),
      Some(Some(Ok(GatewayServerMessage::SessionInvalid)))
    ));
    assert!(matches!(held.pop(), Some(None)));
    assert!(held.pop().is_none());
  }

  #[test]
  fn reading_stops_at_the_bound_of_frames() {
    let limits = HeldLimits {
      max_frames: 2,
      max_bytes: usize::MAX,
    };
    let mut held = HeldFrames::default();
    held.hold(Some(Ok(GatewayServerMessage::SessionInvalid)));
    assert!(held.is_reading(limits));
    held.hold(Some(Ok(GatewayServerMessage::SessionInvalid)));
    assert!(!held.is_reading(limits));
  }

  #[test]
  fn reading_stops_at_the_bound_of_bytes_and_starts_over_when_drained() {
    let frame = GatewayServerMessage::SessionInvalid;
    let size = serde_json::to_vec(&frame).unwrap().len();
    let limits = HeldLimits {
      max_frames: usize::MAX,
      max_bytes: size * 2,
    };
    let mut held = HeldFrames::default();
    held.hold(Some(Ok(frame.clone())));
    assert!(held.is_reading(limits));
    held.hold(Some(Ok(frame)));
    assert!(!held.is_reading(limits));
    held.pop();
    held.pop();
    assert!(held.is_reading(limits));
  }
}
