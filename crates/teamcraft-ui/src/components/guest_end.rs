//! What a meeting guest sees when their part in the meeting is over (#1933): a calm card with the
//! meeting's title, one kind line for why, "Rejoin" where that still works, and one quiet line that
//! nothing stays in this browser. The page has forgotten the guest's keys and session by then
//! (`guest_meeting.rs`); this screen needs nothing from the server.

use leptos::prelude::*;

use crate::components::login::AuthFrame;
use crate::i18n::use_i18n;

/// Why the guest's part in the meeting ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GuestEnd {
  /// The guest hung up or left; `maybe_rejoin` is the meeting link to knock again with (none for
  /// a one-time invitation).
  Left { maybe_rejoin: Option<String> },
  /// A host ended the meeting for everyone.
  Ended,
  /// A host removed the guest.
  Removed,
  /// The link was used, expired, was revoked, or the meeting was cancelled.
  LinkExpired,
}

/// The end screen on its own page.
#[component]
pub fn GuestEndScreen(end: GuestEnd, #[prop(optional)] maybe_title: Option<String>) -> impl IntoView {
  view! {
    <AuthFrame>
      <GuestEndCard end=end maybe_title=maybe_title.unwrap_or_default() />
    </AuthFrame>
  }
}

/// The end screen's content, for a page that has its frame already.
#[component]
pub fn GuestEndCard(end: GuestEnd, #[prop(optional, into)] maybe_title: Option<String>) -> impl IntoView {
  let i18n = use_i18n();
  let title = maybe_title.filter(|title| !title.trim().is_empty());
  let (heading_key, heading_untitled_key, line_key) = match &end {
    GuestEnd::Left { .. } => ("guest-end-left", "guest-end-left-untitled", "guest-end-thanks"),
    GuestEnd::Ended => ("guest-end-ended", "guest-end-ended-untitled", "guest-end-thanks"),
    GuestEnd::Removed => (
      "guest-end-removed",
      "guest-end-removed-untitled",
      "guest-end-removed-line",
    ),
    GuestEnd::LinkExpired => ("guest-end-expired", "guest-end-expired", "guest-end-expired-line"),
  };
  let heading = move || match &title {
    Some(title) => i18n.t_with(heading_key, &[("title", title)]),
    None => i18n.t(heading_untitled_key),
  };
  // A link that never got the guest in left nothing behind to mention.
  let had_session = end != GuestEnd::LinkExpired;
  let maybe_rejoin = match end {
    GuestEnd::Left { maybe_rejoin } => maybe_rejoin,
    GuestEnd::Ended | GuestEnd::Removed | GuestEnd::LinkExpired => None,
  };

  view! {
      <div class="guest-end" role="status">
        <h1>{heading}</h1>
        <p class="guest-end-line">{move || i18n.t(line_key)}</p>
        {maybe_rejoin.map(|href| view! {
          <div class="auth-actions">
            <a class="btn btn-primary" href=href data-autofocus>{move || i18n.t("guest-end-rejoin")}</a>
          </div>
        })}
        {had_session.then(|| view! {
          <p class="muted-text small guest-end-quiet">{move || i18n.t("guest-end-forgotten")}</p>
        })}
      </div>
  }
}
