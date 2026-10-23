//! Pure helpers behind keyboard navigation and the quick switcher: where the "New messages" divider
//! goes, which conversation is the next unread one, fuzzy name matching and most-recently-used lists.

/// Direction of a keyboard step through a list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
  Previous,
  Next,
}

/// Index of the first unread message in a feed of `count` messages, or `None` when nothing is unread.
///
/// `tail_ids` are the IDs of the messages starting at `tail_start`. When the backend knows the first
/// unread message and it is among them, its position wins; otherwise the unread count is taken from
/// the end of the feed.
pub fn divider_index<Id: PartialEq>(
  count: usize,
  unread_count: usize,
  tail_start: usize,
  tail_ids: &[Id],
  maybe_first_unread: Option<Id>,
) -> Option<usize> {
  if unread_count == 0 || count == 0 {
    return None;
  }
  if let Some(first) = maybe_first_unread
    && let Some(position) = tail_ids.iter().position(|id| *id == first)
  {
    return Some(tail_start + position);
  }
  Some(count.saturating_sub(unread_count))
}

/// The next (or previous) entry with unread messages after `maybe_current`, wrapping around the list.
/// `list` holds every entry in display order with whether it is unread.
pub fn next_unread<Id: Copy + PartialEq>(
  list: &[(Id, bool)],
  maybe_current: Option<Id>,
  step: Step,
) -> Option<Id> {
  if list.is_empty() {
    return None;
  }
  let len = list.len();
  let start = maybe_current.and_then(|current| list.iter().position(|(id, _)| *id == current));
  (1..=len)
    .map(|offset| match (start, step) {
      (Some(at), Step::Next) => (at + offset) % len,
      (Some(at), Step::Previous) => (at + len - offset % len) % len,
      (None, Step::Next) => offset - 1,
      (None, Step::Previous) => len - offset,
    })
    .map(|index| list[index])
    .find(|(id, is_unread)| *is_unread && Some(*id) != maybe_current)
    .map(|(id, _)| id)
}

/// How well `needle` matches `name` for the quick switcher; lower is better, `None` is no match.
///
/// Every character of the needle must appear in order (`egr` finds "engineering"). A prefix beats a
/// substring, a substring beats a match at word starts, which beats a scattered match.
pub fn fuzzy_score(needle: &str, name: &str) -> Option<u32> {
  let needle = needle.trim().to_lowercase();
  if needle.is_empty() {
    return Some(0);
  }
  let name = name.to_lowercase();
  if name.starts_with(&needle) {
    return Some(0);
  }
  if name.contains(&needle) {
    return Some(1);
  }
  let chars: Vec<char> = name.chars().collect();
  let mut position = 0;
  let mut gaps = 0_u32;
  let mut all_at_word_starts = true;
  for wanted in needle.chars() {
    let found = chars[position..].iter().position(|c| *c == wanted)? + position;
    gaps += (found - position) as u32;
    let is_word_start = found == 0 || !chars[found - 1].is_alphanumeric();
    all_at_word_starts &= is_word_start;
    position = found + 1;
  }
  if all_at_word_starts {
    return Some(2);
  }
  Some(3 + gaps)
}

/// Moves `item` to the front of a most-recently-used list, keeping at most `max` entries.
pub fn push_recent<T: PartialEq>(list: &mut Vec<T>, item: T, max: usize) {
  list.retain(|existing| *existing != item);
  list.insert(0, item);
  list.truncate(max);
}

#[cfg(test)]
mod tests {
  use super::*;

  mod divider {
    use super::*;

    #[test]
    fn nothing_unread_has_no_divider() {
      assert_eq!(divider_index::<u32>(10, 0, 0, &[], None), None);
      assert_eq!(divider_index::<u32>(0, 3, 0, &[], None), None);
    }

    #[test]
    fn falls_back_to_the_unread_count_from_the_end() {
      assert_eq!(divider_index::<u32>(10, 3, 0, &[], None), Some(7));
      assert_eq!(divider_index::<u32>(2, 5, 0, &[], None), Some(0));
    }

    #[test]
    fn the_known_first_unread_message_wins() {
      let tail = [40_u32, 41, 42, 43];
      assert_eq!(divider_index(50, 1, 40, &tail, Some(42)), Some(42));
      // Unknown to the loaded tail: use the count.
      assert_eq!(divider_index(50, 1, 40, &tail, Some(7)), Some(49));
    }
  }

  mod unread {
    use super::*;

    const LIST: [(u32, bool); 5] = [(1, false), (2, true), (3, false), (4, true), (5, false)];

    #[test]
    fn steps_forward_and_back() {
      assert_eq!(next_unread(&LIST, Some(2), Step::Next), Some(4));
      assert_eq!(next_unread(&LIST, Some(4), Step::Previous), Some(2));
    }

    #[test]
    fn wraps_around() {
      assert_eq!(next_unread(&LIST, Some(5), Step::Next), Some(2));
      assert_eq!(next_unread(&LIST, Some(1), Step::Previous), Some(4));
    }

    #[test]
    fn starts_at_the_edges_without_a_current_entry() {
      assert_eq!(next_unread(&LIST, None, Step::Next), Some(2));
      assert_eq!(next_unread(&LIST, None, Step::Previous), Some(4));
      assert_eq!(next_unread(&LIST, Some(99), Step::Next), Some(2));
    }

    #[test]
    fn none_when_nothing_else_is_unread() {
      assert_eq!(next_unread(&[(1, false), (2, true)], Some(2), Step::Next), None);
      assert_eq!(next_unread::<u32>(&[], None, Step::Next), None);
    }
  }

  mod fuzzy {
    use super::*;

    #[test]
    fn ranks_prefix_substring_initials_and_scattered() {
      assert_eq!(fuzzy_score("eng", "engineering"), Some(0));
      assert_eq!(fuzzy_score("neer", "engineering"), Some(1));
      assert_eq!(fuzzy_score("tb", "Taylor Brooks"), Some(2));
      let scattered = fuzzy_score("egr", "engineering").unwrap_or(0);
      assert!(scattered > 2);
    }

    #[test]
    fn closer_letters_score_better() {
      let close = fuzzy_score("gnl", "general");
      let far = fuzzy_score("gnl", "gxnxxxxxxxl");
      assert!(close < far, "{close:?} {far:?}");
    }

    #[test]
    fn missing_or_out_of_order_letters_do_not_match() {
      assert_eq!(fuzzy_score("xyz", "engineering"), None);
      assert_eq!(fuzzy_score("gne", "eng"), None);
    }

    #[test]
    fn empty_needle_and_case() {
      assert_eq!(fuzzy_score("  ", "anything"), Some(0));
      assert_eq!(fuzzy_score("ENG", "engineering"), Some(0));
      assert_eq!(fuzzy_score("ü", "Grüße"), Some(1));
    }
  }

  #[test]
  fn recent_list_moves_to_front_and_is_capped() {
    let mut list = vec![1, 2, 3];
    push_recent(&mut list, 3, 3);
    assert_eq!(list, [3, 1, 2]);
    push_recent(&mut list, 4, 3);
    assert_eq!(list, [4, 3, 1]);
  }
}
