//! Home. DMs and group DMs live outside spaces (PLAN.md, ARCHITECTURE.md:
//! `conversations.space_id` is null for them), so they get their own entry on the space rail and
//! their own sidebar, as in Element or Discord: newest first, with a filter and "new message".

use leptos::prelude::*;

use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::huddle_bar::CallPill;
use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;

use crate::i18n::use_i18n;
use crate::keys::{QUICK_SWITCHER, ShortcutKeys};
use crate::model::{Conversation, ConversationKind, User, UserId};
use crate::prefs::DmSort;
use crate::sections::{DirectList, DirectSectionKind, HomeFilter, build_direct};
use crate::state::{MainView, Overlay, use_app_state};

/// Display limits offered for the list ("0" shows all).
const LIMIT_OPTIONS: [u32; 4] = [0, 5, 10, 20];

/// The sidebar of Home.
#[component]
pub fn HomeSidebar() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let filter = RwSignal::new(String::new());
  let show_all = RwSignal::new(false);
  let show_options = RwSignal::new(false);
  let chip = RwSignal::new(HomeFilter::All);
  let list = Memo::new(move |_| {
    let (sort, limit) = state
      .settings
      .prefs
      .with(|prefs| (prefs.sidebar.dm_sort, prefs.sidebar.dm_limit as usize));
    let limit = if show_all.get() { 0 } else { limit };
    build_direct(state.direct_entries(), sort, limit, &filter.get(), chip.get())
  });
  let has_any = Memo::new(move |_| state.read(|backend| !backend.direct_conversations().is_empty()));

  view! {
    <nav class="sidebar dm-sidebar" aria-label=move || i18n.t("home-title")>
      <div class="sidebar-head">
        <h2 class="space-name">{move || i18n.t("home-title")}</h2>
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
        <button
          class="icon-btn"
          aria-label=move || i18n.t("dm-new")
          title=move || i18n.t("dm-new")
          on:click=move |_| state.overlay.set(Overlay::NewMessage)
        >
          <Icon name=IconName::Compose />
        </button>
      </div>
      <Show when=move || show_options.get() fallback=|| ()>
        <DirectOptions />
      </Show>
      <button class="jump-btn" on:click=move |_| state.overlay.set(Overlay::QuickSwitcher)>
        <Icon name=IconName::Search />
        <span class="jump-label">{move || i18n.t("sidebar-jump")}</span>
        <ShortcutKeys shortcut=QUICK_SWITCHER />
      </button>
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
      <div class="dm-filter">
        <label class="visually-hidden" for="dm-filter">{move || i18n.t("dm-filter")}</label>
        <input
          id="dm-filter"
          class="input"
          type="search"
          placeholder=move || i18n.t("dm-filter")
          prop:value=move || filter.get()
          on:input=move |ev| filter.set(event_target_value(&ev))
          on:keydown=move |ev| {
            if ev.key() == "Escape" && !filter.get_untracked().is_empty() {
              ev.stop_propagation();
              filter.set(String::new());
            }
          }
        />
      </div>
      <div class="sidebar-scroll">
        {move || {
          let current = list.get();
          if !has_any.get() {
            view! { <DirectEmpty /> }.into_any()
          } else if current.sections.is_empty() {
            view! { <p class="sb-empty">{move || i18n.t("dm-filter-empty")}</p> }.into_any()
          } else {
            view! { <DirectSections list=current show_all=show_all /> }.into_any()
          }
        }}
      </div>
      <CallPill />
    </nav>
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
fn DirectSections(list: DirectList, show_all: RwSignal<bool>) -> impl IntoView {
  let i18n = use_i18n();
  let hidden = list.hidden_count;
  view! {
    {list
      .sections
      .into_iter()
      .map(|section| {
        let kind = section.kind;
        let heading_id = format!("dm-section-{}", section_slug(kind));
        let labelled_by = heading_id.clone();
        view! {
          <section class="sb-section" aria-labelledby=labelled_by>
            <h3 id=heading_id class="sb-section-title">{move || i18n.t(kind.title_key())}</h3>
            <ul>
              {section
                .conversations
                .into_iter()
                .map(|conversation| view! { <li><DirectItem conversation=conversation /></li> })
                .collect_view()}
            </ul>
          </section>
        }
      })
      .collect_view()}
    {(hidden > 0)
      .then(|| {
        view! {
          <button class="sb-more" on:click=move |_| show_all.set(true)>
            {move || i18n.t_count("sidebar-show-more", hidden)}
          </button>
        }
      })}
  }
}

fn section_slug(kind: DirectSectionKind) -> &'static str {
  match kind {
    DirectSectionKind::Favorites => "favorites",
    DirectSectionKind::Conversations => "conversations",
  }
}

/// One DM or group DM: avatar with presence (or the group icon), name, bot marker, status and
/// unread count; bold while unread.
#[component]
fn DirectItem(conversation: Conversation) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = conversation.id;
  let is_current = move || state.view.get() == MainView::Conversation(id);
  let is_unread = conversation.unread_count > 0 && !conversation.is_muted;
  // DMs are addressed to the user, so every unread message counts (muted ones only mentions).
  let count = if conversation.is_muted {
    conversation.mention_count
  } else {
    conversation.unread_count.max(conversation.mention_count)
  };
  let me = state.me().id;
  let maybe_other = (conversation.kind == ConversationKind::Direct)
    .then(|| {
      conversation
        .member_ids
        .iter()
        .copied()
        .find(|member| *member != me)
    })
    .flatten();
  let leading = match maybe_other {
    // Presence arrives later than the row is built, so the avatar is read reactively.
    Some(other) => view! {
      <span class="home-avatar">
        {move || {
          state
            .read(|backend| backend.user(other))
            .map(|user| view! { <Avatar user=user size=AvatarSize::Small show_presence=true /> })
        }}
      </span>
    }
    .into_any(),
    None => {
      view! { <span class="home-avatar home-avatar-group"><Icon name=IconName::Users /></span> }.into_any()
    },
  };
  let maybe_other_user = maybe_other.and_then(|other| state.read(|backend| backend.user(other)));
  let is_bot = maybe_other_user.as_ref().is_some_and(|user| user.is_bot);
  let status_emoji = maybe_other_user
    .and_then(|user| user.maybe_custom_status)
    .map(|status| status.emoji);
  let name = conversation.name.clone();
  let aria_name = name.clone();
  let label = move || {
    if count > 0 {
      i18n.t_with(
        "dm-item-unread",
        &[("name", &aria_name), ("count", &count.to_string())],
      )
    } else {
      aria_name.clone()
    }
  };

  view! {
    <button
      class="sb-item"
      class:current=is_current
      class:unread=is_unread
      class:muted=conversation.is_muted
      data-conversation=id.to_string()
      aria-current=move || is_current().then_some("page")
      aria-label=label
      on:click=move |_| state.open_conversation(id)
    >
      {leading}
      <span class="sb-name" translate="no">{name}</span>
      {is_bot.then(|| view! { <span class="badge">{move || i18n.t("badge-bot")}</span> })}
      {status_emoji.map(|emoji| view! { <span class="sb-status" aria-hidden="true">{emoji}</span> })}
      {move || {
        maybe_other
          .filter(|other| state.read(|backend| backend.is_in_call(*other)))
          .map(|_| view! { <span class="sb-call" title=i18n.t("presence-in-call")><Icon name=IconName::Phone /></span> })
      }}
      {(count > 0).then(|| view! { <span class="badge-count" aria-hidden="true">{count}</span> })}
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
      <button class="btn btn-primary" on:click=move |_| state.overlay.set(Overlay::NewMessage)>
        {move || i18n.t("dm-new")}
      </button>
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
      <Icon name=IconName::Home class="dm-home-icon" />
      <h1 id="dm-home-title">{move || i18n.t("home-title")}</h1>
      <p>{move || i18n.t("dm-home-hint")}</p>
      <button class="btn btn-primary" on:click=move |_| state.overlay.set(Overlay::NewMessage)>
        {move || i18n.t("dm-new")}
      </button>
    </section>
  }
}

/// Pick one person (a DM) or several (a group DM) and open the conversation with them.
#[component]
pub fn NewMessageDialog() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let filter = RwSignal::new(String::new());
  let chosen = RwSignal::new(Vec::<User>::new());
  let candidates = Memo::new(move |_| {
    let me = state.me().id;
    let picked: Vec<UserId> = chosen.with(|list| list.iter().map(|user| user.id).collect());
    let mut users = state.read(|backend| backend.users());
    users.retain(|user| user.id != me && !user.is_bot && !picked.contains(&user.id));
    people_matching(users, &filter.get())
  });
  let input = NodeRef::<leptos::html::Input>::new();
  let start = move || {
    let users: Vec<UserId> = chosen.with_untracked(|list| list.iter().map(|user| user.id).collect());
    if users.is_empty() {
      return;
    }
    state.close_overlay();
    state.open_direct_with(users);
  };
  let pick = move |user: User| {
    chosen.update(|list| list.push(user));
    filter.set(String::new());
    // Back to the search field for the next person.
    if let Some(element) = input.get_untracked() {
      drop(element.focus());
    }
  };
  // Enter picks the first match while searching, and opens the conversation otherwise.
  let submit = move || {
    let maybe_first = (!filter.get_untracked().trim().is_empty())
      .then(|| candidates.get_untracked().into_iter().next())
      .flatten();
    match maybe_first {
      Some(user) => pick(user),
      None => start(),
    }
  };

  view! {
    <Modal labelled_by="new-message-title" class="modal-small new-message">
      <form on:submit=move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        submit();
      }>
        <header class="modal-head">
          <h2 id="new-message-title">{move || i18n.t("dm-new")}</h2>
          <button type="button" class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
            <Icon name=IconName::Close />
          </button>
        </header>
        <div class="modal-body">
          <ul class="chip-list" aria-label=move || i18n.t("dm-new-chosen")>
            <For each=move || chosen.get() key=|user| user.id let:user>
              {
                let id = user.id;
                let remove_label = {
                  let name = user.display_name.clone();
                  move || i18n.t_with("dm-new-remove", &[("name", &name)])
                };
                view! {
                  <li class="chip">
                    <span translate="no">{user.display_name.clone()}</span>
                    <button
                      type="button"
                      class="icon-btn icon-btn-small"
                      aria-label=remove_label.clone()
                      title=remove_label
                      on:click=move |_| chosen.update(|list| list.retain(|user| user.id != id))
                    >
                      <Icon name=IconName::Close />
                    </button>
                  </li>
                }
              }
            </For>
          </ul>
          <label class="visually-hidden" for="new-message-filter">{move || i18n.t("dm-new-filter")}</label>
          <input
            id="new-message-filter"
            node_ref=input
            class="input"
            data-autofocus
            autocomplete="off"
            placeholder=move || i18n.t("dm-new-filter")
            prop:value=move || filter.get()
            on:input=move |ev| filter.set(event_target_value(&ev))
            on:keydown=move |ev| {
              // Backspace in the empty field takes back the last person, as in other pickers.
              if ev.key() == "Backspace" && filter.get_untracked().is_empty() {
                chosen.update(|list| {
                  list.pop();
                });
              }
            }
          />
          <ul class="forward-list" role="listbox" aria-label=move || i18n.t("dm-new-people")>
            <For each=move || candidates.get() key=|user| user.id let:user>
              {
                let picked = user.clone();
                view! {
                  <li role="option" aria-selected="false">
                    <button type="button" class="forward-item person-item" on:click=move |_| pick(picked.clone())>
                      <Avatar user=user.clone() size=AvatarSize::Small show_presence=true />
                      <span translate="no">{user.display_name.clone()}</span>
                      {(!user.username.is_empty()).then(|| view! { <span class="muted-text" translate="no">{format!("@{}", user.username)}</span> })}
                    </button>
                  </li>
                }
              }
            </For>
          </ul>
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

/// People whose name or username contains `filter` (any case), by display name.
fn people_matching(mut users: Vec<User>, filter: &str) -> Vec<User> {
  let needle = filter.trim().to_lowercase();
  users.retain(|user| {
    needle.is_empty()
      || user.display_name.to_lowercase().contains(&needle)
      || user.username.to_lowercase().contains(&needle)
  });
  users.sort_by_key(|user| user.display_name.to_lowercase());
  users
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{PresenceStatus, id_from_raw};

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
    let en = include_str!("../../i18n/en.ftl");
    let de = include_str!("../../i18n/de.ftl");
    for key in keys {
      let line = format!("\n{key} = ");
      assert!(
        en.contains(&line) && de.contains(&line),
        "missing translation {key}"
      );
    }
  }

  #[test]
  fn people_match_by_name_or_username() {
    let users = vec![
      user(1, "Zoe Example", "zoe"),
      user(2, "amy Example", "a.example"),
      user(3, "Bob", "bob"),
    ];
    let names = |list: Vec<User>| list.into_iter().map(|user| user.display_name).collect::<Vec<_>>();
    assert_eq!(
      names(people_matching(users.clone(), "")),
      ["amy Example", "Bob", "Zoe Example"]
    );
    assert_eq!(
      names(people_matching(users.clone(), " EXAMPLE")),
      ["amy Example", "Zoe Example"]
    );
    assert_eq!(names(people_matching(users.clone(), "a.ex")), ["amy Example"]);
    assert!(people_matching(users, "nobody").is_empty());
  }

  fn user(id: u64, name: &str, username: &str) -> User {
    User {
      id: id_from_raw(id),
      username: username.to_owned(),
      display_name: name.to_owned(),
      presence: PresenceStatus::Online,
      maybe_custom_status: None,
      avatar_hue: 0,
      is_guest: false,
      is_bot: false,
      maybe_organization: None,
    }
  }
}
