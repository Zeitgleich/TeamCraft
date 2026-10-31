//! The operator's mappings (docs/import-synology.md, "The mappings"): which channel goes where and
//! which Synology user is which TeamCraft account. Two CSV files with a header row; columns are
//! found by name, so their order and extra columns don't matter.

use std::collections::BTreeMap;
use std::path::Path;

use crate::export::SourceId;

/// Where one Synology channel goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelRow {
  pub channel_id: SourceId,
  pub name: String,
  pub is_public: bool,
  pub space: String,
  pub target: ChannelTarget,
  pub has_history: bool,
  pub is_included: bool,
}

/// The `teamcraft_channel` column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelTarget {
  /// A new channel in the row's space.
  New,
  /// An existing channel, `Space/Channel`.
  Existing { space: String, channel: String },
  /// The notes of the channel's single member.
  Notes,
}

/// The channel and user mappings.
#[derive(Debug, Clone, Default)]
pub struct Mapping {
  pub channels: BTreeMap<SourceId, ChannelRow>,
  /// Synology user ID → identity provider user name.
  pub users: BTreeMap<SourceId, String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MappingError {
  #[error("{file}: {reason}")]
  Read { file: String, reason: String },
  #[error("{file}: the column {column} is missing")]
  MissingColumn { file: String, column: &'static str },
  #[error("{file}, line {line}: {reason}")]
  Row {
    file: String,
    line: usize,
    reason: String,
  },
}

impl Mapping {
  pub fn read(channels: &Path, users: &Path) -> Result<Self, MappingError> {
    Ok(Self {
      channels: parse_channels(&read_file(channels)?, &channels.display().to_string())?,
      users: parse_users(&read_file(users)?, &users.display().to_string())?,
    })
  }
}

fn read_file(path: &Path) -> Result<String, MappingError> {
  std::fs::read_to_string(path).map_err(|error| MappingError::Read {
    file: path.display().to_string(),
    reason: error.to_string(),
  })
}

/// `f`, `false`, `n` or `no` (any case) say no; anything else (also empty) says yes.
fn is_yes(value: &str) -> bool {
  !matches!(
    value.trim().to_ascii_lowercase().as_str(),
    "f" | "false" | "n" | "no"
  )
}

pub fn parse_channels(text: &str, file: &str) -> Result<BTreeMap<SourceId, ChannelRow>, MappingError> {
  let table = Table::parse(text, file)?;
  let id = table.column("channel_id")?;
  let name = table.column("name")?;
  let visibility = table.column("visibility")?;
  let space = table.column("space")?;
  let target = table.column("teamcraft_channel")?;
  let history = table.column("import_history")?;
  let import = table.column("import")?;
  let mut rows = BTreeMap::new();
  for (line, record) in table.records() {
    let row_error = |reason: String| MappingError::Row {
      file: file.to_owned(),
      line,
      reason,
    };
    let channel_id: SourceId = record
      .get(id)
      .map(|value| value.trim())
      .and_then(|value| value.parse().ok())
      .ok_or_else(|| row_error("channel_id is not a number".into()))?;
    let raw_target = record.get(target).map_or("", |value| value.trim());
    let target = match raw_target {
      "" => ChannelTarget::New,
      notes if notes.eq_ignore_ascii_case("notes") => ChannelTarget::Notes,
      path => match path.split_once('/') {
        Some((space, channel)) if !space.trim().is_empty() && !channel.trim().is_empty() => {
          ChannelTarget::Existing {
            space: space.trim().to_owned(),
            channel: channel.trim().to_owned(),
          }
        },
        _ => {
          return Err(row_error(format!(
            "teamcraft_channel {path:?} is not Space/Channel"
          )));
        },
      },
    };
    let value = |column: usize| record.get(column).map_or("", |value| value.trim()).to_owned();
    let row = ChannelRow {
      channel_id,
      name: value(name),
      is_public: value(visibility).eq_ignore_ascii_case("public"),
      space: value(space),
      target,
      has_history: is_yes(&value(history)),
      is_included: is_yes(&value(import)),
    };
    if rows.insert(channel_id, row).is_some() {
      return Err(row_error(format!("channel {channel_id} is listed twice")));
    }
  }
  Ok(rows)
}

pub fn parse_users(text: &str, file: &str) -> Result<BTreeMap<SourceId, String>, MappingError> {
  let table = Table::parse(text, file)?;
  let id = table.column("synology_id")?;
  let user = table.column("teamcraft_user")?;
  let mut users = BTreeMap::new();
  for (line, record) in table.records() {
    let synology_id: SourceId = record
      .get(id)
      .map(|value| value.trim())
      .and_then(|value| value.parse().ok())
      .ok_or_else(|| MappingError::Row {
        file: file.to_owned(),
        line,
        reason: "synology_id is not a number".into(),
      })?;
    let username = record.get(user).map_or("", |value| value.trim());
    if !username.is_empty() {
      users.insert(synology_id, username.to_owned());
    }
  }
  Ok(users)
}

/// A parsed CSV file (RFC 4180: quoted fields with doubled quotes and line breaks, `,` between
/// fields, CRLF or LF between records).
struct Table {
  file: String,
  header: Vec<String>,
  /// Records with the line they start on.
  rows: Vec<(usize, Vec<String>)>,
}

impl Table {
  fn parse(text: &str, file: &str) -> Result<Self, MappingError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut records = parse_csv(text).map_err(|(line, reason)| MappingError::Row {
      file: file.to_owned(),
      line,
      reason: reason.into(),
    })?;
    if records.is_empty() {
      return Err(MappingError::Read {
        file: file.to_owned(),
        reason: "the file is empty".into(),
      });
    }
    let (_, header) = records.remove(0);
    let header = header
      .into_iter()
      .map(|name| name.trim().to_ascii_lowercase())
      .collect();
    Ok(Self {
      file: file.to_owned(),
      header,
      rows: records,
    })
  }

  fn column(&self, name: &'static str) -> Result<usize, MappingError> {
    self
      .header
      .iter()
      .position(|column| column == name)
      .ok_or_else(|| MappingError::MissingColumn {
        file: self.file.clone(),
        column: name,
      })
  }

  /// Records that aren't blank.
  fn records(&self) -> impl Iterator<Item = (usize, &Vec<String>)> {
    self
      .rows
      .iter()
      .filter(|(_, record)| record.iter().any(|field| !field.trim().is_empty()))
      .map(|(line, record)| (*line, record))
  }
}

/// Records with the line each starts on.
type Records = Vec<(usize, Vec<String>)>;

fn parse_csv(text: &str) -> Result<Records, (usize, &'static str)> {
  let mut records = Vec::new();
  let mut record = Vec::new();
  let mut field = String::new();
  let mut line = 1;
  let mut record_line = 1;
  let mut is_quoted = false;
  let mut was_quoted = false;
  let mut chars = text.chars().peekable();
  while let Some(ch) = chars.next() {
    if is_quoted {
      match ch {
        '"' if chars.peek() == Some(&'"') => {
          chars.next();
          field.push('"');
        },
        '"' => is_quoted = false,
        '\n' => {
          line += 1;
          field.push(ch);
        },
        _ => field.push(ch),
      }
      continue;
    }
    match ch {
      '"' if field.is_empty() && !was_quoted => {
        is_quoted = true;
        was_quoted = true;
      },
      '"' => return Err((line, "a quote inside an unquoted field")),
      ',' => {
        record.push(std::mem::take(&mut field));
        was_quoted = false;
      },
      '\r' if chars.peek() == Some(&'\n') => {},
      '\n' => {
        record.push(std::mem::take(&mut field));
        records.push((record_line, std::mem::take(&mut record)));
        was_quoted = false;
        line += 1;
        record_line = line;
      },
      _ if was_quoted => return Err((line, "text after a closing quote")),
      _ => field.push(ch),
    }
  }
  if is_quoted {
    return Err((record_line, "a quoted field is not closed"));
  }
  if !field.is_empty() || !record.is_empty() || was_quoted {
    record.push(field);
    records.push((record_line, record));
  }
  Ok(records)
}

#[cfg(test)]
mod tests {
  use super::*;

  const HEADER: &str = "channel_id,name,visibility,webhook,members,posts,last_post,created,archived,space,teamcraft_channel,import_history,import";

  #[test]
  fn rows_say_where_a_channel_goes() {
    let text = format!(
      "{HEADER}\n1,general,public,,3,10,,,,Team,,yes,\n2,logs,private,yes,2,5,,,,Ops,Ops/Monitoring,no,\n3,\"Mine, all\",private,,1,4,,,,,notes,yes,\n4,old,private,,1,0,,,,,,,F\n"
    );
    let rows = parse_channels(&text, "channels.csv").unwrap();
    assert_eq!(rows[&1].target, ChannelTarget::New);
    assert!(rows[&1].is_public && rows[&1].has_history && rows[&1].is_included);
    assert_eq!(
      rows[&2].target,
      ChannelTarget::Existing {
        space: "Ops".into(),
        channel: "Monitoring".into()
      }
    );
    assert!(!rows[&2].has_history);
    assert_eq!(rows[&3].name, "Mine, all");
    assert_eq!(rows[&3].target, ChannelTarget::Notes);
    assert!(!rows[&4].is_included);
  }

  #[test]
  fn every_no_spelling_excludes() {
    for no in ["f", "F", "false", "FALSE", "n", "No", " no "] {
      assert!(!is_yes(no), "{no}");
    }
    for yes in ["", "yes", "y", "true", "x"] {
      assert!(is_yes(yes), "{yes}");
    }
  }

  #[test]
  fn users_without_an_account_are_unmapped() {
    let text = "synology_id,username,nickname,type,deleted,posts,last_post,teamcraft_user\r\n7,anna,Anna,1,f,3,,anna.a\r\n8,ben,,1,t,0,,\r\n";
    let users = parse_users(text, "users.csv").unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[&7], "anna.a");
  }

  #[test]
  fn malformed_files_are_refused_with_their_line() {
    assert!(matches!(
      parse_channels("channel_id,name\n1,a\n", "c.csv"),
      Err(MappingError::MissingColumn {
        column: "visibility",
        ..
      })
    ));
    let text = format!("{HEADER}\nx,general,public,,,,,,,Team,,,\n");
    assert!(matches!(
      parse_channels(&text, "c.csv"),
      Err(MappingError::Row { line: 2, .. })
    ));
    let text = format!("{HEADER}\n1,general,public,,,,,,,Team,NoSlash,,\n");
    assert!(matches!(
      parse_channels(&text, "c.csv"),
      Err(MappingError::Row { .. })
    ));
    let text = format!("{HEADER}\n1,\"open,public\n");
    assert!(matches!(
      parse_channels(&text, "c.csv"),
      Err(MappingError::Row { .. })
    ));
    assert!(matches!(parse_users("", "u.csv"), Err(MappingError::Read { .. })));
    let text = format!("{HEADER}\n1,a,public,,,,,,,T,,,\n1,b,public,,,,,,,T,,,\n");
    assert!(matches!(
      parse_channels(&text, "c.csv"),
      Err(MappingError::Row { line: 3, .. })
    ));
  }

  #[test]
  fn csv_quotes_and_line_breaks_round_trip() {
    let records = parse_csv("a,\"b \"\"c\"\"\",\"d\ne\"\n1,2,3").unwrap();
    assert_eq!(records[0].1, ["a", "b \"c\"", "d\ne"]);
    assert_eq!(records[1].0, 3);
    assert_eq!(records[1].1, ["1", "2", "3"]);
    assert!(parse_csv("a\"b").is_err());
    assert!(parse_csv("\"a\"b").is_err());
  }
}
