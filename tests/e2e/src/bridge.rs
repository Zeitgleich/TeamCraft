//! The `teamcraft-bridge` process: webhooks and logs into an integration channel.

use std::time::Duration;

use crate::process::{Service, binary, free_tcp_port};
use crate::stack::Stack;

pub const SOURCE_TOKEN: &str = "e2e-source-token-0123456789";
const START_TIMEOUT: Duration = Duration::from_secs(60);

pub struct BridgeProcess {
  pub service: Service,
  /// `http://127.0.0.1:<port>`: `POST /hooks/<token>` and `POST /ingest/<token>`.
  pub base_url: String,
}

impl BridgeProcess {
  /// Starts a bridge for `bot_token` that delivers the source `SOURCE_TOKEN` into `conversation`.
  /// `mention_user_id` is the member that danger alerts mention.
  pub async fn start(stack: &Stack, bot_token: &str, conversation: &str, mention_user_id: &str) -> Self {
    let port = free_tcp_port();
    let config_path = stack.dir.join("bridge.toml");
    let store_path = stack.dir.join("bridge.db");
    let config = format!(
      r#"
[server]
url = "{url}"
bot_token = "{bot_token}"

[store]
path = "{store}"
# 32 bytes of base64: a throwaway key for a throwaway database.
key = "ZTJlLWJyaWRnZS1zdG9yZS1rZXktMDEyMzQ1Njc4OTA="

[identity]
passphrase = "e2e bridge passphrase"
argon2_memory_kib = 1024
argon2_iterations = 1

[listen]
addrs = ["127.0.0.1:{port}"]

[batching]
interval_ms = 200

[index]
interval_secs = 0

[[sources]]
name = "nas"
token = "{SOURCE_TOKEN}"
conversation_id = "{conversation}"
danger_is_alert = true
mentions = [{{ user_id = "{mention_user_id}", name = "carol" }}]
"#,
      url = stack.base_url,
      store = store_path.display(),
    );
    std::fs::write(&config_path, config).expect("writing the bridge config");
    let envs = vec![
      ("NO_COLOR".to_owned(), "1".to_owned()),
      ("RUST_LOG".to_owned(), "info".to_owned()),
    ];
    let args = vec!["--config".to_owned(), config_path.display().to_string()];
    let mut service = Service::spawn("bridge", &binary("teamcraft-bridge"), &args, &envs, &stack.dir)
      .expect("spawning the bridge");
    let base_url = format!("http://127.0.0.1:{port}");
    service
      .wait_for_http(&format!("{base_url}/healthz"), START_TIMEOUT)
      .await
      .expect("the bridge starts");
    Self { service, base_url }
  }

  /// POSTs a body to one of the bridge's endpoints; returns the status.
  pub async fn post(&self, path: &str, content_type: &str, body: &str) -> reqwest::StatusCode {
    reqwest::Client::new()
      .post(format!("{}{path}", self.base_url))
      .header("content-type", content_type)
      .body(body.to_owned())
      .send()
      .await
      .expect("the bridge answers")
      .status()
  }
}
