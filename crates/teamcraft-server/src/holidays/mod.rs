//! Public holidays repeating meetings can skip (#1053, docs/meetings.md, "Holidays").
//!
//! Where the holidays come from is behind [`HolidaySource`] (AGENTS.md, vendor-neutral): the
//! bundled table of [`bundled`], picked by the region an admin sets in the console or by the
//! organizer's own region (#1441), and an iCalendar feed ([`feed`], #1426), instead of the region
//! or on top of it. A source answers at once from what it holds: the feed refreshes in the
//! background and answers with its last good copy, or nothing, while it is down, so a source that
//! is away never stops listing meetings or ringing their invitees. When the holidays change, the
//! calendar events of the series that skip them are written again ([`resync`]).

pub mod bundled;
pub mod feed;
pub mod ics;
pub mod resync;

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::NaiveDate;
use teamcraft_proto::HolidaySettings;

pub use bundled::{BundledHolidays, HolidayRegion};
pub use feed::{FeedCopy, HolidayFeed};

use crate::state::AppState;

/// Where the public holidays of the instance come from.
pub trait HolidaySource: Send + Sync {
  /// The public holidays from `from` to `to`, both included, as dates of the wall clock they are
  /// celebrated on. Never waits for the network.
  fn holidays_between(&self, from: NaiveDate, to: NaiveDate) -> BTreeSet<NaiveDate>;
}

/// The region's table and the feed's copy together.
struct InstanceHolidays {
  maybe_table: Option<BundledHolidays>,
  maybe_copy: Option<Arc<FeedCopy>>,
}

impl HolidaySource for InstanceHolidays {
  fn holidays_between(&self, from: NaiveDate, to: NaiveDate) -> BTreeSet<NaiveDate> {
    let mut holidays = self
      .maybe_table
      .map(|table| table.holidays_between(from, to))
      .unwrap_or_default();
    if let Some(copy) = &self.maybe_copy {
      holidays.extend(copy.holidays_between(from, to));
    }
    holidays
  }
}

/// The holidays of an organizer as they stand now: their own region (`maybe_own_region`), else
/// the settings' region, and the feed's last good copy on top of either (company-wide closing
/// days). `None` when there are none: nothing is skipped then.
pub fn source_of(state: &AppState, maybe_own_region: Option<&str>) -> Option<Box<dyn HolidaySource>> {
  let settings = &state.settings.get().holidays;
  let maybe_copy = settings
    .maybe_feed_url
    .as_deref()
    .and_then(|url| state.holiday_feed.copy_for(url));
  source_from(settings, maybe_own_region, maybe_copy)
}

/// [`source_of`] from the settings, the organizer's own region and the copy of the feed. A
/// region this version doesn't know (which validation refuses anyway) is no table; an own region
/// like that falls back to the settings' one.
pub fn source_from(
  settings: &HolidaySettings,
  maybe_own_region: Option<&str>,
  maybe_copy: Option<Arc<FeedCopy>>,
) -> Option<Box<dyn HolidaySource>> {
  let maybe_table = maybe_own_region
    .and_then(HolidayRegion::from_code)
    .or_else(|| HolidayRegion::from_code(&settings.region))
    .map(BundledHolidays::new);
  if maybe_table.is_none() && maybe_copy.is_none() {
    return None;
  }
  Some(Box::new(InstanceHolidays {
    maybe_table,
    maybe_copy,
  }))
}

#[cfg(test)]
mod tests {
  use time::OffsetDateTime;

  use super::*;

  fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
  }

  #[test]
  fn the_region_and_the_feed_count_together() {
    let copy = Arc::new(FeedCopy {
      url: "https://a.example.com/x.ics".into(),
      fetched_at: OffsetDateTime::UNIX_EPOCH,
      dates: [date(2026, 12, 24), date(2026, 12, 31)].into(),
    });
    let settings = HolidaySettings {
      region: "DE".into(),
      maybe_feed_url: Some(copy.url.clone()),
      ..HolidaySettings::default()
    };
    let source = source_from(&settings, None, Some(copy.clone())).unwrap();
    assert_eq!(
      source.holidays_between(date(2026, 12, 20), date(2026, 12, 31)),
      [
        date(2026, 12, 24),
        date(2026, 12, 25),
        date(2026, 12, 26),
        date(2026, 12, 31)
      ]
      .into()
    );
    // The feed alone.
    let feed_only = HolidaySettings {
      region: String::new(),
      ..settings.clone()
    };
    let source = source_from(&feed_only, None, Some(copy.clone())).unwrap();
    assert_eq!(
      source
        .holidays_between(date(2026, 12, 25), date(2026, 12, 26))
        .len(),
      0
    );
    // An organizer's own region replaces the settings' one; the feed still counts.
    let own = source_from(&settings, Some("CH-ZH"), Some(copy)).unwrap();
    let days = own.holidays_between(date(2026, 12, 20), date(2026, 12, 31));
    assert!(days.contains(&date(2026, 12, 24)) && days.contains(&date(2026, 12, 25)));
    // Corpus Christi is Bavaria's holiday, not Zurich's.
    let corpus_christi = date(2026, 6, 4);
    let bavaria = source_from(&settings, Some("DE-BY"), None).unwrap();
    assert!(
      bavaria
        .holidays_between(corpus_christi, corpus_christi)
        .contains(&corpus_christi)
    );
    assert!(own.holidays_between(corpus_christi, corpus_christi).is_empty());
    // An own region this version doesn't know is the settings' region.
    let unknown = source_from(&settings, Some("XX"), None).unwrap();
    assert_eq!(
      unknown
        .holidays_between(date(2026, 12, 25), date(2026, 12, 25))
        .len(),
      1
    );
    // Neither: nothing is skipped, unless the organizer has a region of their own.
    assert!(source_from(&HolidaySettings::default(), None, None).is_none());
    assert!(source_from(&HolidaySettings::default(), Some("AT"), None).is_some());
  }
}
