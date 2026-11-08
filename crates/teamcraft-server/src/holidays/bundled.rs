//! The bundled holiday table, computed for any year without the network: Germany's public
//! holidays, nationwide and per federal state; Austria's (all of them federal law); Switzerland's
//! federal ones and those of each canton. Movable feasts follow Easter (Gregorian computus).
//!
//! Holidays that only parts of a region keep (Corpus Christi in parts of Saxony and Thuringia,
//! Assumption in Bavaria's Catholic communities, Augsburg's peace festival) are left out: a series
//! of the whole region would skip a day most of it works. For the same reason Austria's state
//! patron days (Saint Leopold in Vienna and so on), which only schools and the public service keep,
//! are not in the table, and neither are the Swiss cantons' optional and half days.

use std::collections::BTreeSet;

use chrono::{Datelike, Duration, NaiveDate, Weekday};
use teamcraft_proto::HOLIDAY_REGIONS;

use super::HolidaySource;

/// Years looked at per question at most: a range wider than this is cut at its end.
const MAX_YEARS: i32 = 100;

/// A region of the table, by its ISO 3166-2 code. A country stands for the holidays all of it
/// keeps; its states or cantons add their own.
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
  /// Austria: its public holidays are federal law, the same in every state.
  Austria,
  /// The holidays all of Switzerland keeps.
  Switzerland,
  Aargau,
  AppenzellInnerrhoden,
  AppenzellAusserrhoden,
  Bern,
  BaselLandschaft,
  BaselStadt,
  Fribourg,
  Geneva,
  Glarus,
  Graubuenden,
  Jura,
  Lucerne,
  Neuchatel,
  Nidwalden,
  Obwalden,
  StGallen,
  Schaffhausen,
  Solothurn,
  Schwyz,
  Thurgau,
  Ticino,
  Uri,
  Vaud,
  Valais,
  Zug,
  Zurich,
}

/// The table's holidays of one region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundledHolidays {
  region: HolidayRegion,
}

/// One holiday of the table: on which day, where, and in which years.
struct Rule {
  day: Day,
  /// The regions that keep it; a country stands for all of it.
  regions: &'static [HolidayRegion],
  years: Years,
  weekdays: Weekdays,
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
  /// Glarus' Näfelser Fahrt: the first Thursday of April, a week later when that is Maundy
  /// Thursday.
  NaefelserFahrt,
  /// Geneva's fast: the Thursday after the first Sunday of September.
  GenevaFast,
  /// Vaud's fast Monday: the day after the third Sunday of September (the federal day of thanks).
  FastMonday,
}

#[derive(Clone, Copy)]
enum Years {
  Since(i32),
  Only(i32),
}

/// On which weekdays a holiday counts: a few cantons keep 2 January or 26 December only when it
/// makes a long weekend.
#[derive(Clone, Copy)]
enum Weekdays {
  Any,
  Only(Weekday),
  Not(&'static [Weekday]),
}

use HolidayRegion::{
  Aargau, AppenzellAusserrhoden, AppenzellInnerrhoden, Austria, BadenWuerttemberg, BaselLandschaft,
  BaselStadt, Bavaria, Berlin, Bern, Brandenburg, Bremen, Fribourg, Geneva, Germany, Glarus, Graubuenden,
  Hamburg, Hesse, Jura, LowerSaxony, Lucerne, MecklenburgVorpommern, Neuchatel, Nidwalden,
  NorthRhineWestphalia, Obwalden, RhinelandPalatinate, Saarland, Saxony, SaxonyAnhalt, Schaffhausen,
  SchleswigHolstein, Schwyz, Solothurn, StGallen, Switzerland, Thurgau, Thuringia, Ticino, Uri, Valais, Vaud,
  Zug, Zurich,
};

const DE: &[HolidayRegion] = &[Germany];
const SINCE_REUNIFICATION: Years = Years::Since(1990);
/// Austria's holidays as they stand since the national day (1967).
const SINCE_NATIONAL_DAY: Years = Years::Since(1967);
/// Switzerland's holidays as the table has them; Jura, the youngest canton, since 1979.
const SINCE_JURA: Years = Years::Since(1979);

const RULES: &[Rule] = &[
  // --- Germany ---
  // New Year's Day.
  fixed(1, 1, DE, SINCE_REUNIFICATION),
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
  easter(-2, DE, SINCE_REUNIFICATION),
  easter(0, &[Brandenburg], SINCE_REUNIFICATION),
  easter(1, DE, SINCE_REUNIFICATION),
  // Labour Day.
  fixed(5, 1, DE, SINCE_REUNIFICATION),
  // The 75th and 80th anniversary of the end of the war, once each.
  fixed(5, 8, &[Berlin], Years::Only(2020)),
  fixed(5, 8, &[Berlin], Years::Only(2025)),
  // Ascension Day, Whit Sunday, Whit Monday, Corpus Christi.
  easter(39, DE, SINCE_REUNIFICATION),
  easter(49, &[Brandenburg], SINCE_REUNIFICATION),
  easter(50, DE, SINCE_REUNIFICATION),
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
    SINCE_REUNIFICATION,
  ),
  // Assumption Day.
  fixed(8, 15, &[Saarland], SINCE_REUNIFICATION),
  // World Children's Day.
  fixed(9, 20, &[Thuringia], Years::Since(2019)),
  // Day of German Unity.
  fixed(10, 3, DE, SINCE_REUNIFICATION),
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
  fixed(10, 31, DE, Years::Only(2017)),
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
  special(Day::Repentance, &[Saxony]),
  // Christmas Day and Boxing Day.
  fixed(12, 25, DE, SINCE_REUNIFICATION),
  fixed(12, 26, DE, SINCE_REUNIFICATION),
  // --- Austria (Feiertagsruhegesetz; Good Friday stopped being one in 2019) ---
  fixed(1, 1, &[Austria], SINCE_NATIONAL_DAY),
  fixed(1, 6, &[Austria], SINCE_NATIONAL_DAY),
  easter(1, &[Austria], SINCE_NATIONAL_DAY),
  fixed(5, 1, &[Austria], SINCE_NATIONAL_DAY),
  easter(39, &[Austria], SINCE_NATIONAL_DAY),
  easter(50, &[Austria], SINCE_NATIONAL_DAY),
  easter(60, &[Austria], SINCE_NATIONAL_DAY),
  fixed(8, 15, &[Austria], SINCE_NATIONAL_DAY),
  // National Day.
  fixed(10, 26, &[Austria], SINCE_NATIONAL_DAY),
  fixed(11, 1, &[Austria], SINCE_NATIONAL_DAY),
  fixed(12, 8, &[Austria], SINCE_NATIONAL_DAY),
  fixed(12, 25, &[Austria], SINCE_NATIONAL_DAY),
  fixed(12, 26, &[Austria], SINCE_NATIONAL_DAY),
  // --- Switzerland: federal (New Year, Ascension, National Day, Christmas) and per canton ---
  fixed(1, 1, &[Switzerland], SINCE_JURA),
  // Berchtold's Day; Neuchâtel only when 1 January is a Sunday.
  fixed(1, 2, &[Aargau, Bern, Jura, Lucerne, Thurgau, Vaud], SINCE_JURA),
  on_weekday(
    fixed(1, 2, &[Neuchatel], SINCE_JURA),
    Weekdays::Only(Weekday::Mon),
  ),
  // Epiphany.
  fixed(1, 6, &[Schwyz, Ticino, Uri], SINCE_JURA),
  // Republic Day of Neuchâtel.
  fixed(3, 1, &[Neuchatel], SINCE_JURA),
  // Saint Joseph.
  fixed(3, 19, &[Nidwalden, Schwyz, Ticino, Uri, Valais], SINCE_JURA),
  special(Day::NaefelserFahrt, &[Glarus]),
  // Good Friday: everywhere but Graubünden, Ticino and Valais.
  easter(
    -2,
    &[
      Aargau,
      AppenzellInnerrhoden,
      AppenzellAusserrhoden,
      Bern,
      BaselLandschaft,
      BaselStadt,
      Fribourg,
      Geneva,
      Glarus,
      Jura,
      Lucerne,
      Neuchatel,
      Nidwalden,
      Obwalden,
      StGallen,
      Schaffhausen,
      Solothurn,
      Schwyz,
      Thurgau,
      Uri,
      Vaud,
      Zug,
      Zurich,
    ],
    SINCE_JURA,
  ),
  // Easter Monday and Whit Monday.
  easter(1, SWISS_EASTER_MONDAY, SINCE_JURA),
  easter(50, SWISS_EASTER_MONDAY, SINCE_JURA),
  // Labour Day.
  fixed(
    5,
    1,
    &[
      Aargau,
      BaselLandschaft,
      BaselStadt,
      Jura,
      Neuchatel,
      Schaffhausen,
      Thurgau,
      Ticino,
      Zurich,
    ],
    SINCE_JURA,
  ),
  easter(39, &[Switzerland], SINCE_JURA),
  // Corpus Christi.
  easter(
    60,
    &[
      Aargau,
      AppenzellInnerrhoden,
      Jura,
      Lucerne,
      Neuchatel,
      Nidwalden,
      Obwalden,
      Schwyz,
      Ticino,
      Uri,
      Valais,
      Zug,
    ],
    SINCE_JURA,
  ),
  // Independence of Jura; Saints Peter and Paul in Ticino.
  fixed(6, 23, &[Jura], SINCE_JURA),
  fixed(6, 29, &[Ticino], SINCE_JURA),
  // National Day.
  fixed(8, 1, &[Switzerland], SINCE_JURA),
  // Assumption Day.
  fixed(8, 15, SWISS_CATHOLIC_FEASTS, SINCE_JURA),
  fixed(8, 15, &[Jura], SINCE_JURA),
  special(Day::GenevaFast, &[Geneva]),
  special(Day::FastMonday, &[Vaud]),
  // Brother Klaus.
  fixed(9, 25, &[Obwalden], SINCE_JURA),
  // All Saints' Day.
  fixed(11, 1, SWISS_CATHOLIC_FEASTS, SINCE_JURA),
  fixed(11, 1, &[Glarus, Jura, StGallen], SINCE_JURA),
  // Immaculate Conception.
  fixed(12, 8, SWISS_CATHOLIC_FEASTS, SINCE_JURA),
  fixed(12, 25, &[Switzerland], SINCE_JURA),
  // Saint Stephen's Day: in the Appenzells and Uri not on a Tuesday or Saturday (so never a lone
  // bridge day), in Neuchâtel only when it is a Monday.
  fixed(
    12,
    26,
    &[
      Aargau,
      Bern,
      BaselLandschaft,
      BaselStadt,
      Glarus,
      Graubuenden,
      Lucerne,
      StGallen,
      Schaffhausen,
      Schwyz,
      Thurgau,
      Ticino,
      Zurich,
    ],
    SINCE_JURA,
  ),
  on_weekday(
    fixed(
      12,
      26,
      &[AppenzellInnerrhoden, AppenzellAusserrhoden, Uri],
      SINCE_JURA,
    ),
    Weekdays::Not(&[Weekday::Tue, Weekday::Sat]),
  ),
  on_weekday(
    fixed(12, 26, &[Neuchatel], SINCE_JURA),
    Weekdays::Only(Weekday::Mon),
  ),
  // Restoration of the Republic of Geneva.
  fixed(12, 31, &[Geneva], SINCE_JURA),
];

/// The cantons with Easter Monday and Whit Monday as public holidays.
const SWISS_EASTER_MONDAY: &[HolidayRegion] = &[
  Aargau,
  AppenzellInnerrhoden,
  AppenzellAusserrhoden,
  Bern,
  BaselLandschaft,
  BaselStadt,
  Geneva,
  Glarus,
  Graubuenden,
  Jura,
  Lucerne,
  StGallen,
  Schaffhausen,
  Schwyz,
  Thurgau,
  Ticino,
  Uri,
  Vaud,
  Zurich,
];

/// The cantons that keep Assumption, All Saints and the Immaculate Conception.
const SWISS_CATHOLIC_FEASTS: &[HolidayRegion] = &[
  Aargau,
  AppenzellInnerrhoden,
  Lucerne,
  Nidwalden,
  Obwalden,
  Schwyz,
  Ticino,
  Uri,
  Valais,
  Zug,
];

impl HolidayRegion {
  /// Every region, in the order of [`HOLIDAY_REGIONS`].
  pub const ALL: [Self; 45] = [
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
    Austria,
    Switzerland,
    Aargau,
    AppenzellInnerrhoden,
    AppenzellAusserrhoden,
    Bern,
    BaselLandschaft,
    BaselStadt,
    Fribourg,
    Geneva,
    Glarus,
    Graubuenden,
    Jura,
    Lucerne,
    Neuchatel,
    Nidwalden,
    Obwalden,
    StGallen,
    Schaffhausen,
    Solothurn,
    Schwyz,
    Thurgau,
    Ticino,
    Uri,
    Vaud,
    Valais,
    Zug,
    Zurich,
  ];

  /// The region of an ISO 3166-2 code (`DE`, `DE-BY`, `CH-ZH`), upper case as the wire has it.
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

  /// The country the region is part of (a country is its own).
  pub fn country(self) -> Self {
    match self.code().get(..2) {
      Some("AT") => Austria,
      Some("CH") => Switzerland,
      _ => Germany,
    }
  }
}

impl BundledHolidays {
  pub fn new(region: HolidayRegion) -> Self {
    Self { region }
  }

  /// The region's holidays of one year.
  fn holidays_of_year(self, year: i32) -> impl Iterator<Item = NaiveDate> {
    let maybe_easter = easter_sunday(year);
    let country = self.region.country();
    RULES
      .iter()
      .filter(move |rule| rule.regions.contains(&self.region) || rule.regions.contains(&country))
      .filter(move |rule| match rule.years {
        Years::Since(first) => year >= first,
        Years::Only(only) => year == only,
      })
      .filter_map(move |rule| {
        let date = match rule.day {
          Day::Fixed { month, day } => NaiveDate::from_ymd_opt(year, month, day),
          Day::Easter(offset) => maybe_easter?.checked_add_signed(Duration::days(offset)),
          Day::Repentance => repentance_day(year),
          Day::NaefelserFahrt => naefelser_fahrt(year, maybe_easter?),
          Day::GenevaFast => nth_weekday(year, 9, Weekday::Sun, 1)?.checked_add_signed(Duration::days(4)),
          Day::FastMonday => nth_weekday(year, 9, Weekday::Sun, 3)?.checked_add_signed(Duration::days(1)),
        }?;
        let counts = match rule.weekdays {
          Weekdays::Any => true,
          Weekdays::Only(weekday) => date.weekday() == weekday,
          Weekdays::Not(weekdays) => !weekdays.contains(&date.weekday()),
        };
        counts.then_some(date)
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
    weekdays: Weekdays::Any,
  }
}

const fn easter(offset: i64, regions: &'static [HolidayRegion], years: Years) -> Rule {
  Rule {
    day: Day::Easter(offset),
    regions,
    years,
    weekdays: Weekdays::Any,
  }
}

const fn special(day: Day, regions: &'static [HolidayRegion]) -> Rule {
  Rule {
    day,
    regions,
    years: SINCE_REUNIFICATION,
    weekdays: Weekdays::Any,
  }
}

const fn on_weekday(rule: Rule, weekdays: Weekdays) -> Rule {
  Rule { weekdays, ..rule }
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

/// The `n`th (from 1) `weekday` of a month.
fn nth_weekday(year: i32, month: u32, weekday: Weekday, n: u8) -> Option<NaiveDate> {
  NaiveDate::from_weekday_of_month_opt(year, month, weekday, n)
}

/// The first Thursday of April, moved a week on when it is Maundy Thursday.
fn naefelser_fahrt(year: i32, easter: NaiveDate) -> Option<NaiveDate> {
  let first = nth_weekday(year, 4, Weekday::Thu, 1)?;
  if Some(first) == easter.checked_sub_signed(Duration::days(3)) {
    first.checked_add_signed(Duration::days(7))
  } else {
    Some(first)
  }
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

  #[test]
  fn austria_keeps_thirteen_holidays_everywhere() {
    assert_eq!(
      in_year(HolidayRegion::Austria, 2026),
      vec![
        (1, 1),
        (1, 6),
        (4, 6),
        (5, 1),
        (5, 14),
        (5, 25),
        (6, 4),
        (8, 15),
        (10, 26),
        (11, 1),
        (12, 8),
        (12, 25),
        (12, 26),
      ]
    );
  }

  #[test]
  fn switzerland_keeps_four_federal_holidays_and_cantons_add_theirs() {
    assert_eq!(
      in_year(HolidayRegion::Switzerland, 2026),
      vec![(1, 1), (5, 14), (8, 1), (12, 25)]
    );
    assert_eq!(
      in_year(HolidayRegion::Zurich, 2026),
      vec![
        (1, 1),
        (4, 3),
        (4, 6),
        (5, 1),
        (5, 14),
        (5, 25),
        (8, 1),
        (12, 25),
        (12, 26),
      ]
    );
    let ticino = in_year(HolidayRegion::Ticino, 2026);
    assert!(ticino.contains(&(6, 29)) && !ticino.contains(&(4, 3)));
    let jura = in_year(HolidayRegion::Jura, 2026);
    assert!(jura.contains(&(6, 23)) && jura.contains(&(11, 1)) && !jura.contains(&(12, 8)));
    for region in HolidayRegion::ALL
      .into_iter()
      .filter(|region| region.code().starts_with("CH-"))
    {
      let found = in_year(region, 2026);
      for federal in [(1, 1), (5, 14), (8, 1), (12, 25)] {
        assert!(found.contains(&federal), "{} {federal:?}", region.code());
      }
    }
  }

  #[test]
  fn the_swiss_moving_days_fall_where_the_cantons_keep_them() {
    // Geneva's fast: the Thursday after the first Sunday of September; Vaud's: the Monday after
    // the third.
    assert!(in_year(HolidayRegion::Geneva, 2026).contains(&(9, 10)));
    assert!(in_year(HolidayRegion::Vaud, 2026).contains(&(9, 21)));
    // Näfelser Fahrt moves off Maundy Thursday (2 April 2026), not otherwise.
    assert!(in_year(HolidayRegion::Glarus, 2026).contains(&(4, 9)));
    assert!(in_year(HolidayRegion::Glarus, 2027).contains(&(4, 1)));
  }

  #[test]
  fn some_cantons_keep_a_day_only_on_some_weekdays() {
    // Neuchâtel: 26 December and 2 January only on a Monday.
    assert!(in_year(HolidayRegion::Neuchatel, 2022).contains(&(12, 26)));
    assert!(!in_year(HolidayRegion::Neuchatel, 2026).contains(&(12, 26)));
    assert!(in_year(HolidayRegion::Neuchatel, 2023).contains(&(1, 2)));
    assert!(!in_year(HolidayRegion::Neuchatel, 2026).contains(&(1, 2)));
    // Uri: not on a Saturday (2026) but on a Friday (2025).
    assert!(!in_year(HolidayRegion::Uri, 2026).contains(&(12, 26)));
    assert!(in_year(HolidayRegion::Uri, 2025).contains(&(12, 26)));
  }

  #[test]
  fn every_region_belongs_to_its_country() {
    assert_eq!(HolidayRegion::Zurich.country(), HolidayRegion::Switzerland);
    assert_eq!(HolidayRegion::Austria.country(), HolidayRegion::Austria);
    assert_eq!(HolidayRegion::Bavaria.country(), HolidayRegion::Germany);
    // A country's own holidays are none of its neighbours'.
    assert!(!in_year(HolidayRegion::Austria, 2026).contains(&(10, 3)));
    assert!(!in_year(HolidayRegion::Germany, 2026).contains(&(8, 1)));
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
