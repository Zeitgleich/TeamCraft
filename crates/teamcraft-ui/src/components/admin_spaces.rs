//! Spaces in the administration console (#536): a row per space with its counts, and the actions
//! admin.md §3.1 gives admins without any content access (rename, archive and restore, how people
//! join). Membership and roles stay with the space's own owners and the signed policy.

use leptos::prelude::*;

use teamcraft_proto::JoinPolicy;

use crate::admin::{AdminLoad, AdminSpace, AdminSpaceChanges};
use crate::clock::{format_relative, now_ms};
use crate::components::icons::{Icon, IconName};
use crate::components::skeleton::ListSkeleton;
use crate::i18n::use_i18n;
use crate::model::SpaceId;
use crate::state::use_app_state;

const SKELETON_ROWS: usize = 6;

/// Stands in for a list that has not arrived: placeholder rows while it loads, and the reason with
/// a retry when it failed, so an empty instance never looks like a slow or broken one.
#[component]
pub fn LoadNotice(#[prop(into)] load: Signal<AdminLoad>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  move || match load.get() {
    AdminLoad::Loading | AdminLoad::Loaded => view! {
      <ul class="skeleton-list">
        <ListSkeleton rows=SKELETON_ROWS label=Signal::derive(move || i18n.t("admin-loading")) />
      </ul>
    }
    .into_any(),
    AdminLoad::Failed(reason) => view! {
      <div class="empty-state is-rich" role="alert">
        <p>{move || i18n.t("admin-load-failed")}</p>
        <p class="muted-text small" translate="no">{reason}</p>
        <button class="btn" on:click=move |_| state.read(|backend| backend.admin_refresh())>
          {move || i18n.t("data-retry")}
        </button>
      </div>
    }
    .into_any(),
  }
}

/// All spaces with the actions an admin has on them.
#[component]
pub fn SpacesPanel(query: RwSignal<String>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let selected = RwSignal::new(None::<SpaceId>);
  let now = now_ms();
  let load = Memo::new(move |_| state.read(|backend| backend.admin_load()));
  let spaces = Memo::new(move |_| state.read(|backend| backend.admin_spaces()));
  let visible = Memo::new(move |_| {
    let needle = query.get().trim().to_lowercase();
    let mut list: Vec<AdminSpace> = spaces
      .get()
      .into_iter()
      .filter(|space| needle.is_empty() || space.name.to_lowercase().contains(&needle))
      .collect();
    list.sort_by_key(|space| (space.is_archived, space.name.to_lowercase()));
    list
  });

  view! {
    <div class="admin-split">
      <div class="table-wrap">
        <Show when=move || load.get() == AdminLoad::Loaded fallback=move || view! { <LoadNotice load=load /> }>
          <table class="data-table">
            <caption class="visually-hidden">{move || i18n.t("admin-tab-spaces")}</caption>
            <thead>
              <tr>
                <th scope="col">{move || i18n.t("admin-col-space")}</th>
                <th scope="col" class="num">{move || i18n.t("admin-col-members")}</th>
                <th scope="col" class="num">{move || i18n.t("admin-col-channels")}</th>
                <th scope="col">{move || i18n.t("admin-col-created")}</th>
              </tr>
            </thead>
            <tbody>
              <For each=move || visible.get() key=|space| space.id let:space>
                <SpaceRow space=space selected=selected now=now />
              </For>
            </tbody>
          </table>
          <Show when=move || visible.get().is_empty() fallback=|| ()>
            <p class="empty-state">{move || i18n.t("admin-empty")}</p>
          </Show>
        </Show>
      </div>
      <aside class="detail-pane" aria-label=move || i18n.t("admin-detail-label")>
        {move || match selected.get() {
          Some(id) => view! { <SpaceDetail id=id spaces=spaces /> }.into_any(),
          None => view! { <p class="empty-state">{move || i18n.t("admin-space-select")}</p> }.into_any(),
        }}
      </aside>
    </div>
  }
}

#[component]
fn SpaceRow(space: AdminSpace, selected: RwSignal<Option<SpaceId>>, now: i64) -> impl IntoView {
  let i18n = use_i18n();
  let id = space.id;
  let is_selected = move || selected.get() == Some(id);
  let created = move || format_relative(space.created_at, now, i18n.locale().tag());

  view! {
    <tr class:selected=is_selected>
      <th scope="row">
        <button class="row-button" aria-pressed=move || is_selected().to_string() on:click=move |_| selected.set(Some(id))>
          <Icon name=IconName::Layers />
          <span class="row-name" translate="no">{space.name.clone()}</span>
          {(space.join_policy == JoinPolicy::InviteOnly).then(|| view! { <span class="badge">{move || i18n.t("admin-space-invite-only")}</span> })}
          {space.is_archived.then(|| view! { <span class="badge badge-warn">{move || i18n.t("admin-space-archived")}</span> })}
        </button>
      </th>
      <td class="num">
        {space.member_count}
        {(space.guest_count > 0).then(|| view! { <span class="muted-text">{format!(" (+{})", space.guest_count)}</span> })}
      </td>
      <td class="num">{space.channel_count}</td>
      <td>{created}</td>
    </tr>
  }
}

/// Created once per selection, so a refresh does not reset what is typed into the name field.
#[component]
fn SpaceDetail(id: SpaceId, spaces: Memo<Vec<AdminSpace>>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let maybe_space = Memo::new(move |_| spaces.get().into_iter().find(|space| space.id == id));
  let name = RwSignal::new(
    maybe_space
      .get_untracked()
      .map(|space| space.name)
      .unwrap_or_default(),
  );
  let change =
    move |changes: AdminSpaceChanges| state.write(|backend| backend.change_admin_space(id, changes));
  let is_rename_valid = move || {
    let trimmed = name.get();
    let trimmed = trimmed.trim();
    !trimmed.is_empty() && maybe_space.with(|space| space.as_ref().is_some_and(|space| space.name != trimmed))
  };
  let rename = move || {
    if is_rename_valid() {
      change(AdminSpaceChanges {
        maybe_name: Some(name.get_untracked().trim().to_owned()),
        ..AdminSpaceChanges::default()
      });
    }
  };

  move || {
    let Some(space) = maybe_space.get() else {
      return view! { <p class="empty-state">{move || i18n.t("admin-space-select")}</p> }.into_any();
    };
    let is_archived = space.is_archived;
    let join_policy = space.join_policy.clone();
    view! {
      <h3 translate="no">{space.name.clone()}</h3>
      <dl class="meta-list">
        <dt>{move || i18n.t("admin-col-members")}</dt>
        <dd>{move || i18n.t_count("members-count", space.member_count as usize)}</dd>
        <dt>{move || i18n.t("admin-guests")}</dt>
        <dd>{space.guest_count}</dd>
        <dt>{move || i18n.t("admin-col-channels")}</dt>
        <dd>{space.channel_count}</dd>
      </dl>
      <p class="no-content-note">
        <Icon name=IconName::EyeOff />
        <span>{move || i18n.t("admin-no-content")}</span>
      </p>
      <form
        class="field inline"
        on:submit=move |ev: leptos::ev::SubmitEvent| {
          ev.prevent_default();
          rename();
        }
      >
        <label class="visually-hidden" for="admin-space-name">{move || i18n.t("admin-space-name")}</label>
        <input
          id="admin-space-name"
          class="input"
          autocomplete="off"
          prop:value=move || name.get()
          on:input=move |ev| name.set(event_target_value(&ev))
        />
        <button type="submit" class="btn" prop:disabled=move || !is_rename_valid()>{move || i18n.t("admin-space-rename")}</button>
      </form>
      <h4 class="detail-heading">{move || i18n.t("admin-space-join")}</h4>
      <div class="segmented" role="group" aria-label=move || i18n.t("admin-space-join")>
        {[JoinPolicy::Open, JoinPolicy::InviteOnly]
          .into_iter()
          .map(|option| {
            let key = if option == JoinPolicy::Open { "admin-space-open" } else { "admin-space-invite-only" };
            let is_current = join_policy == option;
            view! {
              <button
                aria-pressed=is_current.to_string()
                on:click=move |_| {
                  if !is_current {
                    change(AdminSpaceChanges {
                      maybe_join_policy: Some(option.clone()),
                      ..AdminSpaceChanges::default()
                    });
                  }
                }
              >
                {move || i18n.t(key)}
              </button>
            }
          })
          .collect_view()}
      </div>
      <p class="muted-text small">{move || i18n.t("admin-space-members-hint")}</p>
      <div class="detail-actions">
        <button
          class="btn"
          on:click=move |_| change(AdminSpaceChanges {
            maybe_is_archived: Some(!is_archived),
            ..AdminSpaceChanges::default()
          })
        >
          {move || i18n.t(if is_archived { "admin-space-restore" } else { "admin-archive" })}
        </button>
      </div>
    }
    .into_any()
  }
}
