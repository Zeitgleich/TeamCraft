//! macOS: the `UNUserNotificationCenter` delegate that receives clicks, buttons and typed replies
//! of the app's notifications, and lets banners show while the app is in front. Buttons and the
//! reply field only come back through a delegate, and an Objective-C class cannot be defined
//! without `unsafe`, which is why this lives here and not in the shell.

/// What the user did, as macOS reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResponseAction {
  /// Clicked the notification itself.
  Default,
  /// Swiped it away.
  Dismiss,
  /// A button or the reply field, by the identifier the app gave it.
  Custom(String),
}

/// A response to one of the app's notifications.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationResponse {
  /// The identifier the notification was requested with.
  pub request_id: String,
  pub action: ResponseAction,
  /// What the user typed into a reply field.
  pub maybe_text: Option<String>,
}

/// Makes `on_response` hear the responses (from a thread of the system's choosing; keep it short)
/// and shows notifications as banners while the app is in front. Call it once, only from a bundled
/// app (without a bundle the notification center aborts the process), before the first
/// notification. Where there is no notification center it does nothing.
pub fn install_notification_delegate(on_response: impl Fn(NotificationResponse) + Send + Sync + 'static) {
  imp::install(Box::new(on_response));
}

type OnResponse = Box<dyn Fn(NotificationResponse) + Send + Sync + 'static>;

#[cfg(target_os = "macos")]
// The whole module is the Objective-C class definition; `define_class!` expands to `unsafe` code.
#[allow(unsafe_code)]
mod imp {
  use block2::DynBlock;
  use objc2::rc::Retained;
  use objc2::runtime::ProtocolObject;
  use objc2::{AnyThread, DefinedClass, define_class, msg_send};
  use objc2_foundation::{NSObject, NSObjectProtocol};
  use objc2_user_notifications::{
    UNNotification, UNNotificationPresentationOptions, UNNotificationResponse,
    UNTextInputNotificationResponse, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
  };

  use super::{NotificationResponse, OnResponse, ResponseAction};

  /// `UNNotificationDefaultActionIdentifier` and `UNNotificationDismissActionIdentifier`.
  const DEFAULT_ACTION: &str = "com.apple.UNNotificationDefaultActionIdentifier";
  const DISMISS_ACTION: &str = "com.apple.UNNotificationDismissActionIdentifier";

  struct Ivars {
    on_response: OnResponse,
  }

  define_class!(
    // SAFETY: the superclass `NSObject` has no subclassing requirements and `NotificationDelegate`
    // does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[ivars = Ivars]
    struct NotificationDelegate;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for NotificationDelegate {}

    // SAFETY: the method signatures below are those of `UNUserNotificationCenterDelegate`.
    unsafe impl UNUserNotificationCenterDelegate for NotificationDelegate {
      #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
      fn will_present(
        &self,
        _center: &UNUserNotificationCenter,
        _notification: &UNNotification,
        completion_handler: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
      ) {
        // The webview decided to notify, so it is shown even while the app is in front.
        completion_handler.call((UNNotificationPresentationOptions::Banner
          | UNNotificationPresentationOptions::List
          | UNNotificationPresentationOptions::Sound,));
      }

      #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
      fn did_receive(
        &self,
        _center: &UNUserNotificationCenter,
        response: &UNNotificationResponse,
        completion_handler: &DynBlock<dyn Fn()>,
      ) {
        let action_id = response.actionIdentifier().to_string();
        let action = match action_id.as_str() {
          DEFAULT_ACTION => ResponseAction::Default,
          DISMISS_ACTION => ResponseAction::Dismiss,
          _ => ResponseAction::Custom(action_id),
        };
        let maybe_text = response
          .downcast_ref::<UNTextInputNotificationResponse>()
          .map(|typed| typed.userText().to_string());
        let request_id = response.notification().request().identifier().to_string();
        (self.ivars().on_response)(NotificationResponse {
          request_id,
          action,
          maybe_text,
        });
        completion_handler.call(());
      }
    }
  );

  impl NotificationDelegate {
    fn new(on_response: OnResponse) -> Retained<Self> {
      let this = Self::alloc().set_ivars(Ivars { on_response });
      // SAFETY: `init` of `NSObject` takes no arguments and returns the initialized object.
      unsafe { msg_send![super(this), init] }
    }
  }

  pub fn install(on_response: OnResponse) {
    let delegate = NotificationDelegate::new(on_response);
    UNUserNotificationCenter::currentNotificationCenter()
      .setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // The center holds its delegate weakly; this one lives as long as the process.
    std::mem::forget(delegate);
  }
}

#[cfg(not(target_os = "macos"))]
mod imp {
  use super::OnResponse;

  pub fn install(_on_response: OnResponse) {}
}
