use leptos::prelude::*;

use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::model::{Space, initials};
use crate::state::{MainView, use_app_state};

/// Space switcher on the far left (Mattermost "team sidebar").
#[component]
pub fn SpaceRail() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let spaces = Memo::new(move |_| state.read(|backend| backend.spaces()));

  view! {
    <nav class="space-rail" aria-label=move || i18n.t("rail-label")>
      <ul>
        <For each=move || spaces.get() key=|space| (space.id, space.unread_count, space.mention_count) let:space>
          <SpaceButton space=space />
        </For>
      </ul>
      <button class="rail-add" disabled aria-label=move || i18n.t("rail-add-space") title=move || i18n.t("rail-add-space")>
        <Icon name=IconName::Plus />
      </button>
    </nav>
  }
}

#[component]
fn SpaceButton(space: Space) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = space.id;
  let is_current = move || state.space.get() == id;
  let label = {
    let name = space.name.clone();
    move || {
      if space.mention_count > 0 {
        i18n.t_with(
          "rail-space-mentions",
          &[("name", &name), ("count", &space.mention_count.to_string())],
        )
      } else if space.unread_count > 0 {
        i18n.t_with("rail-space-unread", &[("name", &name)])
      } else {
        name.clone()
      }
    }
  };

  let first_conversation = move |space| {
    state.read(|backend| {
      backend
        .space_conversations(space)
        .into_iter()
        .next()
        .map(|conv| conv.id)
    })
  };

  view! {
    <li>
      <button
        class="space-btn"
        class:current=is_current
        style=format!("--hue: {}", space.hue)
        aria-label=label.clone()
        title=label
        aria-current=move || is_current().then_some("true")
        on:click=move |_| {
          state.space.set(id);
          // Land in the first channel unless the current conversation already belongs to the space.
          let belongs = match state.view.get_untracked() {
            MainView::Conversation(current) => state
              .read(|backend| backend.conversation(current))
              .is_some_and(|conv| conv.maybe_space == Some(id) || conv.maybe_space.is_none()),
            MainView::Threads => true,
            MainView::Admin(_) => false,
          };
          if !belongs && let Some(conversation) = first_conversation(id) {
            state.open_conversation(conversation);
          }
        }
      >
        <span aria-hidden="true">{initials(&space.name)}</span>
        {(space.mention_count > 0).then(|| view! { <span class="badge-count" aria-hidden="true">{space.mention_count}</span> })}
        {(space.mention_count == 0 && space.unread_count > 0).then(|| view! { <span class="dot-unread" aria-hidden="true"></span> })}
      </button>
    </li>
  }
}
