//! The sign-in page settings of the server (`sign_in` of `GET /api/v1/server-info`): which methods
//! the sign-in card offers, in which order, the provider label and icon, and the help below. They
//! change at runtime (admin console), so they are loaded each time the sign-in screen is shown;
//! until they arrive, or when the server cannot be reached, the card offers the identity provider
//! only.

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;
use teamcraft_client_api::server_entry::SERVER_INFO_PATH;
use teamcraft_client_api::sign_in_page::SignInPage;
use teamcraft_proto::{SERVER_INFO_PRODUCT, ServerInfoResponse, SignInInfo, SignInMethod};

use crate::auth::{AppConfig, fetch, tauri};
use crate::brand::Brand;
use crate::i18n::use_i18n;
use crate::reactive::SetIfAlive;

/// Longest server version shown ("1.0.9-rc.12+build.345").
const MAX_VERSION_CHARS: usize = 40;

/// The sign-in page of the current server, loading in the background.
#[derive(Clone, Copy)]
pub struct SignInPageState {
  /// What to render; the identity provider only until the server's answer is in.
  pub page: Memo<SignInPage>,
  pub is_loading: RwSignal<bool>,
}

/// One part of the sign-in card, in display order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignInBlock {
  /// "Sign in with {label}".
  Sso {
    label: String,
    maybe_icon_url: Option<String>,
  },
  /// E-mail, password and one-time code.
  Local { requires_code: bool },
  /// "or" between two methods.
  Divider,
}

impl SignInPageState {
  /// Starts loading the page of the server at `server_url` (empty: the page's own origin). The
  /// provider label falls back to the brand's name; the help text follows the UI language.
  pub fn load(server_url: String) -> Self {
    let i18n = use_i18n();
    let brand = Brand::use_current().0;
    let maybe_info = RwSignal::new(None::<SignInInfo>);
    let is_loading = RwSignal::new(true);
    let icon_base = server_url.clone();
    let page = Memo::new(move |_| {
      let brand_name = brand.with(|view| view.display_name.clone());
      maybe_info
        .with(|maybe_info| page_for(maybe_info.as_ref(), &icon_base, &brand_name, i18n.locale().tag()))
    });
    spawn_local(async move {
      match fetch_server_info(&server_url).await {
        Ok((status, body)) => match parse_answer(status, &body) {
          Some(info) => maybe_info.set_if_alive(Some(info)),
          None => tracing::debug!(status, "the server-info answer is unusable; offering SSO only"),
        },
        Err(error) => tracing::debug!(%error, "could not load the sign-in page; offering SSO only"),
      }
      is_loading.set_if_alive(false);
    });
    Self { page, is_loading }
  }
}

/// The page to render: the server's when it answered, else the identity provider alone.
pub fn page_for(
  maybe_info: Option<&SignInInfo>,
  server_url: &str,
  brand_name: &str,
  language: &str,
) -> SignInPage {
  match maybe_info {
    Some(info) => SignInPage::from_info(info, server_url, brand_name, language),
    None => SignInPage::sso_only(brand_name),
  }
}

/// The `sign_in` part of a `server-info` answer, if it is one of a TeamCraft server.
pub fn parse_answer(status: u16, body: &str) -> Option<SignInInfo> {
  if status != 200 {
    return None;
  }
  serde_json::from_str::<ServerInfoResponse>(body)
    .ok()
    .filter(|info| info.product == SERVER_INFO_PRODUCT)
    .map(|info| info.sign_in)
}

/// The version of the server at `server_url` (empty: the page's own origin) from its public
/// `server-info`, for Help & about and the debug info (issue #410). `None` when it can't be reached
/// or the answer isn't a TeamCraft server's.
pub async fn server_version(server_url: &str) -> Option<String> {
  match fetch_server_info(server_url).await {
    Ok((status, body)) => parse_server_version(status, &body),
    Err(error) => {
      tracing::debug!(%error, "could not ask the server for its version");
      None
    },
  }
}

/// The `server_version` of a `server-info` answer, if it reads like a version: it is shown and
/// copied, so only short version text passes.
pub fn parse_server_version(status: u16, body: &str) -> Option<String> {
  if status != 200 {
    return None;
  }
  serde_json::from_str::<ServerInfoResponse>(body)
    .ok()
    .filter(|info| info.product == SERVER_INFO_PRODUCT)
    .map(|info| info.server_version)
    .filter(|version| {
      !version.is_empty()
        && version.len() <= MAX_VERSION_CHARS
        && version
          .chars()
          .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
    })
}

/// The card's parts in the server's order, with an "or" between methods. The desktop shell's
/// content policy blocks remote images, so it shows the provider without its icon.
pub fn blocks(page: &SignInPage, is_desktop: bool) -> Vec<SignInBlock> {
  let mut parts = Vec::new();
  for method in &page.methods {
    let block = match method {
      SignInMethod::Sso => SignInBlock::Sso {
        label: page.sso_label.clone(),
        maybe_icon_url: page.maybe_sso_icon_url.clone().filter(|_| !is_desktop),
      },
      SignInMethod::Local => SignInBlock::Local {
        requires_code: page.local_requires_code,
      },
      SignInMethod::Unknown(_) => continue,
    };
    if !parts.is_empty() {
      parts.push(SignInBlock::Divider);
    }
    parts.push(block);
  }
  parts
}

/// `GET /api/v1/server-info` without credentials: through the desktop shell (the active server), or
/// the page's own fetch.
async fn fetch_server_info(server_url: &str) -> Result<(u16, String), String> {
  #[derive(Deserialize)]
  struct Payload {
    status: u16,
    body: String,
  }
  if tauri::is_available() {
    let payload =
      tauri::call::<Payload>("public_get", &serde_json::json!({ "path": SERVER_INFO_PATH })).await?;
    return Ok((payload.status, payload.body));
  }
  let config = AppConfig {
    server_url: server_url.to_owned(),
    ..AppConfig::default()
  };
  let response = fetch::fetch(
    "GET",
    &config.api_url(SERVER_INFO_PATH),
    &[("Accept".to_owned(), "application/json".to_owned())],
    None,
  )
  .await?;
  Ok((
    response.status,
    String::from_utf8_lossy(&response.body).into_owned(),
  ))
}

#[cfg(test)]
mod tests {
  use super::*;

  const SERVER: &str = "https://team.example.com";

  mod answers {
    use super::*;

    #[test]
    fn the_server_version_is_read_from_a_teamcraft_answer_only() {
      let answer = answer(r#"["sso"]"#);
      assert_eq!(parse_server_version(200, &answer).as_deref(), Some("1.0.0"));
      assert_eq!(parse_server_version(500, &answer), None);
      assert_eq!(parse_server_version(200, "<html>portal</html>"), None);
      let odd = answer.replace("\"1.0.0\"", "\"1.0<script>\"");
      assert_eq!(parse_server_version(200, &odd), None);
    }

    #[test]
    fn a_teamcraft_answer_gives_its_sign_in_part() {
      let info = parse_answer(200, &answer(r#"["local","sso"]"#)).expect("usable");
      assert_eq!(info.methods, [SignInMethod::Local, SignInMethod::Sso]);
      assert!(info.has_local_accounts);
    }

    #[test]
    fn other_answers_are_ignored() {
      assert!(parse_answer(503, &answer(r#"["sso"]"#)).is_none());
      assert!(parse_answer(200, "<html>captive portal</html>").is_none());
      let other_product = answer(r#"["sso"]"#).replace("\"teamcraft\"", "\"something-else\"");
      assert!(parse_answer(200, &other_product).is_none());
    }
  }

  mod rendering {
    use super::*;

    #[test]
    fn methods_keep_the_servers_order_with_a_divider_between() {
      let info = parse_answer(200, &answer(r#"["local","sso"]"#)).expect("usable");
      let page = page_for(Some(&info), SERVER, "Example Chat", "en");
      assert_eq!(
        blocks(&page, false),
        [
          SignInBlock::Local { requires_code: true },
          SignInBlock::Divider,
          SignInBlock::Sso {
            label: "Example SSO".into(),
            maybe_icon_url: Some(format!("{SERVER}/api/v1/brand/assets/sso_icon")),
          },
        ]
      );
    }

    #[test]
    fn the_desktop_shows_no_remote_icon() {
      let info = parse_answer(200, &answer(r#"["sso"]"#)).expect("usable");
      let page = page_for(Some(&info), SERVER, "Example Chat", "en");
      assert_eq!(
        blocks(&page, true),
        [SignInBlock::Sso {
          label: "Example SSO".into(),
          maybe_icon_url: None
        }]
      );
    }

    #[test]
    fn without_an_answer_only_the_identity_provider_is_offered() {
      let page = page_for(None, SERVER, "Example Chat", "en");
      assert_eq!(
        blocks(&page, false),
        [SignInBlock::Sso {
          label: "Example Chat".into(),
          maybe_icon_url: None
        }]
      );
    }
  }

  /// A `server-info` answer with these methods, local accounts that need a code, a provider label
  /// and icon.
  fn answer(methods: &str) -> String {
    format!(
      r#"{{
        "product": "teamcraft",
        "server_version": "1.0.0",
        "minimum_client_version": "1.0.0",
        "brand": {{ "id": "teamcraft", "display_name": "Example Chat", "origin": "{SERVER}" }},
        "sign_in": {{
          "issuer": "https://auth.example.com/realms/company",
          "client_id": "teamcraft",
          "scopes": ["openid"],
          "has_local_accounts": true,
          "local_requires_code": true,
          "methods": {methods},
          "sso_label": "Example SSO",
          "sso_icon": "/api/v1/brand/assets/sso_icon"
        }}
      }}"#
    )
  }
}
