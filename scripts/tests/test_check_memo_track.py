"""Tests of scripts/check-memo-track.py: `track()` on a memo nobody reads never wakes its effect (issue #1203):
python3 -m unittest discover -s scripts/tests"""
import importlib.util
import pathlib
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
SCRIPT = ROOT / "scripts" / "check-memo-track.py"
SPEC = importlib.util.spec_from_file_location("check_memo_track", SCRIPT)
check = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(check)


class MemoTrackTests(unittest.TestCase):
  def test_flags_a_memo_that_is_only_tracked(self):
    # The shape of #1196: the effect is the memo's only user.
    source = """
let mark = Memo::new(move |_| state.read(|backend| backend.link_request_mark()));
Effect::new(move |_| {
  mark.track();
  reload();
});
"""
    self.assertEqual(check.findings_in(source), [(4, "mark")])

  def test_a_memo_read_elsewhere_is_fine(self):
    for read in ("view_of(mark.get())", "mark.with(|mark| mark.len())", "show(mark)", "mark()"):
      source = f"""
let mark = Memo::new(move |_| compute());
Effect::new(move |_| mark.track());
let shown = {read};
"""
      self.assertEqual(check.findings_in(source), [], read)

  def test_a_mention_in_a_comment_is_not_a_read(self):
    source = """
let mark = Memo::new(move |_| compute());
// mark is shown further down
Effect::new(move |_| mark.track());
"""
    self.assertEqual(check.findings_in(source), [(4, "mark")])

  def test_a_mention_in_a_string_is_not_a_read(self):
    source = '''
let count = Memo::new(move |_| compute());
Effect::new(move |_| count.track());
let label = t_with("filter", &[("count", &max)]);
'''
    self.assertEqual(check.findings_in(source), [(3, "count")])

  def test_signals_and_unknown_names_are_fine(self):
    source = """
let counter = RwSignal::new(0);
Effect::new(move |_| {
  counter.track();
  settings.theme.track();
});
"""
    self.assertEqual(check.findings_in(source), [])

  def test_flags_the_memo_name_only_not_a_longer_one(self):
    source = """
let count = Memo::new(move |_| compute());
let recount = RwSignal::new(0);
Effect::new(move |_| {
  count.track();
  recount.get();
});
"""
    self.assertEqual(check.findings_in(source), [(5, "count")])

  def test_the_marker_allows_a_deliberate_track(self):
    source = """
let mark = Memo::new(move |_| compute());
Effect::new(move |_| {
  // memo-ok: another file reads it through the context
  mark.track();
});
"""
    self.assertEqual(check.findings_in(source), [])

  def test_the_fix_is_accepted(self):
    source = """
let mark = Memo::new(move |_| compute());
Effect::new(move |_| {
  let _ = mark.get();
  reload();
});
"""
    self.assertEqual(check.findings_in(source), [])


if __name__ == "__main__":
  unittest.main()
