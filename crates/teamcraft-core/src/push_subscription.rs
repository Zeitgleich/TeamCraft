//! A Web Push subscription the browser rotated while no page was open (e2ee.md §7, issue #145).
//!
//! Registering a subscription normally needs the page that owns the store (e2ee.md §9.4). The
//! service worker can't write the store and has no long-lived credential, so it proves the change
//! with the push key the server already holds: an HMAC over the device and both subscriptions
//! (`teamcraft_crypto::push::subscription_change_mac`). The server swaps the old subscription for
//! the new one only; the page re-registers it as usual the next time it starts
//! ([`crate::Client::check_web_push`]), which is then a no-op on the server.

use teamcraft_crypto::push::subscription_change_mac;
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{Base64Bytes, ChangeWebPushSubscriptionRequest, PushProvider};

use crate::client::registered_push_token;
use crate::convert;
use crate::error::CoreError;
use crate::push_preview::{PUSH_KEY_RECORD, push_server_url};
use crate::records;
use crate::store::{LocalStore, Table};

/// Where to send a subscription change, and what.
#[derive(Debug)]
pub struct SubscriptionChange {
  pub server_url: String,
  pub request: ChangeWebPushSubscriptionRequest,
}

/// The change from the registered subscription to `token` (the browser's current one, JSON), or
/// `None` when there is nothing to do: no Web Push registration, the same subscription, or no
/// server or push key known. Only reads the store.
pub fn subscription_change(
  store: &impl LocalStore,
  token: &str,
) -> Result<Option<SubscriptionChange>, CoreError> {
  let Some(previous_token) = registered_push_token(store, &PushProvider::WebPush)? else {
    return Ok(None);
  };
  subscription_change_from(store, &previous_token, token)
}

/// The change from `previous_token` to `token`: for a second rotation before a page opened, where
/// the server holds the subscription the worker sent last time (the browser's `oldSubscription`)
/// rather than the one in the store. `None` as for [`subscription_change`], and also when this
/// device has no Web Push registration at all.
pub fn subscription_change_from(
  store: &impl LocalStore,
  previous_token: &str,
  token: &str,
) -> Result<Option<SubscriptionChange>, CoreError> {
  if previous_token == token || registered_push_token(store, &PushProvider::WebPush)?.is_none() {
    return Ok(None);
  }
  let (Some(device_id), Some(server_url), Some(push_key)) = (
    records::stored_device_id(store)?,
    push_server_url(store)?,
    stored_push_key(store)?,
  ) else {
    return Ok(None);
  };
  let mac = subscription_change_mac(&push_key, convert::device(device_id), previous_token, token);
  Ok(Some(SubscriptionChange {
    server_url,
    request: ChangeWebPushSubscriptionRequest {
      device_id,
      previous_token: previous_token.to_owned(),
      token: token.to_string(),
      mac: Base64Bytes::new(mac.to_vec()),
    },
  }))
}

fn stored_push_key(store: &impl LocalStore) -> Result<Option<SymmetricKey>, CoreError> {
  let stored: Option<Base64Bytes> = records::get_json(store, Table::Account, PUSH_KEY_RECORD)?;
  Ok(stored.and_then(|bytes| SymmetricKey::from_slice(bytes.as_slice())))
}
