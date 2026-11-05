//! The Channels tab of the directory (#298): channels across the user's spaces, one page at a time.
//! Joining goes through the same action as the channel browser, so the encryption group is handled
//! as there; private channels an admin could join stay in the browser with its signed-roster notes.

use leptos::prelude::*;
use teamcraft_client_api::list_load::{ListLoad, REQUEST_DEADLINE_MS};
use teamcraft_proto::{ConversationVisibility, DirectoryChannel, DirectoryChannelsResponse};

use crate::components::directory_pager::{Pager, PagerFooter, query_path};
use crate::components::empty_state::EmptyState;
use crate::components::icons::{Icon, IconName};
use crate::components::list_status::{ListPlaceholder, ListStatus};
use crate::components::pending_button::PendingButton;
use crate::feedback::PendingAction;
use crate::i18n::use_i18n;
use crate::reactive::use_debounced;
use crate::state::use_app_state;

const SKELETON_ROWS: usize = 6;
const SEARCH_DEBOUNCE_MS: u64 = 250;

#[component]
pub fn ChannelDirectory() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let text = RwSignal::new(String::new());
  let debounced = use_debounced(text, SEARCH_DEBOUNCE_MS);
  let scope = Memo::new(move |_| state.maybe_scoped_space());
  let path = Signal::derive(move || {
    let space = scope.get().map(|space| space.to_string()).unwrap_or_default();
    query_path(
      "/api/v1/directory/channels",
      &[("query", debounced.get().trim()), ("space_id", &space)],
    )
  });
  let pager = Pager::<DirectoryChannel>::new::<DirectoryChannelsResponse>(path);
  let status = ListStatus::new(
    move || pager.rows.with(Vec::is_empty),
    move || {
      if text.with(|typed| typed.trim() != debounced.get().trim()) {
        ListLoad::Loading
      } else {
        pager.load.get()
      }
    },
    Signal::derive(move || path.get()),
    REQUEST_DEADLINE_MS,
  );
  let on_retry = Callback::new(move |()| {
    status.restart();
    pager.reload();
  });
  let on_more = Callback::new(move |()| pager.load_more::<DirectoryChannelsResponse>());
  view! {
    <div class="page-tools directory-tools">
      <label class="page-search">
        <span class="visually-hidden">{move || i18n.t("directory-channel-filter")}</span>
        <Icon name=IconName::Search />
        <input
          id="directory-channel-filter"
          class="input"
          type="search"
          autocomplete="off"
          placeholder=move || i18n.t("directory-channel-filter")
          prop:value=move || text.get()
          on:input=move |ev| text.set(event_target_value(&ev))
        />
      </label>
    </div>
    <ul class="page-rows">
      <For each=move || pager.rows.get() key=|channel| channel.id let:channel>
        <ChannelRow channel=channel />
      </For>
      <ListPlaceholder
        phase=status.phase
        rows=SKELETON_ROWS
        label=Signal::derive(move || i18n.t("directory-channels-loading"))
        on_retry=on_retry
        as_item=true
      >
        <EmptyState icon=IconName::Hash title_key="directory-channels-empty-title" body_key="directory-channels-empty" as_item=true />
      </ListPlaceholder>
    </ul>
    <PagerFooter pager=pager on_more=on_more />
  }
}

#[component]
fn ChannelRow(channel: DirectoryChannel) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = channel.id;
  let is_private = channel.visibility == ConversationVisibility::Private;
  let joined_on_server = channel.is_member;
  // The answer is from when the page loaded; a join here shows as soon as the conversation arrives.
  let is_member =
    Memo::new(move |_| joined_on_server || state.read(|backend| backend.conversation(id).is_some()));
  let pending = PendingAction::new();
  let member_count = channel.member_count as usize;
  let detail = channel.maybe_topic.clone().unwrap_or_default();
  view! {
    <li class="page-row directory-channel">
      <span class="page-portrait-group">
        <Icon name=if is_private { IconName::Lock } else { IconName::Hash } />
      </span>
      <span class="page-row-text directory-row-text">
        <span class="page-row-title" translate="no">{channel.name.clone()}</span>
        <span class="page-row-detail">
          <span translate="no">{channel.space_name.clone()}</span>
          " · "
          {move || i18n.t_count("directory-channel-members", member_count)}
        </span>
        {(!detail.is_empty()).then(|| view! { <span class="page-row-detail" translate="no">{detail.clone()}</span> })}
      </span>
      <Show
        when=move || is_member.get()
        fallback=move || view! {
          <PendingButton
            pending=pending
            class="btn btn-small btn-primary"
            on_click=move |_| pending.run(|| state.write(|backend| backend.join_channel(id)))
          >
            {move || i18n.t("browse-join")}
          </PendingButton>
        }
      >
        <button type="button" class="btn btn-small" on:click=move |_| state.open_conversation(id)>{move || i18n.t("browse-open")}</button>
      </Show>
    </li>
  }
}
