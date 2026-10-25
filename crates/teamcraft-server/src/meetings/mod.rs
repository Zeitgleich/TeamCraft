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

use crate::config::MeetingsConfig;
use crate::runtime::Slot;
use caldav::CalendarStore;
use guest::{GuestTokenError, GuestTokens};
use mailer::Mailer;

/// Configuration and backends of the meeting feature.
pub struct MeetingService {
  base_url: String,
  /// Host part of the public base URL; names the iCalendar UIDs.
  host: String,
  organizer_fallback_address: String,
  calendar: Arc<Slot<dyn CalendarStore>>,
  mailer: Arc<Slot<dyn Mailer>>,
  tokens: GuestTokens,
  guest_grace: Duration,
  max_shared_link_guests: u32,
  /// Token of the inbox handler that forwards RSVP e-mail; `None` disables that endpoint.
  maybe_inbox_token: Option<Vec<u8>>,
}

/// The external systems a meeting service talks to; each may be empty, and admins can replace
/// them while the server runs (admin.md §6).
#[derive(Default)]
pub struct MeetingBackends {
  pub calendar: Arc<Slot<dyn CalendarStore>>,
  pub mailer: Arc<Slot<dyn Mailer>>,
}

/// Why the meeting feature can't be set up from the configuration.
#[derive(Debug, thiserror::Error)]
pub enum MeetingsSetupError {
  #[error("meetings.public_base_url is not an absolute http(s) URL: {0}")]
  InvalidBaseUrl(String),
  #[error("meetings.guest_token_secret is unusable: {0}")]
  GuestTokenSecret(#[from] GuestTokenError),
}

impl MeetingBackends {
  /// Backends that start out as given.
  pub fn fixed(
    maybe_calendar: Option<Arc<dyn CalendarStore>>,
    maybe_mailer: Option<Arc<dyn Mailer>>,
  ) -> Self {
    Self {
      calendar: Arc::new(Slot::new(maybe_calendar)),
      mailer: Arc::new(Slot::new(maybe_mailer)),
    }
  }
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
      calendar: backends.calendar,
      mailer: backends.mailer,
      tokens: GuestTokens::new(&config.guest_token_secret)?,
      guest_grace: Duration::from_secs(config.guest_grace_secs),
      max_shared_link_guests: config.max_shared_link_guests,
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

  pub fn organizer_fallback_address(&self) -> &str {
    &self.organizer_fallback_address
  }

  /// The calendar meetings are written to now, if any.
  pub fn calendar(&self) -> Option<Arc<dyn CalendarStore>> {
    self.calendar.get()
  }

  /// Where admins put a new calendar.
  pub fn calendar_slot(&self) -> &Arc<Slot<dyn CalendarStore>> {
    &self.calendar
  }

  pub fn mailer(&self) -> Option<Arc<dyn Mailer>> {
    self.mailer.get()
  }

  pub fn tokens(&self) -> &GuestTokens {
    &self.tokens
  }

  /// How long after the end of a meeting guests may still use their link and session.
  pub fn guest_grace(&self) -> Duration {
    self.guest_grace
  }

  /// Most guests one shared guest link lets into the lobby.
  pub fn max_shared_link_guests(&self) -> u32 {
    self.max_shared_link_guests
  }

  /// A guest join link (personal or shared): the token is in the fragment, which browsers never send to servers.
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
