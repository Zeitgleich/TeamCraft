//! Adding a device by linking it to an existing one (e2ee.md §5).
//!
//! The new device creates a [`LinkOffer`] and shows it as a QR code (or the approval prompt carries
//! it). It joins the server's relay with [`NewDeviceLink::connect`]; the existing device scans the
//! code and joins with [`Client::start_link_approval`]. After the handshake both screens show the
//! same short code; once the user confirmed it on the existing device,
//! [`Client::complete_link_approval`] transfers the backup key and the key bag, and the new device
//! enrolls with them ([`crate::EnrollmentOptions::maybe_linked`]).

use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use teamcraft_crypto::key_bag::KeyBagBinding;
use teamcraft_crypto::rendezvous::{Initiator, RendezvousChannel, RendezvousKey, Responder};
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{Base64Bytes, RendezvousClientMessage, RendezvousServerMessage};
use zeroize::Zeroizing;

use super::Client;
use crate::api::{RendezvousConnection, RendezvousFrame, ServerApi};
use crate::error::CoreError;
use crate::records;
use crate::store::LocalStore;

const OFFER_PREFIX: &str = "tclink1";

/// What the QR code carries: where to meet (relay ID) and the handshake's authentication key.
#[derive(Debug)]
pub struct LinkOffer {
  rendezvous_id: String,
  key: RendezvousKey,
}

/// What the existing device hands over: enough to decrypt the user's key bag.
pub struct LinkedSecrets {
  pub(crate) backup_secret: SymmetricKey,
  pub(crate) encrypted_key_bag: Vec<u8>,
  pub(crate) key_bag_version: u64,
  /// The wrapped backup keys the bag is bound to.
  pub(crate) wrapped_keys: Vec<u8>,
}

/// The new device's side after the handshake.
pub struct NewDeviceLink<R> {
  connection: R,
  channel: RendezvousChannel,
}

/// The existing device's side after the handshake, waiting for the user to confirm the code.
pub struct LinkApproval<R> {
  connection: R,
  channel: RendezvousChannel,
}

#[derive(Serialize, Deserialize)]
struct TransferPayload {
  backup_key: Base64Bytes,
  encrypted_key_bag: Base64Bytes,
  key_bag_version: u64,
  wrapped_backup_keys: Base64Bytes,
}

impl LinkOffer {
  pub fn generate() -> Self {
    let mut id = [0u8; 16];
    OsRng.fill_bytes(&mut id);
    Self {
      rendezvous_id: hex(&id),
      key: RendezvousKey::generate(&mut OsRng),
    }
  }

  /// The QR code text. Contains the authentication key: show it, never send it anywhere else.
  pub fn to_code(&self) -> String {
    format!(
      "{OFFER_PREFIX}:{}:{}",
      self.rendezvous_id,
      hex(self.key.expose_secret())
    )
  }

  pub fn parse(code: &str) -> Result<Self, CoreError> {
    let invalid = || CoreError::Corrupt("not a device link code".into());
    let mut parts = code.trim().split(':');
    if parts.next() != Some(OFFER_PREFIX) {
      return Err(invalid());
    }
    let rendezvous_id = parts
      .next()
      .filter(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
      .ok_or_else(invalid)?;
    let key_hex = parts.next().ok_or_else(invalid)?;
    let key = Zeroizing::new(
      <[u8; 32]>::try_from(unhex(key_hex).ok_or_else(invalid)?.as_slice()).map_err(|_| invalid())?,
    );
    if parts.next().is_some() {
      return Err(invalid());
    }
    Ok(Self {
      rendezvous_id: rendezvous_id.to_string(),
      key: RendezvousKey::from_bytes(*key),
    })
  }
}

impl<R: RendezvousConnection> NewDeviceLink<R> {
  /// Joins the relay and runs the handshake as initiator; returns once the existing device answered.
  pub async fn connect<A: ServerApi<Rendezvous = R>>(api: &A, offer: &LinkOffer) -> Result<Self, CoreError> {
    let mut connection = join(api, offer).await?;
    let initiator = Initiator::new(&offer.key, &mut OsRng);
    let hello = next_binary(&mut connection).await?;
    let (awaiting, auth_hello) = initiator.receive_hello(&hello, &mut OsRng).map_err(link_error)?;
    connection.send_binary(auth_hello).await?;
    let auth = next_binary(&mut connection).await?;
    let channel = awaiting.receive_auth(&auth).map_err(link_error)?;
    Ok(Self { connection, channel })
  }

  /// Show this; the user confirms it matches the code on the other device.
  pub fn short_code(&self) -> String {
    self.channel.short_code()
  }

  /// Waits for the transfer the existing device sends after confirmation.
  pub async fn receive_secrets(mut self) -> Result<LinkedSecrets, CoreError> {
    let frame = next_binary(&mut self.connection).await?;
    let plaintext = self.channel.open(&frame).map_err(link_error)?;
    let payload: TransferPayload = serde_json::from_slice(plaintext.expose_secret())
      .map_err(|error| CoreError::Corrupt(error.to_string()))?;
    let backup_secret = SymmetricKey::from_slice(payload.backup_key.as_slice())
      .ok_or_else(|| CoreError::Corrupt("bad backup key".into()))?;
    Ok(LinkedSecrets {
      backup_secret,
      encrypted_key_bag: payload.encrypted_key_bag.into_vec(),
      key_bag_version: payload.key_bag_version,
      wrapped_keys: payload.wrapped_backup_keys.into_vec(),
    })
  }
}

impl<R> LinkApproval<R> {
  pub fn short_code(&self) -> String {
    self.channel.short_code()
  }
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Scans a new device's code, joins the relay and runs the handshake as responder.
  pub async fn start_link_approval(&self, code: &str) -> Result<LinkApproval<A::Rendezvous>, CoreError> {
    let offer = LinkOffer::parse(code)?;
    let mut connection = join(&self.api, &offer).await?;
    let (responder, hello) = Responder::new(&offer.key, &mut OsRng).map_err(link_error)?;
    connection.send_binary(hello).await?;
    let auth_hello = next_binary(&mut connection).await?;
    let (channel, auth) = responder
      .receive_auth_hello(&auth_hello, &mut OsRng)
      .map_err(link_error)?;
    connection.send_binary(auth).await?;
    Ok(LinkApproval { connection, channel })
  }

  /// After the user confirmed that both short codes match: sends the backup key and the current key
  /// bag to the new device.
  pub async fn complete_link_approval(
    &mut self,
    mut approval: LinkApproval<A::Rendezvous>,
  ) -> Result<(), CoreError> {
    // Hand over the newest content keys, and the version the server has, so the new device's next
    // upload doesn't conflict.
    if self.sync.is_key_bag_dirty {
      self.upload_key_bag().await?;
    }
    let wrapped_keys = self.wrapped_keys_blob().await?;
    let encrypted = self.account.key_bag.encrypt(
      &self.account.backup_key(),
      KeyBagBinding {
        sequence: self.sync.key_bag_sequence,
        wrapped_keys: &wrapped_keys,
      },
      &mut OsRng,
    )?;
    let payload = TransferPayload {
      backup_key: Base64Bytes::new(self.account.backup_secret.expose_secret().to_vec()),
      encrypted_key_bag: Base64Bytes::new(encrypted.to_bytes()?),
      key_bag_version: self.sync.key_bag_version,
      wrapped_backup_keys: Base64Bytes::new(wrapped_keys),
    };
    let plaintext =
      Zeroizing::new(serde_json::to_vec(&payload).map_err(|error| CoreError::Corrupt(error.to_string()))?);
    let frame = approval.channel.seal(&plaintext).map_err(link_error)?;
    approval.connection.send_binary(frame).await?;
    records::save_key_bag(&self.store, &self.account)
  }
}

impl std::fmt::Debug for LinkedSecrets {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("LinkedSecrets([redacted])")
  }
}

async fn join<A: ServerApi>(api: &A, offer: &LinkOffer) -> Result<A::Rendezvous, CoreError> {
  let mut connection = api.connect_rendezvous().await?;
  connection
    .send_control(&RendezvousClientMessage::Join {
      token: api.gateway_token()?,
      rendezvous_id: offer.rendezvous_id.clone(),
    })
    .await?;
  Ok(connection)
}

/// The next binary frame, skipping control frames that only report progress.
async fn next_binary<R: RendezvousConnection>(connection: &mut R) -> Result<Vec<u8>, CoreError> {
  loop {
    match connection.receive().await {
      Some(Ok(RendezvousFrame::Binary(frame))) => return Ok(frame),
      Some(Ok(RendezvousFrame::Control(
        RendezvousServerMessage::Joined { .. } | RendezvousServerMessage::PeerJoined,
      ))) => {},
      Some(Ok(RendezvousFrame::Control(RendezvousServerMessage::Error { error }))) => {
        return Err(CoreError::Gateway(error.message));
      },
      Some(Ok(RendezvousFrame::Control(other))) => {
        return Err(CoreError::Gateway(format!("relay ended: {other:?}")));
      },
      Some(Err(error)) => return Err(error.into()),
      None => return Err(CoreError::Gateway("relay closed".into())),
    }
  }
}

fn link_error(error: teamcraft_crypto::rendezvous::RendezvousError) -> CoreError {
  CoreError::Gateway(format!("device link: {error}"))
}

fn hex(bytes: &[u8]) -> String {
  bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
  if !text.len().is_multiple_of(2) {
    return None;
  }
  (0..text.len())
    .step_by(2)
    .map(|index| u8::from_str_radix(text.get(index..index + 2)?, 16).ok())
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn offer_code_round_trips_and_rejects_garbage() {
    let offer = LinkOffer::generate();
    let parsed = LinkOffer::parse(&offer.to_code()).unwrap();
    assert_eq!(parsed.rendezvous_id, offer.rendezvous_id);
    assert_eq!(parsed.key.expose_secret(), offer.key.expose_secret());
    for garbage in [
      "",
      "tclink1",
      "tclink2:00:00",
      "tclink1:zz:00",
      &format!("{}:extra", offer.to_code()),
    ] {
      assert!(LinkOffer::parse(garbage).is_err(), "{garbage}");
    }
  }
}
