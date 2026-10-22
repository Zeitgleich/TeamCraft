//! iCalendar (RFC 5545) rendering of meetings for CalDAV and iMIP (RFC 6047), and the small part of
//! parsing needed for RSVP replies.

use teamcraft_proto::RsvpStatus;
use time::OffsetDateTime;

const PRODUCT_ID: &str = "-//TeamCraft//Meetings//EN";
/// Content lines are folded at this many octets (RFC 5545 §3.1).
const FOLD_OCTETS: usize = 75;

/// The iMIP method (RFC 5546) of a message; `None` renders a plain calendar object for CalDAV.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
  Request,
  Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventStatus {
  Confirmed,
  Cancelled,
}

/// One person on the event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventPerson {
  pub email: String,
  pub maybe_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventAttendee {
  pub person: EventPerson,
  pub rsvp: RsvpStatus,
}

/// Everything a VEVENT of a meeting carries.
#[derive(Debug, Clone)]
pub struct EventData<'a> {
  pub uid: &'a str,
  pub sequence: i32,
  pub stamp: OffsetDateTime,
  pub summary: &'a str,
  pub maybe_description: Option<&'a str>,
  pub starts_at: OffsetDateTime,
  pub ends_at: OffsetDateTime,
  /// RRULE value without the property name, for example `FREQ=WEEKLY;INTERVAL=1;COUNT=4`.
  pub maybe_recurrence_rule: Option<&'a str>,
  pub status: EventStatus,
  /// The join link, put into `URL`, `LOCATION` and the description.
  pub join_url: &'a str,
  pub organizer: EventPerson,
  pub attendees: Vec<EventAttendee>,
}

/// Renders a VCALENDAR with one VEVENT, CRLF line endings, folded lines.
pub fn render_event(event: &EventData<'_>, maybe_method: Option<Method>) -> String {
  let mut lines = vec![
    "BEGIN:VCALENDAR".to_owned(),
    "VERSION:2.0".to_owned(),
    format!("PRODID:{PRODUCT_ID}"),
    "CALSCALE:GREGORIAN".to_owned(),
  ];
  if let Some(method) = maybe_method {
    lines.push(format!(
      "METHOD:{}",
      match method {
        Method::Request => "REQUEST",
        Method::Cancel => "CANCEL",
      }
    ));
  }
  lines.push("BEGIN:VEVENT".to_owned());
  lines.push(format!("UID:{}", event.uid));
  lines.push(format!("DTSTAMP:{}", format_utc(event.stamp)));
  lines.push(format!("DTSTART:{}", format_utc(event.starts_at)));
  lines.push(format!("DTEND:{}", format_utc(event.ends_at)));
  lines.push(format!("SEQUENCE:{}", event.sequence));
  lines.push(format!("SUMMARY:{}", escape_text(event.summary)));
  let description = match event.maybe_description {
    Some(text) if !text.is_empty() => format!("{text}\n\nJoin: {}", event.join_url),
    _ => format!("Join: {}", event.join_url),
  };
  lines.push(format!("DESCRIPTION:{}", escape_text(&description)));
  lines.push(format!("LOCATION:{}", escape_text(event.join_url)));
  lines.push(format!("URL:{}", event.join_url));
  if let Some(rule) = event.maybe_recurrence_rule {
    lines.push(format!("RRULE:{rule}"));
  }
  lines.push(format!(
    "STATUS:{}",
    match event.status {
      EventStatus::Confirmed => "CONFIRMED",
      EventStatus::Cancelled => "CANCELLED",
    }
  ));
  lines.push(format!(
    "ORGANIZER{}:mailto:{}",
    common_name(&event.organizer),
    event.organizer.email
  ));
  for attendee in &event.attendees {
    lines.push(format!(
      "ATTENDEE{};PARTSTAT={};RSVP=TRUE:mailto:{}",
      common_name(&attendee.person),
      partstat(&attendee.rsvp),
      attendee.person.email
    ));
  }
  lines.push("END:VEVENT".to_owned());
  lines.push("END:VCALENDAR".to_owned());

  let mut out = String::new();
  for line in lines {
    fold_into(&line, &mut out);
  }
  out
}

/// An invitee's answer found in a REPLY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyEntry {
  pub email: String,
  pub rsvp: RsvpStatus,
}

/// The parts of an iMIP REPLY (RFC 5546 §3.2.3) the server acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
  pub uid: String,
  pub entries: Vec<ReplyEntry>,
}

/// Why a calendar object is not a usable reply.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReplyError {
  #[error("not a calendar object")]
  NotACalendar,
  #[error("the calendar object is not a REPLY")]
  NotAReply,
  #[error("the event has no UID")]
  MissingUid,
  #[error("the reply names no attendee with a valid answer")]
  NoAnswer,
}

/// Parses an iMIP REPLY: `METHOD:REPLY`, the event's `UID` and its `ATTENDEE` lines with `PARTSTAT`.
pub fn parse_reply(text: &str) -> Result<Reply, ReplyError> {
  let lines = unfold(text);
  if !lines
    .iter()
    .any(|line| line.eq_ignore_ascii_case("BEGIN:VCALENDAR"))
  {
    return Err(ReplyError::NotACalendar);
  }
  let is_reply = lines.iter().any(|line| line.eq_ignore_ascii_case("METHOD:REPLY"));
  if !is_reply {
    return Err(ReplyError::NotAReply);
  }
  let mut in_event = false;
  let mut maybe_uid = None;
  let mut entries = Vec::new();
  for line in &lines {
    if line.eq_ignore_ascii_case("BEGIN:VEVENT") {
      in_event = true;
    } else if line.eq_ignore_ascii_case("END:VEVENT") {
      in_event = false;
    } else if in_event {
      let (name_and_params, value) = line.split_once(':').unwrap_or((line.as_str(), ""));
      let mut parts = name_and_params.split(';');
      let name = parts.next().unwrap_or("");
      if name.eq_ignore_ascii_case("UID") {
        maybe_uid = Some(value.trim().to_owned());
      } else if name.eq_ignore_ascii_case("ATTENDEE") {
        let status = parts
          .filter_map(|param| param.split_once('='))
          .find(|(key, _)| key.eq_ignore_ascii_case("PARTSTAT"))
          .and_then(|(_, status)| parse_partstat(status));
        let email = value
          .trim()
          .strip_prefix_ignore_case("mailto:")
          .map(str::to_lowercase);
        if let (Some(rsvp), Some(email)) = (status, email) {
          entries.push(ReplyEntry { email, rsvp });
        }
      }
    }
  }
  let uid = maybe_uid
    .filter(|uid| !uid.is_empty())
    .ok_or(ReplyError::MissingUid)?;
  if entries.is_empty() {
    return Err(ReplyError::NoAnswer);
  }
  Ok(Reply { uid, entries })
}

fn parse_partstat(value: &str) -> Option<RsvpStatus> {
  match value.trim().to_ascii_uppercase().as_str() {
    "ACCEPTED" => Some(RsvpStatus::Accepted),
    "DECLINED" => Some(RsvpStatus::Declined),
    "TENTATIVE" => Some(RsvpStatus::Tentative),
    "NEEDS-ACTION" => Some(RsvpStatus::NeedsAction),
    _ => None,
  }
}

fn partstat(status: &RsvpStatus) -> &'static str {
  match status {
    RsvpStatus::Accepted => "ACCEPTED",
    RsvpStatus::Declined => "DECLINED",
    RsvpStatus::Tentative => "TENTATIVE",
    RsvpStatus::NeedsAction | RsvpStatus::Unknown(_) => "NEEDS-ACTION",
  }
}

/// `;CN="Name"` or nothing. Quoted parameter values may not contain `"` or control characters.
fn common_name(person: &EventPerson) -> String {
  match &person.maybe_name {
    Some(name) => {
      let cleaned: String = name.chars().filter(|c| *c != '"' && !c.is_control()).collect();
      if cleaned.is_empty() {
        String::new()
      } else {
        format!(";CN=\"{cleaned}\"")
      }
    },
    None => String::new(),
  }
}

/// RFC 5545 §3.3.11 TEXT escaping.
fn escape_text(text: &str) -> String {
  let mut out = String::with_capacity(text.len());
  for c in text.chars() {
    match c {
      '\\' => out.push_str("\\\\"),
      ';' => out.push_str("\\;"),
      ',' => out.push_str("\\,"),
      '\r' => {},
      '\n' => out.push_str("\\n"),
      other => out.push(other),
    }
  }
  out
}

/// `YYYYMMDDTHHMMSSZ`.
fn format_utc(time: OffsetDateTime) -> String {
  let utc = time.to_offset(time::UtcOffset::UTC);
  format!(
    "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
    utc.year(),
    u8::from(utc.month()),
    utc.day(),
    utc.hour(),
    utc.minute(),
    utc.second()
  )
}

/// Appends `line` folded to 75 octets per physical line, never inside a UTF-8 sequence.
fn fold_into(line: &str, out: &mut String) {
  let mut octets_on_line = 0;
  for c in line.chars() {
    let width = c.len_utf8();
    if octets_on_line + width > FOLD_OCTETS {
      out.push_str("\r\n ");
      // The continuation space counts as one octet of the new line.
      octets_on_line = 1;
    }
    out.push(c);
    octets_on_line += width;
  }
  out.push_str("\r\n");
}

/// Joins folded lines (a line starting with space or tab continues the previous one).
fn unfold(text: &str) -> Vec<String> {
  let mut lines: Vec<String> = Vec::new();
  for raw in text.split('\n') {
    let line = raw.strip_suffix('\r').unwrap_or(raw);
    if let Some(continuation) = line.strip_prefix([' ', '\t'])
      && let Some(last) = lines.last_mut()
    {
      last.push_str(continuation);
    } else if !line.is_empty() {
      lines.push(line.to_owned());
    }
  }
  lines
}

trait StripPrefixIgnoreCase {
  fn strip_prefix_ignore_case(&self, prefix: &str) -> Option<&str>;
}

impl StripPrefixIgnoreCase for str {
  fn strip_prefix_ignore_case(&self, prefix: &str) -> Option<&str> {
    let head = self.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| &self[prefix.len()..])
  }
}

#[cfg(test)]
mod tests {
  use time::macros::datetime;

  use super::*;

  mod rendering {
    use super::*;

    #[test]
    fn a_request_has_method_times_organizer_and_attendees() {
      let ics = render_event(&event(), Some(Method::Request));

      assert!(ics.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n"));
      assert!(ics.contains("\r\nMETHOD:REQUEST\r\n"));
      assert!(ics.contains("\r\nUID:42@chat.example.com\r\n"));
      assert!(ics.contains("\r\nDTSTART:20261105T093000Z\r\n"));
      assert!(ics.contains("\r\nDTEND:20261105T103000Z\r\n"));
      assert!(ics.contains("\r\nSEQUENCE:2\r\n"));
      assert!(ics.contains("\r\nORGANIZER;CN=\"Olive Organizer\":mailto:olive@example.com\r\n"));
      assert!(unfolded(&ics).contains(
        "\r\nATTENDEE;CN=\"Gus Guest\";PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:gus@example.org\r\n"
      ));
      assert!(ics.contains("\r\nURL:https://chat.example.com/meeting/42\r\n"));
      assert!(ics.ends_with("END:VEVENT\r\nEND:VCALENDAR\r\n"));
    }

    #[test]
    fn caldav_objects_have_no_method_and_cancellations_say_so() {
      assert!(!render_event(&event(), None).contains("METHOD"));
      let mut cancelled = event();
      cancelled.status = EventStatus::Cancelled;
      let ics = render_event(&cancelled, Some(Method::Cancel));
      assert!(ics.contains("\r\nMETHOD:CANCEL\r\n") && ics.contains("\r\nSTATUS:CANCELLED\r\n"));
    }

    #[test]
    fn text_is_escaped_and_the_join_link_is_part_of_the_description() {
      let mut data = event();
      data.summary = "Plan; budget, \\ next\nline";
      data.maybe_description = Some("Agenda\nitem");
      let ics = unfolded(&render_event(&data, None));
      assert!(
        ics.contains("SUMMARY:Plan\\; budget\\, \\\\ next\\nline"),
        "{ics}"
      );
      assert!(ics.contains("DESCRIPTION:Agenda\\nitem\\n\\nJoin: https://chat.example.com/meeting/42"));
    }

    #[test]
    fn long_lines_fold_at_75_octets_without_splitting_characters() {
      let mut data = event();
      let title = "Überlänge ".repeat(30);
      data.summary = &title;
      let ics = render_event(&data, None);
      for line in ics.split("\r\n") {
        assert!(line.len() <= FOLD_OCTETS, "{} octets: {line}", line.len());
      }
      assert!(unfolded(&ics).contains(&format!("SUMMARY:{title}")));
    }

    #[test]
    fn recurrence_and_odd_names_are_handled() {
      let mut data = event();
      data.maybe_recurrence_rule = Some("FREQ=WEEKLY;INTERVAL=1;COUNT=4");
      data.organizer.maybe_name = Some("Quo\"te\r\nd".into());
      let ics = render_event(&data, None);
      assert!(ics.contains("\r\nRRULE:FREQ=WEEKLY;INTERVAL=1;COUNT=4\r\n"));
      assert!(ics.contains("ORGANIZER;CN=\"Quoted\":mailto:olive@example.com"));
    }
  }

  mod replies {
    use super::*;

    const REPLY: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REPLY\r\nBEGIN:VEVENT\r\nUID:42@chat.example.com\r\n\
      ATTENDEE;PARTSTAT=ACCEPTED;CN=\"Gus Guest\":mailto:Gus@Example.org\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn an_accepting_reply_names_uid_and_lowercased_attendee() {
      assert_eq!(
        parse_reply(REPLY).unwrap(),
        Reply {
          uid: "42@chat.example.com".into(),
          entries: vec![ReplyEntry {
            email: "gus@example.org".into(),
            rsvp: RsvpStatus::Accepted
          }]
        }
      );
    }

    #[test]
    fn folded_lines_and_other_answers_are_understood() {
      let tentative = REPLY.replace("ACCEPTED", "tentative");
      assert_eq!(
        parse_reply(&tentative).unwrap().entries[0].rsvp,
        RsvpStatus::Tentative
      );
      let folded = REPLY.replace("PARTSTAT=ACCEPTED", "PARTSTAT=DECL\r\n INED");
      assert_eq!(
        parse_reply(&folded).unwrap().entries[0].rsvp,
        RsvpStatus::Declined
      );
    }

    #[test]
    fn malformed_input_is_rejected() {
      assert_eq!(parse_reply(""), Err(ReplyError::NotACalendar));
      assert_eq!(parse_reply("hello"), Err(ReplyError::NotACalendar));
      assert_eq!(
        parse_reply(&REPLY.replace("REPLY", "REQUEST")),
        Err(ReplyError::NotAReply)
      );
      assert_eq!(
        parse_reply(&REPLY.replace("UID:42@chat.example.com\r\n", "")),
        Err(ReplyError::MissingUid)
      );
      assert_eq!(
        parse_reply(&REPLY.replace("mailto:", "tel:")),
        Err(ReplyError::NoAnswer)
      );
      assert_eq!(
        parse_reply(&REPLY.replace("ACCEPTED", "MAYBE")),
        Err(ReplyError::NoAnswer)
      );
    }

    #[test]
    fn what_we_render_for_attendees_is_not_a_reply() {
      assert_eq!(
        parse_reply(&render_event(&event(), Some(Method::Request))),
        Err(ReplyError::NotAReply)
      );
    }
  }

  fn event() -> EventData<'static> {
    EventData {
      uid: "42@chat.example.com",
      sequence: 2,
      stamp: datetime!(2026-11-01 08:00 UTC),
      summary: "Sprint review",
      maybe_description: None,
      starts_at: datetime!(2026-11-05 09:30 UTC),
      ends_at: datetime!(2026-11-05 10:30 UTC),
      maybe_recurrence_rule: None,
      status: EventStatus::Confirmed,
      join_url: "https://chat.example.com/meeting/42",
      organizer: EventPerson {
        email: "olive@example.com".into(),
        maybe_name: Some("Olive Organizer".into()),
      },
      attendees: vec![EventAttendee {
        person: EventPerson {
          email: "gus@example.org".into(),
          maybe_name: Some("Gus Guest".into()),
        },
        rsvp: RsvpStatus::NeedsAction,
      }],
    }
  }

  fn unfolded(ics: &str) -> String {
    ics.replace("\r\n ", "")
  }
}
