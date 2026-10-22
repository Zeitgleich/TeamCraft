//! iCalendar handling of meetings: parsing a received iMIP REPLY (untrusted mail), and rendering
//! events from user-controlled strings. Rendering must never let a field break out of its line
//! (CR/LF injection) or produce lines over the fold limit.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use teamcraft_proto::RsvpStatus;
use teamcraft_server::meetings::ical::{
  EventAttendee, EventData, EventPerson, EventStatus, Method, parse_reply, render_event,
};
use time::OffsetDateTime;

#[derive(Debug, Arbitrary)]
struct Input<'a> {
  reply: &'a str,
  uid: &'a str,
  summary: &'a str,
  description: &'a str,
  join_url: &'a str,
  organizer_email: &'a str,
  organizer_name: &'a str,
  attendee_email: &'a str,
  attendee_name: &'a str,
  starts: i32,
  length: u16,
}

fuzz_target!(|input: Input<'_>| {
  let _ = parse_reply(input.reply);

  let start = OffsetDateTime::from_unix_timestamp(i64::from(input.starts) * 60).unwrap();
  let person = |email: &str, name: &str| EventPerson {
    email: email.to_owned(),
    maybe_name: Some(name.to_owned()),
  };
  let event = EventData {
    uid: input.uid,
    sequence: 1,
    stamp: start,
    summary: input.summary,
    maybe_description: Some(input.description),
    starts_at: start,
    ends_at: start + time::Duration::minutes(i64::from(input.length) + 1),
    maybe_recurrence_rule: None,
    status: EventStatus::Confirmed,
    join_url: input.join_url,
    organizer: person(input.organizer_email, input.organizer_name),
    attendees: vec![EventAttendee {
      person: person(input.attendee_email, input.attendee_name),
      rsvp: RsvpStatus::NeedsAction,
    }],
  };
  let ics = render_event(&event, Some(Method::Request));
  // Every physical line ends in CRLF, none is longer than 75 octets, and no bare CR or LF exists.
  assert!(ics.ends_with("\r\n"));
  for line in ics.strip_suffix("\r\n").unwrap().split("\r\n") {
    assert!(line.len() <= 75, "{} octets: {line:?}", line.len());
    assert!(!line.contains(['\r', '\n']), "{line:?}");
  }
  // And a rendered object is never mistaken for a reply (METHOD is REQUEST).
  assert!(parse_reply(&ics).is_err());
});
