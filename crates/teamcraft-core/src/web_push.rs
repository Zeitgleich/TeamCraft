//! Web Push in the browser (e2ee.md §7): subscribing the page, and the service worker's handlers.
//!
//! Browsers rotate or drop subscriptions on their own; [`Client::check_web_push`] notices that at
//! start and when the service worker reports `pushsubscriptionchange`, and registers again.
//!
//! The service worker is `web/service-worker.js` (the minimal JavaScript the platform forces: push
//! and click listeners must be registered synchronously at startup, before wasm has loaded). It
//! calls [`handle_push`] and [`handle_notification_click`], exported with the `service-worker`
//! feature. Everything else happens here: the service worker opens the encrypted store read-only,
//! opens the sealed wake-up with the device push key and shows the preview.

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
pub use service_worker::{handle_notification_click, handle_push};

#[cfg(feature = "service-worker")]
mod service_worker {
  use teamcraft_proto::ConversationId;
  use wasm_bindgen::prelude::wasm_bindgen;
  use wasm_bindgen::{JsCast, JsValue};
  use wasm_bindgen_futures::JsFuture;
  use web_sys::{NotificationOptions, ServiceWorkerGlobalScope};

  use crate::api::{HttpServerApi, StaticToken};
  use crate::clock::{Clock, SystemClock};
  use crate::error::CoreError;
  use crate::push_preview::{PreviewContent, PushPreview, push_preview, push_session};
  use crate::store::{IndexedDbStore, browser_database_key};

  /// Shown when nothing else is known: the product name, which isn't translated.
  const FALLBACK_TITLE: &str = "TeamCraft";
  /// Key of the conversation ID in a notification's `data`.
  const CONVERSATION_FIELD: &str = "conversation_id";
  /// Key of the tag in the `data` of notifications the page showed (`teamcraft-ui`).
  const TAG_FIELD: &str = "tag";
  /// The action a plain click reports (`NotificationAction::Open` in `teamcraft-client-api`).
  const CLICK_ACTION: &str = "default";
  /// The one button that needs no window in front (`NotificationAction::MarkRead`).
  const MARK_READ_ACTION: &str = "mark-read";

  /// The service worker's `push` handler: decrypts the wake-up and shows the notification.
  /// Browsers require a notification for every push, so failures still show one, without content.
  #[wasm_bindgen(js_name = handlePush)]
  pub async fn handle_push(database_name: String, payload: Vec<u8>) -> Result<(), JsValue> {
    let scope: ServiceWorkerGlobalScope = js_sys::global().unchecked_into();
    let (title, options) = match preview(&database_name, &payload).await {
      Ok(preview) => notification(&preview),
      Err(error) => {
        tracing::warn!(%error, "push could not be opened");
        (FALLBACK_TITLE.to_string(), NotificationOptions::new())
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
  /// tag; "mark as read" does so without bringing the window up.
  #[wasm_bindgen(js_name = handleNotificationClick)]
  pub async fn handle_notification_click(event: web_sys::NotificationEvent) -> Result<(), JsValue> {
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
    // `NotificationEvent.action` is behind web-sys' unstable APIs; read the property directly.
    let action = js_sys::Reflect::get(&event, &JsValue::from_str("action"))
      .ok()
      .and_then(|value| value.as_string())
      .unwrap_or_default();
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

  async fn preview(database_name: &str, payload: &[u8]) -> Result<PushPreview, CoreError> {
    let key = browser_database_key(database_name).await?;
    let store = IndexedDbStore::open(database_name, &key).await?;
    let maybe_api = push_session(&store, SystemClock.now_ms())?
      .map(|session| HttpServerApi::new(&session.server_url, StaticToken(session.access_token)));
    // Read-only: the preview never writes, so the app's own cache stays authoritative.
    push_preview(&store, maybe_api.as_ref(), payload).await
  }

  /// Title and body from data only (names and message text), so nothing here needs translating.
  fn notification(preview: &PushPreview) -> (String, NotificationOptions) {
    let sender = preview.maybe_sender_name.clone();
    let title = if preview.is_direct {
      sender.clone().or_else(|| preview.maybe_conversation_name.clone())
    } else {
      preview.maybe_conversation_name.clone().or_else(|| sender.clone())
    }
    .unwrap_or_else(|| FALLBACK_TITLE.to_string());
    let maybe_text = match &preview.content {
      PreviewContent::Text(text) => Some(text.clone()),
      PreviewContent::File { file_name, .. } => Some(file_name.clone()),
      PreviewContent::Unavailable => None,
    };
    let body = match (preview.is_direct, sender, maybe_text) {
      (false, Some(sender), Some(text)) => format!("{sender}: {text}"),
      (_, _, Some(text)) => text,
      (false, Some(sender), None) => sender,
      (_, _, None) => String::new(),
    };
    let options = NotificationOptions::new();
    options.set_body(&body);
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
    options.set_data(&data);
    (title, options)
  }
}

fn js_failure(operation: &str, error: &JsValue) -> CoreError {
  browser_error(&format!("{operation}: {}", describe_js_error(error)))
}

fn browser_error(message: &str) -> CoreError {
  CoreError::Browser(message.to_string())
}
