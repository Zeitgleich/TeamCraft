//! Links the system hands to the mobile apps (docs/app-links.md): the parser and the decision of
//! `teamcraft-client-api::app_link`, so Android and iOS treat a link exactly like the desktop app.

use std::fmt;

use teamcraft_client_api::app_link::{self, AppLink, AppLinkStep, AppLinkTarget};
use teamcraft_client_api::server_entry::{HttpPolicy, ServerOrigin};

/// What to do with a link right now, and where it leads once it is the user's turn.
#[derive(PartialEq, Eq, uniffi::Record)]
pub struct OpenedAppLink {
  pub step: OpenedAppLinkStep,
  pub place: AppLinkPlace,
  /// The link on the instance's web client, for "Open in browser". Can hold a share key or a
  /// one-time token in its fragment: it goes to the browser and nowhere else, never to a log.
  pub maybe_web_url: Option<String>,
}

impl fmt::Debug for OpenedAppLink {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("OpenedAppLink")
      .field("step", &self.step)
      .field("place", &self.place)
      .field(
        "maybe_web_url",
        &self.maybe_web_url.as_ref().map(|_| "[redacted]"),
      )
      .finish()
  }
}

/// See `AppLinkStep`. Shows `registrable_domain` most prominently, and `host` whole.
#[derive(Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OpenedAppLinkStep {
  /// Show the place, once signed in.
  Navigate,
  /// Another instance: ask, naming `host`. Offer signing in there only with `can_add_account`.
  AskForeignServer {
    host: String,
    registrable_domain: String,
    can_add_account: bool,
  },
  /// Only the web client can open it: ask before the browser, naming `host`.
  AskWebOnly {
    host: String,
    registrable_domain: String,
  },
}

/// The places a mobile app has.
#[derive(Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppLinkPlace {
  /// The list of conversations.
  Home,
  Conversation {
    conversation_id: String,
  },
  /// A message or a thread, as the message link `resolve_permalink` reads.
  Permalink {
    link: String,
  },
  /// A meeting. The apps have no lobby: they show the user's meetings and offer the browser.
  Meeting,
  /// Nothing in the app: [`AppLinkStep::AskWebOnly`] goes with it.
  None,
}

/// Reads a link the system handed over; `None` when it isn't one. `active_origin` is the server in
/// use. Never logs the link.
#[uniffi::export]
pub fn open_app_link(
  link: String,
  maybe_active_origin: Option<String>,
  is_server_locked: bool,
) -> Option<OpenedAppLink> {
  let parsed = app_link::parse_app_link(&link).ok()?;
  let maybe_active =
    maybe_active_origin.and_then(|origin| ServerOrigin::parse(&origin, HttpPolicy::Allowed).ok());
  let step = match app_link::next_step(&parsed, maybe_active.as_ref(), is_server_locked) {
    AppLinkStep::Navigate => OpenedAppLinkStep::Navigate,
    AppLinkStep::AskForeignServer {
      host,
      registrable_domain,
      can_add_account,
    } => OpenedAppLinkStep::AskForeignServer {
      host,
      registrable_domain,
      can_add_account,
    },
    AppLinkStep::AskWebOnly {
      host,
      registrable_domain,
    } => OpenedAppLinkStep::AskWebOnly {
      host,
      registrable_domain,
    },
  };
  Some(OpenedAppLink {
    step,
    place: place_of(&parsed),
    maybe_web_url: parsed.web_url(),
  })
}

fn place_of(link: &AppLink) -> AppLinkPlace {
  match link.target() {
    AppLinkTarget::Start | AppLinkTarget::Home | AppLinkTarget::Space(_) => AppLinkPlace::Home,
    AppLinkTarget::Channel { channel, .. } => AppLinkPlace::Conversation {
      conversation_id: channel.to_string(),
    },
    AppLinkTarget::Conversation(conversation) | AppLinkTarget::Direct(conversation) => {
      AppLinkPlace::Conversation {
        conversation_id: conversation.to_string(),
      }
    },
    // A thread's ID is its root message's: the same link as for that message, in its thread.
    AppLinkTarget::Thread { conversation, thread } => AppLinkPlace::Permalink {
      link: format!("teamcraft:/c/{conversation}/m/{thread}/t/{thread}"),
    },
    AppLinkTarget::Message { .. } => AppLinkPlace::Permalink {
      link: format!("teamcraft:{}", link.path()),
    },
    AppLinkTarget::Meeting { .. } => AppLinkPlace::Meeting,
    AppLinkTarget::WebOnly { .. } => AppLinkPlace::None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const ACTIVE: &str = "https://chat.example.com";

  #[test]
  fn places_become_what_the_apps_open() {
    for (link, place) in [
      ("https://chat.example.com/home", AppLinkPlace::Home),
      (
        "https://chat.example.com/s/1/c/2",
        AppLinkPlace::Conversation {
          conversation_id: "2".to_owned(),
        },
      ),
      (
        "https://chat.example.com/dm/2",
        AppLinkPlace::Conversation {
          conversation_id: "2".to_owned(),
        },
      ),
      (
        "teamcraft://open/chat.example.com/c/2/m/3/t/4",
        AppLinkPlace::Permalink {
          link: "teamcraft:/c/2/m/3/t/4".to_owned(),
        },
      ),
      (
        "https://chat.example.com/c/2/t/4",
        AppLinkPlace::Permalink {
          link: "teamcraft:/c/2/m/4/t/4".to_owned(),
        },
      ),
      ("https://chat.example.com/meeting/5#v=ab", AppLinkPlace::Meeting),
    ] {
      let opened = open_app_link(link.to_owned(), Some(ACTIVE.to_owned()), false).unwrap();
      assert_eq!(opened.step, OpenedAppLinkStep::Navigate, "{link}");
      assert_eq!(opened.place, place, "{link}");
    }
  }

  #[test]
  fn another_instance_asks_and_a_broken_link_is_nothing() {
    let opened = open_app_link("https://other.example.org/c/2".to_owned(), None, false).unwrap();
    assert_eq!(
      opened.step,
      OpenedAppLinkStep::AskForeignServer {
        host: "other.example.org".to_owned(),
        registrable_domain: "example.org".to_owned(),
        can_add_account: true
      }
    );
    assert_eq!(
      opened.maybe_web_url.as_deref(),
      Some("https://other.example.org/c/2")
    );
    for link in ["", "javascript:alert(1)", "https://chat.example.com/../admin"] {
      assert_eq!(open_app_link(link.to_owned(), None, false), None, "{link}");
    }
  }

  #[test]
  fn a_share_link_stays_on_the_web() {
    let opened = open_app_link(
      "https://chat.example.com/f/abc#key".to_owned(),
      Some(ACTIVE.to_owned()),
      false,
    )
    .unwrap();
    assert_eq!(
      opened.step,
      OpenedAppLinkStep::AskWebOnly {
        host: "chat.example.com".to_owned(),
        registrable_domain: "example.com".to_owned(),
      }
    );
    assert_eq!(opened.place, AppLinkPlace::None);
    assert!(!format!("{opened:?}").contains("key"));
  }
}
