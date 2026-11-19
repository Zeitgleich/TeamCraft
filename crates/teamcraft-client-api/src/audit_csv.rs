//! The audit log as a CSV file (issue #2137, docs/data-export.md): what a file cell may hold, and
//! the pager that turns the server's pages into file chunks.
//!
//! The server pages the log (`GET /api/v1/admin/audit`, newest first, `before` as the cursor); the
//! console writes each page into the file as it arrives, so a long log is never held in memory
//! whole. The rows are metadata only, like the log itself.

use teamcraft_proto::{AuditEntryId, AuditEntryResponse, ListAuditResponse, UnixMillis, UserId};

/// The first line of the file, in this order.
pub const HEADER: [&str; 7] = [
  "time",
  "actor_user_id",
  "actor",
  "action",
  "target_kind",
  "target_id",
  "detail",
];

/// Spreadsheets read UTF-8 as the local code page without this; harmless to every other reader.
const BYTE_ORDER_MARK: &str = "\u{feff}";
/// Rows end like RFC 4180 says.
const LINE_END: &str = "\r\n";

/// Where an export stands between two pages.
#[derive(Debug, Default)]
pub struct AuditCsvPager {
  stage: Stage,
  maybe_cursor: Option<AuditEntryId>,
  row_count: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Stage {
  #[default]
  Start,
  Reading,
  Done,
}

/// What the pager wants next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditCsvStep {
  /// Read the page of entries older than the cursor (`None`: the newest page) and pass it to
  /// [`AuditCsvPager::accept`].
  Read(Option<AuditEntryId>),
  Finished,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuditCsvError {
  /// The server says more follows but sent nothing new: stopping beats paging forever.
  #[error("the server's pages stopped advancing")]
  Stalled,
}

impl AuditCsvPager {
  pub fn new() -> Self {
    Self::default()
  }

  /// The header line of the file with the byte order mark; the first chunk to write.
  pub fn header() -> String {
    let mut text = String::from(BYTE_ORDER_MARK);
    text.push_str(&line(&HEADER));
    text
  }

  /// Rows written so far.
  pub fn row_count(&self) -> u64 {
    self.row_count
  }

  pub fn next_step(&self) -> AuditCsvStep {
    match self.stage {
      Stage::Done => AuditCsvStep::Finished,
      Stage::Start | Stage::Reading => AuditCsvStep::Read(self.maybe_cursor),
    }
  }

  /// Takes a page and returns the lines to append for the entries `keep` accepts (the console's
  /// filters that the server doesn't apply). `actor_name` words a user ID, empty when unknown.
  pub fn accept(
    &mut self,
    page: &ListAuditResponse,
    keep: impl Fn(&AuditEntryResponse) -> bool,
    actor_name: impl Fn(UserId) -> String,
  ) -> Result<String, AuditCsvError> {
    let maybe_oldest = page.entries.last().map(|entry| entry.id);
    // A page that claims more but ends where the last began would repeat forever.
    if page.has_more && (maybe_oldest.is_none() || maybe_oldest == self.maybe_cursor) {
      self.stage = Stage::Done;
      return Err(AuditCsvError::Stalled);
    }
    let mut text = String::new();
    for entry in page.entries.iter().filter(|entry| keep(entry)) {
      text.push_str(&entry_line(entry, &actor_name));
      self.row_count += 1;
    }
    self.maybe_cursor = maybe_oldest;
    self.stage = if page.has_more {
      Stage::Reading
    } else {
      Stage::Done
    };
    Ok(text)
  }
}

/// One entry as a line of the file.
pub fn entry_line(entry: &AuditEntryResponse, actor_name: &impl Fn(UserId) -> String) -> String {
  let actor_id = entry
    .maybe_actor_user_id
    .map(|id| id.to_string())
    .unwrap_or_default();
  let actor = entry.maybe_actor_user_id.map(actor_name).unwrap_or_default();
  line(&[
    &utc_time(entry.created_at),
    &actor_id,
    &actor,
    &entry.action,
    entry.maybe_target_kind.as_deref().unwrap_or_default(),
    entry.maybe_target_id.as_deref().unwrap_or_default(),
    &entry.detail,
  ])
}

/// Cells joined into one line.
pub fn line(cells: &[&str]) -> String {
  let mut text = cells
    .iter()
    .map(|cell| cell_text(cell))
    .collect::<Vec<_>>()
    .join(",");
  text.push_str(LINE_END);
  text
}

/// What one cell is written as. Text that a spreadsheet would run as a formula (it starts with
/// `=`, `+`, `-` or `@`, or with a tab or return that some programs skip first) gets a leading
/// `'`, so the cell shows the text instead of calculating it. Cells with a comma, quote or line
/// break are quoted, quotes doubled.
pub fn cell_text(cell: &str) -> String {
  let is_formula = cell.starts_with(['=', '+', '-', '@', '\t', '\r']);
  let guarded = if is_formula {
    format!("'{cell}")
  } else {
    cell.to_owned()
  };
  if guarded.contains([',', '"', '\r', '\n']) {
    format!("\"{}\"", guarded.replace('"', "\"\""))
  } else {
    guarded
  }
}

/// `2026-11-19T04:30:00.123Z` for a Unix time; a time outside years 0000 to 9999 is shown as its
/// raw milliseconds.
pub fn utc_time(at: UnixMillis) -> String {
  let millis = at.0;
  let days = millis.div_euclid(86_400_000);
  let in_day = millis.rem_euclid(86_400_000);
  // Howard Hinnant's civil-from-days.
  let shifted = days + 719_468;
  let era = shifted.div_euclid(146_097);
  let day_of_era = shifted.rem_euclid(146_097);
  let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
  let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
  let month_index = (5 * day_of_year + 2) / 153;
  let day = day_of_year - (153 * month_index + 2) / 5 + 1;
  let month = if month_index < 10 {
    month_index + 3
  } else {
    month_index - 9
  };
  let year = year_of_era + era * 400 + i64::from(month <= 2);
  if !(0..=9999).contains(&year) {
    return millis.to_string();
  }
  format!(
    "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
    in_day / 3_600_000,
    in_day / 60_000 % 60,
    in_day / 1000 % 60,
    in_day % 1000
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  mod cells {
    use super::*;

    #[test]
    fn plain_text_stays_as_it_is() {
      assert_eq!(cell_text("channel_archived"), "channel_archived");
      assert_eq!(cell_text(""), "");
    }

    #[test]
    fn commas_quotes_and_line_breaks_are_quoted() {
      assert_eq!(cell_text("a,b"), "\"a,b\"");
      assert_eq!(cell_text("say \"hi\""), "\"say \"\"hi\"\"\"");
      assert_eq!(cell_text("two\nlines"), "\"two\nlines\"");
      assert_eq!(cell_text("a\r\nb"), "\"a\r\nb\"");
    }

    #[test]
    fn json_detail_round_trips_through_quoting() {
      let detail = r#"{"note":"a,b","n":1}"#;
      assert_eq!(cell_text(detail), r#""{""note"":""a,b"",""n"":1}""#);
    }

    #[test]
    fn formulas_are_defused() {
      for input in ["=1+1", "+1", "-1", "@SUM(A1)", "\tx", "\rx"] {
        let cell = cell_text(input);
        assert!(
          cell.starts_with('\'') || cell.starts_with("\"'"),
          "{input:?} became {cell:?}"
        );
      }
      assert_eq!(cell_text("=cmd|' /C calc'!A0"), "'=cmd|' /C calc'!A0");
    }

    #[test]
    fn a_guarded_cell_with_a_comma_is_quoted_after_the_guard() {
      assert_eq!(cell_text("=1,2"), "\"'=1,2\"");
    }

    #[test]
    fn text_with_a_formula_character_inside_is_left_alone() {
      assert_eq!(cell_text("a=b"), "a=b");
      assert_eq!(cell_text("x-y@z"), "x-y@z");
    }
  }

  mod times {
    use super::*;

    #[test]
    fn times_are_utc_with_milliseconds() {
      assert_eq!(utc_time(UnixMillis(0)), "1970-01-01T00:00:00.000Z");
      assert_eq!(
        utc_time(UnixMillis(1_792_627_200_123)),
        "2026-10-22T00:00:00.123Z"
      );
      assert_eq!(
        utc_time(UnixMillis(1_709_164_800_000 + 86_399_999)),
        "2024-02-29T23:59:59.999Z"
      );
      assert_eq!(utc_time(UnixMillis(-1)), "1969-12-31T23:59:59.999Z");
    }

    #[test]
    fn out_of_range_times_fall_back_to_milliseconds() {
      assert_eq!(utc_time(UnixMillis(i64::MAX)), i64::MAX.to_string());
    }
  }

  mod paging {
    use super::*;
    use teamcraft_proto::{Id, Snowflake};

    const PAGE: usize = 3;

    #[test]
    fn every_page_is_read_once_and_rows_come_out_in_order() {
      let (text, reads) = export(&log_of(8), |_| true);
      let times: Vec<_> = rows(&text).into_iter().map(|row| row[0].clone()).collect();
      assert_eq!(times.len(), 8);
      assert_eq!(
        times.first().map(String::as_str),
        Some("1970-01-01T00:00:08.000Z")
      );
      assert_eq!(times.last().map(String::as_str), Some("1970-01-01T00:00:01.000Z"));
      assert_eq!(reads, vec![None, Some(6), Some(3)]);
    }

    #[test]
    fn a_log_that_fits_one_page_is_one_read() {
      let (text, reads) = export(&log_of(2), |_| true);
      assert_eq!(rows(&text).len(), 2);
      assert_eq!(reads, vec![None]);
    }

    #[test]
    fn an_empty_log_is_a_file_with_the_header_only() {
      let (text, reads) = export(&[], |_| true);
      assert!(rows(&text).is_empty());
      assert!(text.starts_with("\u{feff}time,actor_user_id,actor,action,"));
      assert_eq!(reads, vec![None]);
    }

    #[test]
    fn rows_the_console_filters_out_are_left_out_but_still_paged_over() {
      let (text, reads) = export(&log_of(8), |entry| entry.id.snowflake().get() % 2 == 0);
      assert_eq!(rows(&text).len(), 4);
      assert_eq!(reads.len(), 3);
    }

    #[test]
    fn a_page_that_claims_more_but_repeats_stops_the_export() {
      let page = ListAuditResponse {
        entries: log_of(3),
        has_more: true,
      };
      let mut pager = AuditCsvPager::new();
      assert!(pager.accept(&page, |_| true, names).is_ok());
      assert_eq!(pager.accept(&page, |_| true, names), Err(AuditCsvError::Stalled));
      assert_eq!(pager.next_step(), AuditCsvStep::Finished);
      let empty = ListAuditResponse {
        entries: vec![],
        has_more: true,
      };
      assert_eq!(
        AuditCsvPager::new().accept(&empty, |_| true, names),
        Err(AuditCsvError::Stalled)
      );
    }

    #[test]
    fn a_detail_that_looks_like_a_formula_is_defused_in_the_row() {
      let mut entry = entry(1);
      entry.detail = "=HYPERLINK(\"x\")".to_owned();
      let line = entry_line(&entry, &names);
      assert!(line.ends_with("\"'=HYPERLINK(\"\"x\"\")\"\r\n"), "{line}");
    }

    /// Reads the whole log through the pager in pages of [`PAGE`]; returns the file text and the
    /// cursors the pager asked for.
    fn export(
      log: &[AuditEntryResponse],
      keep: impl Fn(&AuditEntryResponse) -> bool,
    ) -> (String, Vec<Option<i64>>) {
      let mut pager = AuditCsvPager::new();
      let mut text = AuditCsvPager::header();
      let mut reads = Vec::new();
      while let AuditCsvStep::Read(maybe_cursor) = pager.next_step() {
        let cursor = maybe_cursor.map(|id| id.snowflake().get());
        reads.push(cursor);
        let older: Vec<_> = log
          .iter()
          .filter(|entry| cursor.is_none_or(|cursor| entry.id.snowflake().get() < cursor))
          .cloned()
          .collect();
        let page = ListAuditResponse {
          has_more: older.len() > PAGE,
          entries: older.into_iter().take(PAGE).collect(),
        };
        text.push_str(&pager.accept(&page, &keep, names).expect("page"));
      }
      (text, reads)
    }

    /// Newest first, ids `count` down to 1, one second apart.
    fn log_of(count: i64) -> Vec<AuditEntryResponse> {
      (1..=count).rev().map(entry).collect()
    }

    fn entry(n: i64) -> AuditEntryResponse {
      AuditEntryResponse {
        id: AuditEntryId::from_snowflake(Snowflake::new(n).expect("id")),
        maybe_actor_user_id: None,
        action: "space_created".to_owned(),
        maybe_target_kind: Some("space".to_owned()),
        maybe_target_id: Some(n.to_string()),
        detail: "{}".to_owned(),
        created_at: UnixMillis(n * 1000),
      }
    }

    fn names(_: UserId) -> String {
      "Alex".to_owned()
    }

    /// The data rows of a file, without a full CSV parser: the test rows hold no quoted commas.
    fn rows(text: &str) -> Vec<Vec<String>> {
      text
        .lines()
        .skip(1)
        .map(|row| row.split(',').map(str::to_owned).collect())
        .collect()
    }
  }
}
