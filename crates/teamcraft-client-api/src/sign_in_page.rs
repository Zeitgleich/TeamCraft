//! What the sign-in page shows (the `sign_in` part of `GET /api/v1/server-info`), checked like the
//! brand: known methods only, the provider icon only from the brand endpoint, https links only.
//! The web, desktop and mobile sign-in pages render the same thing from it.

use teamcraft_proto::{SignInInfo, SignInMethod};

use crate::brand::{clean_name, pick, safe_link};

/// The only path the provider icon may have.
const SSO_ICON_PATH: &str = "/api/v1/brand/assets/sso_icon";
const MAX_HELP_CHARS: usize = 1000;

/// The sign-in page of one server, ready to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInPage {
  /// What to offer, in this order; never empty.
  pub methods: Vec<SignInMethod>,
  /// The provider part of "Sign in with …", e.g. "Microsoft".
  pub sso_label: String,
  /// Absolute URL of the provider icon.
  pub maybe_sso_icon_url: Option<String>,
  /// Help in the asked language (else English).
  pub maybe_help_text: Option<String>,
  pub maybe_help_link: Option<String>,
  /// The one-time code field of the local form is required (else optional, "if set up").
  pub local_requires_code: bool,
}

impl SignInPage {
  /// `server_url` is the server's origin (empty: same origin); `brand_name` the fallback provider
  /// label; `language` picks the help text.
  pub fn from_info(info: &SignInInfo, server_url: &str, brand_name: &str, language: &str) -> Self {
    let mut methods: Vec<SignInMethod> = Vec::new();
    for method in &info.methods {
      let is_offered = match method {
        SignInMethod::Sso => true,
        SignInMethod::Local => info.has_local_accounts,
        SignInMethod::Unknown(_) => false,
      };
      if is_offered && !methods.contains(method) {
        methods.push(method.clone());
      }
    }
    if methods.is_empty() {
      methods.push(SignInMethod::Sso);
    }
    let maybe_sso_icon_url = info
      .maybe_sso_icon
      .as_deref()
      .filter(|path| *path == SSO_ICON_PATH)
      .map(|path| format!("{}{path}", server_url.trim_end_matches('/')));
    let maybe_help_text = pick(&info.help_text, language)
      .map(str::trim)
      .filter(|text| !text.is_empty() && text.chars().count() <= MAX_HELP_CHARS)
      .map(str::to_owned);
    Self {
      methods,
      sso_label: clean_name(info.maybe_sso_label.clone())
        .or_else(|| clean_name(Some(brand_name.to_owned())))
        .unwrap_or_else(|| "SSO".to_owned()),
      maybe_sso_icon_url,
      maybe_help_text,
      maybe_help_link: safe_link(info.maybe_help_link.clone()),
      local_requires_code: info.local_requires_code,
    }
  }

  /// Only the identity provider: what a client shows before it knows the server's page (offline,
  /// an older server).
  pub fn sso_only(brand_name: &str) -> Self {
    Self {
      methods: vec![SignInMethod::Sso],
      sso_label: clean_name(Some(brand_name.to_owned())).unwrap_or_else(|| "SSO".to_owned()),
      maybe_sso_icon_url: None,
      maybe_help_text: None,
      maybe_help_link: None,
      local_requires_code: false,
    }
  }

  pub fn offers(&self, method: &SignInMethod) -> bool {
    self.methods.contains(method)
  }
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;

  use super::*;

  const SERVER: &str = "https://team.example.com";

  #[test]
  fn the_servers_choices_are_rendered_in_order() {
    let mut info = info();
    info.methods = vec![SignInMethod::Local, SignInMethod::Sso];
    info.maybe_sso_label = Some(" Example SSO ".into());
    info.maybe_sso_icon = Some(SSO_ICON_PATH.into());
    info.help_text = BTreeMap::from([
      ("en".to_owned(), "Call the help desk.".to_owned()),
      ("de".to_owned(), "Ruf den Helpdesk an.".to_owned()),
    ]);
    info.maybe_help_link = Some("https://help.example.com/sign-in".into());
    let page = SignInPage::from_info(&info, SERVER, "Example Chat", "de-DE");
    assert_eq!(page.methods, [SignInMethod::Local, SignInMethod::Sso]);
    assert_eq!(page.sso_label, "Example SSO");
    assert_eq!(
      page.maybe_sso_icon_url.as_deref(),
      Some("https://team.example.com/api/v1/brand/assets/sso_icon")
    );
    assert_eq!(page.maybe_help_text.as_deref(), Some("Ruf den Helpdesk an."));
    assert_eq!(
      page.maybe_help_link.as_deref(),
      Some("https://help.example.com/sign-in")
    );
  }

  #[test]
  fn local_needs_local_accounts_and_nothing_left_means_sso() {
    let mut info = info();
    info.has_local_accounts = false;
    info.methods = vec![SignInMethod::Local, SignInMethod::Unknown("passkey".into())];
    let page = SignInPage::from_info(&info, SERVER, "Example Chat", "en");
    assert_eq!(page.methods, [SignInMethod::Sso]);
    assert_eq!(page.sso_label, "Example Chat");
  }

  #[test]
  fn hostile_values_are_dropped() {
    let mut info = info();
    info.maybe_sso_label = Some("x".repeat(500));
    info.maybe_sso_icon = Some("https://evil.example/icon.svg".into());
    info.maybe_help_link = Some("javascript:alert(1)".into());
    info.methods = vec![SignInMethod::Sso, SignInMethod::Sso];
    let page = SignInPage::from_info(&info, SERVER, "Example Chat", "en");
    assert_eq!(page.methods, [SignInMethod::Sso]);
    assert_eq!(page.sso_label, "Example Chat");
    assert!(page.maybe_sso_icon_url.is_none());
    assert!(page.maybe_help_link.is_none());
  }

  fn info() -> SignInInfo {
    SignInInfo {
      issuer: "https://auth.example.com/realms/company".into(),
      client_id: "teamcraft".into(),
      scopes: vec!["openid".into()],
      maybe_redirect_uris: None,
      has_local_accounts: true,
      local_requires_code: true,
      methods: vec![SignInMethod::Sso],
      maybe_sso_label: None,
      maybe_sso_icon: None,
      help_text: BTreeMap::new(),
      maybe_help_link: None,
    }
  }
}
