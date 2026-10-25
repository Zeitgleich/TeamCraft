//! Pure helpers of the one search's filter chips: the `in:` scope of an open conversation, moving
//! typed operators into chips, and the date filter of "Last 7 days". The query text stays the
//! single source of truth (chips are its filter tokens), so what is typed and what the chips show
//! cannot drift apart.

use crate::model::{ConversationKind, TimestampMs};
use crate::search::{TokenKind, parse_query};

const MS_PER_DAY: i64 = 86_400_000;
/// "Last 7 days": messages of today and the six days before.
const RECENT_DAYS: i64 = 7;

/// The `in:` token that limits a search to one conversation: `in:#channel` for channels,
/// `in:@person` for direct and group messages. A name with spaces is quoted.
pub fn scope_token(kind: ConversationKind, name: &str) -> String {
  let prefix = match kind {
    ConversationKind::Direct | ConversationKind::Group => '@',
    ConversationKind::Public | ConversationKind::Private | ConversationKind::Integration => '#',
  };
  if name.contains(char::is_whitespace) {
    format!("in:\"{prefix}{name}\"")
  } else {
    format!("in:{prefix}{name}")
  }
}

/// How a chip reads: its token without the quotes around a name with spaces.
pub fn chip_label(raw: &str) -> String {
  raw.replace('"', "")
}

/// Moves the finished filter tokens of typed text into chips. A token is finished once whitespace
/// follows it, so `in:#dev` is still text while it is being typed. Returns the new chips and the
/// text that is left; tokens the parser doesn't accept (`before:yesterday`) stay in the text.
pub fn split_filters(raw: &str) -> (Vec<String>, String) {
  let parsed = parse_query(raw);
  let is_finished = raw.ends_with(char::is_whitespace);
  let count = parsed.tokens.len();
  let mut chips = Vec::new();
  let mut rest = Vec::new();
  for (index, token) in parsed.tokens.into_iter().enumerate() {
    let is_last_unfinished = index + 1 == count && !is_finished;
    if matches!(token.kind, TokenKind::Filter(_)) && !is_last_unfinished {
      chips.push(token.raw);
    } else {
      rest.push(token.raw);
    }
  }
  if chips.is_empty() {
    return (chips, raw.to_owned());
  }
  let mut text = rest.join(" ");
  if is_finished && !text.is_empty() {
    text.push(' ');
  }
  (chips, text)
}

/// The query the chips and the typed text make together.
pub fn full_query(chips: &[String], text: &str) -> String {
  let mut parts: Vec<&str> = chips.iter().map(String::as_str).collect();
  let text = text.trim();
  if !text.is_empty() {
    parts.push(text);
  }
  parts.join(" ")
}

/// Whether `chips` already holds `token` (operators are case-insensitive).
pub fn has_chip(chips: &[String], token: &str) -> bool {
  chips.iter().any(|chip| chip.eq_ignore_ascii_case(token))
}

/// Adds `token` unless it is there, removes it if it is.
pub fn toggle_chip(chips: &mut Vec<String>, token: &str) {
  if has_chip(chips, token) {
    chips.retain(|chip| !chip.eq_ignore_ascii_case(token));
  } else {
    chips.push(token.to_owned());
  }
}

/// `after:YYYY-MM-DD` for "Last 7 days" as seen from `now`.
pub fn recent_days_token(now: TimestampMs) -> String {
  let day = now.div_euclid(MS_PER_DAY) - RECENT_DAYS;
  let (year, month, date) = civil_from_days(day);
  format!("after:{year:04}-{month:02}-{date:02}")
}

/// Whether a chip is a date filter (the "Date" chip is on).
pub fn is_date_chip(chip: &str) -> bool {
  chip.len() > 6 && chip[..6].eq_ignore_ascii_case("after:")
    || chip.len() > 7 && chip[..7].eq_ignore_ascii_case("before:")
}

/// Calendar date of a day count since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
  let shifted = days + 719_468;
  let era = shifted.div_euclid(146_097);
  let day_of_era = shifted - era * 146_097;
  let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
  let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
  let month_part = (5 * day_of_year + 2) / 153;
  let day = day_of_year - (153 * month_part + 2) / 5 + 1;
  let month = if month_part < 10 {
    month_part + 3
  } else {
    month_part - 9
  };
  let year = year_of_era + era * 400 + i64::from(month <= 2);
  (year, month, day)
}

#[cfg(test)]
mod tests {
  use super::*;

  mod scope {
    use super::*;

    #[test]
    fn a_channel_is_scoped_with_a_hash_and_a_person_with_an_at() {
      assert_eq!(scope_token(ConversationKind::Public, "dev"), "in:#dev");
      assert_eq!(scope_token(ConversationKind::Direct, "Ada"), "in:@Ada");
    }

    #[test]
    fn a_name_with_spaces_is_quoted_and_reads_without_quotes() {
      let token = scope_token(ConversationKind::Private, "team news");
      assert_eq!(token, "in:\"#team news\"");
      assert_eq!(chip_label(&token), "in:#team news");
      assert!(matches!(
        parse_query(&token).tokens[0].kind,
        TokenKind::Filter(crate::search::Filter::In(ref name)) if name == "team news"
      ));
    }
  }

  mod chips {
    use super::*;

    #[test]
    fn a_finished_operator_becomes_a_chip() {
      let (chips, text) = split_filters("in:#dev budget ");
      assert_eq!(chips, ["in:#dev"]);
      assert_eq!(text, "budget ");
    }

    #[test]
    fn an_operator_still_being_typed_stays_text() {
      let (chips, text) = split_filters("budget in:#de");
      assert!(chips.is_empty());
      assert_eq!(text, "budget in:#de");
    }

    #[test]
    fn an_unreadable_operator_stays_text() {
      let (chips, text) = split_filters("before:yesterday ");
      assert!(chips.is_empty());
      assert_eq!(text, "before:yesterday ");
    }

    #[test]
    fn a_quoted_name_stays_one_chip() {
      let (chips, text) = split_filters("in:\"#team news\" ");
      assert_eq!(chips, ["in:\"#team news\""]);
      assert_eq!(text, "");
    }

    #[test]
    fn the_full_query_puts_chips_before_the_text() {
      assert_eq!(full_query(&["in:#dev".to_owned()], " budget "), "in:#dev budget");
      assert_eq!(full_query(&[], "  "), "");
    }

    #[test]
    fn toggling_adds_then_removes_ignoring_case() {
      let mut chips = Vec::new();
      toggle_chip(&mut chips, "has:link");
      assert!(has_chip(&chips, "HAS:LINK"));
      toggle_chip(&mut chips, "HAS:link");
      assert!(chips.is_empty());
    }
  }

  mod dates {
    use super::*;

    #[test]
    fn last_seven_days_counts_back_from_today() {
      // 2024-03-01 12:00 UTC.
      let now = 19_783 * MS_PER_DAY + 12 * 3_600_000;
      assert_eq!(recent_days_token(now), "after:2024-02-23");
    }

    #[test]
    fn date_chips_are_recognised() {
      assert!(is_date_chip("after:2024-02-23"));
      assert!(is_date_chip("before:2024-02-23"));
      assert!(!is_date_chip("has:link"));
    }
  }
}
