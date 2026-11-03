//! The links the operating system hands to an installed app (docs/app-links.md): permalinks, invite
//! and meeting links, as `https://` (Android App Links, iOS Universal Links) or as the
//! `teamcraft://open/<host>/<path>` fallback that any instance can offer.
//!
//! This is outside input: any web page or app can fire such a link. So the parser is an allow-list
//! of the places a link may name, over a character set no real link leaves. Anything else is
//! refused rather than repaired, because the `url` crate would quietly resolve `/c/1/../2` or
//! `%2e%2e` to somewhere else. A link only ever names a place: what happens there still takes a
//! click, and [`next_step`] says when the user has to be asked first.
//!
//! Pure functions, shared by the desktop app's UI, the web client's "Open in the app" button and,
//! through `teamcraft-ffi`, the mobile apps, so the platforms can't disagree about a link.

use std::fmt;

use teamcraft_crypto::meeting_link::is_meeting_link_token;
use teamcraft_proto::{ConversationId, MeetingId, MessageId, SpaceId, ThreadId};

use crate::server_entry::{HttpPolicy, ServerOrigin};

/// The custom scheme the apps register. Not exclusive on mobile (another app may claim it too), so
/// links in this scheme carry no secret: see [`AppLink::scheme_link`].
pub const APP_LINK_SCHEME: &str = "teamcraft";
/// `teamcraft://open/<host>/<path>`: names the instance. Other authorities are not this form:
/// `teamcraft://c/…` is a permalink of the current server, `teamcraft://link?…` a device link.
const OPEN_AUTHORITY: &str = "open";
/// Longer links are not looked at. The longest real one is well under this.
pub const MAX_APP_LINK_BYTES: usize = 2048;
/// The longest share ID a download page accepts.
const MAX_SHARE_ID_LEN: usize = 128;
/// `/meeting/<token>`: the path of a meeting link.
const MEETING_SEGMENT: &str = "meeting";
/// `/f/<id>`: the path of a share link.
const SHARE_SEGMENT: &str = "f";
/// A one-time guest token in a meeting link's fragment (docs/meetings.md, "Links").
const INVITE_FRAGMENT_KEY: &str = "invite=";
/// The organizer key's fingerprint in a meeting link's fragment, and its name in older links.
const VERIFIER_FRAGMENT_KEYS: [&str; 2] = ["v=", "o="];

/// A link that was understood. Holds no raw input; `Debug` never shows a fragment's secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppLink {
  /// The instance the link names; `None` for a permalink of whichever server is active.
  maybe_origin: Option<ServerOrigin>,
  target: AppLinkTarget,
}

/// Where a link leads. The allow-list: what is not here (settings, administration, the API) can't
/// be reached by a link from outside.
#[derive(Clone, PartialEq, Eq)]
pub enum AppLinkTarget {
  /// `/`: the app's start.
  Start,
  /// `/home`: Home, what an invitation mail links to.
  Home,
  /// `/s/{space}`.
  Space(SpaceId),
  /// `/s/{space}/c/{channel}`.
  Channel { space: SpaceId, channel: ConversationId },
  /// `/c/{conversation}`.
  Conversation(ConversationId),
  /// `/dm/{conversation}`.
  Direct(ConversationId),
  /// `/c/{conversation}/t/{thread}`.
  Thread {
    conversation: ConversationId,
    thread: ThreadId,
  },
  /// `/c/{conversation}/m/{message}[/t/{thread}]`.
  Message {
    conversation: ConversationId,
    message: MessageId,
    maybe_thread: Option<ThreadId>,
  },
  /// `/meeting/{id or token}`: only lets the holder knock (docs/meetings.md, "Links").
  Meeting {
    handle: String,
    maybe_verifier: Option<String>,
  },
  /// A link only the web client serves: a share link (the file's key is in the fragment) or a
  /// personal one-time guest link. Holds the fragment, so it is kept out of `Debug` and never
  /// becomes a [`AppLink::scheme_link`].
  WebOnly { path: String, fragment: String },
}

/// What a client does with a link that was understood.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppLinkStep {
  /// It belongs to the server in use (or names none): show the place, once signed in.
  Navigate,
  /// It names another instance: ask, naming the host. Never opened silently.
  AskForeignServer {
    host: String,
    /// There is no account yet and the device isn't locked to a server, so signing in there is
    /// possible. With an account the apps offer the browser only: they hold one account at a time.
    can_add_account: bool,
  },
  /// Only the web client can open it: ask before leaving for the browser, naming the host.
  AskWebOnly { host: String },
}

/// Why a link isn't one. Carries nothing of the input, so it is safe to log.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AppLinkError {
  #[error("the link is empty")]
  Empty,
  #[error("the link is too long")]
  TooLong,
  #[error("the link contains characters no link has")]
  Malformed,
  #[error("the link's scheme is not one the app opens")]
  UnsupportedScheme,
  #[error("the link names no usable server")]
  InvalidHost,
  #[error("the link does not lead to a place in the app")]
  UnknownPlace,
}

impl AppLink {
  pub fn maybe_origin(&self) -> Option<&ServerOrigin> {
    self.maybe_origin.as_ref()
  }

  pub fn target(&self) -> &AppLinkTarget {
    &self.target
  }

  /// The path of the place: for the web client's `Route::parse`, which routes it like any URL.
  pub fn path(&self) -> String {
    self.target.path()
  }

  /// The link on the instance's web client (with its fragment), for "Open in browser". `None`
  /// without an origin.
  pub fn web_url(&self) -> Option<String> {
    let origin = self.maybe_origin.as_ref()?;
    Some(format!(
      "{}{}{}",
      origin.as_str(),
      self.path(),
      self.target.fragment()
    ))
  }

  /// The form the "Open in the app" button uses: `teamcraft://open/<host>/<path>`. `None` for a
  /// link without an origin and for a [`AppLinkTarget::WebOnly`]: on Android and iOS any app can
  /// register this scheme, so a share key or a one-time token never goes through it.
  pub fn scheme_link(&self) -> Option<String> {
    let origin = self.maybe_origin.as_ref()?;
    if matches!(self.target, AppLinkTarget::WebOnly { .. }) {
      return None;
    }
    Some(format!(
      "{APP_LINK_SCHEME}://{OPEN_AUTHORITY}/{}{}{}",
      origin.host(),
      self.path(),
      self.target.fragment()
    ))
  }
}

impl AppLinkTarget {
  fn path(&self) -> String {
    match self {
      Self::Start => "/".to_owned(),
      Self::Home => "/home".to_owned(),
      Self::Space(space) => format!("/s/{space}"),
      Self::Channel { space, channel } => format!("/s/{space}/c/{channel}"),
      Self::Conversation(conversation) => format!("/c/{conversation}"),
      Self::Direct(conversation) => format!("/dm/{conversation}"),
      Self::Thread { conversation, thread } => format!("/c/{conversation}/t/{thread}"),
      Self::Message {
        conversation,
        message,
        maybe_thread: None,
      } => format!("/c/{conversation}/m/{message}"),
      Self::Message {
        conversation,
        message,
        maybe_thread: Some(thread),
      } => format!("/c/{conversation}/m/{message}/t/{thread}"),
      Self::Meeting { handle, .. } => format!("/{MEETING_SEGMENT}/{handle}"),
      Self::WebOnly { path, .. } => path.clone(),
    }
  }

  fn fragment(&self) -> String {
    match self {
      Self::Meeting {
        maybe_verifier: Some(verifier),
        ..
      } => format!("#{}{verifier}", VERIFIER_FRAGMENT_KEYS[0]),
      Self::WebOnly { fragment, .. } if !fragment.is_empty() => format!("#{fragment}"),
      _ => String::new(),
    }
  }
}

impl fmt::Debug for AppLinkTarget {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      // Share keys and one-time tokens never reach a log (security.md).
      Self::WebOnly { .. } => f.write_str("WebOnly([redacted])"),
      other => f.write_str(&other.path()),
    }
  }
}

/// Reads a link. See the module docs for what it refuses and why.
pub fn parse_app_link(text: &str) -> Result<AppLink, AppLinkError> {
  let text = text.trim_ascii();
  if text.is_empty() {
    return Err(AppLinkError::Empty);
  }
  if text.len() > MAX_APP_LINK_BYTES {
    return Err(AppLinkError::TooLong);
  }
  if !text.bytes().all(is_link_byte) {
    return Err(AppLinkError::Malformed);
  }
  let (without_fragment, fragment) = text.split_once('#').unwrap_or((text, ""));
  // Nothing the apps link to has a query; whatever a link carries there is dropped, never passed on.
  let (base, _query) = without_fragment.split_once('?').unwrap_or((without_fragment, ""));
  let (scheme, rest) = base.split_once(':').ok_or(AppLinkError::UnsupportedScheme)?;
  let scheme = scheme.to_ascii_lowercase();
  match scheme.as_str() {
    "https" | "http" => {
      let rest = rest.strip_prefix("//").ok_or(AppLinkError::Malformed)?;
      parse_on_instance(&scheme, rest, fragment)
    },
    APP_LINK_SCHEME => match rest.strip_prefix("//").and_then(strip_open) {
      Some(on_instance) => parse_on_instance(scheme_of_authority(on_instance), on_instance, fragment),
      None => parse_permalink(rest),
    },
    _ => Err(AppLinkError::UnsupportedScheme),
  }
}

/// What to do with `link` given the server in use and whether the device is locked to one.
pub fn next_step(link: &AppLink, maybe_active: Option<&ServerOrigin>, is_server_locked: bool) -> AppLinkStep {
  let Some(origin) = &link.maybe_origin else {
    return AppLinkStep::Navigate;
  };
  let host = origin.host().to_owned();
  if matches!(link.target, AppLinkTarget::WebOnly { .. }) {
    return AppLinkStep::AskWebOnly { host };
  }
  if maybe_active == Some(origin) {
    return AppLinkStep::Navigate;
  }
  AppLinkStep::AskForeignServer {
    host,
    can_add_account: maybe_active.is_none() && !is_server_locked,
  }
}

/// The bytes a real link is made of. No `%` (nothing real is escaped, and it is how a traversal
/// hides), `@` (`https://good.example@evil.example/`), `\` (which URL parsers read as `/`), spaces,
/// control characters or anything beyond ASCII (a look-alike host; hosts come as punycode).
fn is_link_byte(byte: u8) -> bool {
  byte.is_ascii_alphanumeric()
    || matches!(
      byte,
      b':' | b'/' | b'.' | b'-' | b'_' | b'~' | b'#' | b'?' | b'=' | b'&' | b'[' | b']'
    )
}

fn strip_open(rest: &str) -> Option<&str> {
  rest.strip_prefix(OPEN_AUTHORITY)?.strip_prefix('/')
}

/// The scheme of the scheme form's instance: https, except for a server on this device, which
/// [`ServerOrigin`] only accepts as http.
fn scheme_of_authority(on_instance: &str) -> &'static str {
  let authority = on_instance.split('/').next().unwrap_or(on_instance);
  match ServerOrigin::parse(&format!("http://{authority}"), HttpPolicy::LoopbackOnly) {
    Ok(_) => "http",
    Err(_) => "https",
  }
}

/// `<authority>/<path>` of a link that names its instance.
fn parse_on_instance(scheme: &str, rest: &str, fragment: &str) -> Result<AppLink, AppLinkError> {
  let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
  if authority.is_empty() {
    return Err(AppLinkError::InvalidHost);
  }
  let origin = ServerOrigin::parse(&format!("{scheme}://{authority}"), HttpPolicy::LoopbackOnly)
    .map_err(|_| AppLinkError::InvalidHost)?;
  Ok(AppLink {
    maybe_origin: Some(origin),
    target: parse_place(path, fragment)?,
  })
}

/// `teamcraft:/c/{c}/m/{m}[/t/{t}]` (also with `//`), the format of a message link inside messages:
/// names no instance, and nothing but a message.
fn parse_permalink(rest: &str) -> Result<AppLink, AppLinkError> {
  let path = rest.strip_prefix("//").or_else(|| rest.strip_prefix('/'));
  let target = parse_place(path.unwrap_or(rest), "")?;
  if !matches!(target, AppLinkTarget::Message { .. }) {
    return Err(AppLinkError::UnknownPlace);
  }
  Ok(AppLink {
    maybe_origin: None,
    target,
  })
}

/// The place of a path without its leading slash.
fn parse_place(path: &str, fragment: &str) -> Result<AppLinkTarget, AppLinkError> {
  let path = path.strip_suffix('/').unwrap_or(path);
  if path.is_empty() {
    return Ok(AppLinkTarget::Start);
  }
  let segments: Vec<&str> = path.split('/').collect();
  let target = match segments.as_slice() {
    ["home"] => AppLinkTarget::Home,
    ["s", space] => AppLinkTarget::Space(id(space)?),
    ["s", space, "c", channel] => AppLinkTarget::Channel {
      space: id(space)?,
      channel: id(channel)?,
    },
    ["c", conversation] => AppLinkTarget::Conversation(id(conversation)?),
    ["dm", conversation] => AppLinkTarget::Direct(id(conversation)?),
    ["c", conversation, "t", thread] => AppLinkTarget::Thread {
      conversation: id(conversation)?,
      thread: id(thread)?,
    },
    ["c", conversation, "m", message] => AppLinkTarget::Message {
      conversation: id(conversation)?,
      message: id(message)?,
      maybe_thread: None,
    },
    ["c", conversation, "m", message, "t", thread] => AppLinkTarget::Message {
      conversation: id(conversation)?,
      message: id(message)?,
      maybe_thread: Some(id(thread)?),
    },
    [MEETING_SEGMENT, handle] => parse_meeting(handle, fragment)?,
    [SHARE_SEGMENT, share] if is_share_id(share) => AppLinkTarget::WebOnly {
      path: format!("/{SHARE_SEGMENT}/{share}"),
      fragment: fragment.to_owned(),
    },
    _ => return Err(AppLinkError::UnknownPlace),
  };
  Ok(target)
}

fn parse_meeting(handle: &str, fragment: &str) -> Result<AppLinkTarget, AppLinkError> {
  if id::<MeetingId>(handle).is_err() && !is_meeting_link_token(handle) {
    return Err(AppLinkError::UnknownPlace);
  }
  if fragment
    .split('&')
    .any(|part| part.starts_with(INVITE_FRAGMENT_KEY))
  {
    // A personal one-time link is a guest's (docs/meetings.md), and the app is a member's.
    return Ok(AppLinkTarget::WebOnly {
      path: format!("/{MEETING_SEGMENT}/{handle}"),
      fragment: fragment.to_owned(),
    });
  }
  let maybe_verifier = VERIFIER_FRAGMENT_KEYS.iter().find_map(|key| {
    fragment
      .split('&')
      .find_map(|part| part.strip_prefix(key))
      .filter(|value| !value.is_empty())
  });
  Ok(AppLinkTarget::Meeting {
    handle: handle.to_owned(),
    maybe_verifier: maybe_verifier.map(str::to_owned),
  })
}

/// A canonical ID: digits only, no sign or leading zero, so every ID has exactly one spelling.
fn id<T: std::str::FromStr + fmt::Display>(text: &str) -> Result<T, AppLinkError> {
  if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
    return Err(AppLinkError::UnknownPlace);
  }
  let id: T = text.parse().map_err(|_| AppLinkError::UnknownPlace)?;
  if id.to_string() == text {
    Ok(id)
  } else {
    Err(AppLinkError::UnknownPlace)
  }
}

fn is_share_id(text: &str) -> bool {
  !text.is_empty()
    && text.len() <= MAX_SHARE_ID_LEN
    && text
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[cfg(test)]
mod tests {
  use super::*;

  const TOKEN: &str = "AbCdEfGhIjKlMnOpQrStUvWxYz012345";

  mod places {
    use super::*;

    #[test]
    fn every_place_reads_from_https_and_the_scheme_alike() {
      for path in [
        "/",
        "/home",
        "/s/1",
        "/s/1/c/2",
        "/c/2",
        "/dm/2",
        "/c/2/t/4",
        "/c/2/m/3",
        "/c/2/m/3/t/4",
        "/meeting/5",
      ] {
        let https = parse_app_link(&format!("https://chat.example.com{path}")).unwrap();
        let scheme = parse_app_link(&format!("teamcraft://open/chat.example.com{path}")).unwrap();
        assert_eq!(https, scheme, "{path}");
        assert_eq!(https.path(), path);
        assert_eq!(https.maybe_origin().unwrap().as_str(), "https://chat.example.com");
      }
    }

    #[test]
    fn a_trailing_slash_a_query_and_case_change_nothing() {
      let plain = parse_app_link("https://chat.example.com/c/2").unwrap();
      assert_eq!(parse_app_link("https://chat.example.com/c/2/").unwrap(), plain);
      assert_eq!(parse_app_link("https://chat.example.com/c/2?a=b").unwrap(), plain);
      assert_eq!(parse_app_link("  https://chat.example.com/c/2\n").unwrap(), plain);
      assert_eq!(parse_app_link("HTTPS://CHAT.example.com/c/2").unwrap(), plain);
      assert_eq!(parse_app_link("https://chat.example.com").unwrap().path(), "/");
    }

    #[test]
    fn places_a_link_may_not_name_are_refused() {
      for path in [
        "/settings",
        "/settings/security",
        "/admin",
        "/admin/users",
        "/api/v1/brand",
        "/.well-known/assetlinks.json",
        "/search",
        "/unknown",
        "/s",
        "/s/1/c",
        "/c/1/m",
        "/c/1/m/2/t/3/x",
        "/dm/1/2",
        "/meeting",
        "/meeting/not-a-token",
        "/f",
      ] {
        assert_eq!(
          parse_app_link(&format!("https://chat.example.com{path}")),
          Err(AppLinkError::UnknownPlace),
          "{path}"
        );
      }
    }

    #[test]
    fn ids_must_be_canonical() {
      for id in ["+5", "-5", "007", "5a", "1e3", "99999999999999999999999"] {
        assert!(
          parse_app_link(&format!("https://chat.example.com/dm/{id}")).is_err(),
          "{id}"
        );
      }
    }

    #[test]
    fn a_permalink_names_a_message_of_the_active_server() {
      for link in [
        "teamcraft:/c/2/m/3",
        "teamcraft://c/2/m/3",
        " teamcraft://c/2/m/3 ",
      ] {
        let parsed = parse_app_link(link).unwrap();
        assert_eq!(parsed.maybe_origin(), None, "{link}");
        assert_eq!(parsed.path(), "/c/2/m/3");
      }
      for link in [
        "teamcraft:/c/2",
        "teamcraft:/settings/security",
        "teamcraft://link?v=1&rv=abc&ak=def",
        "teamcraft:",
      ] {
        assert!(parse_app_link(link).is_err(), "{link}");
      }
    }
  }

  mod hostile {
    use super::*;

    #[test]
    fn path_traversal_is_refused_not_resolved() {
      for link in [
        "https://chat.example.com/c/1/../2",
        "https://chat.example.com/c/1/./2",
        "https://chat.example.com/../admin/users",
        "https://chat.example.com/c/1/m/../../../admin",
        "https://chat.example.com/%2e%2e/admin",
        "https://chat.example.com/c/1%2f..%2f2",
        "https://chat.example.com/c/1\\..\\2",
        "https://chat.example.com//c/1",
        "https://chat.example.com/c//1",
        "teamcraft://open/chat.example.com/../../etc/passwd",
        "teamcraft://open/chat.example.com/%2e%2e/admin",
        "teamcraft:/../admin",
      ] {
        assert!(parse_app_link(link).is_err(), "{link}");
      }
    }

    #[test]
    fn other_schemes_are_refused() {
      for link in [
        "javascript:alert(1)",
        "file:///etc/passwd",
        "content://provider/item",
        "intent://scan#Intent;end",
        "data:text/html,x",
        "ftp://chat.example.com/c/1",
        "tel:+491234",
        "httpx://chat.example.com/c/1",
        "teamcraftx://open/chat.example.com/c/1",
        "chat.example.com/c/1",
        "/c/1",
        "c",
      ] {
        assert!(parse_app_link(link).is_err(), "{link}");
      }
    }

    #[test]
    fn hosts_that_hide_or_mislead_are_refused() {
      for link in [
        "https://good.example.com@evil.example.org/c/1",
        "https://user:pw@chat.example.com/c/1",
        "https:///c/1",
        "https://",
        "https:/chat.example.com/c/1",
        "https:chat.example.com/c/1",
        "teamcraft://open//c/1",
        "teamcraft://open/user@chat.example.com/c/1",
        "https://ch\u{0430}t.example.com/c/1",
        "https://chat.example.com:notaport/c/1",
        "https://chat.example.com:99999/c/1",
      ] {
        assert!(parse_app_link(link).is_err(), "{link}");
      }
    }

    #[test]
    fn plain_http_only_reaches_this_device() {
      assert_eq!(
        parse_app_link("http://chat.example.com/c/1"),
        Err(AppLinkError::InvalidHost)
      );
      let local = parse_app_link("http://localhost:8080/c/1").unwrap();
      assert_eq!(local.maybe_origin().unwrap().as_str(), "http://localhost:8080");
      assert_eq!(
        parse_app_link("teamcraft://open/localhost:8080/c/1").unwrap(),
        local
      );
    }

    #[test]
    fn control_characters_and_oversized_links_are_refused() {
      for link in [
        "https://chat.example.com/c/1\u{0}",
        "https://chat.example.com/c/\n1",
        "https://chat.example.com/c/\t1",
        "https://chat.example.com/c/ 1",
        "https://chat.example.com/c/1\"><script>",
      ] {
        assert_eq!(parse_app_link(link), Err(AppLinkError::Malformed), "{link:?}");
      }
      let long = format!("https://chat.example.com/c/1#{}", "a".repeat(MAX_APP_LINK_BYTES));
      assert_eq!(parse_app_link(&long), Err(AppLinkError::TooLong));
      assert_eq!(parse_app_link(""), Err(AppLinkError::Empty));
      assert_eq!(parse_app_link("   "), Err(AppLinkError::Empty));
      assert_eq!(
        parse_app_link(&format!("https://chat.example.com/{}", "a/".repeat(500))),
        Err(AppLinkError::UnknownPlace)
      );
    }
  }

  mod fragments {
    use super::*;

    #[test]
    fn a_meeting_link_keeps_its_verifier() {
      let link = parse_app_link(&format!("https://chat.example.com/meeting/{TOKEN}#v=ab12")).unwrap();
      assert_eq!(link.path(), format!("/meeting/{TOKEN}"));
      assert_eq!(
        link.web_url().unwrap(),
        format!("https://chat.example.com/meeting/{TOKEN}#v=ab12")
      );
      assert_eq!(
        link.scheme_link().unwrap(),
        format!("teamcraft://open/chat.example.com/meeting/{TOKEN}#v=ab12")
      );
      assert_eq!(parse_app_link(&link.scheme_link().unwrap()).unwrap(), link);
      // An older link's name for it, and the secret `k` older apps sent, which goes nowhere.
      let older = parse_app_link(&format!(
        "https://chat.example.com/meeting/{TOKEN}#k=s3cret&o=ab12"
      ))
      .unwrap();
      assert_eq!(older, link);
    }

    #[test]
    fn share_and_one_time_links_stay_on_the_web_and_out_of_debug() {
      for link in [
        "https://chat.example.com/f/abc-1#key.s3cret",
        "https://chat.example.com/meeting/123#invite=abc.s3cret&v=ab12",
      ] {
        let parsed = parse_app_link(link).unwrap();
        assert!(matches!(parsed.target(), AppLinkTarget::WebOnly { .. }), "{link}");
        assert_eq!(parsed.web_url().as_deref(), Some(link));
        // The custom scheme can be claimed by another app: nothing secret goes through it.
        assert_eq!(parsed.scheme_link(), None, "{link}");
        assert!(!format!("{parsed:?}").contains("s3cret"), "{link}");
      }
    }

    #[test]
    fn other_places_drop_their_fragment() {
      let link = parse_app_link("https://chat.example.com/c/1#anything").unwrap();
      assert_eq!(link.web_url().unwrap(), "https://chat.example.com/c/1");
    }
  }

  mod steps {
    use super::*;

    #[test]
    fn a_link_of_the_active_server_navigates() {
      let active = origin("https://chat.example.com");
      for link in ["https://chat.example.com/c/1", "https://CHAT.example.com:443/c/1"] {
        assert_eq!(
          next_step(&parse_app_link(link).unwrap(), Some(&active), false),
          AppLinkStep::Navigate,
          "{link}"
        );
      }
      assert_eq!(
        next_step(&parse_app_link("teamcraft:/c/1/m/2").unwrap(), None, false),
        AppLinkStep::Navigate
      );
    }

    #[test]
    fn another_instance_is_never_opened_silently() {
      let link = parse_app_link("https://other.example.org:8443/c/1").unwrap();
      let host = "other.example.org:8443".to_owned();
      assert_eq!(
        next_step(&link, None, false),
        AppLinkStep::AskForeignServer {
          host: host.clone(),
          can_add_account: true
        }
      );
      // The app holds one account at a time, and a managed device stays on its server.
      assert_eq!(
        next_step(&link, Some(&origin("https://chat.example.com")), false),
        AppLinkStep::AskForeignServer {
          host: host.clone(),
          can_add_account: false
        }
      );
      assert_eq!(
        next_step(&link, None, true),
        AppLinkStep::AskForeignServer {
          host,
          can_add_account: false
        }
      );
    }

    #[test]
    fn a_look_alike_of_the_active_host_is_foreign() {
      let active = origin("https://chat.example.com");
      for link in [
        "https://chat.example.com.example.org/c/1",
        "https://chat-example.com/c/1",
        "https://chat.example.com:8443/c/1",
      ] {
        assert!(
          matches!(
            next_step(&parse_app_link(link).unwrap(), Some(&active), false),
            AppLinkStep::AskForeignServer { .. }
          ),
          "{link}"
        );
      }
    }

    #[test]
    fn web_only_links_ask_before_the_browser_even_on_the_active_server() {
      let active = origin("https://chat.example.com");
      let link = parse_app_link("https://chat.example.com/f/abc#key").unwrap();
      assert_eq!(
        next_step(&link, Some(&active), false),
        AppLinkStep::AskWebOnly {
          host: "chat.example.com".to_owned()
        }
      );
    }

    fn origin(text: &str) -> ServerOrigin {
      ServerOrigin::parse(text, HttpPolicy::LoopbackOnly).unwrap()
    }
  }
}
