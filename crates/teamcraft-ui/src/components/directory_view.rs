//! The directory: people, channels, external people, user groups and open invitations (#298). Inside
//! a space each list shows that space, as far as the user may see it (#954); elsewhere everything.

use leptos::prelude::*;
use teamcraft_proto::{
  CreateUserGroupRequest, ErrorCode, ListUserGroupsResponse, Permission, UserGroupDetailResponse,
  UserGroupId, UserGroupResponse,
};

use crate::auth::ApiMethod;
use crate::channel_access::{can_in_workspace_now, is_guest};
use crate::components::compact_back::CompactBack;
use crate::components::directory_channels::ChannelDirectory;
use crate::components::directory_invitations::InvitationList;
use crate::components::directory_people::{PeopleGrid, PeopleTab};
use crate::components::empty_state::EmptyState;
use crate::components::icons::{Icon, IconName};
use crate::components::page::ScopedSpaceName;
use crate::components::skeleton::ListSkeleton;
use crate::components::sliding_indicator::SlidingIndicator;
use crate::components::tab_list::{on_tab_keys, roving_tabindex};
use crate::feedback::report_failure;
use crate::form_errors::{FieldError, FieldErrors, FormErrors};
use crate::i18n::use_i18n;
use crate::model::{Space, SpaceId, UserId};
use crate::reactive::{SetIfAlive, UpdateIfAlive};
use crate::rest::RestClient;
use crate::session::use_session;
use crate::state::{Overlay, use_app_state};

/// Placeholder rows while the user groups load.
const GROUP_SKELETON_ROWS: usize = 3;

/// Element ID of the panel the directory tabs control.
const DIRECTORY_PANEL_ID: &str = "directory-panel";
const GROUP_FORM_ID: &str = "group-create";
const GROUP_HANDLE_ID: &str = "group-handle";
const GROUP_NAME_ID: &str = "group-name";
/// The length of a group handle the server accepts.
const GROUP_HANDLE_CHARS: std::ops::RangeInclusive<usize> = 2..=32;
/// Server refusals of a new group that are about one field: a taken handle. `invalid_request` names
/// no field (a reserved handle or a name over the instance's limit), so it stays a toast.
const GROUP_SERVER_FIELDS: [(ErrorCode, &str); 1] = [(ErrorCode::Conflict, GROUP_HANDLE_ID)];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum DirectoryTab {
  People,
  Channels,
  External,
  Groups,
  Invitations,
}

impl DirectoryTab {
  /// The tabs this user is offered: guests have no directory beyond who they share a conversation
  /// with (docs/admin.md), and Invitations is for who can invite.
  fn offered(am_guest: bool, can_invite: bool) -> Vec<Self> {
    if am_guest {
      return vec![Self::People, Self::Groups];
    }
    let mut tabs = vec![Self::People, Self::Channels, Self::External, Self::Groups];
    if can_invite {
      tabs.push(Self::Invitations);
    }
    tabs
  }

  fn label_key(self) -> &'static str {
    match self {
      Self::People => "directory-people",
      Self::Channels => "directory-channels",
      Self::External => "directory-external",
      Self::Groups => "directory-groups",
      Self::Invitations => "directory-invitations",
    }
  }

  fn tab_id(self) -> String {
    format!("{}-tab", self.label_key())
  }
}

#[component]
pub fn DirectoryView() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let tab = RwSignal::new(DirectoryTab::People);
  let offered =
    Memo::new(move |_| DirectoryTab::offered(is_guest(state), !inviting_spaces(state).is_empty()));
  let on_keydown =
    move |ev: web_sys::KeyboardEvent| on_tab_keys(&ev, &offered.get_untracked(), tab, DirectoryTab::tab_id);
  view! {
    <section class="directory page-view" aria-label=move || i18n.t("directory-title")>
      <header class="panel-header">
        <CompactBack />
        <div class="panel-title">
          <h1>{move || i18n.t("directory-title")}</h1>
          <ScopedSpaceName />
        </div>
        <div class="segmented" role="tablist" aria-label=move || i18n.t("directory-title") on:keydown=on_keydown>
          <SlidingIndicator />
          <For each=move || offered.get() key=|value| *value let:value>
            <button
              id=value.tab_id()
              role="tab"
              aria-selected=move || (tab.get() == value).to_string()
              aria-controls=DIRECTORY_PANEL_ID
              tabindex=move || roving_tabindex(tab.get() == value)
              on:click=move |_| tab.set(value)
            >
              {move || i18n.t(value.label_key())}
            </button>
          </For>
        </div>
      </header>
      <div class="panel-scroll directory-body" role="tabpanel" id=DIRECTORY_PANEL_ID aria-labelledby=move || tab.get().tab_id()>
        {move || match tab.get() {
          DirectoryTab::People => view! { <PeopleGrid tab=PeopleTab::People /> }.into_any(),
          DirectoryTab::Channels => view! { <ChannelDirectory /> }.into_any(),
          DirectoryTab::External => view! { <PeopleGrid tab=PeopleTab::External /> }.into_any(),
          DirectoryTab::Groups => view! { <UserGroups /> }.into_any(),
          DirectoryTab::Invitations => view! { <InvitationList /> }.into_any(),
        }}
      </div>
    </section>
  }
}

/// "Invite people": for the space on screen, the only space the user may invite to, or a choice of
/// them. Only offered where the user may invite anybody; the dialog then offers the roles.
#[component]
pub fn InvitePeopleButton() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let spaces = Memo::new(move |_| {
    let mut spaces = inviting_spaces(state);
    if let Some(scoped) = state.maybe_scoped_space() {
      spaces.retain(|space| space.id == scoped);
    }
    spaces
  });
  let open = move |space: SpaceId| state.overlay.set(Overlay::InvitePeople(space));
  move || {
    let spaces = spaces.get();
    match spaces.as_slice() {
      [] => ().into_any(),
      [only] => {
        let id = only.id;
        view! {
          <button type="button" class="btn btn-primary directory-invite" on:click=move |_| open(id)>
            <Icon name=IconName::UserPlus />
            {move || i18n.t("invite-title")}
          </button>
        }
        .into_any()
      },
      many => {
        let options = many.to_vec();
        view! {
          <label class="visually-hidden" for="directory-invite-space">{move || i18n.t("invite-title")}</label>
          <select
            id="directory-invite-space"
            class="input page-select directory-invite-choice"
            prop:value=""
            on:change=move |ev| {
              let chosen = event_target_value(&ev);
              if let Ok(space) = chosen.parse::<SpaceId>() {
                open(space);
              }
              if let Some(select) = ev.target().and_then(|target| wasm_bindgen::JsCast::dyn_into::<web_sys::HtmlSelectElement>(target).ok()) {
                select.set_value("");
              }
            }
          >
            <option value="">{move || i18n.t("invite-title")}</option>
            {options.into_iter().map(|space| view! { <option value=space.id.to_string() translate="no">{space.name.clone()}</option> }).collect_view()}
          </select>
        }
        .into_any()
      },
    }
  }
}

/// The spaces the user may invite somebody to.
fn inviting_spaces(state: crate::state::AppState) -> Vec<Space> {
  state
    .read(|backend| backend.spaces())
    .into_iter()
    .filter(|space| space.can_add_members || space.can_invite_guests)
    .collect()
}

/// A typed group handle as the server stores it: without a leading `@`, lower case.
fn normalized_handle(typed: &str) -> String {
  typed.trim().trim_start_matches('@').to_lowercase()
}

/// Whether a handle has the length and characters the server accepts (it also refuses reserved
/// names).
fn is_group_handle(handle: &str) -> bool {
  GROUP_HANDLE_CHARS.contains(&handle.chars().count())
    && handle
      .chars()
      .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
}

/// User groups: mentionable handles with members. Admins create and edit them.
#[component]
fn UserGroups() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let Some(session) = use_session() else {
    return view! { <p class="empty-state">{move || i18n.t("groups-unavailable")}</p> }.into_any();
  };
  let client = StoredValue::new_local(RestClient::new(session.authenticator()));
  let reload = RwSignal::new(0_u32);
  let groups = RwSignal::new(Vec::<UserGroupResponse>::new());
  let open = RwSignal::new(None::<UserGroupId>);
  let error = RwSignal::new(None::<String>);
  // Until the first answer the list shows placeholders, not "no groups".
  let is_loaded = RwSignal::new(false);
  // The server asks for the workspace permissions `create_user_group` and
  // `manage_user_group_members` (admins and owners in the built-in scheme).
  let can_create = Memo::new(move |_| can_in_workspace_now(state, &Permission::CreateUserGroup));
  let can_edit_members = Memo::new(move |_| can_in_workspace_now(state, &Permission::ManageUserGroupMembers));

  Effect::new(move |_| {
    reload.track();
    leptos::task::spawn_local(async move {
      match client
        .get_value()
        .get::<ListUserGroupsResponse>("/api/v1/user-groups")
        .await
      {
        Ok(list) => groups.set_if_alive(list.groups),
        Err(failure) => error.set_if_alive(Some(failure.to_string())),
      }
      is_loaded.set_if_alive(true);
    });
  });

  let handle = RwSignal::new(String::new());
  let name = RwSignal::new(String::new());
  let errors = FormErrors::new();
  let field_errors = move || {
    let mut found = FieldErrors::new();
    if !is_group_handle(&normalized_handle(&handle.get())) {
      found.push(GROUP_HANDLE_ID, i18n.t("groups-error-handle"));
    }
    if name.with(|name| name.trim().is_empty()) {
      found.push(GROUP_NAME_ID, i18n.t("form-error-name"));
    }
    found
  };
  Effect::new(move |_| errors.follow(field_errors()));
  let create = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    let found = untrack(field_errors);
    if !found.is_empty() {
      errors.refuse(GROUP_FORM_ID, found);
      return;
    }
    let request = CreateUserGroupRequest {
      handle: normalized_handle(&handle.get_untracked()),
      name: name.get_untracked().trim().to_owned(),
      maybe_description: None,
    };
    leptos::task::spawn_local(async move {
      match client
        .get_value()
        .post::<UserGroupResponse>("/api/v1/user-groups", &request)
        .await
      {
        Ok(_) => {
          handle.set_if_alive(String::new());
          name.set_if_alive(String::new());
          error.set_if_alive(None);
          reload.update_if_alive(|value| *value += 1);
        },
        Err(refusal) => {
          let failure = refusal.to_failure();
          // A taken handle goes to the handle field; anything else is a toast.
          let message = i18n.t("groups-error-handle-taken");
          if !errors.refuse_server(GROUP_FORM_ID, &failure, &GROUP_SERVER_FIELDS, message) {
            report_failure(failure);
          }
        },
      }
    });
  };

  view! {
    {move || error.get().map(|message| view! { <p class="field-error" role="alert">{message}</p> })}
    <ul class="group-list page-rows">
      <For each=move || groups.get() key=|group| (group.id, group.member_count, group.name.clone()) let:group>
        {
          let id = group.id;
          view! {
            <li class="group-row">
              <button class="person-main" aria-expanded=move || (open.get() == Some(id)).to_string() on:click=move |_| open.update(|current| *current = if *current == Some(id) { None } else { Some(id) })>
                <span class="person-text">
                  <span class="person-name" translate="no">{group.name.clone()} " (@" {group.handle.clone()} ")"</span>
                  <span class="person-facts">{move || i18n.t_count("groups-members", group.member_count as usize)}</span>
                </span>
                {group.is_synced.then(|| view! { <span class="badge">{move || i18n.t("groups-synced")}</span> })}
              </button>
              <Show when=move || open.get() == Some(id) fallback=|| ()>
                <GroupMembers group=id is_synced=group.is_synced can_edit=can_edit_members client=client reload=reload />
              </Show>
            </li>
          }
        }
      </For>
      <Show when=move || !is_loaded.get() fallback=|| ()>
        <ListSkeleton rows=GROUP_SKELETON_ROWS label=Signal::derive(move || i18n.t("groups-loading")) />
      </Show>
      <Show when=move || is_loaded.get() && groups.get().is_empty() fallback=|| ()>
        <EmptyState icon=IconName::Users title_key="groups-empty-title" body_key="groups-empty" as_item=true />
      </Show>
    </ul>
    <Show when=move || can_create.get() fallback=|| ()>
      <form id=GROUP_FORM_ID class="group-create" on:submit=create>
        <div class="inline-form">
          <div class="group-create-field">
            <label class="visually-hidden" for=GROUP_HANDLE_ID>{move || i18n.t("groups-handle")}</label>
            <input id=GROUP_HANDLE_ID class="input" placeholder=move || i18n.t("groups-handle") aria-required="true"
              aria-invalid=errors.invalid(GROUP_HANDLE_ID) aria-describedby=errors.described(GROUP_HANDLE_ID)
              prop:value=move || handle.get() on:input=move |ev| handle.set(event_target_value(&ev)) />
            <FieldError errors=errors field=GROUP_HANDLE_ID />
          </div>
          <div class="group-create-field">
            <label class="visually-hidden" for=GROUP_NAME_ID>{move || i18n.t("groups-name")}</label>
            <input id=GROUP_NAME_ID class="input" placeholder=move || i18n.t("groups-name") aria-required="true"
              aria-invalid=errors.invalid(GROUP_NAME_ID) aria-describedby=errors.described(GROUP_NAME_ID)
              prop:value=move || name.get() on:input=move |ev| name.set(event_target_value(&ev)) />
            <FieldError errors=errors field=GROUP_NAME_ID />
          </div>
          <button type="submit" class="btn btn-primary">{move || i18n.t("create")}</button>
        </div>
      </form>
    </Show>
  }
  .into_any()
}

#[component]
fn GroupMembers(
  group: UserGroupId,
  is_synced: bool,
  can_edit: Memo<bool>,
  client: StoredValue<RestClient, LocalStorage>,
  reload: RwSignal<u32>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let members = RwSignal::new(Vec::<teamcraft_proto::UserSummary>::new());
  let refresh = RwSignal::new(0_u32);
  let chosen = RwSignal::new(String::new());
  Effect::new(move |_| {
    refresh.track();
    leptos::task::spawn_local(async move {
      let path = format!("/api/v1/user-groups/{group}");
      if let Ok(detail) = client.get_value().get::<UserGroupDetailResponse>(&path).await {
        members.set_if_alive(detail.members);
      }
    });
  });
  let change = move |method: ApiMethod, user: UserId| {
    leptos::task::spawn_local(async move {
      let path = format!("/api/v1/user-groups/{group}/members/{user}");
      if client.get_value().send(method, &path, None).await.is_ok() {
        refresh.update_if_alive(|value| *value += 1);
        reload.update_if_alive(|value| *value += 1);
      }
    });
  };
  let candidates = Memo::new(move |_| {
    let present = members.get();
    state
      .read(|backend| backend.users())
      .into_iter()
      .filter(|user| !user.is_bot && !present.iter().any(|member| member.id == user.id))
      .collect::<Vec<_>>()
  });
  let can_change = move || can_edit.get() && !is_synced;
  view! {
    <ul class="group-members">
      <For each=move || members.get() key=|member| member.id let:member>
        {
          let id = member.id;
          view! {
            <li>
              <span translate="no">{member.display_name.clone()}</span>
              <Show when=can_change fallback=|| ()>
                <button class="btn btn-small" on:click=move |_| change(ApiMethod::Delete, id)>{move || i18n.t("groups-remove")}</button>
              </Show>
            </li>
          }
        }
      </For>
    </ul>
    <Show when=can_change fallback=|| ()>
      <div class="inline-form">
        <label class="visually-hidden" for=format!("group-add-{group}")>{move || i18n.t("groups-add")}</label>
        <select id=format!("group-add-{group}") class="input" on:change=move |ev| chosen.set(event_target_value(&ev))>
          <option value="">{move || i18n.t("channel-pick-person")}</option>
          <For each=move || candidates.get() key=|user| user.id let:user>
            <option value=user.id.to_string() translate="no">{user.display_name.clone()}</option>
          </For>
        </select>
        <button
          class="btn"
          prop:disabled=move || chosen.get().is_empty()
          on:click=move |_| {
            if let Ok(user) = chosen.get_untracked().parse::<UserId>() {
              change(ApiMethod::Put, user);
              chosen.set(String::new());
            }
          }
        >
          {move || i18n.t("groups-add")}
        </button>
      </div>
    </Show>
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::feedback::{ActionFailure, FailureKind};

  #[test]
  fn guests_are_offered_only_what_they_may_see() {
    assert_eq!(
      DirectoryTab::offered(true, true),
      vec![DirectoryTab::People, DirectoryTab::Groups]
    );
  }

  #[test]
  fn invitations_are_offered_to_who_can_invite() {
    assert!(DirectoryTab::offered(false, true).contains(&DirectoryTab::Invitations));
    let without = DirectoryTab::offered(false, false);
    assert!(!without.contains(&DirectoryTab::Invitations));
    assert!(without.contains(&DirectoryTab::Channels) && without.contains(&DirectoryTab::External));
  }
  use crate::form_errors::server_error_field;

  #[test]
  fn a_typed_handle_is_read_like_the_server_reads_it() {
    assert_eq!(normalized_handle(" @Design-Team "), "design-team");
    assert!(is_group_handle("design_team-2"));
    assert!(!is_group_handle("a"));
    assert!(!is_group_handle("design team"));
  }

  #[test]
  fn a_taken_handle_comes_back_at_the_handle_field() {
    let refusal = |code: ErrorCode| ActionFailure::new(FailureKind::Server(code), String::new());
    assert_eq!(
      server_error_field(&refusal(ErrorCode::Conflict), &GROUP_SERVER_FIELDS),
      Some(GROUP_HANDLE_ID)
    );
    assert_eq!(
      server_error_field(&refusal(ErrorCode::InvalidRequest), &GROUP_SERVER_FIELDS),
      None
    );
  }
}
