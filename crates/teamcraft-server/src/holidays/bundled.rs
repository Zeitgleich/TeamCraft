//! The bundled holiday table: Germany's public holidays, nationwide and per federal state, computed
//! for any year without the network. Movable feasts follow Easter (Gregorian computus); repentance
//! day is the Wednesday before 23 November.
//!
//! Holidays that only parts of a state keep (Corpus Christi in parts of Saxony and Thuringia,
//! Assumption in Bavaria's Catholic communities, Augsburg's peace festival) are left out: a series
//! of the whole state would skip a day most of it works.

use std::collections::BTreeSet;

use chrono::{Datelike, Duration, NaiveDate, Weekday};
use teamcraft_proto::HOLIDAY_REGIONS;

use super::HolidaySource;

/// Years looked at per question at most: a range wider than this is cut at its end.
const MAX_YEARS: i32 = 100;

/// A region of the table, by its ISO 3166-2 code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HolidayRegion {
  /// The holidays all of Germany keeps.
  Germany,
  BadenWuerttemberg,
  Bavaria,
  Berlin,
  Brandenburg,
  Bremen,
  Hamburg,
  Hesse,
  MecklenburgVorpommern,
  LowerSaxony,
  NorthRhineWestphalia,
  RhinelandPalatinate,
  Saarland,
  Saxony,
  SaxonyAnhalt,
  SchleswigHolstein,
  Thuringia,
}

/// The table's holidays of one region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundledHolidays {
  region: HolidayRegion,
}

/// One holiday of the table: on which day, where, and in which years.
struct Rule {
  day: Day,
  /// The federal states that keep it; empty: all of Germany.
  regions: &'static [HolidayRegion],
  years: Years,
}

#[derive(Clone, Copy)]
enum Day {
  Fixed {
    month: u32,
    day: u32,
  },
  /// Days after Easter Sunday (negative: before).
  Easter(i64),
  /// The Wednesday before 23 November.
  Repentance,
}

#[derive(Clone, Copy)]
enum Years {
  Since(i32),
  Only(i32),
}

use HolidayRegion::{
  BadenWuerttemberg, Bavaria, Berlin, Brandenburg, Bremen, Hamburg, Hesse, LowerSaxony,
  MecklenburgVorpommern, NorthRhineWestphalia, RhinelandPalatinate, Saarland, Saxony, SaxonyAnhalt,
  SchleswigHolstein, Thuringia,
};

const EVERYWHERE: &[HolidayRegion] = &[];
const SINCE_REUNIFICATION: Years = Years::Since(1990);

const RULES: &[Rule] = &[
  // New Year's Day.
  fixed(1, 1, EVERYWHERE, SINCE_REUNIFICATION),
  // Epiphany.
  fixed(
    1,
    6,
    &[BadenWuerttemberg, Bavaria, SaxonyAnhalt],
    SINCE_REUNIFICATION,
  ),
  // International Women's Day.
  fixed(3, 8, &[Berlin], Years::Since(2019)),
  fixed(3, 8, &[MecklenburgVorpommern], Years::Since(2023)),
  // Good Friday, Easter Sunday, Easter Monday.
  easter(-2, EVERYWHERE),
  easter(0, &[Brandenburg]),
  easter(1, EVERYWHERE),
  // Labour Day.
  fixed(5, 1, EVERYWHERE, SINCE_REUNIFICATION),
  // The 75th and 80th anniversary of the end of the war, once each.
  fixed(5, 8, &[Berlin], Years::Only(2020)),
  fixed(5, 8, &[Berlin], Years::Only(2025)),
  // Ascension Day, Whit Sunday, Whit Monday, Corpus Christi.
  easter(39, EVERYWHERE),
  easter(49, &[Brandenburg]),
  easter(50, EVERYWHERE),
  easter(
    60,
    &[
      BadenWuerttemberg,
      Bavaria,
      Hesse,
      NorthRhineWestphalia,
      RhinelandPalatinate,
      Saarland,
    ],
  ),
  // Assumption Day.
  fixed(8, 15, &[Saarland], SINCE_REUNIFICATION),
  // World Children's Day.
  fixed(9, 20, &[Thuringia], Years::Since(2019)),
  // Day of German Unity.
  fixed(10, 3, EVERYWHERE, SINCE_REUNIFICATION),
  // Reformation Day: all of Germany for its 500th anniversary.
  fixed(
    10,
    31,
    &[
      Brandenburg,
      MecklenburgVorpommern,
      Saxony,
      SaxonyAnhalt,
      Thuringia,
    ],
    SINCE_REUNIFICATION,
  ),
  fixed(
    10,
    31,
    &[Bremen, Hamburg, LowerSaxony, SchleswigHolstein],
    Years::Since(2018),
  ),
  fixed(10, 31, EVERYWHERE, Years::Only(2017)),
  // All Saints' Day.
  fixed(
    11,
    1,
    &[
      BadenWuerttemberg,
      Bavaria,
      NorthRhineWestphalia,
      RhinelandPalatinate,
      Saarland,
    ],
    SINCE_REUNIFICATION,
  ),
  // Day of Repentance and Prayer.
  Rule {
    day: Day::Repentance,
    regions: &[Saxony],
    years: SINCE_REUNIFICATION,
  },
  // Christmas Day and Boxing Day.
  fixed(12, 25, EVERYWHERE, SINCE_REUNIFICATION),
  fixed(12, 26, EVERYWHERE, SINCE_REUNIFICATION),
];

impl HolidayRegion {
  /// Every region, in the order of [`HOLIDAY_REGIONS`].
  pub const ALL: [Self; 17] = [
    Self::Germany,
    BadenWuerttemberg,
    Bavaria,
    Berlin,
    Brandenburg,
    Bremen,
    Hamburg,
    Hesse,
    MecklenburgVorpommern,
    LowerSaxony,
    NorthRhineWestphalia,
    RhinelandPalatinate,
    Saarland,
    Saxony,
    SaxonyAnhalt,
    SchleswigHolstein,
    Thuringia,
  ];

  /// The region of an ISO 3166-2 code (`DE`, `DE-BY`), upper case as the wire has it.
  pub fn from_code(code: &str) -> Option<Self> {
    let index = HOLIDAY_REGIONS.iter().position(|known| *known == code)?;
    Self::ALL.get(index).copied()
  }

  /// The ISO 3166-2 code.
  pub fn code(self) -> &'static str {
    let index = Self::ALL
      .iter()
      .position(|region| *region == self)
      .unwrap_or_default();
    HOLIDAY_REGIONS[index]
  }
}

impl BundledHolidays {
  pub fn new(region: HolidayRegion) -> Self {
    Self { region }
  }

  /// The region's holidays of one year.
  fn holidays_of_year(self, year: i32) -> impl Iterator<Item = NaiveDate> {
    let maybe_easter = easter_sunday(year);
    RULES
      .iter()
      .filter(move |rule| rule.regions.is_empty() || rule.regions.contains(&self.region))
      .filter(move |rule| match rule.years {
        Years::Since(first) => year >= first,
        Years::Only(only) => year == only,
      })
      .filter_map(move |rule| match rule.day {
        Day::Fixed { month, day } => NaiveDate::from_ymd_opt(year, month, day),
        Day::Easter(offset) => maybe_easter?.checked_add_signed(Duration::days(offset)),
        Day::Repentance => repentance_day(year),
      })
  }
}

impl HolidaySource for BundledHolidays {
  fn holidays_between(&self, from: NaiveDate, to: NaiveDate) -> BTreeSet<NaiveDate> {
    let last_year = to.year().min(from.year().saturating_add(MAX_YEARS - 1));
    (from.year()..=last_year)
      .flat_map(|year| self.holidays_of_year(year))
      .filter(|date| (from..=to).contains(date))
      .collect()
  }
}

const fn fixed(month: u32, day: u32, regions: &'static [HolidayRegion], years: Years) -> Rule {
  Rule {
    day: Day::Fixed { month, day },
    regions,
    years,
  }
}

const fn easter(offset: i64, regions: &'static [HolidayRegion]) -> Rule {
  Rule {
    day: Day::Easter(offset),
    regions,
    years: SINCE_REUNIFICATION,
  }
}

/// Easter Sunday of the Gregorian calendar (the anonymous computus of Meeus, Jones and Butcher).
fn easter_sunday(year: i32) -> Option<NaiveDate> {
  let golden = year.rem_euclid(19);
  let (century, of_century) = (year.div_euclid(100), year.rem_euclid(100));
  let (leap_centuries, century_rest) = (century / 4, century % 4);
  let correction = (century + 8) / 25;
  let moon = (century - correction + 1) / 3;
  let epact = (19 * golden + century - leap_centuries - moon + 15) % 30;
  let (leap_years, year_rest) = (of_century / 4, of_century % 4);
  let weekday = (32 + 2 * century_rest + 2 * leap_years - epact - year_rest).rem_euclid(7);
  let shift = (golden + 11 * epact + 22 * weekday) / 451;
  let month = (epact + weekday - 7 * shift + 114) / 31;
  let day = (epact + weekday - 7 * shift + 114) % 31 + 1;
  NaiveDate::from_ymd_opt(year, u32::try_from(month).ok()?, u32::try_from(day).ok()?)
}

/// The Wednesday before 23 November: between the 16th and the 22nd.
fn repentance_day(year: i32) -> Option<NaiveDate> {
  let before = NaiveDate::from_ymd_opt(year, 11, 22)?;
  let back = (7 + before.weekday().num_days_from_monday() - Weekday::Wed.num_days_from_monday()) % 7;
  before.checked_sub_signed(Duration::days(i64::from(back)))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn easter_sunday_matches_known_years() {
    for (year, month, day) in [
      (1990, 4, 15),
      (2000, 4, 23),
      (2008, 3, 23),
      (2011, 4, 24),
      (2019, 4, 21),
      (2024, 3, 31),
      (2025, 4, 20),
      (2026, 4, 5),
      (2027, 3, 28),
      (2038, 4, 25),
    ] {
      assert_eq!(
        easter_sunday(year),
        NaiveDate::from_ymd_opt(year, month, day),
        "{year}"
      );
    }
  }

  #[test]
  fn repentance_day_is_the_wednesday_before_the_23rd_of_november() {
    assert_eq!(repentance_day(2026), NaiveDate::from_ymd_opt(2026, 11, 18));
    // The 22nd itself is a Wednesday.
    assert_eq!(repentance_day(2028), NaiveDate::from_ymd_opt(2028, 11, 22));
    assert_eq!(repentance_day(2023), NaiveDate::from_ymd_opt(2023, 11, 22));
    assert_eq!(repentance_day(2024), NaiveDate::from_ymd_opt(2024, 11, 20));
  }

  #[test]
  fn germany_keeps_nine_holidays_a_year() {
    let found = in_year(HolidayRegion::Germany, 2026);
    assert_eq!(
      found,
      vec![
        (1, 1),
        (4, 3),
        (4, 6),
        (5, 1),
        (5, 14),
        (5, 25),
        (10, 3),
        (12, 25),
        (12, 26),
      ]
    );
  }

  #[test]
  fn states_add_their_own() {
    let bavaria = in_year(HolidayRegion::Bavaria, 2026);
    for day in [(1, 6), (6, 4), (11, 1)] {
      assert!(bavaria.contains(&day), "{day:?}");
    }
    assert!(!bavaria.contains(&(10, 31)));
    let saxony = in_year(HolidayRegion::Saxony, 2026);
    assert!(saxony.contains(&(10, 31)) && saxony.contains(&(11, 18)));
    let brandenburg = in_year(HolidayRegion::Brandenburg, 2026);
    assert!(brandenburg.contains(&(4, 5)) && brandenburg.contains(&(5, 24)));
    assert_eq!(in_year(HolidayRegion::Hamburg, 2026).len(), 10);
  }

  #[test]
  fn holidays_start_and_end_in_the_years_they_were_made() {
    assert!(!in_year(HolidayRegion::Berlin, 2018).contains(&(3, 8)));
    assert!(in_year(HolidayRegion::Berlin, 2019).contains(&(3, 8)));
    assert!(in_year(HolidayRegion::Berlin, 2025).contains(&(5, 8)));
    assert!(!in_year(HolidayRegion::Berlin, 2026).contains(&(5, 8)));
    assert!(in_year(HolidayRegion::Germany, 2017).contains(&(10, 31)));
    assert!(!in_year(HolidayRegion::Germany, 2018).contains(&(10, 31)));
    assert!(!in_year(HolidayRegion::Bremen, 2017 - 1).contains(&(10, 31)));
  }

  #[test]
  fn a_range_includes_both_ends_and_spans_years() {
    let table = BundledHolidays::new(HolidayRegion::Germany);
    let found = table.holidays_between(date(2026, 12, 26), date(2027, 1, 1));
    assert_eq!(
      found.into_iter().collect::<Vec<_>>(),
      vec![date(2026, 12, 26), date(2027, 1, 1)]
    );
    assert!(
      table
        .holidays_between(date(2026, 1, 2), date(2026, 1, 5))
        .is_empty()
    );
  }

  #[test]
  fn a_huge_range_stops() {
    let table = BundledHolidays::new(HolidayRegion::Germany);
    let found = table.holidays_between(NaiveDate::MIN, NaiveDate::MAX);
    assert!(found.len() <= 9 * usize::try_from(MAX_YEARS).unwrap() + 1);
  }

  #[test]
  fn codes_are_the_wire_list() {
    let codes: Vec<&str> = HolidayRegion::ALL.iter().map(|region| region.code()).collect();
    assert_eq!(codes, HOLIDAY_REGIONS);
    for code in HOLIDAY_REGIONS {
      assert_eq!(
        HolidayRegion::from_code(code).map(HolidayRegion::code),
        Some(code)
      );
    }
    assert_eq!(HolidayRegion::from_code("de-by"), None);
    assert_eq!(HolidayRegion::from_code(""), None);
  }

  fn in_year(region: HolidayRegion, year: i32) -> Vec<(u32, u32)> {
    BundledHolidays::new(region)
      .holidays_between(date(year, 1, 1), date(year, 12, 31))
      .into_iter()
      .map(|day| (day.month(), day.day()))
      .collect()
  }

  fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
  }
}
