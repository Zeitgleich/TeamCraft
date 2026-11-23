//! In-call reactions (issue #2324): the picker of the call's reactions button, and the reactions
//! floating up over the stage, the user's own and everybody else's.
//!
//! Reactions are emoji (user content); the button itself carries the line icon. With reduced
//! motion a reaction fades in and out in place instead of rising. Screen readers hear the others'
//! reactions through the call's announcer (`call_announcer.rs`).

use std::collections::BTreeMap;
use std::time::Duration;

use leptos::prelude::*;
use teamcraft_client_api::calls::ShownReaction;
use teamcraft_proto::CallReaction;

use crate::components::call_popover::CallPopover;
use crate::i18n::{I18n, use_i18n};
use crate::model::UserId;
use crate::state::use_app_state;

/// How long a reaction shows over the stage (the CSS animation's length).
const FLOAT_TIME: Duration = Duration::from_millis(3200);
/// The most reactions floating at once; older ones make room.
const MAX_FLOATING: usize = 12;
/// A reaction older than this when the page first sees it isn't floated (a page that opens while
/// the call runs, a reload of the desktop app's page).
const STALE_AFTER_MS: i64 = 5_000;

/// One reaction rising over the stage.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Floating {
  key: u64,
  emoji: &'static str,
  /// Who reacted; empty for the user's own.
  name: String,
  /// Where across the stage it rises, in percent.
  x_percent: u8,
}

/// The reactions to pick from: one press sends one, and the picker stays open for more.
#[component]
pub fn ReactionPicker(on_close: Callback<()>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let call = state.call;
  view! {
    <CallPopover class="effects-popover reaction-popover" labelled_by="reaction-menu-title" on_close=on_close>
      <h2 id="reaction-menu-title" class="visually-hidden">{move || i18n.t("call-reactions")}</h2>
      <div class="reaction-options">
        {CallReaction::ALL
          .into_iter()
          .filter_map(|reaction| reaction.emoji().map(|emoji| (reaction, emoji)))
          .map(|(reaction, emoji)| {
            let label = reaction_name(i18n, &reaction);
            view! {
              <button
                type="button"
                class="reaction-option"
                aria-label=label.clone()
                title=label
                on:click=move |_| call.send_reaction(reaction.clone())
              >
                <span aria-hidden="true">{emoji}</span>
              </button>
            }
          })
          .collect_view()}
      </div>
    </CallPopover>
  }
}

/// The reactions floating up over the stage: the others' from the media layer, the user's own as
/// they press them.
#[component]
pub fn FloatingReactions(
  /// The names of the others in the call, by id.
  names: Signal<Vec<(UserId, String)>>,
) -> impl IntoView {
  let state = use_app_state();
  let call = state.call;
  let floating = RwSignal::new(Vec::<Floating>::new());
  let next_key = StoredValue::new(0u64);
  let push = move |emoji: &'static str, name: String| {
    let key = next_key.try_update_value(|key| {
      *key += 1;
      *key
    });
    let Some(key) = key else {
      return;
    };
    floating.update(|all| {
      if all.len() >= MAX_FLOATING {
        all.remove(0);
      }
      all.push(Floating {
        key,
        emoji,
        name,
        x_percent: spread(key),
      });
    });
    set_timeout(
      move || {
        floating.try_update(|all| all.retain(|shown| shown.key != key));
      },
      FLOAT_TIME,
    );
  };

  // The others': those above the last id seen, unless they are old news.
  let last_seen = StoredValue::new(0u64);
  Effect::new(move |_| {
    let reactions = state.read(|backend| {
      backend
        .active_call()
        .map(|active| active.reactions)
        .unwrap_or_default()
    });
    let now_ms = crate::clock::now_ms();
    let names: BTreeMap<UserId, String> = names.get_untracked().into_iter().collect();
    let seen = last_seen.get_value();
    for shown in fresh(&reactions, seen, now_ms) {
      if let Some(emoji) = shown.reaction.emoji() {
        push(emoji, names.get(&shown.user).cloned().unwrap_or_default());
      }
    }
    if let Some(newest) = reactions.iter().map(|shown| shown.id).max() {
      last_seen.set_value(newest.max(seen));
    }
  });

  // The user's own, as pressed.
  Effect::new(move |previous: Option<u64>| {
    let (count, maybe_reaction) = call.own_reaction.get();
    if previous.is_some_and(|seen| seen != count)
      && let Some(emoji) = maybe_reaction.and_then(|reaction| reaction.emoji())
    {
      push(emoji, String::new());
    }
    count
  });

  view! {
    <div class="reaction-layer" aria-hidden="true">
      <For each=move || floating.get() key=|shown| shown.key let:shown>
        <span class="floating-reaction" style:--reaction-x=format!("{}%", shown.x_percent)>
          <span class="floating-reaction-emoji">{shown.emoji}</span>
          {(!shown.name.is_empty()).then(|| view! { <span class="floating-reaction-name">{shown.name.clone()}</span> })}
        </span>
      </For>
    </div>
  }
}

/// The others' reactions to show: newer than `seen` and not older than [`STALE_AFTER_MS`].
fn fresh(reactions: &[ShownReaction], seen: u64, now_ms: i64) -> impl Iterator<Item = &ShownReaction> {
  reactions
    .iter()
    .filter(move |shown| shown.id > seen && now_ms.saturating_sub(shown.at) <= STALE_AFTER_MS)
}

/// Spreads reactions across the lower stage, so a burst doesn't stack in one column.
fn spread(key: u64) -> u8 {
  // A golden-ratio walk over 10..90 %.
  let step = (key.wrapping_mul(61_803) % 80) as u8;
  10 + step
}

/// The reaction's spoken name ("Applause").
pub fn reaction_name(i18n: I18n, reaction: &CallReaction) -> String {
  let key = match reaction {
    CallReaction::ThumbsUp => "call-reaction-thumbs-up",
    CallReaction::Applause => "call-reaction-applause",
    CallReaction::Laugh => "call-reaction-laugh",
    CallReaction::Heart => "call-reaction-heart",
    CallReaction::Celebrate => "call-reaction-celebrate",
    CallReaction::Surprise => "call-reaction-surprise",
    CallReaction::Unknown(_) => "call-reaction-other",
  };
  i18n.t(key)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn only_new_and_recent_reactions_float() {
    let reactions = [shown(1, 1_000), shown(2, 9_000), shown(3, 9_500)];
    let ids: Vec<u64> = fresh(&reactions, 1, 10_000).map(|shown| shown.id).collect();
    assert_eq!(ids, [2, 3]);
    assert_eq!(fresh(&reactions, 0, 10_000).count(), 2, "the first is old news");
    assert_eq!(fresh(&reactions, 3, 10_000).count(), 0);
  }

  #[test]
  fn reactions_spread_within_the_stage() {
    assert!((1..200).map(spread).all(|x| (10..90).contains(&x)));
    assert_ne!(spread(1), spread(2));
  }

  fn shown(id: u64, at: i64) -> ShownReaction {
    ShownReaction {
      id,
      user: UserId::from_snowflake(Snowflake::new(5).expect("a valid test id")),
      reaction: CallReaction::Heart,
      at,
    }
  }
}
