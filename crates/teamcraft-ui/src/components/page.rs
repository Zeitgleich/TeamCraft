//! Pieces the Home pages (Calls, Meetings, People, Saved) share, so they read as one family: the
//! portrait of a conversation and the day headings. The layout itself is the `.page-view` class in
//! the stylesheet (one centred column, one header).

use leptos::prelude::*;

use crate::clock::{format_date, local_day, now_ms};
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::icons::{Icon, IconName};
use crate::i18n::I18n;
use crate::model::{Conversation, ConversationKind, TimestampMs};
use crate::state::use_app_state;

/// The picture of a conversation: the other person for a DM (with presence), a group icon for
/// everything else.
#[component]
pub fn ConversationPortrait(
  conversation: Conversation,
  #[prop(default = AvatarSize::Medium)] size: AvatarSize,
) -> impl IntoView {
  let state = use_app_state();
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
  match maybe_other {
    // Presence arrives after the row is built, so the user is read reactively.
    Some(other) => view! {
      <span class="page-portrait">
        {move || {
          state
            .read(|backend| backend.user(other))
            .map(|user| view! { <Avatar user=user size=size show_presence=true /> })
        }}
      </span>
    }
    .into_any(),
    None => view! { <span class="page-portrait page-portrait-group"><Icon name=IconName::Users /></span> }
      .into_any(),
  }
}

/// "Today", "Yesterday" or the date, for the heading of a day.
pub fn day_heading(day: i64, first_ts: TimestampMs, i18n: I18n) -> String {
  match day - local_day(now_ms()) {
    0 => i18n.t("page-today"),
    -1 => i18n.t("page-yesterday"),
    _ => format_date(first_ts, i18n.locale().tag()),
  }
}
