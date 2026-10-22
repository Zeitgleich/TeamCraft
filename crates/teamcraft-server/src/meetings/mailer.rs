//! Invitation e-mail (iMIP, RFC 6047): a plain text part and the calendar object as
//! `text/calendar; method=...` alternative plus an `.ics` attachment, so every mail client shows
//! the invitation with accept/decline buttons.

use std::time::Duration;

use lettre::message::header::ContentType;
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use secrecy::ExposeSecret;

use super::ical::Method;
use crate::auth::BoxFuture;
use crate::config::{SmtpConfig, SmtpSecurity};

/// One e-mail to one invitee.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingMail {
  pub to_address: String,
  pub maybe_to_name: Option<String>,
  pub subject: String,
  pub text: String,
  pub method: Method,
  /// The VCALENDAR object (CRLF line endings); empty for a plain text message.
  pub ics: String,
}

/// Sends invitation e-mail.
pub trait Mailer: Send + Sync {
  fn send<'a>(&'a self, mail: &'a OutgoingMail) -> BoxFuture<'a, Result<(), MailError>>;
}

#[derive(Debug, thiserror::Error)]
pub enum MailError {
  #[error("the mail could not be built: {0}")]
  Build(String),
  #[error("the mail server refused or could not be reached: {0}")]
  Send(String),
}

/// SMTP through `lettre` with rustls.
pub struct SmtpMailer {
  transport: AsyncSmtpTransport<Tokio1Executor>,
  from: Mailbox,
}

impl SmtpMailer {
  pub fn new(config: &SmtpConfig) -> Result<Self, MailError> {
    let tls =
      |host: &str| TlsParameters::new(host.to_owned()).map_err(|error| MailError::Build(error.to_string()));
    let mut builder = match config.security {
      SmtpSecurity::StartTls => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&config.host)
        .tls(Tls::Required(tls(&config.host)?)),
      SmtpSecurity::Tls => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&config.host)
        .tls(Tls::Wrapper(tls(&config.host)?)),
      SmtpSecurity::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&config.host),
    }
    .port(config.port)
    .timeout(Some(Duration::from_secs(config.timeout_secs)));
    if let (Some(username), Some(password)) = (&config.username, &config.password) {
      builder = builder.credentials(Credentials::new(
        username.clone(),
        password.expose_secret().to_owned(),
      ));
    }
    let from = mailbox(&config.from_address, config.from_name.as_deref())?;
    Ok(Self {
      transport: builder.build(),
      from,
    })
  }
}

impl Mailer for SmtpMailer {
  fn send<'a>(&'a self, mail: &'a OutgoingMail) -> BoxFuture<'a, Result<(), MailError>> {
    Box::pin(async move {
      let message = build_message(&self.from, mail)?;
      self
        .transport
        .send(message)
        .await
        .map(|_| ())
        .map_err(|error| MailError::Send(error.to_string()))
    })
  }
}

/// The RFC 6047 message for one invitation.
fn build_message(from: &Mailbox, mail: &OutgoingMail) -> Result<Message, MailError> {
  if mail.ics.is_empty() {
    return Message::builder()
      .from(from.clone())
      .to(mailbox(&mail.to_address, mail.maybe_to_name.as_deref())?)
      .subject(mail.subject.clone())
      .body(mail.text.clone())
      .map_err(|error| MailError::Build(error.to_string()));
  }
  let method = match mail.method {
    Method::Request => "REQUEST",
    Method::Cancel => "CANCEL",
  };
  let calendar_type = ContentType::parse(&format!("text/calendar; charset=UTF-8; method={method}"))
    .map_err(|error| MailError::Build(error.to_string()))?;
  let attachment_type = ContentType::parse("application/ics; charset=UTF-8")
    .map_err(|error| MailError::Build(error.to_string()))?;
  let body = MultiPart::mixed()
    .multipart(
      MultiPart::alternative()
        .singlepart(SinglePart::plain(mail.text.clone()))
        .singlepart(SinglePart::builder().header(calendar_type).body(mail.ics.clone())),
    )
    .singlepart(Attachment::new("invite.ics".to_owned()).body(mail.ics.clone(), attachment_type));
  Message::builder()
    .from(from.clone())
    .to(mailbox(&mail.to_address, mail.maybe_to_name.as_deref())?)
    .subject(mail.subject.clone())
    .multipart(body)
    .map_err(|error| MailError::Build(error.to_string()))
}

fn mailbox(address: &str, maybe_name: Option<&str>) -> Result<Mailbox, MailError> {
  let address = address
    .parse()
    .map_err(|error: lettre::address::AddressError| MailError::Build(error.to_string()))?;
  Ok(Mailbox::new(maybe_name.map(str::to_owned), address))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_message_has_text_calendar_alternative_and_ics_attachment() {
    let from = mailbox("meetings@example.com", Some("TeamCraft")).unwrap();
    let mail = OutgoingMail {
      to_address: "gus@example.org".into(),
      maybe_to_name: Some("Gus Guest".into()),
      subject: "Invitation: Sprint review".into(),
      text: "Join soon".into(),
      method: Method::Request,
      ics: "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nEND:VCALENDAR\r\n".into(),
    };

    let message = build_message(&from, &mail).unwrap();
    let raw = String::from_utf8(message.formatted()).unwrap();

    assert!(raw.contains("Subject: Invitation: Sprint review"), "{raw}");
    assert!(
      raw.contains("To: \"Gus Guest\" <gus@example.org>") || raw.contains("To: Gus Guest <gus@example.org>")
    );
    assert!(raw.contains("multipart/mixed") && raw.contains("multipart/alternative"));
    assert!(
      raw.contains("text/calendar; charset=utf-8; method=REQUEST"),
      "{raw}"
    );
    assert!(raw.contains("filename=\"invite.ics\""));
  }

  #[test]
  fn invalid_addresses_are_rejected_before_sending() {
    assert!(mailbox("not an address", None).is_err());
    assert!(mailbox("a@example.org\r\nBcc: x@example.org", None).is_err());
  }
}
