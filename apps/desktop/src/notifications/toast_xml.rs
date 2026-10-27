//! The XML of a Windows toast (the Adaptive Toast schema): title and text, the sender's or channel's
//! picture as the logo (#543), buttons, an optional reply field and, for a ringing call, the
//! incoming-call scenario without sound (the app rings itself). A message whose sound the app plays
//! itself (or none, the user's choice) is silent too. Pure, so it is tested on every platform;
//! `win_toast.rs` hands it to WinRT.

use teamcraft_client_api::os_notify::{NotificationAction, OsNotification};

/// The `id` of the reply field; WinRT returns the typed text under it.
pub const REPLY_INPUT_ID: &str = "reply";

/// The picture in place of the app's logo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToastLogo {
  /// A `file:///` URI of the PNG.
  pub uri: String,
  /// A person: Windows cuts the logo to a circle.
  pub is_circle: bool,
}

/// The toast document for `notification`, with `maybe_logo` in place of the app's logo.
pub fn toast_xml(notification: &OsNotification, maybe_logo: Option<&ToastLogo>) -> String {
  let mut xml = String::new();
  let scenario = if notification.kind.is_ringing() {
    r#" scenario="incomingCall""#
  } else {
    ""
  };
  xml.push_str(&format!(
    r#"<toast launch="{}" activationType="foreground"{scenario}>"#,
    NotificationAction::Open.key()
  ));
  xml.push_str(r#"<visual><binding template="ToastGeneric">"#);
  xml.push_str(&format!("<text>{}</text>", escape(&notification.title)));
  if let Some(body) = &notification.maybe_body {
    xml.push_str(&format!("<text>{}</text>", escape(body)));
  }
  if let Some(logo) = maybe_logo {
    let crop = if logo.is_circle {
      r#" hint-crop="circle""#
    } else {
      ""
    };
    xml.push_str(&format!(
      r#"<image placement="appLogoOverride" src="{}"{crop}/>"#,
      escape(&logo.uri)
    ));
  }
  xml.push_str("</binding></visual>");
  if !notification.actions.is_empty() || notification.maybe_reply.is_some() {
    xml.push_str("<actions>");
    if let Some(reply) = &notification.maybe_reply {
      xml.push_str(&format!(
        r#"<input id="{REPLY_INPUT_ID}" type="text" placeHolderContent="{}"/>"#,
        escape(&reply.placeholder)
      ));
      xml.push_str(&format!(
        r#"<action content="{}" arguments="{}" hint-inputId="{REPLY_INPUT_ID}" activationType="foreground"/>"#,
        escape(&reply.send_label),
        NotificationAction::Reply.key()
      ));
    }
    for button in &notification.actions {
      xml.push_str(&format!(
        r#"<action content="{}" arguments="{}" activationType="foreground"/>"#,
        escape(&button.label),
        button.action.key()
      ));
    }
    xml.push_str("</actions>");
  }
  if notification.is_quiet() {
    xml.push_str(r#"<audio silent="true"/>"#);
  }
  xml.push_str("</toast>");
  xml
}

/// Text and attribute values: names and message text are the user's, never markup.
fn escape(text: &str) -> String {
  let mut escaped = String::with_capacity(text.len());
  for character in text.chars() {
    match character {
      '&' => escaped.push_str("&amp;"),
      '<' => escaped.push_str("&lt;"),
      '>' => escaped.push_str("&gt;"),
      '"' => escaped.push_str("&quot;"),
      '\'' => escaped.push_str("&apos;"),
      // Control characters other than tab and newlines are not allowed in XML 1.0.
      control if control.is_control() && !matches!(control, '\t' | '\n' | '\r') => {},
      other => escaped.push(other),
    }
  }
  escaped
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::os_notify::{
    NotificationButton, NotificationSound, NotificationTag, OsNotificationKind, ReplyField,
  };

  use super::*;

  #[test]
  fn a_message_has_its_texts_buttons_and_reply_field() {
    let xml = toast_xml(
      &OsNotification {
        tag: NotificationTag::conversation(7),
        kind: OsNotificationKind::Message,
        title: "Bob".into(),
        maybe_body: Some("lunch?".into()),
        actions: vec![NotificationButton {
          action: NotificationAction::MarkRead,
          label: "Mark as read".into(),
        }],
        maybe_reply: Some(ReplyField {
          placeholder: "Reply…".into(),
          send_label: "Send".into(),
        }),
        maybe_picture: None,
        sound: NotificationSound::System,
      },
      None,
    );
    assert_eq!(
      xml,
      concat!(
        r#"<toast launch="default" activationType="foreground">"#,
        r#"<visual><binding template="ToastGeneric"><text>Bob</text><text>lunch?</text></binding></visual>"#,
        r#"<actions><input id="reply" type="text" placeHolderContent="Reply…"/>"#,
        r#"<action content="Send" arguments="inline-reply" hint-inputId="reply" activationType="foreground"/>"#,
        r#"<action content="Mark as read" arguments="mark-read" activationType="foreground"/>"#,
        r#"</actions></toast>"#
      )
    );
  }

  #[test]
  fn a_ringing_call_uses_the_call_scenario_without_sound() {
    let xml = toast_xml(
      &OsNotification {
        tag: NotificationTag::call(7),
        kind: OsNotificationKind::IncomingCall,
        title: "Incoming call".into(),
        maybe_body: None,
        actions: vec![NotificationButton {
          action: NotificationAction::Decline,
          label: "Decline".into(),
        }],
        maybe_reply: None,
        maybe_picture: None,
        sound: NotificationSound::System,
      },
      None,
    );
    assert!(
      xml.starts_with(r#"<toast launch="default" activationType="foreground" scenario="incomingCall">"#)
    );
    assert!(xml.ends_with(r#"<audio silent="true"/></toast>"#));
    assert!(!xml.contains("<input"));
  }

  #[test]
  fn user_text_cannot_inject_markup() {
    let xml = toast_xml(
      &OsNotification {
        tag: NotificationTag::conversation(7),
        kind: OsNotificationKind::Message,
        title: r#"<b>"Eve" & co</b>"#.into(),
        maybe_body: Some("a\u{1}b</text><action/>".into()),
        actions: Vec::new(),
        maybe_reply: None,
        maybe_picture: None,
        sound: NotificationSound::System,
      },
      None,
    );
    assert!(xml.contains("<text>&lt;b&gt;&quot;Eve&quot; &amp; co&lt;/b&gt;</text>"));
    assert!(xml.contains("<text>ab&lt;/text&gt;&lt;action/&gt;</text>"));
    assert!(!xml.contains("<actions>"));
  }

  #[test]
  fn a_message_whose_sound_the_app_plays_is_silent() {
    let xml = toast_xml(
      &OsNotification {
        tag: NotificationTag::conversation(7),
        kind: OsNotificationKind::Message,
        title: "Bob".into(),
        maybe_body: None,
        actions: Vec::new(),
        maybe_reply: None,
        maybe_picture: None,
        sound: NotificationSound::Silent,
      },
      None,
    );
    assert!(xml.ends_with(r#"<audio silent="true"/></toast>"#));
    assert!(!xml.contains("scenario"));
  }

  #[test]
  fn the_picture_replaces_the_logo_round_for_people() {
    let notification = OsNotification {
      tag: NotificationTag::conversation(7),
      kind: OsNotificationKind::Message,
      title: "Bob".into(),
      maybe_body: Some("lunch?".into()),
      actions: Vec::new(),
      maybe_reply: None,
      maybe_picture: None,
      sound: NotificationSound::System,
    };
    let person = ToastLogo {
      uri: "file:///C:/Users/a/AppData/Local/teamcraft/notification-pictures/1-0.png".into(),
      is_circle: true,
    };
    assert_eq!(
      toast_xml(&notification, Some(&person)),
      concat!(
        r#"<toast launch="default" activationType="foreground">"#,
        r#"<visual><binding template="ToastGeneric"><text>Bob</text><text>lunch?</text>"#,
        r#"<image placement="appLogoOverride" "#,
        r#"src="file:///C:/Users/a/AppData/Local/teamcraft/notification-pictures/1-0.png" hint-crop="circle"/>"#,
        r#"</binding></visual></toast>"#
      )
    );
    let place = ToastLogo {
      uri: r#"file:///C:/x/"quoted"&.png"#.into(),
      is_circle: false,
    };
    let xml = toast_xml(&notification, Some(&place));
    assert!(
      xml.contains(r#"<image placement="appLogoOverride" src="file:///C:/x/&quot;quoted&quot;&amp;.png"/>"#)
    );
  }
}
