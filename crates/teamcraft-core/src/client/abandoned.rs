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

use std::time::Duration;

use futures_util::future::{Either, select};
use teamcraft_proto::UserId;

use super::enroll::revoke_orphan;
use super::{Client, ClientSettings, PendingEnrollment};
use crate::api::ServerApi;
use crate::clock::Clock;
use crate::error::CoreError;
use crate::store::LocalStore;
use crate::{platform, records};

/// How long signing out waits for the server to revoke the device an unfinished enrollment
/// registered. What is left by then is revoked when the account is opened next.
pub const SIGN_OUT_REVOKE_PATIENCE: Duration = Duration::from_secs(5);

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
    let is_dropped = drop_expired_device(store, clock.now_ms(), settings.unfinished_enrollment_expiry)?;
    if records::load_abandoned_devices(store)?.is_some() {
      let me = api.me().await?.id;
      revoke_abandoned_devices(api, store, me).await?;
    }
    Ok(is_dropped)
  }

  /// The user signs out before this device is set up (issues #168, #415): the keys of a link and the
  /// device key pair go at once ([`records::forget_unfinished_enrollment`]); the device the key pair
  /// registered is revoked if the server answers within `patience`, otherwise when this account is
  /// opened next. Leaves an enrolled account alone.
  pub async fn forget_unfinished<A: ServerApi, S: LocalStore>(
    api: &A,
    store: &S,
    patience: Duration,
  ) -> Result<(), CoreError> {
    records::forget_unfinished_enrollment(store)?;
    if records::load_abandoned_devices(store)?.is_none() {
      return Ok(());
    }
    let revoking = Box::pin(async {
      let me = api.me().await?.id;
      revoke_abandoned_devices(api, store, me).await
    });
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
  /// [`revoke_abandoned_devices`] for this account: devices registered by setups given up before
  /// this one finished are revoked from the upkeep once the server can be reached.
  pub async fn revoke_abandoned_devices(&self) -> Result<usize, CoreError> {
    revoke_abandoned_devices(&self.api, &self.store, self.account.user_id).await
  }
}

/// Revokes the devices that enrollments given up on registered, as far as the server still lists
/// them (issue #415), and returns how many it revoked. A failure that may pass (offline, sign-in)
/// stops it and keeps the rest for the next call; a registration the server refuses to revoke for
/// good is given up (the user can still remove it from the device list). Never revokes this
/// store's own device. Asks the server nothing while there is nothing to revoke.
pub async fn revoke_abandoned_devices<A: ServerApi, S: LocalStore>(
  api: &A,
  store: &S,
  user: UserId,
) -> Result<usize, CoreError> {
  let Some(mut abandoned) = records::load_abandoned_devices(store)? else {
    return Ok(0);
  };
  if abandoned.user_id != user {
    // Stores are per account; another account's devices can't be revoked from this one.
    tracing::warn!("dropping the device registrations to revoke of another account");
    abandoned.public_keys.clear();
    records::save_abandoned_devices(store, &abandoned)?;
    return Ok(0);
  }
  let maybe_own = records::stored_device_id(store)?;
  let registered = api.list_user_devices(user).await?.devices;
  let mut revoked = 0;
  while let Some(public_key) = abandoned.public_keys.first().cloned() {
    let maybe_device = registered
      .iter()
      .find(|device| device.public_key == public_key)
      .map(|device| device.id);
    match maybe_device {
      // A leftover of an enrollment that did finish: the key pair is this device's own.
      Some(device) if Some(device) == maybe_own => {
        tracing::warn!(device_id = %device, "an abandoned registration is this device; keeping it");
      },
      Some(device) => match revoke_orphan(api, device).await {
        Ok(()) => revoked += 1,
        Err(error) if error.is_transient() || error.is_sign_in_required() => return Err(error),
        Err(error) => {
          tracing::warn!(%error, device_id = %device, "giving up revoking an abandoned device registration");
        },
      },
      // Never registered, or revoked already.
      None => {},
    }
    abandoned.public_keys.remove(0);
    records::save_abandoned_devices(store, &abandoned)?;
  }
  Ok(revoked)
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
