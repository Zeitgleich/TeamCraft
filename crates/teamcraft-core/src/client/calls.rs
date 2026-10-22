//! What a call needs from the client (e2ee.md §8): a join token, this device's signature over its
//! per-call DAVE key, and the verified certificates of the devices that may be in the call. Media
//! runs outside the core (`teamcraft-media`); the device's private key stays inside it.

use std::fmt;

use teamcraft_crypto::call_binding::{CallBindingError, CallId as CryptoCallId, SignedCallKeyBinding};
use teamcraft_crypto::identity::{DeviceKeyPair, VerifiedDevice};
use teamcraft_proto::{CallJoinTokenResponse, ConversationId, CreateCallJoinTokenRequest, GatewayEvent};

use super::Client;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::store::LocalStore;
use crate::trust::PinnedDirectory;

/// Receives the gateway's call events (start, update, end, ring) for the app's call handling.
pub type CallEventSink = Box<dyn FnMut(&GatewayEvent)>;

/// Signs call key bindings with this device's key, without handing the key out. Cheap to keep for
/// the length of a call; it can be moved to the thread that runs the media.
pub struct CallKeySigner {
  device: DeviceKeyPair,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Call events from the gateway go to `sink` from now on (one sink; a new one replaces it).
  pub fn set_call_event_sink(&mut self, sink: CallEventSink) {
    self.maybe_call_sink = Some(sink);
  }

  /// A token for joining (or starting) the conversation's call with this device.
  pub async fn call_join_token(
    &self,
    conversation: ConversationId,
  ) -> Result<CallJoinTokenResponse, CoreError> {
    let request = CreateCallJoinTokenRequest {
      device_id: self.account.device_id,
    };
    Ok(self.api.create_call_join_token(conversation, &request).await?)
  }

  pub fn call_key_signer(&self) -> CallKeySigner {
    let (signing, agreement) = self.account.device.to_secrets();
    CallKeySigner {
      device: DeviceKeyPair::from_secrets(
        self.account.device.user_id(),
        self.account.device.device_id(),
        &signing,
        &agreement,
      ),
    }
  }

  /// The devices of the conversation's MLS group, their certificates verified against the pinned
  /// identities: who may be in its call. This device included.
  pub async fn verified_call_devices(
    &mut self,
    conversation: ConversationId,
  ) -> Result<Vec<VerifiedDevice>, CoreError> {
    let users: Vec<_> = self
      .group_or_error(conversation)?
      .members()?
      .into_iter()
      .map(|member| crate::convert::user_from_crypto(member.user_id))
      .collect::<Result<_, _>>()?;
    let directory: PinnedDirectory = self.pin_identities(users).await?;
    Ok(self.group_or_error(conversation)?.verified_members(&directory)?)
  }

  pub(super) fn emit_call_event(&mut self, event: &GatewayEvent) {
    if let Some(sink) = &mut self.maybe_call_sink {
      sink(event);
    }
  }
}

impl CallKeySigner {
  pub fn sign_call_key_binding(
    &self,
    call_id: CryptoCallId,
    dave_public_key: &[u8],
    not_after: u64,
  ) -> Result<SignedCallKeyBinding, CallBindingError> {
    self
      .device
      .sign_call_key_binding(call_id, dave_public_key, not_after)
  }
}

// The key pair must not end up in logs.
impl fmt::Debug for CallKeySigner {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("CallKeySigner").finish_non_exhaustive()
  }
}
