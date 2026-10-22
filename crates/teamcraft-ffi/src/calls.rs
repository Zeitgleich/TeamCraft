//! Calls for the mobile apps: the engine thread prepares a join (token, device signer, verified
//! devices; e2ee.md §8) and the media runs in `teamcraft-media`'s native driver on a call thread of
//! its own. The app integrates the call with the platform (Android Telecom) and drives it through
//! [`CallSession`]; [`CallListener`] tells it about rings and the call's state.

use std::sync::{Arc, Mutex, MutexGuard};

use teamcraft_core::client::CallKeySigner;
use teamcraft_crypto::call_binding::{CallBindingError, CallId as CryptoCallId, SignedCallKeyBinding};
use teamcraft_crypto::identity::VerifiedDevice;
use teamcraft_media::audio_io::{AudioDeviceChoice, AudioSink, AudioSource};
use teamcraft_media::audio_processing::AudioProcessingConfig;
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_auth::{CallBindingSigner, DeviceCallKeyAuthenticator};
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_native::{NativeCall, NativeCallConfig};
use teamcraft_proto::{
  AudioCodec, CallJoinTokenResponse, ConversationId, DeviceId, GatewayEvent, MediaCapabilities, UserId,
};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::error::TeamCraftError;

/// Worker threads of a call's runtime: network and engine; audio has its own thread.
const CALL_WORKER_THREADS: usize = 2;

/// Told about rings and about the call this device is in. Called from the engine and call
/// threads; implementations hand over to their UI thread.
#[uniffi::export(with_foreign)]
pub trait CallListener: Send + Sync {
  fn on_call_state(&self, state: CallStateInfo);
  /// Someone calls this user in a conversation (ring it, e.g. as an incoming Telecom call).
  fn on_incoming_call(&self, call: IncomingCallInfo);
  /// The conversation's call stopped ringing (answered elsewhere, declined or ended).
  fn on_ring_ended(&self, conversation_id: String);
}

/// The app's handle on the call this device is in.
#[derive(uniffi::Object)]
pub struct CallSession {
  controls: mpsc::UnboundedSender<Control>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CallStateInfo {
  pub conversation_id: String,
  pub phase: CallPhase,
  pub is_muted: bool,
  /// End-to-end encrypted with every participant's device verified.
  pub is_secured: bool,
  /// Other devices in the call.
  pub participant_count: u32,
  /// The code participants compare to rule out a man in the middle (once secured).
  pub maybe_privacy_code: Option<String>,
  /// Why the call ended (with [`CallPhase::Ended`]).
  pub maybe_end_reason: Option<CallEndReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum CallPhase {
  Connecting,
  Connected,
  Ended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum CallEndReason {
  Left,
  RemovedByHost,
  EndedByHost,
  /// The connection to the call broke.
  Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct IncomingCallInfo {
  pub conversation_id: String,
  pub caller_user_id: String,
}

/// Everything joining a conversation's call needs from the core (e2ee.md §8).
pub(crate) struct CallPreparation {
  pub(crate) conversation: ConversationId,
  pub(crate) join: CallJoinTokenResponse,
  pub(crate) user_id: UserId,
  pub(crate) device_id: DeviceId,
  /// Signs this device's per-call DAVE key; the device key itself stays in the core.
  pub(crate) signer: CallKeySigner,
  /// The conversation's devices (this one included) whose certificates verified.
  pub(crate) devices: Vec<VerifiedDevice>,
}

/// Where call events meet the app's listener: the engine feeds it gateway events, call threads
/// feed it the call's state.
#[derive(Clone, Default)]
pub(crate) struct CallHub {
  inner: Arc<Mutex<HubState>>,
}

#[derive(Default)]
struct HubState {
  maybe_listener: Option<Arc<dyn CallListener>>,
  maybe_active: Option<ConversationId>,
}

enum Control {
  SetMuted(bool),
  Leave,
}

/// Lets the media layer sign with the core's signer.
struct CoreSigner(CallKeySigner);

#[uniffi::export]
impl CallSession {
  pub fn set_muted(&self, is_muted: bool) {
    self.send(Control::SetMuted(is_muted));
  }

  /// Leaves the call; the listener gets the ended state.
  pub fn leave(&self) {
    self.send(Control::Leave);
  }
}

impl CallSession {
  fn send(&self, control: Control) {
    // The call already ended if its thread is gone; the listener was told then.
    let _ = self.controls.send(control);
  }
}

impl CallHub {
  pub(crate) fn set_listener(&self, listener: Arc<dyn CallListener>) {
    lock(&self.inner).maybe_listener = Some(listener);
  }

  /// Rings from the gateway, for the core's call event sink.
  pub(crate) fn on_gateway_event(&self, event: &GatewayEvent) {
    let (maybe_listener, maybe_active) = {
      let state = lock(&self.inner);
      (state.maybe_listener.clone(), state.maybe_active)
    };
    let Some(listener) = maybe_listener else {
      return;
    };
    match event {
      GatewayEvent::CallRing {
        conversation_id,
        from_user_id,
        ..
      } if maybe_active != Some(*conversation_id) => listener.on_incoming_call(IncomingCallInfo {
        conversation_id: conversation_id.to_string(),
        caller_user_id: from_user_id.to_string(),
      }),
      GatewayEvent::CallUpdate {
        conversation_id,
        is_ringing: false,
        ..
      }
      | GatewayEvent::CallEnd { conversation_id, .. } => listener.on_ring_ended(conversation_id.to_string()),
      _ => {},
    }
  }

  fn report(&self, state: CallStateInfo) {
    let maybe_listener = {
      let mut hub = lock(&self.inner);
      if state.phase == CallPhase::Ended {
        hub.maybe_active = None;
      }
      hub.maybe_listener.clone()
    };
    if let Some(listener) = maybe_listener {
      listener.on_call_state(state);
    }
  }
}

/// Joins the prepared call on a new call thread; returns once connected.
pub(crate) async fn join(
  preparation: CallPreparation,
  hub: CallHub,
) -> Result<Arc<CallSession>, TeamCraftError> {
  let conversation = preparation.conversation;
  let config = native_config(preparation)?;
  let (controls, control_rx) = mpsc::unbounded_channel();
  let (joined, joined_rx) = oneshot::channel();
  lock(&hub.inner).maybe_active = Some(conversation);
  let mut state = CallStateInfo {
    conversation_id: conversation.to_string(),
    phase: CallPhase::Connecting,
    is_muted: false,
    is_secured: false,
    participant_count: 0,
    maybe_privacy_code: None,
    maybe_end_reason: None,
  };
  hub.report(state.clone());
  let thread_hub = hub.clone();
  let spawned = std::thread::Builder::new()
    .name("teamcraft-call".into())
    .spawn(move || {
      let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(CALL_WORKER_THREADS)
        .enable_all()
        .build()
      {
        Ok(runtime) => runtime,
        Err(error) => {
          // The joining caller waits for this; if it has gone, nobody needs the answer.
          let _ = joined.send(Err(TeamCraftError::Call(error.to_string())));
          return;
        },
      };
      runtime.block_on(async move {
        let call = match NativeCall::join(config).await {
          Ok(call) => call,
          Err(error) => {
            let _ = joined.send(Err(TeamCraftError::Call(error.to_string())));
            return;
          },
        };
        let _ = joined.send(Ok(()));
        run_call(call, control_rx, &thread_hub, &mut state).await;
      });
    });
  if let Err(error) = spawned {
    lock(&hub.inner).maybe_active = None;
    return Err(TeamCraftError::Call(error.to_string()));
  }
  let result = joined_rx.await.unwrap_or(Err(TeamCraftError::EngineStopped));
  if let Err(error) = &result {
    warn!(%error, "joining the call failed");
    hub.report(ended(conversation, CallEndReason::Failed));
  }
  result.map(|()| Arc::new(CallSession { controls }))
}

/// Applies the app's controls and reports the engine's events until the call ends.
async fn run_call(
  mut call: NativeCall,
  mut controls: mpsc::UnboundedReceiver<Control>,
  hub: &CallHub,
  state: &mut CallStateInfo,
) {
  let mut reason = CallEndReason::Left;
  loop {
    tokio::select! {
      maybe_control = controls.recv() => match maybe_control {
        Some(Control::SetMuted(is_muted)) => call.set_muted(is_muted),
        // Leave asked for, or the app dropped the session.
        Some(Control::Leave) | None => break,
      },
      maybe_event = call.next_event() => match maybe_event {
        Some(event) => {
          if let Some(end) = apply_event(state, event) {
            reason = end;
            break;
          }
          hub.report(state.clone());
        },
        None => {
          reason = CallEndReason::Failed;
          break;
        },
      },
    }
  }
  if let Err(error) = call.leave().await {
    info!(%error, "the call ended with an error");
  }
  state.phase = CallPhase::Ended;
  state.maybe_end_reason = Some(reason);
  hub.report(state.clone());
}

/// Updates the call's state; returns why the call ends if the event ends it.
fn apply_event(state: &mut CallStateInfo, event: CallEngineEvent) -> Option<CallEndReason> {
  match event {
    CallEngineEvent::Joined { participants, .. } => {
      state.phase = CallPhase::Connected;
      state.participant_count = count(participants.len());
    },
    CallEngineEvent::ParticipantJoined(_) => state.participant_count += 1,
    CallEngineEvent::ParticipantLeft(_) => {
      state.participant_count = state.participant_count.saturating_sub(1)
    },
    CallEngineEvent::Secured { privacy_code, .. } => {
      state.is_secured = true;
      state.maybe_privacy_code = Some(privacy_code);
    },
    CallEngineEvent::MuteChanged(is_muted) => state.is_muted = is_muted,
    CallEngineEvent::RemovedByHost { .. } => return Some(CallEndReason::RemovedByHost),
    CallEngineEvent::EndedByHost { .. } => return Some(CallEndReason::EndedByHost),
    CallEngineEvent::Error(error) => warn!(error_code = ?error.error_code, "the SFU rejected a call request"),
    _ => {},
  }
  None
}

fn ended(conversation: ConversationId, reason: CallEndReason) -> CallStateInfo {
  CallStateInfo {
    conversation_id: conversation.to_string(),
    phase: CallPhase::Ended,
    is_muted: false,
    is_secured: false,
    participant_count: 0,
    maybe_privacy_code: None,
    maybe_end_reason: Some(reason),
  }
}

/// Audio-only for now: the phone's default microphone and output (Telecom routes them).
fn native_config(preparation: CallPreparation) -> Result<NativeCallConfig, TeamCraftError> {
  let engine = CallEngineConfig {
    user_id: preparation.user_id,
    device_id: preparation.device_id,
    capabilities: MediaCapabilities {
      audio_codecs: vec![AudioCodec::Opus],
      ..MediaCapabilities::default()
    },
    bwe: BweConfig::default(),
  };
  let authenticator =
    DeviceCallKeyAuthenticator::from_verified(Arc::new(CoreSigner(preparation.signer)), preparation.devices)
      .map_err(|error| TeamCraftError::Call(error.to_string()))?;
  let mut config = NativeCallConfig::device_bound(
    preparation.join.sfu_url,
    preparation.join.token,
    engine,
    authenticator,
  );
  config.source = AudioSource::Microphone(AudioDeviceChoice::Default);
  config.sink = AudioSink::Speakers(AudioDeviceChoice::Default);
  config.maybe_processing = Some(AudioProcessingConfig::default());
  Ok(config)
}

fn count(len: usize) -> u32 {
  u32::try_from(len).unwrap_or(u32::MAX)
}

impl CallBindingSigner for CoreSigner {
  fn sign_call_key_binding(
    &self,
    call_id: CryptoCallId,
    dave_public_key: &[u8],
    not_after: u64,
  ) -> Result<SignedCallKeyBinding, CallBindingError> {
    self.0.sign_call_key_binding(call_id, dave_public_key, not_after)
  }
}

fn lock(state: &Mutex<HubState>) -> MutexGuard<'_, HubState> {
  // A panicking listener can't leave the hub inconsistent: each update is a single assignment.
  state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{CallId, Id, Snowflake};

  use super::*;

  #[test]
  fn rings_reach_the_listener_unless_in_that_call() {
    let hub = CallHub::default();
    let listener = Arc::new(RecordingListener::default());
    hub.set_listener(listener.clone());

    hub.on_gateway_event(&ring(5));
    lock(&hub.inner).maybe_active = Some(conversation(6));
    hub.on_gateway_event(&ring(6));
    hub.on_gateway_event(&GatewayEvent::CallEnd {
      conversation_id: conversation(5),
      call_id: call(1),
    });

    assert_eq!(*listener.incoming.lock().unwrap(), vec!["5".to_string()]);
    assert_eq!(*listener.ring_ended.lock().unwrap(), vec!["5".to_string()]);
  }

  #[test]
  fn engine_events_update_the_state() {
    let mut state = ended(conversation(5), CallEndReason::Left);
    state.phase = CallPhase::Connecting;
    assert_eq!(apply_event(&mut state, CallEngineEvent::MuteChanged(true)), None);
    assert!(state.is_muted);
    assert_eq!(
      apply_event(&mut state, CallEngineEvent::EndedByHost { by_user_id: user(1) }),
      Some(CallEndReason::EndedByHost)
    );
  }

  #[derive(Default)]
  struct RecordingListener {
    incoming: Mutex<Vec<String>>,
    ring_ended: Mutex<Vec<String>>,
  }

  impl CallListener for RecordingListener {
    fn on_call_state(&self, _state: CallStateInfo) {}

    fn on_incoming_call(&self, call: IncomingCallInfo) {
      self.incoming.lock().unwrap().push(call.conversation_id);
    }

    fn on_ring_ended(&self, conversation_id: String) {
      self.ring_ended.lock().unwrap().push(conversation_id);
    }
  }

  fn ring(conversation_id: i64) -> GatewayEvent {
    GatewayEvent::CallRing {
      conversation_id: conversation(conversation_id),
      call_id: call(1),
      from_user_id: user(2),
    }
  }

  fn conversation(raw: i64) -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn call(raw: i64) -> CallId {
    CallId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
