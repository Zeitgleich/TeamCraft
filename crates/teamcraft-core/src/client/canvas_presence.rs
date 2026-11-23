//! Sealing and opening the words about who has a page of a channel's notes open (e2ee.md §4.11,
//! "Who has the page open"); [`crate::canvas_presence`] keeps what they say.

use rand_core::OsRng;
use teamcraft_crypto::ephemeral::{SealedSignal, SignalContext, SignalError, open_signal, seal_signal};
use teamcraft_proto::{
  Base64Bytes, CanvasPresence, ConversationId, DeviceId, GatewayClientMessage, MAX_CANVAS_PRESENCE_BYTES,
  UserId,
};
use zeroize::Zeroizing;

use super::Client;
use crate::api::ServerApi;
use crate::canvas_presence::CanvasPresenceState;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::LocalStore;

/// Why a word about the notes wasn't sealed or opened. Nothing the user acts on: a word is
/// replaced by the device's next one.
#[derive(Debug, thiserror::Error)]
pub enum CanvasPresenceError {
  #[error("no content key for the conversation")]
  NoContentKey,
  #[error("the sender device's key isn't known here")]
  UnknownSender,
  #[error("the sealed word is larger than a relay takes")]
  TooLarge,
  #[error("sealing: {0}")]
  Signal(#[from] SignalError),
  #[error("encoding: {0}")]
  Encoding(#[from] serde_json::Error),
  #[error("store: {0}")]
  Core(#[from] Box<CoreError>),
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Who has which page of which channel's notes open, shared: a UI adapter reads it while the
  /// sync loop owns the client.
  pub fn canvas_presence_state(&self) -> CanvasPresenceState {
    self.canvas_presence.clone()
  }

  /// The gateway frame that tells the other members `word`: sealed with the conversation's
  /// current content key and signed by this device.
  pub fn canvas_presence_frame(
    &self,
    conversation: ConversationId,
    word: &CanvasPresence,
  ) -> Result<GatewayClientMessage, CanvasPresenceError> {
    let ring = self
      .account
      .key_bag
      .ring(convert::conversation(conversation))
      .ok_or(CanvasPresenceError::NoContentKey)?;
    let plaintext = Zeroizing::new(serde_json::to_vec(word)?);
    let context = SignalContext {
      conversation_id: convert::conversation(conversation),
      sender_user_id: convert::user(self.account.user_id),
      sender_device_id: convert::device(self.account.device_id),
    };
    let sealed = seal_signal(ring, &context, &plaintext, &self.account.device, &mut OsRng)?.to_bytes()?;
    if sealed.len() > MAX_CANVAS_PRESENCE_BYTES {
      return Err(CanvasPresenceError::TooLarge);
    }
    Ok(GatewayClientMessage::CanvasPresence {
      conversation_id: conversation,
      sealed: Base64Bytes::new(sealed),
    })
  }

  /// Opens what `device` of `user` said about `conversation`'s notes and keeps it. Words this
  /// device can't open (no key, an unknown device, a forged or tampered one) are dropped.
  pub(crate) fn note_canvas_presence(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    device: DeviceId,
    sealed: &[u8],
  ) {
    if user == self.account.user_id {
      return;
    }
    match self.open_canvas_presence(conversation, user, device, sealed) {
      Ok(word) => {
        self
          .canvas_presence
          .note(conversation, user, device, word, self.now_ms());
      },
      Err(error) => tracing::debug!(conversation_id = %conversation, %error, "canvas presence dropped"),
    }
  }

  fn open_canvas_presence(
    &self,
    conversation: ConversationId,
    user: UserId,
    device: DeviceId,
    sealed: &[u8],
  ) -> Result<CanvasPresence, CanvasPresenceError> {
    let ring = self
      .account
      .key_bag
      .ring(convert::conversation(conversation))
      .ok_or(CanvasPresenceError::NoContentKey)?;
    let sender_key = records::device_signing_key(&self.store, user, device)
      .map_err(Box::new)?
      .ok_or(CanvasPresenceError::UnknownSender)?;
    let context = SignalContext {
      conversation_id: convert::conversation(conversation),
      sender_user_id: convert::user(user),
      sender_device_id: convert::device(device),
    };
    let signal = SealedSignal::from_bytes(sealed)?;
    let plaintext = open_signal(ring, &context, &signal, &sender_key)?;
    Ok(serde_json::from_slice(plaintext.expose_secret())?)
  }
}
