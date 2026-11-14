//! Where a block of the shared notes lands when it moves (issue #1456): one place up or down from
//! the keyboard, or where its handle is dropped. Pure, so the rules are tested without a browser;
//! the view turns the answer into `CanvasDoc::move_block`.

/// Where a block goes: right after another block, or to the top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MoveTarget {
  Top,
  After(String),
}

impl MoveTarget {
  pub fn after(&self) -> Option<&str> {
    match self {
      Self::Top => None,
      Self::After(id) => Some(id),
    }
  }
}

/// The target of moving `id` one place up (`step` −1) or down (+1) among `ids`; `None` at either end.
pub fn step_target(ids: &[&str], id: &str, step: isize) -> Option<MoveTarget> {
  let index = ids.iter().position(|candidate| *candidate == id)?;
  let to = index.checked_add_signed(step).filter(|to| *to < ids.len())?;
  let others: Vec<&str> = ids.iter().copied().filter(|candidate| *candidate != id).collect();
  Some(match to.checked_sub(1) {
    None => MoveTarget::Top,
    Some(before) => MoveTarget::After(others[before].to_owned()),
  })
}

/// Where the dragged block `id` would be put, as the gap it lands in: before the block at that
/// index of `rows` (`rows.len()`: after the last). `rows` are the blocks in page order with the
/// vertical middle of each on screen; the pointer at `y` falls before the first middle below it.
pub fn drop_gap(rows: &[(String, f64)], y: f64) -> usize {
  rows
    .iter()
    .position(|(_, middle)| y < *middle)
    .unwrap_or(rows.len())
}

/// The move a drop into `gap` makes of `id`; `None` when the block would stay where it is.
pub fn drop_target(ids: &[&str], id: &str, gap: usize) -> Option<MoveTarget> {
  let index = ids.iter().position(|candidate| *candidate == id)?;
  // The gaps right before and right after the block leave it in place.
  if gap == index || gap == index + 1 {
    return None;
  }
  Some(match gap.checked_sub(1) {
    None => MoveTarget::Top,
    Some(before) => MoveTarget::After(ids.get(before)?.to_string()),
  })
}

/// The block's place after the move (1-based) and the number of blocks, for the announcement.
pub fn place_after(ids: &[&str], id: &str, target: &MoveTarget) -> Option<(usize, usize)> {
  let others: Vec<&str> = ids.iter().copied().filter(|candidate| *candidate != id).collect();
  let position = match target {
    MoveTarget::Top => 1,
    MoveTarget::After(after) => others.iter().position(|candidate| candidate == after)? + 2,
  };
  Some((position, ids.len()))
}

#[cfg(test)]
mod tests {
  use super::*;

  const IDS: [&str; 4] = ["a", "b", "c", "d"];

  fn after(id: &str) -> MoveTarget {
    MoveTarget::After(id.to_owned())
  }

  #[test]
  fn a_step_goes_one_place_and_stops_at_the_ends() {
    assert_eq!(step_target(&IDS, "c", -1), Some(after("a")));
    assert_eq!(step_target(&IDS, "b", -1), Some(MoveTarget::Top));
    assert_eq!(step_target(&IDS, "b", 1), Some(after("c")));
    assert_eq!(step_target(&IDS, "a", -1), None);
    assert_eq!(step_target(&IDS, "d", 1), None);
    assert_eq!(step_target(&IDS, "gone", 1), None);
  }

  #[test]
  fn the_pointer_falls_into_the_gap_before_the_next_middle() {
    let rows: Vec<(String, f64)> = IDS
      .iter()
      .enumerate()
      .map(|(index, id)| ((*id).to_owned(), index as f64 * 20.0 + 10.0))
      .collect();
    assert_eq!(drop_gap(&rows, 0.0), 0);
    assert_eq!(drop_gap(&rows, 15.0), 1);
    assert_eq!(drop_gap(&rows, 69.0), 3);
    assert_eq!(drop_gap(&rows, 500.0), 4);
    assert_eq!(drop_gap(&[], 3.0), 0);
  }

  #[test]
  fn a_drop_next_to_the_block_moves_nothing() {
    assert_eq!(drop_target(&IDS, "b", 1), None);
    assert_eq!(drop_target(&IDS, "b", 2), None);
    assert_eq!(drop_target(&IDS, "b", 0), Some(MoveTarget::Top));
    assert_eq!(drop_target(&IDS, "b", 4), Some(after("d")));
    assert_eq!(drop_target(&IDS, "d", 2), Some(after("b")));
  }

  #[test]
  fn the_announced_place_counts_from_one() {
    assert_eq!(place_after(&IDS, "d", &MoveTarget::Top), Some((1, 4)));
    assert_eq!(place_after(&IDS, "a", &after("d")), Some((4, 4)));
    assert_eq!(place_after(&IDS, "a", &after("b")), Some((2, 4)));
  }
}
