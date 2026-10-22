//! Meetings: the calendar side (CalDAV), iCalendar rendering and the settings the service needs.
//! The business logic is in `services::meetings`.

pub mod caldav;
pub mod guest;
pub mod ical;
pub mod mailer;

use std::sync::Arc;
use std::time::Duration;

use secrecy::ExposeSecret;
use subtle::ConstantTimeEq;
use teamcraft_proto::MeetingId;
use url::Url;

use crate::config::{MailTexts, MeetingsConfig};
use caldav::CalendarStore;
use guest::{GuestTokenError, GuestTokens};
use mailer::Mailer;

/// Configuration and backends of the meeting feature.
pub struct MeetingService {
  base_url: String,
  /// Host part of the public base URL; names the iCalendar UIDs.
  host: String,
  organizer_fallback_address: String,
  max_invitees: usize,
  maybe_calendar: Option<Arc<dyn CalendarStore>>,
  maybe_mailer: Option<Arc<dyn Mailer>>,
  texts: MailTexts,
  tokens: GuestTokens,
  guest_grace: Duration,
  /// Token of the inbox handler that forwards RSVP e-mail; `None` disables that endpoint.
  maybe_inbox_token: Option<Vec<u8>>,
}

/// The external systems a meeting service talks to; each is optional.
#[derive(Default)]
pub struct MeetingBackends {
  pub maybe_calendar: Option<Arc<dyn CalendarStore>>,
  pub maybe_mailer: Option<Arc<dyn Mailer>>,
}

/// Why the meeting feature can't be set up from the configuration.
#[derive(Debug, thiserror::Error)]
pub enum MeetingsSetupError {
  #[error("meetings.public_base_url is not an absolute http(s) URL: {0}")]
  InvalidBaseUrl(String),
  #[error("meetings.guest_token_secret is unusable: {0}")]
  GuestTokenSecret(#[from] GuestTokenError),
}

impl MeetingService {
  pub fn new(config: &MeetingsConfig, backends: MeetingBackends) -> Result<Self, MeetingsSetupError> {
    let url = Url::parse(&config.public_base_url)
      .map_err(|error| MeetingsSetupError::InvalidBaseUrl(error.to_string()))?;
    let host = match (url.scheme(), url.host_str()) {
      ("http" | "https", Some(host)) => host.to_owned(),
      _ => return Err(MeetingsSetupError::InvalidBaseUrl(config.public_base_url.clone())),
    };
    Ok(Self {
      base_url: config.public_base_url.trim_end_matches('/').to_owned(),
      host,
      organizer_fallback_address: config.organizer_fallback_address.clone(),
      max_invitees: config.max_invitees,
      maybe_calendar: backends.maybe_calendar,
      maybe_mailer: backends.maybe_mailer,
      texts: config.mail_texts.clone(),
      tokens: GuestTokens::new(&config.guest_token_secret)?,
      guest_grace: Duration::from_secs(config.guest_grace_secs),
      maybe_inbox_token: config
        .maybe_inbox_token
        .as_ref()
        .map(|token| token.expose_secret().as_bytes().to_vec()),
    })
  }

  /// The link members open to join; guests get personal links with a token.
  pub fn join_url(&self, meeting: MeetingId) -> String {
    format!("{}/meeting/{meeting}", self.base_url)
  }

  /// The iCalendar UID of a meeting.
  pub fn uid(&self, meeting: MeetingId) -> String {
    format!("{meeting}@{}", self.host)
  }

  pub fn max_invitees(&self) -> usize {
    self.max_invitees
  }

  pub fn organizer_fallback_address(&self) -> &str {
    &self.organizer_fallback_address
  }

  pub fn calendar(&self) -> Option<&Arc<dyn CalendarStore>> {
    self.maybe_calendar.as_ref()
  }

  pub fn mailer(&self) -> Option<&Arc<dyn Mailer>> {
    self.maybe_mailer.as_ref()
  }

  pub fn texts(&self) -> &MailTexts {
    &self.texts
  }

  pub fn tokens(&self) -> &GuestTokens {
    &self.tokens
  }

  /// How long after the end of a meeting guests may still use their link and session.
  pub fn guest_grace(&self) -> Duration {
    self.guest_grace
  }

  /// A personal join link: the token is in the fragment, which browsers never send to servers.
  pub fn guest_join_url(&self, meeting: MeetingId, token: &str) -> String {
    format!("{}#invite={token}", self.join_url(meeting))
  }

  /// Whether `presented` is the inbox handler's token (constant time); `None` if the endpoint is
  /// not configured.
  pub fn check_inbox_token(&self, presented: &[u8]) -> Option<bool> {
    self
      .maybe_inbox_token
      .as_deref()
      .map(|expected| bool::from(expected.ct_eq(presented)))
  }
}
