//! Joins calls the way the desktop app does: join token from the server, device-bound DAVE keys
//! (signed by the client core), and the conversation's verified devices as the trusted roster.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use teamcraft_core::api::{HttpServerApi, ServerApi};
use teamcraft_crypto::call_binding::{CallBindingError, CallId as CryptoCallId, SignedCallKeyBinding};
use teamcraft_media::audio_io::{AudioSink, AudioSource};
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_auth::{CallBindingSigner, DeviceCallKeyAuthenticator};
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent, CallStats};
use teamcraft_media::call_native::{JoinTicketSource, NativeCall, NativeCallConfig, TicketFuture};
use teamcraft_media::call_resilience::{JoinTicket, JoinTicketError};
use teamcraft_media::fallback_link::{FallbackSocket, fallback_opener};
use teamcraft_proto::{
  ConversationId, CreateCallJoinTokenRequest, DeviceId, Id, MediaCapabilities, Snowflake,
};

use crate::peer::Peer;
use crate::tokens::UserTokens;

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

/// A self-signed SFU certificate valid for ten days (certificate hash pins, the native clients'
/// and the browsers', accept at most two weeks), with its SHA-256 as the server hands it out
/// (`[calls] sfu_certificate_sha256`).
pub struct SfuIdentity {
  pub certificate_pem: String,
  pub key_pem: String,
  /// 64 lowercase hex digits.
  pub sha256_hex: String,
}

/// Fresh join tickets from the server, as the apps fetch them for a rejoin.
struct ServerTickets {
  api: Arc<HttpServerApi<UserTokens>>,
  conversation: ConversationId,
  device_id: DeviceId,
}

impl SfuIdentity {
  pub fn generate() -> Self {
    let mut params = rcgen::CertificateParams::new(vec!["localhost".to_owned(), "::1".to_owned()])
      .expect("certificate names");
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(9);
    let signing_key = rcgen::KeyPair::generate().expect("a key pair");
    let cert = params
      .self_signed(&signing_key)
      .expect("a self-signed certificate");
    Self {
      sha256_hex: Sha256::digest(cert.der())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect(),
      certificate_pem: cert.pem(),
      key_pem: signing_key.serialize_pem(),
    }
  }
}

impl JoinTicketSource for ServerTickets {
  fn fetch(&self) -> TicketFuture {
    let (api, conversation, device_id) = (Arc::clone(&self.api), self.conversation, self.device_id);
    Box::pin(async move {
      let join = api
        .create_call_join_token(conversation, &CreateCallJoinTokenRequest { device_id })
        .await
        .map_err(|error| JoinTicketError {
          reason: error.to_string(),
        })?;
      Ok(JoinTicket::from(join))
    })
  }
}

/// Asks the server for a join token and joins the conversation's call with a test tone.
pub async fn join_call(
  peer: &mut Peer,
  conversation: ConversationId,
  certificate_hash: [u8; 32],
  frequency_hz: f32,
) -> NativeCall {
  let config = call_config(peer, conversation, frequency_hz).await;
  join_with(config, Some(certificate_hash)).await
}

/// Joins as the apps do: every connection, the first and each rejoin, accepts only the SFU
/// certificates its own join ticket names, and a rejoin fetches a fresh ticket from the server
/// at `base_url` (an SFU restarted with a new certificate).
pub async fn join_call_with_tickets(
  peer: &mut Peer,
  base_url: &str,
  conversation: ConversationId,
  frequency_hz: f32,
) -> NativeCall {
  let mut config = call_config(peer, conversation, frequency_hz).await;
  config.maybe_ticket_source = Some(Arc::new(ServerTickets {
    api: Arc::new(HttpServerApi::new(base_url, peer.tokens.clone())),
    conversation,
    device_id: peer.client.device_id(),
  }));
  join_with(config, None).await
}

async fn call_config(peer: &mut Peer, conversation: ConversationId, frequency_hz: f32) -> NativeCallConfig {
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
  let ticket = JoinTicket::from(join);
  let mut config = NativeCallConfig::new(ticket.sfu_url, ticket.token, engine);
  config.maybe_fallback_url = ticket.maybe_fallback_url;
  config.certificate_hashes = ticket.certificate_hashes;
  // As the apps open it: the core's HTTP stack, with its proxy handling.
  config.maybe_fallback_opener = Some(fallback_opener(|url, protocols| async move {
    let socket = teamcraft_core::api::open_call_socket(&url, &protocols)
      .await
      .map_err(|error| error.to_string())?;
    Ok(FallbackSocket::new(socket))
  }));
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
  config
}

/// `maybe_certificate_hash`: a certificate accepted besides the ticket's (an SFU whose own
/// self-signed one the server doesn't know).
async fn join_with(mut config: NativeCallConfig, maybe_certificate_hash: Option<[u8; 32]>) -> NativeCall {
  config.maybe_certificate_hash = maybe_certificate_hash;
  NativeCall::join(config).await.expect("joining the SFU")
}

/// Lets the call run for `duration`, fails on trust problems, then leaves and returns the statistics.
/// Like [`run_call`] for a call a person joins on a device by hand: whatever happens is reported
/// (they may not answer, or not in time), nothing fails. The events seen, then the statistics.
pub async fn hold_call(mut call: NativeCall, duration: Duration) -> (Vec<String>, Option<CallStats>) {
  let deadline = tokio::time::Instant::now() + duration;
  let mut events = Vec::new();
  while let Ok(Some(event)) = tokio::time::timeout_at(deadline, call.next_event()).await {
    if !matches!(
      event,
      CallEngineEvent::AudioFrame { .. }
        | CallEngineEvent::VideoFrame(_)
        | CallEngineEvent::ActiveSpeakers(_)
    ) {
      events.push(format!("{event:?}"));
    }
  }
  (events, call.leave().await.ok())
}

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
