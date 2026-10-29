//! The notetaker (e2ee.md §8.2, admin.md §3.6): an AI participant that transcribes a call and posts
//! a summary into the call's conversation. It sends the call's audio out of the end-to-end
//! encryption, so everything that decides whether and where audio goes is owner-signed policy
//! ([`NotetakerPolicy`]), and the notetaker joins a call only when a member asks it in with an
//! encrypted control message ([`NotetakerRequest`]), which the server can neither forge nor read.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;

use crate::UserId;
use crate::wire_enum::wire_enum;

/// Default OpenRouter model of the transcription: German and English among its languages, billed
/// per second of audio, with zero-data-retention endpoints (and EU ones) at its provider.
pub const DEFAULT_TRANSCRIPTION_MODEL: &str = "mistralai/voxtral-mini-transcribe-3-0";
/// Default OpenRouter chat model of the summary: a long context for the transcript of a long meeting.
pub const DEFAULT_SUMMARY_MODEL: &str = "mistralai/mistral-large-2512";
/// The `language` that lets the model detect the language of every piece itself.
pub const LANGUAGE_AUTO: &str = "auto";
/// Longest model slug the policy takes.
pub const MAX_MODEL_CHARS: usize = 128;
/// Longest summary prompt the policy takes.
pub const MAX_PROMPT_CHARS: usize = 8_000;
/// The summary instructions when the policy has none of its own. `{language}`, `{date}` and
/// `{participants}` are filled in; the transcript follows as the user's message.
pub const DEFAULT_SUMMARY_PROMPT: &str = "You write the minutes of a meeting from its transcript. \
Write in {language}. The meeting took place on {date}; the participants were {participants}.

Use exactly these sections, as Markdown headings:

## Summary
Three to six sentences: what the meeting was about and what came out of it.

## Decisions
One bullet per decision.

## Action items
One bullet per task: what, who (the owner, as named in the transcript) and when, if a date was said.

## Open questions
One bullet per question that stayed open.

Only write what the transcript says; never invent names, numbers or dates. Write \"None\" under a \
section that has nothing. The transcript comes from automatic speech recognition: words may be \
misheard, so prefer the meaning over the exact wording.";

/// The notetaker group of the signed workspace policy (e2ee.md §8.2). Off by default; clients and
/// the notetaker read it only from the policy they verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct NotetakerPolicy {
  /// The notetaker may be asked into calls.
  pub is_enabled: bool,
  /// The bot account that is the notetaker: the one clients show as such, the only one the server
  /// hands the API key to.
  #[serde(rename = "bot", skip_serializing_if = "Option::is_none")]
  pub maybe_bot: Option<UserId>,
  /// Who may ask it into a call.
  pub inviters: NotetakerInviters,
  /// Anyone in the conversation, guests included, may send it away (an objection is enough).
  pub can_anyone_stop: bool,
  /// OpenRouter model slug of the transcription (`POST /api/v1/audio/transcriptions`).
  pub transcription_model: String,
  /// OpenRouter model slug of the summary (`POST /api/v1/chat/completions`).
  pub summary_model: String,
  /// [`LANGUAGE_AUTO`] or an ISO 639-1 code (`de`, `en`): the language of transcript and summary.
  pub language: String,
  /// The summary instructions; `None` takes [`DEFAULT_SUMMARY_PROMPT`].
  #[serde(rename = "prompt", skip_serializing_if = "Option::is_none")]
  pub maybe_prompt: Option<String>,
  /// Requests go only to endpoints that keep no data and don't train on it.
  #[serde(rename = "zero_data_retention")]
  pub is_zero_data_retention: bool,
  /// SHA-256 (lower-case hex) of the OpenRouter API key the owner entered: the notetaker uses only
  /// a key with this hash, so a server can't swap in another account's key.
  #[serde(rename = "api_key_sha256", skip_serializing_if = "Option::is_none")]
  pub maybe_api_key_sha256: Option<String>,
}

wire_enum! {
  /// Who may ask the notetaker into a call.
  pub enum NotetakerInviters {
    /// Members of the conversation who aren't guests.
    Members => "members",
    /// Admins and owners of the verified roster.
    Admins => "admins",
  }
}

/// What a member asks of the notetaker: the plaintext of an encrypted, hidden control message in
/// the call's conversation (`MessageContent::Notetaker` in `teamcraft-core`). The conversation is
/// the one whose call it means; a request can never point elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum NotetakerRequest {
  /// Join the conversation's call.
  Join { scope: NotetakerScope },
  /// Leave the call and post what was heard so far; ends a standing [`NotetakerScope::EveryCall`].
  Stop,
  /// From a newer client.
  #[serde(other)]
  Unknown,
}

wire_enum! {
  /// Which calls a [`NotetakerRequest::Join`] means.
  pub enum NotetakerScope {
    /// The call running now, or one starting soon after the request.
    ThisCall => "this_call",
    /// Every call of the conversation until a [`NotetakerRequest::Stop`] (a meeting scheduled with
    /// "Notetaker joins").
    EveryCall => "every_call",
  }
}

/// Why a [`NotetakerPolicy`] can't be signed or published.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NotetakerPolicyError {
  #[error("an enabled notetaker needs its bot account")]
  MissingBot,
  #[error("an enabled notetaker needs the SHA-256 of its API key")]
  MissingKeyHash,
  #[error("the {field} is not an OpenRouter model slug")]
  InvalidModel { field: &'static str },
  #[error("the language is neither `auto` nor an ISO 639-1 code")]
  InvalidLanguage,
  #[error("the prompt is empty or longer than {MAX_PROMPT_CHARS} characters")]
  InvalidPrompt,
  #[error("the API key hash is not 64 lower-case hex digits")]
  InvalidKeyHash,
  #[error("unknown inviters `{0}`")]
  UnknownInviters(String),
}

impl Default for NotetakerPolicy {
  fn default() -> Self {
    Self {
      is_enabled: false,
      maybe_bot: None,
      inviters: NotetakerInviters::Members,
      can_anyone_stop: true,
      transcription_model: DEFAULT_TRANSCRIPTION_MODEL.to_owned(),
      summary_model: DEFAULT_SUMMARY_MODEL.to_owned(),
      language: LANGUAGE_AUTO.to_owned(),
      maybe_prompt: None,
      is_zero_data_retention: true,
      maybe_api_key_sha256: None,
    }
  }
}

impl NotetakerPolicy {
  /// Whether the notetaker is on and has its account.
  pub fn active_bot(&self) -> Option<UserId> {
    self.maybe_bot.filter(|_| self.is_enabled)
  }

  /// Whether `user` is the notetaker of an enabled policy.
  pub fn is_notetaker(&self, user: UserId) -> bool {
    self.active_bot() == Some(user)
  }

  /// The ISO 639-1 code to hand the models, `None` for automatic detection.
  pub fn maybe_language_code(&self) -> Option<&str> {
    let language = self.language.trim();
    (!language.is_empty() && language != LANGUAGE_AUTO).then_some(language)
  }

  /// The summary instructions in force.
  pub fn prompt(&self) -> &str {
    self.maybe_prompt.as_deref().unwrap_or(DEFAULT_SUMMARY_PROMPT)
  }

  /// Checks what owners' clients sign and the server accepts. A disabled policy may stay
  /// incomplete, but what it holds must still be well-formed.
  pub fn check(&self) -> Result<(), NotetakerPolicyError> {
    if let NotetakerInviters::Unknown(other) = &self.inviters {
      return Err(NotetakerPolicyError::UnknownInviters(other.clone()));
    }
    if !is_model_slug(&self.transcription_model) {
      return Err(NotetakerPolicyError::InvalidModel {
        field: "transcription model",
      });
    }
    if !is_model_slug(&self.summary_model) {
      return Err(NotetakerPolicyError::InvalidModel {
        field: "summary model",
      });
    }
    if !is_language(&self.language) {
      return Err(NotetakerPolicyError::InvalidLanguage);
    }
    if let Some(prompt) = &self.maybe_prompt
      && (prompt.trim().is_empty() || prompt.chars().count() > MAX_PROMPT_CHARS)
    {
      return Err(NotetakerPolicyError::InvalidPrompt);
    }
    if let Some(hash) = &self.maybe_api_key_sha256
      && !is_sha256_hex(hash)
    {
      return Err(NotetakerPolicyError::InvalidKeyHash);
    }
    if self.is_enabled && self.maybe_bot.is_none() {
      return Err(NotetakerPolicyError::MissingBot);
    }
    if self.is_enabled && self.maybe_api_key_sha256.is_none() {
      return Err(NotetakerPolicyError::MissingKeyHash);
    }
    Ok(())
  }
}

/// An OpenRouter model slug: `author/name`, optionally with a `:variant`, in the characters
/// OpenRouter uses.
fn is_model_slug(slug: &str) -> bool {
  let Some((author, name)) = slug.split_once('/') else {
    return false;
  };
  let allowed = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.');
  slug.len() <= MAX_MODEL_CHARS
    && !author.is_empty()
    && !name.is_empty()
    && author.chars().all(allowed)
    && name.chars().all(|c| allowed(c) || c == ':')
}

/// `auto`, or a two- or three-letter lower-case language code.
fn is_language(language: &str) -> bool {
  language == LANGUAGE_AUTO
    || ((2..=3).contains(&language.len()) && language.chars().all(|c| c.is_ascii_lowercase()))
}

fn is_sha256_hex(hash: &str) -> bool {
  hash.len() == 64
    && hash
      .chars()
      .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{Id, Snowflake};

  const HASH: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

  mod wire {
    use super::*;

    #[test]
    fn the_default_policy_is_off_with_the_documented_defaults() {
      let json = serde_json::to_string(&NotetakerPolicy::default()).unwrap();
      assert_eq!(
        json,
        r#"{"is_enabled":false,"inviters":"members","can_anyone_stop":true,"transcription_model":"mistralai/voxtral-mini-transcribe-3-0","summary_model":"mistralai/mistral-large-2512","language":"auto","zero_data_retention":true}"#
      );
    }

    #[test]
    fn a_partial_or_empty_group_takes_the_defaults() {
      let policy: NotetakerPolicy = serde_json::from_str(r#"{"is_enabled":true,"bot":"42"}"#).unwrap();
      assert!(policy.is_enabled);
      assert_eq!(policy.maybe_bot, Some(user(42)));
      assert!(policy.is_zero_data_retention && policy.can_anyone_stop);
      assert_eq!(
        serde_json::from_str::<NotetakerPolicy>("{}").unwrap(),
        NotetakerPolicy::default()
      );
    }

    #[test]
    fn requests_have_a_stable_form_and_unknown_ones_survive() {
      let join = NotetakerRequest::Join {
        scope: NotetakerScope::ThisCall,
      };
      assert_eq!(
        serde_json::to_string(&join).unwrap(),
        r#"{"action":"join","scope":"this_call"}"#
      );
      assert_eq!(
        serde_json::to_string(&NotetakerRequest::Stop).unwrap(),
        r#"{"action":"stop"}"#
      );
      let every: NotetakerRequest =
        serde_json::from_str(r#"{"action":"join","scope":"every_call"}"#).unwrap();
      assert_eq!(
        every,
        NotetakerRequest::Join {
          scope: NotetakerScope::EveryCall
        }
      );
      let newer: NotetakerRequest = serde_json::from_str(r#"{"action":"pause","for":"5m"}"#).unwrap();
      assert_eq!(newer, NotetakerRequest::Unknown);
      let scope: NotetakerRequest = serde_json::from_str(r#"{"action":"join","scope":"next_week"}"#).unwrap();
      assert_eq!(
        scope,
        NotetakerRequest::Join {
          scope: NotetakerScope::Unknown("next_week".into())
        }
      );
    }
  }

  mod check {
    use super::*;

    #[test]
    fn an_enabled_policy_needs_its_bot_and_key_hash() {
      let mut policy = enabled();
      assert_eq!(policy.check(), Ok(()));
      policy.maybe_bot = None;
      assert_eq!(policy.check(), Err(NotetakerPolicyError::MissingBot));
      policy = enabled();
      policy.maybe_api_key_sha256 = None;
      assert_eq!(policy.check(), Err(NotetakerPolicyError::MissingKeyHash));
      assert_eq!(NotetakerPolicy::default().check(), Ok(()));
    }

    #[test]
    fn malformed_fields_are_refused() {
      let cases: [(fn(&mut NotetakerPolicy), NotetakerPolicyError); 6] = [
        (
          |p| p.transcription_model = "whisper".into(),
          NotetakerPolicyError::InvalidModel {
            field: "transcription model",
          },
        ),
        (
          |p| p.summary_model = "a/b c".into(),
          NotetakerPolicyError::InvalidModel {
            field: "summary model",
          },
        ),
        (
          |p| p.language = "German".into(),
          NotetakerPolicyError::InvalidLanguage,
        ),
        (
          |p| p.maybe_prompt = Some("  ".into()),
          NotetakerPolicyError::InvalidPrompt,
        ),
        (
          |p| p.maybe_api_key_sha256 = Some(HASH.to_uppercase()),
          NotetakerPolicyError::InvalidKeyHash,
        ),
        (
          |p| p.inviters = NotetakerInviters::Unknown("everyone".into()),
          NotetakerPolicyError::UnknownInviters("everyone".into()),
        ),
      ];
      for (change, error) in cases {
        let mut policy = enabled();
        change(&mut policy);
        assert_eq!(policy.check(), Err(error));
      }
      let mut long = enabled();
      long.maybe_prompt = Some("x".repeat(MAX_PROMPT_CHARS + 1));
      assert_eq!(long.check(), Err(NotetakerPolicyError::InvalidPrompt));
    }

    #[test]
    fn model_slugs_with_variants_and_language_codes_pass() {
      let mut policy = enabled();
      policy.summary_model = "google/gemini-3.5-flash:batch".into();
      policy.language = "de".into();
      assert_eq!(policy.check(), Ok(()));
      assert_eq!(policy.maybe_language_code(), Some("de"));
      assert_eq!(NotetakerPolicy::default().maybe_language_code(), None);
    }

    #[test]
    fn only_the_enabled_policys_bot_is_the_notetaker() {
      let policy = enabled();
      assert!(policy.is_notetaker(user(7)));
      assert!(!policy.is_notetaker(user(8)));
      let off = NotetakerPolicy {
        is_enabled: false,
        ..enabled()
      };
      assert!(!off.is_notetaker(user(7)));
      assert_eq!(off.active_bot(), None);
    }
  }

  fn enabled() -> NotetakerPolicy {
    NotetakerPolicy {
      is_enabled: true,
      maybe_bot: Some(user(7)),
      maybe_api_key_sha256: Some(HASH.into()),
      ..NotetakerPolicy::default()
    }
  }

  fn user(id: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(id).unwrap())
  }
}
