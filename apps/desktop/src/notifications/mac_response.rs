//! What a macOS notification response means for the webview: the notification's identifier is its
//! tag, a button's identifier is the action's key (`mac_center::show`), and a click or a dismissal
//! are macOS's own identifiers. Pure, so it is tested on every platform.

use teamcraft_client_api::os_notify::{NotificationAction, NotificationActionEvent, NotificationTag};
use teamcraft_desktop_native::{NotificationResponse, ResponseAction};

/// The event for the webview; `None` for a dismissal (swiping a notification away is no action).
pub fn action_event(response: NotificationResponse) -> Option<NotificationActionEvent> {
  let action = match response.action {
    ResponseAction::Default => NotificationAction::Open,
    ResponseAction::Dismiss => return None,
    ResponseAction::Custom(identifier) => NotificationAction::from_key(&identifier),
  };
  let maybe_text = response
    .maybe_text
    .filter(|_| action == NotificationAction::Reply);
  Some(NotificationActionEvent {
    tag: NotificationTag::from_raw(response.request_id),
    action,
    maybe_text,
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  const TAG: &str = "conversation-1";

  #[test]
  fn a_click_opens() {
    let event = action_event(response(ResponseAction::Default, None)).unwrap();
    assert_eq!(event.action, NotificationAction::Open);
    assert_eq!(event.tag, NotificationTag::from_raw(TAG));
  }

  #[test]
  fn buttons_map_by_key() {
    for action in [
      NotificationAction::AcceptAudio,
      NotificationAction::Decline,
      NotificationAction::MarkRead,
      NotificationAction::JoinMeeting,
    ] {
      let custom = ResponseAction::Custom(action.key().to_owned());
      assert_eq!(action_event(response(custom, None)).unwrap().action, action);
    }
  }

  #[test]
  fn a_reply_carries_its_text() {
    let custom = ResponseAction::Custom(NotificationAction::Reply.key().to_owned());
    let event = action_event(response(custom, Some("on my way"))).unwrap();
    assert_eq!(event.action, NotificationAction::Reply);
    assert_eq!(event.maybe_text.as_deref(), Some("on my way"));
  }

  #[test]
  fn text_of_other_actions_is_dropped() {
    let custom = ResponseAction::Custom(NotificationAction::MarkRead.key().to_owned());
    assert_eq!(
      action_event(response(custom, Some("x"))).unwrap().maybe_text,
      None
    );
  }

  #[test]
  fn a_dismissal_reports_nothing() {
    assert_eq!(action_event(response(ResponseAction::Dismiss, None)), None);
  }

  #[test]
  fn an_unknown_identifier_is_unknown() {
    let custom = ResponseAction::Custom("from-the-future".to_owned());
    assert_eq!(
      action_event(response(custom, None)).unwrap().action,
      NotificationAction::Unknown
    );
  }

  fn response(action: ResponseAction, maybe_text: Option<&str>) -> NotificationResponse {
    NotificationResponse {
      request_id: TAG.to_owned(),
      action,
      maybe_text: maybe_text.map(str::to_owned),
    }
  }
}
