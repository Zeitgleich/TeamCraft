//! The call's announcer (issue #2281, docs/accessibility.md W-9, W-10): one polite live region that
//! exists before its first text, fed from roster diffs, lobby arrivals, mute changes, recording
//! and the call's phases, so a screen reader hears what the screen only shows.
//!
//! Events are collected for a short window and said together: a meeting that fills up says
//! "5 people joined the call" once, not five times. Whatever repeats a visible control's own
//! announcement is left out (a mute button that changed its label says so itself).

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use leptos::prelude::*;

use crate::i18n::{I18n, use_i18n};
use crate::model::UserId;
use crate::reactive::SetIfAlive;
use crate::state::use_app_state;

/// Events of one window are said together after this long.
const WINDOW: Duration = Duration::from_millis(1500);
/// Up to this many names are spoken; more are counted.
const MAX_NAMED: usize = 3;

/// One thing worth saying, before wording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallNote {
  Joined(String),
  Left(String),
  /// Somebody waits in the lobby.
  Waiting(String),
  /// The own microphone, `true` when it is on.
  Microphone(bool),
  /// Somebody else started recording.
  RecordingBy(String),
  /// This device started recording.
  RecordingOwn,
  RecordingStopped,
  /// A phase of the call, as the i18n key of its visible text.
  Phase(&'static str),
}

/// A sentence to say: its message key and arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sentence {
  pub key: &'static str,
  pub count: usize,
  pub names: String,
}

/// Merges the notes of one window into sentences: a name list per kind (a count above
/// [`MAX_NAMED`]), a join and a leave of the same person cancelling out, the last microphone state
/// only, each phase once, in the order the kinds first appeared.
pub fn sentences(notes: &[CallNote]) -> Vec<Sentence> {
  let mut joined: Vec<String> = Vec::new();
  let mut left: Vec<String> = Vec::new();
  let mut waiting: Vec<String> = Vec::new();
  let mut recording_by: Vec<String> = Vec::new();
  let mut order: Vec<&'static str> = Vec::new();
  let mut maybe_microphone = None;
  let mut is_recording_own = false;
  let mut is_recording_stopped = false;
  let mut phases: Vec<&'static str> = Vec::new();
  let mut see = |kind: &'static str| {
    if !order.contains(&kind) {
      order.push(kind);
    }
  };
  for note in notes {
    match note {
      CallNote::Joined(name) => {
        if let Some(at) = left.iter().position(|gone| gone == name) {
          left.remove(at);
        } else {
          joined.push(name.clone());
        }
        see("joined");
        see("left");
      },
      CallNote::Left(name) => {
        if let Some(at) = joined.iter().position(|came| came == name) {
          joined.remove(at);
        } else {
          left.push(name.clone());
        }
        see("joined");
        see("left");
      },
      CallNote::Waiting(name) => {
        waiting.push(name.clone());
        see("waiting");
      },
      CallNote::Microphone(is_on) => {
        maybe_microphone = Some(*is_on);
        see("microphone");
      },
      CallNote::RecordingBy(name) => {
        recording_by.push(name.clone());
        see("recording-by");
      },
      CallNote::RecordingOwn => {
        is_recording_own = true;
        is_recording_stopped = false;
        see("recording-own");
      },
      CallNote::RecordingStopped => {
        is_recording_stopped = true;
        is_recording_own = false;
        see("recording-stopped");
      },
      CallNote::Phase(key) => {
        if !phases.contains(key) {
          phases.push(key);
        }
        see("phase");
      },
    }
  }
  let listed = |key: &'static str, mut names: Vec<String>, many: &'static str| {
    names.dedup();
    (!names.is_empty()).then(|| {
      if names.len() <= MAX_NAMED {
        Sentence {
          key,
          count: names.len(),
          names: names.join(", "),
        }
      } else {
        Sentence {
          key: many,
          count: names.len(),
          names: String::new(),
        }
      }
    })
  };
  let mut out = Vec::new();
  for kind in order {
    match kind {
      "joined" => out.extend(listed(
        "call-announce-joined",
        std::mem::take(&mut joined),
        "call-announce-joined-many",
      )),
      "left" => out.extend(listed(
        "call-announce-left",
        std::mem::take(&mut left),
        "call-announce-left-many",
      )),
      "waiting" => out.extend(listed(
        "call-announce-waiting",
        std::mem::take(&mut waiting),
        "call-announce-waiting-many",
      )),
      "recording-by" => out.extend(listed(
        "call-announce-recording-by",
        std::mem::take(&mut recording_by),
        "call-announce-recording-by-many",
      )),
      "microphone" => out.extend(maybe_microphone.map(|is_on| Sentence {
        key: if is_on {
          "call-announce-mic-on"
        } else {
          "call-announce-mic-off"
        },
        count: 1,
        names: String::new(),
      })),
      "recording-own" if is_recording_own => out.push(Sentence {
        key: "call-announce-recording-own",
        count: 1,
        names: String::new(),
      }),
      "recording-stopped" if is_recording_stopped => out.push(Sentence {
        key: "call-announce-recording-stopped",
        count: 1,
        names: String::new(),
      }),
      "phase" => out.extend(phases.iter().map(|key| Sentence {
        key,
        count: 1,
        names: String::new(),
      })),
      _ => {},
    }
  }
  out
}

/// Words the sentences.
pub fn say(i18n: I18n, sentences: &[Sentence]) -> String {
  sentences
    .iter()
    .map(|sentence| {
      if sentence.names.is_empty() {
        i18n.t_count(sentence.key, sentence.count)
      } else {
        i18n.t_count_with(sentence.key, sentence.count, &[("names", &sentence.names)])
      }
    })
    .collect::<Vec<_>>()
    .join(". ")
}

/// The persistent, initially empty live region of the call screen.
#[component]
pub fn CallAnnouncer(
  /// Who is in the call besides the user, by id and name.
  remote: Signal<Vec<(UserId, String)>>,
  /// Who waits in the lobby of the meeting this user hosts.
  lobby: Signal<Vec<(UserId, String)>>,
  /// The phase texts to say when they appear: (is shown, i18n key).
  phases: Signal<Vec<(bool, &'static str)>>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let call = state.call;
  let text = RwSignal::new(String::new());
  let pending = StoredValue::new(Vec::<CallNote>::new());
  let is_flush_planned = StoredValue::new(false);
  // Said again after an identical text: the alternate suffix makes the region change.
  let flips = StoredValue::new(false);

  let flush = move || {
    is_flush_planned.set_value(false);
    let notes = pending.try_update_value(std::mem::take).unwrap_or_default();
    let said = say(i18n, &sentences(&notes));
    if said.is_empty() {
      return;
    }
    let is_flipped = flips.try_update_value(|flipped| {
      *flipped = !*flipped;
      *flipped
    });
    text.set_if_alive(if is_flipped.unwrap_or(false) {
      format!("{said}\u{200b}")
    } else {
      said
    });
  };
  let note = move |note: CallNote| {
    pending.update_value(|all| all.push(note));
    if !is_flush_planned.get_value() {
      is_flush_planned.set_value(true);
      set_timeout(flush, WINDOW);
    }
  };

  // Rosters: the first look is the baseline, not news.
  let watch = move |source: Signal<Vec<(UserId, String)>>,
                    added: fn(String) -> CallNote,
                    maybe_gone: Option<fn(String) -> CallNote>| {
    let known = StoredValue::new(None::<BTreeMap<UserId, String>>);
    Effect::new(move |_| {
      let now: BTreeMap<UserId, String> = source.get().into_iter().collect();
      let Some(before) = known.get_value() else {
        known.set_value(Some(now));
        return;
      };
      let (came, went) = roster_diff(&before, &now);
      known.set_value(Some(now));
      for name in came {
        note(added(someone_if_empty(i18n, name)));
      }
      if let Some(gone) = maybe_gone {
        for name in went {
          note(gone(someone_if_empty(i18n, name)));
        }
      }
    });
  };
  watch(remote, CallNote::Joined, Some(CallNote::Left));
  watch(lobby, CallNote::Waiting, None);

  // The own microphone, when it changed away from the control bar: its button names its state
  // itself. The global shortcut (mute_shortcut.rs) and the tray come here.
  let last_mic = StoredValue::new(call.is_mic_on.get_untracked());
  Effect::new(move |_| {
    let is_on = call.is_mic_on.get();
    if last_mic.get_value() == is_on {
      return;
    }
    last_mic.set_value(is_on);
    if !is_focus_in_controls() {
      note(CallNote::Microphone(is_on));
    }
  });

  // Recording: the first look is the baseline too (the banner says it on joining).
  let last_recording = StoredValue::new(None::<(bool, BTreeMap<UserId, String>)>);
  Effect::new(move |_| {
    let (is_own, others) = crate::components::call_recording::recording_now(state);
    let others: BTreeMap<UserId, String> = others.into_iter().collect();
    let Some((was_own, before)) = last_recording.get_value() else {
      last_recording.set_value(Some((is_own, others)));
      return;
    };
    let (came, _) = roster_diff(&before, &others);
    let is_stopped = (was_own && !is_own) || (!before.is_empty() && others.is_empty() && !is_own);
    last_recording.set_value(Some((is_own, others)));
    if is_own && !was_own {
      note(CallNote::RecordingOwn);
    }
    for name in came {
      note(CallNote::RecordingBy(someone_if_empty(i18n, name)));
    }
    if is_stopped {
      note(CallNote::RecordingStopped);
    }
  });

  // Phases: each visible status text the moment it appears.
  let shown = StoredValue::new(BTreeSet::<&'static str>::new());
  Effect::new(move |_| {
    let now: BTreeSet<&'static str> = phases
      .get()
      .into_iter()
      .filter(|(is_shown, _)| *is_shown)
      .map(|(_, key)| key)
      .collect();
    let before = shown.get_value();
    shown.set_value(now.clone());
    for key in now.difference(&before) {
      note(CallNote::Phase(key));
    }
  });

  view! { <div class="visually-hidden" role="status" aria-live="polite" aria-atomic="true">{move || text.get()}</div> }
}

/// Who is new in `now` and who is gone from `before`, as names, in id order.
fn roster_diff(
  before: &BTreeMap<UserId, String>,
  now: &BTreeMap<UserId, String>,
) -> (Vec<String>, Vec<String>) {
  let came = now
    .iter()
    .filter(|(id, _)| !before.contains_key(id))
    .map(|(_, name)| name.clone())
    .collect();
  let went = before
    .iter()
    .filter(|(id, _)| !now.contains_key(id))
    .map(|(_, name)| name.clone())
    .collect();
  (came, went)
}

fn someone_if_empty(i18n: I18n, name: String) -> String {
  if name.is_empty() {
    i18n.t("call-recording-someone")
  } else {
    name
  }
}

/// The focus is on a control of the call bar, which says its own state change.
fn is_focus_in_controls() -> bool {
  web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.active_element())
    .is_some_and(|active| active.closest(".call-controls").ok().flatten().is_some())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn user(raw: i64) -> UserId {
    use teamcraft_proto::Id;
    UserId::from_snowflake(teamcraft_proto::Snowflake::new(raw).expect("a valid test id"))
  }

  fn joined(name: &str) -> CallNote {
    CallNote::Joined(name.to_owned())
  }

  fn left(name: &str) -> CallNote {
    CallNote::Left(name.to_owned())
  }

  mod joins_and_leaves {
    use super::*;

    #[test]
    fn a_few_names_are_listed() {
      let said = sentences(&[joined("Ada"), joined("Bo")]);
      assert_eq!(said.len(), 1);
      assert_eq!(said[0].key, "call-announce-joined");
      assert_eq!(said[0].names, "Ada, Bo");
    }

    #[test]
    fn a_crowd_is_counted_not_listed() {
      let notes: Vec<CallNote> = (0..9).map(|n| joined(&format!("P{n}"))).collect();
      let said = sentences(&notes);
      assert_eq!(said.len(), 1);
      assert_eq!(said[0].key, "call-announce-joined-many");
      assert_eq!(said[0].count, 9);
    }

    #[test]
    fn a_flicker_of_one_person_says_nothing() {
      assert!(sentences(&[joined("Ada"), left("Ada")]).is_empty());
      assert!(sentences(&[left("Ada"), joined("Ada")]).is_empty());
    }

    #[test]
    fn joins_and_leaves_are_said_apart() {
      let said = sentences(&[left("Bo"), joined("Ada")]);
      assert_eq!(
        said.iter().map(|s| s.key).collect::<Vec<_>>(),
        ["call-announce-joined", "call-announce-left"]
      );
    }
  }

  mod the_rest {
    use super::*;

    #[test]
    fn only_the_last_microphone_state_counts() {
      let said = sentences(&[CallNote::Microphone(false), CallNote::Microphone(true)]);
      assert_eq!(said.len(), 1);
      assert_eq!(said[0].key, "call-announce-mic-on");
    }

    #[test]
    fn a_phase_is_said_once() {
      let said = sentences(&[
        CallNote::Phase("call-reconnecting"),
        CallNote::Phase("call-reconnecting"),
      ]);
      assert_eq!(said.len(), 1);
    }

    #[test]
    fn recording_started_then_stopped_says_only_the_stop() {
      let said = sentences(&[CallNote::RecordingOwn, CallNote::RecordingStopped]);
      assert_eq!(
        said.iter().map(|s| s.key).collect::<Vec<_>>(),
        ["call-announce-recording-stopped"]
      );
    }

    #[test]
    fn the_roster_diff_finds_who_came_and_went() {
      let before: BTreeMap<UserId, String> = [(user(1), "Ada".to_owned()), (user(2), "Bo".to_owned())].into();
      let now: BTreeMap<UserId, String> = [(user(2), "Bo".to_owned()), (user(3), "Cy".to_owned())].into();
      assert_eq!(
        roster_diff(&before, &now),
        (vec!["Cy".to_owned()], vec!["Ada".to_owned()])
      );
    }
  }
}
