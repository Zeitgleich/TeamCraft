//! The emoji the pickers offer, with English search words (the `:shortcode:` names people know from
//! other chat apps). Grouped roughly by how often they are used in a team chat.

/// `(emoji, search words)`; the first word is the shortcode.
pub const CATALOG: [(&str, &str); 120] = [
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

/// The emoji whose search words contain every word of `query` (case-insensitive), in catalog order.
/// An empty query lists everything.
pub fn search(query: &str) -> Vec<&'static str> {
  let words: Vec<String> = query
    .split_whitespace()
    .map(|word| word.trim_matches(':').to_lowercase())
    .filter(|word| !word.is_empty())
    .collect();
  CATALOG
    .iter()
    .filter(|(emoji, names)| {
      words
        .iter()
        .all(|word| names.split(' ').any(|name| name.contains(word.as_str())) || emoji == word)
    })
    .map(|(emoji, _)| *emoji)
    .collect()
}

/// The shortcode of an emoji in the catalog, for labels (`:thumbsup:`).
pub fn shortcode(emoji: &str) -> Option<&'static str> {
  CATALOG
    .iter()
    .find(|(candidate, _)| *candidate == emoji)
    .and_then(|(_, names)| names.split(' ').next())
}

#[cfg(test)]
mod tests {
  use std::collections::HashSet;

  use super::*;

  #[test]
  fn catalog_has_no_duplicates_and_every_entry_has_a_name() {
    let unique: HashSet<&str> = CATALOG.iter().map(|(emoji, _)| *emoji).collect();
    assert_eq!(unique.len(), CATALOG.len());
    assert!(CATALOG.iter().all(|(_, names)| !names.trim().is_empty()));
  }

  #[test]
  fn search_matches_words_and_shortcode_syntax() {
    assert_eq!(search("thumbsup").first(), Some(&"👍"));
    assert_eq!(search(":tada:"), ["🎉"]);
    assert!(search("vacation").contains(&"🌴"));
    assert!(search("VACATION beach").contains(&"🏖️"));
    assert!(search("nothing-like-this").is_empty());
  }

  #[test]
  fn empty_query_lists_everything() {
    assert_eq!(search("  ").len(), CATALOG.len());
  }

  #[test]
  fn shortcodes_for_labels() {
    assert_eq!(shortcode("👍"), Some("thumbsup"));
    assert_eq!(shortcode("🦄"), None);
  }
}
