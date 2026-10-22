//! Joins calls the way the desktop app does: join token from the server, device-bound DAVE keys
//! (signed by the client core), and the conversation's verified devices as the trusted roster.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use teamcraft_crypto::call_binding::{CallBindingError, CallId as CryptoCallId, SignedCallKeyBinding};
use teamcraft_media::audio_io::{AudioSink, AudioSource};
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_auth::{CallBindingSigner, DeviceCallKeyAuthenticator};
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent, CallStats};
use teamcraft_media::call_native::{NativeCall, NativeCallConfig};
use teamcraft_proto::{ConversationId, DeviceId, Id, MediaCapabilities, Snowflake};

use crate::peer::Peer;

type SignFn =
  dyn Fn(CryptoCallId, &[u8], u64) -> Result<SignedCallKeyBinding, CallBindingError> + Send + Sync;

/// The client core's signer as the media crate's signer trait.
struct CoreSigner(Box<SignFn>);

impl CallBindingSigner for CoreSigner {
  fn sign_call_key_binding(
    &self,
    call_id: CryptoCallId,
    dave_public_key: &[u8],
    not_after: u64,
  ) -> Result<SignedCallKeyBinding, CallBindingError> {
    (self.0)(call_id, dave_public_key, not_after)
  }
}

/// Asks the server for a join token and joins the conversation's call with a test tone.
pub async fn join_call(
  peer: &mut Peer,
  conversation: ConversationId,
  certificate_hash: [u8; 32],
  frequency_hz: f32,
) -> NativeCall {
  let join = peer
    .client
    .call_join_token(conversation)
    .await
    .expect("join token");
  let verified = peer
    .client
    .verified_call_devices(conversation)
    .await
    .expect("verified devices of the conversation");
  let own_device = peer.client.device_id();
  let mut devices = HashMap::new();
  for device in verified {
    let raw = i64::try_from(device.device_id().get()).expect("device id fits");
    let id = DeviceId::from_snowflake(Snowflake::new(raw).expect("non-negative device id"));
    devices.insert(id, device);
  }
  let roster: Vec<DeviceId> = devices.keys().copied().filter(|id| *id != own_device).collect();
  let engine = CallEngineConfig {
    user_id: peer.client.user_id(),
    device_id: own_device,
    capabilities: MediaCapabilities::default(),
    bwe: BweConfig::default(),
  };
  let mut config = NativeCallConfig::new(join.sfu_url, join.token, engine);
  config.maybe_certificate_hash = Some(certificate_hash);
  config.source = AudioSource::Tone {
    frequency_hz,
    amplitude: 0.3,
  };
  config.sink = AudioSink::Discard;
  config.maybe_trusted_roster = Some(roster);
  let signer = peer.client.call_key_signer();
  let core_signer = CoreSigner(Box::new(move |call, key, not_after| {
    signer.sign_call_key_binding(call, key, not_after)
  }));
  config.maybe_authenticator = Some(Arc::new(DeviceCallKeyAuthenticator::new(
    Arc::new(core_signer),
    devices,
  )));
  NativeCall::join(config).await.expect("joining the SFU")
}

/// Lets the call run for `duration`, fails on trust problems, then leaves and returns the statistics.
pub async fn run_call(mut call: NativeCall, duration: Duration) -> CallStats {
  let deadline = tokio::time::Instant::now() + duration;
  let mut is_secured = false;
  while let Ok(Some(event)) = tokio::time::timeout_at(deadline, call.next_event()).await {
    assert!(
      !matches!(
        event,
        CallEngineEvent::UntrustedRoster | CallEngineEvent::UnauthenticatedKeys | CallEngineEvent::Error(_)
      ),
      "{event:?}"
    );
    is_secured |= matches!(event, CallEngineEvent::Secured { .. });
  }
  assert!(is_secured, "the DAVE group was never established");
  call.leave().await.expect("leaving the call")
}
