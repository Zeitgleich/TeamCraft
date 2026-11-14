//! Which word prefixes a message's text may hold, as a small filter per message (issue #1833): the
//! browser's search, which has no tantivy, reads these instead of every message.
//!
//! A word is a run of alphanumeric characters, compared lowercased character by character as
//! [`teamcraft_client_api::search::find_ci`] does. A message's [`WordFilter`] holds the first one to
//! [`MAX_PREFIX_CHARS`] characters of each of its words in a Bloom filter: a search term finds every
//! message in which it starts a word, and a few that don't (the filter's false positives), which
//! the caller then checks against the text. A term longer than [`MAX_PREFIX_CHARS`] is looked up by
//! its first characters. Nothing is ever missed that starts a word; a term that matches only in the
//! middle of a word ("arch" in "search") is not found, as with the native apps' word index.
//!
//! The filter is derived data kept in the sealed store's catalog, so it is encrypted like the text
//! it is made from (e2ee.md §9.1).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use teamcraft_proto::Base64Bytes;

use super::{MessageContent, StoredMessage};

/// Characters of a word whose prefixes go into the filter: a search term of up to this many
/// characters is looked up exactly, a longer one by its first ones.
pub(crate) const MAX_PREFIX_CHARS: usize = 6;
/// Filter bits per distinct prefix: with [`HASHES`] probes about 2.4 % false positives per term.
const BITS_PER_PREFIX: usize = 8;
/// Probes per prefix.
const HASHES: u64 = 4;
/// Filter size bounds. A long message's filter fills up and lets more terms through, which the
/// text check then turns away; it never misses one.
const MIN_FILTER_BYTES: usize = 8;
const MAX_FILTER_BYTES: usize = 256;

/// The word prefixes of one message's searchable text ([`StoredMessage::searchable_text`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WordFilter(Base64Bytes);

/// The prefixes a search looks for, hashed once for every filter it is tested against.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WordProbe {
  hashes: Vec<u64>,
}

impl WordFilter {
  /// The filter of `message`, or `None` for a message search never shows (no text, no file).
  pub(crate) fn of(message: &StoredMessage) -> Option<Self> {
    let text = message.searchable_text()?;
    let prefixes: BTreeSet<u64> = words(text)
      .flat_map(|word| (1..=word.len().min(MAX_PREFIX_CHARS)).map(move |len| prefix_hash(&word[..len])))
      .collect();
    if prefixes.is_empty() {
      return Some(Self(Base64Bytes::new(Vec::new())));
    }
    let size_bytes = (prefixes.len() * BITS_PER_PREFIX)
      .div_ceil(8)
      .clamp(MIN_FILTER_BYTES, MAX_FILTER_BYTES);
    let mut bits = vec![0u8; size_bytes];
    for hash in prefixes {
      for bit in probes(hash, size_bytes) {
        bits[bit / 8] |= 1 << (bit % 8);
      }
    }
    Some(Self(Base64Bytes::new(bits)))
  }

  /// Whether the message may hold every prefix of `probe`: always when it does, sometimes when not.
  pub(crate) fn may_contain(&self, probe: &WordProbe) -> bool {
    let bits = self.0.as_slice();
    if bits.is_empty() {
      return probe.hashes.is_empty();
    }
    probe
      .hashes
      .iter()
      .all(|hash| probes(*hash, bits.len()).all(|bit| bits[bit / 8] & (1 << (bit % 8)) != 0))
  }

  /// About how many bytes the filter takes in memory.
  pub(crate) fn heap_bytes(&self) -> usize {
    self.0.len()
  }
}

impl WordProbe {
  /// What a message must hold to match every one of `terms` as a substring that starts a word:
  /// each word of each term, the first of a term as a prefix of the message's word, the others as
  /// whole words (a term "foo-ba" needs a word starting with "foo" and one starting with "ba").
  /// Terms without letters or digits ask for nothing, so every message passes.
  pub fn of_terms<'a>(terms: impl IntoIterator<Item = &'a str>) -> Self {
    let hashes: BTreeSet<u64> = terms
      .into_iter()
      .flat_map(words)
      .map(|word| prefix_hash(&word[..word.len().min(MAX_PREFIX_CHARS)]))
      .collect();
    Self {
      hashes: hashes.into_iter().collect(),
    }
  }
}

impl StoredMessage {
  /// The text search looks at: a text's body or a file's name. `None` for what search never shows.
  pub fn searchable_text(&self) -> Option<&str> {
    match &self.content {
      MessageContent::Text { body, .. } => Some(body),
      MessageContent::File { attachment } => Some(&attachment.file_name),
      _ => None,
    }
  }
}

/// The words of `text`, each lowercased character by character.
fn words(text: &str) -> impl Iterator<Item = Vec<char>> + '_ {
  text
    .split(|character: char| !character.is_alphanumeric())
    .filter(|word| !word.is_empty())
    .map(|word| word.chars().flat_map(char::to_lowercase).collect())
}

/// 64-bit FNV-1a of the prefix's UTF-8: stable across versions and platforms, as the filters are
/// stored. Not a cryptographic hash, and it needn't be: the filters are sealed.
fn prefix_hash(prefix: &[char]) -> u64 {
  const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
  const PRIME: u64 = 0x0000_0100_0000_01b3;
  let mut hash = OFFSET;
  let mut buffer = [0u8; 4];
  for character in prefix {
    for byte in character.encode_utf8(&mut buffer).bytes() {
      hash ^= u64::from(byte);
      hash = hash.wrapping_mul(PRIME);
    }
  }
  hash
}

/// The bits a prefix sets in a filter of `size_bytes`: double hashing of its two halves.
fn probes(hash: u64, size_bytes: usize) -> impl Iterator<Item = usize> {
  let bits = u64::try_from(size_bytes * 8).unwrap_or(u64::MAX);
  let first = hash & 0xffff_ffff;
  let step = (hash >> 32) | 1;
  (0..HASHES).map(move |round| {
    // Invariant: the remainder is below `bits`, which came from a usize.
    usize::try_from(first.wrapping_add(round.wrapping_mul(step)) % bits).unwrap_or(0)
  })
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::search::find_ci;

  use super::*;
  use crate::store::tests::{id, message};

  #[test]
  fn every_word_prefix_is_found() {
    let filter = filter_of("Quarterly Report-Draft für Müller, v2.0 ÄRGER");
    for term in [
      "q",
      "quart",
      "quarterly",
      "quarterlyx",
      "report",
      "rep",
      "draft",
      "report-dr",
      "FÜR",
      "mül",
      "müller",
      "v2",
      "v2.0",
      "ärg",
      "ärger",
    ] {
      assert!(filter.may_contain(&WordProbe::of_terms([term])), "{term}");
    }
  }

  #[test]
  fn several_terms_must_all_be_there() {
    let filter = filter_of("deploy the staging server");
    assert!(filter.may_contain(&WordProbe::of_terms(["stag", "serv"])));
    assert!(!filter.may_contain(&WordProbe::of_terms(["stag", "zebra"])));
  }

  #[test]
  fn terms_without_letters_ask_for_nothing() {
    assert!(filter_of("hello").may_contain(&WordProbe::of_terms(["--"])));
    assert!(filter_of("!!").may_contain(&WordProbe::of_terms(["!"])));
    assert!(!filter_of("!!").may_contain(&WordProbe::of_terms(["a"])));
  }

  /// Against the substring search it replaces: whatever starts a word is never filtered out, and
  /// few of the others get through.
  #[test]
  fn agrees_with_substring_search_on_word_prefixes() {
    let texts = [
      "Meeting notes from Monday",
      "please review the PR before lunch",
      "Rechnung 2026-11 an Kunde A",
      "größere Änderung am Server",
      "report_final_v3.pdf",
      "the build broke again",
    ];
    let terms = [
      "mee", "notes", "mon", "pr", "lunch", "rech", "2026", "11", "kunde", "grö", "änd", "serv", "final",
      "v3", "pdf", "brok", "again", "xyz", "zz", "qq", "build",
    ];
    let mut false_positives = 0;
    for text in texts {
      let filter = filter_of(text);
      for term in terms {
        let starts_a_word = words(text).any(|word| {
          let needle: Vec<char> = term.chars().flat_map(char::to_lowercase).collect();
          word.starts_with(&needle)
        });
        let passes = filter.may_contain(&WordProbe::of_terms([term]));
        if starts_a_word {
          assert!(passes, "{term} in {text}");
          assert!(find_ci(text, term).is_some());
        } else if passes {
          false_positives += 1;
        }
      }
    }
    assert!(false_positives <= 10, "{false_positives} false positives");
  }

  /// Few messages without a term get through: about [`BITS_PER_PREFIX`]'s rate.
  #[test]
  fn false_positives_are_rare() {
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    let mut next_word = move || {
      seed ^= seed << 13;
      seed ^= seed >> 7;
      seed ^= seed << 17;
      let len = 3 + seed % 8;
      (0..len)
        .map(|shift| char::from(b'a' + u8::try_from((seed >> (shift * 5)) % 26).unwrap()))
        .collect::<String>()
    };
    let (mut passed, mut checks) = (0, 0);
    for _ in 0..500 {
      let text: Vec<String> = (0..12).map(|_| next_word()).collect();
      let filter = filter_of(&text.join(" "));
      for _ in 0..10 {
        let term = next_word();
        if text
          .iter()
          .any(|word| word.starts_with(&term[..term.len().min(MAX_PREFIX_CHARS)]))
        {
          continue;
        }
        checks += 1;
        if filter.may_contain(&WordProbe::of_terms([term.as_str()])) {
          passed += 1;
        }
      }
    }
    assert!(passed * 25 <= checks, "{passed} of {checks}");
  }

  #[test]
  fn messages_search_never_shows_have_no_filter() {
    let mut stored = message(id(1), 0, None, None, 0);
    stored.content = MessageContent::Deleted;
    assert!(WordFilter::of(&stored).is_none());
  }

  #[test]
  fn a_long_message_still_finds_its_words() {
    let text: String = (0..2_000).map(|n| format!("word{n} ")).collect();
    let filter = filter_of(&text);
    assert!(filter.heap_bytes() <= MAX_FILTER_BYTES);
    assert!(filter.may_contain(&WordProbe::of_terms(["word1999"])));
  }

  fn filter_of(text: &str) -> WordFilter {
    let mut stored = message(id(1), 0, None, None, 0);
    stored.content = MessageContent::Text {
      body: text.into(),
      mentions: Vec::new(),
      meta: Default::default(),
    };
    WordFilter::of(&stored).unwrap()
  }
}
