//! Calls on the desktop: the server's call roster and ringing (from the core's gateway), and the
//! media of the call this device is in (`teamcraft-media`'s native driver: WebTransport to the SFU,
//! the microphone and speakers, Opus, DAVE).
//!
//! [`CallHost`] keeps what the UI shows as a [`CallSnapshot`] and emits
//! [`CALL_CHANGED_EVENT`] whenever it changes; the UI re-reads with `call_snapshot`. Joining asks the
//! core for a join token, the verified device certificates of the conversation and a signer for the
//! call key binding (e2ee.md §8), so DAVE only admits devices the user's contacts certified.

pub mod commands;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

use teamcraft_client_api::calls::{
  ActiveCall, CallRequest, IncomingCall, MediaDevice, MediaDevices, Participant, RunningCall,
};
use teamcraft_client_api::remote::CallSnapshot;
use teamcraft_core::CallKeySigner;
use teamcraft_crypto::call_binding::{CallBindingError, CallId as CryptoCallId, SignedCallKeyBinding};
use teamcraft_media::audio_device::{list_input_devices, list_output_devices};
use teamcraft_media::audio_io::{AudioDeviceChoice, AudioDeviceId, AudioDeviceInfo, AudioSink, AudioSource};
use teamcraft_media::audio_processing::AudioProcessingConfig;
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_auth::{CallBindingSigner, DeviceCallKeyAuthenticator};
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_native::{NativeCall, NativeCallConfig};
use teamcraft_proto::{
  CallRosterEntry, ConversationId, DeviceId, GatewayEvent, Id, MediaCapabilities, Snowflake, UserId,
  VideoTrackKind,
};
use tokio::sync::{mpsc, oneshot};

use crate::core::CallPreparation;

pub use teamcraft_client_api::remote::CALL_CHANGED_EVENT;

/// Development SFUs use self-signed certificates; this variable pins one by its SHA-256 hash (hex,
/// colons allowed). Production SFUs have certificates the system trusts.
const CERT_HASH_VARIABLE: &str = "TEAMCRAFT_SFU_CERT_HASH";

/// The call state of the signed-in user, shared by the Tauri commands and the media task.
pub struct CallHost {
  state: Arc<Mutex<CallState>>,
  notify: Arc<dyn Fn() + Send + Sync>,
  /// The running media call, if any; a mutex across awaits so joins and leaves don't interleave.
  media: tokio::sync::Mutex<Option<MediaCall>>,
}

#[derive(Default)]
struct CallState {
  running: BTreeMap<ConversationId, RunningCall>,
  incoming: BTreeMap<ConversationId, IncomingCall>,
  maybe_active: Option<ActiveCall>,
  devices: MediaDevices,
  microphone: AudioDeviceChoice,
  speakers: AudioDeviceChoice,
}

/// Control of the task that owns the [`NativeCall`].
struct MediaCall {
  controls: mpsc::UnboundedSender<Control>,
  task: tokio::task::JoinHandle<()>,
}

enum Control {
  Mute(bool),
  Deafen(bool),
  Audio {
    source: AudioSource,
    sink: AudioSink,
    done: oneshot::Sender<Result<(), String>>,
  },
  Leave,
}

/// Who is who in the call: the engine reports devices, the UI shows people.
#[derive(Default)]
struct Roster {
  users_by_device: HashMap<DeviceId, UserId>,
  sharing_screen: BTreeSet<DeviceId>,
}

/// Lets the media layer sign with the core's signer (the device key stays in the core).
struct CoreSigner(CallKeySigner);

impl CallHost {
  /// `notify` tells the UI that the call state changed.
  pub fn new(notify: Arc<dyn Fn() + Send + Sync>) -> Self {
    Self {
      state: Arc::new(Mutex::new(CallState::default())),
      notify,
      media: tokio::sync::Mutex::new(None),
    }
  }

  /// For the core runtime: applies the gateway's call events on whatever thread they arrive.
  pub fn gateway_handler(self: &Arc<Self>) -> Arc<dyn Fn(GatewayEvent) + Send + Sync> {
    let host = Arc::clone(self);
    Arc::new(move |event| host.on_gateway_event(event))
  }

  pub fn snapshot(&self) -> CallSnapshot {
    let state = lock(&self.state);
    CallSnapshot {
      maybe_active: state.maybe_active.clone(),
      incoming: state.incoming.values().copied().collect(),
      running: state.running.values().cloned().collect(),
      devices: state.devices.clone(),
    }
  }

  /// Lists microphones and speakers again (devices come and go).
  pub async fn refresh_devices(&self) -> Result<MediaDevices, String> {
    let (inputs, outputs) = tokio::task::spawn_blocking(|| (list_input_devices(), list_output_devices()))
      .await
      .map_err(|error| error.to_string())?;
    let devices = MediaDevices {
      cameras: Vec::new(),
      microphones: inputs
        .map_err(|error| error.to_string())?
        .iter()
        .map(media_device)
        .collect(),
      speakers: outputs
        .map_err(|error| error.to_string())?
        .iter()
        .map(media_device)
        .collect(),
    };
    lock(&self.state).devices = devices.clone();
    self.changed();
    Ok(devices)
  }

  /// Joins the conversation's call with what the core prepared, leaving any other call first.
  pub async fn join(&self, request: CallRequest, preparation: CallPreparation) -> Result<(), String> {
    let mut media = self.media.lock().await;
    if let Some(previous) = media.take() {
      previous.leave().await;
    }
    let (microphone, speakers) = {
      let state = lock(&self.state);
      (state.microphone.clone(), state.speakers.clone())
    };
    let config = native_config(preparation, microphone, speakers)?;
    let call = NativeCall::join(config)
      .await
      .map_err(|error| error.to_string())?;
    {
      let mut state = lock(&self.state);
      state.incoming.remove(&request.conversation);
      state.maybe_active = Some(ActiveCall {
        conversation: request.conversation,
        kind: request.kind,
        is_muted: false,
        is_deafened: false,
        is_secured: false,
        maybe_privacy_code: None,
        participants: Vec::new(),
        active_speakers: Vec::new(),
      });
    }
    let (controls, control_rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_media(
      call,
      control_rx,
      Arc::clone(&self.state),
      Arc::clone(&self.notify),
    ));
    *media = Some(MediaCall { controls, task });
    self.changed();
    Ok(())
  }

  pub async fn leave(&self) {
    if let Some(call) = self.media.lock().await.take() {
      call.leave().await;
    }
    lock(&self.state).maybe_active = None;
    self.changed();
  }

  /// Stops ringing on this device; the call goes on for the others.
  pub fn decline(&self, conversation: ConversationId) {
    lock(&self.state).incoming.remove(&conversation);
    self.changed();
  }

  pub async fn set_muted(&self, is_muted: bool) {
    self.control(Control::Mute(is_muted)).await;
  }

  pub async fn set_deafened(&self, is_deafened: bool) {
    self.control(Control::Deafen(is_deafened)).await;
  }

  /// Remembers the microphone for the next call and switches the running one; `None` follows
  /// the system default.
  pub async fn select_microphone(&self, maybe_device: Option<String>) -> Result<(), String> {
    lock(&self.state).microphone = device_choice(maybe_device);
    self.switch_audio().await
  }

  pub async fn select_speakers(&self, maybe_device: Option<String>) -> Result<(), String> {
    lock(&self.state).speakers = device_choice(maybe_device);
    self.switch_audio().await
  }

  async fn switch_audio(&self) -> Result<(), String> {
    let (source, sink) = {
      let state = lock(&self.state);
      (
        AudioSource::Microphone(state.microphone.clone()),
        AudioSink::Speakers(state.speakers.clone()),
      )
    };
    let media = self.media.lock().await;
    let Some(call) = media.as_ref() else {
      return Ok(());
    };
    let (done, done_rx) = oneshot::channel();
    if call.controls.send(Control::Audio { source, sink, done }).is_err() {
      return Ok(());
    }
    // No answer means the call ended meanwhile; the choice still applies to the next one.
    done_rx.await.unwrap_or(Ok(()))
  }

  async fn control(&self, control: Control) {
    if let Some(call) = self.media.lock().await.as_ref() {
      // A closed channel means the call just ended; nothing left to change.
      let _ = call.controls.send(control);
    }
  }

  fn on_gateway_event(&self, event: GatewayEvent) {
    {
      let mut state = lock(&self.state);
      match event {
        GatewayEvent::CallStart {
          conversation_id,
          is_ringing,
          participants,
          ..
        }
        | GatewayEvent::CallUpdate {
          conversation_id,
          is_ringing,
          participants,
          ..
        } => {
          if !is_ringing {
            state.incoming.remove(&conversation_id);
          }
          state.running.insert(
            conversation_id,
            RunningCall {
              conversation: conversation_id,
              participants: users_of(&participants),
              is_ringing,
            },
          );
        },
        GatewayEvent::CallEnd { conversation_id, .. } => {
          state.running.remove(&conversation_id);
          state.incoming.remove(&conversation_id);
        },
        GatewayEvent::CallRing {
          conversation_id,
          from_user_id,
          ..
        } => {
          let is_in_it = state
            .maybe_active
            .as_ref()
            .is_some_and(|active| active.conversation == conversation_id);
          if !is_in_it {
            state.incoming.insert(
              conversation_id,
              IncomingCall {
                caller: from_user_id,
                conversation: conversation_id,
                with_video: false,
              },
            );
          }
        },
        _ => return,
      }
    }
    self.changed();
  }

  fn changed(&self) {
    (self.notify)();
  }
}

impl MediaCall {
  async fn leave(self) {
    let _ = self.controls.send(Control::Leave);
    if let Err(error) = self.task.await {
      tracing::warn!(%error, "call task failed");
    }
  }
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

/// Owns the call: applies controls and turns engine events into the UI's view of the call.
async fn run_media(
  mut call: NativeCall,
  mut controls: mpsc::UnboundedReceiver<Control>,
  state: Arc<Mutex<CallState>>,
  notify: Arc<dyn Fn() + Send + Sync>,
) {
  let mut roster = Roster::default();
  loop {
    tokio::select! {
      maybe_control = controls.recv() => match maybe_control {
        Some(Control::Mute(is_muted)) => call.set_muted(is_muted),
        Some(Control::Deafen(is_deafened)) => call.set_deafened(is_deafened),
        Some(Control::Audio { source, sink, done }) => {
          let result = call.set_audio(source, sink).await.map_err(|error| error.to_string());
          let _ = done.send(result);
        },
        Some(Control::Leave) | None => break,
      },
      maybe_event = call.next_event() => match maybe_event {
        Some(event) => {
          if apply_event(&mut roster, &state, event) {
            notify();
          }
        },
        // The call ended on its own (the SFU closed it or the network failed).
        None => break,
      },
    }
  }
  match call.leave().await {
    Ok(stats) => tracing::info!(
      frames_sent = stats.frames_sent,
      frames_received = stats.frames_received,
      "left the call"
    ),
    Err(error) => tracing::warn!(%error, "call ended with an error"),
  }
  lock(&state).maybe_active = None;
  notify();
}

/// Updates the active call from one engine event; `true` if the UI should re-read.
fn apply_event(roster: &mut Roster, state: &Mutex<CallState>, event: CallEngineEvent) -> bool {
  let mut state = lock(state);
  let Some(active) = state.maybe_active.as_mut() else {
    return false;
  };
  match event {
    CallEngineEvent::Joined { participants, .. } => {
      for participant in participants {
        roster
          .users_by_device
          .insert(participant.device_id, participant.user_id);
      }
    },
    CallEngineEvent::ParticipantJoined(participant) => {
      roster
        .users_by_device
        .insert(participant.device_id, participant.user_id);
    },
    CallEngineEvent::ParticipantLeft(device_id) => {
      roster.users_by_device.remove(&device_id);
      roster.sharing_screen.remove(&device_id);
    },
    CallEngineEvent::ActiveSpeakers(devices) => {
      let mut speakers = Vec::new();
      for user in devices
        .iter()
        .filter_map(|device| roster.users_by_device.get(device))
      {
        if !speakers.contains(user) {
          speakers.push(*user);
        }
      }
      active.active_speakers = speakers;
    },
    CallEngineEvent::Secured { privacy_code, .. } => {
      active.is_secured = true;
      active.maybe_privacy_code = Some(privacy_code);
    },
    CallEngineEvent::MuteChanged(is_muted) => active.is_muted = is_muted,
    CallEngineEvent::DeafenChanged(is_deafened) => active.is_deafened = is_deafened,
    CallEngineEvent::VideoPublished(track) if track.kind == VideoTrackKind::Screen => {
      roster.sharing_screen.insert(track.device_id);
    },
    CallEngineEvent::Error(error) => {
      tracing::warn!(error_code = ?error.error_code, "the SFU rejected a call request");
      return false;
    },
    CallEngineEvent::UntrustedRoster | CallEngineEvent::UnauthenticatedKeys => {
      // The desktop always passes a roster and an authenticator; seeing these is a bug.
      tracing::warn!(?event, "call runs without full verification");
      return false;
    },
    _ => return false,
  }
  active.participants = participants(roster);
  true
}

fn participants(roster: &Roster) -> Vec<Participant> {
  let mut by_user: BTreeMap<UserId, Participant> = BTreeMap::new();
  for (device, user) in &roster.users_by_device {
    let participant = by_user.entry(*user).or_insert(Participant {
      user: *user,
      // The SFU doesn't report others' mute state yet; silence shows as not speaking.
      is_muted: false,
      is_camera_on: false,
      has_hand_raised: false,
      is_sharing_screen: false,
    });
    participant.is_sharing_screen |= roster.sharing_screen.contains(device);
  }
  by_user.into_values().collect()
}

/// The native call for a prepared join: device-bound DAVE keys, and only the conversation's
/// verified devices as the trusted roster.
fn native_config(
  preparation: CallPreparation,
  microphone: AudioDeviceChoice,
  speakers: AudioDeviceChoice,
) -> Result<NativeCallConfig, String> {
  let mut devices = HashMap::new();
  for device in preparation.devices {
    let id = proto_device_id(device.device_id().get())?;
    devices.insert(id, device);
  }
  let roster: Vec<DeviceId> = devices
    .keys()
    .copied()
    .filter(|device| *device != preparation.device_id)
    .collect();
  let engine = CallEngineConfig {
    user_id: preparation.user_id,
    device_id: preparation.device_id,
    capabilities: MediaCapabilities::default(),
    bwe: BweConfig::default(),
  };
  let mut config = NativeCallConfig::new(preparation.join.sfu_url, preparation.join.token, engine);
  config.maybe_certificate_hash = certificate_hash_from_env()?;
  config.source = AudioSource::Microphone(microphone);
  config.sink = AudioSink::Speakers(speakers);
  config.maybe_processing = Some(AudioProcessingConfig::default());
  config.maybe_trusted_roster = Some(roster);
  config.maybe_authenticator = Some(Arc::new(DeviceCallKeyAuthenticator::new(
    Arc::new(CoreSigner(preparation.signer)),
    devices,
  )));
  Ok(config)
}

fn certificate_hash_from_env() -> Result<Option<[u8; 32]>, String> {
  let Ok(value) = std::env::var(CERT_HASH_VARIABLE) else {
    return Ok(None);
  };
  let hex: String = value.chars().filter(|c| *c != ':').collect();
  if hex.len() != 64 {
    return Err(format!("{CERT_HASH_VARIABLE} needs 32 bytes as hex"));
  }
  let mut hash = [0u8; 32];
  for (index, byte) in hash.iter_mut().enumerate() {
    *byte = u8::from_str_radix(&hex[2 * index..2 * index + 2], 16)
      .map_err(|_| format!("{CERT_HASH_VARIABLE} is not hex"))?;
  }
  Ok(Some(hash))
}

fn proto_device_id(raw: u64) -> Result<DeviceId, String> {
  let raw = i64::try_from(raw).map_err(|_| format!("device id {raw} is out of range"))?;
  Snowflake::new(raw)
    .map(DeviceId::from_snowflake)
    .map_err(|error| error.to_string())
}

fn users_of(entries: &[CallRosterEntry]) -> Vec<UserId> {
  let mut users: Vec<UserId> = entries.iter().map(|entry| entry.user_id).collect();
  users.sort_unstable();
  users.dedup();
  users
}

fn media_device(info: &AudioDeviceInfo) -> MediaDevice {
  MediaDevice {
    id: info.id.to_string(),
    label: info.name.clone(),
  }
}

fn device_choice(maybe_id: Option<String>) -> AudioDeviceChoice {
  match maybe_id {
    Some(id) => AudioDeviceChoice::Device(AudioDeviceId::new(id)),
    None => AudioDeviceChoice::Default,
  }
}

fn lock(state: &Mutex<CallState>) -> std::sync::MutexGuard<'_, CallState> {
  // Poisoning only means a panic mid-update of plain view data; keep going with it.
  state.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{CallId, CallParticipant, MediaStreamId, UnixMillis, VideoCodec, VideoTrack};

  use super::*;

  mod ringing {
    use super::*;

    #[test]
    fn ring_shows_until_the_call_stops_ringing() {
      let host = host();
      host.on_gateway_event(GatewayEvent::CallRing {
        conversation_id: conversation(5),
        call_id: call(),
        from_user_id: user(1),
      });
      assert_eq!(host.snapshot().incoming.len(), 1);
      host.on_gateway_event(GatewayEvent::CallUpdate {
        conversation_id: conversation(5),
        call_id: call(),
        is_ringing: false,
        participants: vec![entry(1, 10), entry(2, 20), entry(2, 21)],
      });
      let snapshot = host.snapshot();
      assert!(snapshot.incoming.is_empty());
      assert_eq!(snapshot.running[0].participants, vec![user(1), user(2)]);
      host.on_gateway_event(GatewayEvent::CallEnd {
        conversation_id: conversation(5),
        call_id: call(),
      });
      assert!(host.snapshot().running.is_empty());
    }

    #[test]
    fn declined_ring_disappears_but_the_call_stays_listed() {
      let host = host();
      host.on_gateway_event(GatewayEvent::CallStart {
        conversation_id: conversation(5),
        call_id: call(),
        started_by: user(1),
        started_at: UnixMillis(0),
        is_ringing: true,
        participants: vec![entry(1, 10)],
      });
      host.on_gateway_event(GatewayEvent::CallRing {
        conversation_id: conversation(5),
        call_id: call(),
        from_user_id: user(1),
      });
      host.decline(conversation(5));
      let snapshot = host.snapshot();
      assert!(snapshot.incoming.is_empty());
      assert_eq!(snapshot.running.len(), 1);
    }
  }

  mod media_events {
    use super::*;

    #[test]
    fn devices_become_people_and_speakers() {
      let state = Mutex::new(CallState {
        maybe_active: Some(active()),
        ..CallState::default()
      });
      let mut roster = Roster::default();
      apply_event(
        &mut roster,
        &state,
        CallEngineEvent::Joined {
          call_id: call(),
          participants: vec![participant(1, 10), participant(1, 11)],
        },
      );
      apply_event(
        &mut roster,
        &state,
        CallEngineEvent::ParticipantJoined(participant(2, 20)),
      );
      apply_event(
        &mut roster,
        &state,
        CallEngineEvent::ActiveSpeakers(vec![device(11), device(10), device(20)]),
      );
      apply_event(
        &mut roster,
        &state,
        CallEngineEvent::VideoPublished(VideoTrack {
          stream_id: MediaStreamId(9),
          device_id: device(20),
          kind: VideoTrackKind::Screen,
          codec: VideoCodec::Vp9,
          layers: Vec::new(),
        }),
      );
      assert!(apply_event(
        &mut roster,
        &state,
        CallEngineEvent::Secured {
          epoch: 1,
          privacy_code: "123".into(),
        },
      ));
      let active = lock(&state).maybe_active.clone().unwrap();
      assert_eq!(active.active_speakers, vec![user(1), user(2)]);
      assert_eq!(active.participants.len(), 2, "one entry per person");
      assert!(
        active
          .participants
          .iter()
          .any(|p| p.user == user(2) && p.is_sharing_screen)
      );
      assert!(active.is_secured);
      assert_eq!(active.maybe_privacy_code.as_deref(), Some("123"));
    }

    #[test]
    fn audio_frames_do_not_wake_the_ui() {
      let state = Mutex::new(CallState {
        maybe_active: Some(active()),
        ..CallState::default()
      });
      let event = CallEngineEvent::UplinkEstimate { bps: 1 };
      assert!(!apply_event(&mut Roster::default(), &state, event));
    }
  }

  #[test]
  fn crypto_device_ids_map_to_wire_ids() {
    assert_eq!(proto_device_id(7).unwrap(), device(7));
    assert!(proto_device_id(u64::MAX).is_err());
  }

  fn host() -> CallHost {
    CallHost::new(Arc::new(|| {}))
  }

  fn active() -> ActiveCall {
    ActiveCall {
      conversation: conversation(5),
      kind: teamcraft_client_api::calls::CallKind::Group,
      is_muted: false,
      is_deafened: false,
      is_secured: false,
      maybe_privacy_code: None,
      participants: Vec::new(),
      active_speakers: Vec::new(),
    }
  }

  fn participant(user_id: i64, device_id: i64) -> CallParticipant {
    CallParticipant {
      user_id: user(user_id),
      device_id: device(device_id),
      audio_stream_id: MediaStreamId(1),
    }
  }

  fn entry(user_id: i64, device_id: i64) -> CallRosterEntry {
    CallRosterEntry {
      user_id: user(user_id),
      device_id: device(device_id),
    }
  }

  fn call() -> CallId {
    CallId::from_snowflake(Snowflake::new(99).unwrap())
  }

  fn conversation(raw: i64) -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn device(raw: i64) -> DeviceId {
    DeviceId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
