//! Bots and the bridge setup in the app (docs/bridge.md): the admin's list of bot accounts with
//! create / rotate / revoke, the text a bridge operator pastes (configuration), and the webhook and
//! log URLs of a source. The server owns the bots; this module is the REST client and the pure
//! builders.

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_crypto::source_token::SourceToken;
use teamcraft_proto::{
  BotResponse, BotTokenResponse, CreateBotRequest, ListBotsResponse, MAX_SOURCE_NAME_CHARS, SourceTokenHash,
};

use crate::auth::ApiMethod;
use crate::model::UserId;
use crate::reactive::SetIfAlive;
use crate::rest::{RestClient, RestError};

/// Where the bridge reads the bot token from (a mounted secret in the container).
const TOKEN_FILE: &str = "/run/secrets/teamcraft-bridge-token";
/// Where the bridge listens by default (`[listen]` of its configuration).
const DEFAULT_BRIDGE_URL: &str = "http://127.0.0.1:8088";
/// Longest bot name the form accepts; the server has the last word.
pub const MAX_BOT_NAME_CHARS: usize = 80;

/// Where the bridge that receives a source's webhooks and log lines runs; it decides the base of the
/// URLs the channel settings show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeLocation {
  /// On the instance (teamcraft-deploy role `teamcraft_bridge`): the proxy routes `/hooks/` and
  /// `/ingest/` of the instance's host name to it.
  Instance,
  /// Next to the producers, on its default loopback address.
  Loopback,
}

/// The bots of the instance, for the admin console. Provided as context after sign-in.
#[derive(Clone, Copy)]
pub struct BotsHub {
  client: StoredValue<RestClient, LocalStorage>,
  pub bots: RwSignal<Vec<BotResponse>>,
  pub is_loaded: RwSignal<bool>,
  /// The bot just created or re-keyed, with the token that is shown once. Dropped when the dialog
  /// that shows it closes.
  pub maybe_reveal: RwSignal<Option<BotTokenResponse>>,
}

impl BotsHub {
  pub fn new(client: RestClient) -> Self {
    Self {
      client: StoredValue::new_local(client),
      bots: RwSignal::new(Vec::new()),
      is_loaded: RwSignal::new(false),
      maybe_reveal: RwSignal::new(None),
    }
  }

  /// Reloads the list; a failure (a non-admin asking) leaves what is shown.
  pub fn refresh(&self) {
    let hub = *self;
    spawn_local(async move {
      match hub
        .client
        .get_value()
        .get::<ListBotsResponse>("/api/v1/admin/bots")
        .await
      {
        Ok(list) => {
          hub.bots.set_if_alive(list.bots);
          hub.is_loaded.set_if_alive(true);
        },
        Err(error) => tracing::debug!(%error, "bots not loaded"),
      }
    });
  }

  pub async fn create(&self, name: String) -> Result<(), RestError> {
    let created: BotTokenResponse = self
      .client
      .get_value()
      .post(
        "/api/v1/admin/bots",
        &CreateBotRequest {
          name,
          maybe_description: None,
        },
      )
      .await?;
    self.maybe_reveal.set_if_alive(Some(created));
    self.refresh();
    Ok(())
  }

  /// A new token; the old one stops working at once.
  pub async fn rotate(&self, bot: UserId) -> Result<(), RestError> {
    let rotated: BotTokenResponse = self
      .client
      .get_value()
      .post(&format!("/api/v1/admin/bots/{bot}/token"), &serde_json::json!({}))
      .await?;
    self.maybe_reveal.set_if_alive(Some(rotated));
    self.refresh();
    Ok(())
  }

  /// Deactivates the bot: its token dies, its devices go and its sessions end.
  pub async fn revoke(&self, bot: UserId) -> Result<(), RestError> {
    self
      .client
      .get_value()
      .send(ApiMethod::Delete, &format!("/api/v1/admin/bots/{bot}"), None)
      .await?;
    self.refresh();
    Ok(())
  }
}

pub fn use_bots() -> Option<BotsHub> {
  use_context::<BotsHub>()
}

/// What `teamcraft-bridge.toml` needs for this bot: the server, and a reference to a file that holds
/// the token. The token itself is not written into the snippet, so a pasted configuration never
/// carries the credential; it is shown beside it to be saved into that file.
pub fn bridge_config(server_url: &str, bot_name: &str) -> String {
  format!(
    "[server]\nurl = \"{}\"\nbot_token_file = \"{TOKEN_FILE}\"\n\n[identity]\ndevice_name = \"{}\"\n",
    toml_escape(server_url.trim_end_matches('/')),
    toml_escape(bot_name),
  )
}

/// Where producers reach the bridge: the instance's address (`server_url`) or the loopback default.
/// Without a known instance address the loopback default is the only usable answer.
pub fn bridge_base_url(location: BridgeLocation, server_url: &str) -> String {
  let server = server_url.trim().trim_end_matches('/');
  match location {
    BridgeLocation::Instance if !server.is_empty() => server.to_owned(),
    BridgeLocation::Instance | BridgeLocation::Loopback => DEFAULT_BRIDGE_URL.to_owned(),
  }
}

/// The URL a webhook sender posts to.
pub fn webhook_url(base_url: &str, source_token: &str) -> String {
  format!("{base_url}/hooks/{source_token}")
}

/// The URL log lines are piped to.
pub fn ingest_url(base_url: &str, source_token: &str) -> String {
  format!("{base_url}/ingest/{source_token}")
}

/// A source token made for "Add source": the token to show once, and the hash that is announced.
pub struct NewSource {
  pub token: String,
  pub token_sha256: SourceTokenHash,
}

impl NewSource {
  /// `None` when the browser has no random source: never a predictable token.
  pub fn generate() -> Option<Self> {
    let token = SourceToken::random()?;
    Some(Self {
      token_sha256: SourceTokenHash::from_bytes(token.sha256()),
      token: token.expose_secret().to_owned(),
    })
  }
}

/// Whether a source name may be announced (the receiving devices drop other names).
pub fn is_valid_source_name(name: &str) -> bool {
  let name = name.trim();
  !name.is_empty() && name.chars().count() <= MAX_SOURCE_NAME_CHARS && !name.chars().any(char::is_control)
}

fn toml_escape(text: &str) -> String {
  text
    .replace('\\', "\\\\")
    .replace('"', "\\\"")
    .replace(['\n', '\r'], " ")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_config_names_server_token_and_device() {
    let text = bridge_config("https://chat.example.com/", "NAS \"bridge\"");
    assert!(text.contains("url = \"https://chat.example.com\""));
    assert!(text.contains("bot_token_file = \"/run/secrets/teamcraft-bridge-token\""));
    assert!(!text.contains("tcb_"), "the token is never inline");
    assert!(text.contains("device_name = \"NAS \\\"bridge\\\"\""), "{text}");
  }

  #[test]
  fn a_new_source_announces_the_hash_of_the_token_it_shows() {
    let source = NewSource::generate().unwrap();
    assert!(source.token.starts_with("tcs_"));
    assert_eq!(
      source.token_sha256,
      SourceTokenHash::from_bytes(teamcraft_crypto::source_token::source_token_sha256(&source.token))
    );
    assert_ne!(NewSource::generate().unwrap().token, source.token);
  }

  #[test]
  fn source_names_must_be_short_printable_and_not_blank() {
    assert!(is_valid_source_name(" ci "));
    assert!(!is_valid_source_name("   "));
    assert!(!is_valid_source_name(&"x".repeat(MAX_SOURCE_NAME_CHARS + 1)));
    assert!(!is_valid_source_name("a\nb"));
  }

  #[test]
  fn urls_carry_the_source_token() {
    assert_eq!(
      webhook_url(DEFAULT_BRIDGE_URL, "t"),
      "http://127.0.0.1:8088/hooks/t"
    );
    assert_eq!(
      ingest_url(DEFAULT_BRIDGE_URL, "t"),
      "http://127.0.0.1:8088/ingest/t"
    );
  }

  #[test]
  fn a_bridge_on_the_instance_is_reached_through_its_host_name() {
    let base = bridge_base_url(BridgeLocation::Instance, "https://chat.example.com/");
    assert_eq!(base, "https://chat.example.com");
    assert_eq!(webhook_url(&base, "t"), "https://chat.example.com/hooks/t");
    assert_eq!(ingest_url(&base, "t"), "https://chat.example.com/ingest/t");
  }

  #[test]
  fn a_local_bridge_or_an_unknown_instance_uses_the_loopback_default() {
    let server = "https://chat.example.com";
    assert_eq!(
      bridge_base_url(BridgeLocation::Loopback, server),
      DEFAULT_BRIDGE_URL
    );
    assert_eq!(bridge_base_url(BridgeLocation::Instance, " "), DEFAULT_BRIDGE_URL);
  }

  #[test]
  fn control_characters_cannot_break_out_of_a_toml_string() {
    assert_eq!(toml_escape("a\nb\"c"), "a b\\\"c");
  }
}
