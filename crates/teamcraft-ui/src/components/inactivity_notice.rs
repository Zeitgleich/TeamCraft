//! The inactivity notice of a channel (issue #212, admin.md §3.1.3): its owners read in the channel
//! itself that nobody wrote in it for a while, when it is archived or deleted, and may keep it open.
//! What is said, and who is told, is decided in `teamcraft_client_api::inactivity_notice`; this
//! only draws it. Only people who may manage the channel get the status from the server.

use leptos::prelude::*;
use teamcraft_client_api::inactivity_notice::InactivityNotice;
use teamcraft_proto::ConversationId;

use crate::clock::{format_date, now_ms};
use crate::components::icons::{Icon, IconName};
use crate::feedback::PendingAction;
use crate::i18n::use_i18n;
use crate::state::use_app_state;

#[component]
pub fn ChannelInactivityNotice(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let pending = PendingAction::new();
  let notice = Memo::new(move |_| {
    state.read(|backend| {
      backend
        .conversation(conversation)
        .and_then(|channel| channel.inactivity_notice(now_ms()))
    })
  });
  move || {
    notice.get().map(|notice| {
      let tag = i18n.locale().tag();
      let text = match notice {
        InactivityNotice::Quiet {
          archive_at: Some(at),
        } => i18n.t_with("inactivity-notice-archive-on", &[("date", &format_date(at.0, tag))]),
        InactivityNotice::Quiet { archive_at: None } => i18n.t("inactivity-notice-quiet"),
        InactivityNotice::ArchivedByPolicy {
          delete_at: Some(at),
        } => i18n.t_with("inactivity-notice-delete-on", &[("date", &format_date(at.0, tag))]),
        InactivityNotice::ArchivedByPolicy { delete_at: None } => i18n.t("inactivity-notice-archived"),
        InactivityNotice::KeptOpen { until } => {
          i18n.t_with("inactivity-notice-kept-open", &[("date", &format_date(until.0, tag))])
        },
      };
      let is_archived = matches!(notice, InactivityNotice::ArchivedByPolicy { .. });
      view! {
        <div class="key-banner inactivity-notice" role="status">
          <Icon name=IconName::Clock />
          <div class="key-banner-text">
            <span>{text}</span>
          </div>
          {notice.offers_keep_open().then(|| view! {
            <button
              type="button"
              class="btn btn-small"
              prop:disabled=move || pending.is_pending()
              on:click=move |_| {
                pending.run(|| state.write(|backend| backend.snooze_channel_inactivity(conversation)));
              }
            >
              {move || i18n.t(if is_archived { "inactivity-notice-postpone" } else { "inactivity-notice-keep" })}
            </button>
          })}
        </div>
      }
    })
  }
}
