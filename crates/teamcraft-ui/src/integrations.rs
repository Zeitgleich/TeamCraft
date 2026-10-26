//! Bots and the bridge setup in the app (docs/bridge.md): the admin's list of bot accounts with
//! create / rotate / revoke, and the text a bridge operator pastes (configuration, webhook and log
//! URLs). The server owns the bots; this module is the REST client and the pure text builders.

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_proto::{BotResponse, BotTokenResponse, CreateBotRequest, ListBotsResponse};

use crate::auth::ApiMethod;
use crate::model::{ConversationId, UserId};
use crate::reactive::SetIfAlive;
use crate::rest::{RestClient, RestError};

/// Where the bridge listens by default (`[listen]` of its configuration).
const DEFAULT_BRIDGE_URL: &str = "http://127.0.0.1:8088";
/// Longest bot name the form accepts; the server has the last word.
pub const MAX_BOT_NAME_CHARS: usize = 80;

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

/// What `teamcraft-bridge.toml` needs for this bot: the server and the token (written to a file
/// the operator keeps secret, never inline in the example). The token is shown once, so the snippet
/// is built from the reveal.
pub fn bridge_config(server_url: &str, bot_name: &str, token: &str) -> String {
  format!(
    "[server]\nurl = \"{}\"\nbot_token = \"{token}\"\n\n[identity]\ndevice_name = \"{}\"\n",
    toml_escape(server_url.trim_end_matches('/')),
    toml_escape(bot_name),
  )
}

/// A `[[sources]]` block that sends to `conversation`; `source_name` is the label in the bridge's
/// logs and `source_token` the secret part of the URLs.
pub fn source_block(conversation: ConversationId, source_name: &str, source_token: &str) -> String {
  format!(
    "[[sources]]\nname = \"{}\"\ntoken = \"{source_token}\"\nconversation_id = \"{conversation}\"\n",
    toml_escape(source_name)
  )
}

/// The URL a webhook sender posts to.
pub fn webhook_url(source_token: &str) -> String {
  format!("{DEFAULT_BRIDGE_URL}/hooks/{source_token}")
}

/// The URL log lines are piped to.
pub fn ingest_url(source_token: &str) -> String {
  format!("{DEFAULT_BRIDGE_URL}/ingest/{source_token}")
}

/// A random-looking source token for the snippet: 32 hex characters from the browser's CSPRNG
/// (`getrandom`); the operator may replace it with any 24+ characters.
pub fn new_source_token() -> String {
  let mut bytes = [0_u8; 16];
  // Without a random source the snippet shows a placeholder the operator must replace: never a
  // predictable token that looks real.
  if getrandom::getrandom(&mut bytes).is_err() {
    return "replace-with-24-or-more-random-characters".to_owned();
  }
  bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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
  use crate::model::id_from_raw;

  #[test]
  fn the_config_names_server_token_and_device() {
    let text = bridge_config("https://chat.example.com/", "NAS \"bridge\"", "tcb_abc");
    assert!(text.contains("url = \"https://chat.example.com\""));
    assert!(text.contains("bot_token = \"tcb_abc\""));
    assert!(text.contains("device_name = \"NAS \\\"bridge\\\"\""), "{text}");
  }

  #[test]
  fn a_source_block_points_at_the_channel() {
    let text = source_block(id_from_raw(42), "nas-logs", "tok");
    assert!(text.contains("conversation_id = \"42\""));
    assert!(text.contains("name = \"nas-logs\""));
  }

  #[test]
  fn urls_carry_the_source_token() {
    assert_eq!(webhook_url("t"), "http://127.0.0.1:8088/hooks/t");
    assert_eq!(ingest_url("t"), "http://127.0.0.1:8088/ingest/t");
  }

  #[test]
  fn control_characters_cannot_break_out_of_a_toml_string() {
    assert_eq!(toml_escape("a\nb\"c"), "a b\\\"c");
  }
}
