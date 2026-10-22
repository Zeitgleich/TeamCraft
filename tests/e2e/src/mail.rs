//! Mailpit, the dev stack's mail catcher: what the server sent as iMIP invitations.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::stack::MAILPIT_URL;

pub struct Mailpit {
  http: reqwest::Client,
}

/// One caught e-mail.
#[derive(Debug)]
pub struct ReceivedMail {
  /// Mailpit's ID of the message.
  pub id: String,
  pub subject: String,
  /// Plain text body.
  pub text: String,
  /// The whole message as sent (headers, text/calendar part, invite.ics attachment).
  pub raw: String,
}

impl Mailpit {
  pub fn new() -> Self {
    Self {
      http: reqwest::Client::new(),
    }
  }

  /// Waits for a mail to `address` (use an address unique to the test run).
  pub async fn wait_for_mail_to(&self, address: &str, timeout: Duration) -> ReceivedMail {
    let deadline = Instant::now() + timeout;
    loop {
      if let Some(mail) = self.find(address).await {
        return mail;
      }
      assert!(
        Instant::now() < deadline,
        "no mail to {address} within {timeout:?}"
      );
      tokio::time::sleep(Duration::from_millis(200)).await;
    }
  }

  /// Every mail to `address`, newest first.
  pub async fn all_to(&self, address: &str) -> Vec<ReceivedMail> {
    let mut mails = Vec::new();
    for id in self.search(address).await {
      mails.extend(self.fetch(&id).await);
    }
    mails
  }

  /// The number of mails to `address` right now.
  pub async fn count_to(&self, address: &str) -> usize {
    self.search(address).await.len()
  }

  async fn find(&self, address: &str) -> Option<ReceivedMail> {
    let id = self.search(address).await.into_iter().next()?;
    self.fetch(&id).await
  }

  async fn fetch(&self, id: &str) -> Option<ReceivedMail> {
    let message: Value = self
      .http
      .get(format!("{MAILPIT_URL}/api/v1/message/{id}"))
      .send()
      .await
      .ok()?
      .json()
      .await
      .ok()?;
    let raw = self
      .http
      .get(format!("{MAILPIT_URL}/api/v1/message/{id}/raw"))
      .send()
      .await
      .ok()?
      .text()
      .await
      .ok()?;
    Some(ReceivedMail {
      id: id.to_owned(),
      subject: message["Subject"].as_str().unwrap_or_default().to_owned(),
      text: message["Text"].as_str().unwrap_or_default().to_owned(),
      raw,
    })
  }

  async fn search(&self, address: &str) -> Vec<String> {
    let found: Value = self
      .http
      .get(format!("{MAILPIT_URL}/api/v1/search"))
      .query(&[("query", format!("to:{address}")), ("limit", "500".to_owned())])
      .send()
      .await
      .expect("Mailpit answers")
      .json()
      .await
      .unwrap_or(Value::Null);
    found["messages"]
      .as_array()
      .map(|messages| {
        messages
          .iter()
          .filter_map(|message| message["ID"].as_str().map(str::to_owned))
          .collect()
      })
      .unwrap_or_default()
  }
}

impl Default for Mailpit {
  fn default() -> Self {
    Self::new()
  }
}
