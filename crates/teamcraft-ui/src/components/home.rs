//! Chats (the rail's first entry). DMs and group DMs live outside spaces (PLAN.md, ARCHITECTURE.md:
//! `conversations.space_id` is null for them), so they get their own entry on the space rail and
//! their own sidebar, as in Element or Discord: newest first, with a text filter, filter chips and "new message".

use leptos::prelude::*;

use crate::channel_access::{can_invite_to, can_start_any_direct};
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::channel_view::shown_name;
use crate::components::huddle_bar::CallPill;
use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::components::setup_checklist::SetupChecklist;
use crate::components::sidebar::{
  EntriesDisclosure, EntriesToggle, GlobalEntries, ListFilter, ListFilterField, ListFilterToggle,
};

use crate::components::people_picker::PeoplePicker;
use crate::i18n::use_i18n;
use crate::model::{Conversation, ConversationKind, User, UserId};
use crate::prefs::DmSort;
use crate::reactive::{items_by_key, keyed_item};
use crate::sections::{DirectList, DirectSectionKind, HomeFilter, build_direct};
use crate::state::{MainView, Overlay, use_app_state};

/// Display limits offered for the list ("0" shows all).
const LIMIT_OPTIONS: [u32; 4] = [0, 5, 10, 20];

/// The sidebar of Home.
#[component]
pub fn HomeSidebar() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let show_all = RwSignal::new(false);
  let show_options = RwSignal::new(false);
  let chip = RwSignal::new(HomeFilter::All);
  let filter = ListFilter::new();
  let query = filter.query;
  let entries = EntriesDisclosure::new();
  let list = Memo::new(move |_| {
    let (sort, limit) = state
      .settings
      .prefs
      .with(|prefs| (prefs.sidebar.dm_sort, prefs.sidebar.dm_limit as usize));
    let limit = if show_all.get() { 0 } else { limit };
    build_direct(state.direct_entries(), sort, limit, &query.get(), chip.get())
  });
  let has_any = Memo::new(move |_| state.read(|backend| !backend.direct_conversations().is_empty()));
  let is_filtered_out = Memo::new(move |_| list.with(|list| list.sections.is_empty()));

  view! {
    <nav class="sidebar dm-sidebar" aria-label=move || i18n.t("chats-title")>
      <div class="sidebar-head">
        <h2 class="space-name">{move || i18n.t("chats-title")}</h2>
        <EntriesToggle entries=entries controls="chats-entries" />
        <ListFilterToggle filter=filter field_id="chats-filter" label_key="chats-filter" />
        <button
          class="icon-btn"
          aria-label=move || i18n.t("dm-options")
          title=move || i18n.t("dm-options")
          aria-expanded=move || show_options.get().to_string()
          aria-controls="dm-options"
          on:click=move |_| show_options.update(|shown| *shown = !*shown)
        >
          <Icon name=IconName::More />
        </button>
        <Show when=move || can_start_any_direct(state) fallback=|| ()>
          <button
            class="icon-btn"
            aria-label=move || i18n.t("dm-new")
            title=move || i18n.t("dm-new")
            on:click=move |_| state.overlay.set(Overlay::NewMessage)
          >
            <Icon name=IconName::Compose />
          </button>
        </Show>
      </div>
      <Show when=move || show_options.get() fallback=|| ()>
        <DirectOptions />
      </Show>
      <GlobalEntries entries=entries id="chats-entries" />
      <ListFilterField filter=filter id="chats-filter" label_key="chats-filter" />
      <div class="home-chips" role="group" aria-label=move || i18n.t("home-filters")>
        {HomeFilter::CHIPS
          .into_iter()
          .map(|kind| {
            view! {
              <button
                type="button"
                class="home-chip"
                aria-pressed=move || (chip.get() == kind).to_string()
                on:click=move |_| chip.update(|current| {
                  // Pressing the selected chip again shows everything.
                  *current = if *current == kind { HomeFilter::All } else { kind };
                })
              >
                {move || i18n.t(kind.label_key())}
              </button>
            }
          })
          .collect_view()}
      </div>
      <div class="sidebar-scroll">
        // Only a change between these three builds the list again; its rows follow `list` in place.
        {move || {
          if !has_any.get() {
            view! { <DirectEmpty /> }.into_any()
          } else if is_filtered_out.get() {
            view! { <p class="sb-empty">{move || i18n.t("dm-filter-empty")}</p> }.into_any()
          } else {
            view! { <DirectSections list=list show_all=show_all /> }.into_any()
          }
        }}
      </div>
      <InviteFooter />
      <CallPill />
    </nav>
  }
}

/// "Invite people" under the list, for people who may invite to the current space (not guests).
#[component]
fn InviteFooter() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let can_invite = Memo::new(move |_| can_invite_to(state, state.space.get()));
  view! {
    <Show when=move || can_invite.get() fallback=|| ()>
      <button class="sb-more sb-footer" on:click=move |_| state.overlay.set(Overlay::InvitePeople(state.space.get_untracked()))>
        {move || i18n.t("invite-title")}
      </button>
    </Show>
  }
}

/// Sort order and display limit of the list (moved here from the sidebar settings).
#[component]
fn DirectOptions() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let prefs = state.settings.prefs;
  view! {
    <div id="dm-options" class="dm-options">
      <label for="dm-sort">{move || i18n.t("dm-sort")}</label>
      <select
        id="dm-sort"
        class="input"
        on:change=move |ev| {
          if let Some(sort) = DmSort::from_slug(&event_target_value(&ev)) {
            prefs.update(|all| all.sidebar.dm_sort = sort);
          }
        }
      >
        {DmSort::ALL
          .into_iter()
          .map(|sort| {
            view! {
              <option value=sort.slug() selected=move || prefs.with(|all| all.sidebar.dm_sort == sort)>
                {move || i18n.t(&format!("dm-sort-{}", sort.slug()))}
              </option>
            }
          })
          .collect_view()}
      </select>
      <label for="dm-limit">{move || i18n.t("settings-dm-limit")}</label>
      <select
        id="dm-limit"
        class="input"
        aria-describedby="dm-limit-hint"
        on:change=move |ev| {
          if let Ok(limit) = event_target_value(&ev).parse::<u32>() {
            prefs.update(|all| all.sidebar.dm_limit = limit);
          }
        }
      >
        {LIMIT_OPTIONS
          .into_iter()
          .map(|limit| {
            view! {
              <option value=limit.to_string() selected=move || prefs.with(|all| all.sidebar.dm_limit == limit)>
                {move || i18n.t(&format!("settings-dm-limit-{limit}"))}
              </option>
            }
          })
          .collect_view()}
      </select>
      <p id="dm-limit-hint" class="muted-text small">{move || i18n.t("settings-dm-limit-hint")}</p>
    </div>
  }
}

#[component]
fn DirectSections(list: Memo<DirectList>, show_all: RwSignal<bool>) -> impl IntoView {
  let i18n = use_i18n();
  let hidden = Memo::new(move |_| list.with(|list| list.hidden_count));
  // Sections and rows are built once and follow the list (issue #687): a new message reorders the
  // rows and changes a count, it doesn't build the avatars again.
  view! {
    <For
      each=move || list.with(|list| list.sections.iter().map(|section| section.kind).collect::<Vec<_>>())
      key=|kind| *kind
      let:kind
    >
      {
        let heading_id = format!("dm-section-{}", section_slug(kind));
        let labelled_by = heading_id.clone();
        // The pinned notes need no heading of their own: the row says what it is.
        let is_notes = kind == DirectSectionKind::Notes;
        let conversations = Memo::new(move |previous: Option<&Vec<Conversation>>| {
          list
            .with(|list| {
              list
                .sections
                .iter()
                .find(|section| section.kind == kind)
                .map(|section| section.conversations.clone())
            })
            .or_else(|| previous.cloned())
            .unwrap_or_default()
        });
        let by_id = items_by_key(conversations.into(), |conv: &Conversation| conv.id);
        view! {
          <section class="sb-section" aria-labelledby=labelled_by>
            <h3 id=heading_id class="sb-section-title" class:visually-hidden=is_notes>{move || i18n.t(kind.title_key())}</h3>
            <ul>
              <For each=move || conversations.get() key=|conv| conv.id let:conv>
                <li><DirectItem conversation=keyed_item(by_id, conv.id, conv) /></li>
              </For>
            </ul>
          </section>
        }
      }
    </For>
    {move || {
      let hidden = hidden.get();
      (hidden > 0)
        .then(|| {
          view! {
            <button class="sb-more" on:click=move |_| show_all.set(true)>
              {move || i18n.t_count("sidebar-show-more", hidden)}
            </button>
          }
        })
    }}
  }
}

fn section_slug(kind: DirectSectionKind) -> &'static str {
  match kind {
    DirectSectionKind::Notes => "notes",
    DirectSectionKind::Favorites => "favorites",
    DirectSectionKind::Conversations => "conversations",
    DirectSectionKind::Deactivated => "deactivated",
  }
}

/// One DM or group DM: avatar with presence (or the group icon), name, bot marker, status and
/// unread count; bold while unread. Built once per row; what it shows follows `conversation`.
#[component]
fn DirectItem(#[prop(into)] conversation: Signal<Conversation>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  // A conversation keeps its id, kind and members' other person.
  let (id, kind, is_notes, member_ids) =
    conversation.with_untracked(|conv| (conv.id, conv.kind, conv.is_notes_to_self, conv.member_ids.clone()));
  let is_current = move || state.view.get() == MainView::Conversation(id);
  let is_muted = Memo::new(move |_| conversation.with(|conv| conv.is_muted));
  let is_unread = Memo::new(move |_| conversation.with(|conv| conv.unread_count > 0 && !conv.is_muted));
  // DMs are addressed to the user, so every unread message counts (muted ones only mentions).
  let count = Memo::new(move |_| {
    conversation.with(|conv| {
      if conv.is_muted {
        conv.mention_count
      } else {
        conv.unread_count.max(conv.mention_count)
      }
    })
  });
  let me = state.me().id;
  let maybe_other = (kind == ConversationKind::Direct)
    .then(|| member_ids.iter().copied().find(|member| *member != me))
    .flatten();
  let maybe_other_user =
    Memo::new(move |_| maybe_other.and_then(|other| state.read(|backend| backend.user(other))));
  let leading = match maybe_other {
    // Notes to self show the user's own picture, as other chat apps do.
    _ if is_notes => {
      let me = state.me();
      view! { <span class="home-avatar"><Avatar user=me size=AvatarSize::Small /></span> }.into_any()
    },
    // Presence arrives later than the row is built, so the avatar is read reactively.
    Some(_) => view! {
      <span class="home-avatar">
        {move || {
          maybe_other_user
            .get()
            .map(|user| view! { <Avatar user=user size=AvatarSize::Small show_presence=true /> })
        }}
      </span>
    }
    .into_any(),
    None => {
      view! { <span class="home-avatar home-avatar-group"><Icon name=IconName::Users /></span> }.into_any()
    },
  };
  let is_bot = Memo::new(move |_| {
    maybe_other_user.with(|maybe_user| maybe_user.as_ref().is_some_and(|user| user.is_bot))
  });
  let status_emoji = Memo::new(move |_| {
    maybe_other_user.with(|maybe_user| {
      maybe_user
        .as_ref()
        .and_then(|user| user.maybe_custom_status.as_ref())
        .map(|status| status.emoji.clone())
    })
  });
  let name = Memo::new(move |_| conversation.with(|conv| shown_name(i18n, conv)));
  let is_peer_in_call =
    Memo::new(move |_| maybe_other.is_some_and(|other| state.read(|backend| backend.is_in_call(other))));
  let label = move || {
    let aria_name = name.get();
    let count = count.get();
    let label = if count > 0 {
      i18n.t_with(
        "dm-item-unread",
        &[("name", &aria_name), ("count", &count.to_string())],
      )
    } else {
      aria_name
    };
    if is_muted.get() {
      i18n.t_with("sidebar-item-muted-label", &[("label", &label)])
    } else {
      label
    }
  };

  view! {
    <button
      class="sb-item"
      class:current=is_current
      class:unread=move || is_unread.get()
      class:muted=move || is_muted.get()
      data-conversation=id.to_string()
      aria-current=move || is_current().then_some("page")
      aria-label=label
      on:click=move |_| state.open_conversation(id)
    >
      {leading}
      <span class="sb-name" translate="no">{move || name.get()}</span>
      {move || is_bot.get().then(|| view! { <span class="badge">{move || i18n.t("badge-bot")}</span> })}
      {move || status_emoji.get().map(|emoji| view! { <span class="sb-status" aria-hidden="true">{emoji}</span> })}
      {move || {
        is_peer_in_call
          .get()
          .then(|| view! { <span class="sb-call" title=i18n.t("presence-in-call")><Icon name=IconName::Phone /></span> })
      }}
      {move || is_muted.get().then(|| view! { <span class="sb-muted-icon" title=i18n.t("sidebar-item-muted")><Icon name=IconName::BellOff /></span> })}
      {move || {
        let count = count.get();
        (count > 0).then(|| view! { <span class="badge-count" aria-hidden="true">{count}</span> })
      }}
    </button>
  }
}

/// No DMs yet: say where they come from and offer to write one.
#[component]
fn DirectEmpty() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  view! {
    <div class="sb-empty">
      <p>{move || i18n.t("dm-empty")}</p>
      <Show when=move || can_start_any_direct(state) fallback=|| ()>
        <button class="btn btn-primary" on:click=move |_| state.overlay.set(Overlay::NewMessage)>
          {move || i18n.t("dm-new")}
        </button>
      </Show>
    </div>
  }
}

/// The main column of Home while no conversation is open.
#[component]
pub fn HomeStart() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  view! {
    <section class="dm-home" aria-labelledby="dm-home-title">
      <Icon name=IconName::Chat class="dm-home-icon" />
      <h1 id="dm-home-title">{move || i18n.t("chats-title")}</h1>
      <p>{move || i18n.t("dm-home-hint")}</p>
      <Show when=move || can_start_any_direct(state) fallback=|| ()>
        <button class="btn btn-primary" on:click=move |_| state.overlay.set(Overlay::NewMessage)>
          {move || i18n.t("dm-new")}
        </button>
      </Show>
      <SetupChecklist />
    </section>
  }
}

/// Pick one person (a DM) or several (a group DM) and open the conversation with them.
#[component]
pub fn NewMessageDialog() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let chosen = RwSignal::new(Vec::<User>::new());
  let start = move || {
    let users: Vec<UserId> = chosen.with_untracked(|list| list.iter().map(|user| user.id).collect());
    if users.is_empty() {
      return;
    }
    state.close_overlay();
    state.open_direct_with(users);
  };

  view! {
    <Modal labelled_by="new-message-title" class="modal-small new-message">
      <form on:submit=move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        start();
      }>
        <header class="modal-head">
          <h2 id="new-message-title">{move || i18n.t("dm-new")}</h2>
          <button type="button" class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
            <Icon name=IconName::Close />
          </button>
        </header>
        <div class="modal-body">
          // Enter picks the first match while searching, and opens the conversation otherwise.
          <PeoplePicker chosen=chosen chosen_label_key="dm-new-chosen" allows_guest_accounts=true on_submit=Callback::new(move |()| start()) />
          <p class="muted-text small">{move || i18n.t("dm-new-hint")}</p>
        </div>
        <footer class="modal-foot">
          <span class="grow"></span>
          <button type="button" class="btn" on:click=move |_| state.close_overlay()>{move || i18n.t("cancel")}</button>
          <button type="button" class="btn btn-primary" prop:disabled=move || chosen.with(Vec::is_empty) on:click=move |_| start()>
            {move || {
              if chosen.with(|list| list.len() > 1) { i18n.t("dm-new-start-group") } else { i18n.t("dm-new-start") }
            }}
          </button>
        </footer>
      </form>
    </Modal>
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn keys_built_from_options_exist() {
    let mut keys: Vec<String> = DmSort::ALL
      .iter()
      .map(|sort| format!("dm-sort-{}", sort.slug()))
      .collect();
    keys.extend(
      LIMIT_OPTIONS
        .iter()
        .map(|limit| format!("settings-dm-limit-{limit}")),
    );
    let en = crate::i18n::joined_source(crate::i18n::Locale::En);
    let de = crate::i18n::joined_source(crate::i18n::Locale::De);
    for key in keys {
      let line = format!("\n{key} = ");
      assert!(
        en.contains(&line) && de.contains(&line),
        "missing translation {key}"
      );
    }
  }
}
