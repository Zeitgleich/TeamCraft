//! The emoji the pickers offer: Unicode's full set (the one Noto Color Emoji draws) in Unicode's
//! groups and order, with CLDR names and gemoji `:shortcodes:` as search words, and skin tones.
//!
//! The table is generated text (`emoji_catalog.tsv`, from the `emojis` crate; the test
//! `catalog_file_is_current` writes it with `UPDATE_EMOJI_CATALOG=1`) and parsed the first time it
//! is used: one string compresses far better than a table of structs, which keeps the web download
//! small (robustness.md, "Download size").

use std::collections::HashMap;
use std::sync::LazyLock;

/// One emoji per line: emoji, group, emoji version, shortcodes, name[, skin tones, their version].
const CATALOG_TSV: &str = include_str!("emoji_catalog.tsv");

/// The emoji a team chat uses most, in that order, with extra search words (the first word is a
/// shortcode alias, as people know it from other chat apps). They fill "frequently used" until the
/// user has their own and come first in search results.
pub const FAVORITES: [(&str, &str); 120] = [
  ("👍", "thumbsup +1 yes like ok"),
  ("👎", "thumbsdown -1 no dislike"),
  ("❤️", "heart love"),
  ("😂", "joy laugh tears lol"),
  ("🎉", "tada party celebrate hooray"),
  ("🙏", "pray thanks please"),
  ("👀", "eyes look watching"),
  ("✅", "check done yes complete"),
  ("🚀", "rocket ship launch release"),
  ("🔥", "fire hot lit"),
  ("👏", "clap applause bravo"),
  ("💯", "100 hundred perfect"),
  ("🙌", "raised_hands hooray yay"),
  ("😄", "smile happy"),
  ("😀", "grinning happy"),
  ("😁", "grin happy"),
  ("🙂", "slightly_smiling smile"),
  ("😉", "wink"),
  ("😊", "blush happy"),
  ("😍", "heart_eyes love"),
  ("🤩", "star_struck wow"),
  ("😎", "sunglasses cool"),
  ("🤔", "thinking hmm"),
  ("🤨", "raised_eyebrow doubt"),
  ("😐", "neutral meh"),
  ("😅", "sweat_smile phew"),
  ("🤣", "rofl laugh"),
  ("😆", "laughing xd"),
  ("😇", "innocent angel"),
  ("🙃", "upside_down silly"),
  ("😬", "grimacing awkward"),
  ("😮", "open_mouth wow surprised"),
  ("😱", "scream shocked"),
  ("😢", "cry sad tear"),
  ("😭", "sob crying"),
  ("😡", "rage angry"),
  ("😤", "triumph huff"),
  ("😴", "sleeping tired zzz"),
  ("🤯", "exploding_head mind_blown"),
  ("🥳", "partying celebrate birthday"),
  ("🤗", "hugging hug"),
  ("🤝", "handshake deal agree"),
  ("🫡", "salute respect"),
  ("🤞", "crossed_fingers luck hope"),
  ("✌️", "v peace victory"),
  ("👌", "ok_hand perfect fine"),
  ("👋", "wave hello hi bye"),
  ("💪", "muscle strong flex"),
  ("🙈", "see_no_evil oops monkey"),
  ("🤷", "shrug dunno whatever"),
  ("🤦", "facepalm"),
  ("👉", "point_right"),
  ("👆", "point_up this"),
  ("☝️", "point_up_2 one"),
  ("✍️", "writing note"),
  ("🧠", "brain smart"),
  ("💡", "bulb idea light"),
  ("⭐", "star favourite"),
  ("✨", "sparkles new shiny"),
  ("⚡", "zap lightning fast"),
  ("💥", "boom crash"),
  ("💬", "speech_balloon comment chat"),
  ("📌", "pushpin pin"),
  ("📎", "paperclip attachment"),
  ("📝", "memo note write"),
  ("📅", "calendar date"),
  ("⏰", "alarm_clock time"),
  ("⏳", "hourglass waiting"),
  ("📈", "chart_up growth"),
  ("📉", "chart_down decline"),
  ("📊", "bar_chart stats"),
  ("📣", "mega announcement"),
  ("🔔", "bell notification"),
  ("🔕", "no_bell mute"),
  ("🔒", "lock secure private"),
  ("🔑", "key"),
  ("🛠️", "tools fix"),
  ("🔧", "wrench fix"),
  ("⚙️", "gear settings"),
  ("🐛", "bug"),
  ("🧪", "test_tube experiment"),
  ("💻", "computer laptop"),
  ("🖥️", "desktop screen"),
  ("📱", "phone mobile"),
  ("☎️", "telephone call"),
  ("📞", "phone_receiver call"),
  ("🎧", "headphones listening"),
  ("🎥", "movie_camera video"),
  ("📷", "camera photo"),
  ("📦", "package box ship"),
  ("🚧", "construction wip"),
  ("🚨", "rotating_light alert incident"),
  ("⚠️", "warning caution"),
  ("❌", "x no wrong cancel"),
  ("⛔", "no_entry stop blocked"),
  ("❓", "question"),
  ("❗", "exclamation important"),
  ("➕", "plus add"),
  ("🆗", "ok"),
  ("🆕", "new"),
  ("🏁", "checkered_flag finish done"),
  ("🎯", "dart target goal"),
  ("🏆", "trophy win"),
  ("🥇", "first_place gold"),
  ("🎂", "birthday cake"),
  ("🍕", "pizza food lunch"),
  ("🍻", "beers cheers"),
  ("☕", "coffee break"),
  ("🍪", "cookie"),
  ("🌴", "palm_tree vacation holiday"),
  ("🏖️", "beach vacation"),
  ("🤒", "sick ill"),
  ("🏠", "house home"),
  ("🚗", "car commute"),
  ("✈️", "airplane travel flight"),
  ("🌞", "sun sunny"),
  ("🌧️", "rain"),
  ("❄️", "snowflake cold"),
  ("🌈", "rainbow"),
  ("🐢", "turtle slow"),
];

static CATALOG: LazyLock<Catalog> = LazyLock::new(|| Catalog::parse(CATALOG_TSV));

/// One emoji of the catalog.
#[derive(Debug)]
pub struct CatalogEmoji {
  pub emoji: &'static str,
  pub group: EmojiGroup,
  /// The emoji version (UTS #51) that added it: a device's font may not draw newer ones.
  pub version: EmojiVersion,
  /// The CLDR name ("grinning face").
  pub name: &'static str,
  shortcodes: &'static str,
  maybe_skin_tones: Option<SkinTones>,
}

/// Unicode's emoji groups: the pickers' categories, in Unicode's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EmojiGroup {
  SmileysAndEmotion,
  PeopleAndBody,
  AnimalsAndNature,
  FoodAndDrink,
  TravelAndPlaces,
  Activities,
  Objects,
  Symbols,
  Flags,
}

/// An emoji version (UTS #51), e.g. 15.1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EmojiVersion {
  major: u8,
  minor: u8,
}

/// The skin tone the pickers put on emoji that have one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SkinTone {
  /// The yellow default.
  #[default]
  Default,
  Light,
  MediumLight,
  Medium,
  MediumDark,
  Dark,
}

/// The parsed table with its lookups.
struct Catalog {
  entries: Vec<CatalogEmoji>,
  /// Every emoji, its skin tones and their forms without U+FE0F → index into `entries`.
  by_emoji: HashMap<String, usize>,
  /// Gemoji shortcodes and the favorites' aliases → index into `entries`.
  by_shortcode: HashMap<&'static str, usize>,
  /// Index into `entries` → position in [`FAVORITES`].
  favorite_rank: HashMap<usize, usize>,
}

/// The five skin tones of an emoji (light to dark) and the emoji version that added them.
#[derive(Debug)]
struct SkinTones {
  variants: [&'static str; 5],
  version: EmojiVersion,
}

/// How well an emoji matches a search, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
  /// The query is one of its shortcodes.
  Shortcode,
  /// One of the [`FAVORITES`].
  Favorite,
  /// A shortcode starts with the query.
  ShortcodePrefix,
  Other,
}

impl CatalogEmoji {
  /// The main gemoji shortcode (`thumbsup`), for labels.
  pub fn shortcode(&self) -> Option<&'static str> {
    self.shortcodes().next()
  }

  pub fn shortcodes(&self) -> impl Iterator<Item = &'static str> {
    self.shortcodes.split(' ').filter(|code| !code.is_empty())
  }

  pub fn has_skin_tones(&self) -> bool {
    self.maybe_skin_tones.is_some()
  }

  /// The emoji in `tone`; itself for the default tone and emoji without skin tones.
  pub fn with_skin_tone(&self, tone: SkinTone) -> &'static str {
    match (&self.maybe_skin_tones, tone.index()) {
      (Some(tones), Some(index)) => tones.variants[index],
      _ => self.emoji,
    }
  }

  /// The emoji version that added its skin tones, if it has them.
  pub fn skin_tone_version(&self) -> Option<EmojiVersion> {
    self.maybe_skin_tones.as_ref().map(|tones| tones.version)
  }

  /// How it matches `words` (lowercase, non-empty), or `None`.
  fn rank(&self, words: &[String], maybe_favorite_words: Option<&'static str>) -> Option<Rank> {
    let is_match = words.iter().all(|word| {
      self.emoji == word
        || self
          .search_words()
          .any(|candidate| starts_with_ignoring_case(candidate, word))
        || maybe_favorite_words.is_some_and(|extra| {
          extra
            .split(' ')
            .any(|candidate| candidate.starts_with(word.as_str()))
        })
    });
    if !is_match {
      return None;
    }
    let joined = words.join("_");
    let is_alias = maybe_favorite_words.and_then(|extra| extra.split(' ').next()) == Some(joined.as_str());
    Some(if is_alias || self.shortcodes().any(|code| code == joined) {
      Rank::Shortcode
    } else if maybe_favorite_words.is_some() {
      Rank::Favorite
    } else if self.shortcodes().any(|code| code.starts_with(joined.as_str())) {
      Rank::ShortcodePrefix
    } else {
      Rank::Other
    })
  }

  /// Its shortcodes, their parts and the words of its name.
  fn search_words(&self) -> impl Iterator<Item = &'static str> {
    self
      .shortcodes()
      .chain(self.shortcodes.split(['_', ' ']))
      .chain(self.name.split(|character: char| !character.is_alphanumeric()))
      .filter(|word| !word.is_empty())
  }
}

impl EmojiGroup {
  pub const ALL: [Self; 9] = [
    Self::SmileysAndEmotion,
    Self::PeopleAndBody,
    Self::AnimalsAndNature,
    Self::FoodAndDrink,
    Self::TravelAndPlaces,
    Self::Activities,
    Self::Objects,
    Self::Symbols,
    Self::Flags,
  ];

  /// A stable name, for i18n keys (`emoji-group-smileys`) and element ids.
  pub fn slug(self) -> &'static str {
    match self {
      Self::SmileysAndEmotion => "smileys",
      Self::PeopleAndBody => "people",
      Self::AnimalsAndNature => "nature",
      Self::FoodAndDrink => "food",
      Self::TravelAndPlaces => "travel",
      Self::Activities => "activities",
      Self::Objects => "objects",
      Self::Symbols => "symbols",
      Self::Flags => "flags",
    }
  }
}

impl EmojiVersion {
  pub const fn new(major: u8, minor: u8) -> Self {
    Self { major, minor }
  }

  /// `"15.1"` as a version.
  pub fn parse(text: &str) -> Option<Self> {
    let (major, minor) = text.split_once('.')?;
    Some(Self::new(major.parse().ok()?, minor.parse().ok()?))
  }
}

impl SkinTone {
  pub const ALL: [Self; 6] = [
    Self::Default,
    Self::Light,
    Self::MediumLight,
    Self::Medium,
    Self::MediumDark,
    Self::Dark,
  ];

  /// A stable name, for settings and i18n keys (`emoji-tone-medium-dark`).
  pub fn slug(self) -> &'static str {
    match self {
      Self::Default => "default",
      Self::Light => "light",
      Self::MediumLight => "medium-light",
      Self::Medium => "medium",
      Self::MediumDark => "medium-dark",
      Self::Dark => "dark",
    }
  }

  pub fn from_slug(slug: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|tone| tone.slug() == slug)
  }

  /// Its place among an emoji's five skin tones; `None` for the default.
  fn index(self) -> Option<usize> {
    match self {
      Self::Default => None,
      Self::Light => Some(0),
      Self::MediumLight => Some(1),
      Self::Medium => Some(2),
      Self::MediumDark => Some(3),
      Self::Dark => Some(4),
    }
  }
}

impl Catalog {
  fn parse(tsv: &'static str) -> Self {
    let entries: Vec<CatalogEmoji> = tsv.lines().filter_map(parse_line).collect();
    let mut by_emoji = HashMap::with_capacity(entries.len() * 2);
    let mut by_shortcode = HashMap::with_capacity(entries.len() * 2);
    for (index, entry) in entries.iter().enumerate() {
      let tones = entry.maybe_skin_tones.iter().flat_map(|tones| tones.variants);
      for emoji in std::iter::once(entry.emoji).chain(tones) {
        by_emoji.entry(emoji.to_owned()).or_insert(index);
        by_emoji.entry(without_variation_selector(emoji)).or_insert(index);
      }
      for code in entry.shortcodes() {
        by_shortcode.entry(code).or_insert(index);
      }
    }
    let mut favorite_rank = HashMap::with_capacity(FAVORITES.len());
    for (rank, (emoji, words)) in FAVORITES.iter().enumerate() {
      if let Some(&index) = by_emoji.get(*emoji) {
        favorite_rank.insert(index, rank);
        if let Some(alias) = words.split(' ').next() {
          by_shortcode.entry(alias).or_insert(index);
        }
      }
    }
    Self {
      entries,
      by_emoji,
      by_shortcode,
      favorite_rank,
    }
  }

  fn get(&self, emoji: &str) -> Option<&CatalogEmoji> {
    // Typed or pasted emoji often lack the variation selector the table has, or have one extra.
    let index = self
      .by_emoji
      .get(emoji)
      .or_else(|| self.by_emoji.get(&without_variation_selector(emoji)))?;
    self.entries.get(*index)
  }

  fn favorite_words(&self, index: usize) -> Option<&'static str> {
    self.favorite_rank.get(&index).map(|&rank| FAVORITES[rank].1)
  }
}

/// Every emoji, in Unicode's order (groups one after the other).
pub fn all() -> &'static [CatalogEmoji] {
  &CATALOG.entries
}

/// The emoji of one group, in Unicode's order.
pub fn group(group: EmojiGroup) -> impl Iterator<Item = &'static CatalogEmoji> {
  all().iter().filter(move |entry| entry.group == group)
}

/// The catalog entry of `emoji`, also for its skin tones and without U+FE0F.
pub fn get(emoji: &str) -> Option<&'static CatalogEmoji> {
  CATALOG.get(emoji)
}

/// The emoji of a `:shortcode:` (gemoji, or a favorite's alias), without the colons.
pub fn by_shortcode(code: &str) -> Option<&'static str> {
  let catalog = &*CATALOG;
  catalog
    .by_shortcode
    .get(code)
    .map(|&index| catalog.entries[index].emoji)
}

/// The shortcode of an emoji (or one of its skin tones), for labels (`:thumbsup:`).
pub fn shortcode(emoji: &str) -> Option<&'static str> {
  let entry = get(emoji)?;
  entry.shortcode().or_else(|| {
    FAVORITES
      .iter()
      .find(|(favorite, _)| *favorite == entry.emoji)
      .and_then(|(_, words)| words.split(' ').next())
  })
}

/// The [`FAVORITES`] as their catalog entries, in their order.
pub fn favorites() -> impl Iterator<Item = &'static CatalogEmoji> {
  FAVORITES.iter().filter_map(|(emoji, _)| get(emoji))
}

/// The emoji whose shortcodes, name or extra words start with every word of `query`
/// (case-insensitive, `:colons:` ignored), best match first: an exact shortcode, the favorites,
/// shortcodes starting with the query, then Unicode's order. An empty query lists everything.
pub fn search(query: &str) -> Vec<&'static CatalogEmoji> {
  let words: Vec<String> = query
    .split_whitespace()
    .map(|word| word.trim_matches(':').to_lowercase())
    .filter(|word| !word.is_empty())
    .collect();
  let catalog = &*CATALOG;
  if words.is_empty() {
    return catalog.entries.iter().collect();
  }
  let mut found: Vec<(Rank, usize, &CatalogEmoji)> = catalog
    .entries
    .iter()
    .enumerate()
    .filter_map(|(index, entry)| {
      let rank = entry.rank(&words, catalog.favorite_words(index))?;
      let favorite = catalog.favorite_rank.get(&index).copied().unwrap_or(usize::MAX);
      Some((rank, favorite, entry))
    })
    .collect();
  // Stable: Unicode's order within the same rank.
  found.sort_by_key(|(rank, favorite, _)| (*rank, *favorite));
  found.into_iter().map(|(_, _, entry)| entry).collect()
}

/// A line of the table; `None` for a malformed one (the test checks there are none).
fn parse_line(line: &'static str) -> Option<CatalogEmoji> {
  let mut fields = line.split('\t');
  let emoji = fields.next()?;
  let group = EmojiGroup::ALL
    .get(fields.next()?.parse::<usize>().ok()?)
    .copied()?;
  let version = EmojiVersion::parse(fields.next()?)?;
  let shortcodes = fields.next()?;
  let name = fields.next()?;
  let maybe_skin_tones = match (fields.next(), fields.next()) {
    (Some(variants), Some(version)) => Some(SkinTones {
      variants: variants.split(' ').collect::<Vec<_>>().try_into().ok()?,
      version: EmojiVersion::parse(version)?,
    }),
    _ => None,
  };
  Some(CatalogEmoji {
    emoji,
    group,
    version,
    name,
    shortcodes,
    maybe_skin_tones,
  })
}

/// `emoji` without U+FE0F (emoji presentation), the form lookups fall back to.
fn without_variation_selector(emoji: &str) -> String {
  emoji.replace('\u{fe0f}', "")
}

fn starts_with_ignoring_case(candidate: &str, prefix: &str) -> bool {
  candidate
    .get(..prefix.len())
    .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
}

#[cfg(test)]
mod tests {
  use std::collections::HashSet;

  use super::*;

  #[test]
  fn catalog_file_is_current() {
    let generated = generate_catalog_tsv();
    if std::env::var_os("UPDATE_EMOJI_CATALOG").is_some() {
      let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/emoji_catalog.tsv");
      std::fs::write(path, &generated).expect("write the catalog");
    } else {
      assert!(
        CATALOG_TSV == generated,
        "emoji_catalog.tsv is out of date: UPDATE_EMOJI_CATALOG=1 cargo test -p teamcraft-client-api emoji_catalog"
      );
    }
  }

  #[test]
  fn every_line_parses_into_a_known_emoji() {
    assert_eq!(all().len(), CATALOG_TSV.lines().count());
    assert!(all().len() > 1_900);
    for entry in all() {
      let known = emojis::get(entry.emoji).unwrap_or_else(|| panic!("{} is no emoji", entry.emoji));
      assert_eq!(known.name(), entry.name);
      for tone in SkinTone::ALL {
        assert!(emojis::get(entry.with_skin_tone(tone)).is_some());
      }
    }
    let unique: HashSet<&str> = all().iter().map(|entry| entry.emoji).collect();
    assert_eq!(unique.len(), all().len());
  }

  #[test]
  fn every_group_has_emoji() {
    for group_name in EmojiGroup::ALL {
      assert!(group(group_name).count() > 20, "{group_name:?}");
    }
  }

  #[test]
  fn favorites_are_in_the_catalog_once() {
    assert_eq!(favorites().count(), FAVORITES.len());
    let unique: HashSet<&str> = favorites().map(|entry| entry.emoji).collect();
    assert_eq!(unique.len(), FAVORITES.len());
    assert!(FAVORITES.iter().all(|(_, words)| !words.trim().is_empty()));
  }

  mod search {
    use super::*;

    #[test]
    fn by_shortcode_first() {
      assert_eq!(first("thumbsup"), "👍");
      assert_eq!(first(":tada:"), "🎉");
      assert_eq!(first("+1"), "👍");
      assert_eq!(first("melting_face"), "🫠");
    }

    #[test]
    fn by_name_and_keyword() {
      assert!(emoji_of(&search("melting")).contains(&"🫠"));
      assert!(emoji_of(&search("Germany")).contains(&"🇩🇪"));
      assert!(emoji_of(&search("vacation")).contains(&"🌴"));
      assert!(emoji_of(&search("VACATION beach")).contains(&"🏖️"));
      assert!(emoji_of(&search("face tears joy")).contains(&"😂"));
    }

    #[test]
    fn favorites_rank_before_the_rest() {
      let found = emoji_of(&search("heart"));
      assert_eq!(found.first(), Some(&"❤️"));
      assert!(found.len() > 10);
    }

    #[test]
    fn nothing_and_everything() {
      assert!(search("nothing-like-this").is_empty());
      assert_eq!(search("  ").len(), all().len());
    }

    fn first(query: &str) -> &'static str {
      search(query).first().map_or("", |entry| entry.emoji)
    }

    fn emoji_of(found: &[&'static CatalogEmoji]) -> Vec<&'static str> {
      found.iter().map(|entry| entry.emoji).collect()
    }
  }

  #[test]
  fn shortcodes_for_labels_and_conversion() {
    assert_eq!(shortcode("👍"), Some("+1"));
    assert_eq!(shortcode("👍🏽"), Some("+1"));
    assert_eq!(shortcode("☕"), Some("coffee"));
    assert_eq!(by_shortcode("thumbsup"), Some("👍"));
    assert_eq!(by_shortcode("check"), Some("✅"));
    assert_eq!(by_shortcode("nope_not_a_code"), None);
  }

  #[test]
  fn skin_tones() {
    let wave = get("👋").expect("wave");
    assert!(wave.has_skin_tones());
    assert_eq!(wave.with_skin_tone(SkinTone::Default), "👋");
    assert_eq!(wave.with_skin_tone(SkinTone::Dark), "👋🏿");
    assert_eq!(get("👋🏿").map(|entry| entry.emoji), Some("👋"));
    let rocket = get("🚀").expect("rocket");
    assert_eq!(rocket.with_skin_tone(SkinTone::Dark), "🚀");
    assert_eq!(SkinTone::from_slug("medium-dark"), Some(SkinTone::MediumDark));
  }

  #[test]
  fn lookups_ignore_the_variation_selector() {
    assert_eq!(get("❤").map(|entry| entry.emoji), Some("❤️"));
  }

  /// The table from the `emojis` crate: one line per emoji in Unicode's order.
  fn generate_catalog_tsv() -> String {
    let mut out = String::new();
    for emoji in emojis::iter() {
      let shortcodes: Vec<&str> = emoji.shortcodes().collect();
      let version = emoji.emoji_version();
      let group = emojis::Group::iter()
        .position(|candidate| candidate == emoji.group())
        .expect("group");
      out.push_str(&format!(
        "{}\t{group}\t{}.{}\t{}\t{}",
        emoji.as_str(),
        version.major(),
        version.minor(),
        shortcodes.join(" "),
        emoji.name(),
      ));
      if let Some((tones, tone_version)) = generated_skin_tones(emoji) {
        out.push_str(&format!(
          "\t{}\t{}.{}",
          tones.join(" "),
          tone_version.major(),
          tone_version.minor()
        ));
      }
      out.push('\n');
    }
    out
  }

  /// The five single skin tones, when the emoji has all of them.
  fn generated_skin_tones(emoji: &emojis::Emoji) -> Option<(Vec<&str>, emojis::EmojiVersion)> {
    use emojis::SkinTone as Tone;
    let tones = [
      Tone::Light,
      Tone::MediumLight,
      Tone::Medium,
      Tone::MediumDark,
      Tone::Dark,
    ];
    let variants = tones
      .iter()
      .map(|tone| emoji.with_skin_tone(*tone))
      .collect::<Option<Vec<_>>>()?;
    let version = variants.iter().map(|variant| variant.emoji_version()).max()?;
    Some((variants.iter().map(|variant| variant.as_str()).collect(), version))
  }
}
