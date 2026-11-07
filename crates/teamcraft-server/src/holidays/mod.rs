//! Public holidays repeating meetings can skip (#1053, docs/meetings.md, "Holidays").
//!
//! Where the holidays come from is behind [`HolidaySource`] (AGENTS.md, vendor-neutral): today the
//! bundled table of [`bundled`], picked by the region an admin sets in the console. A source answers
//! at once from what it holds: one that fetches (an iCalendar feed) refreshes in the background and
//! answers with its last good copy, or nothing, while it is down, so a source that is away never
//! stops listing meetings or ringing their invitees.

pub mod bundled;

use std::collections::BTreeSet;

use chrono::NaiveDate;
use teamcraft_proto::HolidaySettings;

pub use bundled::{BundledHolidays, HolidayRegion};

/// Where the public holidays of the instance come from.
pub trait HolidaySource: Send + Sync {
  /// The public holidays from `from` to `to`, both included, as dates of the wall clock they are
  /// celebrated on. Never waits for the network.
  fn holidays_between(&self, from: NaiveDate, to: NaiveDate) -> BTreeSet<NaiveDate>;
}

/// The source the settings name; `None` when no region is set (or one this version doesn't know,
/// which validation refuses anyway): nothing is skipped then.
pub fn source_of(settings: &HolidaySettings) -> Option<Box<dyn HolidaySource>> {
  let region = HolidayRegion::from_code(&settings.region)?;
  Some(Box::new(BundledHolidays::new(region)))
}
