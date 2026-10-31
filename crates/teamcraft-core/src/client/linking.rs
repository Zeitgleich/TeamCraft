//! Adding a device by linking it to an existing one (e2ee.md §5).
//!
//! The new device creates a [`LinkOffer`] and shows it as a QR code (or the approval prompt carries
//! it). It joins the server's relay with [`NewDeviceLink::connect`]; the existing device scans the
//! code and joins with [`Client::start_link_approval`]. After the handshake both screens show the
//! same short code; once the user confirmed it on the existing device,
//! [`Client::complete_link_approval`] transfers the backup key and the key bag. The new device
//! checks that the key bag opens and answers with a receipt; only that receipt makes the existing
//! device report success, so a lost frame fails on both sides. The wait for it needs no client
//! ([`LinkReceipt`]): apps run it on a task of their own, so their loop goes on meanwhile (#415).
//! Before it answers, the new device stores the secrets (encrypted like the account, issue #168);
//! it then enrolls with them ([`crate::EnrollmentSecret::Linked`]). If it stops before the account
//! is stored, its next start finishes enrolling with the stored secrets
//! ([`crate::PendingEnrollment::resume_link`]) instead of asking for another link the existing
//! device already reported as done.
//!
//! The protocol version is in the code itself (`tclink2`): a device that speaks another version
//! refuses the code when it is scanned, before any key leaves the existing device.
//!
//! The code belongs on the screen that can't scan (issue #998). A new computer shows it and a phone
//! scans it, as above. A new phone has no one to show it to: it asks the other devices instead
//! ([`LinkOffer::announce`]), or it scans the code a signed-in computer shows for it (*Link a
//! phone*, `tcjoin2`). Then the roles in the relay swap: the signed-in device waits there
//! ([`LinkWait::approval`]) and the new phone joins ([`NewDeviceLink::join_scanned`]). The keys
//! still go from the signed-in device to the new one, and the user still types the code the new
//! device shows into the signed-in one. Each kind of code is refused by the side that must not
//! scan it, so neither device waits for the wrong partner.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures_util::FutureExt;
use futures_util::future::{AbortHandle, Abortable, Either, select};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use teamcraft_crypto::backup::BackupKey;
use teamcraft_crypto::key_bag::{EncryptedKeyBag, KeyBagBinding};
use teamcraft_crypto::rendezvous::{Initiator, RendezvousChannel, RendezvousKey, Responder};
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{
  Base64Bytes, CreateDeviceLinkRequest, DeviceLinkPlatform, DeviceLinkRequest, RendezvousClientMessage,
  RendezvousServerMessage, UserId,
};
use zeroize::Zeroizing;

use super::Client;
use crate::api::{ApiError, RendezvousConnection, RendezvousFrame, ServerApi};
use crate::error::CoreError;
use crate::store::LocalStore;
use crate::{convert, platform, records};

/// Version of the linking protocol: 2 added the receipt.
const LINK_PROTOCOL_VERSION: u32 = 2;
/// Prefix of a code a new device shows; a signed-in device scans it.
const OFFER_PREFIX: &str = "tclink";
/// Prefix of a code a signed-in device shows for a new phone, which scans it (issue #998).
const JOIN_PREFIX: &str = "tcjoin";
/// How long the existing device waits for the receipt after sending the keys. The new device
/// answers as soon as it opened the key bag, which takes well under a second.
const RECEIPT_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the approving device waits for the new device's part of the handshake once both are
/// in the relay. The new device answers at once; anything slower is a device that went away.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// What the QR code carries: where to meet (relay ID) and the handshake's authentication key.
#[derive(Debug)]
pub struct LinkOffer {
  kind: LinkCodeKind,
  rendezvous_id: String,
  key: RendezvousKey,
}

/// Which device shows a link code, and so which one may scan it (issue #998).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkCodeKind {
  /// A new device shows it (a computer); a signed-in device scans it and sends the keys.
  ShownByNewDevice,
  /// A signed-in device shows it for a new phone (*Link a phone*); the phone scans it and
  /// receives the keys.
  ShownBySignedInDevice,
}

/// The device that showed a code, waiting in the relay for the one that scans it. It knows how long
/// the relay stays open, so the screen can show when the code runs out.
pub struct LinkWait<R> {
  kind: LinkCodeKind,
  connection: R,
  initiator: Initiator,
  maybe_expires_in: Option<Duration>,
}

/// What the new device tells the user's other devices about itself, so they can ask to approve it
/// ([`LinkOffer::announce`]). The server and the other devices only have the new device's word for
/// both; they are shown as unverified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkAnnouncement {
  pub device_name: String,
  pub platform: DeviceLinkPlatform,
}

/// The open link requests, shared so an app can read them while the sync loop owns the client.
/// Times are on the clock the caller passes in.
#[derive(Clone, Default)]
pub struct LinkRequestList(Arc<Mutex<Vec<PendingLinkRequest>>>);

impl LinkRequestList {
  /// The requests still open at `now_ms`, oldest first; expired ones are dropped.
  pub fn open(&self, now_ms: i64) -> Vec<LinkRequestInfo> {
    let mut requests = self.lock();
    requests.retain(|entry| entry.expires_at_ms > now_ms);
    requests
      .iter()
      .map(|entry| LinkRequestInfo {
        id: entry.request.id.clone(),
        device_name: entry.request.device_name.clone(),
        platform: entry.request.platform.clone(),
        seconds_left: u64::try_from((entry.expires_at_ms - now_ms) / 1000).unwrap_or(0),
      })
      .collect()
  }

  fn lock(&self) -> MutexGuard<'_, Vec<PendingLinkRequest>> {
    // A poisoned lock only means another thread panicked; the list itself stays consistent.
    self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }
}

/// A request of a new device to be linked, as the user's other devices hold it. The key stays in
/// the core: the app shows [`LinkRequestInfo`] and asks to review by ID.
pub(crate) struct PendingLinkRequest {
  request: DeviceLinkRequest,
  /// On this device's clock; the server counted the time left, not a date, so a wrong clock only
  /// moves the prompt's end a little. The server and the relay enforce the real expiry.
  expires_at_ms: i64,
}

/// What the app shows of a pending request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRequestInfo {
  pub id: String,
  pub device_name: String,
  pub platform: DeviceLinkPlatform,
  /// Whole seconds left when [`Client::link_requests`] was called.
  pub seconds_left: u64,
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

/// The existing device's side once the keys went out ([`Client::send_link_transfer`]): the new
/// device's receipt is still to come. It holds the relay, not the client, so an app waits for it on
/// a task of its own while its loop goes on (issue #415).
pub struct LinkReceipt<R> {
  approval: LinkApproval<R>,
}

/// Stops the wait of [`LinkReceipt::detach`] when cancelled or dropped.
pub struct ReceiptCancel(AbortHandle);

/// Stops the wait of [`LinkWait::detach_approval`] when dropped: the relay closes, the code is dead.
pub struct LinkWaitCancel(AbortHandle);

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
  /// A code for a new device to show.
  pub fn generate() -> Self {
    Self::generate_shown_by(LinkCodeKind::ShownByNewDevice)
  }

  /// A code for a signed-in device to show, which a new phone scans (*Link a phone*).
  pub fn generate_for_new_phone() -> Self {
    Self::generate_shown_by(LinkCodeKind::ShownBySignedInDevice)
  }

  fn generate_shown_by(kind: LinkCodeKind) -> Self {
    let mut id = [0u8; 16];
    OsRng.fill_bytes(&mut id);
    Self {
      kind,
      rendezvous_id: hex(&id),
      key: RendezvousKey::generate(&mut OsRng),
    }
  }

  pub fn kind(&self) -> LinkCodeKind {
    self.kind
  }

  /// The QR code text. Contains the authentication key: show it, never send it anywhere else.
  pub fn to_code(&self) -> String {
    let prefix = match self.kind {
      LinkCodeKind::ShownByNewDevice => OFFER_PREFIX,
      LinkCodeKind::ShownBySignedInDevice => JOIN_PREFIX,
    };
    format!(
      "{prefix}{LINK_PROTOCOL_VERSION}:{}:{}",
      self.rendezvous_id,
      hex(self.key.expose_secret())
    )
  }

  /// The offer a link request carries (the key came through the server; the handshake and the code
  /// the user types catch a server that tampered with it, e2ee.md §5).
  pub(crate) fn from_request(request: &DeviceLinkRequest) -> Result<Self, CoreError> {
    let invalid = || CoreError::Corrupt("not a device link request".into());
    if request.id.len() != 32 || !request.id.bytes().all(|b| b.is_ascii_hexdigit()) {
      return Err(invalid());
    }
    let key = Zeroizing::new(<[u8; 32]>::try_from(request.key.as_slice()).map_err(|_| invalid())?);
    Ok(Self {
      kind: LinkCodeKind::ShownByNewDevice,
      rendezvous_id: request.id.clone(),
      key: RendezvousKey::from_bytes(*key),
    })
  }

  /// Asks the user's other devices to approve this one (a prompt on each, e2ee.md §5), while
  /// [`NewDeviceLink::connect`] waits in the relay. **This hands the authentication key to the
  /// server**, which a scanned code does not: call it only when the user chose the prompt. The
  /// typed code still decides, so a server that injects itself is detected.
  pub async fn announce<A: ServerApi>(
    &self,
    api: &A,
    announcement: &LinkAnnouncement,
  ) -> Result<(), CoreError> {
    let request = CreateDeviceLinkRequest {
      rendezvous_id: self.rendezvous_id.clone(),
      key: Base64Bytes::new(self.key.expose_secret().to_vec()),
      device_name: announcement.device_name.clone(),
      platform: announcement.platform.clone(),
    };
    api.announce_device_link(&request).await?;
    Ok(())
  }

  /// Reads a scanned or typed code. Text of another app is [`CoreError::LinkCodeInvalid`]; a code
  /// of another linking version is [`CoreError::LinkVersionMismatch`], so the app can say which
  /// side to update instead of doing nothing (issue #645).
  pub fn parse(code: &str) -> Result<Self, CoreError> {
    let invalid = || CoreError::LinkCodeInvalid;
    let mut parts = code.trim().split(':');
    let head = parts.next().ok_or_else(invalid)?;
    let (kind, version) = [
      (OFFER_PREFIX, LinkCodeKind::ShownByNewDevice),
      (JOIN_PREFIX, LinkCodeKind::ShownBySignedInDevice),
    ]
    .into_iter()
    .find_map(|(prefix, kind)| Some((kind, head.strip_prefix(prefix)?)))
    .ok_or_else(invalid)?;
    let version = version.parse::<u32>().map_err(|_| invalid())?;
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
      kind,
      rendezvous_id: rendezvous_id.to_string(),
      key: RendezvousKey::from_bytes(*key),
    })
  }

  /// The offer, if a device on `side` may scan it; the other kind says which device to use.
  fn scanned_by(self, side: LinkCodeKind) -> Result<Self, CoreError> {
    match (self.kind, side) {
      (shown, scanning) if shown == scanning => Ok(self),
      (LinkCodeKind::ShownBySignedInDevice, _) => Err(CoreError::LinkCodeForNewDevice),
      (LinkCodeKind::ShownByNewDevice, _) => Err(CoreError::LinkCodeForSignedInDevice),
    }
  }
}

impl<R: RendezvousConnection> LinkWait<R> {
  /// Joins the relay of a code this device shows. Returns once the server took the join, before
  /// anyone scanned the code.
  pub async fn join<A: ServerApi<Rendezvous = R>>(api: &A, offer: &LinkOffer) -> Result<Self, CoreError> {
    let mut connection = join(api, offer).await?;
    let initiator = Initiator::new(&offer.key, &mut OsRng);
    let maybe_expires_in = match connection.receive().await {
      Some(Ok(RendezvousFrame::Control(RendezvousServerMessage::Joined {
        maybe_expires_in_secs,
        ..
      }))) => maybe_expires_in_secs.map(Duration::from_secs),
      other => return Err(join_failure(other)),
    };
    Ok(Self {
      kind: offer.kind,
      connection,
      initiator,
      maybe_expires_in,
    })
  }

  /// How long the relay stays open from the join; `None` from a server that doesn't say.
  pub fn maybe_expires_in(&self) -> Option<Duration> {
    self.maybe_expires_in
  }

  /// New device: waits until a signed-in device scanned the code and ran the handshake.
  pub async fn new_device<A: ServerApi<Rendezvous = R>>(
    self,
    api: &A,
  ) -> Result<NewDeviceLink<R>, CoreError> {
    if self.kind != LinkCodeKind::ShownByNewDevice {
      return Err(CoreError::LinkCodeForNewDevice);
    }
    let user = api.me().await?.id;
    let (connection, channel) = self.handshake().await?;
    Ok(NewDeviceLink {
      connection,
      channel,
      user,
    })
  }

  /// Signed-in device (*Link a phone*): waits until the new phone scanned the code and ran the
  /// handshake. The user then types the code the phone shows, as for a code this device scanned.
  pub async fn approval(self) -> Result<LinkApproval<R>, CoreError> {
    if self.kind != LinkCodeKind::ShownBySignedInDevice {
      return Err(CoreError::LinkCodeForSignedInDevice);
    }
    let (connection, channel) = self.handshake().await?;
    Ok(LinkApproval { connection, channel })
  }

  /// [`Self::approval`] as a future that borrows nothing, to run on a task of its own beside the
  /// app's loop while the code is on screen, and the handle that stops it. A stopped (or dropped)
  /// handle ends the wait at once with [`CoreError::LinkCancelled`] and closes the relay.
  pub fn detach_approval(
    self,
  ) -> (
    impl Future<Output = Result<LinkApproval<R>, CoreError>> + use<R>,
    LinkWaitCancel,
  ) {
    let (handle, registration) = AbortHandle::new_pair();
    let waiting = Abortable::new(self.approval(), registration)
      .map(|outcome| outcome.unwrap_or(Err(CoreError::LinkCancelled)));
    (waiting, LinkWaitCancel(handle))
  }

  /// The handshake as initiator, once the scanning device answered. Waits as long as the relay
  /// lives; when it ends unused the code expired and the app offers a new one.
  async fn handshake(mut self) -> Result<(R, RendezvousChannel), CoreError> {
    let hello = next_binary(&mut self.connection)
      .await
      .map_err(|error| match error {
        CoreError::Gateway(_) => CoreError::LinkCodeExpired,
        other => other,
      })?;
    let (awaiting, auth_hello) = self
      .initiator
      .receive_hello(&hello, &mut OsRng)
      .map_err(link_error)?;
    self.connection.send_binary(auth_hello).await?;
    let auth = next_binary(&mut self.connection).await?;
    let channel = awaiting.receive_auth(&auth).map_err(link_error)?;
    Ok((self.connection, channel))
  }
}

impl<R: RendezvousConnection> NewDeviceLink<R> {
  /// Joins the relay and runs the handshake as initiator; returns once the existing device answered.
  /// Nothing about the code leaves the device except to the relay: the key reaches the server only
  /// when the user asks for the prompt on the other devices ([`LinkOffer::announce`]).
  pub async fn connect<A: ServerApi<Rendezvous = R>>(api: &A, offer: &LinkOffer) -> Result<Self, CoreError> {
    LinkWait::join(api, offer).await?.new_device(api).await
  }

  /// New phone: joins the relay of the code a signed-in device shows for it (*Link a phone*,
  /// issue #998) and runs the handshake. The signed-in device waits there; without it the code
  /// expired or was closed ([`CoreError::LinkPeerGone`]). A code another new device shows is
  /// refused ([`CoreError::LinkCodeForSignedInDevice`]) before anything joins.
  pub async fn join_scanned<A: ServerApi<Rendezvous = R>>(api: &A, code: &str) -> Result<Self, CoreError> {
    let offer = LinkOffer::parse(code)?.scanned_by(LinkCodeKind::ShownBySignedInDevice)?;
    let user = api.me().await?.id;
    let (connection, channel) = respond(api, &offer).await?;
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
  /// The new devices of this account that ask to be linked and wait for a decision; the expired
  /// ones are dropped. Order: oldest first.
  pub fn link_requests(&self) -> Vec<LinkRequestInfo> {
    self.link_requests.open(self.now_ms())
  }

  /// A handle on the same list for an app whose sync loop owns the client; read it with a clock.
  pub fn link_request_list(&self) -> LinkRequestList {
    self.link_requests.clone()
  }

  /// Takes up a link request: joins its relay as the approving device and runs the handshake. The
  /// other devices drop their prompts. The user then types the code the new device shows
  /// ([`Self::complete_link_approval`]); nothing is sent to the new device before that.
  pub async fn review_link_request(&mut self, id: &str) -> Result<LinkApproval<A::Rendezvous>, CoreError> {
    let request = self
      .link_requests
      .lock()
      .iter()
      .find(|entry| entry.request.id == id)
      .map(|entry| entry.request.clone())
      .ok_or(CoreError::LinkRequestGone)?;
    let offer = LinkOffer::from_request(&request)?;
    // The request stays listed until the join worked: a failed try (network) can be repeated.
    let approval = self.approve_offer(&offer).await?;
    self.forget_link_request(id);
    Ok(approval)
  }

  /// Denies a link request: it is closed at the server and the new device is told.
  pub async fn reject_link_request(&mut self, id: &str) -> Result<(), CoreError> {
    match self.api.reject_device_link(id).await {
      Ok(()) => {
        self.forget_link_request(id);
        Ok(())
      },
      // Taken up elsewhere or expired: no longer listed here either, and the user is told.
      Err(error) if error.is_not_found() => {
        self.forget_link_request(id);
        Err(CoreError::LinkRequestGone)
      },
      Err(error) => Err(error.into()),
    }
  }

  /// A request arrived over the gateway (or the list read at connect).
  pub(super) fn note_link_request(&mut self, request: DeviceLinkRequest) {
    let expires_at_ms =
      self.now_ms() + i64::try_from(request.expires_in_secs.saturating_mul(1000)).unwrap_or(0);
    // A repeated announcement replaces the earlier one of the same ID.
    {
      let mut requests = self.link_requests.lock();
      requests.retain(|entry| entry.request.id != request.id);
      requests.push(PendingLinkRequest {
        request,
        expires_at_ms,
      });
    }
    self.note_link_requests_changed();
  }

  pub(super) fn forget_link_request(&mut self, id: &str) {
    let was_removed = {
      let mut requests = self.link_requests.lock();
      let before = requests.len();
      requests.retain(|entry| entry.request.id != id);
      requests.len() != before
    };
    if was_removed {
      self.note_link_requests_changed();
    }
  }

  /// Replaces the list with what the server holds (after a (re)connect: events in between were
  /// lost). Failures are only logged; the next connect tries again.
  pub(super) async fn refresh_link_requests(&mut self) {
    let read = match self.read_ahead.link_requests.take() {
      Some(read) => read,
      None => self.api.device_links().await,
    };
    let requests = match read {
      Ok(requests) => requests,
      Err(error) => {
        tracing::debug!(%error, "could not read the open device link requests");
        return;
      },
    };
    let now = self.now_ms();
    *self.link_requests.lock() = requests
      .into_iter()
      .map(|request| PendingLinkRequest {
        expires_at_ms: now + i64::try_from(request.expires_in_secs.saturating_mul(1000)).unwrap_or(0),
        request,
      })
      .collect();
    self.note_link_requests_changed();
  }

  /// Tells the UI, which reads [`Self::link_requests`] when the mark moves.
  fn note_link_requests_changed(&self) {
    if let Err(error) = crate::read_state::update_marks(&self.store, |marks| marks.link_requests += 1) {
      tracing::warn!(%error, "could not note the change of the device link requests");
    }
  }

  /// Scans a new device's code, joins the relay and runs the handshake as responder. A code a
  /// signed-in device shows for a new phone is refused ([`CoreError::LinkCodeForNewDevice`]).
  pub async fn start_link_approval(&self, code: &str) -> Result<LinkApproval<A::Rendezvous>, CoreError> {
    let offer = LinkOffer::parse(code)?.scanned_by(LinkCodeKind::ShownByNewDevice)?;
    self.approve_offer(&offer).await
  }

  /// Joins the new device's relay as the approving device.
  async fn approve_offer(&self, offer: &LinkOffer) -> Result<LinkApproval<A::Rendezvous>, CoreError> {
    let (connection, channel) = respond(&self.api, offer).await?;
    Ok(LinkApproval { connection, channel })
  }

  /// After the user confirmed that both short codes match: sends the backup key and the current key
  /// bag to the new device and waits for its receipt. Succeeds only once the new device confirmed
  /// that the key bag opened. An app with a loop sends with [`Self::send_link_transfer`] and waits
  /// beside it instead ([`LinkReceipt::detach`]).
  pub async fn complete_link_approval(
    &mut self,
    approval: LinkApproval<A::Rendezvous>,
  ) -> Result<(), CoreError> {
    self.send_link_transfer(approval).await?.wait().await
  }

  /// After the user confirmed that both short codes match: sends the backup key and the current key
  /// bag to the new device. The link succeeded only once its receipt is in ([`LinkReceipt`]), which
  /// needs no client: the app's loop goes on meanwhile (issue #415).
  pub async fn send_link_transfer(
    &mut self,
    mut approval: LinkApproval<A::Rendezvous>,
  ) -> Result<LinkReceipt<A::Rendezvous>, CoreError> {
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
    Ok(LinkReceipt { approval })
  }
}

impl<R: RendezvousConnection> LinkReceipt<R> {
  /// The new device's receipt: `Ok` once it confirmed that the key bag opened. A closed relay, a
  /// garbled frame or a minute of silence fail with [`CoreError::LinkNotConfirmed`].
  pub async fn wait(mut self) -> Result<(), CoreError> {
    self.approval.wait_for_receipt().await
  }

  /// [`Self::wait`] as a future that borrows nothing, to run on a task of its own beside the app's
  /// loop (issue #415), and the handle that stops it. A stopped (or dropped) handle ends the wait at
  /// once with [`CoreError::LinkCancelled`] and closes the relay.
  pub fn detach(
    self,
  ) -> (
    impl Future<Output = Result<(), CoreError>> + use<R>,
    ReceiptCancel,
  ) {
    let (handle, registration) = AbortHandle::new_pair();
    let waiting = Abortable::new(self.wait(), registration)
      .map(|outcome| outcome.unwrap_or(Err(CoreError::LinkCancelled)));
    (waiting, ReceiptCancel(handle))
  }
}

impl ReceiptCancel {
  /// Ends the wait (the user closed the dialog, the app stops).
  pub fn cancel(self) {
    drop(self);
  }
}

impl Drop for ReceiptCancel {
  fn drop(&mut self) {
    self.0.abort();
  }
}

impl Drop for LinkWaitCancel {
  fn drop(&mut self) {
    self.0.abort();
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

/// Joins the relay of a code this device scanned and runs the handshake as responder. The device
/// that shows the code joined first and waits there; a relay without it (expired, or that device
/// went away) fails at once with [`CoreError::LinkPeerGone`] instead of waiting for nobody (issue
/// #645).
async fn respond<A: ServerApi>(
  api: &A,
  offer: &LinkOffer,
) -> Result<(A::Rendezvous, RendezvousChannel), CoreError> {
  let mut connection = join(api, offer).await?;
  if !with_timeout(wait_joined(&mut connection)).await? {
    return Err(CoreError::LinkPeerGone);
  }
  let (responder, hello) = Responder::new(&offer.key, &mut OsRng).map_err(link_error)?;
  connection.send_binary(hello).await?;
  let auth_hello = with_timeout(next_binary(&mut connection)).await?;
  let (channel, auth) = responder
    .receive_auth_hello(&auth_hello, &mut OsRng)
    .map_err(link_error)?;
  connection.send_binary(auth).await?;
  Ok((connection, channel))
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

/// Whether the other device was in the relay when this one joined (the server's `joined` answer).
async fn wait_joined<R: RendezvousConnection>(connection: &mut R) -> Result<bool, CoreError> {
  match connection.receive().await {
    Some(Ok(RendezvousFrame::Control(RendezvousServerMessage::Joined { peer_present, .. }))) => {
      Ok(peer_present)
    },
    other => Err(join_failure(other)),
  }
}

/// What a first frame other than `joined` means.
fn join_failure(frame: Option<Result<RendezvousFrame, ApiError>>) -> CoreError {
  match frame {
    Some(Ok(RendezvousFrame::Control(RendezvousServerMessage::Error { error }))) => {
      CoreError::Gateway(error.message)
    },
    Some(Ok(RendezvousFrame::Control(other))) => CoreError::Gateway(format!("relay ended: {other:?}")),
    Some(Ok(RendezvousFrame::Binary(_))) => CoreError::Gateway("relay sent data before the join".into()),
    Some(Err(error)) => error.into(),
    None => CoreError::Gateway("relay closed".into()),
  }
}

/// `step`, or [`CoreError::LinkTimedOut`] after [`HANDSHAKE_TIMEOUT`].
async fn with_timeout<T>(step: impl Future<Output = Result<T, CoreError>>) -> Result<T, CoreError> {
  let step = Box::pin(step);
  let timeout = Box::pin(platform::sleep(HANDSHAKE_TIMEOUT));
  match select(step, timeout).await {
    Either::Left((result, _)) => result,
    Either::Right(((), _)) => Err(CoreError::LinkTimedOut),
  }
}

/// The next binary frame, skipping control frames that only report progress.
async fn next_binary<R: RendezvousConnection>(connection: &mut R) -> Result<Vec<u8>, CoreError> {
  loop {
    match connection.receive().await {
      Some(Ok(RendezvousFrame::Binary(frame))) => return Ok(frame),
      Some(Ok(RendezvousFrame::Control(
        RendezvousServerMessage::Joined { .. } | RendezvousServerMessage::PeerJoined,
      ))) => {},
      Some(Ok(RendezvousFrame::Control(RendezvousServerMessage::Rejected))) => {
        return Err(CoreError::LinkDenied);
      },
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
        matches!(LinkOffer::parse(garbage), Err(CoreError::LinkCodeInvalid)),
        "{garbage}"
      );
    }
  }

  #[test]
  fn each_kind_of_code_keeps_its_kind() {
    for offer in [LinkOffer::generate(), LinkOffer::generate_for_new_phone()] {
      let parsed = LinkOffer::parse(&offer.to_code()).unwrap();
      assert_eq!(parsed.kind, offer.kind);
      assert_eq!(parsed.rendezvous_id, offer.rendezvous_id);
    }
    assert!(
      LinkOffer::generate_for_new_phone()
        .to_code()
        .starts_with("tcjoin2:")
    );
    let newer = LinkOffer::generate_for_new_phone()
      .to_code()
      .replacen("tcjoin2", "tcjoin3", 1);
    assert!(matches!(
      LinkOffer::parse(&newer),
      Err(CoreError::LinkVersionMismatch { theirs: 3, .. })
    ));
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
