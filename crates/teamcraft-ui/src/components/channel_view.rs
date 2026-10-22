use leptos::prelude::*;

use crate::calls::{CallKind, CallPhase, CallRequest};
use crate::components::avatar::PresenceDot;
use crate::components::bookmarks::BookmarksBar;
use crate::components::composer::Composer;
use crate::components::huddle_bar::HuddleBar;
use crate::components::icons::{Icon, IconName};
use crate::components::log_view::ChannelFeed;
use crate::components::security_ui::KeyChangeBanner;
use crate::i18n::use_i18n;
use crate::model::{Conversation, ConversationId, ConversationKind};
use crate::state::{Overlay, SidePanel, use_app_state};

/// How far back the seen line looks for the user's own latest message.
const SEEN_LOOKBACK: usize = 50;

/// Header, bookmarks bar, message feed and composer of one conversation.
#[component]
pub fn ChannelView(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let info = Memo::new(move |_| state.read(|backend| backend.conversation(conversation)));

  move || {
    // Everything below is recreated per conversation, which resets scroll state and drafts.
    let conv = info.get_untracked()?;
    let name = conv.name.clone();
    let label_name = conv.name;
    let can_post = conv.can_post;
    Some(view! {
      <section class="channel-view" aria-label=move || i18n.t_with("channel-region-label", &[("name", &label_name)])>
        <ChannelHeader conversation=conversation info=info />
        <KeyChangeBanner conversation=conversation />
        <HuddleBar conversation=conversation />
        <BookmarksBar conversation=conversation info=info />
        <ChannelFeed conversation=conversation />
        <SeenLine conversation=conversation />
        <Composer conversation=conversation can_post=can_post conversation_name=name />
      </section>
    })
  }
}

#[component]
fn ChannelHeader(conversation: ConversationId, info: Memo<Option<Conversation>>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();

  let title = move || {
    let conv = info.get()?;
    let icon = match conv.kind {
      ConversationKind::Public => view! { <Icon name=IconName::Hash class="header-icon" /> }.into_any(),
      ConversationKind::Private => view! { <Icon name=IconName::Lock class="header-icon" /> }.into_any(),
      ConversationKind::Integration => {
        view! { <Icon name=IconName::Terminal class="header-icon" /> }.into_any()
      },
      ConversationKind::Group => view! { <Icon name=IconName::Users class="header-icon" /> }.into_any(),
      ConversationKind::Direct => {
        let me = state.me();
        let other = conv.member_ids.iter().copied().find(|id| *id != me.id);
        let presence = other
          .and_then(|id| state.read(|backend| backend.user(id)))
          .map(|user| user.presence);
        match presence {
          Some(presence) => view! { <PresenceDot presence=presence /> }.into_any(),
          None => ().into_any(),
        }
      },
    };
    Some(view! {
      <h1 class="header-title" translate="no">
        {icon}
        {conv.name}
      </h1>
    })
  };

  // A direct conversation shows the other person's local time (from their profile).
  let locale = i18n.locale().tag();
  let local_time = move || {
    let conv = info.get()?;
    if conv.kind != crate::model::ConversationKind::Direct {
      return None;
    }
    let other = conv.member_ids.iter().copied().find(|id| *id != state.me().id)?;
    crate::components::profile_views::local_time_label(other, locale)
  };
  let topic = move || {
    info
      .get()
      .map(|conv| conv.topic)
      .filter(|topic| !topic.is_empty())
  };
  let has_external = move || {
    info
      .get()
      .is_some_and(|conv| state.read(|backend| has_external_members(backend, &conv)))
  };
  // Jump to date: the picked day is searched in the feed; unloaded parts load while we wait.
  let jump_target = RwSignal::new(None::<i64>);
  let is_picking_date = RwSignal::new(false);
  let requested_history = RwSignal::new(None::<i64>);
  Effect::new(move |_| {
    let Some(at) = jump_target.get() else {
      return;
    };
    // Channels too big to hold locally fetch the history around the date first (once per jump).
    if !state.read(|backend| backend.is_history_loaded(conversation, at)) {
      if requested_history.get_untracked() != Some(at) {
        requested_history.set(Some(at));
        state.write(|backend| backend.request_history_at(conversation, at));
      }
      return;
    }
    requested_history.set(None);
    let outcome = state.read(|backend| {
      let count = backend.message_count(conversation);
      first_index_at_or_after(
        count,
        |index| {
          backend
            .messages(conversation, index, 1)
            .first()
            .map(|message| message.sent_at)
        },
        at,
      )
    });
    match outcome {
      DateSearch::Found(index) => {
        state.jump_to_index.set(Some((conversation, index)));
        jump_target.set(None);
      },
      DateSearch::Empty => jump_target.set(None),
      DateSearch::Pending => {},
    }
  });
  let is_favorite = move || info.get().is_some_and(|conv| conv.is_favorite);
  let member_count = move || info.get().map_or(0, |conv| conv.member_ids.len());
  let members_open = move || state.panel.get() == SidePanel::Members;

  view! {
    <header class="channel-header">
      <div class="header-main">
        {title}
        <button
          class="icon-btn"
          aria-pressed=move || is_favorite().to_string()
          aria-label=move || if is_favorite() { i18n.t("favorite-remove") } else { i18n.t("favorite-add") }
          title=move || if is_favorite() { i18n.t("favorite-remove") } else { i18n.t("favorite-add") }
          on:click=move |_| state.write(|backend| backend.set_favorite(conversation, !is_favorite()))
        >
          <Icon name=IconName::Star />
        </button>
        <div class="date-jump">
          <button
            class="icon-btn"
            aria-label=move || i18n.t("jump-to-date")
            title=move || i18n.t("jump-to-date")
            aria-expanded=move || is_picking_date.get().to_string()
            on:click=move |_| is_picking_date.update(|open| *open = !*open)
          >
            <Icon name=IconName::Calendar />
          </button>
          <Show when=move || is_picking_date.get() fallback=|| ()>
            <input
              class="input date-jump-input"
              type="date"
              aria-label=move || i18n.t("jump-to-date")
              on:change=move |ev| {
                let day = event_target_value(&ev);
                if let Some(at) = crate::clock::parse_local_datetime(&format!("{day}T00:00")) {
                  jump_target.set(Some(at));
                  is_picking_date.set(false);
                }
              }
            />
          </Show>
        </div>
        {move || {
          info.get().and_then(|conv| {
            (conv.kind == crate::model::ConversationKind::Direct)
              .then(|| conv.member_ids.iter().copied().find(|id| *id != state.me().id))
              .flatten()
              .map(|other| view! { <crate::components::profile_views::InMeetingBadge user=other /> })
          })
        }}
        {move || local_time().map(|label| view! { <span class="header-local-time" title=move || i18n.t("profile-local-time")>{label}</span> })}
        <Show when=has_external fallback=|| ()>
          <span class="badge badge-guest" title=move || i18n.t("external-badge-detail")>
            {move || i18n.t("external-badge")}
          </span>
        </Show>
        <Show when=move || topic().is_some() fallback=|| ()>
          <span class="header-divider" aria-hidden="true"></span>
          <p class="header-topic" title=move || topic().unwrap_or_default()>
            {move || topic().unwrap_or_default()}
          </p>
        </Show>
      </div>
      <div class="header-actions">
        <CallButtons conversation=conversation info=info />
        <button
          class="header-btn"
          aria-label=move || i18n.t("channel-settings-title")
          title=move || i18n.t("channel-settings-title")
          on:click=move |_| state.overlay.set(Overlay::ChannelSettings(conversation))
        >
          <Icon name=IconName::Sliders />
        </button>
        <button
          class="header-btn"
          aria-pressed=move || (state.panel.get() == SidePanel::Files).to_string()
          aria-label=move || i18n.t("files-title")
          title=move || i18n.t("files-title")
          on:click=move |_| {
            state.panel.update(|panel| {
              *panel = if *panel == SidePanel::Files { SidePanel::Closed } else { SidePanel::Files };
            });
          }
        >
          <Icon name=IconName::Paperclip />
        </button>
        <button
          class="header-btn"
          aria-pressed=move || (state.panel.get() == SidePanel::Pins).to_string()
          aria-label=move || i18n.t("pins-title")
          title=move || i18n.t("pins-title")
          on:click=move |_| {
            state.panel.update(|panel| {
              *panel = if *panel == SidePanel::Pins { SidePanel::Closed } else { SidePanel::Pins };
            });
          }
        >
          <Icon name=IconName::Star />
        </button>
        <button
          class="header-btn"
          aria-pressed=move || members_open().to_string()
          aria-label=move || i18n.t_count("members-button-label", member_count())
          on:click=move |_| {
            state.panel.update(|panel| {
              *panel = if *panel == SidePanel::Members { SidePanel::Closed } else { SidePanel::Members };
            });
          }
        >
          <Icon name=IconName::Users />
          <span>{member_count}</span>
        </button>
      </div>
    </header>
  }
}

/// Start buttons in the header: audio and video call, plus a drop-in huddle in channels.
#[component]
fn CallButtons(conversation: ConversationId, info: Memo<Option<Conversation>>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let call = state.call;
  let kind = move || info.get().map(|conv| conv.kind);
  let call_kind = move || match kind() {
    Some(ConversationKind::Direct) => CallKind::Direct,
    _ => CallKind::Group,
  };
  let is_callable = move || kind().is_some_and(|kind| kind != ConversationKind::Integration);
  let is_channel = move || matches!(kind(), Some(ConversationKind::Public | ConversationKind::Private));
  let is_busy = move || call.phase.get() != CallPhase::Idle;

  view! {
    <Show when=is_callable fallback=|| ()>
      <button
        class="icon-btn"
        disabled=is_busy
        aria-label=move || i18n.t("call-start-audio")
        title=move || i18n.t("call-start-audio")
        on:click=move |_| call.prepare(CallRequest { conversation, kind: call_kind() }, false)
      >
        <Icon name=IconName::Phone />
      </button>
      <button
        class="icon-btn"
        disabled=is_busy
        aria-label=move || i18n.t("call-start-video")
        title=move || i18n.t("call-start-video")
        on:click=move |_| call.prepare(CallRequest { conversation, kind: call_kind() }, true)
      >
        <Icon name=IconName::Video />
      </button>
      <Show when=is_channel fallback=|| ()>
        <button
          class="icon-btn"
          disabled=is_busy
          aria-label=move || i18n.t("huddle-start")
          title=move || i18n.t("huddle-start")
          on:click=move |_| {
            call.is_camera_on.set(false);
            call.join(CallRequest { conversation, kind: CallKind::Huddle });
          }
        >
          <Icon name=IconName::Headphones />
        </button>
      </Show>
    </Show>
  }
}

/// "Seen by …" for the newest message of mine, when a DM member has read it. Receipts are
/// reciprocal and only exist in DMs and group DMs; elsewhere the backend returns nobody.
#[component]
fn SeenLine(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let me = state.me().id;
  let names = Memo::new(move |_| {
    state.read(|backend| {
      let count = backend.message_count(conversation);
      let recent = backend.messages(conversation, count.saturating_sub(SEEN_LOOKBACK), SEEN_LOOKBACK);
      let newest = recent.into_iter().rev().find(|message| message.author == me)?;
      let names: Vec<String> = backend
        .seen_by(conversation, newest.id)
        .into_iter()
        .filter_map(|user| backend.user(user))
        .map(|user| user.display_name)
        .collect();
      (!names.is_empty()).then(|| names.join(", "))
    })
  });
  move || {
    names.get().map(|names| {
      view! { <div class="seen-line" translate="no">{i18n.t_with("message-seen-by", &[("names", &names)])}</div> }
    })
  }
}

/// Whether the conversation has members from outside the organization (guests).
pub fn has_external_members(backend: &dyn crate::backend::Backend, conversation: &Conversation) -> bool {
  backend.has_external_people(conversation.id)
    || conversation
      .member_ids
      .iter()
      .any(|member| backend.user(*member).is_some_and(|user| user.is_guest))
}

/// Where a search for the first message at or after a time stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateSearch {
  Found(usize),
  /// A probed message is not loaded yet; ask again when the feed changed.
  Pending,
  /// The conversation has no messages.
  Empty,
}

/// Binary search over the feed: the index of the first message sent at or after `at`, or the last
/// index when every message is older. `sent_at_of` returns `None` while a message is not loaded.
pub fn first_index_at_or_after(
  count: usize,
  sent_at_of: impl Fn(usize) -> Option<i64>,
  at: i64,
) -> DateSearch {
  if count == 0 {
    return DateSearch::Empty;
  }
  let (mut low, mut high) = (0, count);
  while low < high {
    let middle = low + (high - low) / 2;
    match sent_at_of(middle) {
      None => return DateSearch::Pending,
      Some(sent_at) if sent_at < at => low = middle + 1,
      Some(_) => high = middle,
    }
  }
  DateSearch::Found(low.min(count - 1))
}

#[cfg(test)]
mod date_search_tests {
  use super::*;

  #[test]
  fn the_first_message_on_or_after_a_time_is_found() {
    let times = [10_i64, 20, 20, 30, 50];
    let probe = |index: usize| times.get(index).copied();
    assert_eq!(first_index_at_or_after(5, probe, 0), DateSearch::Found(0));
    assert_eq!(first_index_at_or_after(5, probe, 20), DateSearch::Found(1));
    assert_eq!(first_index_at_or_after(5, probe, 21), DateSearch::Found(3));
    assert_eq!(first_index_at_or_after(5, probe, 99), DateSearch::Found(4));
    assert_eq!(first_index_at_or_after(0, probe, 5), DateSearch::Empty);
  }

  #[test]
  fn an_unloaded_probe_means_ask_again_later() {
    assert_eq!(first_index_at_or_after(5, |_| None, 5), DateSearch::Pending);
  }
}
