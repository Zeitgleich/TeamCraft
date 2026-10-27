//! Windows: toasts through WinRT, tagged so a newer one replaces the older and a read conversation's
//! or an answered call's toast is removed from the screen and the Action Center. Buttons and the
//! reply field come back through the toast's `Activated` event while the app runs (it keeps
//! running in the tray); the installer registers the app's identifier as AppUserModelID.

use std::sync::Arc;

use teamcraft_client_api::os_notify::{
  NotificationAction, NotificationActionEvent, NotificationTag, OsNotification,
};
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::{IPropertyValue, TypedEventHandler};
use windows::UI::Notifications::{ToastActivatedEventArgs, ToastNotification, ToastNotificationManager};
use windows::core::{HSTRING, IInspectable, Interface};

use super::toast_xml::{REPLY_INPUT_ID, toast_xml};

/// Every toast of the app is in this group, so withdrawing by tag finds it.
const GROUP: &str = "teamcraft";

/// Reports a click or button to the webview.
pub type OnAction = Arc<dyn Fn(NotificationActionEvent) + Send + Sync>;

/// Shows (or replaces) the toast of `notification`.
pub fn show(app_id: &str, notification: &OsNotification, on_action: OnAction) -> windows::core::Result<()> {
  let document = XmlDocument::new()?;
  document.LoadXml(&HSTRING::from(toast_xml(notification)))?;
  let toast = ToastNotification::CreateToastNotification(&document)?;
  toast.SetTag(&HSTRING::from(notification.tag.as_str()))?;
  toast.SetGroup(&HSTRING::from(GROUP))?;
  let tag = notification.tag.clone();
  toast.Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(
    move |_, args| {
      if let Some(args) = args.as_ref() {
        on_action(activation(&tag, args));
      }
      Ok(())
    },
  ))?;
  ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(app_id))?.Show(&toast)
}

/// Removes the toasts with these tags (already gone ones are no failure).
pub fn withdraw(app_id: &str, tags: &[String]) {
  let history = match ToastNotificationManager::History() {
    Ok(history) => history,
    Err(error) => {
      tracing::debug!(%error, "no toast history");
      return;
    },
  };
  for tag in tags {
    if let Err(error) = history.RemoveGroupedTagWithId(
      &HSTRING::from(tag.as_str()),
      &HSTRING::from(GROUP),
      &HSTRING::from(app_id),
    ) {
      tracing::trace!(%error, "toast not withdrawn");
    }
  }
}

/// What the user did: the button's arguments (the toast's `launch` for a click) and the reply text.
fn activation(tag: &NotificationTag, args: &IInspectable) -> NotificationActionEvent {
  let maybe_args = args.cast::<ToastActivatedEventArgs>().ok();
  let action = maybe_args
    .as_ref()
    .and_then(|args| args.Arguments().ok())
    .map_or(NotificationAction::Open, |arguments| {
      NotificationAction::from_key(&arguments.to_string_lossy())
    });
  let maybe_text = maybe_args
    .as_ref()
    .filter(|_| action == NotificationAction::Reply)
    .and_then(|args| args.UserInput().ok())
    .and_then(|input| input.Lookup(&HSTRING::from(REPLY_INPUT_ID)).ok())
    .and_then(|value| value.cast::<IPropertyValue>().ok())
    .and_then(|value| value.GetString().ok())
    .map(|text| text.to_string_lossy());
  NotificationActionEvent {
    tag: tag.clone(),
    action,
    maybe_text,
  }
}
