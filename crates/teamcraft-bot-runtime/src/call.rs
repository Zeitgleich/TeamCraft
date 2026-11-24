//! A call as a bot participant: muted, its DAVE key signed by the device key the core holds, only
//! the conversation's verified devices trusted (e2ee.md §8).

use std::sync::Arc;

use teamcraft_core::CallKeySigner;
use teamcraft_crypto::call_binding::{CallBindingError, CallId as CryptoCallId, SignedCallKeyBinding};
use teamcraft_media::audio_io::{AudioSink, AudioSource};
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_auth::{CallBindingSigner, DeviceCallKeyAuthenticator};
use teamcraft_media::call_engine::CallEngineConfig;
use teamcraft_media::call_native::{JoinTicketSource, NativeCallConfig};
use teamcraft_media::call_resilience::JoinTicket;
use teamcraft_proto::{DeviceId, MediaCapabilities, UserId};

/// How to reach the call: the prepared join and the keys.
pub struct CallAccess {
  pub ticket: JoinTicket,
  pub signer: CallKeySigner,
  pub devices: Vec<teamcraft_crypto::identity::VerifiedDevice>,
  pub tickets: Arc<dyn JoinTicketSource>,
}

/// Signs this device's DAVE key with the device key the core holds.
struct CoreSigner(CallKeySigner);

/// The native call of a bot: audio only, silent and muted from the start, received audio going to
/// `sink` (the notetaker takes the frames, the recorder lets the call mix them).
pub fn bot_call_config(
  user_id: UserId,
  device_id: DeviceId,
  access: CallAccess,
  sink: AudioSink,
) -> Result<NativeCallConfig, String> {
  let engine = CallEngineConfig {
    user_id,
    device_id,
    // Audio only: the bots neither send nor watch video.
    capabilities: MediaCapabilities::default(),
    bwe: BweConfig::default(),
  };
  let authenticator =
    DeviceCallKeyAuthenticator::from_verified(Arc::new(CoreSigner(access.signer)), access.devices)
      .map_err(|error| error.to_string())?;
  let mut config =
    NativeCallConfig::device_bound(access.ticket.sfu_url, access.ticket.token, engine, authenticator);
  config.certificate_hashes = access.ticket.certificate_hashes;
  config.source = AudioSource::Silence;
  config.sink = sink;
  config.is_muted_at_start = true;
  config.maybe_ticket_source = Some(access.tickets);
  Ok(config)
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
