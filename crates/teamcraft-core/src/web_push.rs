//! Web Push in the browser (e2ee.md §7): subscribing the page, and the service worker's handlers.
//!
//! Browsers rotate or drop subscriptions on their own; [`Client::check_web_push`] notices that at
//! start and when the service worker reports `pushsubscriptionchange`, and registers again.
//!
//! The service worker is `web/service-worker.js` (the minimal JavaScript the platform forces: push
//! and click listeners must be registered synchronously at startup, before wasm has loaded). It
//! calls [`handle_push`] and [`handle_notification_click`], exported with the `service-worker`
//! feature. Everything else happens here: the service worker opens the encrypted store read-only,
//! opens the sealed wake-up with the device push key and shows the preview, with the sender's or
//! channel's picture from the media cache (#543, [`crate::push_picture`]).

use teamcraft_proto::PushProvider;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use crate::Client;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::platform::describe_js_error;
use crate::store::LocalStore;

/// Subscribes this browser to Web Push via the page's service worker registration and returns
/// the subscription as JSON (the registration token the server expects).
///
/// `vapid_public_key` is the server's application server key, base64url (the server logs it at
/// start; see the crate README).
pub async fn subscribe(vapid_public_key: &str) -> Result<String, CoreError> {
  let registration = registration().await?;
  let options = web_sys::PushSubscriptionOptionsInit::new();
  // Browsers only allow subscriptions that show a notification for every push.
  options.set_user_visible_only(true);
  options.set_application_server_key_opt_str(Some(vapid_public_key));
  let subscribe = registration
    .push_manager()
    .and_then(|manager| manager.subscribe_with_options(&options))
    .map_err(|error| js_failure("pushManager.subscribe", &error))?;
  let subscription: web_sys::PushSubscription = JsFuture::from(subscribe)
    .await
    .map_err(|error| js_failure("pushManager.subscribe", &error))?
    .unchecked_into();
  subscription_json(&subscription)
}

/// The browser's current subscription as JSON (`pushManager.getSubscription()`); `None` when the
/// browser has none (it expired, or the user revoked the permission).
pub async fn current_subscription() -> Result<Option<String>, CoreError> {
  let registration = registration().await?;
  let request = registration
    .push_manager()
    .and_then(|manager| manager.get_subscription())
    .map_err(|error| js_failure("pushManager.getSubscription", &error))?;
  let subscription = JsFuture::from(request)
    .await
    .map_err(|error| js_failure("pushManager.getSubscription", &error))?;
  if subscription.is_null() || subscription.is_undefined() {
    return Ok(None);
  }
  subscription_json(&subscription.unchecked_into()).map(Some)
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Subscribes this browser and registers the subscription with a fresh push key.
  pub async fn register_web_push(&mut self, vapid_public_key: &str) -> Result<(), CoreError> {
    let subscription = subscribe(vapid_public_key).await?;
    self.register_push(PushProvider::WebPush, &subscription).await
  }

  /// Browsers rotate or drop push subscriptions without asking (e2ee.md §7). Call at start and
  /// when the service worker reports `pushsubscriptionchange`: a dropped subscription is made again
  /// with the server's key, a changed one is registered under the same push key. Does nothing when
  /// this device has no Web Push registration. Returns whether the registration changed.
  pub async fn check_web_push(&mut self) -> Result<bool, CoreError> {
    if self.registered_push_provider()? != Some(PushProvider::WebPush) {
      return Ok(false);
    }
    let subscription = match current_subscription().await? {
      Some(subscription) => subscription,
      None => {
        let key = self
          .api()
          .web_push_key()
          .await?
          .maybe_public_key
          .ok_or_else(|| browser_error("the server no longer sends Web Push"))?;
        subscribe(&key).await?
      },
    };
    self.renew_push_token(PushProvider::WebPush, &subscription).await
  }
}

/// The page's service worker registration, once it is active.
async fn registration() -> Result<web_sys::ServiceWorkerRegistration, CoreError> {
  let window = web_sys::window().ok_or_else(|| browser_error("no window: subscribe from a page"))?;
  let ready = window
    .navigator()
    .service_worker()
    .ready()
    .map_err(|error| js_failure("serviceWorker.ready", &error))?;
  Ok(
    JsFuture::from(ready)
      .await
      .map_err(|error| js_failure("serviceWorker.ready", &error))?
      .unchecked_into(),
  )
}

/// The subscription as the server expects it: `PushSubscription.toJSON()`, as text.
fn subscription_json(subscription: &web_sys::PushSubscription) -> Result<String, CoreError> {
  let json = subscription
    .to_json()
    .map_err(|error| js_failure("subscription.toJSON", &error))?;
  js_sys::JSON::stringify(&json)
    .map(String::from)
    .map_err(|error| js_failure("JSON.stringify", &error))
}

#[cfg(feature = "service-worker")]
pub use service_worker::{handle_notification_click, handle_push, handle_subscription_change};

#[cfg(feature = "service-worker")]
mod service_worker {
  use teamcraft_proto::{ConversationId, MessageId, SetReadPositionRequest};
  use wasm_bindgen::prelude::wasm_bindgen;
  use wasm_bindgen::{JsCast, JsValue};
  use wasm_bindgen_futures::JsFuture;
  use web_sys::{NotificationOptions, ServiceWorkerGlobalScope};

  use crate::api::ApiError;
  use crate::api::{HttpServerApi, ServerApi, StaticToken, change_web_push_subscription};
  use crate::clock::{Clock, SystemClock};
  use crate::error::CoreError;
  use crate::media_cache::{BrowserMediaCache, MAX_MEDIA_MAX_AGE, MediaKey};
  use crate::push_picture::{account_user, icon_data_url, push_picture_keys};
  use crate::push_preview::{PushPreview, push_preview, push_session};
  use crate::push_subscription::{subscription_change, subscription_change_from};
  use crate::push_text::{NotificationLabels, notification_text};
  use crate::store::{IndexedDbStore, browser_database_key};

  /// Key of the conversation ID in a notification's `data`.
  const CONVERSATION_FIELD: &str = "conversation_id";
  /// Key of the tag in the `data` of notifications the page showed (`teamcraft-ui`).
  const TAG_FIELD: &str = "tag";
  /// The action a plain click reports (`NotificationAction::Open` in `teamcraft-client-api`).
  const CLICK_ACTION: &str = "default";
  /// The one button that needs no window in front (`NotificationAction::MarkRead`).
  const MARK_READ_ACTION: &str = "mark-read";
  /// Key of the newest message's ID in a notification's `data`, for the "mark as read" button.
  const MESSAGE_FIELD: &str = "message_id";

  /// The service worker's `push` handler: decrypts the wake-up and shows the notification.
  /// Browsers require a notification for every push, so failures still show one that says a
  /// message came. `maybe_labels` is the JSON of [`NotificationLabels`], the translated texts the
  /// page leaves next to the database name (the worker has no translations, and no page may be
  /// open); without them the texts are English and the notification has no "Mark as read" button.
  #[wasm_bindgen(js_name = handlePush)]
  pub async fn handle_push(
    database_name: String,
    payload: Vec<u8>,
    maybe_labels: Option<String>,
  ) -> Result<(), JsValue> {
    let scope: ServiceWorkerGlobalScope = js_sys::global().unchecked_into();
    let labels = maybe_labels
      .as_deref()
      .map(NotificationLabels::from_json)
      .unwrap_or_default();
    let (title, options) = match preview(&database_name, &payload).await {
      Ok(opened) => {
        let preview = &opened.preview;
        if let Some(gap) = preview.maybe_gap {
          // Why the notification shows less than the message; never what the message says.
          tracing::info!(?gap, "push shown without the full message");
        }
        let (title, options) = notification(preview, &labels);
        if let Some(icon) = picture_icon(opened.maybe_account, &opened.picture_keys).await {
          options.set_icon(&icon);
        }
        (title, options)
      },
      Err(error) => {
        tracing::warn!(%error, "push could not be opened");
        let text = labels.generic();
        let options = NotificationOptions::new();
        options.set_body(&text.body);
        (text.title, options)
      },
    };
    let promise = scope
      .registration()
      .show_notification_with_options(&title, &options)?;
    JsFuture::from(promise).await?;
    Ok(())
  }

  /// The service worker's `notificationclick` handler: focuses an open app window and tells it which
  /// conversation to show, or opens one. A button (or a click on a notification the page showed for
  /// a call or meeting) goes to the page as a `notification_action` message with the notification's
  /// tag; "mark as read" does so without bringing the window up. On a push notification (it names
  /// the newest message) the worker sets the read position itself with the push session's token,
  /// so the button works while no tab is open; if that fails the click opens the conversation.
  #[wasm_bindgen(js_name = handleNotificationClick)]
  pub async fn handle_notification_click(
    event: web_sys::NotificationEvent,
    maybe_database_name: Option<String>,
  ) -> Result<(), JsValue> {
    let notification = event.notification();
    notification.close();
    let data = notification.data();
    let field = |key: &str| {
      js_sys::Reflect::get(&data, &JsValue::from_str(key))
        .ok()
        .and_then(|value| value.as_string())
    };
    let maybe_conversation = field(CONVERSATION_FIELD);
    let maybe_tag = field(TAG_FIELD);
    let maybe_message = field(MESSAGE_FIELD);
    // `NotificationEvent.action` is behind web-sys' unstable APIs; read the property directly.
    let action = js_sys::Reflect::get(&event, &JsValue::from_str("action"))
      .ok()
      .and_then(|value| value.as_string())
      .unwrap_or_default();
    if action == MARK_READ_ACTION
      && let (Some(database_name), Some(conversation), Some(message)) =
        (&maybe_database_name, &maybe_conversation, &maybe_message)
    {
      match mark_read(database_name, conversation, message).await {
        Ok(()) => return Ok(()),
        // Falls through to opening the conversation: the user still wants to see it.
        Err(error) => tracing::warn!(%error, "could not mark the conversation read from its notification"),
      }
    }
    let scope: ServiceWorkerGlobalScope = js_sys::global().unchecked_into();
    let clients = scope.clients();
    let query = web_sys::ClientQueryOptions::new();
    query.set_type(web_sys::ClientType::Window);
    query.set_include_uncontrolled(true);
    let windows: js_sys::Array = JsFuture::from(clients.match_all_with_options(&query))
      .await?
      .unchecked_into();
    if let Some(window) = windows.iter().next() {
      let window: web_sys::WindowClient = window.unchecked_into();
      let is_page_action = !action.is_empty() || (maybe_conversation.is_none() && maybe_tag.is_some());
      let message = js_sys::Object::new();
      if let (true, Some(tag)) = (is_page_action, &maybe_tag) {
        let action = if action.is_empty() {
          CLICK_ACTION
        } else {
          action.as_str()
        };
        js_sys::Reflect::set(&message, &"type".into(), &"notification_action".into())?;
        js_sys::Reflect::set(&message, &TAG_FIELD.into(), &tag.into())?;
        js_sys::Reflect::set(&message, &"action".into(), &action.into())?;
        window.post_message(&message)?;
        if action == MARK_READ_ACTION {
          return Ok(());
        }
      } else if let Some(conversation) = &maybe_conversation {
        js_sys::Reflect::set(&message, &"type".into(), &"open_conversation".into())?;
        js_sys::Reflect::set(&message, &CONVERSATION_FIELD.into(), &conversation.into())?;
        window.post_message(&message)?;
      }
      JsFuture::from(window.focus()?).await?;
      return Ok(());
    }
    let mut url = scope.registration().scope();
    if let Some(conversation) = maybe_conversation {
      url.push_str(&format!("?{CONVERSATION_FIELD}={conversation}"));
    }
    JsFuture::from(clients.open_window(&url)).await?;
    Ok(())
  }

  /// Sets the read position to the notification's message with the stored push session.
  async fn mark_read(database_name: &str, conversation: &str, message: &str) -> Result<(), CoreError> {
    let unreadable = || CoreError::Corrupt("unreadable notification data".into());
    let conversation: ConversationId = conversation.parse().map_err(|_| unreadable())?;
    let message_id: MessageId = message.parse().map_err(|_| unreadable())?;
    let key = browser_database_key(database_name).await?;
    let store = IndexedDbStore::open(database_name, &key).await?;
    let session = push_session(&store, SystemClock.now_ms())?
      .ok_or_else(|| CoreError::Browser("no valid push session".into()))?;
    let api = HttpServerApi::new(&session.server_url, StaticToken(session.access_token));
    api
      .set_read_position(conversation, &SetReadPositionRequest { message_id })
      .await?;
    Ok(())
  }

  /// The service worker's `pushsubscriptionchange` handler, after it subscribed again where the
  /// browser handed no new subscription: tells the server about the browser's current subscription
  /// right away, so pushes don't go to the old one until a page opens (issue #145). Reads the store
  /// only; the page registers the subscription itself when it starts next. Failures are logged and
  /// leave that page as the fallback. `maybe_old_subscription` is the browser's `oldSubscription`
  /// (JSON), for a second rotation before a page opened: the server then holds the one this worker
  /// sent before, not the one in the store.
  #[wasm_bindgen(js_name = handleSubscriptionChange)]
  pub async fn handle_subscription_change(
    database_name: String,
    maybe_old_subscription: Option<String>,
  ) -> Result<(), JsValue> {
    let scope: ServiceWorkerGlobalScope = js_sys::global().unchecked_into();
    let request = scope.registration().push_manager()?.get_subscription()?;
    let subscription = JsFuture::from(request).await?;
    if subscription.is_null() || subscription.is_undefined() {
      tracing::info!("no push subscription after the change; the page subscribes again when it opens");
      return Ok(());
    }
    let token = super::subscription_json(&subscription.unchecked_into()).map_err(core_error)?;
    match register_change(&database_name, maybe_old_subscription.as_deref(), &token).await {
      Ok(true) => tracing::info!("registered the rotated push subscription"),
      Ok(false) => tracing::debug!("push subscription unchanged or not registered"),
      Err(error) => tracing::warn!(%error, "rotated push subscription not registered"),
    }
    Ok(())
  }

  async fn register_change(
    database_name: &str,
    maybe_old_subscription: Option<&str>,
    token: &str,
  ) -> Result<bool, CoreError> {
    let key = browser_database_key(database_name).await?;
    let store = IndexedDbStore::open(database_name, &key).await?;
    let Some(change) = subscription_change(&store, token)? else {
      return Ok(false);
    };
    let error = match change_web_push_subscription(&change.server_url, &change.request).await {
      Ok(()) => return Ok(true),
      Err(error) => error,
    };
    // 409: the server no longer holds the store's subscription, most likely the one this worker
    // sent at an earlier rotation. Without `oldSubscription` (not every browser sends it) the page
    // registers the new one when it opens.
    let is_conflict = matches!(error, ApiError::Status { status: 409, .. });
    let maybe_retry = match maybe_old_subscription {
      Some(old) if is_conflict && old != change.request.previous_token => {
        subscription_change_from(&store, old, token)?
      },
      _ => None,
    };
    let Some(retry) = maybe_retry else {
      return Err(error.into());
    };
    change_web_push_subscription(&retry.server_url, &retry.request).await?;
    Ok(true)
  }

  fn core_error(error: CoreError) -> JsValue {
    JsValue::from_str(&error.to_string())
  }

  /// A push opened with the store: the preview, and where its picture may come from.
  struct OpenedPush {
    preview: PushPreview,
    /// The account whose media cache holds the pictures.
    maybe_account: Option<teamcraft_proto::UserId>,
    /// Best first; empty where the preview names nobody.
    picture_keys: Vec<MediaKey>,
  }

  async fn preview(database_name: &str, payload: &[u8]) -> Result<OpenedPush, CoreError> {
    let key = browser_database_key(database_name).await?;
    let store = IndexedDbStore::open(database_name, &key).await?;
    let maybe_api = push_session(&store, SystemClock.now_ms())?
      .map(|session| HttpServerApi::new(&session.server_url, StaticToken(session.access_token)));
    // Read-only: the preview never writes, so the app's own cache stays authoritative.
    let preview = push_preview(&store, maybe_api.as_ref(), payload).await?;
    let picture_keys = push_picture_keys(&store, &preview);
    // Without the account the notification only goes without its picture.
    let maybe_account = account_user(&store).ok().flatten();
    Ok(OpenedPush {
      preview,
      maybe_account,
      picture_keys,
    })
  }

  /// The first of `keys` the account's media cache holds, as a `data:` URL; `None` (the browser's
  /// default icon) when it holds none or can't be opened. The cache's own age setting lives in the
  /// page; the worker takes the longest the settings offer, so its read never drops what the page
  /// keeps.
  async fn picture_icon(maybe_account: Option<teamcraft_proto::UserId>, keys: &[MediaKey]) -> Option<String> {
    let account = maybe_account?;
    if keys.is_empty() {
      return None;
    }
    let cache = match BrowserMediaCache::open(&BrowserMediaCache::database_name(account)).await {
      Ok(cache) => cache,
      Err(error) => {
        tracing::debug!(%error, "no media cache for the notification's picture");
        return None;
      },
    };
    let mut maybe_icon = None;
    for key in keys {
      if let Some(media) = cache.get(key, MAX_MEDIA_MAX_AGE).await
        && let Some(icon) = icon_data_url(&media.content_type, media.bytes.expose_secret())
      {
        maybe_icon = Some(icon);
        break;
      }
    }
    // A sign-out in a page deletes the database; an open handle would hold that up.
    cache.close();
    maybe_icon
  }

  /// Title and body from the preview's names and text and the page's translated labels
  /// ([`notification_text`]).
  fn notification(preview: &PushPreview, labels: &NotificationLabels) -> (String, NotificationOptions) {
    let text = notification_text(preview, labels);
    let options = NotificationOptions::new();
    options.set_body(&text.body);
    // One notification per conversation; a newer push replaces the older one.
    let conversation: ConversationId = preview.wake_up.conversation_id;
    options.set_tag(&conversation.to_string());
    let data = js_sys::Object::new();
    // Setting a property on a fresh plain object can't fail.
    let _ = js_sys::Reflect::set(
      &data,
      &CONVERSATION_FIELD.into(),
      &conversation.to_string().into(),
    );
    // "Mark as read" for messages of a conversation (a thread has its own read position, which the
    // wake-up does not let this button set).
    let maybe_message = preview
      .wake_up
      .maybe_message_id
      .filter(|_| preview.wake_up.maybe_thread_id.is_none() && preview.is_conversation_verified);
    if let (Some(label), Some(message)) = (labels.maybe_mark_read.as_deref(), maybe_message) {
      let _ = js_sys::Reflect::set(&data, &MESSAGE_FIELD.into(), &message.to_string().into());
      let button = js_sys::Object::new();
      let _ = js_sys::Reflect::set(&button, &"action".into(), &MARK_READ_ACTION.into());
      let _ = js_sys::Reflect::set(&button, &"title".into(), &label.into());
      let actions = js_sys::Array::of1(&button);
      // `NotificationOptions.actions` is behind web-sys' unstable APIs; set the property directly.
      let _ = js_sys::Reflect::set(&options, &"actions".into(), &actions);
    }
    options.set_data(&data);
    (text.title, options)
  }
}

fn js_failure(operation: &str, error: &JsValue) -> CoreError {
  browser_error(&format!("{operation}: {}", describe_js_error(error)))
}

fn browser_error(message: &str) -> CoreError {
  CoreError::Browser(message.to_string())
}
