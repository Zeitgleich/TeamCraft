//! Adding a device by linking it to an existing one (e2ee.md §5).
//!
//! The new device creates a [`LinkOffer`] and shows it as a QR code (or the approval prompt carries
//! it). It joins the server's relay with [`NewDeviceLink::connect`]; the existing device scans the
//! code and joins with [`Client::start_link_approval`]. After the handshake both screens show the
//! same short code; once the user confirmed it on the existing device,
//! [`Client::complete_link_approval`] transfers the backup key and the key bag. The new device
//! checks that the key bag opens and answers with a receipt; only that receipt makes the existing
//! device report success, so a lost frame fails on both sides. Before it answers, the new device
//! stores the secrets (encrypted like the account, issue #168); it then enrolls with them
//! ([`crate::EnrollmentSecret::Linked`]). If it stops before the account is stored, its next start
//! finishes enrolling with the stored secrets ([`crate::PendingEnrollment::resume_link`]) instead
//! of asking for another link the existing device already reported as done.
//!
//! The protocol version is in the code itself (`tclink2`): a device that speaks another version
//! refuses the code when it is scanned, before any key leaves the existing device.

use std::time::Duration;

use futures_util::future::{Either, select};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use teamcraft_crypto::backup::BackupKey;
use teamcraft_crypto::key_bag::{EncryptedKeyBag, KeyBagBinding};
use teamcraft_crypto::rendezvous::{Initiator, RendezvousChannel, RendezvousKey, Responder};
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{Base64Bytes, RendezvousClientMessage, RendezvousServerMessage, UserId};
use zeroize::Zeroizing;

use super::Client;
use crate::api::{RendezvousConnection, RendezvousFrame, ServerApi};
use crate::error::CoreError;
use crate::store::LocalStore;
use crate::{convert, platform, records};

/// Version of the linking protocol: 2 added the receipt.
const LINK_PROTOCOL_VERSION: u32 = 2;
const OFFER_PREFIX: &str = "tclink";
/// How long the existing device waits for the receipt after sending the keys. The new device
/// answers as soon as it opened the key bag, which takes well under a second.
const RECEIPT_TIMEOUT: Duration = Duration::from_secs(60);

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
  /// The account being linked, to check that the key bag opens before confirming it.
  user: UserId,
}

/// The existing device's side after the handshake, waiting for the user to confirm the code.
pub struct LinkApproval<R> {
  connection: R,
  channel: RendezvousChannel,
}

#[derive(Serialize, Deserialize)]
struct TransferPayload {
  /// Missing in version 1.
  #[serde(default = "first_version")]
  version: u32,
  backup_key: Base64Bytes,
  encrypted_key_bag: Base64Bytes,
  key_bag_version: u64,
  wrapped_backup_keys: Base64Bytes,
}

/// The new device's answer to the transfer.
#[derive(Serialize, Deserialize)]
struct Receipt {
  version: u32,
  outcome: ReceiptOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReceiptOutcome {
  /// The key bag opened with the transferred backup key.
  Received,
  /// The transfer arrived but didn't open.
  Rejected,
  #[serde(other)]
  Unknown,
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
      "{OFFER_PREFIX}{LINK_PROTOCOL_VERSION}:{}:{}",
      self.rendezvous_id,
      hex(self.key.expose_secret())
    )
  }

  pub fn parse(code: &str) -> Result<Self, CoreError> {
    let invalid = || CoreError::Corrupt("not a device link code".into());
    let mut parts = code.trim().split(':');
    let version = parts
      .next()
      .and_then(|prefix| prefix.strip_prefix(OFFER_PREFIX))
      .and_then(|version| version.parse::<u32>().ok())
      .ok_or_else(invalid)?;
    check_version(version)?;
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
    let user = api.me().await?.id;
    let mut connection = join(api, offer).await?;
    let initiator = Initiator::new(&offer.key, &mut OsRng);
    let hello = next_binary(&mut connection).await?;
    let (awaiting, auth_hello) = initiator.receive_hello(&hello, &mut OsRng).map_err(link_error)?;
    connection.send_binary(auth_hello).await?;
    let auth = next_binary(&mut connection).await?;
    let channel = awaiting.receive_auth(&auth).map_err(link_error)?;
    Ok(Self {
      connection,
      channel,
      user,
    })
  }

  /// Show this; the user confirms it matches the code on the other device.
  pub fn short_code(&self) -> String {
    self.channel.short_code()
  }

  /// Waits for the transfer the existing device sends after confirmation, checks that the key bag
  /// in it opens, keeps it in `store` for an enrollment as `device_name` and confirms the receipt
  /// to the existing device. Enroll with the secrets next; [`records::delete_pending_link`] drops
  /// them if the user gives up instead.
  pub async fn receive_secrets(
    mut self,
    store: &impl LocalStore,
    device_name: &str,
  ) -> Result<LinkedSecrets, CoreError> {
    let frame = next_binary(&mut self.connection).await?;
    let plaintext = self.channel.open(&frame).map_err(link_error)?;
    let payload: TransferPayload = serde_json::from_slice(plaintext.expose_secret())
      .map_err(|error| CoreError::Corrupt(error.to_string()))?;
    check_version(payload.version)?;
    let kept = self.open_transfer(payload).and_then(|secrets| {
      // Stored before the receipt: once the existing device reports success, a restart here
      // doesn't lose the keys (issue #168).
      let pending = records::PendingLink {
        user_id: self.user,
        device_name: device_name.to_string(),
        secrets,
      };
      records::save_pending_link(store, &pending)?;
      store.write_soon();
      Ok(pending.secrets)
    });
    let secrets = match kept {
      Ok(secrets) => secrets,
      Err(error) => {
        self.send_receipt(ReceiptOutcome::Rejected).await;
        return Err(error);
      },
    };
    self.send_receipt(ReceiptOutcome::Received).await;
    Ok(secrets)
  }

  /// The transferred secrets, if the key bag opens with them.
  fn open_transfer(&self, payload: TransferPayload) -> Result<LinkedSecrets, CoreError> {
    let backup_secret = SymmetricKey::from_slice(payload.backup_key.as_slice())
      .ok_or_else(|| CoreError::Corrupt("bad backup key".into()))?;
    EncryptedKeyBag::from_bytes(payload.encrypted_key_bag.as_slice())?.decrypt(
      convert::user(self.user),
      &BackupKey::from_key(backup_secret.duplicate()),
      payload.wrapped_backup_keys.as_slice(),
    )?;
    Ok(LinkedSecrets {
      backup_secret,
      encrypted_key_bag: payload.encrypted_key_bag.into_vec(),
      key_bag_version: payload.key_bag_version,
      wrapped_keys: payload.wrapped_backup_keys.into_vec(),
    })
  }

  /// Tells the existing device how the transfer went. A receipt that gets lost makes the existing
  /// device report a failure although this one has the keys: the user links again, which is safe.
  async fn send_receipt(&mut self, outcome: ReceiptOutcome) {
    let receipt = Receipt {
      version: LINK_PROTOCOL_VERSION,
      outcome,
    };
    let sent = match serde_json::to_vec(&receipt) {
      Ok(plaintext) => match self.channel.seal(&plaintext) {
        Ok(frame) => self.connection.send_binary(frame).await.map_err(CoreError::from),
        Err(error) => Err(link_error(error)),
      },
      Err(error) => Err(CoreError::Corrupt(error.to_string())),
    };
    if let Err(error) = sent {
      tracing::warn!(%error, "could not confirm the device link transfer");
    }
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
  /// bag to the new device and waits for its receipt. Succeeds only once the new device confirmed
  /// that the key bag opened.
  pub async fn complete_link_approval(
    &mut self,
    mut approval: LinkApproval<A::Rendezvous>,
  ) -> Result<(), CoreError> {
    // Hand over the newest content keys and removal records, and the version the server has, so
    // the new device's next upload doesn't conflict.
    self.share_removed_senders()?;
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
      version: LINK_PROTOCOL_VERSION,
      backup_key: Base64Bytes::new(self.account.backup_secret.expose_secret().to_vec()),
      encrypted_key_bag: Base64Bytes::new(encrypted.to_bytes()?),
      key_bag_version: self.sync.key_bag_version,
      wrapped_backup_keys: Base64Bytes::new(wrapped_keys),
    };
    let plaintext =
      Zeroizing::new(serde_json::to_vec(&payload).map_err(|error| CoreError::Corrupt(error.to_string()))?);
    let frame = approval.channel.seal(&plaintext).map_err(link_error)?;
    approval.connection.send_binary(frame).await?;
    records::save_key_bag(&self.store, &self.account)?;
    approval.wait_for_receipt().await
  }
}

impl<R: RendezvousConnection> LinkApproval<R> {
  /// The new device's receipt; a closed relay, a garbled frame or silence count as failure.
  async fn wait_for_receipt(&mut self) -> Result<(), CoreError> {
    let frame = {
      let receive = Box::pin(next_binary(&mut self.connection));
      let timeout = Box::pin(platform::sleep(RECEIPT_TIMEOUT));
      match select(receive, timeout).await {
        Either::Left((Ok(frame), _)) => frame,
        Either::Left((Err(error), _)) => {
          tracing::warn!(%error, "the device link ended without a receipt");
          return Err(CoreError::LinkNotConfirmed);
        },
        Either::Right(((), _)) => return Err(CoreError::LinkNotConfirmed),
      }
    };
    let plaintext = self
      .channel
      .open(&frame)
      .map_err(|_| CoreError::LinkNotConfirmed)?;
    let receipt: Receipt =
      serde_json::from_slice(plaintext.expose_secret()).map_err(|_| CoreError::LinkNotConfirmed)?;
    check_version(receipt.version)?;
    match receipt.outcome {
      ReceiptOutcome::Received => Ok(()),
      ReceiptOutcome::Rejected => Err(CoreError::LinkRejected),
      ReceiptOutcome::Unknown => Err(CoreError::LinkNotConfirmed),
    }
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

/// Both devices must speak the same linking version; anything else fails before keys are sent.
fn check_version(theirs: u32) -> Result<(), CoreError> {
  if theirs == LINK_PROTOCOL_VERSION {
    return Ok(());
  }
  Err(CoreError::LinkVersionMismatch {
    ours: LINK_PROTOCOL_VERSION,
    theirs,
  })
}

fn first_version() -> u32 {
  1
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
    assert!(offer.to_code().starts_with("tclink2:"));
    for garbage in [
      "",
      "tclink2",
      "tclinkx:00:00",
      "tclink2:zz:00",
      &format!("{}:extra", offer.to_code()),
    ] {
      assert!(
        matches!(LinkOffer::parse(garbage), Err(CoreError::Corrupt(_))),
        "{garbage}"
      );
    }
  }

  #[test]
  fn a_code_of_another_protocol_version_is_refused_clearly() {
    let current = LinkOffer::generate().to_code();
    for (prefix, theirs) in [("tclink1", 1), ("tclink3", 3)] {
      let code = current.replacen("tclink2", prefix, 1);
      assert!(matches!(
        LinkOffer::parse(&code),
        Err(CoreError::LinkVersionMismatch { ours: 2, theirs: found }) if found == theirs
      ));
    }
  }

  #[test]
  fn a_version_one_transfer_has_no_version_field() {
    let payload: TransferPayload = serde_json::from_str(
      r#"{"backup_key":"","encrypted_key_bag":"","key_bag_version":1,"wrapped_backup_keys":""}"#,
    )
    .unwrap();
    assert_eq!(payload.version, 1);
    assert!(matches!(
      check_version(payload.version),
      Err(CoreError::LinkVersionMismatch { .. })
    ));
  }

  #[test]
  fn receipts_keep_their_wire_format() {
    let receipt = Receipt {
      version: 2,
      outcome: ReceiptOutcome::Received,
    };
    assert_eq!(
      serde_json::to_string(&receipt).unwrap(),
      r#"{"version":2,"outcome":"received"}"#
    );
    let unknown: Receipt = serde_json::from_str(r#"{"version":2,"outcome":"later"}"#).unwrap();
    assert_eq!(unknown.outcome, ReceiptOutcome::Unknown);
  }
}
