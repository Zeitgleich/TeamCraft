//! A word-level diff for the edit history: which words of a version were added or removed compared
//! with the one before.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
  Same,
  Added,
  Removed,
}

/// Words of `new` and `old` with what changed, in reading order (removed words come first at a
/// change). Longest common subsequence over whitespace-separated words.
pub fn word_diff(old: &str, new: &str) -> Vec<(Change, String)> {
  let old: Vec<&str> = old.split_whitespace().collect();
  let new: Vec<&str> = new.split_whitespace().collect();
  let mut table = vec![vec![0_usize; new.len() + 1]; old.len() + 1];
  for i in (0..old.len()).rev() {
    for j in (0..new.len()).rev() {
      table[i][j] = if old[i] == new[j] {
        table[i + 1][j + 1] + 1
      } else {
        table[i + 1][j].max(table[i][j + 1])
      };
    }
  }
  let (mut i, mut j) = (0, 0);
  let mut out = Vec::new();
  while i < old.len() || j < new.len() {
    if i < old.len() && j < new.len() && old[i] == new[j] {
      out.push((Change::Same, new[j].to_owned()));
      i += 1;
      j += 1;
    } else if i < old.len() && (j == new.len() || table[i + 1][j] >= table[i][j + 1]) {
      out.push((Change::Removed, old[i].to_owned()));
      i += 1;
    } else {
      out.push((Change::Added, new[j].to_owned()));
      j += 1;
    }
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn replaced_words_show_as_removed_then_added() {
    let diff = word_diff("meeting at 3 today", "meeting at 4 today");
    assert_eq!(
      diff,
      [
        (Change::Same, "meeting".to_owned()),
        (Change::Same, "at".to_owned()),
        (Change::Removed, "3".to_owned()),
        (Change::Added, "4".to_owned()),
        (Change::Same, "today".to_owned()),
      ]
    );
  }

  #[test]
  fn identical_and_empty_texts_work() {
    assert!(word_diff("", "").is_empty());
    assert!(
      word_diff("a b", "a b")
        .iter()
        .all(|(change, _)| *change == Change::Same)
    );
    assert_eq!(word_diff("", "new"), [(Change::Added, "new".to_owned())]);
  }
}
