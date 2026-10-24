//! Generating link previews before a message goes out (e2ee.md §12). A message with a link waits
//! in the outbox until its preview is made or given up on (bounded by the fetch timeouts).

#[cfg(not(target_arch = "wasm32"))]
use std::future::Future;
#[cfg(not(target_arch = "wasm32"))]
use std::pin::Pin;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use teamcraft_proto::{Base64Bytes, ClientMessageId, ConversationId, LinkPreviewPolicy};
use url::Url;

use super::Client;
use crate::api::ServerApi;
use crate::error::CoreError;
#[cfg(not(target_arch = "wasm32"))]
use crate::link_preview::Fetched;
use crate::link_preview::{PreviewFetcher, first_url, is_allowed, parse_page};
use crate::records;
use crate::store::{LinkPreviewData, LocalStore, MessageContent, Table};

/// Limits of preview fetching. Configurable; the defaults keep a slow site from holding a message
/// back for long.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkPreviewSettings {
  /// Per request (page, then image).
  pub timeout: Duration,
  /// Only the start of a page is read; metadata is in the head.
  pub max_page_bytes: usize,
  pub max_image_bytes: usize,
  pub max_redirects: usize,
}

/// A message waiting for its preview.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingPreview {
  pub conversation_id: ConversationId,
  pub client_message_id: ClientMessageId,
  pub url: String,
}

impl Default for LinkPreviewSettings {
  fn default() -> Self {
    Self {
      timeout: Duration::from_secs(5),
      max_page_bytes: 512 * 1024,
      max_image_bytes: 2 * 1024 * 1024,
      max_redirects: 3,
    }
  }
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Replaces how previews are fetched (tests, or an app with its own HTTP stack).
  pub fn set_preview_fetcher(&mut self, fetcher: Box<dyn PreviewFetcher>) {
    self.maybe_preview_fetcher = Some(fetcher);
  }

  /// Makes the previews of waiting messages; returns how many got one.
  pub async fn generate_pending_previews(&mut self) -> Result<usize, CoreError> {
    let pending: Vec<PendingPreview> = records::list_json(&self.store, Table::PendingPreviews)?;
    let policy = self.workspace_policy()?.link_previews;
    let mut made = 0;
    for preview in pending {
      if let Some(data) = self.make_preview(&preview.url, &policy).await
        && let Some(mut stored) = self
          .store
          .message(preview.conversation_id, preview.client_message_id)?
        && let MessageContent::Text { meta, .. } = &mut stored.content
      {
        meta.maybe_link_preview = Some(data);
        self.store.upsert_message(&stored)?;
        made += 1;
      }
      self.store.delete(
        Table::PendingPreviews,
        &records::outbox_key(preview.conversation_id, preview.client_message_id),
      )?;
    }
    Ok(made)
  }

  async fn make_preview(&self, url: &str, policy: &LinkPreviewPolicy) -> Option<LinkPreviewData> {
    let url = Url::parse(url).ok()?;
    let fetcher = self.maybe_preview_fetcher.as_ref()?;
    // Checked again here: the policy may have changed since the message was written.
    if !is_allowed(policy, &url) {
      return None;
    }
    let settings = &self.settings.link_previews;
    let page = fetcher.fetch(url.clone(), settings.max_page_bytes).await?;
    // A redirect to a disallowed domain gets no preview either.
    if !is_allowed(policy, &page.final_url) || !page.content_type.contains("html") {
      return None;
    }
    let meta = parse_page(&String::from_utf8_lossy(&page.body), &page.final_url);
    if meta.maybe_title.is_none() && meta.maybe_description.is_none() {
      return None;
    }
    let maybe_image = match meta.maybe_image_url {
      Some(image_url) => fetcher
        .fetch(image_url, settings.max_image_bytes)
        .await
        .and_then(|image| crate::thumbnail::thumbnail(&image.body, &image.content_type))
        .map(Base64Bytes::new),
      None => None,
    };
    Some(LinkPreviewData {
      url: url.to_string(),
      maybe_title: meta.maybe_title,
      maybe_description: meta.maybe_description,
      maybe_site_name: meta.maybe_site_name,
      maybe_image,
    })
  }
}

/// Holds a just-written message back for a preview if it links somewhere the policy allows.
pub fn queue_preview(
  store: &impl LocalStore,
  policy: &LinkPreviewPolicy,
  conversation: ConversationId,
  message: ClientMessageId,
  body: &str,
) -> Result<bool, CoreError> {
  let Some(url) = first_url(body).filter(|url| is_allowed(policy, url)) else {
    return Ok(false);
  };
  let pending = PendingPreview {
    conversation_id: conversation,
    client_message_id: message,
    url: url.to_string(),
  };
  records::put_json(
    store,
    Table::PendingPreviews,
    &records::outbox_key(conversation, message),
    &pending,
  )?;
  Ok(true)
}

pub fn has_pending_preview(
  store: &impl LocalStore,
  conversation: ConversationId,
  message: ClientMessageId,
) -> Result<bool, CoreError> {
  Ok(
    store
      .get(
        Table::PendingPreviews,
        &records::outbox_key(conversation, message),
      )?
      .is_some(),
  )
}

/// Fetches over HTTP with the configured timeout, redirect limit and size caps (native targets;
/// in the browser, cross-origin pages can't be read, so there are no previews).
#[cfg(not(target_arch = "wasm32"))]
pub struct HttpPreviewFetcher {
  client: reqwest::Client,
}

#[cfg(not(target_arch = "wasm32"))]
impl HttpPreviewFetcher {
  pub fn new(settings: &LinkPreviewSettings) -> Option<Self> {
    let client = reqwest::Client::builder()
      .timeout(settings.timeout)
      .redirect(reqwest::redirect::Policy::limited(settings.max_redirects))
      .user_agent("TeamCraft link preview")
      .build()
      .ok()?;
    Some(Self { client })
  }
}

#[cfg(not(target_arch = "wasm32"))]
impl PreviewFetcher for HttpPreviewFetcher {
  fn fetch(&self, url: Url, max_bytes: usize) -> Pin<Box<dyn Future<Output = Option<Fetched>> + '_>> {
    Box::pin(async move {
      let mut response = self.client.get(url).send().await.ok()?.error_for_status().ok()?;
      let final_url = response.url().clone();
      let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
      let is_page = content_type.contains("html");
      let mut body = Vec::new();
      while let Some(chunk) = response.chunk().await.ok()? {
        body.extend_from_slice(&chunk);
        if body.len() > max_bytes {
          if !is_page {
            // An image over the limit isn't used at all; a page is read only up to it.
            return None;
          }
          body.truncate(max_bytes);
          break;
        }
      }
      Some(Fetched {
        final_url,
        content_type,
        body,
      })
    })
  }
}

/// The fetcher a client starts with.
pub(crate) fn default_fetcher(settings: &LinkPreviewSettings) -> Option<Box<dyn PreviewFetcher>> {
  #[cfg(not(target_arch = "wasm32"))]
  {
    HttpPreviewFetcher::new(settings).map(|fetcher| Box::new(fetcher) as Box<dyn PreviewFetcher>)
  }
  #[cfg(target_arch = "wasm32")]
  {
    let _ = settings;
    None
  }
}
