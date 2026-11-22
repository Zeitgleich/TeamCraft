//! A reaction under a message with the people behind it (issue #2176): a tooltip on hover and
//! keyboard focus, a popover with everyone on a long press or the tooltip's "Show everyone". The
//! names come with the message's reactions and resolve from the local user directory, so nothing
//! is requested on hover.

use leptos::prelude::*;
use teamcraft_client_api::model::{ConversationId, MessageId, Reaction, User};

use crate::components::long_press::LongPress;
use crate::custom_emoji::EmojiGlyph;
use fluent_bundle::FluentArgs;

use crate::i18n::{Locale, translate, use_i18n};
use crate::state::use_app_state;

/// The tooltip names this many people before "and N others".
const TOOLTIP_NAMES: usize = 3;

/// Who reacted, as far as this device knows them: the viewer ("you") first, then the others by the
/// order the core gave. People it can't name (departed, unknown) only count towards the total.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReactorNames {
  pub has_me: bool,
  /// Display names of the others, in order.
  pub others: Vec<String>,
  /// Everyone who reacted, named or not.
  pub total: usize,
}

impl ReactorNames {
  pub fn new(
    reaction: &Reaction,
    mut name_of: impl FnMut(teamcraft_client_api::model::UserId) -> Option<String>,
  ) -> Self {
    let mut others = Vec::new();
    let mut has_me = false;
    for (index, user) in reaction.reactors.iter().enumerate() {
      // The core puts the viewer first; `reacted_by_me` says whether they are among them.
      if reaction.reacted_by_me && index == 0 {
        has_me = true;
      } else if let Some(name) = name_of(*user) {
        others.push(name);
      }
    }
    Self {
      has_me,
      others,
      total: reaction.count as usize,
    }
  }

  /// How many people have a name to show.
  fn named(&self) -> usize {
    usize::from(self.has_me) + self.others.len()
  }

  /// The people in one phrase, at most `limit` names and then "and N others": `None` when nobody
  /// has a name (the label then only counts).
  pub fn phrase(&self, locale: Locale, limit: usize) -> Option<String> {
    let you = self.has_me.then(|| translate(locale, "reaction-you", None));
    let names: Vec<&str> = you
      .iter()
      .map(String::as_str)
      .chain(self.others.iter().map(String::as_str))
      .collect();
    let shown = names.len().min(limit);
    if shown == 0 {
      return None;
    }
    let others = self.total.saturating_sub(shown);
    let with = |key: &str, pairs: &[(&str, &str)]| {
      let mut args = FluentArgs::new();
      for (name, value) in pairs {
        args.set(name.to_string(), value.to_string());
      }
      translate(locale, key, Some(&args))
    };
    Some(match (shown, others) {
      (1, 0) => names[0].to_owned(),
      (2, 0) => with("reaction-names-two", &[("first", names[0]), ("second", names[1])]),
      (_, 0) => with(
        "reaction-names-three",
        &[("first", names[0]), ("second", names[1]), ("third", names[2])],
      ),
      _ => {
        let mut args = FluentArgs::new();
        args.set("names", names[..shown].join(", "));
        args.set("count", others);
        translate(locale, "reaction-names-more", Some(&args))
      },
    })
  }

  /// "Alex, Sam and you reacted with +1", or the count alone when no name is known.
  pub fn label(&self, locale: Locale, emoji: &str) -> String {
    let mut args = FluentArgs::new();
    args.set("emoji", emoji.to_owned());
    match self.phrase(locale, TOOLTIP_NAMES) {
      Some(names) => {
        args.set("names", names);
        translate(locale, "reaction-label-names", Some(&args))
      },
      None => {
        args.set("count", self.total);
        translate(locale, "reaction-label", Some(&args))
      },
    }
  }

  /// More people than the tooltip names: the popover has the rest.
  pub fn has_more(&self) -> bool {
    self.total > TOOLTIP_NAMES || self.named() < self.total
  }
}

#[component]
pub fn ReactionChip(
  reaction: Reaction,
  conversation: ConversationId,
  message: MessageId,
  can_react: bool,
  pop_delay: String,
  is_popping: bool,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let names = state.read(|backend| {
    ReactorNames::new(&reaction, |id| {
      backend.user(id).map(|user: User| user.display_name)
    })
  });
  let is_open = RwSignal::new(false);
  let emoji = reaction.emoji.clone();
  let plain_emoji = reaction.emoji.clone();
  let count = reaction.count;
  let label_names = names.clone();
  let label = move || label_names.label(i18n.locale(), &plain_emoji);
  let tip_names = names.clone();
  let tip = move || tip_names.phrase(i18n.locale(), TOOLTIP_NAMES);
  let has_more = names.has_more();
  let popover_names = names.clone();
  let popover_emoji = reaction.emoji.clone();
  let long_press = LongPress::new(move |_, _| is_open.set(true));
  let wrap_class = move || {
    if is_open.get() {
      "reaction-wrap is-open"
    } else {
      "reaction-wrap"
    }
  };
  view! {
    <span
      class=wrap_class
      on:pointerdown=move |ev| {
        // A press on a chip is the chip's, not the message's long press.
        ev.stop_propagation();
        long_press.start(&ev, true);
      }
      on:pointermove=move |ev| long_press.track(&ev)
      on:pointerup=move |_| long_press.cancel()
      on:pointercancel=move |_| long_press.cancel()
      on:contextmenu=move |ev| {
        if long_press.was_touch() {
          ev.prevent_default();
        }
      }
      on:keydown=move |ev| {
        if ev.key() == "Escape" && is_open.get_untracked() {
          ev.stop_propagation();
          is_open.set(false);
        }
      }
    >
      <button
        class="reaction"
        class:is-popping=is_popping
        style:animation-delay=pop_delay
        class:reaction-mine=reaction.reacted_by_me
        class:is-pressing=move || long_press.is_pressing.get()
        aria-pressed=reaction.reacted_by_me.to_string()
        aria-disabled=(!can_react).to_string()
        aria-label=label.clone()
        title=label
        on:keydown=move |ev| {
          // Shift+F10 or the menu key: the full list of who reacted, as the pointer's "Show everyone" (W-22).
          if has_more && (ev.key() == "ContextMenu" || (ev.shift_key() && ev.key() == "F10")) {
            ev.prevent_default();
            is_open.set(true);
          }
        }
        on:click=move |_| {
          // The long press that opened the list is no tap.
          if long_press.has_fired() || !can_react {
            return;
          }
          state.write(|backend| backend.toggle_reaction(conversation, message, &emoji));
        }
      >
        <span aria-hidden="true"><EmojiGlyph emoji=reaction.emoji maybe_reference=reaction.maybe_custom /></span>
        <span class="reaction-count">{count}</span>
      </button>
      {move || {
        tip().map(|text| {
          view! {
            <span class="reaction-tip" aria-hidden="true">
              <span class="reaction-tip-text">{text}</span>
              {has_more.then(|| view! {
                <button class="reaction-tip-more" tabindex="-1" on:click=move |_| is_open.set(true)>
                  {i18n.t("reaction-show-everyone")}
                </button>
              })}
            </span>
          }
        })
      }}
      {move || is_open.get().then(|| {
        let heading = popover_names.label(i18n.locale(), &popover_emoji);
        let everyone = popover_names.clone();
        // Opened from the keyboard, the focus goes into the list; Escape or Close ends it.
        let close_button = NodeRef::<leptos::html::Button>::new();
        close_button.on_load(|button| {
          let _ = button.focus();
        });
        view! {
          <span class="reaction-scrim" on:click=move |_| is_open.set(false)></span>
          <span class="reaction-popover" role="dialog" aria-label=heading>
            <span class="reaction-popover-title">{heading.clone()}</span>
            <ul class="reaction-popover-list">
              {everyone.has_me.then(|| view! { <li>{i18n.t("reaction-you")}</li> })}
              {everyone.others.iter().map(|name| view! { <li>{name.clone()}</li> }).collect_view()}
            </ul>
            {(everyone.named() < everyone.total).then(|| view! {
              <span class="reaction-popover-rest">
                {i18n.t_count("reaction-others-unnamed", everyone.total - everyone.named())}
              </span>
            })}
            <button class="btn btn-small" node_ref=close_button on:click=move |_| is_open.set(false)>{i18n.t("reaction-close")}</button>
          </span>
        }
      })}
    </span>
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::i18n::Locale;

  fn names(has_me: bool, others: &[&str], total: usize) -> ReactorNames {
    ReactorNames {
      has_me,
      others: others.iter().map(|name| (*name).to_owned()).collect(),
      total,
    }
  }

  #[test]
  fn one_two_and_three_people_are_named() {
    let en = Locale::En;
    assert_eq!(names(false, &["Alex"], 1).phrase(en, 3).unwrap(), "Alex");
    assert_eq!(names(true, &["Sam"], 2).phrase(en, 3).unwrap(), "You and Sam");
    assert_eq!(
      names(true, &["Alex Example", "Sam Example"], 3)
        .phrase(en, 3)
        .unwrap(),
      "You, Alex Example and Sam Example"
    );
  }

  #[test]
  fn many_people_end_in_others_with_the_plural() {
    let en = Locale::En;
    assert_eq!(
      names(false, &["A", "B", "C", "D"], 4).phrase(en, 3).unwrap(),
      "A, B, C and 1 other"
    );
    assert_eq!(
      names(true, &["A", "B", "C"], 12).phrase(en, 3).unwrap(),
      "You, A, B and 9 others"
    );
  }

  #[test]
  fn unnamed_people_only_count() {
    let en = Locale::En;
    assert!(names(false, &[], 2).phrase(en, 3).is_none());
    assert_eq!(names(false, &[], 1).label(en, "+1"), "+1 reaction, 1 person");
    assert_eq!(names(false, &[], 2).label(en, "+1"), "+1 reaction, 2 people");
    assert_eq!(names(false, &["A"], 3).phrase(en, 3).unwrap(), "A and 2 others");
  }
}
