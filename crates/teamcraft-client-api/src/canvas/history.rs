//! A page's version history (issue #1245, e2ee.md §4.11 "Version history"), built on the device
//! from the updates it holds: nothing about it reaches the server beyond the edits themselves.
//!
//! Every update the core keeps comes with who sent it and when ([`CanvasOrigin`]). What an update
//! changed is a Yjs snapshot of its own: the clocks it inserted up to and the items it deleted
//! ([`CanvasMark`]). The page as it was at a time is the union of the changes of every update up
//! to then, rendered from the updates, which keep deleted text (merging is lossless). A union per
//! time, not a snapshot taken on arrival, so an older edit read later from the channel's history
//! still lands in the versions it belongs to.
//!
//! When the editor compacts the core's list, the marks of the compacted updates move into the
//! state's `history`, coalesced per author and stretch of editing, and the core keeps them.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{Base64Bytes, UserId};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::{Encode, Encoder, EncoderV1};
use yrs::{Doc, Options, ReadTxn, Snapshot, StateVector, Transact, Update};

use super::{Block, CanvasDoc, CanvasError, CanvasState, merge_updates};

/// Edits of one author at most this far apart count as one version.
pub const GROUP_GAP_MS: i64 = 5 * 60 * 1000;
/// A version spans at most this long, so a long session still leaves several.
pub const GROUP_SPAN_MS: i64 = 30 * 60 * 1000;
/// The most versions a page keeps; older ones fold into the next (their changes stay part of
/// every later version, only the entry goes).
pub const MAX_MARKS: usize = 300;

/// Who sent an update of the page and when (the server's time for a received one, the device's for
/// an own one), and whether it was a snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanvasOrigin {
  pub by: UserId,
  pub at_ms: i64,
  #[serde(default)]
  pub is_snapshot: bool,
}

/// What a stretch of edits of one author changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanvasMark {
  /// `None` for the changes from before the page had a history: part of every version, listed as
  /// none.
  pub maybe_by: Option<UserId>,
  pub from_ms: i64,
  pub until_ms: i64,
  /// The Yjs snapshot (lib0 v1) of what the edits inserted and deleted; opaque to the core.
  pub changes: Base64Bytes,
}

/// The page as it was after a stretch of edits of one author.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanvasVersion {
  pub by: UserId,
  pub from_ms: i64,
  pub until_ms: i64,
  /// Everything up to and including these edits.
  snapshot: Snapshot,
}

/// The origin of every update of `state`, by position; `None` for one kept before the page had a
/// history and for a compaction's merge.
pub fn origin_of(state: &CanvasState, index: usize) -> Option<CanvasOrigin> {
  // Kept before origins were: the list is shorter, and the missing ones are the first.
  let missing = state.updates.len().saturating_sub(state.origins.len());
  index
    .checked_sub(missing)
    .and_then(|index| state.origins.get(index).copied().flatten())
}

/// The page's history: the compacted part and a mark per update since, oldest first, coalesced.
pub fn marks(state: &CanvasState) -> Result<Vec<CanvasMark>, CanvasError> {
  let mut marks = if state.is_compacted {
    state.history.clone()
  } else {
    Vec::new()
  };
  for (index, update) in state.updates.iter().enumerate() {
    if index == 0 && state.is_compacted {
      continue;
    }
    let changes = encode(&changes_of(update.as_ref())?);
    marks.push(match origin_of(state, index) {
      Some(origin) => CanvasMark {
        maybe_by: Some(origin.by),
        from_ms: origin.at_ms,
        until_ms: origin.at_ms,
        changes,
      },
      None => CanvasMark {
        maybe_by: None,
        from_ms: i64::MIN,
        until_ms: i64::MIN,
        changes,
      },
    });
  }
  coalesce(marks)
}

/// The versions of the page, newest first.
pub fn versions(state: &CanvasState) -> Result<Vec<CanvasVersion>, CanvasError> {
  let mut versions = Vec::new();
  let mut so_far = Snapshot::default();
  for mark in marks(state)? {
    union(&mut so_far, decode(&mark.changes)?);
    if let Some(by) = mark.maybe_by {
      versions.push(CanvasVersion {
        by,
        from_ms: mark.from_ms,
        until_ms: mark.until_ms,
        snapshot: so_far.clone(),
      });
    }
  }
  versions.reverse();
  Ok(versions)
}

/// The blocks of the page as it was at `version`.
pub fn page_at(state: &CanvasState, version: &CanvasVersion) -> Result<Vec<Block>, CanvasError> {
  // A document that keeps deleted items, so an earlier state can be read from it.
  let doc = Doc::with_options(Options {
    skip_gc: true,
    ..Options::default()
  });
  let merged = merge_updates(&ordered(state)?)?;
  let update = Update::decode_v1(&merged).map_err(|_| CanvasError::Decode)?;
  doc
    .transact_mut()
    .apply_update(update)
    .map_err(|_| CanvasError::Apply)?;
  let mut encoder = EncoderV1::new();
  doc
    .transact()
    .encode_state_from_snapshot(&version.snapshot, &mut encoder)
    .map_err(|_| CanvasError::Apply)?;
  Ok(CanvasDoc::from_updates(&[encoder.to_vec()])?.blocks())
}

/// What the editor hands the core to compact the list: all updates as one, and the history with
/// their marks in it.
pub fn compaction(state: &CanvasState) -> Result<(Vec<u8>, Vec<CanvasMark>), CanvasError> {
  Ok((merge_updates(&ordered(state)?)?, marks(state)?))
}

/// The other members who edited the page since `since_ms`, the latest first: whoever is
/// writing on it right now, as far as this device can tell without any presence of its own.
pub fn editors_since(state: &CanvasState, since_ms: i64, me: UserId) -> Vec<UserId> {
  let compacted = state
    .history
    .iter()
    .filter(|_| state.is_compacted)
    .filter_map(|mark| Some((mark.maybe_by?, mark.until_ms)));
  let recent = (0..state.updates.len())
    .filter_map(|index| origin_of(state, index))
    .map(|origin| (origin.by, origin.at_ms));
  let mut edits: Vec<(UserId, i64)> = compacted
    .chain(recent)
    .filter(|(by, at_ms)| *at_ms >= since_ms && *by != me)
    .collect();
  edits.sort_by_key(|(_, at_ms)| std::cmp::Reverse(*at_ms));
  let mut editors: Vec<UserId> = Vec::new();
  for (by, _) in edits {
    if !editors.contains(&by) {
      editors.push(by);
    }
  }
  editors
}

/// The updates to build the history from, losing no deleted text. A snapshot carries none for
/// what it deleted (a garbage-collected range in its place), and where it and an update cover the
/// same range the merge may keep either. So a snapshot that adds nothing to the other updates is
/// left out (the usual case on a device that holds the history), and the others come last.
fn ordered(state: &CanvasState) -> Result<Vec<&[u8]>, CanvasError> {
  let is_snapshot = |index: usize| origin_of(state, index).is_some_and(|origin| origin.is_snapshot);
  let (snapshots, edits): (Vec<usize>, Vec<usize>) =
    (0..state.updates.len()).partition(|&index| is_snapshot(index));
  let mut kept: Vec<&[u8]> = edits.iter().map(|&index| state.updates[index].as_ref()).collect();
  if snapshots.is_empty() {
    return Ok(kept);
  }
  let base = Update::decode_v1(&merge_updates(&kept)?).map_err(|_| CanvasError::Decode)?;
  let (inserted, deleted) = (base.insertions(true), base.delete_set().clone());
  for index in snapshots {
    let update = state.updates[index].as_ref();
    let snapshot = Update::decode_v1(update).map_err(|_| CanvasError::Decode)?;
    let adds_nothing =
      snapshot.insertions(true).diff(&inserted).is_empty() && snapshot.delete_set().diff(&deleted).is_empty();
    if !adds_nothing {
      kept.push(update);
    }
  }
  Ok(kept)
}

/// What one update inserted (up to which clock per client) and deleted.
fn changes_of(update: &[u8]) -> Result<Snapshot, CanvasError> {
  let update = Update::decode_v1(update).map_err(|_| CanvasError::Decode)?;
  let mut state_map = StateVector::default();
  for (client, ranges) in update.insertions(true).iter() {
    if let Some(end) = ranges.iter().map(|range| range.end).max() {
      state_map.set_max(*client, end);
    }
  }
  Ok(Snapshot::new(state_map, update.delete_set().clone()))
}

/// Sorts marks by time and joins the stretches of one author; beyond [`MAX_MARKS`] the oldest
/// fold into the next.
fn coalesce(mut marks: Vec<CanvasMark>) -> Result<Vec<CanvasMark>, CanvasError> {
  marks.sort_by_key(|mark| mark.from_ms);
  let mut joined: Vec<(CanvasMark, Snapshot)> = Vec::with_capacity(marks.len());
  for mark in marks {
    let changes = decode(&mark.changes)?;
    match joined.last_mut() {
      Some((last, so_far))
        if last.maybe_by == mark.maybe_by
          && (mark.maybe_by.is_none()
            || (mark.from_ms.saturating_sub(last.until_ms) <= GROUP_GAP_MS
              && mark.until_ms.saturating_sub(last.from_ms) <= GROUP_SPAN_MS)) =>
      {
        last.until_ms = last.until_ms.max(mark.until_ms);
        union(so_far, changes);
      },
      _ => joined.push((mark, changes)),
    }
  }
  while joined.len() > MAX_MARKS {
    let (_, oldest) = joined.remove(0);
    union(&mut joined[0].1, oldest);
  }
  Ok(
    joined
      .into_iter()
      .map(|(mark, changes)| CanvasMark {
        changes: encode(&changes),
        ..mark
      })
      .collect(),
  )
}

fn union(into: &mut Snapshot, other: Snapshot) {
  into.state_map.merge(other.state_map);
  into.delete_set.merge_with(other.delete_set);
}

fn encode(snapshot: &Snapshot) -> Base64Bytes {
  Base64Bytes::new(snapshot.encode_v1())
}

fn decode(changes: &Base64Bytes) -> Result<Snapshot, CanvasError> {
  Snapshot::decode_v1(changes.as_ref()).map_err(|_| CanvasError::Decode)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::canvas::{BlockKind, snapshot};
  use teamcraft_proto::{Id, Snowflake};

  const MINUTE: i64 = 60 * 1000;

  fn user(id: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(id).unwrap())
  }

  /// The page's state as the core keeps it, with an origin per update.
  fn state(updates: &[(&Vec<u8>, UserId, i64, bool)]) -> CanvasState {
    CanvasState {
      revision: updates.len() as u64,
      updates: updates
        .iter()
        .map(|(update, ..)| Base64Bytes::new((*update).clone()))
        .collect(),
      origins: updates
        .iter()
        .map(|(_, by, at_ms, is_snapshot)| {
          Some(CanvasOrigin {
            by: *by,
            at_ms: *at_ms,
            is_snapshot: *is_snapshot,
          })
        })
        .collect(),
      ..CanvasState::default()
    }
  }

  fn texts(blocks: &[Block]) -> Vec<String> {
    blocks.iter().map(|block| block.text.clone()).collect()
  }

  fn texts_at(state: &CanvasState) -> Vec<Vec<String>> {
    versions(state)
      .unwrap()
      .iter()
      .map(|version| texts(&page_at(state, version).unwrap()))
      .collect()
  }

  /// Alice writes a draft, Bob adds a line an hour later, Alice rewrites her draft another hour
  /// later: three updates, three versions.
  fn three_versions() -> (Vec<Vec<u8>>, CanvasDoc) {
    let mut alice = CanvasDoc::new();
    let (id, first) = alice.insert_block(None, BlockKind::Paragraph, "draft");
    let mut bob = CanvasDoc::from_updates(&[&first]).unwrap();
    let (_, second) = bob.insert_block(Some(&id), BlockKind::Bullet, "from Bob");
    alice.sync(&[&second]).unwrap();
    let third = alice.set_text(&id, "final").unwrap();
    (vec![first, second, third], alice)
  }

  #[test]
  fn versions_show_the_page_as_it_was_with_who_and_when() {
    let (updates, alice) = three_versions();
    let (ann, bob) = (user(1), user(2));
    let page = state(&[
      (&updates[0], ann, 0, false),
      (&updates[1], bob, 60 * MINUTE, false),
      (&updates[2], ann, 120 * MINUTE, false),
    ]);
    let versions = versions(&page).unwrap();
    assert_eq!(
      versions
        .iter()
        .map(|version| (version.by, version.from_ms))
        .collect::<Vec<_>>(),
      [(ann, 120 * MINUTE), (bob, 60 * MINUTE), (ann, 0)],
      "newest first"
    );
    assert_eq!(
      texts_at(&page),
      [
        vec!["final", "from Bob"],
        vec!["draft", "from Bob"],
        vec!["draft"]
      ]
    );
    assert_eq!(page_at(&page, &versions[0]).unwrap(), alice.blocks());
  }

  #[test]
  fn an_older_edit_read_later_lands_in_the_versions_it_belongs_to() {
    let (updates, _) = three_versions();
    let (ann, bob) = (user(1), user(2));
    // The device read the newest edits first and Ann's first one from the history later.
    let page = state(&[
      (&updates[2], ann, 120 * MINUTE, false),
      (&updates[1], bob, 60 * MINUTE, false),
      (&updates[0], ann, 0, false),
    ]);
    assert_eq!(
      texts_at(&page),
      [
        vec!["final", "from Bob"],
        vec!["draft", "from Bob"],
        vec!["draft"]
      ]
    );
  }

  #[test]
  fn a_compacted_page_keeps_its_history() {
    let (updates, _) = three_versions();
    let (ann, bob) = (user(1), user(2));
    let page = state(&[
      (&updates[0], ann, 0, false),
      (&updates[1], bob, 60 * MINUTE, false),
      (&updates[2], ann, 120 * MINUTE, false),
    ]);
    let before = texts_at(&page);
    let (merged, history) = compaction(&page).unwrap();
    let compacted = CanvasState {
      revision: 4,
      updates: vec![merged.into()],
      origins: vec![None],
      is_compacted: true,
      history,
      ..CanvasState::default()
    };
    assert_eq!(texts_at(&compacted), before);

    // An edit after it adds a version on top.
    let mut carol = CanvasDoc::from_updates(&compacted.updates).unwrap();
    let id = carol.blocks()[1].id.clone();
    let fourth = carol.set_text(&id, "from Bob, seen by Carol").unwrap();
    let mut later = compacted.clone();
    later.updates.push(fourth.into());
    later.origins.push(Some(CanvasOrigin {
      by: user(3),
      at_ms: 180 * MINUTE,
      is_snapshot: false,
    }));
    let texts = texts_at(&later);
    assert_eq!(texts[0], ["final", "from Bob, seen by Carol"]);
    assert_eq!(texts[1..], before[..]);
    // Compacting again keeps every version.
    let (merged, history) = compaction(&later).unwrap();
    let again = CanvasState {
      updates: vec![merged.into()],
      origins: vec![None],
      is_compacted: true,
      history,
      ..CanvasState::default()
    };
    assert_eq!(texts_at(&again), texts);
  }

  #[test]
  fn a_snapshot_read_first_doesnt_hide_deleted_text_from_older_versions() {
    let (updates, _) = three_versions();
    let (ann, bob) = (user(1), user(2));
    // A snapshot carries no deleted text ("draft" was replaced), and it arrived first.
    let whole = snapshot(&updates).unwrap().unwrap();
    let page = state(&[
      (&whole, bob, 130 * MINUTE, true),
      (&updates[2], ann, 120 * MINUTE, false),
      (&updates[1], bob, 60 * MINUTE, false),
      (&updates[0], ann, 0, false),
    ]);
    let texts = texts_at(&page);
    assert_eq!(texts.last().unwrap(), &["draft"]);
    assert_eq!(texts[0], ["final", "from Bob"]);
    // The compaction keeps it as well.
    let (merged, history) = compaction(&page).unwrap();
    let compacted = CanvasState {
      updates: vec![merged.into()],
      origins: vec![None],
      is_compacted: true,
      history,
      ..CanvasState::default()
    };
    assert_eq!(texts_at(&compacted), texts);
  }

  #[test]
  fn edits_close_together_are_one_version() {
    let mut ann_page = CanvasDoc::new();
    let ann = user(1);
    let (id, first) = ann_page.insert_block(None, BlockKind::Paragraph, "a");
    let second = ann_page.set_text(&id, "ab").unwrap();
    let third = ann_page.set_text(&id, "abc").unwrap();
    let fourth = ann_page.set_text(&id, "abcd").unwrap();
    let page = state(&[
      (&first, ann, 0, false),
      (&second, ann, 2 * MINUTE, false),
      // Six minutes of quiet: a new version.
      (&third, ann, 8 * MINUTE, false),
      (&fourth, ann, 9 * MINUTE, false),
    ]);
    let versions = versions(&page).unwrap();
    assert_eq!(
      versions
        .iter()
        .map(|version| (version.from_ms, version.until_ms))
        .collect::<Vec<_>>(),
      [(8 * MINUTE, 9 * MINUTE), (0, 2 * MINUTE)]
    );
    assert_eq!(texts_at(&page), [vec!["abcd"], vec!["ab"]]);
  }

  #[test]
  fn beyond_the_limit_the_oldest_versions_fold_into_the_next() {
    let mut page = CanvasDoc::new();
    let ann = user(1);
    let (id, first) = page.insert_block(None, BlockKind::Paragraph, "0");
    let mut updates = vec![(first, 0_i64)];
    for step in 1..=(MAX_MARKS as i64 + 5) {
      let update = page.set_text(&id, &step.to_string()).unwrap();
      updates.push((update, step * 10 * MINUTE));
    }
    let listed: Vec<(&Vec<u8>, UserId, i64, bool)> = updates
      .iter()
      .map(|(update, at_ms)| (update, ann, *at_ms, false))
      .collect();
    let state = state(&listed);
    let versions = versions(&state).unwrap();
    assert_eq!(versions.len(), MAX_MARKS);
    // The oldest one left holds everything up to it.
    let oldest = versions.last().unwrap();
    assert_eq!(texts(&page_at(&state, oldest).unwrap()), ["6"]);
  }

  #[test]
  fn a_page_kept_before_it_had_a_history_is_the_base_of_every_version() {
    let (updates, _) = three_versions();
    let ann = user(1);
    // The first two updates from before origins were kept.
    let page = CanvasState {
      updates: updates.iter().cloned().map(Base64Bytes::new).collect(),
      origins: vec![Some(CanvasOrigin {
        by: ann,
        at_ms: 120 * MINUTE,
        is_snapshot: false,
      })],
      ..CanvasState::default()
    };
    assert_eq!(origin_of(&page, 0), None);
    assert_eq!(origin_of(&page, 2).map(|origin| origin.by), Some(ann));
    assert_eq!(texts_at(&page), [vec!["final", "from Bob"]]);
  }

  #[test]
  fn restoring_a_version_is_one_edit_that_others_merge() {
    // Client IDs are random, and with them the order of concurrent inserts: many rounds.
    for _ in 0..30 {
      let (updates, mut alice) = three_versions();
      let (ann, bob) = (user(1), user(2));
      let page = state(&[
        (&updates[0], ann, 0, false),
        (&updates[1], bob, 60 * MINUTE, false),
        (&updates[2], ann, 120 * MINUTE, false),
      ]);
      let versions = versions(&page).unwrap();
      // Back to the first version: Ann's draft alone.
      let old = page_at(&page, &versions[2]).unwrap();
      let restored = alice.restore(&old).unwrap();
      assert_eq!(texts(&alice.blocks()), ["draft"]);
      assert_eq!(alice.blocks()[0].id, old[0].id, "the block keeps its identity");
      assert!(alice.restore(&old).is_none(), "nothing left to change");

      // Bob typed into Ann's block meanwhile: his typing survives the restore.
      let mut bob_page = CanvasDoc::from_updates(&updates).unwrap();
      let id = bob_page.blocks()[0].id.clone();
      let typed = bob_page.set_text(&id, "final!").unwrap();
      bob_page
        .sync(&[&updates[0], &updates[1], &updates[2], &restored])
        .unwrap();
      alice.sync(&[&typed]).unwrap();
      assert_eq!(alice.blocks(), bob_page.blocks());
      assert_eq!(texts(&alice.blocks()), ["draft!"]);

      // Back to the second: Bob's line comes back afresh, in its place.
      let second = page_at(&page, &versions[1]).unwrap();
      alice.restore(&second).unwrap();
      assert_eq!(texts(&alice.blocks()), ["draft", "from Bob"]);
    }
  }

  #[test]
  fn restoring_reorders_blocks() {
    let mut page = CanvasDoc::new();
    let (a, _) = page.insert_block(None, BlockKind::Paragraph, "a");
    let (b, _) = page.insert_block(Some(&a), BlockKind::Check, "b");
    let (_, _) = page.insert_block(Some(&b), BlockKind::Heading, "c");
    let mut target = page.blocks();
    target.reverse();
    target[1].is_checked = true;
    target.push(Block {
      id: "gone-1".into(),
      kind: BlockKind::Bullet,
      text: "d".into(),
      is_checked: false,
      spans: Vec::new(),
    });
    page.restore(&target).unwrap();
    assert_eq!(page.blocks(), target);
  }

  #[test]
  fn who_edited_lately() {
    let (updates, _) = three_versions();
    let (ann, bob, me) = (user(1), user(2), user(3));
    let page = state(&[
      (&updates[0], bob, 0, false),
      (&updates[1], me, 60 * MINUTE, false),
      (&updates[2], ann, 61 * MINUTE, false),
    ]);
    assert_eq!(editors_since(&page, 59 * MINUTE, me), [ann]);
    assert_eq!(editors_since(&page, 0, me), [ann, bob]);
    assert!(editors_since(&page, 62 * MINUTE, me).is_empty());
  }
}
