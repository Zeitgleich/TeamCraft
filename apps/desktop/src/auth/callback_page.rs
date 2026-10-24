//! The page the browser shows after the identity provider sent it back to the loopback listener:
//! one language (the app's, else the browser's), the app's theme, the server's brand when the UI
//! passed it along. Self-contained HTML under a strict content security policy: no scripts, no
//! external requests, images only as checked `data:` URLs. Nothing from the redirect (code, state,
//! error text) is ever written into it.
//!
//! The strings come from the UI's Fluent files (`callback-*`, plain text without placeables), so
//! translators keep one place.

use base64::Engine;
use teamcraft_client_api::sign_in_look::{LookTheme, SignInLook};

/// The page's content security policy: inline styles and `data:` images, nothing else.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'; img-src data:; \
                                           base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

const EN_SOURCE: &str = include_str!("../../../../crates/teamcraft-ui/i18n/en.ftl");
const DE_SOURCE: &str = include_str!("../../../../crates/teamcraft-ui/i18n/de.ftl");
/// A brand name longer than this is not shown (the brand check allows 100 characters).
const MAX_BRAND_NAME_CHARS: usize = 100;
/// A logo larger than this (as base64) is left out; the shell passes brand images up to 2 MiB.
const MAX_LOGO_CHARS: usize = 3 * 1024 * 1024;
/// Image types a logo may have. SVG is safe here: an `<img>` never runs its scripts.
const LOGO_TYPES: [&str; 6] = [
  "image/png",
  "image/jpeg",
  "image/webp",
  "image/gif",
  "image/avif",
  "image/svg+xml",
];

/// How the sign-in ended, as far as the browser tab is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallbackOutcome {
  SignedIn,
  /// The identity provider (or the user there) refused or cancelled.
  Refused,
  /// The redirect did not belong to the sign-in this app started.
  StateMismatch,
  /// Anything else (token exchange, network); the app shows the details.
  Failed,
}

/// The languages the page speaks; the UI's languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageLanguage {
  En,
  De,
}

impl CallbackOutcome {
  /// The HTTP status of the page.
  pub fn status(self) -> (u16, &'static str) {
    match self {
      Self::StateMismatch => (400, "Bad Request"),
      Self::SignedIn | Self::Refused | Self::Failed => (200, "OK"),
    }
  }

  fn keys(self) -> (&'static str, &'static str) {
    match self {
      Self::SignedIn => ("callback-signed-in-title", "callback-signed-in-body"),
      Self::Refused => ("callback-refused-title", "callback-refused-body"),
      Self::StateMismatch => ("callback-mismatch-title", "callback-mismatch-body"),
      Self::Failed => ("callback-failed-title", "callback-failed-body"),
    }
  }

  fn is_success(self) -> bool {
    self == Self::SignedIn
  }
}

impl PageLanguage {
  fn tag(self) -> &'static str {
    match self {
      Self::En => "en",
      Self::De => "de",
    }
  }

  fn from_tag(tag: &str) -> Option<Self> {
    let primary = tag.trim().split(['-', '_']).next()?.to_ascii_lowercase();
    match primary.as_str() {
      "en" => Some(Self::En),
      "de" => Some(Self::De),
      _ => None,
    }
  }

  /// The app's language if it sent one, else the best one of the browser's `Accept-Language`,
  /// else English.
  fn choose(maybe_app_locale: Option<&str>, maybe_accept_language: Option<&str>) -> Self {
    maybe_app_locale
      .and_then(Self::from_tag)
      .or_else(|| maybe_accept_language.and_then(Self::from_accept_language))
      .unwrap_or(Self::En)
  }

  /// `de-DE,de;q=0.9,en;q=0.8`: the supported language with the highest weight; among equal
  /// weights the first one listed.
  fn from_accept_language(header: &str) -> Option<Self> {
    let mut best: Option<(Self, f32)> = None;
    for entry in header.split(',') {
      let mut parts = entry.split(';');
      let Some(language) = parts.next().and_then(Self::from_tag) else {
        continue;
      };
      let weight = parts
        .filter_map(|param| param.trim().strip_prefix("q="))
        .find_map(|value| value.trim().parse::<f32>().ok())
        .unwrap_or(1.0);
      if weight > 0.0 && best.is_none_or(|(_, best_weight)| weight > best_weight) {
        best = Some((language, weight));
      }
    }
    best.map(|(language, _)| language)
  }

  fn text(self, key: &str) -> &'static str {
    let source = match self {
      Self::En => EN_SOURCE,
      Self::De => DE_SOURCE,
    };
    message(source, key)
      .or_else(|| message(EN_SOURCE, key))
      .unwrap_or_default()
  }
}

/// The complete HTML page for `outcome`.
pub fn render(outcome: CallbackOutcome, look: &SignInLook, maybe_accept_language: Option<&str>) -> String {
  let language = PageLanguage::choose(look.maybe_locale.as_deref(), maybe_accept_language);
  let (title_key, body_key) = outcome.keys();
  let title = language.text(title_key);
  let body = language.text(body_key);
  let maybe_name = brand_name(look);
  let page_title = match &maybe_name {
    Some(name) => format!("{} · {}", language.text("callback-page-title"), name),
    None => language.text("callback-page-title").to_owned(),
  };
  let theme = match look.theme {
    LookTheme::Light => " data-theme=\"light\"",
    LookTheme::Dark => " data-theme=\"dark\"",
    LookTheme::System | LookTheme::Unknown => "",
  };
  let state = if outcome.is_success() { "ok" } else { "error" };
  let icon = if outcome.is_success() {
    ICON_CHECK
  } else {
    ICON_ALERT
  };
  format!(
    "<!doctype html><html lang=\"{lang}\"{theme}><head><meta charset=\"utf-8\">\
     <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
     <meta name=\"color-scheme\" content=\"light dark\"><meta name=\"referrer\" content=\"no-referrer\">\
     <title>{page_title}</title><style>{STYLE}</style></head><body><main class=\"card\">{brand}\
     <div class=\"status {state}\" aria-hidden=\"true\">{icon}</div><h1>{title}</h1><p>{body}</p>\
     </main></body></html>",
    lang = language.tag(),
    page_title = escape(&page_title),
    brand = brand_block(look, maybe_name.as_deref()),
    title = escape(title),
    body = escape(body),
  )
}

/// The brand's logo and name above the status, or nothing (a neutral page).
fn brand_block(look: &SignInLook, maybe_name: Option<&str>) -> String {
  let maybe_light = look.maybe_logo.as_deref().filter(|url| is_safe_logo(url));
  let maybe_dark = look
    .maybe_logo_dark
    .as_deref()
    .filter(|url| is_safe_logo(url) && Some(*url) != maybe_light);
  let has_logo = maybe_light.is_some() || maybe_dark.is_some();
  // A wordmark carries the name: it is the image's alternative text instead of a second line.
  let is_wordmark = look.is_wordmark && has_logo;
  let alt = if is_wordmark {
    escape(maybe_name.unwrap_or_default())
  } else {
    String::new()
  };
  let mut block = String::new();
  match (maybe_light, maybe_dark) {
    (Some(light), Some(dark)) => {
      block.push_str(&format!(
        "<img class=\"logo logo-light\" src=\"{}\" alt=\"{alt}\"><img class=\"logo logo-dark\" src=\"{}\" alt=\"{alt}\">",
        escape(light),
        escape(dark)
      ));
    },
    (Some(only), None) | (None, Some(only)) => {
      block.push_str(&format!(
        "<img class=\"logo\" src=\"{}\" alt=\"{alt}\">",
        escape(only)
      ));
    },
    (None, None) => {},
  }
  if let Some(name) = maybe_name.filter(|_| !is_wordmark) {
    block.push_str(&format!(
      "<p class=\"brand\" translate=\"no\">{}</p>",
      escape(name)
    ));
  }
  if block.is_empty() {
    block
  } else {
    format!("<header>{block}</header>")
  }
}

fn brand_name(look: &SignInLook) -> Option<String> {
  look
    .maybe_brand_name
    .as_deref()
    .map(str::trim)
    .filter(|name| !name.is_empty() && name.chars().count() <= MAX_BRAND_NAME_CHARS)
    .filter(|name| !name.chars().any(char::is_control))
    .map(str::to_owned)
}

/// A `data:` URL of a known image type with valid base64, and not too large.
fn is_safe_logo(url: &str) -> bool {
  let Some((media_type, data)) = url
    .strip_prefix("data:")
    .and_then(|rest| rest.split_once(";base64,"))
  else {
    return false;
  };
  // `image/svg+xml; charset=utf-8`: the parameters don't change the type.
  let essence = media_type.split(';').next().unwrap_or_default().trim();
  LOGO_TYPES.contains(&essence)
    && !data.is_empty()
    && data.len() <= MAX_LOGO_CHARS
    && base64::engine::general_purpose::STANDARD.decode(data).is_ok()
}

/// The value of a single-line Fluent message `key = value`.
fn message(source: &'static str, key: &str) -> Option<&'static str> {
  source.lines().find_map(|line| {
    let (name, value) = line.split_once('=')?;
    (name.trim_end() == key && !line.starts_with(char::is_whitespace)).then(|| value.trim())
  })
}

/// Text for HTML content and double-quoted attributes.
fn escape(text: &str) -> String {
  let mut escaped = String::with_capacity(text.len());
  for ch in text.chars() {
    match ch {
      '&' => escaped.push_str("&amp;"),
      '<' => escaped.push_str("&lt;"),
      '>' => escaped.push_str("&gt;"),
      '"' => escaped.push_str("&quot;"),
      '\'' => escaped.push_str("&#39;"),
      other => escaped.push(other),
    }
  }
  escaped
}

const ICON_CHECK: &str =
  "<svg viewBox=\"0 0 24 24\" width=\"28\" height=\"28\"><path d=\"M5 12.5l4.5 4.5L19 7.5\"/></svg>";
const ICON_ALERT: &str =
  "<svg viewBox=\"0 0 24 24\" width=\"28\" height=\"28\"><path d=\"M12 8v5m0 3.5v.01\"/></svg>";

/// The UI's sign-in card tokens (style/main.css), light and dark.
const STYLE: &str = "\
:root{color-scheme:light;--bg:#f6f7f9;--surface:#fff;--border:#dce0e7;--text:#1b2230;--muted:#566175;\
--accent:#0b7f78;--accent-soft:#dff3f1;--danger:#c0392b;--danger-soft:#f9e3e0;\
--shadow:0 1px 2px rgba(16,24,40,.06),0 12px 32px rgba(16,24,40,.12);\
font:15px/1.5 system-ui,-apple-system,\"Segoe UI\",Roboto,\"Noto Sans\",\"Helvetica Neue\",sans-serif}\
@media (prefers-color-scheme:dark){:root:not([data-theme=light]){color-scheme:dark;--bg:#0f141d;\
--surface:#151b27;--border:#28324a;--text:#e4e9f2;--muted:#9aa6ba;--accent:#35c6ba;--accent-soft:#123a38;\
--danger:#ff7a6b;--danger-soft:#3a1d1a;--shadow:0 1px 2px rgba(0,0,0,.3),0 12px 32px rgba(0,0,0,.45)}}\
:root[data-theme=dark]{color-scheme:dark;--bg:#0f141d;--surface:#151b27;--border:#28324a;--text:#e4e9f2;\
--muted:#9aa6ba;--accent:#35c6ba;--accent-soft:#123a38;--danger:#ff7a6b;--danger-soft:#3a1d1a;\
--shadow:0 1px 2px rgba(0,0,0,.3),0 12px 32px rgba(0,0,0,.45)}\
*{box-sizing:border-box}\
body{margin:0;min-height:100vh;display:grid;place-items:center;padding:24px 16px;background:var(--bg);color:var(--text)}\
.card{width:min(420px,100%);padding:40px 32px 36px;border:1px solid var(--border);border-radius:16px;\
background:var(--surface);box-shadow:var(--shadow);text-align:center;animation:enter .32s cubic-bezier(.2,.8,.2,1) both}\
header{display:flex;flex-direction:column;align-items:center;gap:8px;margin-bottom:28px}\
.logo{display:block;max-width:200px;max-height:48px;object-fit:contain}\
.logo-dark{display:none}\
@media (prefers-color-scheme:dark){:root:not([data-theme=light]) .logo-dark{display:block}\
:root:not([data-theme=light]) .logo-light{display:none}}\
:root[data-theme=dark] .logo-dark{display:block}:root[data-theme=dark] .logo-light{display:none}\
.brand{margin:0;font-weight:600;font-size:15px;color:var(--muted)}\
.status{display:grid;place-items:center;width:56px;height:56px;margin:0 auto 20px;border-radius:50%}\
.status svg{fill:none;stroke:currentColor;stroke-width:2.4;stroke-linecap:round;stroke-linejoin:round}\
.ok{background:var(--accent-soft);color:var(--accent)}.error{background:var(--danger-soft);color:var(--danger)}\
h1{margin:0 0 8px;font-size:22px;line-height:1.3;font-weight:650;letter-spacing:-.01em}\
p{margin:0;color:var(--muted)}\
@keyframes enter{from{opacity:0;transform:translateY(8px)}to{opacity:1;transform:none}}\
@media (prefers-reduced-motion:reduce){.card{animation:none}}\
@media (max-width:480px){body{place-items:start center}.card{padding:32px 20px 28px;box-shadow:none}}";

#[cfg(test)]
mod tests {
  use super::*;

  const LOGO: &str = "data:image/png;base64,iVBORw0KGgo=";

  mod language {
    use super::*;

    #[test]
    fn the_apps_locale_wins_over_the_browser() {
      let page = render(
        CallbackOutcome::SignedIn,
        &look(Some("de")),
        Some("en-US,en;q=0.9"),
      );
      assert!(page.contains("<html lang=\"de\""));
      assert!(page.contains("Angemeldet"));
      assert!(!page.contains("Signed in"));
    }

    #[test]
    fn without_the_apps_locale_the_browser_decides() {
      let page = render(
        CallbackOutcome::SignedIn,
        &look(None),
        Some("fr-FR,de;q=0.8,en;q=0.5"),
      );
      assert!(page.contains("lang=\"de\""));
      assert_eq!(
        PageLanguage::from_accept_language("en;q=0.2, de-AT;q=0.9"),
        Some(PageLanguage::De)
      );
      assert_eq!(
        PageLanguage::from_accept_language("de;q=0, en"),
        Some(PageLanguage::En)
      );
      assert_eq!(PageLanguage::from_accept_language("fr, it"), None);
    }

    #[test]
    fn unknown_languages_fall_back_to_english() {
      let page = render(CallbackOutcome::Failed, &look(Some("fr")), Some("ja"));
      assert!(page.contains("lang=\"en\""));
      assert!(page.contains("Sign-in failed"));
    }

    #[test]
    fn every_outcome_has_both_languages() {
      let outcomes = [
        CallbackOutcome::SignedIn,
        CallbackOutcome::Refused,
        CallbackOutcome::StateMismatch,
        CallbackOutcome::Failed,
      ];
      for outcome in outcomes {
        let (title, body) = outcome.keys();
        for key in [title, body, "callback-page-title"] {
          for source in [EN_SOURCE, DE_SOURCE] {
            let text = message(source, key).unwrap_or_default();
            assert!(!text.is_empty(), "{key} is missing");
            assert!(!text.contains('{'), "{key} must be plain text");
          }
          assert_ne!(message(EN_SOURCE, key), message(DE_SOURCE, key), "{key}");
        }
      }
    }
  }

  mod escaping {
    use super::*;

    #[test]
    fn the_brand_name_is_escaped() {
      let mut hostile = look(Some("en"));
      hostile.maybe_brand_name = Some("<script>alert(\"x\")</script> & Co".to_owned());
      let page = render(CallbackOutcome::SignedIn, &hostile, None);
      assert!(!page.contains("<script>"));
      assert!(page.contains("&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt; &amp; Co"));
    }

    #[test]
    fn only_checked_data_images_are_used() {
      let mut branded = look(Some("en"));
      branded.maybe_logo = Some(LOGO.to_owned());
      assert!(render(CallbackOutcome::SignedIn, &branded, None).contains(LOGO));
      for bad in [
        "https://example.com/logo.png",
        "data:text/html;base64,PHNjcmlwdD4=",
        "data:image/png;base64,\" onerror=\"alert(1)",
        "data:image/png,plain",
        "javascript:alert(1)",
      ] {
        branded.maybe_logo = Some(bad.to_owned());
        let page = render(CallbackOutcome::SignedIn, &branded, None);
        assert!(!page.contains("<img"), "{bad}");
      }
    }

    #[test]
    fn a_page_without_brand_is_neutral() {
      let page = render(CallbackOutcome::SignedIn, &look(Some("en")), None);
      assert!(!page.contains("<header>") && !page.contains("<img"));
      assert!(page.contains("<title>Sign-in</title>"));
    }

    #[test]
    fn a_wordmark_replaces_the_name_and_an_icon_keeps_it() {
      let mut branded = look(Some("en"));
      branded.maybe_brand_name = Some("Example Chat".to_owned());
      branded.maybe_logo = Some(LOGO.to_owned());
      branded.is_wordmark = true;
      let page = render(CallbackOutcome::SignedIn, &branded, None);
      assert!(page.contains("alt=\"Example Chat\""));
      assert!(!page.contains("class=\"brand\""));
      assert!(page.contains("<title>Sign-in · Example Chat</title>"));

      branded.is_wordmark = false;
      let page = render(CallbackOutcome::SignedIn, &branded, None);
      assert!(page.contains("alt=\"\"") && page.contains("class=\"brand\""));

      // A wordmark that failed the checks: the name is shown after all.
      branded.is_wordmark = true;
      branded.maybe_logo = Some("https://example.com/logo.png".to_owned());
      let page = render(CallbackOutcome::SignedIn, &branded, None);
      assert!(page.contains("class=\"brand\""));
    }

    #[test]
    fn the_page_loads_nothing_and_runs_nothing() {
      let mut branded = look(Some("de"));
      branded.maybe_brand_name = Some("Example Chat".to_owned());
      branded.maybe_logo = Some(LOGO.to_owned());
      branded.theme = LookTheme::Dark;
      let page = render(CallbackOutcome::StateMismatch, &branded, None);
      assert!(!page.contains("<script") && !page.contains("http://") && !page.contains("https://"));
      assert!(page.contains("data-theme=\"dark\""));
      assert!(page.contains("Example Chat"));
    }
  }

  #[test]
  fn outcomes_have_their_status() {
    assert_eq!(CallbackOutcome::SignedIn.status().0, 200);
    assert_eq!(CallbackOutcome::StateMismatch.status().0, 400);
  }

  fn look(maybe_locale: Option<&str>) -> SignInLook {
    SignInLook {
      maybe_locale: maybe_locale.map(str::to_owned),
      ..SignInLook::default()
    }
  }
}
