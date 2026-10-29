//! OpenRouter's API, as far as the notetaker uses it (e2ee.md §8.2): speech to text
//! (`POST /audio/transcriptions`) and a chat completion for the summary
//! (`POST /chat/completions`). Rate limits, timeouts, server errors and lost connections are
//! retried with backoff (honouring `Retry-After`); a refused key or an empty account are not.
//! Every answer's `usage` (seconds, tokens, cost) is added up so the costs of a call can be logged.
//!
//! With zero data retention (signed policy, default on) every request carries the provider
//! preferences `zdr: true` and `data_collection: "deny"`. The API key is never logged.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use reqwest::header::{HeaderMap, RETRY_AFTER};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tracing::{debug, warn};

/// OpenRouter's API.
pub const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
/// Error text kept from an answer, at most.
const MAX_ERROR_CHARS: usize = 300;

/// A client for one API key.
pub struct OpenRouterClient {
  http: reqwest::Client,
  base_url: String,
  api_key: SecretString,
  retry: RetrySettings,
  maybe_app_title: Option<String>,
}

/// How failed requests are repeated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetrySettings {
  /// Attempts after the first.
  pub max_retries: u32,
  /// Wait before the first retry; doubles with each one.
  pub base_delay: Duration,
  /// Longest wait, also for a server's `Retry-After`.
  pub max_delay: Duration,
}

/// One piece of audio to transcribe.
#[derive(Debug, Clone, Copy)]
pub struct TranscriptionRequest<'a> {
  pub model: &'a str,
  /// A WAV file.
  pub wav: &'a [u8],
  /// ISO 639-1; `None` lets the model detect it.
  pub maybe_language: Option<&'a str>,
  pub is_zero_data_retention: bool,
}

/// A chat completion with one instruction and one input.
#[derive(Debug, Clone, Copy)]
pub struct ChatRequest<'a> {
  pub model: &'a str,
  pub system: &'a str,
  pub user: &'a str,
  pub max_tokens: u32,
  pub is_zero_data_retention: bool,
}

/// What a request cost, as OpenRouter reports it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
  pub requests: u32,
  pub audio_seconds: f64,
  pub input_tokens: u64,
  pub output_tokens: u64,
  /// In US dollars.
  pub cost_usd: f64,
}

/// A transcribed piece.
#[derive(Debug, Clone, PartialEq)]
pub struct Transcription {
  pub text: String,
  pub usage: Usage,
}

/// A chat answer.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatAnswer {
  pub text: String,
  pub usage: Usage,
}

#[derive(Debug, Error)]
pub enum OpenRouterError {
  /// 401: the key is wrong or revoked.
  #[error("OpenRouter refused the API key")]
  Unauthorized,
  /// 402: the account has no credits left.
  #[error("the OpenRouter account has no credits left")]
  PaymentRequired,
  #[error("OpenRouter answered {status}: {message}")]
  Status { status: u16, message: String },
  #[error("OpenRouter could not be reached: {0}")]
  Network(String),
  #[error("unreadable answer from OpenRouter: {0}")]
  Decode(String),
}

/// `provider` preferences of a request.
#[derive(Debug, Serialize)]
struct ProviderPreferences {
  zdr: bool,
  data_collection: &'static str,
}

#[derive(Serialize)]
struct TranscriptionBody<'a> {
  model: &'a str,
  input_audio: InputAudio,
  #[serde(rename = "language", skip_serializing_if = "Option::is_none")]
  maybe_language: Option<&'a str>,
  temperature: f32,
  #[serde(rename = "provider", skip_serializing_if = "Option::is_none")]
  maybe_provider: Option<ProviderPreferences>,
}

#[derive(Serialize)]
struct InputAudio {
  data: String,
  format: &'static str,
}

#[derive(Serialize)]
struct ChatBody<'a> {
  model: &'a str,
  messages: [ChatMessage<'a>; 2],
  temperature: f32,
  max_tokens: u32,
  #[serde(rename = "provider", skip_serializing_if = "Option::is_none")]
  maybe_provider: Option<ProviderPreferences>,
  usage: UsageRequest,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
  role: &'static str,
  content: &'a str,
}

#[derive(Serialize)]
struct UsageRequest {
  include: bool,
}

#[derive(Deserialize)]
struct TranscriptionAnswer {
  text: String,
  #[serde(default)]
  usage: Option<UsageAnswer>,
}

#[derive(Deserialize)]
struct ChatCompletionAnswer {
  #[serde(default)]
  choices: Vec<ChatChoice>,
  #[serde(default)]
  usage: Option<UsageAnswer>,
}

#[derive(Deserialize)]
struct ChatChoice {
  message: ChatChoiceMessage,
}

#[derive(Deserialize)]
struct ChatChoiceMessage {
  #[serde(default)]
  content: Option<String>,
}

/// The fields of `usage` the two endpoints report (each reports some).
#[derive(Deserialize, Default)]
struct UsageAnswer {
  #[serde(default)]
  seconds: Option<f64>,
  #[serde(default)]
  input_tokens: Option<u64>,
  #[serde(default)]
  output_tokens: Option<u64>,
  #[serde(default)]
  prompt_tokens: Option<u64>,
  #[serde(default)]
  completion_tokens: Option<u64>,
  #[serde(default)]
  cost: Option<f64>,
}

impl OpenRouterClient {
  pub fn new(
    base_url: &str,
    api_key: SecretString,
    request_timeout: Duration,
    retry: RetrySettings,
    maybe_app_title: Option<String>,
  ) -> Result<Self, OpenRouterError> {
    let http = reqwest::Client::builder()
      .timeout(request_timeout)
      .build()
      .map_err(|error| OpenRouterError::Network(error.to_string()))?;
    Ok(Self {
      http,
      base_url: base_url.trim_end_matches('/').to_owned(),
      api_key,
      retry,
      maybe_app_title: maybe_app_title.filter(|title| !title.is_empty()),
    })
  }

  /// Transcribes one piece of audio.
  pub async fn transcribe(
    &self,
    request: TranscriptionRequest<'_>,
  ) -> Result<Transcription, OpenRouterError> {
    let body = TranscriptionBody {
      model: request.model,
      input_audio: InputAudio {
        data: STANDARD.encode(request.wav),
        format: "wav",
      },
      maybe_language: request.maybe_language,
      temperature: 0.0,
      maybe_provider: provider(request.is_zero_data_retention),
    };
    let answer: TranscriptionAnswer = self.post("audio/transcriptions", &body).await?;
    Ok(Transcription {
      text: answer.text.trim().to_owned(),
      usage: Usage::from_answer(answer.usage),
    })
  }

  /// One chat completion.
  pub async fn complete(&self, request: ChatRequest<'_>) -> Result<ChatAnswer, OpenRouterError> {
    let body = ChatBody {
      model: request.model,
      messages: [
        ChatMessage {
          role: "system",
          content: request.system,
        },
        ChatMessage {
          role: "user",
          content: request.user,
        },
      ],
      temperature: 0.2,
      max_tokens: request.max_tokens,
      maybe_provider: provider(request.is_zero_data_retention),
      usage: UsageRequest { include: true },
    };
    let answer: ChatCompletionAnswer = self.post("chat/completions", &body).await?;
    let text = answer
      .choices
      .into_iter()
      .find_map(|choice| choice.message.content)
      .map(|text| text.trim().to_owned())
      .filter(|text| !text.is_empty())
      .ok_or_else(|| OpenRouterError::Decode("the answer has no text".into()))?;
    Ok(ChatAnswer {
      text,
      usage: Usage::from_answer(answer.usage),
    })
  }

  /// Posts `body`, repeating what is worth repeating.
  async fn post<B: Serialize, T: for<'de> Deserialize<'de>>(
    &self,
    path: &str,
    body: &B,
  ) -> Result<T, OpenRouterError> {
    let url = format!("{}/{path}", self.base_url);
    let mut attempt = 0;
    loop {
      let (result, maybe_retry_after) = self.post_once(&url, body).await;
      let error = match result {
        Ok(value) => return Ok(value),
        Err(error) => error,
      };
      if attempt >= self.retry.max_retries || !error.is_retryable() {
        return Err(error);
      }
      let delay = self.retry.delay(attempt, maybe_retry_after);
      warn!(%error, path, attempt = attempt + 1, ?delay, "an OpenRouter request failed; retrying");
      tokio::time::sleep(delay).await;
      attempt += 1;
    }
  }

  async fn post_once<B: Serialize, T: for<'de> Deserialize<'de>>(
    &self,
    url: &str,
    body: &B,
  ) -> (Result<T, OpenRouterError>, Option<Duration>) {
    let mut request = self
      .http
      .post(url)
      .bearer_auth(self.api_key.expose_secret())
      .json(body);
    if let Some(title) = &self.maybe_app_title {
      request = request.header("X-Title", title);
    }
    let response = match request.send().await {
      Ok(response) => response,
      Err(error) => return (Err(OpenRouterError::Network(network_error(&error))), None),
    };
    let status = response.status();
    let maybe_retry_after = retry_after(response.headers());
    let text = match response.text().await {
      Ok(text) => text,
      Err(error) => return (Err(OpenRouterError::Network(network_error(&error))), None),
    };
    if !status.is_success() {
      return (Err(status_error(status.as_u16(), &text)), maybe_retry_after);
    }
    // OpenRouter reports some upstream failures inside a 200 answer.
    if let Ok(value) = serde_json::from_str::<Value>(&text)
      && let Some(error) = value.get("error")
    {
      let status = error.get("code").and_then(Value::as_u64).unwrap_or(502);
      return (
        Err(status_error(u16::try_from(status).unwrap_or(502), &text)),
        maybe_retry_after,
      );
    }
    let parsed = serde_json::from_str(&text).map_err(|error| OpenRouterError::Decode(error.to_string()));
    (parsed, None)
  }
}

impl OpenRouterError {
  /// Worth another attempt: rate limits, timeouts, server and provider errors, lost connections.
  pub fn is_retryable(&self) -> bool {
    match self {
      Self::Network(_) => true,
      Self::Status { status, .. } => matches!(*status, 408 | 425 | 429) || *status >= 500,
      Self::Unauthorized | Self::PaymentRequired | Self::Decode(_) => false,
    }
  }
}

impl RetrySettings {
  /// The wait before retry number `attempt` (from 0): the server's `Retry-After` if it gave one,
  /// else the base delay doubled per attempt; never longer than the maximum.
  fn delay(&self, attempt: u32, maybe_retry_after: Option<Duration>) -> Duration {
    let backoff = self.base_delay.saturating_mul(1u32 << attempt.min(16));
    maybe_retry_after.unwrap_or(backoff).min(self.max_delay)
  }
}

impl Usage {
  pub fn add(&mut self, other: &Self) {
    self.requests += other.requests;
    self.audio_seconds += other.audio_seconds;
    self.input_tokens += other.input_tokens;
    self.output_tokens += other.output_tokens;
    self.cost_usd += other.cost_usd;
  }

  fn from_answer(maybe_usage: Option<UsageAnswer>) -> Self {
    let usage = maybe_usage.unwrap_or_default();
    Self {
      requests: 1,
      audio_seconds: usage.seconds.unwrap_or(0.0),
      input_tokens: usage.input_tokens.or(usage.prompt_tokens).unwrap_or(0),
      output_tokens: usage.output_tokens.or(usage.completion_tokens).unwrap_or(0),
      cost_usd: usage.cost.unwrap_or(0.0),
    }
  }
}

fn provider(is_zero_data_retention: bool) -> Option<ProviderPreferences> {
  is_zero_data_retention.then_some(ProviderPreferences {
    zdr: true,
    data_collection: "deny",
  })
}

fn status_error(status: u16, body: &str) -> OpenRouterError {
  match status {
    401 => OpenRouterError::Unauthorized,
    402 => OpenRouterError::PaymentRequired,
    _ => OpenRouterError::Status {
      status,
      message: error_message(body),
    },
  }
}

/// The `error.message` of an error answer, else the start of its text.
fn error_message(body: &str) -> String {
  let message = serde_json::from_str::<Value>(body)
    .ok()
    .and_then(|value| {
      value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .map(str::to_owned)
    })
    .unwrap_or_else(|| body.to_owned());
  message.chars().take(MAX_ERROR_CHARS).collect()
}

/// `Retry-After` in seconds (an HTTP date is rare here and treated as absent).
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
  headers
    .get(RETRY_AFTER)?
    .to_str()
    .ok()?
    .trim()
    .parse::<u64>()
    .ok()
    .map(Duration::from_secs)
}

/// A network error without the URL (which holds no secret, but the reason is what matters).
fn network_error(error: &reqwest::Error) -> String {
  let kind = if error.is_timeout() {
    "timeout"
  } else if error.is_connect() {
    "connect"
  } else {
    "request"
  };
  debug!(%error, "OpenRouter request error");
  format!("{kind} error")
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::test_support::{FakeOpenRouter, Reply};

  mod transcription {
    use super::*;

    #[tokio::test]
    async fn sends_the_audio_language_and_data_policy_and_reads_the_usage() {
      let server = FakeOpenRouter::start(vec![Reply::json(
        200,
        r#"{"text":" Guten Morgen. ","usage":{"seconds":2.5,"cost":0.00015}}"#,
      )])
      .await;
      let client = server.client(3);
      let transcription = client
        .transcribe(TranscriptionRequest {
          model: "mistralai/voxtral-mini-transcribe-3-0",
          wav: b"RIFF....",
          maybe_language: Some("de"),
          is_zero_data_retention: true,
        })
        .await
        .unwrap();
      assert_eq!(transcription.text, "Guten Morgen.");
      assert_eq!(transcription.usage.audio_seconds, 2.5);
      assert_eq!(transcription.usage.cost_usd, 0.00015);
      let request = server.requests().remove(0);
      assert_eq!(request.path, "/api/v1/audio/transcriptions");
      assert_eq!(request.authorization.as_deref(), Some("Bearer sk-or-v1-test-key"));
      assert_eq!(request.title.as_deref(), Some("TeamCraft notetaker"));
      assert_eq!(request.body["model"], "mistralai/voxtral-mini-transcribe-3-0");
      assert_eq!(request.body["input_audio"]["format"], "wav");
      assert_eq!(request.body["input_audio"]["data"], STANDARD.encode(b"RIFF...."));
      assert_eq!(request.body["language"], "de");
      assert_eq!(request.body["provider"]["zdr"], true);
      assert_eq!(request.body["provider"]["data_collection"], "deny");
    }

    #[tokio::test]
    async fn without_zero_data_retention_or_language_neither_is_sent() {
      let server = FakeOpenRouter::start(vec![Reply::json(200, r#"{"text":"Hello"}"#)]).await;
      server
        .client(0)
        .transcribe(TranscriptionRequest {
          model: "openai/whisper-large-v3",
          wav: b"x",
          maybe_language: None,
          is_zero_data_retention: false,
        })
        .await
        .unwrap();
      let body = &server.requests()[0].body;
      assert!(body.get("provider").is_none() && body.get("language").is_none());
    }
  }

  mod retries {
    use super::*;

    #[tokio::test]
    async fn rate_limits_and_server_errors_are_retried() {
      let server = FakeOpenRouter::start(vec![
        Reply::json(429, r#"{"error":{"message":"slow down","code":429}}"#).retry_after(0),
        Reply::json(503, "upstream down"),
        Reply::json(200, r#"{"text":"ok"}"#),
      ])
      .await;
      let text = server.client(3).transcribe(piece()).await.unwrap().text;
      assert_eq!((text.as_str(), server.requests().len()), ("ok", 3));
    }

    #[tokio::test]
    async fn an_error_inside_a_200_answer_is_an_error() {
      let server = FakeOpenRouter::start(vec![
        Reply::json(200, r#"{"error":{"message":"provider timed out","code":504}}"#),
        Reply::json(200, r#"{"text":"ok"}"#),
      ])
      .await;
      assert_eq!(server.client(1).transcribe(piece()).await.unwrap().text, "ok");
    }

    #[tokio::test]
    async fn retries_end_and_the_last_error_comes_back() {
      let server = FakeOpenRouter::start(vec![Reply::json(500, "boom"); 3]).await;
      let error = server.client(2).transcribe(piece()).await.unwrap_err();
      assert!(matches!(error, OpenRouterError::Status { status: 500, .. }));
      assert_eq!(server.requests().len(), 3);
    }

    #[tokio::test]
    async fn a_refused_key_an_empty_account_and_bad_requests_are_not_retried() {
      for (status, expected) in [(401, "key"), (402, "credits"), (400, "400")] {
        let server = FakeOpenRouter::start(vec![Reply::json(
          status,
          r#"{"error":{"message":"no","code":0}}"#,
        )])
        .await;
        let error = server.client(3).transcribe(piece()).await.unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
        assert_eq!(server.requests().len(), 1);
      }
    }

    #[test]
    fn the_delay_doubles_and_follows_retry_after_within_the_maximum() {
      let settings = RetrySettings {
        max_retries: 5,
        base_delay: Duration::from_secs(1),
        max_delay: Duration::from_secs(10),
      };
      assert_eq!(settings.delay(0, None), Duration::from_secs(1));
      assert_eq!(settings.delay(2, None), Duration::from_secs(4));
      assert_eq!(settings.delay(5, None), Duration::from_secs(10));
      assert_eq!(
        settings.delay(0, Some(Duration::from_secs(7))),
        Duration::from_secs(7)
      );
      assert_eq!(
        settings.delay(0, Some(Duration::from_secs(70))),
        Duration::from_secs(10)
      );
    }
  }

  mod chat {
    use super::*;

    #[tokio::test]
    async fn sends_both_messages_and_reads_the_answer_and_its_cost() {
      let server = FakeOpenRouter::start(vec![Reply::json(
        200,
        r###"{"choices":[{"message":{"role":"assistant","content":"## Summary\nDone."}}],
            "usage":{"prompt_tokens":1200,"completion_tokens":80,"cost":0.0007}}"###,
      )])
      .await;
      let answer = server
        .client(0)
        .complete(ChatRequest {
          model: "mistralai/mistral-large-2512",
          system: "Write minutes.",
          user: "[00:00:01] Alice: Hello",
          max_tokens: 500,
          is_zero_data_retention: true,
        })
        .await
        .unwrap();
      assert_eq!(answer.text, "## Summary\nDone.");
      assert_eq!(
        (answer.usage.input_tokens, answer.usage.output_tokens),
        (1200, 80)
      );
      let request = server.requests().remove(0);
      assert_eq!(request.path, "/api/v1/chat/completions");
      assert_eq!(request.body["messages"][0]["role"], "system");
      assert_eq!(request.body["messages"][1]["content"], "[00:00:01] Alice: Hello");
      assert_eq!(request.body["max_tokens"], 500);
      assert_eq!(request.body["provider"]["zdr"], true);
    }

    #[tokio::test]
    async fn an_answer_without_text_is_an_error() {
      let server = FakeOpenRouter::start(vec![Reply::json(200, r#"{"choices":[]}"#)]).await;
      let error = server
        .client(0)
        .complete(ChatRequest {
          model: "a/b",
          system: "s",
          user: "u",
          max_tokens: 10,
          is_zero_data_retention: false,
        })
        .await
        .unwrap_err();
      assert!(matches!(error, OpenRouterError::Decode(_)));
    }
  }

  #[test]
  fn usage_adds_up() {
    let mut total = Usage::default();
    let piece = Usage {
      requests: 1,
      audio_seconds: 10.0,
      input_tokens: 0,
      output_tokens: 0,
      cost_usd: 0.001,
    };
    total.add(&piece);
    total.add(&piece);
    assert_eq!((total.requests, total.audio_seconds), (2, 20.0));
    assert!((total.cost_usd - 0.002).abs() < 1e-12);
  }

  fn piece() -> TranscriptionRequest<'static> {
    TranscriptionRequest {
      model: "a/b",
      wav: b"x",
      maybe_language: None,
      is_zero_data_retention: true,
    }
  }
}
