use leptos::prelude::*;

use teamcraft_proto::{ChannelModeration, HistoryPolicy, Permission};

use crate::admin::{
  AdminChannel, AdminChannelFilter, AdminChannelQuery, AdminLoad, SelfJoinOutcome, format_bytes,
};
use crate::admission::{Admission, admission};
use crate::channel_access::ChannelAccess;
use crate::clock::{format_relative, now_ms};
use crate::components::admin_console::{ListMore, SectionHeader};
use crate::components::admin_spaces::{LoadNotice, SpacesPanel};
use crate::components::channel_view::UnconfirmedRemovalActions;
use crate::components::icons::{Icon, IconName};
use crate::components::pending_button::PendingButton;
use crate::components::sliding_indicator::SlidingIndicator;
use crate::components::workspace_dialogs::DeleteChannelSection;
use crate::feedback::PendingAction;
use crate::i18n::{I18n, use_i18n};
use crate::model::{ConversationId, ConversationKind, UserId};
use crate::reactive::use_debounced;
use crate::state::{AppState, use_app_state};

/// The wait after the last key before the search asks the server.
const SEARCH_DEBOUNCE_MS: u64 = 250;

const CHANNEL_FILTERS: [AdminChannelFilter; 5] = [
  AdminChannelFilter::All,
  AdminChannelFilter::Private,
  AdminChannelFilter::WithGuests,
  AdminChannelFilter::Inactive,
  AdminChannelFilter::Orphaned,
];

fn filter_slug(filter: AdminChannelFilter) -> &'static str {
  match filter {
    AdminChannelFilter::All => "all",
    AdminChannelFilter::Private => "private",
    AdminChannelFilter::WithGuests => "guests",
    AdminChannelFilter::Inactive => "inactive",
    AdminChannelFilter::Orphaned => "orphaned",
  }
}

/// What the section lists: spaces (with their admin actions) or channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListTab {
  Channels,
  Spaces,
}

impl ListTab {
  const ALL: [ListTab; 2] = [Self::Channels, Self::Spaces];

  fn slug(self) -> &'static str {
    match self {
      Self::Channels => "channels",
      Self::Spaces => "spaces",
    }
  }
}

/// Has the console read the channels and accounts the `(channel, user)` pairs name, wherever they
/// sit in the lists, so the notices show names and the deactivated marks (issue #1433).
fn resolve_references(state: AppState, pairs: Memo<Vec<(ConversationId, UserId)>>) {
  Effect::new(move |_| {
    let pairs = pairs.get();
    if pairs.is_empty() {
      return;
    }
    let channels = pairs.iter().map(|(channel, _)| *channel).collect();
    let users = pairs.iter().map(|(_, user)| *user).collect();
    state.write(|backend| backend.admin_resolve_references(channels, users));
  });
}

/// This admin's removals from channels it isn't in that can't be confirmed (e2ee.md §4.6): the
/// admin removes again or keeps the person; the client never settles it on the server's word.
#[component]
fn UnconfirmedRemovals(channels: Memo<Vec<AdminChannel>>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let removals = Memo::new(move |_| state.read(|backend| backend.unconfirmed_removals()));
  resolve_references(state, removals);
  move || {
    removals
      .get()
      .into_iter()
      .map(|(conversation, user)| {
        let channel = channels
          .get()
          .into_iter()
          .find(|channel| channel.id == conversation)
          .map(|channel| channel.name)
          .unwrap_or_default();
        let name = state
          .read(|backend| backend.user(user))
          .map(|user| user.display_name)
          .filter(|name| !name.is_empty())
          .unwrap_or_else(|| i18n.t("left-in-group-unknown"));
        let text = i18n.t_with("removal-unconfirmed", &[("name", &name), ("channel", &channel)]);
        view! {
          <div class="key-banner" role="alert">
            <Icon name=IconName::Lock />
            <div class="key-banner-text">
              <strong translate="no">{text}</strong>
            </div>
            <UnconfirmedRemovalActions conversation=conversation user=user />
          </div>
        }
      })
      .collect_view()
  }
}

/// Users still in an encryption group of this admin's device whom the server no longer lists,
/// confirmed by fresh member lists (#661), as one compact list. No client removes them on the
/// server's word; the admin decides here, and this device signs and commits the removal (#734).
/// Members never see this.
#[component]
fn GroupLeftovers(channels: Memo<Vec<AdminChannel>>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let leftovers = Memo::new(move |_| state.read(|backend| backend.group_leftovers()));
  resolve_references(state, leftovers);
  // The removal is signed by this client; without its keys the list only informs.
  let can_remove = state.read(|backend| backend.can_remove_from_chats());
  let remove_all = PendingAction::new();
  move || {
    let rows = leftovers.get();
    if rows.is_empty() {
      return ().into_any();
    }
    let has_several = rows.len() > 1;
    view! {
      <section class="key-banner leftover-list" role="status" aria-labelledby="leftover-title">
        <Icon name=IconName::Lock />
        <div class="key-banner-text">
          <strong id="leftover-title">{move || i18n.t("admin-left-in-group-title")}</strong>
          <span class="muted-text small">{move || i18n.t("admin-left-in-group-hint")}</span>
          <ul class="leftover-rows">
            {rows
              .into_iter()
              .map(|(conversation, user)| view! { <LeftoverRow conversation=conversation user=user channels=channels can_remove=can_remove /> })
              .collect_view()}
          </ul>
        </div>
        {(can_remove && has_several)
          .then(|| {
            view! {
              <PendingButton
                pending=remove_all
                class="btn btn-small"
                on_click=move |_| {
                  remove_all.run(|| {
                    for (conversation, user) in leftovers.get_untracked() {
                      state.write(|backend| backend.remove_group_leftover(conversation, user));
                    }
                  });
                }
              >
                {move || i18n.t("admin-left-in-group-remove-all")}
              </PendingButton>
            }
          })}
      </section>
    }
    .into_any()
  }
}

#[component]
fn LeftoverRow(
  conversation: ConversationId,
  user: UserId,
  channels: Memo<Vec<AdminChannel>>,
  can_remove: bool,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let pending = PendingAction::new();
  let text = move || {
    let name = leftover_name(state, i18n, user);
    let place = conversation_label(state, i18n, conversation, user, &channels.get());
    i18n.t_with(
      "admin-left-in-group-row",
      &[("name", &name), ("conversation", &place)],
    )
  };
  view! {
    <li class="leftover-row">
      <span class="leftover-text" translate="no">{text}</span>
      {can_remove
        .then(|| {
          view! {
            <PendingButton
              pending=pending
              class="btn btn-small"
              on_click=move |_| pending.run(|| state.write(|backend| backend.remove_group_leftover(conversation, user)))
            >
              {move || i18n.t("admin-left-in-group-remove")}
            </PendingButton>
          }
        })}
    </li>
  }
}

/// The leftover account's current name: the console's user list has it from the server (the
/// group may know the person under an older name), marked when the account is deactivated.
fn leftover_name(state: AppState, i18n: I18n, user: UserId) -> String {
  let maybe_account = state.read(|backend| {
    backend
      .admin_known_users()
      .into_iter()
      .find(|entry| entry.user.id == user)
  });
  if let Some(account) = maybe_account {
    if account.user.is_deleted {
      return i18n.t("user-deleted-name");
    }
    if !account.is_active {
      return i18n.t_with(
        "admin-account-deactivated-name",
        &[("name", &account.user.display_name)],
      );
    }
    return account.user.display_name;
  }
  state
    .read(|backend| backend.user(user))
    .map(|known| known.display_name)
    .filter(|name| !name.is_empty())
    .unwrap_or_else(|| i18n.t("left-in-group-unknown"))
}

/// Where the leftover is: `#channel`, a direct message with the other people, or a deleted
/// conversation the console no longer knows.
fn conversation_label(
  state: AppState,
  i18n: I18n,
  conversation: ConversationId,
  leftover: UserId,
  channels: &[AdminChannel],
) -> String {
  let me = state.read(|backend| backend.me().id);
  match state.read(|backend| backend.conversation(conversation)) {
    Some(known) if known.kind.is_direct_like() => {
      let names: Vec<String> = known
        .member_ids
        .iter()
        .filter(|member| **member != me && **member != leftover)
        .filter_map(|member| state.read(|backend| backend.user(*member)))
        .map(|member| member.display_name)
        .filter(|name| !name.is_empty())
        .collect();
      if names.is_empty() {
        i18n.t("admin-conversation-direct-unnamed")
      } else {
        i18n.t_with("admin-conversation-direct", &[("names", &names.join(", "))])
      }
    },
    Some(known) if !known.name.is_empty() => {
      i18n.t_with("admin-conversation-channel", &[("name", &known.name)])
    },
    _ => channels
      .iter()
      .find(|channel| channel.id == conversation && !channel.name.is_empty())
      .map_or_else(
        || i18n.t("admin-conversation-deleted"),
        |channel| i18n.t_with("admin-conversation-channel", &[("name", &channel.name)]),
      ),
  }
}

/// All spaces and channels including private ones, with metadata only.
#[component]
pub fn ChannelsSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  // The list is the server's: it filters and searches, and the next pages load on request
  // (issue #1241). The page comes back to what it was asked last.
  let asked = state.read(|backend| backend.admin_channel_query());
  let query = RwSignal::new(asked.text);
  let filter = RwSignal::new(asked.filter);
  let debounced = use_debounced(query, SEARCH_DEBOUNCE_MS);
  Effect::new(move |_| {
    let asked = AdminChannelQuery {
      text: debounced.get(),
      filter: filter.get(),
    };
    state.write(|backend| backend.admin_set_channel_query(asked));
  });
  let selected = RwSignal::new(None::<ConversationId>);
  let tab = RwSignal::new(ListTab::Channels);
  let now = now_ms();

  let load = Memo::new(move |_| state.read(|backend| backend.admin_load()));
  let channels = Memo::new(move |_| state.read(|backend| backend.admin_channels()));
  let visible = channels;
  // The notices name channels that may sit on a page that isn't loaded.
  let referenced = Memo::new(move |_| state.read(|backend| backend.admin_referenced_channels()));
  let named_channels = Memo::new(move |_| {
    let mut named = channels.get();
    named.extend(referenced.get());
    named
  });
  let selected_channel = Memo::new(move |_| {
    selected
      .get()
      .and_then(|id| channels.get().into_iter().find(|channel| channel.id == id))
  });
  // The detail is built again for another channel only: a refresh with new counts updates it in
  // place, so the moderation form and a join result stay and nothing flickers (issue #687).
  let detail_channel = Memo::new(move |_| {
    selected_channel.with(|maybe_channel| maybe_channel.as_ref().map(|channel| channel.id))
  });

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-channels-title"))
      subtitle=Signal::derive(move || i18n.t("admin-channels-subtitle"))
    />
    <UnconfirmedRemovals channels=named_channels />
    <GroupLeftovers channels=named_channels />
    <div class="admin-toolbar">
      <label class="visually-hidden" for="admin-channel-search">{move || i18n.t("admin-search")}</label>
      <input
        id="admin-channel-search"
        class="input"
        type="search"
        name="q"
        autocomplete="off"
        spellcheck="false"
        placeholder=move || i18n.t("admin-search-placeholder")
        prop:value=move || query.get()
        on:input=move |ev| query.set(event_target_value(&ev))
      />
      <div class="segmented" role="group" aria-label=move || i18n.t("admin-list-tab")>
        <SlidingIndicator />
        {ListTab::ALL
          .into_iter()
          .map(|option| {
            view! {
              <button aria-pressed=move || (tab.get() == option).to_string() on:click=move |_| tab.set(option)>
                {move || i18n.t(&format!("admin-tab-{}", option.slug()))}
              </button>
            }
          })
          .collect_view()}
      </div>
      <Show when=move || tab.get() == ListTab::Channels fallback=|| ()>
      <div class="segmented" role="group" aria-label=move || i18n.t("admin-filter")>
        <SlidingIndicator />
        {CHANNEL_FILTERS
          .into_iter()
          .map(|option| {
            view! {
              <button aria-pressed=move || (filter.get() == option).to_string() on:click=move |_| filter.set(option)>
                {move || i18n.t(&format!("admin-filter-{}", filter_slug(option)))}
              </button>
            }
          })
          .collect_view()}
      </div>
      </Show>
    </div>
    <Show when=move || tab.get() == ListTab::Spaces fallback=|| ()>
      <SpacesPanel query=query />
    </Show>
    <Show when=move || tab.get() == ListTab::Channels fallback=|| ()>
    <div class="admin-split">
      <div class="table-wrap">
        <Show when=move || load.get() == AdminLoad::Loaded fallback=move || view! { <LoadNotice load=load /> }>
        <table class="data-table">
          <caption class="visually-hidden">{move || i18n.t("admin-channels-title")}</caption>
          <thead>
            <tr>
              <th scope="col">{move || i18n.t("admin-col-channel")}</th>
              <th scope="col">{move || i18n.t("admin-col-space")}</th>
              <th scope="col" class="num">{move || i18n.t("admin-col-members")}</th>
              <th scope="col">{move || i18n.t("admin-col-owner")}</th>
              <th scope="col">{move || i18n.t("admin-col-activity")}</th>
              <th scope="col" class="num">{move || i18n.t("admin-col-storage")}</th>
            </tr>
          </thead>
          <tbody>
            <For each=move || visible.get() key=|channel| channel.id let:channel>
              <ChannelRow channel=channel selected=selected now=now />
            </For>
          </tbody>
        </table>
        <Show when=move || visible.get().is_empty() fallback=|| ()>
          <p class="empty-state">{move || i18n.t("admin-empty")}</p>
        </Show>
        <ListMore
          has_more=Signal::derive(move || state.read(|backend| backend.admin_channels_have_more()))
          on_more=Callback::new(move |()| state.write(|backend| backend.admin_load_more_channels()))
        />
        </Show>
      </div>
      <aside class="detail-pane" aria-label=move || i18n.t("admin-detail-label")>
        {move || match detail_channel.get().and_then(|_| selected_channel.get_untracked()) {
          Some(channel) => view! { <ChannelDetail channel=channel latest=selected_channel /> }.into_any(),
          None => view! { <p class="empty-state">{move || i18n.t("admin-channel-select")}</p> }.into_any(),
        }}
      </aside>
    </div>
    </Show>
  }
}

#[component]
fn ChannelRow(channel: AdminChannel, selected: RwSignal<Option<ConversationId>>, now: i64) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = channel.id;
  let owner = channel
    .maybe_owner
    .and_then(|owner| state.read(|backend| backend.user(owner)))
    .map(|user| user.display_name);
  let is_private = channel.kind == ConversationKind::Private;
  let icon = match channel.kind {
    ConversationKind::Private => IconName::Lock,
    ConversationKind::Integration => IconName::Terminal,
    _ => IconName::Hash,
  };
  let activity = move || format_relative(channel.last_activity, now, i18n.locale().tag());
  let is_selected = move || selected.get() == Some(id);

  view! {
    <tr class:selected=is_selected>
      <th scope="row">
        <button class="row-button" aria-pressed=move || is_selected().to_string() on:click=move |_| selected.set(Some(id))>
          <Icon name=icon />
          <span class="row-name" translate="no">{channel.name.clone()}</span>
          {is_private.then(|| view! { <span class="badge">{move || i18n.t("admin-kind-private")}</span> })}
          {channel.maybe_owner.is_none().then(|| view! { <span class="badge badge-warn">{move || i18n.t("admin-orphaned")}</span> })}
        </button>
      </th>
      <td translate="no">{channel.space_name.clone()}</td>
      <td class="num">
        {channel.member_count}
        {(channel.guest_count > 0).then(|| view! { <span class="muted-text">{format!(" (+{})", channel.guest_count)}</span> })}
      </td>
      <td translate="no">{owner.unwrap_or_else(|| "–".to_owned())}</td>
      <td>{activity}</td>
      <td class="num">{format_bytes(channel.storage_bytes)}</td>
    </tr>
  }
}

#[component]
fn ChannelDetail(channel: AdminChannel, latest: Memo<Option<AdminChannel>>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let outcome = RwSignal::new(None::<SelfJoinOutcome>);
  let id = channel.id;
  let is_private = channel.kind == ConversationKind::Private;
  let delete_access = ChannelAccess::new(state, id);
  let is_confirming_archive = RwSignal::new(false);
  // The channel as the last refresh has it; the one it was opened with once it is gone.
  let current = Memo::new(move |previous: Option<&AdminChannel>| {
    latest
      .get()
      .or_else(|| previous.cloned())
      .unwrap_or_else(|| channel.clone())
  });
  let maybe_owner = Memo::new(move |_| current.with(|channel| channel.maybe_owner));
  let owner_name = Memo::new(move |_| {
    maybe_owner
      .get()
      .and_then(|owner| state.read(|backend| backend.user(owner)))
      .map(|user| user.display_name)
  });
  let retention = move || {
    let retention = current.with(|channel| channel.retention);
    let mut parts = Vec::new();
    if let Some(days) = retention.maybe_days {
      parts.push(i18n.t_count("admin-retention-days", days as usize));
    }
    if let Some(messages) = retention.maybe_max_messages {
      parts.push(i18n.t_count("admin-retention-messages", messages as usize));
    }
    if parts.is_empty() {
      i18n.t("admin-retention-forever")
    } else {
      parts.join(" / ")
    }
  };
  let history = move || {
    current.with(|channel| match &channel.history {
      HistoryPolicy::FromJoin => "from_join".to_owned(),
      HistoryPolicy::Full => "full".to_owned(),
      HistoryPolicy::Unknown(other) => other.clone(),
    })
  };

  view! {
    <h3 translate="no">{move || current.with(|channel| format!("# {}", channel.name))}</h3>
    <p class="muted-text" translate="no">{move || current.with(|channel| channel.space_name.clone())}</p>
    <dl class="meta-list">
      <dt>{move || i18n.t("admin-col-members")}</dt>
      <dd>{move || i18n.t_count("members-count", current.with(|channel| channel.member_count) as usize)}</dd>
      <dt>{move || i18n.t("admin-guests")}</dt>
      <dd>{move || current.with(|channel| channel.guest_count)}</dd>
      <dt>{move || i18n.t("admin-col-owner")}</dt>
      <dd translate="no">{move || owner_name.get().unwrap_or_else(|| "–".to_owned())}</dd>
      <dt>{move || i18n.t("admin-messages")}</dt>
      <dd>{move || current.with(|channel| channel.message_count.to_string())}</dd>
      <dt>{move || i18n.t("admin-col-storage")}</dt>
      <dd>{move || current.with(|channel| format_bytes(channel.storage_bytes))}</dd>
      <dt>{move || i18n.t("admin-history")}</dt>
      <dd>{move || {
        let history = history();
        let key = format!("admin-history-{history}");
        let text = i18n.t(&key);
        if text == key { history } else { text }
      }}</dd>
      <dt>{move || i18n.t("admin-retention")}</dt>
      <dd>{retention}</dd>
    </dl>
    <p class="no-content-note">
      <Icon name=IconName::EyeOff />
      <span>{move || i18n.t("admin-no-content")}</span>
    </p>
    <div class="detail-actions">
      {move || {
        // Only for an admin who isn't in the channel yet, or whose device no member let in within
        // the retry interval (a refused self-join asks again from here, issues #907, #908). While
        // the request is being taken care of there is nothing to press.
        let label = match admin_join_state(state, id) {
          AdminJoinState::NotMember => "admin-join",
          AdminJoinState::Waiting(admission) if admission.can_ask_again() => "admin-join-again",
          AdminJoinState::Waiting(_) | AdminJoinState::Joined => return None,
        };
        is_private.then(|| {
          view! {
            <button class="btn" on:click=move |_| outcome.set(Some(state.write(|backend| backend.admin_join_channel(id))))>
              {move || i18n.t(label)}
            </button>
          }
        })
      }}
      <Show when=move || delete_access.can(&Permission::ManageChannelProperties) fallback=|| ()>
      <button
        type="button"
        class="btn"
        on:click=move |_| {
          if current.with(|channel| channel.is_archived) {
            state.write(|backend| backend.admin_set_channel_archived(id, false));
          } else {
            is_confirming_archive.set(true);
          }
        }
      >
        {move || i18n.t(if current.with(|channel| channel.is_archived) { "admin-channel-restore" } else { "admin-archive" })}
      </button>
      </Show>
    </div>
    <Show when=move || is_confirming_archive.get() fallback=|| ()>
      <div class="detail-actions" role="alert">
        <p>{move || i18n.t("admin-channel-archive-confirm")}</p>
        <button
          type="button"
          class="btn btn-danger"
          on:click=move |_| {
            state.write(|backend| backend.admin_set_channel_archived(id, true));
            is_confirming_archive.set(false);
          }
        >
          {move || i18n.t("admin-archive")}
        </button>
        <button type="button" class="btn" on:click=move |_| is_confirming_archive.set(false)>{move || i18n.t("cancel")}</button>
      </div>
    </Show>
    {move || maybe_owner.get().is_none().then(|| view! { <OwnerAssign channel=id /> })}
    <ModerationEditor channel=id />
    <Show when=move || delete_access.can(&Permission::DeleteChannel) fallback=|| ()>
      <DeleteChannelSection conversation=id on_deleted=Callback::new(move |()| state.write(|backend| backend.admin_refresh())) />
    </Show>
    <p class="status-line" role="status">
      {move || is_private.then(|| join_status_text(i18n, admin_join_state(state, id), outcome.get())).flatten()}
    </p>
  }
}

/// Where the admin viewing the console stands in a channel, live: membership, presence and the
/// self-add's progress come from gateway events (issue #908).
#[derive(Clone, Copy, PartialEq, Eq)]
enum AdminJoinState {
  NotMember,
  /// A member on the server whose device asked to be let into the encryption group and waits.
  Waiting(Admission),
  Joined,
}

fn admin_join_state(state: AppState, channel: ConversationId) -> AdminJoinState {
  state.read(|backend| {
    if backend.conversation(channel).is_none() {
      return AdminJoinState::NotMember;
    }
    admission(backend, channel).map_or(AdminJoinState::Joined, AdminJoinState::Waiting)
  })
}

/// What the channel details say about the admin's join: what happens now, not that somebody has
/// to decide, except under `owner_approval` where the owners really do (admin.md §2.4).
fn join_status_text(
  i18n: I18n,
  join: AdminJoinState,
  maybe_outcome: Option<SelfJoinOutcome>,
) -> Option<String> {
  match (join, maybe_outcome) {
    (AdminJoinState::Waiting(admission), _) => Some(i18n.t(match admission {
      Admission::Adding => "admin-join-adding",
      Admission::NoMemberOnline => "admin-join-no-member-online",
      Admission::NotYetAdded => "admin-join-not-yet",
    })),
    (AdminJoinState::Joined, _) => Some(i18n.t("admin-join-member")),
    // The server hasn't listed the membership yet: the request is on its way.
    (AdminJoinState::NotMember, Some(SelfJoinOutcome::Adding)) => Some(i18n.t("admin-join-adding")),
    (AdminJoinState::NotMember, Some(SelfJoinOutcome::ApprovalRequested)) => {
      Some(i18n.t("admin-join-approval"))
    },
    (AdminJoinState::NotMember, Some(SelfJoinOutcome::Disabled)) => Some(i18n.t("admin-join-disabled")),
    (AdminJoinState::NotMember, None) => None,
  }
}

/// Picks a new owner for a channel without one, from its members (admin.md §2.5).
#[component]
fn OwnerAssign(channel: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let candidates = Memo::new(move |_| state.read(|backend| backend.channel_member_candidates(channel)));
  let chosen = RwSignal::new(None::<UserId>);
  let assigned = RwSignal::new(false);

  view! {
    <section class="owner-assign" aria-labelledby="owner-assign-title">
      <h4 id="owner-assign-title" class="detail-heading">{move || i18n.t("admin-assign-owner")}</h4>
      <p class="muted-text small">{move || i18n.t("admin-assign-owner-hint")}</p>
      <div class="field inline">
        <label class="visually-hidden" for="owner-candidate">{move || i18n.t("admin-owner-candidate")}</label>
        <select
          id="owner-candidate"
          class="input"
          on:change=move |ev| {
            let value = event_target_value(&ev);
            chosen.set(candidates.get().into_iter().find(|user| user.to_string() == value));
          }
        >
          <option value="">{move || i18n.t("admin-owner-choose")}</option>
          <For each=move || candidates.get() key=|user: &UserId| *user let:user>
            {
              let name = state.read(|backend| backend.user(user)).map(|found| found.display_name).unwrap_or_default();
              view! { <option value=user.to_string() translate="no">{name}</option> }
            }
          </For>
        </select>
        <button
          class="btn btn-primary"
          disabled=move || chosen.get().is_none()
          on:click=move |_| assign_owner(state, channel, chosen.get_untracked(), assigned)
        >
          {move || i18n.t("admin-assign")}
        </button>
      </div>
      <p class="status-line" role="status">{move || assigned.get().then(|| i18n.t("admin-owner-assigned"))}</p>
    </section>
  }
}

fn assign_owner(
  state: crate::state::AppState,
  channel: ConversationId,
  maybe_user: Option<UserId>,
  done: RwSignal<bool>,
) {
  if let Some(user) = maybe_user {
    state.write(|backend| backend.assign_channel_owner(channel, user));
    done.set(true);
  }
}

/// What members and guests may not do in this channel. A channel can only take permissions away.
#[component]
fn ModerationEditor(channel: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let moderation = Memo::new(move |_| state.read(|backend| backend.channel_moderation(channel)));

  view! {
    <section class="moderation" aria-labelledby="moderation-title">
      <h4 id="moderation-title" class="detail-heading">{move || i18n.t("admin-moderation")}</h4>
      <p class="muted-text small">{move || i18n.t("admin-moderation-hint")}</p>
      <table class="data-table moderation-table">
        <caption class="visually-hidden">{move || i18n.t("admin-moderation")}</caption>
        <thead>
          <tr>
            <th scope="col">{move || i18n.t("permissions-permission")}</th>
            <th scope="col">{move || i18n.t("permissions-role-member")}</th>
            <th scope="col">{move || i18n.t("permissions-role-guest")}</th>
          </tr>
        </thead>
        <tbody>
          {MODERATABLE
            .into_iter()
            .map(|permission| {
              let key = format!("permission-{}", permission.as_str());
              let (member_perm, guest_perm) = (permission.clone(), permission.clone());
              let (member_set, guest_set) = (permission.clone(), permission);
              view! {
                <tr>
                  <th scope="row">{move || i18n.t(&key)}</th>
                  <td>
                    <label class="checkbox">
                      <input
                        type="checkbox"
                        prop:checked=move || !moderation.get().member_denied.contains(&member_perm)
                        on:change=move |ev| set_allowed(state, channel, moderation.get_untracked(), Who::Members, member_set.clone(), event_target_checked(&ev))
                      />
                      <span class="visually-hidden">{move || i18n.t("admin-moderation-allowed")}</span>
                    </label>
                  </td>
                  <td>
                    <label class="checkbox">
                      <input
                        type="checkbox"
                        prop:checked=move || !moderation.get().guest_denied.contains(&guest_perm)
                        on:change=move |ev| set_allowed(state, channel, moderation.get_untracked(), Who::Guests, guest_set.clone(), event_target_checked(&ev))
                      />
                      <span class="visually-hidden">{move || i18n.t("admin-moderation-allowed")}</span>
                    </label>
                  </td>
                </tr>
              }
            })
            .collect_view()}
        </tbody>
      </table>
    </section>
  }
}

/// The permissions a channel may take away (`Permission::is_channel_moderatable`).
const MODERATABLE: [Permission; 6] = [
  Permission::CreatePost,
  Permission::AddReaction,
  Permission::UseChannelMentions,
  Permission::UseGroupMentions,
  Permission::ManageBookmarks,
  Permission::ManageMembers,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Who {
  Members,
  Guests,
}

fn set_allowed(
  state: crate::state::AppState,
  channel: ConversationId,
  current: ChannelModeration,
  who: Who,
  permission: Permission,
  is_allowed: bool,
) {
  let next = with_allowed(current, who, permission, is_allowed);
  state.write(|backend| backend.set_channel_moderation(channel, next));
}

fn with_allowed(
  mut moderation: ChannelModeration,
  who: Who,
  permission: Permission,
  is_allowed: bool,
) -> ChannelModeration {
  let denied = match who {
    Who::Members => &mut moderation.member_denied,
    Who::Guests => &mut moderation.guest_denied,
  };
  if is_allowed {
    denied.remove(&permission);
  } else {
    denied.insert(permission);
  }
  moderation
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn toggles_add_and_remove_denials_per_audience() {
    let denied = with_allowed(
      ChannelModeration::default(),
      Who::Guests,
      Permission::CreatePost,
      false,
    );
    assert!(denied.guest_denied.contains(&Permission::CreatePost));
    assert!(denied.member_denied.is_empty());
    let allowed = with_allowed(denied, Who::Guests, Permission::CreatePost, true);
    assert!(allowed.guest_denied.is_empty());
  }

  #[test]
  fn only_moderatable_permissions_are_offered() {
    assert!(MODERATABLE.iter().all(Permission::is_channel_moderatable));
  }
}
