//! Enrollments nobody finishes (issue #415).
//!
//! An enrollment stores its device key pair before it registers it (e2ee.md §3.2), so a retry picks
//! the registration up. A user who gives up instead (signs out, or never comes back) would leave the
//! key pair in the store for good, and the registration counting against the user's device limit.
//! So the key pair is dropped: on sign-out at once, otherwise once it is older than
//! [`ClientSettings::unfinished_enrollment_expiry`] and nothing can finish the enrollment any more.
//! Its public key stays ([`records::AbandonedDevices`]) until the server revoked the device it
//! registered; the next start, a sign-out or the upkeep of an enrolled client tries again until the
//! server answers.
//!
//! A registration is revoked only if no enrolled device can be using it: it was certified by an
//! identity that isn't the account's (enrollment reuses only registrations of its own identity), or
//! the device list the account's identity signed doesn't name it. The server's word alone never
//! decides: a copy of the store made while the setup was unfinished (a restored backup) must not
//! revoke the device the original finished enrolling with the same key pair.

use std::time::Duration;

use futures_util::future::{Either, select};
use teamcraft_crypto::device_list::{SignedDeviceList, VerifiedDeviceList};
use teamcraft_crypto::identity::IdentityPublicKey;
use teamcraft_proto::{Base64Bytes, DeviceId, DeviceResponse, UserId};

use super::enroll::revoke_orphan;
use super::{Client, ClientSettings, PendingEnrollment};
use crate::api::ServerApi;
use crate::clock::Clock;
use crate::error::CoreError;
use crate::records::{self, AbandonedDevices};
use crate::store::LocalStore;
use crate::{convert, platform};

/// How long signing out waits for the server to revoke the device an unfinished enrollment
/// registered. What is left by then is revoked when the account is opened next.
pub const SIGN_OUT_REVOKE_PATIENCE: Duration = Duration::from_secs(5);
/// After a refusal that won't pass by itself (not the network), the next try waits this long; the
/// wait doubles with each further refusal, up to [`RETRY_MAX`]. Each try costs a few requests.
const RETRY_FIRST: Duration = Duration::from_secs(60 * 60);
const RETRY_MAX: Duration = Duration::from_secs(24 * 60 * 60);

/// What this client can verify about the account's devices in use.
enum AccountDevices {
  /// The account has no identity key, so no device of it finished enrolling.
  NoIdentity,
  /// The account's identity key, and the device list it signed if one was found and verified.
  Identity {
    key: [u8; 32],
    maybe_list: Option<VerifiedDeviceList>,
  },
}

/// What becomes of one registration to revoke.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
  Revoke,
  /// An enrolled device uses it.
  InUse,
  /// No signed device list could tell: it is left alone.
  Unverifiable,
}

impl PendingEnrollment {
  /// Drops the device key pair of an enrollment the user gave up on and revokes the devices such
  /// key pairs registered (issue #415). The key pair stays while the enrollment can still finish:
  /// an account is stored, the keys of a link wait to be enrolled ([`Self::resume_link`]), or it is
  /// younger than `settings.unfinished_enrollment_expiry`. Run it only while no enrollment and no
  /// link is in progress on this device.
  ///
  /// Returns whether a key pair was dropped. An error (offline) concerns the revocation only: the
  /// key pair is gone, and the next call revokes its device.
  pub async fn drop_abandoned<A: ServerApi, S: LocalStore>(
    api: &A,
    store: &S,
    clock: &dyn Clock,
    settings: &ClientSettings,
  ) -> Result<bool, CoreError> {
    let now_ms = clock.now_ms();
    let is_dropped = drop_expired_device(store, now_ms, settings.unfinished_enrollment_expiry)?;
    revoke_before_enrollment(api, store, now_ms).await?;
    Ok(is_dropped)
  }

  /// The user signs out before this device is set up (issues #168, #415): the keys of a link and the
  /// device key pair go at once ([`records::forget_unfinished_enrollment`]); the device the key pair
  /// registered is revoked if the server answers within `patience`, otherwise when this account is
  /// opened next. Leaves an enrolled account alone.
  pub async fn forget_unfinished<A: ServerApi, S: LocalStore>(
    api: &A,
    store: &S,
    clock: &dyn Clock,
    patience: Duration,
  ) -> Result<(), CoreError> {
    records::forget_unfinished_enrollment(store)?;
    let revoking = Box::pin(revoke_before_enrollment(api, store, clock.now_ms()));
    match select(revoking, Box::pin(platform::sleep(patience))).await {
      Either::Left((revoked, _)) => revoked.map(drop),
      Either::Right(((), _)) => {
        tracing::info!("no answer from the server; the abandoned device registration is revoked later");
        Ok(())
      },
    }
  }
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Revokes the devices that setups given up before this one registered (issue #415), from the
  /// upkeep once the server can be reached; returns how many. Checked against the device list this
  /// account's identity signed (fetched, verified, never older than the stored one). Asks the
  /// server nothing while there is nothing to revoke.
  pub async fn revoke_abandoned_devices(&mut self) -> Result<usize, CoreError> {
    let now_ms = self.now_ms();
    let me = self.account.user_id;
    let Some(mut abandoned) = due_list(&self.store, now_ms)? else {
      return Ok(0);
    };
    if !abandoned.devices.iter().any(|entry| entry.user_id == me) {
      return Ok(0);
    }
    let identity = self.account.identity_public_key();
    let maybe_list = match self.fetch_device_list(me, &identity).await {
      Ok(maybe_list) => maybe_list,
      // A list that doesn't verify (or is older than one seen) can't vouch for anything.
      Err(CoreError::DeviceList(error)) => {
        tracing::warn!(%error, "refused the account's device list; no abandoned registration is revoked");
        None
      },
      Err(error) => return Err(back_off(&self.store, &mut abandoned, error, now_ms)),
    };
    let account = AccountDevices::Identity {
      key: *identity.as_bytes(),
      maybe_list,
    };
    let own = Some(self.account.device_id);
    revoke_listed(&self.api, &self.store, me, own, &account, abandoned, now_ms).await
  }
}

/// The revocations of a device that has no account yet: the account's identity key and device list
/// come from the server, and count only where they match the identity key this device certified a
/// registration with (it kept it locally).
async fn revoke_before_enrollment<A: ServerApi, S: LocalStore>(
  api: &A,
  store: &S,
  now_ms: i64,
) -> Result<usize, CoreError> {
  // Enrolled meanwhile: the client's upkeep revokes, against its own identity key.
  if records::is_account_stored(store)? {
    return Ok(0);
  }
  let Some(mut abandoned) = due_list(store, now_ms)? else {
    return Ok(0);
  };
  let me = match api.me().await {
    Ok(me) => me.id,
    Err(error) => return Err(back_off(store, &mut abandoned, error.into(), now_ms)),
  };
  // Another account's registrations stay for it.
  if !abandoned.devices.iter().any(|entry| entry.user_id == me) {
    return Ok(0);
  }
  let account = match account_devices_from_server(api, me, &abandoned).await {
    Ok(account) => account,
    Err(error) => return Err(back_off(store, &mut abandoned, error, now_ms)),
  };
  revoke_listed(api, store, me, None, &account, abandoned, now_ms).await
}

/// The account's identity key and device list as the server holds them. The list is fetched and
/// verified only against an identity key one of `abandoned`'s registrations was certified with.
async fn account_devices_from_server<A: ServerApi>(
  api: &A,
  user: UserId,
  abandoned: &AbandonedDevices,
) -> Result<AccountDevices, CoreError> {
  let Some(identity) = api.identity_key(user).await? else {
    return Ok(AccountDevices::NoIdentity);
  };
  let key = <[u8; 32]>::try_from(identity.public_key.as_slice())
    .map_err(|_| CoreError::Corrupt("the account's identity key has the wrong size".into()))?;
  let is_certifier = abandoned
    .devices
    .iter()
    .any(|entry| entry.user_id == user && entry.identity_public_key == key);
  if !is_certifier {
    return Ok(AccountDevices::Identity {
      key,
      maybe_list: None,
    });
  }
  let maybe_response = match api.device_list(user).await {
    Ok(maybe_response) => maybe_response,
    // A server without device lists: nothing can be verified.
    Err(error) if error.is_not_found() => None,
    Err(error) => return Err(error.into()),
  };
  let maybe_list = maybe_response.and_then(|response| {
    let verified = SignedDeviceList::from_bytes(response.signed_list.as_slice())
      .and_then(|signed| signed.verify(convert::user(user), &IdentityPublicKey::from_bytes(key)));
    match verified {
      Ok(list) => Some(list),
      Err(error) => {
        tracing::warn!(%error, "refused the account's device list; no abandoned registration is revoked");
        None
      },
    }
  });
  Ok(AccountDevices::Identity { key, maybe_list })
}

/// Revokes `user`'s registrations in `abandoned` that the server still lists and no enrolled device
/// can be using, and returns how many it revoked. Each entry is done (revoked, in use, or given up)
/// before the next, and the list is saved after each. A failure that may pass (offline, sign-in)
/// stops it and keeps the rest; one the server refuses for good is given up (the user can still
/// remove the device from the list). Never revokes `maybe_own`.
async fn revoke_listed<A: ServerApi, S: LocalStore>(
  api: &A,
  store: &S,
  user: UserId,
  maybe_own: Option<DeviceId>,
  account: &AccountDevices,
  mut abandoned: AbandonedDevices,
  now_ms: i64,
) -> Result<usize, CoreError> {
  let registered = match api.list_user_devices(user).await {
    Ok(response) => response.devices,
    Err(error) => return Err(back_off(store, &mut abandoned, error.into(), now_ms)),
  };
  abandoned.failures = 0;
  abandoned.maybe_retry_at_ms = None;
  let mut revoked = 0;
  let mut index = 0;
  while let Some(entry) = abandoned.devices.get(index).cloned() {
    if entry.user_id != user {
      index += 1;
      continue;
    }
    if let Some(device) = registered_id(&registered, &entry.public_key) {
      match verdict(account, maybe_own, &entry.identity_public_key, device) {
        Verdict::Revoke => match revoke_orphan(api, device).await {
          Ok(()) => revoked += 1,
          Err(error) if is_retryable(&error) => {
            records::save_abandoned_devices(store, &abandoned)?;
            return Err(error);
          },
          Err(error) => {
            tracing::warn!(%error, device_id = %device, "giving up revoking an abandoned device registration");
          },
        },
        Verdict::InUse => {
          tracing::warn!(device_id = %device, "an abandoned registration is in use by an enrolled device; kept");
        },
        Verdict::Unverifiable => {
          tracing::warn!(device_id = %device, "no signed device list tells whether an abandoned registration is in use; kept");
        },
      }
    }
    abandoned.devices.remove(index);
    records::save_abandoned_devices(store, &abandoned)?;
  }
  records::save_abandoned_devices(store, &abandoned)?;
  Ok(revoked)
}

/// Whether the registration `device`, certified by `certified_by`, may be revoked.
fn verdict(
  account: &AccountDevices,
  maybe_own: Option<DeviceId>,
  certified_by: &[u8; 32],
  device: DeviceId,
) -> Verdict {
  if maybe_own == Some(device) {
    return Verdict::InUse;
  }
  match account {
    AccountDevices::NoIdentity => Verdict::Revoke,
    // Enrollment reuses only a registration its own identity certified (e2ee.md §3.2).
    AccountDevices::Identity { key, .. } if key != certified_by => Verdict::Revoke,
    AccountDevices::Identity {
      maybe_list: Some(list),
      ..
    } => {
      let device = convert::device(device);
      if list.devices().iter().any(|entry| entry.device_id == device) {
        Verdict::InUse
      } else {
        Verdict::Revoke
      }
    },
    AccountDevices::Identity { maybe_list: None, .. } => Verdict::Unverifiable,
  }
}

/// The server's ID of the registration with `public_key`, if it is still listed.
fn registered_id(registered: &[DeviceResponse], public_key: &Base64Bytes) -> Option<DeviceId> {
  registered
    .iter()
    .find(|device| device.public_key == *public_key)
    .map(|device| device.id)
}

/// The registrations to revoke, unless there are none or a refusal makes them wait.
fn due_list(store: &impl LocalStore, now_ms: i64) -> Result<Option<AbandonedDevices>, CoreError> {
  Ok(records::load_abandoned_devices(store)?.filter(|abandoned| {
    abandoned
      .maybe_retry_at_ms
      .is_none_or(|retry_at_ms| retry_at_ms <= now_ms)
  }))
}

/// Returns `error`, and after a refusal that won't pass by itself delays the next try (doubling,
/// [`RETRY_FIRST`] to [`RETRY_MAX`]), so a server that keeps refusing isn't asked every upkeep.
fn back_off(
  store: &impl LocalStore,
  abandoned: &mut AbandonedDevices,
  error: CoreError,
  now_ms: i64,
) -> CoreError {
  if is_retryable(&error) {
    return error;
  }
  abandoned.failures = abandoned.failures.saturating_add(1);
  let doublings = abandoned.failures.saturating_sub(1).min(16);
  let wait = RETRY_FIRST.saturating_mul(2u32.pow(doublings)).min(RETRY_MAX);
  let wait_ms = i64::try_from(wait.as_millis()).unwrap_or(i64::MAX);
  abandoned.maybe_retry_at_ms = Some(now_ms.saturating_add(wait_ms));
  if let Err(save_error) = records::save_abandoned_devices(store, abandoned) {
    tracing::warn!(error = %save_error, "could not note when to revoke abandoned registrations next");
  }
  error
}

/// A failure the next try may not see: the network, or a sign-in that is renewed.
fn is_retryable(error: &CoreError) -> bool {
  error.is_transient() || error.is_sign_in_required()
}

/// [`records::abandon_pending_device`] once the key pair is older than `expiry` and nothing can
/// finish its enrollment.
fn drop_expired_device(store: &impl LocalStore, now_ms: i64, expiry: Duration) -> Result<bool, CoreError> {
  // Enrolled: a pending record left next to the account holds this device's own key pair.
  if records::is_account_stored(store)? {
    return Ok(false);
  }
  // The keys of a link wait for the enrollment that needs this registration (issue #168).
  if records::has_pending_link(store)? {
    return Ok(false);
  }
  let Some(created_at_ms) = records::pending_device_created_at(store, now_ms)? else {
    return Ok(false);
  };
  // The local clock decides; a wrong one only drops a key pair nobody uses sooner or later.
  let expiry_ms = i64::try_from(expiry.as_millis()).unwrap_or(i64::MAX);
  if now_ms.saturating_sub(created_at_ms) < expiry_ms {
    return Ok(false);
  }
  tracing::info!("dropping the device key pair of a setup that was given up");
  records::abandon_pending_device(store)
}

#[cfg(test)]
mod tests {
  use teamcraft_crypto::device_list::{DeviceList, DeviceListEntry};
  use teamcraft_crypto::identity::{DeviceKeyPair, IdentityKeyPair};
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  const OURS: [u8; 32] = [1; 32];

  #[test]
  fn only_a_registration_no_enrolled_device_can_use_is_revoked() {
    let identity = IdentityKeyPair::generate(&mut rand_core::OsRng);
    let key = *identity.public_key().as_bytes();
    let (listed, unlisted, own) = (device(7), device(8), device(9));
    let account = AccountDevices::Identity {
      key,
      maybe_list: Some(list_naming(&identity, listed)),
    };
    assert_eq!(verdict(&account, None, &key, listed), Verdict::InUse);
    assert_eq!(verdict(&account, None, &key, unlisted), Verdict::Revoke);
    assert_eq!(verdict(&account, Some(own), &key, own), Verdict::InUse);
    // Certified by an identity that never became the account's: nobody can use it.
    assert_eq!(verdict(&account, None, &OURS, listed), Verdict::Revoke);
    assert_eq!(
      verdict(&AccountDevices::NoIdentity, None, &OURS, listed),
      Verdict::Revoke
    );
    // Without a list nothing is known.
    let unknown = AccountDevices::Identity {
      key,
      maybe_list: None,
    };
    assert_eq!(verdict(&unknown, None, &key, unlisted), Verdict::Unverifiable);
  }

  #[test]
  fn refusals_wait_longer_each_time_up_to_a_day() {
    let store = crate::store::MemoryStore::new();
    let mut abandoned = AbandonedDevices::default();
    let refused = || CoreError::Corrupt("refused".into());
    let hour_ms = 60 * 60 * 1000;
    back_off(&store, &mut abandoned, refused(), 0);
    assert_eq!(abandoned.maybe_retry_at_ms, Some(hour_ms));
    back_off(&store, &mut abandoned, refused(), 0);
    assert_eq!(abandoned.maybe_retry_at_ms, Some(2 * hour_ms));
    for _ in 0..10 {
      back_off(&store, &mut abandoned, refused(), 0);
    }
    assert_eq!(abandoned.maybe_retry_at_ms, Some(24 * hour_ms));
    // The network doesn't count.
    let offline = CoreError::Api(crate::api::ApiError::Network("offline".into()));
    let mut fresh = AbandonedDevices::default();
    back_off(&store, &mut fresh, offline, 0);
    assert_eq!(fresh.maybe_retry_at_ms, None);
  }

  fn device(raw: i64) -> DeviceId {
    DeviceId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn list_naming(identity: &IdentityKeyPair, device: DeviceId) -> VerifiedDeviceList {
    let user = UserId::from_snowflake(Snowflake::new(5).unwrap());
    let keys = DeviceKeyPair::generate(
      convert::user(user),
      convert::device(device),
      &mut rand_core::OsRng,
    );
    let entry = DeviceListEntry {
      device_id: convert::device(device),
      signing_key: keys.public_keys().signing_key,
    };
    DeviceList::next(convert::user(user), None, vec![entry], &[])
      .sign(identity)
      .unwrap()
      .verify(convert::user(user), &identity.public_key())
      .unwrap()
  }
}
